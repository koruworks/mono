// SPDX-License-Identifier: MPL-2.0

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    KoruEdit,
    NativeApproval,
    PrivateOperation,
}
impl DecisionKind {
    /// Whether a decision of this kind is a Koru decision: it waits by
    /// stopping its origin and resumes in a recorded successor, never through
    /// a native prompt. Every "is this a Koru wait" site asks this one
    /// predicate rather than naming a kind (M5 contract 8.4).
    pub fn resumes_by_successor(self) -> bool {
        match self {
            Self::KoruEdit | Self::PrivateOperation => true,
            Self::NativeApproval => false,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAuthorization {
    Pending,
    Granted,
    Denied,
    Canceled,
    Expired,
    Consumed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionPhase {
    Publishing,
    Holding,
    Held,
    NativeWait,
    ResumePrepared,
    AwaitRetry,
    Effect,
    NativeDispatch,
    NativeUnknown,
    StopPending,
    Settled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionClosureReason {
    Completed,
    Denied,
    Canceled,
    Expired,
    ClockUncertain,
    AuthorityChanged,
    Unsupported,
    EffectNotApplied,
    EffectUnknown,
    NativeUnknown,
    AiTerminal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionState {
    pub kind: DecisionKind,
    pub authorization: DecisionAuthorization,
    pub phase: DecisionPhase,
    pub revision: DecisionRevision,
}
impl DecisionValidate for DecisionState {
    fn validate(&self) -> Result<(), DecisionError> {
        use DecisionAuthorization as A;
        use DecisionPhase as P;
        let pair = match self.authorization {
            A::Pending => matches!(
                self.phase,
                P::Publishing | P::Holding | P::Held | P::NativeWait
            ),
            A::Granted => matches!(
                self.phase,
                P::Held
                    | P::ResumePrepared
                    | P::AwaitRetry
                    | P::NativeWait
                    | P::NativeDispatch
                    | P::NativeUnknown
            ),
            A::Consumed => matches!(
                self.phase,
                P::Effect | P::NativeDispatch | P::NativeUnknown | P::StopPending | P::Settled
            ),
            A::Denied => matches!(
                self.phase,
                P::NativeDispatch | P::NativeUnknown | P::StopPending | P::Settled
            ),
            A::Canceled | A::Expired => matches!(self.phase, P::StopPending | P::Settled),
        };
        require(
            pair && match self.kind {
                DecisionKind::KoruEdit | DecisionKind::PrivateOperation => !matches!(
                    self.phase,
                    P::NativeWait | P::NativeDispatch | P::NativeUnknown
                ),
                DecisionKind::NativeApproval => {
                    !matches!(self.phase, P::ResumePrepared | P::AwaitRetry | P::Effect)
                }
            },
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotWaitPhase {
    Fenced,
    HoldRequested,
    Held,
    Retired,
    ResumeReserved,
    CleanupPending,
    Settled,
}
impl BotWaitPhase {
    /// Shape-only monotonicity. Actual transition still requires exact row CAS,
    /// observed current attempt and retained proof; this never releases a slot.
    pub fn permits(self, next: Self, has_resume: bool) -> bool {
        use BotWaitPhase::*;
        if has_resume && matches!(next, Held | Retired) {
            return false;
        }
        self == next
            || matches!(
                (self, next),
                (Fenced, HoldRequested | CleanupPending | Settled)
                    | (HoldRequested, Held | Retired | CleanupPending)
                    | (Held | Retired, ResumeReserved | CleanupPending)
                    | (ResumeReserved, CleanupPending | Settled)
                    | (CleanupPending, Settled)
            )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotWaitReason {
    Decision { decision: DecisionId },
    Capacity {},
    NativeExpired {},
    OverlappingPrompt {},
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionDeadline {
    pub created_boot: crate::DelegateBootId,
    pub created_at_ms: i64,
    pub deadline_at_ms: i64,
    pub native_deadline_at_ms: Option<i64>,
}
impl DecisionValidate for DecisionDeadline {
    fn validate(&self) -> Result<(), DecisionError> {
        let full = self
            .created_at_ms
            .checked_add(DECISION_WAIT_MILLIS)
            .ok_or(DecisionError::Invalid)?;
        let expected = self
            .native_deadline_at_ms
            .map_or(full, |native| native.min(full));
        require(expected > self.created_at_ms && self.deadline_at_ms == expected)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "choice", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionAnswerChoice {
    AllowOnce {},
    Deny {},
    Native { offered: DecisionNativeChoice },
}
impl DecisionAnswerChoice {
    pub fn matches_binding(&self, binding: &DecisionBindingV1) -> bool {
        match (self, binding) {
            (
                Self::AllowOnce {} | Self::Deny {},
                DecisionBindingV1::KoruEdit(_) | DecisionBindingV1::PrivateOperation(_),
            ) => true,
            (Self::Native { offered }, DecisionBindingV1::NativeApproval(v)) => {
                v.choices.as_slice().contains(offered)
            }
            _ => false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionAnswerReceipt {
    pub decision: DecisionId,
    pub request: DecisionAnswerId,
    pub expected_revision: DecisionRevision,
    pub committed_revision: DecisionRevision,
    pub binding_digest: DecisionDigest,
    pub human_profile: crate::ProfileId,
    pub choice: DecisionAnswerChoice,
    pub answered_at_ms: i64,
    pub authority_digest: DecisionDigest,
}
impl DecisionValidate for DecisionAnswerReceipt {
    fn validate(&self) -> Result<(), DecisionError> {
        require(
            self.expected_revision.get().checked_add(1) == Some(self.committed_revision.get()),
        )?;
        bounded::encode(self, DECISION_RECEIPT_BYTES).map(|_| ())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionDisposition {
    Open {
        state: DecisionState,
    },
    Settled {
        state: DecisionState,
        reason: DecisionClosureReason,
        outcome_digest: DecisionDigest,
    },
}
impl DecisionValidate for DecisionDisposition {
    fn validate(&self) -> Result<(), DecisionError> {
        match self {
            Self::Open { state } => {
                state.validate()?;
                require(state.phase != DecisionPhase::Settled)
            }
            Self::Settled { state, .. } => {
                state.validate()?;
                require(state.phase == DecisionPhase::Settled)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionClosure {
    pub reason: DecisionClosureReason,
    pub settled_at_ms: i64,
}
/// Complete retained decision value. The SQL owner must also compare all scalar
/// columns/related receipts and enforce atomic transitions; decoding is not CAS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionReceipt {
    pub decision: DecisionId,
    pub binding: DecisionBindingV1,
    pub binding_digest: DecisionDigest,
    pub deadline: DecisionDeadline,
    pub state: DecisionState,
    pub answer: Option<DecisionAnswerReceipt>,
    pub consumed_at_ms: Option<i64>,
    pub closure: Option<DecisionClosure>,
    pub last_observed_at_ms: i64,
    pub updated_at_ms: i64,
}
impl DecisionValidate for DecisionReceipt {
    fn validate(&self) -> Result<(), DecisionError> {
        self.state.validate()?;
        self.deadline.validate()?;
        require(
            self.binding.digest()? == self.binding_digest
                && self.binding.kind() == self.state.kind
                && (self.state.phase == DecisionPhase::Settled) == self.closure.is_some()
                && (self.state.authorization == DecisionAuthorization::Consumed)
                    == self.consumed_at_ms.is_some(),
        )?;
        match &self.binding {
            DecisionBindingV1::KoruEdit(_) | DecisionBindingV1::PrivateOperation(_) => {
                require(self.deadline.native_deadline_at_ms.is_none())?
            }
            DecisionBindingV1::NativeApproval(v) => {
                require(self.deadline.native_deadline_at_ms == v.native_deadline_at_ms)?
            }
        }
        let needs_answer = matches!(
            self.state.authorization,
            DecisionAuthorization::Granted
                | DecisionAuthorization::Denied
                | DecisionAuthorization::Consumed
        );
        require(!needs_answer || self.answer.is_some())?;
        if let Some(answer) = &self.answer {
            answer.validate()?;
            require(
                answer.decision == self.decision
                    && answer.binding_digest == self.binding_digest
                    && answer.choice.matches_binding(&self.binding)
                    && answer.committed_revision <= self.state.revision,
            )?;
            require(answer.human_profile == self.binding.human_profile())?;
            let grants = match &answer.choice {
                DecisionAnswerChoice::AllowOnce {} => true,
                DecisionAnswerChoice::Deny {} => false,
                DecisionAnswerChoice::Native { offered } => offered.intent.grants(),
            };
            match self.state.authorization {
                DecisionAuthorization::Pending => return Err(DecisionError::Invalid),
                DecisionAuthorization::Granted | DecisionAuthorization::Consumed => {
                    require(grants)?
                }
                DecisionAuthorization::Denied => require(!grants)?,
                DecisionAuthorization::Canceled | DecisionAuthorization::Expired => {}
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionNativeResponseKey {
    pub decision: DecisionId,
    pub session: crate::SessionId,
    pub turn: crate::AgentTurnId,
    pub prompt: crate::ApprovalId,
    pub response_key: crate::IdempotencyKey,
    pub choice: DecisionNativeChoice,
}
impl DecisionNativeResponseKey {
    pub fn matches_binding(&self, decision: DecisionId, binding: &DecisionBindingV1) -> bool {
        matches!(binding,DecisionBindingV1::NativeApproval(v) if self.decision==decision
            && self.session==v.origin_session && self.turn==v.origin_turn && self.prompt==v.prompt
            && v.choices.as_slice().contains(&self.choice))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionNativeResponse {
    Prepared {
        key: DecisionNativeResponseKey,
    },
    Unconfirmed {
        key: DecisionNativeResponseKey,
    },
    Acknowledged {
        key: DecisionNativeResponseKey,
        acknowledgment: DecisionNativeResponseKey,
        observed_at_ms: i64,
    },
}
impl DecisionValidate for DecisionNativeResponse {
    fn validate(&self) -> Result<(), DecisionError> {
        if let Self::Acknowledged {
            key,
            acknowledgment,
            ..
        } = self
        {
            require(key == acknowledgment)?;
        }
        bounded::encode(self, DECISION_RECEIPT_BYTES).map(|_| ())
    }
}
