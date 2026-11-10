// SPDX-License-Identifier: MPL-2.0

//! Neutral native-result and current-policy evidence; values grant no authority.
use crate::{
    AgentTurnId, AttemptGeneration, CapturedBotPolicy, DocumentId, NativeResultReference,
    RunAttemptId, RunId, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    pub request: crate::BotRequestId,
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
    NotApplied { code: NativeResultRefusal },
    Unconfirmed,
    Durable { evidence: NativeResultReference },
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
    pub kind: NativeResultOperation,
    pub destination: WorkspaceId,
    pub request_fingerprint: [u8; 32],
    pub target: DocumentId,
    pub store_request: Uuid,
    pub stage: NativeResultStage,
    pub verified: Option<NativeResultReference>,
    pub disposition: NativeResultDisposition,
}
