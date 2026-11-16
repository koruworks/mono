// SPDX-License-Identifier: MPL-2.0

use super::*;

/// The content-free request to hold a child wait's origin, frozen in the
/// transaction that admits the ask. It names no decision: a child wait has
/// none. Historical enrollment is not evidence that the stop happened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotChildWaitCleanupIntentV1 {
    pub version: DecisionVersionV1,
    pub owner: BotOwner,
    pub bot: BotId,
    pub run: RunId,
    pub wait_id: BotWaitId,
    pub wait_revision: DecisionRevision,
    pub origin_attempt: RunAttemptId,
    #[serde(deserialize_with = "generation")]
    pub origin_generation: AttemptGeneration,
    pub origin_session: SessionId,
    pub origin_turn: AgentTurnId,
    pub cause: BotWaitCleanupCause,
}
impl BotChildWaitCleanupIntentV1 {
    /// The intent a wait born at `origin` freezes. Its revision is 1: the
    /// wait row is written once, in the admitting transaction.
    pub fn frozen(wait: BotWaitId, origin: &DecisionFence) -> Result<Self, DecisionError> {
        let value = Self {
            version: DecisionVersionV1,
            owner: origin.owner,
            bot: origin.bot,
            run: origin.run,
            wait_id: wait,
            wait_revision: DecisionRevision::new(1)?,
            origin_attempt: origin.attempt,
            origin_generation: origin.generation,
            origin_session: origin.session,
            origin_turn: origin.turn,
            cause: BotWaitCleanupCause::ChildWaitCleanup,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn origin(&self) -> DecisionFence {
        DecisionFence {
            owner: self.owner,
            bot: self.bot,
            run: self.run,
            attempt: self.origin_attempt,
            generation: self.origin_generation,
            session: self.origin_session,
            turn: self.origin_turn,
        }
    }
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, DecisionError> {
        self.validate()?;
        encode_decision_value(self, DECISION_EVIDENCE_BYTES)
    }
    pub fn digest(&self) -> Result<DecisionDigest, DecisionError> {
        self.validate()?;
        crate::routine_digest_json(
            b"koru.bot.child.wait-enrollment.v1",
            self,
            DECISION_EVIDENCE_BYTES,
        )
        .map_err(Into::into)
    }
    pub fn key(&self) -> Result<BotChildWaitEnrollmentKey, DecisionError> {
        Ok(BotChildWaitEnrollmentKey {
            owner: self.owner,
            bot: self.bot,
            run: self.run,
            wait_id: self.wait_id,
            wait_revision: self.wait_revision,
            origin_attempt: self.origin_attempt,
            origin_generation: self.origin_generation,
            origin_session: self.origin_session,
            origin_turn: self.origin_turn,
            receipt_digest: self.digest()?,
        })
    }
}
impl DecisionValidate for BotChildWaitCleanupIntentV1 {
    fn validate(&self) -> Result<(), DecisionError> {
        require(self.cause == BotWaitCleanupCause::ChildWaitCleanup)?;
        self.origin().validate()
    }
}

/// The intent's fields without `cause`, plus the intent's digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotChildWaitEnrollmentKey {
    pub owner: BotOwner,
    pub bot: BotId,
    pub run: RunId,
    pub wait_id: BotWaitId,
    pub wait_revision: DecisionRevision,
    pub origin_attempt: RunAttemptId,
    #[serde(deserialize_with = "generation")]
    pub origin_generation: AttemptGeneration,
    pub origin_session: SessionId,
    pub origin_turn: AgentTurnId,
    pub receipt_digest: DecisionDigest,
}
impl BotChildWaitEnrollmentKey {
    pub fn origin(&self) -> DecisionFence {
        DecisionFence {
            owner: self.owner,
            bot: self.bot,
            run: self.run,
            attempt: self.origin_attempt,
            generation: self.origin_generation,
            session: self.origin_session,
            turn: self.origin_turn,
        }
    }
}
impl DecisionValidate for BotChildWaitEnrollmentKey {
    fn validate(&self) -> Result<(), DecisionError> {
        self.origin().validate()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollBotChildWaitCleanup {
    pub key: BotChildWaitEnrollmentKey,
    pub cleanup_intent: BotChildWaitCleanupIntentV1,
}
impl DecisionValidate for EnrollBotChildWaitCleanup {
    fn validate(&self) -> Result<(), DecisionError> {
        require(self.cleanup_intent.key()? == self.key)?;
        encode_decision_value(self, DECISION_EVIDENCE_BYTES).map(|_| ())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadBotChildWaitCleanup {
    pub key: BotChildWaitEnrollmentKey,
}
impl DecisionValidate for ReadBotChildWaitCleanup {
    fn validate(&self) -> Result<(), DecisionError> {
        self.key.validate()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotChildWaitEnrollmentReply {
    Enrolled {
        key: BotChildWaitEnrollmentKey,
        ai_attempt: Uuid,
        receipt_digest: DecisionDigest,
    },
    Missing {
        key: BotChildWaitEnrollmentKey,
    },
    Refused {
        code: BotWaitRefusal,
    },
}
impl DecisionValidate for BotChildWaitEnrollmentReply {
    fn validate(&self) -> Result<(), DecisionError> {
        match self {
            Self::Enrolled {
                key,
                receipt_digest,
                ..
            } => {
                key.validate()?;
                require(key.receipt_digest == *receipt_digest)?;
                encode_decision_value(self, DECISION_EVIDENCE_BYTES).map(|_| ())
            }
            Self::Missing { key } => key.validate(),
            Self::Refused { .. } => Ok(()),
        }
    }
}
impl BotChildWaitEnrollmentReply {
    /// Enroll cannot turn uncertainty or absence into a successful Missing.
    pub fn matches_request(&self, key: &BotChildWaitEnrollmentKey, is_read: bool) -> bool {
        self.validate().is_ok()
            && match self {
                Self::Enrolled { key: k, .. } => k == key,
                Self::Missing { key: k } => is_read && k == key,
                Self::Refused { .. } => true,
            }
    }
}

/// The parent's resume after a child wait: always a recorded successor,
/// keyed by the wait. The same shape as a decision's resume without the
/// decision, so one row decoder serves both.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildResume {
    pub wait: BotWaitId,
    pub origin: DecisionFence,
    pub consumer: DecisionConsumer,
    pub input_key: IdempotencyKey,
    pub input_fingerprint: [u8; 32],
    pub state: DecisionResumeState,
}
impl ChildResume {
    /// Attempt, generation, session and creation key of the successor.
    pub fn consumer_identity(
        &self,
    ) -> Option<(
        RunAttemptId,
        AttemptGeneration,
        Option<SessionId>,
        IdempotencyKey,
    )> {
        match self.consumer {
            DecisionConsumer::SameSession { .. } => None,
            DecisionConsumer::RecordedSuccessor {
                attempt,
                generation,
                creation_key,
                session,
            } => Some((attempt, generation, session, creation_key)),
        }
    }
    /// The turn the successor's continuation was acknowledged under.
    pub fn accepted_turn(&self) -> Option<AgentTurnId> {
        match &self.state {
            DecisionResumeState::InputAcknowledged { acknowledgment }
            | DecisionResumeState::Closed {
                acknowledgment: Some(acknowledgment),
            } => Some(acknowledgment.turn),
            _ => None,
        }
    }
    /// The successor as a cleanup target, re-resolved from this retained row.
    pub fn cleanup_target(&self) -> Result<DecisionCleanupTarget, DecisionError> {
        self.validate()?;
        let (attempt, generation, session, creation_key) =
            self.consumer_identity().ok_or(DecisionError::Invalid)?;
        Ok(DecisionCleanupTarget::Consumer {
            origin: self.origin,
            attempt,
            generation,
            creation_key,
            session,
            input_key: self.input_key,
            accepted_turn: self.accepted_turn(),
        })
    }
}
impl DecisionValidate for ChildResume {
    fn validate(&self) -> Result<(), DecisionError> {
        self.origin.validate()?;
        // A child wait never resumes in the stopped session.
        let (attempt, generation, session, _) =
            self.consumer_identity().ok_or(DecisionError::Invalid)?;
        require(
            attempt != self.origin.attempt
                && self.origin.generation.get().checked_add(1) == Some(generation.get())
                && session != Some(self.origin.session),
        )?;
        require(
            matches!(
                self.state,
                DecisionResumeState::Prepared {} | DecisionResumeState::Closed { .. }
            ) || session.is_some(),
        )?;
        if let DecisionResumeState::Closed {
            acknowledgment: Some(_),
        } = &self.state
        {
            require(session.is_some())?;
        }
        Ok(())
    }
}

/// Whether a child wait is still open, and why it closed without a resume.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChildWaitDisposition {
    Open {},
    /// `cause` is `canceled` (a Stop, the budget, an unqualified wait) or
    /// `ai_terminal` (the origin's own turn was delivered).
    Closed {
        cause: DecisionClosureReason,
    },
}
impl DecisionValidate for ChildWaitDisposition {
    fn validate(&self) -> Result<(), DecisionError> {
        match self {
            Self::Open {} => Ok(()),
            Self::Closed { cause } => require(matches!(
                cause,
                DecisionClosureReason::Canceled | DecisionClosureReason::AiTerminal
            )),
        }
    }
}

/// The delegate-owned observation of one child wait, for the AI owner's
/// terminal interceptor. Read-only and historical: it never grants
/// permission to stop anything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotChildWaitObservation {
    pub key: BotChildWaitEnrollmentKey,
    pub cleanup_intent: BotChildWaitCleanupIntentV1,
    pub current_revision: DecisionRevision,
    pub fresh_stop_eligibility: FreshWaitStopEligibility,
    pub origin_stop: OriginWaitStop,
    pub consumer_cleanup: ConsumerCleanup,
    pub disposition: ChildWaitDisposition,
    pub resume: Option<ChildResume>,
}
impl DecisionValidate for BotChildWaitObservation {
    fn validate(&self) -> Result<(), DecisionError> {
        require(
            self.cleanup_intent.key()? == self.key
                && self.current_revision >= self.key.wait_revision,
        )?;
        self.consumer_cleanup.validate()?;
        self.disposition.validate()?;
        if let OriginWaitStop::Confirmed {
            origin_fence,
            wait_id,
            intent_digest,
            enrollment_digest,
            cause,
            native_drain,
            ..
        } = &self.origin_stop
        {
            native_drain.validate()?;
            require(
                *origin_fence == self.cleanup_intent.origin()
                    && *wait_id == self.key.wait_id
                    && *intent_digest == self.key.receipt_digest
                    && *enrollment_digest == self.key.receipt_digest
                    && *cause == BotWaitCleanupCause::ChildWaitCleanup
                    && native_drain.run == origin_fence.run
                    && native_drain.attempt == origin_fence.attempt
                    && native_drain.generation == origin_fence.generation,
            )?;
        }
        encode_decision_value(&self.origin_stop, DECISION_EVIDENCE_BYTES)?;
        if let ConsumerCleanup::Owed { target, .. } | ConsumerCleanup::Confirmed { target, .. } =
            &self.consumer_cleanup
        {
            require(*target.origin() == self.cleanup_intent.origin())?;
        }
        if let Some(resume) = &self.resume {
            resume.validate()?;
            require(resume.wait == self.key.wait_id && resume.origin == self.key.origin())?;
        }
        Ok(())
    }
}

/// The wait as a reader of the parent Run sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildWaitView {
    pub wait: BotWaitId,
    pub phase: BotWaitPhase,
    pub revision: DecisionRevision,
    pub open_requests: u8,
}
