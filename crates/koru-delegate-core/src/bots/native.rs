// SPDX-License-Identifier: MPL-2.0

//! Neutral native-result and current-policy evidence; values grant no authority.
use crate::{
    AgentTurnId, AttemptGeneration, CapturedBotPolicy, DocumentId, NativeResultReference,
    RunAttemptId, RunId, WorkspaceId,
};
pub use koru_store_core::model::Timestamp as NativeEffectTimestamp;
pub use koru_store_core::{NativeEffectBinding, NativeEffectDrainReceipt, NativeEffectKind};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
mod policy_id;
pub use policy_id::NativePolicyRequestId;

/// Server-derived classification after a Run bearer has been authenticated.
/// Unknown or stale state is an error, never an Ordinary classification.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunToolAuthority {
    Ordinary {},
    Bot {
        provenance: crate::BotProvenance,
        policy: CapturedBotPolicy,
    },
    /// IPC 16 (decision contract section 3): the same Bot Run while its
    /// decision wait is open. Never Ordinary and never a lease: callers allow
    /// reads, retained replay and the exact decision retry, and refuse new
    /// unrelated mutations. `phase` is the decision's phase as the delegate
    /// last read it from its ledger.
    BotDecisionWait {
        provenance: crate::BotProvenance,
        policy: CapturedBotPolicy,
        decision: crate::decisions::DecisionId,
        phase: crate::decisions::DecisionPhase,
    },
}
impl RunToolAuthority {
    /// The Bot subject of either Bot classification; `None` for Ordinary.
    pub fn bot_subject(&self) -> Option<(&crate::BotProvenance, &CapturedBotPolicy)> {
        match self {
            Self::Ordinary {} => None,
            Self::Bot { provenance, policy }
            | Self::BotDecisionWait {
                provenance, policy, ..
            } => Some((provenance, policy)),
        }
    }
}
impl std::fmt::Debug for RunToolAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RunToolAuthority([REDACTED])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotPolicyBarrierKey {
    pub owner: crate::BotOwner,
    pub bot: crate::BotId,
    pub request: NativePolicyRequestId,
}

/// A current AI-owned policy snapshot for effect fencing, never a second
/// canonical configuration. P1.4 acquires this before taking an effect lease.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotPolicyBarrier {
    pub key: BotPolicyBarrierKey,
    pub config_revision: u64,
    pub policy_revision: u64,
    pub policy: CapturedBotPolicy,
    pub effects_allowed: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotPolicyBarrierState {
    Pending,
    Effective,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotPolicyBarrierReceipt {
    pub barrier: BotPolicyBarrier,
    pub state: BotPolicyBarrierState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeResultKey {
    pub run: RunId,
    pub attempt: RunAttemptId,
    pub generation: AttemptGeneration,
    pub turn: AgentTurnId,
    pub operation_id: Uuid,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeResultOperation {
    CreatePage,
    EditPage,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareNativeResult {
    pub key: NativeResultKey,
    pub kind: NativeResultOperation,
    pub destination: WorkspaceId,
    pub target: Option<DocumentId>,
    pub request_fingerprint: [u8; 32],
    pub expected_revision: Option<Vec<u8>>,
    /// Fresh authority obtained before the actor grants the effect lease.
    /// Once leased, Store commit/reconcile must not call back into AI.
    pub config_revision: u64,
    pub policy_revision: u64,
    pub title: String,
}
impl std::fmt::Debug for PrepareNativeResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrepareNativeResult")
            .field("key", &self.key)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
impl PrepareNativeResult {
    pub fn valid(&self) -> bool {
        self.config_revision > 0
            && self.config_revision <= i64::MAX as u64
            && self.policy_revision > 0
            && self.policy_revision <= i64::MAX as u64
            && self.key.operation_id.get_version() == Some(uuid::Version::Random)
            && self.key.operation_id.get_variant() == uuid::Variant::RFC4122
            && !self.title.trim().is_empty()
            && self.title.len() <= 256
            && match self.kind {
                NativeResultOperation::CreatePage => {
                    self.target.is_none() && self.expected_revision.is_none()
                }
                NativeResultOperation::EditPage => {
                    self.target.is_some()
                        && self
                            .expected_revision
                            .as_ref()
                            .is_some_and(|v| !v.is_empty() && v.len() <= 65536)
                }
            }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeResultRefusal {
    AccessDenied,
    InvalidInput,
    Conflict,
    Unavailable,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "observation", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeResultObservation {
    NotApplied {
        code: NativeResultRefusal,
    },
    Unconfirmed,
    Durable {
        evidence: NativeResultReference,
    },
    DrainedUnknown {
        receipt: koru_store_core::NativeEffectDrainReceipt,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmNativeResult {
    pub key: NativeResultKey,
    pub request_fingerprint: [u8; 32],
    pub observation: NativeResultObservation,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeResultStage {
    Prepared,
    Unconfirmed,
    SettledUnknown,
    Verified,
    Registered,
    Refused,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeResultDisposition {
    Execute,
    ReconcileOnly,
    AlreadyRegistered,
    Refused,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeResultIntentView {
    pub key: NativeResultKey,
    pub purpose: super::NativeEffectPurpose,
    pub kind: NativeResultOperation,
    pub destination: WorkspaceId,
    pub request_fingerprint: [u8; 32],
    pub target: DocumentId,
    pub store_request: Uuid,
    pub stage: NativeResultStage,
    pub verified: Option<NativeResultReference>,
    pub disposition: NativeResultDisposition,
}

/// Stable, bounded keyset cursor. Wall time orders pages, never authorizes an effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRecoveryCursor {
    pub created_at_ms: i64,
    pub run: RunId,
    pub operation_id: Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePendingEffect {
    pub owner: crate::BotOwner,
    pub bot: crate::BotId,
    pub key: NativeResultKey,
    pub binding: koru_store_core::NativeEffectBinding,
    pub cursor: NativeRecoveryCursor,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePendingEffects {
    pub targets: Vec<NativePendingEffect>,
    pub next: Option<NativeRecoveryCursor>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum NativeDrainReason {
    RunRecovery,
    PolicyPending {
        key: BotPolicyBarrierKey,
    },
    RoutinePolicyPending {
        owner: crate::BotOwner,
        bot: crate::BotId,
        schedule: crate::ScheduleId,
        request: crate::NativePolicyRequestId,
    },
    OwnerStop,
    TerminalTurn,
    UnconfirmedOperation,
    /// Only trusted gateway-process startup may supply this, while its native gate is closed.
    GatewayStartup,
}

impl<'de> Deserialize<'de> for NativeDrainReason {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Serde's internally tagged unit variants ignore extra fields even
        // with deny_unknown_fields. Empty struct variants enforce the boundary.
        #[derive(Deserialize)]
        #[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
        enum Wire {
            RunRecovery {},
            PolicyPending {
                key: BotPolicyBarrierKey,
            },
            RoutinePolicyPending {
                owner: crate::BotOwner,
                bot: crate::BotId,
                schedule: crate::ScheduleId,
                request: crate::NativePolicyRequestId,
            },
            OwnerStop {},
            TerminalTurn {},
            UnconfirmedOperation {},
            GatewayStartup {},
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::RunRecovery {} => Self::RunRecovery,
            Wire::PolicyPending { key } => Self::PolicyPending { key },
            Wire::RoutinePolicyPending {
                owner,
                bot,
                schedule,
                request,
            } => Self::RoutinePolicyPending {
                owner,
                bot,
                schedule,
                request,
            },
            Wire::OwnerStop {} => Self::OwnerStop,
            Wire::TerminalTurn {} => Self::TerminalTurn,
            Wire::UnconfirmedOperation {} => Self::UnconfirmedOperation,
            Wire::GatewayStartup {} => Self::GatewayStartup,
        })
    }
}

pub fn native_store_scope(owner: crate::BotOwner, bot: crate::BotId, run: RunId) -> String {
    format!("koru.bot.native.v1:{}:{}:{}", owner.user_scope, bot, run)
}

impl NativeResultIntentView {
    pub fn store_binding(
        &self,
        owner: crate::BotOwner,
        bot: crate::BotId,
    ) -> koru_store_core::NativeEffectBinding {
        koru_store_core::NativeEffectBinding {
            user_scope: owner.user_scope,
            scope: native_store_scope(owner, bot, self.key.run),
            request_key: self.store_request,
            workspace: self.destination,
            document: self.target,
            kind: match self.kind {
                NativeResultOperation::CreatePage => {
                    koru_store_core::NativeEffectKind::CreateDocument
                }
                NativeResultOperation::EditPage => koru_store_core::NativeEffectKind::EditDocument,
            },
            request_fingerprint: self.request_fingerprint,
        }
    }
}
