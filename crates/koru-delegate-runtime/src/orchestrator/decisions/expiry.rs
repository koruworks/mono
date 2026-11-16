// SPDX-License-Identifier: MPL-2.0

//! The decision deadline (O157). Nothing else closes a Koru edit decision
//! nobody answered: the ledger refuses an answer, a reservation and a consume
//! after the deadline, but refusing is not closing, and the Run would wait
//! with its fence up until a person's Stop.
//!
//! This is one check on the resume driver's existing per-turn read of a Run's
//! open decision wait, so it runs on the actor's existing maintenance tick
//! and, because that selection reads the durable open-wait index, after a
//! restart too. It adds no timer, thread or durable row: when the deadline
//! has passed it asks for the existing cleanup (`request_publication_cleanup`)
//! with `Expired`, and the existing cleanup owner stops the retained current
//! consumer, records the final cleanup and ends the Run as "Needed you".
//!
//! The rules it keeps (P4.2 decision contract, section 9):
//!
//! - **Which decisions.** Only a Koru edit decision that is still Pending or
//!   Granted. A Consumed decision is saving its approved edit and settles on
//!   that save (O150); the execution budget owns it. A Settled decision has
//!   no deadline any more and is never reopened. A decision whose cleanup is
//!   already owned (a person's Stop, a Deny, an earlier expiry) or already
//!   finally recorded (O152) is left to that owner.
//! - **An answer before the deadline wins.** The request is a compare-and-set
//!   on the exact wait and decision revisions read on this turn, and the
//!   ledger refuses `Expired` before the deadline. An answer that committed
//!   first changes the revision, so the request is refused and the next turn
//!   reads the answered decision. What an answer cannot do is outlive the
//!   deadline unconsumed: the ledger refuses the reservation and the consume
//!   after it (`clock`), so a Granted decision that has not been consumed by
//!   then is closed here rather than left waiting for a consume that cannot
//!   be accepted.
//! - **Elapsed time.** Within one process the deadline is the earlier of the
//!   wall clock reaching it (which also covers sleep, since wall time keeps
//!   advancing) and the same span elapsing on the monotonic clock. When only
//!   the monotonic clock says so, the wall clock is not trusted and the cause
//!   is `ClockUncertain`, which the ledger accepts without a wall deadline.
//!   A decision created under another delegate boot has unproven elapsed
//!   time: it is closed as `ClockUncertain` at once, even if its saved
//!   deadline looks future (no answer or consume is accepted for it anyway).
//! - **Pacing (O142).** A refused request is asked again no sooner than the
//!   actor's ordinary tick, in memory only, however often the driver is woken.

use super::*;
use crate::orchestrator::state::DecisionDeadlineWatch;
use koru_delegate_core::decisions::{
    BotWaitPhase, DecisionAuthorization, DecisionClosureReason, DecisionDeadline, DecisionPhase,
    DecisionState,
};
use koru_delegate_sqlite::DecisionWaitExecution;
use std::time::{Duration, Instant};

/// How long a refused expiry request waits before it is asked again: the
/// actor's ordinary maintenance tick (O142).
const EXPIRY_RETRY: Duration = Duration::from_secs(5);

/// Why this decision's wait is over by its deadline, if it is.
pub(super) fn deadline_cause(
    deadline: &DecisionDeadline,
    boot: DelegateBootId,
    now_ms: i64,
    monotonic_elapsed: bool,
) -> Option<DecisionClosureReason> {
    if deadline.created_boot != boot {
        // Another process created it: how long it has waited is unproven.
        return Some(DecisionClosureReason::ClockUncertain);
    }
    if now_ms >= deadline.deadline_at_ms {
        return Some(DecisionClosureReason::Expired);
    }
    // The whole wait has elapsed on the monotonic clock while the wall clock
    // says it has not: the wall clock moved, so it proves nothing.
    monotonic_elapsed.then_some(DecisionClosureReason::ClockUncertain)
}

/// Whether the deadline may still close this wait: an open Koru decision (an
/// edit or, since M5, a private operation) that is Pending or Granted and
/// whose cleanup nobody owns yet.
pub(super) fn deadline_applies(execution: &DecisionWaitExecution) -> bool {
    deadline_applies_to(
        execution.decision_state.as_ref(),
        execution.wait.phase,
        execution.wait.cleanup.is_some(),
    )
}

/// [`deadline_applies`] on the three facts it reads.
pub(super) fn deadline_applies_to(
    state: Option<&DecisionState>,
    wait: BotWaitPhase,
    cleanup_recorded: bool,
) -> bool {
    state.is_some_and(|state| {
        // Every Koru decision has this deadline. A native prompt keeps its
        // own fallback (`decisions/publication/fallback.rs`).
        state.kind.resumes_by_successor()
            && matches!(
                state.authorization,
                DecisionAuthorization::Pending | DecisionAuthorization::Granted
            )
            && !matches!(
                state.phase,
                DecisionPhase::Settled | DecisionPhase::StopPending
            )
    }) && wait != BotWaitPhase::CleanupPending
        && !cleanup_recorded
}

impl Orchestrator {
    /// The deadline's part of one driver turn. `None` means the deadline has
    /// nothing to say and the resume chain continues; `Some` is this turn's
    /// outcome (the wait was closed as expired, or its refused request waits).
    pub(super) fn decision_deadline_step(
        &mut self,
        owner: BotOwner,
        run: RunId,
        execution: &DecisionWaitExecution,
    ) -> Result<Option<DecisionResumeStep>, RunError> {
        let Some(expected) = execution.expected.decision.as_ref() else {
            return Ok(None);
        };
        if !deadline_applies(execution) {
            if let Some(state) = self.runs.get_mut(&run) {
                state.decision_deadline = None;
            }
            return Ok(None);
        }
        let record = self
            .bot_ledger()?
            .bot_decision(owner, run, expected.decision)?
            .ok_or_else(|| refused("bot_fenced"))?;
        let deadline = record.receipt.deadline;
        let boot = self.delegate_boot();
        let now_ms = self.ports.clock.now_ms();
        let now = Instant::now();
        self.restore_publication_owner(run)?;
        let Some(state) = self.runs.get_mut(&run) else {
            return Ok(None);
        };
        if state.decision_cleanup.is_some() {
            // A cleanup request is already retained for this Run (a person's
            // Stop whose reply was lost, say): its owner retries it.
            return Ok(None);
        }
        let watch = match &mut state.decision_deadline {
            Some(watch) if watch.decision == expected.decision => watch,
            slot => {
                // First sight in this process: the span the wall clock says
                // is left, started now on the monotonic clock. Never longer
                // than the wall deadline, so it cannot extend the wait.
                let left = deadline.deadline_at_ms.saturating_sub(now_ms).max(0);
                slot.insert(DecisionDeadlineWatch {
                    decision: expected.decision,
                    monotonic: now + Duration::from_millis(left.unsigned_abs()),
                    retry_at: None,
                })
            }
        };
        let Some(cause) = deadline_cause(&deadline, boot, now_ms, watch.monotonic <= now) else {
            return Ok(None);
        };
        if watch.retry_at.is_some_and(|at| at > now) {
            return Ok(Some(DecisionResumeStep::Waiting(
                "bot_decision_expiry_paced",
            )));
        }
        watch.retry_at = Some(now + EXPIRY_RETRY);
        match self.request_publication_cleanup(run, cause) {
            Ok(true) => Ok(Some(DecisionResumeStep::Expired)),
            // The wait closed between the read and the request.
            Ok(false) => Ok(None),
            Err(error) => {
                // A refused request must not stay retained: its expected
                // revisions are stale as soon as anything else moves the wait
                // (an answer that won the race), and a later Stop would be
                // made to retry it. Unless the ledger shows the cleanup is
                // owned after all (a lost reply), drop it; the next paced turn
                // reads the wait again and decides afresh.
                let owned = self
                    .bot_ledger()?
                    .decision_cleanup_ownership(owner, run)
                    .ok()
                    .flatten()
                    .is_some();
                if !owned && let Some(state) = self.runs.get_mut(&run) {
                    state.decision_cleanup = None;
                }
                self.trace(run, "decision_expiry_refused", format_args!("{error}"));
                Ok(Some(DecisionResumeStep::Waiting(if owned {
                    "bot_cleanup_owned"
                } else {
                    "bot_decision_expiry_refused"
                })))
            }
        }
    }
}

#[cfg(test)]
mod tests;
