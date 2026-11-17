// SPDX-License-Identifier: MPL-2.0

//! The hold of a child wait: the AI owner's enrollment and the proven stop of
//! the parent's origin attempt (contract 4.1, steps 5 and 6).
//!
//! The cleanup intent was frozen in the transaction that admitted the ask,
//! so there is no enrollment request step. The enrollment HTTP call belongs
//! to the host behind [`ChildEnrollmentBridge`] and runs only while the
//! fixed worker is not holding the Run's document handle. The stop records
//! positive proof only: an unproven end, a held native lease or an
//! unavailable drain records nothing and the Run stays occupied.

use super::*;
use koru_delegate_core::children::{BotChildWaitEnrollmentReply, EnrollBotChildWaitCleanup};
use koru_delegate_core::decisions::{
    BotWaitCleanupCause, DecisionCleanupTarget, DecisionNativeDrain, ObservedCreationRetirement,
    ObservedRetirementResult, OriginWaitStop,
};
use koru_delegate_sqlite::{AttemptPhase, BotLedgerError, ChildWaitRecord};
use koru_session_protocol::{CreationLookup, ProcessState};

/// The host's enrollment client for a child wait: one finite HTTP request to
/// the AI worker carrying the frozen intent, answering its `Enrolled` reply.
/// A refusal is an error, never a reply. There is no default; a host without
/// it cannot hold a parent.
pub trait ChildEnrollmentBridge {
    fn enroll_child(
        &mut self,
        request: &EnrollBotChildWaitCleanup,
    ) -> crate::PortResult<BotChildWaitEnrollmentReply>;
}

impl Orchestrator {
    /// Send the frozen intent through the host bridge and retain the reply.
    pub fn bot_enroll_child(
        &mut self,
        owner: BotOwner,
        run: RunId,
        bridge: &mut dyn ChildEnrollmentBridge,
    ) -> Result<ChildWaitRecord, RunError> {
        if self.runs.get(&run).is_some_and(|state| {
            matches!(
                state.doc,
                crate::orchestrator::publication::RunDocumentOwner::PublicationInFlight { .. }
            )
        }) {
            // The worker still holds the handle: enrollment waits for it.
            return Err(crate::publication_pending().into());
        }
        let execution = self.child_ticket(owner, run)?;
        let intent = execution.wait.intent.clone();
        let request = EnrollBotChildWaitCleanup {
            key: intent.key().map_err(|_| refused("bot_invalid"))?,
            cleanup_intent: intent,
        };
        let reply = bridge.enroll_child(&request)?;
        let execution = self.child_ticket(owner, run)?;
        Ok(self
            .bot_ledger()?
            .record_child_enrollment(&execution.expected, &reply)?)
    }

    /// Physically stop and retire the origin consumer, then record the
    /// immutable release with the complete native drain. `Ok(None)` while
    /// the end is not proven.
    pub fn bot_stop_child_origin(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<ChildWaitRecord>, RunError> {
        let execution = self.child_ticket(owner, run)?;
        if execution.resume.is_some() {
            return Err(refused("bot_fenced"));
        }
        if !matches!(
            execution.wait.enrollment,
            Some(BotChildWaitEnrollmentReply::Enrolled { .. })
        ) {
            return Err(refused("bot_enrollment_required"));
        }
        let digest = execution
            .wait
            .intent
            .digest()
            .map_err(|_| refused("bot_invalid"))?;
        let origin = execution.wait.origin;
        let target = execution.cleanup_target.clone();
        let Some(observed) = self.child_observe_creation_end(run, &target)? else {
            return Ok(None);
        };
        let row = execution.current_attempt.clone();
        if row.attempt != origin.attempt
            || row.generation != origin.generation
            || row.host_instance != Some(observed.host)
        {
            return Ok(None);
        }
        if !row.retired {
            if !row.phase.is_terminal() {
                let phase = match observed.result {
                    ObservedRetirementResult::NoEvidence {} => AttemptPhase::Unknown,
                    _ => AttemptPhase::Ended,
                };
                self.ports.store.advance(run, origin.generation, phase)?;
            }
            self.ports.store.retire(run, origin.generation)?;
        }
        let now = self.ports.clock.now_ms();
        let native_drain: DecisionNativeDrain = match self.bot_ledger()?.decision_native_drain(
            owner,
            run,
            origin.attempt,
            origin.generation,
            now,
        ) {
            Ok(drain) => drain,
            Err(BotLedgerError::Fenced) | Err(BotLedgerError::Capacity) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let release = OriginWaitStop::Confirmed {
            origin_fence: origin,
            wait_id: execution.wait.wait,
            intent_digest: digest,
            enrollment_digest: digest,
            cause: BotWaitCleanupCause::ChildWaitCleanup,
            creation_retirement: observed,
            native_drain,
        };
        let execution = self.child_ticket(owner, run)?;
        Ok(Some(
            self.bot_ledger()?
                .record_child_origin_stop(&execution.expected, &release)?,
        ))
    }

    /// Terminate (best effort) and retire the exact creation; only a known
    /// ended, exited or failed answer, or host-wide no evidence, is positive.
    fn child_observe_creation_end(
        &mut self,
        run: RunId,
        target: &DecisionCleanupTarget,
    ) -> Result<Option<ObservedCreationRetirement>, RunError> {
        let key = target.creation_key();
        if let Some(session) = target.session()
            && let Err(error) = self.ports.sessions.terminate(session)
        {
            self.trace(
                run,
                "child_terminate_refused",
                format_args!("{}", error.code),
            );
        }
        let (host, retired) = self.ports.sessions.retire(key)?;
        let result = match retired {
            CreationLookup::NoEvidence => ObservedRetirementResult::NoEvidence {},
            CreationLookup::KnownEnded { session } => ObservedRetirementResult::KnownEnded {
                session: session.id,
            },
            CreationLookup::Known { session } => match session.process {
                ProcessState::Exited { code } => ObservedRetirementResult::KnownExited {
                    session: session.id,
                    code,
                },
                ProcessState::Failed { error } => ObservedRetirementResult::KnownFailed {
                    session: session.id,
                    error: error.code,
                },
                ProcessState::Starting | ProcessState::Running => return Ok(None),
            },
            CreationLookup::Creating => return Ok(None),
        };
        let observed = ObservedCreationRetirement {
            creation_key: key,
            host,
            result,
            observed_at_ms: self.ports.clock.now_ms(),
        };
        Ok(observed.matches_target(target).then_some(observed))
    }
}
