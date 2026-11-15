// SPDX-License-Identifier: MPL-2.0

//! Closed projection input shared by the SQL decoder and document mapper.
//! It describes retained effects, never permission to perform an operation.

use super::*;
use crate::{ModelIdentity, RunEvent, RunFailureReason, RunStatus, TimelineEntry, transition_bot};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionProjectionPrior {
    pub transition: DecisionTransitionId,
    pub sequence: DecisionRevision,
    pub target_digest: DecisionDigest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionProjectionPredecessor {
    pub projection: Option<DecisionProjectionPrior>,
    #[serde(deserialize_with = "strict_leaf")]
    pub status: RunStatus,
    #[serde(deserialize_with = "strict_leaf")]
    pub failure_reason: Option<RunFailureReason>,
}

/// Allocated facts only: preparation never mints a step, model or event ID.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionProjectionEffects {
    #[serde(deserialize_with = "strict_leaf")]
    pub event: Option<RunEvent>,
    #[serde(deserialize_with = "strict_leaf")]
    pub status: RunStatus,
    #[serde(deserialize_with = "strict_leaf")]
    pub failure_reason: Option<RunFailureReason>,
    /// Frozen source observation time for the existing usage economics fold.
    /// Present exactly when this target appends a usage fact.
    pub economics_at_ms: Option<i64>,
    #[serde(deserialize_with = "strict_leaf")]
    pub steps: DecisionList<TimelineEntry, 65_536>,
    #[serde(deserialize_with = "strict_leaf")]
    pub models: DecisionList<ModelIdentity, 65_536>,
}
impl std::fmt::Debug for DecisionProjectionEffects {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DecisionProjectionEffects([REDACTED])")
    }
}

/// Actual non-stream owners. There is deliberately no generic RunEvent arm:
/// tool/turn/usage/gap observations cannot be hidden in coalescible Control.
/// Delivery needs its exact receipt owner and is not enabled by D1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionControlAction {
    Preserve {},
    DecisionWaiting {},
    DecisionResolved { outcome: crate::ApprovalOutcome },
    Stop {},
    Interrupt { prior_attempt: crate::PriorAttempt },
    Resume { via: crate::ResumeSeam },
    HandoverRequested {},
    ControlTaken { discovered: bool },
    ControlReturned {},
}
impl DecisionControlAction {
    fn mapping(self) -> (Option<RunEvent>, Option<crate::StepKind>) {
        use crate::{RunEvent as E, StepKind as S};
        match self {
            Self::Preserve {} => (None, None),
            Self::DecisionWaiting {} => (Some(E::ApprovalRequired), Some(S::ApprovalRequired)),
            Self::DecisionResolved { outcome } => (
                Some(E::ApprovalResolved),
                Some(S::ApprovalResolved { outcome }),
            ),
            Self::Stop {} => (
                Some(E::Cancel),
                Some(S::Cancelled {
                    reason: crate::CancelReason::Run,
                }),
            ),
            Self::Interrupt { prior_attempt } => (
                Some(E::Interrupt { prior_attempt }),
                Some(S::Interrupted { prior_attempt }),
            ),
            Self::Resume { via } => (Some(E::Resume), Some(S::Resumed { via })),
            Self::HandoverRequested {} => (Some(E::HandoverRequested), None),
            Self::ControlTaken { discovered } => {
                (Some(E::ControlTaken), Some(S::ControlTaken { discovered }))
            }
            Self::ControlReturned {} => (Some(E::ControlReturned), Some(S::ControlReturned)),
        }
    }
    fn validate_effects(
        self,
        before: &RunStatus,
        effects: &DecisionProjectionEffects,
    ) -> Result<(), DecisionError> {
        if before.is_terminal() {
            return require(effects.event.is_none() && effects.steps.as_slice().is_empty());
        }
        let (event, step) = self.mapping();
        require(
            effects.event == event
                && effects.steps.as_slice().len() <= 1
                && effects
                    .steps
                    .as_slice()
                    .iter()
                    .all(|entry| step.as_ref() == Some(&entry.kind)),
        )
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunProjectionTargetV1 {
    Source {
        version: DecisionVersionV1,
        predecessor: DecisionProjectionPredecessor,
        effects: DecisionProjectionEffects,
        bookkeeping: DecisionSourceBookkeeping,
    },
    Control {
        version: DecisionVersionV1,
        predecessor: DecisionProjectionPredecessor,
        effects: DecisionProjectionEffects,
        fence: DecisionFence,
        disposition: Option<DecisionDisposition>,
        wait_phase: BotWaitPhase,
        reason: BotWaitReason,
        action: DecisionControlAction,
        retained_bookkeeping: Option<DecisionSourceBookkeeping>,
    },
}
impl std::fmt::Debug for RunProjectionTargetV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RunProjectionTargetV1([REDACTED])")
    }
}
impl RunProjectionTargetV1 {
    pub fn kind(&self) -> DecisionProjectionKind {
        match self {
            Self::Source { .. } => DecisionProjectionKind::Source,
            Self::Control { .. } => DecisionProjectionKind::Control,
        }
    }
    pub fn predecessor(&self) -> &DecisionProjectionPredecessor {
        match self {
            Self::Source { predecessor, .. } | Self::Control { predecessor, .. } => predecessor,
        }
    }
    pub fn effects(&self) -> &DecisionProjectionEffects {
        match self {
            Self::Source { effects, .. } | Self::Control { effects, .. } => effects,
        }
    }
    pub fn fence(&self) -> &DecisionFence {
        match self {
            Self::Source { bookkeeping, .. } => &bookkeeping.boundary.fence,
            Self::Control { fence, .. } => fence,
        }
    }
    pub fn bookkeeping(&self) -> Option<&DecisionSourceBookkeeping> {
        match self {
            Self::Source { bookkeeping, .. } => Some(bookkeeping),
            Self::Control {
                retained_bookkeeping,
                ..
            } => retained_bookkeeping.as_ref(),
        }
    }
    pub fn source_bookkeeping(&self) -> Option<&DecisionSourceBookkeeping> {
        match self {
            Self::Source { bookkeeping, .. } => Some(bookkeeping),
            Self::Control { .. } => None,
        }
    }
    pub fn canonical_bytes(&self) -> Result<DecisionPayload, DecisionError> {
        self.validate()?;
        DecisionBytes::new(bounded::encode(self, DECISION_PAYLOAD_BYTES)?)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, DecisionError> {
        if bytes.len() > DECISION_PAYLOAD_BYTES {
            return Err(DecisionError::TooLarge);
        }
        let target: Self = serde_json::from_slice(bytes).map_err(|_| DecisionError::Invalid)?;
        target.validate()?;
        Ok(target)
    }
    pub fn digest(&self) -> Result<DecisionDigest, DecisionError> {
        self.validate()?;
        crate::routine_digest_json(
            b"koru.bot.decision.projection.target.v1",
            self,
            DECISION_PAYLOAD_BYTES,
        )
        .map_err(Into::into)
    }
    pub fn matches_identity(
        &self,
        identity: &DecisionProjectionIdentity,
    ) -> Result<(), DecisionError> {
        self.validate()?;
        require(
            self.kind() == identity.kind
                && self.fence().run == identity.run
                && self.digest()? == identity.target_digest,
        )?;
        if let Some(prior) = &self.predecessor().projection {
            require(prior.sequence < identity.sequence && prior.transition != identity.transition)?;
        }
        if let Self::Control {
            disposition,
            reason,
            ..
        } = self
        {
            match (disposition, &identity.decision, reason) {
                (Some(disposition), Some(revision), BotWaitReason::Decision { decision }) => {
                    let state = match disposition {
                        DecisionDisposition::Open { state }
                        | DecisionDisposition::Settled { state, .. } => state,
                    };
                    require(*decision == revision.decision && state.revision == revision.revision)?;
                }
                (
                    None,
                    None,
                    BotWaitReason::Capacity {}
                    | BotWaitReason::NativeExpired {}
                    | BotWaitReason::OverlappingPrompt {},
                ) => {}
                _ => return Err(DecisionError::Invalid),
            }
        }
        Ok(())
    }
}
impl DecisionValidate for RunProjectionTargetV1 {
    fn validate(&self) -> Result<(), DecisionError> {
        self.fence().validate()?;
        let effects = self.effects();
        let before = &self.predecessor().status;
        let expected = match effects.event {
            // An already accepted terminal source cannot be undone by later Stop.
            Some(_) if before.is_terminal() => before.clone(),
            Some(event) => transition_bot(before, &event).map_err(|_| DecisionError::Invalid)?,
            None => before.clone(),
        };
        require(
            expected == effects.status
                && (effects.failure_reason.is_none()
                    || matches!(effects.status, RunStatus::Failed { .. })),
        )?;
        if effects.event.is_none() || before.is_terminal() {
            require(effects.failure_reason == self.predecessor().failure_reason)?;
        }
        let mut steps = BTreeSet::new();
        require(
            effects
                .steps
                .as_slice()
                .iter()
                .all(|step| steps.insert(step.id)),
        )?;
        let mut models = BTreeSet::new();
        require(
            effects
                .models
                .as_slice()
                .iter()
                .all(|model| models.insert(model.id)),
        )?;
        match self {
            Self::Source { bookkeeping, .. } => {
                bookkeeping.validate()?;
                let allocated: BTreeSet<_> = bookkeeping
                    .allocated_steps
                    .as_slice()
                    .iter()
                    .copied()
                    .collect();
                require(allocated == steps)?;
                require(bookkeeping.usage.is_some() == effects.economics_at_ms.is_some())?;
            }
            Self::Control {
                disposition,
                retained_bookkeeping,
                action,
                ..
            } => {
                action.validate_effects(before, effects)?;
                if let Some(disposition) = disposition {
                    disposition.validate()?;
                }
                if let Some(bookkeeping) = retained_bookkeeping {
                    bookkeeping.validate()?;
                    require(
                        bookkeeping.boundary.fence.run == self.fence().run
                            && bookkeeping.boundary.fence.owner == self.fence().owner
                            && bookkeeping.boundary.fence.bot == self.fence().bot,
                    )?;
                }
                require(effects.models.as_slice().is_empty())?;
                require(effects.economics_at_ms.is_none())?;
                if before.is_terminal() {
                    require(effects.event.is_none() && effects.steps.as_slice().is_empty())?;
                }
            }
        }
        // Count with the existing bounded streaming digest owner before copying.
        crate::routine_digest_json(
            b"koru.bot.decision.projection.target.v1",
            self,
            DECISION_PAYLOAD_BYTES,
        )?;
        Ok(())
    }
}

/// Immutable Run subject commitment. Mutable status, delivery and result fields
/// are deliberately outside this identity; the owning decoder validates those.
pub fn decision_projection_subject_digest(
    header: &crate::RunHeader,
) -> Result<DecisionDigest, DecisionError> {
    require(header.bot.is_some() && header.bot_fields_valid())?;
    #[derive(Serialize)]
    struct Subject<'a> {
        run: RunId,
        created_at_ms: i64,
        requested_by: &'a crate::ActorRef,
        objective: &'a crate::Objective,
        agent: &'a crate::SessionProfileKey,
        requested_model: &'a Option<crate::ModelName>,
        context: &'a [crate::RunContextItem],
        grants: &'a std::collections::BTreeSet<WorkspaceId>,
        bot: &'a crate::BotProvenance,
        origin: &'a crate::RunOrigin,
        scope: &'a crate::WorkspaceScope,
        environment: &'a crate::ExecutionEnvironment,
    }
    crate::routine_digest_json(
        b"koru.bot.decision.projection.subject.v1",
        &Subject {
            run: header.run,
            created_at_ms: header.created_at_ms,
            requested_by: &header.requested_by,
            objective: &header.objective,
            agent: &header.agent,
            requested_model: &header.requested_model,
            context: &header.context,
            grants: &header.grants,
            bot: header.bot.as_ref().ok_or(DecisionError::Invalid)?,
            origin: &header.origin,
            scope: &header.scope,
            environment: &header.environment,
        },
        DECISION_FRAME_BYTES,
    )
    .map_err(Into::into)
}

/// Exact byte-array commitment, using the same bounded canonical JSON digest
/// owner as target/subject. VV equality remains semantic at the document owner.
pub fn decision_projection_delta_digest(
    delta: &DecisionPreparedDelta,
) -> Result<DecisionDigest, DecisionError> {
    crate::routine_digest_json(
        b"koru.bot.decision.projection.delta.v1",
        delta,
        DECISION_FRAME_BYTES,
    )
    .map_err(Into::into)
}

/// Existing ordinary leaf codecs intentionally tolerate future fields. A new
/// retained projection cannot silently discard them. Only this closed envelope
/// applies the exact roundtrip check; ordinary Run decoding is unchanged.
fn strict_leaf<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Serialize,
{
    let raw = serde_json::Value::deserialize(deserializer)?;
    let value: T = serde_json::from_value(raw.clone())
        .map_err(|_| serde::de::Error::custom(DecisionError::Invalid))?;
    let encoded = serde_json::to_value(&value)
        .map_err(|_| serde::de::Error::custom(DecisionError::Invalid))?;
    if raw != encoded {
        return Err(serde::de::Error::custom(DecisionError::Invalid));
    }
    Ok(value)
}
