// SPDX-License-Identifier: MPL-2.0

//! Cancellation — the one verb a person has on a run that is not finished
//! and does not need a fresh attempt (`resume.rs` is the other).
//!
//! **Cancel is `CancelReason::Run`, always** (D36): it terminalizes. During
//! an orchestrator turn it reaches the session as a cancellation of that
//! turn and the run ends on the `TurnCancelled` that comes back; outside a
//! turn the run is `Cancelled` at once and the session is ended. A takeover
//! boundary is never asked for here — that is the session's own handover
//! verb, and it ends a step, not the job.
//!
//! **It reaches the process tree while a person holds control** (§7.6).
//! The lease is then theirs and the active turn is theirs, so the turn is
//! not cancelled through a lease this orchestrator gave up; the run ends at
//! once and **the session is ended**, which is what takes the provider's
//! process tree with it. Ending a delegated session needs no lease — the
//! lease belongs to the person who borrowed a step, the session belongs to
//! the run that created it — and the timeline records that control came
//! back to the run before it records the end.

use koru_delegate_core::{
    CancelReason, ControlState, RunAttemptId, RunEvent, RunHeader, RunId, RunStatus, StepKind,
    TimelineRef, TimelineRefs,
};
use koru_delegate_sqlite::AttemptPhase;

use crate::RunFact;

use super::{Orchestrator, RunError};

impl Orchestrator {
    /// End the run. Idempotent on a finished run.
    pub fn cancel(&mut self, run: RunId) -> Result<RunHeader, RunError> {
        let status = self.status(run)?;
        if status.is_terminal() {
            return self.header(run);
        }
        let human_holds_control = matches!(status.control(), Some(ControlState::Human { .. }));
        let live = self
            .open(run)?
            .attempt
            .as_ref()
            .map(|attempt| (attempt.id, attempt.session, attempt.orchestrator_turn));
        if !human_holds_control
            && let (RunStatus::Running { .. }, Some((_, Some(session), Some(turn)))) =
                (&status, live)
        {
            // The orchestrator's own turn is running: the cancellation
            // reaches it, and the run ends on the `TurnCancelled` it
            // answers with. A cancellation the host refuses — the turn
            // ended in the race — falls through to the immediate end.
            match self.ports.sessions.cancel_turn(
                session,
                turn,
                koru_session_protocol::CancelReason::Run,
            ) {
                Ok(()) => {
                    self.trace(run, "cancel_sent", format_args!("turn cancelled"));
                    return self.header(run);
                }
                Err(error) => {
                    self.trace(run, "cancel_refused", format_args!("{}", error.code));
                }
            }
        }
        let attempt = live.map(|(attempt, _, _)| attempt);
        // A cancellation before any attempt existed is still stamped with a
        // generation, because the row spells one: the ledger's if there is a
        // row, else the generation the first attempt would have been.
        let generation = self
            .open(run)?
            .attempt_ids()
            .map(|(_, generation)| generation)
            .or(self
                .ports
                .store
                .current_attempt(run)?
                .map(|row| row.generation))
            .unwrap_or(koru_delegate_core::AttemptGeneration::FIRST);
        if let Some(attempt) = attempt
            && human_holds_control
        {
            // The person's control ends with the run: ending the session
            // takes their lease with it.
            self.control_returned_by_the_end(run, attempt)?;
        }
        self.apply(run, &RunEvent::Cancel)?;
        let refs = match attempt {
            Some(attempt) => {
                TimelineRefs::new([TimelineRef::Attempt { attempt }]).expect("one ref")
            }
            None => TimelineRefs::default(),
        };
        self.step(
            run,
            StepKind::Cancelled {
                reason: CancelReason::Run,
            },
            refs,
            Some(RunFact::Cancelled {
                run,
                attempt,
                generation,
                reason: CancelReason::Run,
            }),
        )?;
        if attempt.is_some() {
            self.end_attempt(run, AttemptPhase::Ended, true)?;
        } else if let Some(row) = self.ports.store.current_attempt(run)?
            && row.is_live()
        {
            // An interrupted attempt this device is not driving: the row
            // ends with the run.
            self.ports
                .store
                .advance(run, row.generation, AttemptPhase::Ended)?;
            self.ports.store.retire(run, row.generation)?;
        }
        self.header(run)
    }

    /// A run cancelled while a person held control: the record that control
    /// came back to the run, written before the end that took the person's
    /// lease with it.
    ///
    /// No status moves here. `ControlReturned` on a satisfied objective
    /// completes the run, which is not what a cancellation means; the
    /// `Cancel` that follows is what moves the status, and it is legal from
    /// `Running` under any control.
    fn control_returned_by_the_end(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
    ) -> Result<(), RunError> {
        self.step(
            run,
            StepKind::ControlReturned,
            TimelineRefs::new([TimelineRef::Attempt { attempt }]).expect("one ref"),
            Some(RunFact::ControlReturned { run, attempt }),
        )
    }
}
