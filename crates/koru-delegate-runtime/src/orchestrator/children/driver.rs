// SPDX-License-Identifier: MPL-2.0

//! The per-turn driver of a child wait. The actor calls it once per
//! maintenance turn for one Run with an open child wait; it reads the
//! retained wait and takes at most one exact step (enrollment, origin stop,
//! reservation, binding, installation, continuation input, or a cleanup
//! request), or none.
//!
//! The sequence is contract 4.1: the wait is born `hold_requested`; the
//! origin is enrolled and then stopped (`retired`); when every request has
//! settled a successor is reserved (`resume_reserved`), launched, installed
//! and fed its continuation; the wait settles once the Control that clears
//! the waiting marker is applied. A wait that cannot go on is handed to the
//! cleanup owner, never left to hold a slot.

use super::launch::resume_open;
use super::*;
use koru_delegate_core::DeviceId;
use koru_delegate_core::children::BotChildWaitEnrollmentReply;
use koru_delegate_core::decisions::{BotWaitPhase, DecisionResumeState};
use koru_delegate_sqlite::{CHILD_RESUME_RUN_PAGE, ChildWaitCursor, ChildWaitRun};

/// What one driver turn did for a Run's open child wait. Each acting variant
/// names the single step taken. `Waiting` changed nothing and is not success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildResumeStep {
    /// No open child wait on this Run.
    Idle,
    /// Nothing this driver owns can advance the wait now; the code names
    /// what it waits for.
    Waiting(&'static str),
    Enrolled,
    OriginStopped,
    SuccessorReserved,
    SuccessorBound,
    SuccessorInstalled,
    InputUnconfirmed,
    InputAcknowledged,
    /// The wait cannot be resumed and was handed to its cleanup owner; the
    /// code names why.
    CleanupRequested(&'static str),
}

/// A refusal that only means "not yet": retry on a later turn.
fn retry_code(error: &RunError) -> Option<&'static str> {
    match error {
        RunError::Port(error) => match error.code.as_str() {
            "publication_pending" => Some("publication_pending"),
            "decision_enrollment_pending" => Some("decision_enrollment_pending"),
            "bot_stale" => Some("bot_stale"),
            "bot_fenced" => Some("bot_fenced"),
            "bot_capacity" => Some("bot_capacity"),
            "bot_policy_refresh_required" => Some("bot_policy_refresh_required"),
            "bot_policy_stale" => Some("bot_policy_stale"),
            "bot_profile_unready" => Some("bot_profile_unready"),
            "bot_controller_required" => Some("bot_controller_required"),
            _ => None,
        },
        _ => None,
    }
}

impl Orchestrator {
    /// Open child waits to offer the driver, rotating after `after`: one
    /// bounded keyset page strictly after the cursor, then one wrapped page
    /// at or before it. Durable, so it survives a restart. Identity only.
    pub fn bot_child_resume_runs(
        &mut self,
        device: DeviceId,
        after: Option<ChildWaitCursor>,
    ) -> Result<Vec<ChildWaitRun>, RunError> {
        let mut runs =
            self.bot_ledger()?
                .child_resume_runs(device, after.as_ref(), CHILD_RESUME_RUN_PAGE)?;
        if let Some(cursor) = after {
            for entry in
                self.bot_ledger()?
                    .child_resume_runs(device, None, CHILD_RESUME_RUN_PAGE)?
            {
                let wrapped = (entry.cursor.created_at_ms, entry.cursor.wait)
                    <= (cursor.created_at_ms, cursor.wait);
                if wrapped && !runs.iter().any(|known| known.wait == entry.wait) {
                    runs.push(entry);
                }
            }
        }
        Ok(runs)
    }

    /// Advance this Run's open child wait by at most one exact step.
    /// Retryable refusals become `Waiting`. Whatever happened, the Run's
    /// classification and its requests' events follow the ledger.
    pub fn bot_advance_child_resume(
        &mut self,
        owner: BotOwner,
        run: RunId,
        bridge: &mut dyn ChildEnrollmentBridge,
    ) -> Result<ChildResumeStep, RunError> {
        let step = match self.child_resume_step(owner, run, bridge) {
            Err(error) => match retry_code(&error) {
                Some(code) => Ok(ChildResumeStep::Waiting(code)),
                None => Err(error),
            },
            step => step,
        };
        self.child_publish_changes(owner, run);
        if let Err(error) = self.sync_child_authority(owner, run) {
            self.trace(run, "child_wait_unpublished", format_args!("{error}"));
        }
        step
    }

    fn child_resume_step(
        &mut self,
        owner: BotOwner,
        run: RunId,
        bridge: &mut dyn ChildEnrollmentBridge,
    ) -> Result<ChildResumeStep, RunError> {
        use ChildResumeStep as Step;
        let Some(execution) = self.bot_ledger()?.child_wait_execution(owner, run)? else {
            self.children.launches.remove(&run);
            return Ok(Step::Idle);
        };
        let wait = execution.wait.wait;
        match execution.wait.phase {
            BotWaitPhase::HoldRequested => {
                let held = self.child_hold_step(owner, run, &execution, bridge);
                // A parent that could not be retired cannot be given a
                // successor. Once none of its requests is open there is
                // nothing left to wait for: its cleanup is requested instead
                // of a resume (contract section 9).
                let progressed = matches!(held, Ok(Step::Enrolled | Step::OriginStopped));
                // An enrollment job in flight, or a handle that is away, is
                // still on its way and is not yet a failure to retire.
                let in_flight = matches!(&held, Err(error) if matches!(
                    retry_code(error),
                    Some("decision_enrollment_pending" | "publication_pending")
                ));
                if !progressed && !in_flight && execution.open_requests == 0 {
                    self.child_request_cleanup(run)?;
                    return Ok(Step::CleanupRequested("bot_origin_stop_unproven"));
                }
                held
            }
            BotWaitPhase::Retired => {
                if execution.open_requests != 0 {
                    return Ok(Step::Waiting("bot_child_requests_open"));
                }
                self.child_reserve_step(owner, run, &execution)
            }
            BotWaitPhase::ResumeReserved => {
                let acknowledged = execution.resume.as_ref().is_some_and(|resume| {
                    matches!(resume.state, DecisionResumeState::InputAcknowledged { .. })
                });
                let step = self.child_successor_step(owner, run, &execution);
                let progressed = matches!(
                    step,
                    Ok(Step::SuccessorBound | Step::SuccessorInstalled | Step::InputAcknowledged)
                );
                if acknowledged || progressed {
                    self.children.stalled.remove(&wait);
                    return step;
                }
                // A reserved successor whose input is never acknowledged is
                // not bounded by its budget, which runs only once it is
                // activated: it is cleaned up after the stall limit.
                if resume_open(&execution) && self.child_stalled(wait) {
                    self.child_request_cleanup(run)?;
                    return Ok(Step::CleanupRequested("bot_child_stalled"));
                }
                step
            }
            // The cleanup owner has the wait; a settled wait is not listed.
            _ => Ok(Step::Waiting("bot_cleanup_owned")),
        }
    }

    /// Enrollment, then the origin stop.
    fn child_hold_step(
        &mut self,
        owner: BotOwner,
        run: RunId,
        execution: &ChildWaitExecution,
        bridge: &mut dyn ChildEnrollmentBridge,
    ) -> Result<ChildResumeStep, RunError> {
        use ChildResumeStep as Step;
        match &execution.wait.enrollment {
            None => {
                self.bot_enroll_child(owner, run, bridge)?;
                Ok(Step::Enrolled)
            }
            Some(BotChildWaitEnrollmentReply::Enrolled { .. }) => {
                match self.bot_stop_child_origin(owner, run)? {
                    Some(_) => Ok(Step::OriginStopped),
                    None => Ok(Step::Waiting("bot_origin_stop_unproven")),
                }
            }
            // Missing or Refused is never a stop.
            Some(_) => Ok(Step::Waiting("bot_enrollment_refused")),
        }
    }

    /// The successor half of the chain, from the retained resume row.
    fn child_successor_step(
        &mut self,
        owner: BotOwner,
        run: RunId,
        execution: &ChildWaitExecution,
    ) -> Result<ChildResumeStep, RunError> {
        use ChildResumeStep as Step;
        let Some(resume) = execution.resume.clone() else {
            return Ok(Step::Waiting("bot_cleanup_owned"));
        };
        let Some((attempt, generation, session, _)) = resume.consumer_identity() else {
            return Ok(Step::Waiting("bot_cleanup_owned"));
        };
        if matches!(resume.state, DecisionResumeState::Prepared {}) {
            return match self.bot_launch_child_successor(owner, run)? {
                Some(_) => Ok(Step::SuccessorBound),
                None => Ok(Step::Waiting("bot_successor_creation_unknown")),
            };
        }
        if matches!(resume.state, DecisionResumeState::Closed { .. }) {
            return Ok(Step::Waiting("bot_cleanup_owned"));
        }
        let acknowledged = resume.accepted_turn();
        let mirrored = self
            .runs
            .get(&run)
            .and_then(|state| state.attempt.as_ref())
            .is_some_and(|memory| {
                memory.id == attempt
                    && memory.generation == generation
                    && memory.session == session
                    && memory.holds_lease
                    && memory.resume_turn == acknowledged
            });
        if !mirrored {
            self.bot_install_child_successor(owner, run)?;
            return Ok(Step::SuccessorInstalled);
        }
        match resume.state {
            DecisionResumeState::SessionBound {} | DecisionResumeState::InputUnconfirmed {}
                if !resume_open(execution) =>
            {
                Ok(Step::Waiting("bot_cleanup_owned"))
            }
            DecisionResumeState::SessionBound {} | DecisionResumeState::InputUnconfirmed {} => {
                match self.bot_submit_child_continuation(owner, run)? {
                    Some(recorded) => match recorded.resume.map(|resume| resume.state) {
                        Some(DecisionResumeState::InputAcknowledged { .. }) => {
                            Ok(Step::InputAcknowledged)
                        }
                        _ => Ok(Step::InputUnconfirmed),
                    },
                    None => Ok(Step::Waiting("bot_continuation_unconfirmed")),
                }
            }
            // Acknowledged: the wait settles when the Control that clears
            // the waiting marker is applied.
            _ => Ok(Step::Waiting("publication_pending")),
        }
    }
}
