// SPDX-License-Identifier: MPL-2.0

//! The Koru edit resume chain, one exact C transition per step. Each step
//! reads the current retained ticket, supplies only positive physical
//! evidence, and is safe to retry: C replays an exact retained transition.
//! The enrollment HTTP call itself belongs to the host (author A) behind the
//! narrow `DecisionEnrollmentBridge`; it runs only after the fixed worker
//! has returned the Run's document handle (plan section 4).

use super::*;
use koru_delegate_core::decisions::{
    BotWaitCleanupCause, BotWaitEnrollmentReply, DecisionExpected, DecisionInputAcknowledgment,
    DecisionNativeDrain, EnrollBotWaitCleanup, ObservedCreationRetirement,
    ObservedRetirementResult, OriginWaitStop,
};
use koru_delegate_core::{AgentTurnId, HostInstanceId, IdempotencyKey, SessionId};
use koru_session_protocol::{CreationLookup, ProcessState};

/// The host's enrollment client: one finite HTTP request to the AI worker
/// carrying the frozen intent, answering its Enrolled/Refused reply. There is
/// no default; a host without it cannot resume a Koru decision.
pub trait DecisionEnrollmentBridge {
    fn enroll(
        &mut self,
        request: &EnrollBotWaitCleanup,
    ) -> crate::PortResult<BotWaitEnrollmentReply>;
}

impl Orchestrator {
    pub(super) fn decision_ticket(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<koru_delegate_sqlite::DecisionWaitExecution, RunError> {
        self.bot_ledger()?
            .decision_wait_execution(owner, run)?
            .ok_or_else(|| refused("bot_fenced"))
    }

    pub(super) fn decision_expected(
        execution: &koru_delegate_sqlite::DecisionWaitExecution,
    ) -> Result<DecisionExpected, RunError> {
        execution
            .expected
            .decision
            .clone()
            .ok_or_else(|| refused("bot_fenced"))
    }

    /// Freeze the AI cleanup intent (decision Holding). Returns the exact
    /// request the host must send; it is only sent once the document handle
    /// is attached here (not in flight), so no handle crosses HTTP.
    pub fn bot_request_decision_enrollment(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<EnrollBotWaitCleanup, RunError> {
        let execution = self.decision_ticket(owner, run)?;
        let record = match execution.wait.enrollment_request.clone() {
            Some(request) => return Ok(request),
            None => self
                .bot_ledger()?
                .enroll_decision_cleanup(&execution.expected, DecisionTransitionId::mint())?,
        };
        record
            .wait
            .enrollment_request
            .ok_or_else(|| refused("bot_fenced"))
    }

    /// Send the frozen request through the host bridge and record the reply.
    pub fn bot_enroll_decision(
        &mut self,
        owner: BotOwner,
        run: RunId,
        bridge: &mut dyn DecisionEnrollmentBridge,
    ) -> Result<DecisionRecord, RunError> {
        if self.runs.get(&run).is_some_and(|state| {
            matches!(
                state.doc,
                crate::orchestrator::publication::RunDocumentOwner::PublicationInFlight { .. }
            )
        }) {
            // The worker still holds the handle: enrollment waits for it.
            return Err(crate::publication_pending().into());
        }
        let request = self.bot_request_decision_enrollment(owner, run)?;
        let reply = bridge.enroll(&request)?;
        let execution = self.decision_ticket(owner, run)?;
        let expected = Self::decision_expected(&execution)?;
        Ok(self.bot_ledger()?.record_decision_enrollment(
            &expected,
            DecisionTransitionId::mint(),
            &reply,
        )?)
    }

    /// Physically stop and retire the origin consumer, then record the
    /// immutable release with the complete drain. Positive proof only: an
    /// unproven end, a held lease or an unavailable drain records nothing.
    pub fn bot_stop_decision_origin(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<DecisionRecord>, RunError> {
        use koru_delegate_sqlite::AttemptPhase;
        let execution = self.decision_ticket(owner, run)?;
        if execution.resume.is_some() {
            return Err(refused("bot_fenced"));
        }
        let request = execution
            .wait
            .enrollment_request
            .clone()
            .ok_or_else(|| refused("bot_enrollment_required"))?;
        if execution.wait.enrollment.is_none() {
            return Err(refused("bot_enrollment_required"));
        }
        let origin = execution.wait.origin;
        let target = execution.cleanup_target.clone();
        let Some(observed) = self.observe_creation_end(run, &target)? else {
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
            Err(koru_delegate_sqlite::BotLedgerError::Fenced)
            | Err(koru_delegate_sqlite::BotLedgerError::Capacity) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let release = OriginWaitStop::Confirmed {
            origin_fence: origin,
            wait_id: execution.wait.wait,
            intent_digest: request.key.receipt_digest,
            enrollment_digest: request.key.receipt_digest,
            cause: BotWaitCleanupCause::DecisionWaitCleanup,
            creation_retirement: observed,
            native_drain,
        };
        let execution = self.decision_ticket(owner, run)?;
        let expected = Self::decision_expected(&execution)?;
        Ok(Some(self.bot_ledger()?.record_decision_origin_stop(
            &expected,
            DecisionTransitionId::mint(),
            &release,
        )?))
    }

    /// Terminate (best effort) and retire the exact creation; only a known
    /// ended/exited/failed or host-wide no-evidence answer is positive.
    fn observe_creation_end(
        &mut self,
        run: RunId,
        target: &koru_delegate_core::decisions::DecisionCleanupTarget,
    ) -> Result<Option<ObservedCreationRetirement>, RunError> {
        let key = target.creation_key();
        if let Some(session) = target.session()
            && let Err(error) = self.ports.sessions.terminate(session)
        {
            self.trace(
                run,
                "decision_terminate_refused",
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

    /// Reserve the one recorded successor after AllowOnce and origin release.
    /// Returns the successor's attempt identity and creation key; the host
    /// creates the session under exactly that key (a lost reply is recovered
    /// by `lookup(key)`, never a second creation).
    pub fn bot_reserve_decision_successor(
        &mut self,
        owner: BotOwner,
        run: RunId,
        host: HostInstanceId,
        fingerprint: [u8; 32],
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
    ) -> Result<koru_delegate_core::decisions::DecisionResume, RunError> {
        let execution = self.decision_ticket(owner, run)?;
        if let Some(resume) = execution.resume {
            return Ok(resume);
        }
        let expected = Self::decision_expected(&execution)?;
        let resume = self
            .bot_ledger()?
            .reserve_decision_successor(
                &expected,
                DecisionTransitionId::mint(),
                host,
                fingerprint,
                proof,
                boot,
            )?
            .resume
            .ok_or_else(|| refused("bot_fenced"))?;
        // Only a reservation this process made may ask for its creation; one
        // found already retained (a replay, or an earlier process) is
        // recovered by its exact creation key, never created a second time.
        let (_, _, _, creation) = resume.consumer_identity();
        if let Some(state) = self.runs.get_mut(&run) {
            state.decision_launch = Some(crate::orchestrator::state::DecisionLaunch::Reserved(
                creation,
            ));
        }
        Ok(resume)
    }

    /// Bind the created successor session, from the host's positive answer
    /// for the reserved creation key (including a recovered lost reply).
    pub fn bot_bind_decision_successor(
        &mut self,
        owner: BotOwner,
        run: RunId,
        creation: IdempotencyKey,
        host: HostInstanceId,
        session: SessionId,
    ) -> Result<DecisionRecord, RunError> {
        let execution = self.decision_ticket(owner, run)?;
        let expected = Self::decision_expected(&execution)?;
        Ok(self.bot_ledger()?.bind_decision_successor(
            &expected,
            DecisionTransitionId::mint(),
            creation,
            host,
            session,
        )?)
    }

    /// Record the exact continuation input under its retained key and
    /// fingerprint, with the accepted turn only when the host positively
    /// acknowledged it. An unconfirmed input stays unconfirmed: delivery and
    /// consume by the successor remain refused until acknowledgment.
    pub fn bot_record_decision_resume_input(
        &mut self,
        owner: BotOwner,
        run: RunId,
        accepted: Option<(AgentTurnId, DecisionDigest)>,
    ) -> Result<DecisionRecord, RunError> {
        let execution = self.decision_ticket(owner, run)?;
        let resume = execution
            .resume
            .clone()
            .ok_or_else(|| refused("bot_fenced"))?;
        let expected = Self::decision_expected(&execution)?;
        let acknowledgment = accepted.map(|(turn, receipt_digest)| DecisionInputAcknowledgment {
            turn,
            receipt_digest,
        });
        Ok(self.bot_ledger()?.record_decision_resume_input(
            &expected,
            DecisionTransitionId::mint(),
            resume.input_key,
            resume.input_fingerprint,
            acknowledgment.as_ref(),
        )?)
    }
}
