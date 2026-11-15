// SPDX-License-Identifier: MPL-2.0

//! The per-turn resume driver. The actor calls it once per maintenance turn
//! for a Run with an open decision wait; it reads the retained wait and takes
//! at most one exact C transition (enrollment request, enrollment, origin
//! stop, reservation, binding, continuation input), or none. The choice of
//! path is made here, not in the actor: a native approval decision keeps its
//! occupied prompt and is answered on it (the human's answer route forwards
//! it with the journaled key), while a Koru edit decision takes the
//! stop-and-successor path only when the existing delegated launch can serve
//! it. Same-session capacity-releasing hold is never selected.

use super::*;
use koru_delegate_core::decisions::{
    BotWaitEnrollmentReply, BotWaitPhase, DecisionAuthorization as Authorization, DecisionKind,
    DecisionPhase as Phase, DecisionResumeState,
};

/// What one driver turn did for a Run's open decision wait. Each acting
/// variant names the single step taken. `Waiting` and `Unsupported` changed
/// nothing; neither is success, and neither releases capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionResumeStep {
    /// No open decision wait on this Run.
    Idle,
    /// Nothing this driver owns can advance the wait now; the code names
    /// what it is waiting for (publication, the human's answer, the cleanup
    /// owner, the AI's consume, or positive evidence not yet available).
    Waiting(&'static str),
    /// Stop-and-successor cannot be served here; the origin keeps running.
    Unsupported(&'static str),
    EnrollmentRequested,
    Enrolled,
    OriginStopped,
    SuccessorReserved,
    SuccessorBound,
    SuccessorInstalled,
    InputUnconfirmed,
    InputAcknowledged,
}

/// A refusal that only means "not yet": retry on a later turn.
fn retry_code(error: &RunError) -> Option<&'static str> {
    match error {
        RunError::Port(error) => match error.code.as_str() {
            "publication_pending" => Some("publication_pending"),
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
    /// Advance this Run's open decision wait by at most one exact step. The
    /// enrollment HTTP call goes out through `bridge` (the host's; there is
    /// no default) only while the fixed worker is not holding the document
    /// handle. The reservation runs under this process's own boot
    /// (`delegate_boot`). Retryable refusals become `Waiting`.
    pub fn bot_advance_decision_resume(
        &mut self,
        owner: BotOwner,
        run: RunId,
        bridge: &mut dyn DecisionEnrollmentBridge,
    ) -> Result<DecisionResumeStep, RunError> {
        match self.decision_resume_step(owner, run, bridge) {
            Err(error) => match retry_code(&error) {
                Some(code) => Ok(DecisionResumeStep::Waiting(code)),
                None => Err(error),
            },
            step => step,
        }
    }

    fn decision_resume_step(
        &mut self,
        owner: BotOwner,
        run: RunId,
        bridge: &mut dyn DecisionEnrollmentBridge,
    ) -> Result<DecisionResumeStep, RunError> {
        use DecisionResumeStep as Step;
        let Some(execution) = self.bot_ledger()?.decision_wait_execution(owner, run)? else {
            return Ok(Step::Idle);
        };
        // A no-decision cleanup wait belongs to its cleanup owner.
        let Some(state) = execution.decision_state.clone() else {
            return Ok(Step::Waiting("bot_cleanup_owned"));
        };
        if state.kind == DecisionKind::NativeApproval {
            // The occupied prompt is answered in place; no stop, no successor.
            return Ok(Step::Waiting("bot_native_occupied"));
        }
        if let Some(resume) = execution.resume.clone() {
            if execution.wait.phase != BotWaitPhase::ResumeReserved {
                // Stop, expiry or terminal cleanup owns this consumer now.
                return Ok(Step::Waiting("bot_cleanup_owned"));
            }
            let open = super::launch::resume_open(&execution);
            return self.decision_resume_successor(owner, run, &resume, open);
        }
        match (state.authorization, state.phase, execution.wait.phase) {
            (Authorization::Pending, Phase::Publishing, _) => {
                Ok(Step::Waiting("publication_pending"))
            }
            (Authorization::Pending, Phase::Holding, BotWaitPhase::Fenced)
                if execution.wait.enrollment_request.is_none() =>
            {
                // The choice: stop-and-successor only when the existing
                // delegated launch could serve the successor at all.
                if let Some(code) = self.decision_successor_unsupported(run)? {
                    return Ok(Step::Unsupported(code));
                }
                self.bot_request_decision_enrollment(owner, run)?;
                Ok(Step::EnrollmentRequested)
            }
            (Authorization::Pending, Phase::Holding, BotWaitPhase::HoldRequested) => {
                match &execution.wait.enrollment {
                    None => {
                        self.bot_enroll_decision(owner, run, bridge)?;
                        Ok(Step::Enrolled)
                    }
                    Some(BotWaitEnrollmentReply::Enrolled { .. }) => {
                        match self.bot_stop_decision_origin(owner, run)? {
                            Some(_) => Ok(Step::OriginStopped),
                            None => Ok(Step::Waiting("bot_origin_stop_unproven")),
                        }
                    }
                    // Missing or Refused is never a stop; expiry, a person's
                    // Stop or cancel owns what happens next.
                    Some(_) => Ok(Step::Waiting("bot_enrollment_refused")),
                }
            }
            (Authorization::Pending, Phase::Held, BotWaitPhase::Retired) => {
                Ok(Step::Waiting("bot_answer_pending"))
            }
            (Authorization::Granted, Phase::Held, BotWaitPhase::Retired) => {
                if let Some(code) = self.decision_successor_unsupported(run)? {
                    return Ok(Step::Unsupported(code));
                }
                let host = self.ports.sessions.host_instance()?;
                let fingerprint = self.bot_decision_continuation_fingerprint(owner, run)?;
                let proof = self.decision_policy_proof(run)?;
                let boot = self.delegate_boot();
                self.bot_reserve_decision_successor(owner, run, host, fingerprint, &proof, boot)?;
                Ok(Step::SuccessorReserved)
            }
            _ => Ok(Step::Waiting("bot_cleanup_owned")),
        }
    }

    /// Why the existing delegated launch cannot serve a successor for this
    /// Run, if it cannot. A capability bit alone never qualifies a release;
    /// this only refuses to start a chain that could not finish.
    pub(super) fn decision_successor_unsupported(
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
        if admission.submission.provenance.routine.is_some() {
            return Ok(Some("bot_routine_decision_unsupported"));
        }
        if !self
            .ports
            .sessions
            .profile_delegatable(&admission.submission.request.agent)?
        {
            return Ok(Some("bot_profile_unready"));
        }
        Ok(None)
    }

    /// The successor half of the chain, from the retained resume.
    fn decision_resume_successor(
        &mut self,
        owner: BotOwner,
        run: RunId,
        resume: &koru_delegate_core::decisions::DecisionResume,
        open: bool,
    ) -> Result<DecisionResumeStep, RunError> {
        use DecisionResumeStep as Step;
        let (attempt, generation, session, _) = resume.consumer_identity();
        if matches!(resume.state, DecisionResumeState::Prepared {}) {
            return match self.bot_launch_decision_successor(owner, run)? {
                Some(_) => Ok(Step::SuccessorBound),
                None => Ok(Step::Waiting("bot_successor_creation_unknown")),
            };
        }
        if matches!(resume.state, DecisionResumeState::Closed { .. }) {
            // Stop closed the input; final cleanup of this consumer is the
            // cleanup owner's, and nothing new is sent or attached here.
            return Ok(Step::Waiting("bot_cleanup_owned"));
        }
        let acknowledged = super::successor::acknowledged_turn(resume);
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
            self.bot_install_decision_successor(owner, run)?;
            return Ok(Step::SuccessorInstalled);
        }
        match resume.state {
            DecisionResumeState::SessionBound {} | DecisionResumeState::InputUnconfirmed {}
                if !open =>
            {
                Ok(Step::Waiting("bot_cleanup_owned"))
            }
            DecisionResumeState::SessionBound {} | DecisionResumeState::InputUnconfirmed {} => {
                match self.bot_submit_decision_continuation(owner, run)? {
                    Some(record) => match record.resume.map(|resume| resume.state) {
                        Some(DecisionResumeState::InputAcknowledged { .. }) => {
                            Ok(Step::InputAcknowledged)
                        }
                        _ => Ok(Step::InputUnconfirmed),
                    },
                    None => Ok(Step::Waiting("bot_continuation_unconfirmed")),
                }
            }
            // Acknowledged: the one-use consume is the AI's tool call.
            _ => Ok(Step::Waiting("bot_consume_owned")),
        }
    }
}
