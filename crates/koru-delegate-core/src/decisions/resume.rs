// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{
    AgentTurnId, AttemptGeneration, IdempotencyKey, NativeResultKey, RunAttemptId, SessionId,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionConsumer {
    SameSession {
        creation_key: IdempotencyKey,
    },
    RecordedSuccessor {
        attempt: RunAttemptId,
        #[serde(deserialize_with = "identity::generation")]
        generation: AttemptGeneration,
        creation_key: IdempotencyKey,
        session: Option<SessionId>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionInputAcknowledgment {
    pub turn: AgentTurnId,
    pub receipt_digest: DecisionDigest,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionResumeState {
    Prepared {},
    SessionBound {},
    InputUnconfirmed {},
    InputAcknowledged {
        acknowledgment: DecisionInputAcknowledgment,
    },
    Closed {
        acknowledgment: Option<DecisionInputAcknowledgment>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionResume {
    pub decision: DecisionId,
    pub origin: DecisionFence,
    pub consumer: DecisionConsumer,
    pub input_key: IdempotencyKey,
    pub input_fingerprint: [u8; 32],
    pub state: DecisionResumeState,
}
impl DecisionResume {
    pub fn consumer_identity(
        &self,
    ) -> (
        RunAttemptId,
        AttemptGeneration,
        Option<SessionId>,
        IdempotencyKey,
    ) {
        match self.consumer {
            DecisionConsumer::SameSession { creation_key } => (
                self.origin.attempt,
                self.origin.generation,
                Some(self.origin.session),
                creation_key,
            ),
            DecisionConsumer::RecordedSuccessor {
                attempt,
                generation,
                creation_key,
                session,
            } => (attempt, generation, session, creation_key),
        }
    }
    /// Return a candidate full key, never a lease. The ledger must prove this
    /// exact acknowledgment/current fence and fresh authority atomically.
    pub fn candidate_native_key(
        &self,
        binding: &DecisionBindingV1,
    ) -> Result<NativeResultKey, DecisionError> {
        self.validate()?;
        binding.validate()?;
        require(self.origin == binding.origin())?;
        let DecisionBindingV1::KoruEdit(edit) = binding else {
            return Err(DecisionError::Invalid);
        };
        let DecisionResumeState::InputAcknowledged { acknowledgment } = &self.state else {
            return Err(DecisionError::Invalid);
        };
        let (attempt, generation, _, _) = self.consumer_identity();
        Ok(NativeResultKey {
            run: self.origin.run,
            attempt,
            generation,
            turn: acknowledgment.turn,
            operation_id: edit.operation_id,
        })
    }
}
impl DecisionValidate for DecisionResume {
    fn validate(&self) -> Result<(), DecisionError> {
        self.origin.validate()?;
        let (attempt, generation, session, _) = self.consumer_identity();
        match self.consumer {
            DecisionConsumer::SameSession { .. } => {}
            DecisionConsumer::RecordedSuccessor { .. } => {
                require(
                    attempt != self.origin.attempt
                        && generation.get() > 0
                        && self.origin.generation.get().checked_add(1) == Some(generation.get())
                        && session != Some(self.origin.session),
                )?;
            }
        }
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

/// Retained first consuming key: later lookup must use it unchanged even if the
/// caller has moved to another generation. Constructing this grants no effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionEffectProof {
    pub decision: DecisionId,
    pub binding_digest: DecisionDigest,
    pub answer: DecisionAnswerId,
    pub revision: DecisionRevision,
    pub consumer: NativeResultKey,
}
impl DecisionValidate for DecisionEffectProof {
    fn validate(&self) -> Result<(), DecisionError> {
        require(self.consumer.generation.get() > 0 && identity::v4(self.consumer.operation_id))
    }
}

/// No invented session for a creation with unknown completion. The target must
/// be re-resolved from the same retained resume row after every external phase.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionCleanupTarget {
    Origin {
        fence: DecisionFence,
        creation_key: IdempotencyKey,
    },
    Consumer {
        origin: DecisionFence,
        attempt: RunAttemptId,
        #[serde(deserialize_with = "identity::generation")]
        generation: AttemptGeneration,
        creation_key: IdempotencyKey,
        session: Option<SessionId>,
        input_key: IdempotencyKey,
        accepted_turn: Option<AgentTurnId>,
    },
}
impl DecisionCleanupTarget {
    pub fn from_resume(resume: &DecisionResume) -> Result<Self, DecisionError> {
        resume.validate()?;
        let (attempt, generation, session, creation_key) = resume.consumer_identity();
        let accepted_turn = match &resume.state {
            DecisionResumeState::InputAcknowledged { acknowledgment }
            | DecisionResumeState::Closed {
                acknowledgment: Some(acknowledgment),
            } => Some(acknowledgment.turn),
            _ => None,
        };
        Ok(Self::Consumer {
            origin: resume.origin,
            attempt,
            generation,
            creation_key,
            session,
            input_key: resume.input_key,
            accepted_turn,
        })
    }
    pub fn origin(&self) -> &DecisionFence {
        match self {
            Self::Origin { fence, .. } => fence,
            Self::Consumer { origin, .. } => origin,
        }
    }
    pub fn creation_key(&self) -> IdempotencyKey {
        match self {
            Self::Origin { creation_key, .. } | Self::Consumer { creation_key, .. } => {
                *creation_key
            }
        }
    }
    pub fn session(&self) -> Option<SessionId> {
        match self {
            Self::Origin { fence, .. } => Some(fence.session),
            Self::Consumer { session, .. } => *session,
        }
    }
    pub fn attempt(&self) -> (RunAttemptId, AttemptGeneration) {
        match self {
            Self::Origin { fence, .. } => (fence.attempt, fence.generation),
            Self::Consumer {
                attempt,
                generation,
                ..
            } => (*attempt, *generation),
        }
    }
}
impl DecisionValidate for DecisionCleanupTarget {
    fn validate(&self) -> Result<(), DecisionError> {
        self.origin().validate()?;
        match self {
            Self::Origin { .. } => Ok(()),
            Self::Consumer {
                origin,
                attempt,
                generation,
                session,
                accepted_turn,
                ..
            } => {
                require(accepted_turn.is_none() || session.is_some())?;
                if *attempt == origin.attempt {
                    require(*generation == origin.generation && *session == Some(origin.session))
                } else {
                    require(
                        origin.generation.get().checked_add(1) == Some(generation.get())
                            && *session != Some(origin.session),
                    )
                }
            }
        }
    }
}
