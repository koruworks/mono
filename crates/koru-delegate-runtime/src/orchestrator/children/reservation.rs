// SPDX-License-Identifier: MPL-2.0

//! The parent's successor reservation after a child wait (contract 4.5).
//!
//! A wait that is `retired` with no open request is resumed as soon as a
//! host slot is free. The one rule for a resume that cannot happen is here:
//! a definitive refusal cleans the wait up at once; an error is retried and,
//! after two minutes of counted time of nothing but errors, cleaned up too.
//! "Host busy" and a paused parent Bot are neither: the wait stays as it is.
//!
//! None of this is the decision path. Its qualification says nothing about
//! Routines, and a Routine parent's proof is obtained here without the
//! decision path's Routine refusal and without the execution-policy read
//! that refuses while the Run's document is held.

use super::driver::ChildResumeStep;
use super::*;
use koru_delegate_core::FreshBotPolicyProof;
use koru_delegate_core::decisions::DecisionClosureReason;
use koru_delegate_sqlite::{BotLedgerError, ChildSuccessorReservation};

/// What a reservation attempt means for the wait.
enum Verdict {
    Step(ChildResumeStep),
    /// The wait can never be resumed: clean it up now.
    Definitive(&'static str),
    /// Something was unavailable: retry, within the stall limit.
    Error(RunError),
}

impl Orchestrator {
    /// Why no successor could be launched for this Run at all, if none
    /// could. Its own predicate: it does not look at the Routine (6.7).
    pub(super) fn child_successor_unsupported(
        &mut self,
        run: RunId,
    ) -> Result<Option<&'static str>, RunError> {
        if !self.ports.sessions.supports_delegation() {
            return Ok(Some("bot_successor_unsupported"));
        }
        let admission = self
            .bot_ledger()?
            .bot_admission(run)?
            .ok_or(RunError::UnknownRun)?;
        if !self
            .ports
            .sessions
            .profile_delegatable(&admission.submission.request.agent)?
        {
            return Ok(Some("bot_profile_unready"));
        }
        Ok(None)
    }

    /// The current proof a Routine parent's successor needs: the Routine
    /// link, then the Bot's and the Routine's current policy from the
    /// snapshot the AI owner synchronized. `None` for an interactive parent,
    /// which continues under its captured policy. Usable while the Run's
    /// document is held for publication.
    pub(super) fn child_policy_proof(
        &mut self,
        run: RunId,
    ) -> Result<Option<FreshBotPolicyProof>, RunError> {
        let admitted = self
            .bot_ledger()?
            .bot_admission(run)?
            .ok_or(RunError::UnknownRun)?;
        if admitted.submission.provenance.routine.is_none() {
            return Ok(None);
        }
        self.linked_routine_admission(&admitted)?;
        let provenance = &admitted.submission.provenance;
        let current = self
            .native_ledger()?
            .current_native_policy(provenance.owner, provenance.bot)?
            .ok_or_else(|| refused("bot_policy_refresh_required"))?;
        self.current_policy_for_submission(
            &admitted.submission,
            current.config_revision,
            current.policy_revision,
        )
        .map(Some)
    }

    fn child_reserve_verdict(
        &mut self,
        owner: BotOwner,
        run: RunId,
        execution: &ChildWaitExecution,
    ) -> Verdict {
        match self.child_successor_unsupported(run) {
            Ok(None) => {}
            Ok(Some(code)) => return Verdict::Definitive(code),
            Err(error) => return Verdict::Error(error),
        }
        let host = match self.ports.sessions.host_instance() {
            Ok(host) => host,
            Err(error) => return Verdict::Error(error.into()),
        };
        let fingerprint = match self.bot_child_continuation_fingerprint(owner, run) {
            Ok(fingerprint) => fingerprint,
            Err(error) => return Verdict::Error(error),
        };
        let proof = match self.child_policy_proof(run) {
            Ok(proof) => proof,
            // A Routine that was retired or unlinked, or whose access was
            // edited, is refused by the ledger below with the proof it can
            // still get; a cache not yet refilled is an error.
            Err(error) => return Verdict::Error(error),
        };
        let reserved = self.bot_ledger().and_then(|ledger| {
            Ok(ledger.reserve_child_successor(
                &execution.expected,
                host,
                fingerprint,
                proof.as_ref(),
            ))
        });
        let reserved = match reserved {
            Ok(reserved) => reserved,
            Err(error) => return Verdict::Error(error),
        };
        match reserved {
            Ok(ChildSuccessorReservation::Reserved(reserved)) => {
                // Only a reservation this process made may ask for its
                // creation; one found retained is recovered by its key.
                if let Some((_, _, _, creation)) = reserved
                    .resume
                    .as_ref()
                    .and_then(|resume| resume.consumer_identity())
                {
                    self.children
                        .launches
                        .insert(run, ChildLaunch::Reserved(creation));
                }
                Verdict::Step(ChildResumeStep::SuccessorReserved)
            }
            Ok(ChildSuccessorReservation::AlreadyReserved(_)) => {
                Verdict::Step(ChildResumeStep::SuccessorReserved)
            }
            Ok(ChildSuccessorReservation::HostBusy) => {
                Verdict::Step(ChildResumeStep::Waiting("bot_capacity"))
            }
            Ok(ChildSuccessorReservation::Paused) => {
                Verdict::Step(ChildResumeStep::Waiting("bot_paused"))
            }
            Ok(ChildSuccessorReservation::OpenRequests) => {
                Verdict::Step(ChildResumeStep::Waiting("bot_child_requests_open"))
            }
            Ok(ChildSuccessorReservation::NotRetired) => {
                Verdict::Step(ChildResumeStep::Waiting("bot_origin_stop_unproven"))
            }
            Ok(ChildSuccessorReservation::BudgetSpent) => {
                Verdict::Definitive("bot_budget_exhausted")
            }
            Ok(ChildSuccessorReservation::Unqualified) => {
                Verdict::Definitive("bot_child_unqualified")
            }
            Err(BotLedgerError::Fenced) => Verdict::Definitive("bot_fenced"),
            Err(BotLedgerError::Conflict) => Verdict::Definitive("bot_conflict"),
            Err(BotLedgerError::Invalid) => Verdict::Definitive("bot_invalid"),
            Err(BotLedgerError::Stale) => Verdict::Definitive("bot_stale"),
            Err(error) => Verdict::Error(error.into()),
        }
    }

    /// One reservation attempt for a wait that is `retired` with no open
    /// request, and the one rule applied to its answer.
    pub(super) fn child_reserve_step(
        &mut self,
        owner: BotOwner,
        run: RunId,
        execution: &ChildWaitExecution,
    ) -> Result<ChildResumeStep, RunError> {
        let wait = execution.wait.wait;
        match self.child_reserve_verdict(owner, run, execution) {
            Verdict::Step(step) => {
                if !matches!(step, ChildResumeStep::Waiting(_)) {
                    self.children.stalled.remove(&wait);
                }
                Ok(step)
            }
            Verdict::Definitive(code) => {
                self.child_request_cleanup(run)?;
                Ok(ChildResumeStep::CleanupRequested(code))
            }
            Verdict::Error(error) => {
                if self.child_stalled(wait) {
                    self.child_request_cleanup(run)?;
                    return Ok(ChildResumeStep::CleanupRequested("bot_child_stalled"));
                }
                Err(error)
            }
        }
    }

    /// Record that this wait met an error or is still short of its
    /// acknowledgment, and answer whether it has been so for the stall limit
    /// of counted time.
    pub(super) fn child_stalled(&mut self, wait: BotWaitId) -> bool {
        let counted = self.children.clock.counted_ms();
        let since = *self.children.stalled.entry(wait).or_insert(counted);
        counted.saturating_sub(since) >= CHILD_STALL_LIMIT_MS
    }

    /// Ask the wait's cleanup owner to end it, with cause `Canceled`, and
    /// stop its current consumer and its children. The requests are settled
    /// by the ledger in the same transaction that moves the phase.
    pub(in crate::orchestrator) fn child_request_cleanup(
        &mut self,
        run: RunId,
    ) -> Result<(), RunError> {
        let requested = self.request_publication_cleanup(run, DecisionClosureReason::Canceled);
        // A writer may have committed despite returning an error; physical
        // cleanup re-reads the retained current consumer by itself.
        let observed = self.stop_decision_consumer(run);
        let device = self
            .bot_ledger()
            .and_then(|ledger| Ok(ledger.bot_execution(run)?))
            .ok()
            .flatten()
            .map(|record| record.owner.device);
        if let Some(device) = device {
            self.child_cleanup_requested(run, device);
        }
        requested?;
        if let Err(error) = observed {
            self.trace(run, "child_cleanup_unobserved", format_args!("{error}"));
        }
        Ok(())
    }
}
