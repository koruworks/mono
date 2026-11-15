// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{
    AgentTurnId, AttemptGeneration, BotId, BotOwner, HostInstanceId, IdempotencyKey,
    NativeEffectDrainReceipt, NativeResultKey, NativeResultReference, NativeResultRefusal,
    RunAttemptId, RunId, SessionId,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotWaitCleanupCause {
    DecisionWaitCleanup,
}

/// Immutable digest-free request, frozen with post-transition revisions before
/// HTTP. Historical enrollment is not evidence that the physical stop happened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotWaitCleanupIntentV1 {
    pub version: DecisionVersionV1,
    pub owner: BotOwner,
    pub bot: BotId,
    pub run: RunId,
    pub wait_id: BotWaitId,
    pub decision_id: DecisionId,
    pub decision_revision: DecisionRevision,
    pub wait_revision: DecisionRevision,
    pub origin_attempt: RunAttemptId,
    #[serde(deserialize_with = "identity::generation")]
    pub origin_generation: AttemptGeneration,
    pub origin_session: SessionId,
    pub origin_turn: AgentTurnId,
    pub cause: BotWaitCleanupCause,
}
impl BotWaitCleanupIntentV1 {
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
        bounded::encode(self, DECISION_EVIDENCE_BYTES)
    }
    pub fn digest(&self) -> Result<DecisionDigest, DecisionError> {
        self.validate()?;
        crate::routine_digest_json(
            b"koru.bot.decision.wait-enrollment.v1",
            self,
            DECISION_EVIDENCE_BYTES,
        )
        .map_err(Into::into)
    }
    pub fn key(&self) -> Result<BotWaitEnrollmentKey, DecisionError> {
        Ok(BotWaitEnrollmentKey {
            owner: self.owner,
            bot: self.bot,
            run: self.run,
            wait_id: self.wait_id,
            decision_id: self.decision_id,
            decision_revision: self.decision_revision,
            wait_revision: self.wait_revision,
            origin_attempt: self.origin_attempt,
            origin_generation: self.origin_generation,
            origin_session: self.origin_session,
            origin_turn: self.origin_turn,
            receipt_digest: self.digest()?,
        })
    }
}
impl DecisionValidate for BotWaitCleanupIntentV1 {
    fn validate(&self) -> Result<(), DecisionError> {
        self.origin().validate()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotWaitEnrollmentKey {
    pub owner: BotOwner,
    pub bot: BotId,
    pub run: RunId,
    pub wait_id: BotWaitId,
    pub decision_id: DecisionId,
    pub decision_revision: DecisionRevision,
    pub wait_revision: DecisionRevision,
    pub origin_attempt: RunAttemptId,
    #[serde(deserialize_with = "identity::generation")]
    pub origin_generation: AttemptGeneration,
    pub origin_session: SessionId,
    pub origin_turn: AgentTurnId,
    pub receipt_digest: DecisionDigest,
}
impl DecisionValidate for BotWaitEnrollmentKey {
    fn validate(&self) -> Result<(), DecisionError> {
        DecisionFence {
            owner: self.owner,
            bot: self.bot,
            run: self.run,
            attempt: self.origin_attempt,
            generation: self.origin_generation,
            session: self.origin_session,
            turn: self.origin_turn,
        }
        .validate()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollBotWaitCleanup {
    pub key: BotWaitEnrollmentKey,
    pub cleanup_intent: BotWaitCleanupIntentV1,
}
impl DecisionValidate for EnrollBotWaitCleanup {
    fn validate(&self) -> Result<(), DecisionError> {
        require(self.cleanup_intent.key()? == self.key)?;
        bounded::encode(self, DECISION_EVIDENCE_BYTES).map(|_| ())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadBotWaitCleanup {
    pub key: BotWaitEnrollmentKey,
}
impl DecisionValidate for ReadBotWaitCleanup {
    fn validate(&self) -> Result<(), DecisionError> {
        self.key.validate()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotWaitRefusal {
    Invalid,
    Forbidden,
    Conflict,
    Stale,
    Unavailable,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotWaitEnrollmentReply {
    Enrolled {
        key: BotWaitEnrollmentKey,
        ai_attempt: uuid::Uuid,
        receipt_digest: DecisionDigest,
    },
    Missing {
        key: BotWaitEnrollmentKey,
    },
    Refused {
        code: BotWaitRefusal,
    },
}
impl DecisionValidate for BotWaitEnrollmentReply {
    fn validate(&self) -> Result<(), DecisionError> {
        match self {
            // ai_attempt is deliberately any UUID; the owning AI parser has
            // that retained domain. Fresh minting is an AI writer obligation.
            Self::Enrolled {
                key,
                receipt_digest,
                ..
            } => {
                key.validate()?;
                require(key.receipt_digest == *receipt_digest)?;
                bounded::encode(self, DECISION_EVIDENCE_BYTES).map(|_| ())
            }
            Self::Missing { key } => key.validate(),
            Self::Refused { .. } => Ok(()),
        }
    }
}
impl BotWaitEnrollmentReply {
    /// Enroll cannot turn uncertainty/absence into a successful Missing reply.
    pub fn matches_request(&self, key: &BotWaitEnrollmentKey, is_read: bool) -> bool {
        self.validate().is_ok()
            && match self {
                Self::Enrolled { key: k, .. } => k == key,
                Self::Missing { key: k } => is_read && k == key,
                Self::Refused { .. } => true,
            }
    }
}

/// A positive *observed retire-call result*, not an already durable receipt.
/// NoEvidence from ordinary lookup is insufficient: retire must have fenced
/// future creates first. Host/attempt owners verify this provenance in C/D.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedCreationRetirement {
    pub creation_key: IdempotencyKey,
    pub host: HostInstanceId,
    pub result: ObservedRetirementResult,
    pub observed_at_ms: i64,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservedRetirementResult {
    NoEvidence {},
    KnownEnded {
        session: SessionId,
    },
    KnownExited {
        session: SessionId,
        code: Option<i32>,
    },
    KnownFailed {
        session: SessionId,
        error: koru_session_protocol::ErrorCode,
    },
}
impl std::fmt::Debug for ObservedRetirementResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ObservedRetirementResult([REDACTED])")
    }
}
impl ObservedCreationRetirement {
    pub fn matches_target(&self, target: &DecisionCleanupTarget) -> bool {
        self.creation_key == target.creation_key()
            && match self.result {
                ObservedRetirementResult::NoEvidence {} => true,
                ObservedRetirementResult::KnownEnded { session }
                | ObservedRetirementResult::KnownExited { session, .. }
                | ObservedRetirementResult::KnownFailed { session, .. } => {
                    target.session() == Some(session)
                }
            }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionAttemptRetirement {
    pub run: RunId,
    pub attempt: RunAttemptId,
    #[serde(deserialize_with = "identity::generation")]
    pub generation: AttemptGeneration,
    pub retired_at_ms: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionNativeDrain {
    pub run: RunId,
    pub attempt: RunAttemptId,
    #[serde(deserialize_with = "identity::generation")]
    pub generation: AttemptGeneration,
    pub observed_at_ms: i64,
    pub effects: DecisionList<NativeEffectDrainReceipt, DECISION_EVIDENCE_BYTES>,
}
impl DecisionValidate for DecisionNativeDrain {
    fn validate(&self) -> Result<(), DecisionError> {
        require(
            self.generation.get() > 0
                && self
                    .effects
                    .as_slice()
                    .iter()
                    .all(|v| v.binding.valid() && v.drained_at >= v.fenced_at),
        )?;
        bounded::encode(self, DECISION_EVIDENCE_BYTES).map(|_| ())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionRetainedEffect {
    NotApplied {
        key: NativeResultKey,
        code: NativeResultRefusal,
    },
    Durable {
        key: NativeResultKey,
        reference: NativeResultReference,
    },
    DrainedUnknown {
        key: NativeResultKey,
        drain: NativeEffectDrainReceipt,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin_stop", rename_all = "snake_case", deny_unknown_fields)]
pub enum OriginWaitStop {
    Pending {},
    Confirmed {
        origin_fence: DecisionFence,
        wait_id: BotWaitId,
        intent_digest: DecisionDigest,
        enrollment_digest: DecisionDigest,
        cause: BotWaitCleanupCause,
        creation_retirement: ObservedCreationRetirement,
        native_drain: DecisionNativeDrain,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cleanup", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConsumerCleanup {
    NotRequested {},
    Owed {
        cleanup_revision: DecisionRevision,
        target: DecisionCleanupTarget,
    },
    Confirmed {
        cleanup_revision: DecisionRevision,
        target: DecisionCleanupTarget,
        creation_retirement: ObservedCreationRetirement,
        attempt_retirement: DecisionAttemptRetirement,
        native_drain: DecisionNativeDrain,
        retained_effect_outcome: Option<DecisionRetainedEffect>,
    },
}
impl DecisionValidate for ConsumerCleanup {
    fn validate(&self) -> Result<(), DecisionError> {
        match self {
            Self::NotRequested {} => {}
            Self::Owed { target, .. } => target.validate()?,
            Self::Confirmed {
                target,
                creation_retirement,
                attempt_retirement,
                native_drain,
                retained_effect_outcome,
                ..
            } => {
                target.validate()?;
                native_drain.validate()?;
                let (attempt, generation) = target.attempt();
                require(
                    creation_retirement.matches_target(target)
                        && attempt_retirement.run == target.origin().run
                        && attempt_retirement.attempt == attempt
                        && attempt_retirement.generation == generation
                        && native_drain.run == target.origin().run
                        && native_drain.attempt == attempt
                        && native_drain.generation == generation,
                )?;
                if let Some(effect) = retained_effect_outcome {
                    let key = match effect {
                        DecisionRetainedEffect::NotApplied { key, .. }
                        | DecisionRetainedEffect::Durable { key, .. }
                        | DecisionRetainedEffect::DrainedUnknown { key, .. } => key,
                    };
                    let turn = match target {
                        DecisionCleanupTarget::Origin { fence, .. } => Some(fence.turn),
                        DecisionCleanupTarget::Consumer { accepted_turn, .. } => *accepted_turn,
                    };
                    require(
                        key.run == target.origin().run
                            && key.attempt == attempt
                            && key.generation == generation
                            && Some(key.turn) == turn
                            && identity::v4(key.operation_id),
                    )?;
                    match effect {
                        DecisionRetainedEffect::Durable { reference, .. } => {
                            require(reference.valid())?
                        }
                        DecisionRetainedEffect::DrainedUnknown { drain, .. } => {
                            require(drain.binding.valid() && drain.drained_at >= drain.fenced_at)?
                        }
                        DecisionRetainedEffect::NotApplied { .. } => {}
                    }
                }
            }
        }
        bounded::encode(self, DECISION_EVIDENCE_BYTES).map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshWaitStopEligibility {
    EligibleOriginWaitStop,
    Superseded,
    Terminal,
}

/// Future authenticated read payload, not a registered IPC reply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotDecisionWaitObservation {
    pub key: BotWaitEnrollmentKey,
    pub cleanup_intent: BotWaitCleanupIntentV1,
    pub current_revision: DecisionRevision,
    pub fresh_stop_eligibility: FreshWaitStopEligibility,
    pub origin_stop: OriginWaitStop,
    pub consumer_cleanup: ConsumerCleanup,
    pub disposition: DecisionDisposition,
    pub publication: DecisionPublication,
}
impl DecisionValidate for BotDecisionWaitObservation {
    fn validate(&self) -> Result<(), DecisionError> {
        require(
            self.cleanup_intent.key()? == self.key
                && self.current_revision >= self.key.wait_revision,
        )?;
        self.consumer_cleanup.validate()?;
        self.disposition.validate()?;
        let state = match &self.disposition {
            DecisionDisposition::Open { state } | DecisionDisposition::Settled { state, .. } => {
                state
            }
        };
        require(
            state.kind == DecisionKind::KoruEdit && state.revision >= self.key.decision_revision,
        )?;
        if let OriginWaitStop::Confirmed {
            origin_fence,
            wait_id,
            intent_digest,
            enrollment_digest,
            creation_retirement,
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
                    && native_drain.run == origin_fence.run
                    && native_drain.attempt == origin_fence.attempt
                    && native_drain.generation == origin_fence.generation
                    && creation_retirement.matches_target(&DecisionCleanupTarget::Origin {
                        fence: *origin_fence,
                        creation_key: creation_retirement.creation_key,
                    }),
            )?;
        }
        bounded::encode(&self.origin_stop, DECISION_EVIDENCE_BYTES)?;
        if let ConsumerCleanup::Owed { target, .. } | ConsumerCleanup::Confirmed { target, .. } =
            &self.consumer_cleanup
        {
            require(*target.origin() == self.cleanup_intent.origin())?;
        }
        Ok(())
    }
}
