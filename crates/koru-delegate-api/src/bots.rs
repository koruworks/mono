// SPDX-License-Identifier: MPL-2.0

//! The private AI-owner to delegate boundary. Possessing a Run ID or an
//! ordinary `runs.*` scope does not grant access to this port.
use crate::wire::{RestoreRunResponse, RunView, TimelineQuery, TrashRunResponse};
use koru_delegate_core::{
    AgentEventObservation, AgentTurnId, ApprovalChoiceId, ApprovalDecision, ApprovalId, Attachment,
    AttachmentId, AttachmentSource, AttemptGeneration, BotAdmissionBarrier, BotDeliveryReceipt,
    BotExecutionFence, BotSubmission, CapturedBotPolicy, DocumentId, NativeResultReference,
    PendingInput, QuestionAnswer, QuestionRequestId, RunAttemptId, RunId, StreamSequence,
    WorkspaceId,
};
pub use koru_delegate_core::{BotAdmissionState, BotAdmissionView, BotRunKey, BotSubmissionKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Per-Agent-run secret passed only through trusted bootstrap. Not an owner
/// identity and not persisted with an operation or Run. Same-account process
/// memory access is outside this capability's isolation boundary.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BotDelegateAuthority(String);
impl TryFrom<String> for BotDelegateAuthority {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value).ok_or("invalid Bot delegate authority")
    }
}
impl From<BotDelegateAuthority> for String {
    fn from(value: BotDelegateAuthority) -> Self {
        value.0
    }
}
impl BotDelegateAuthority {
    pub fn new(value: String) -> Option<Self> {
        (value.len() >= 32 && value.len() <= 256 && !value.contains('\0')).then_some(Self(value))
    }
    pub fn matches(&self, other: &Self) -> bool {
        // Avoid a content-dependent early exit for equal-length capabilities.
        self.0.len() == other.0.len()
            && self
                .0
                .bytes()
                .zip(other.0.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }
}
impl std::fmt::Debug for BotDelegateAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotDelegateAuthority([REDACTED])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotObservation {
    pub fence: BotExecutionFence,
    pub observation: AgentEventObservation,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotCommand {
    Submit {
        submission: BotSubmission,
    },
    Reconcile {
        key: BotSubmissionKey,
    },
    Barrier {
        barrier: BotAdmissionBarrier,
    },
    Read {
        key: BotRunKey,
        #[serde(with = "StrictTimelineQuery")]
        window: TimelineQuery,
    },
    /// Reserved for P1.7. Results are always kept; no purge authority exists.
    Trash {
        key: BotRunKey,
        lifecycle: BotLifecycleFence,
    },
    Restore {
        key: BotRunKey,
        lifecycle: BotLifecycleFence,
    },
    Observe {
        fence: BotExecutionFence,
        after: Option<StreamSequence>,
    },
    /// Prior AI history may appear transiently in this input, never in the
    /// ledger. Stable send identity and semantic fingerprint bind retries.
    BeginTurn {
        fence: BotExecutionFence,
        #[serde(with = "koru_delegate_core::bot_wire::StrictPendingInput")]
        input: PendingInput,
    },
    Steer {
        fence: BotExecutionFence,
        #[serde(with = "koru_delegate_core::bot_wire::StrictPendingInput")]
        input: PendingInput,
    },
    /// Session upload limits apply, separately from the 4 MiB admission intent.
    /// The owner supplies the validated session and controller lease itself.
    IngestAttachment {
        fence: BotExecutionFence,
        attachment_id: AttachmentId,
        source: AttachmentSource,
        data_base64: String,
    },
    RemoveAttachment {
        fence: BotExecutionFence,
        attachment_id: AttachmentId,
    },
    ListAttachments {
        fence: BotExecutionFence,
    },
    /// None is only valid before an execution identity exists; active Stop
    /// requires the exact fence but never requires the automation lease.
    Stop {
        key: BotRunKey,
        fence: Option<BotExecutionFence>,
    },
    AnswerApproval {
        fence: BotExecutionFence,
        approval: ApprovalId,
        decision: ApprovalDecision,
        choice: Option<ApprovalChoiceId>,
    },
    AnswerQuestions {
        fence: BotExecutionFence,
        request: QuestionRequestId,
        #[serde(deserialize_with = "koru_delegate_core::bot_wire::deserialize_answers")]
        answers: Vec<QuestionAnswer>,
    },
    TakeOver {
        fence: BotExecutionFence,
    },
    HandBack {
        fence: BotExecutionFence,
    },
    EffectFence {
        fence: BotExecutionFence,
    },
    SynchronizePolicy {
        barrier: BotPolicyBarrier,
    },
    ReconcilePolicyBarrier {
        key: BotPolicyBarrierKey,
    },
    PrepareNativeResult {
        request: PrepareNativeResult,
    },
    ReconcileNativeResult {
        key: NativeResultKey,
    },
    ConfirmNativeResult {
        request: ConfirmNativeResult,
    },
    AcknowledgeDelivery {
        key: BotRunKey,
        receipt: BotDeliveryReceipt,
    },
}
impl BotCommand {
    /// The explicit owner asserted by the trusted AI owner. Native-result
    /// commands instead resolve their immutable Run binding inside the actor.
    pub fn owner(&self) -> Option<&koru_delegate_core::BotOwner> {
        match self {
            Self::Submit { submission } => Some(&submission.provenance.owner),
            Self::Reconcile { key } => Some(&key.owner),
            Self::Barrier { barrier } => Some(&barrier.owner),
            Self::Read { key, .. }
            | Self::Trash { key, .. }
            | Self::Restore { key, .. }
            | Self::Stop { key, .. }
            | Self::AcknowledgeDelivery { key, .. } => Some(&key.owner),
            Self::Observe { fence, .. }
            | Self::BeginTurn { fence, .. }
            | Self::Steer { fence, .. }
            | Self::IngestAttachment { fence, .. }
            | Self::RemoveAttachment { fence, .. }
            | Self::ListAttachments { fence }
            | Self::AnswerApproval { fence, .. }
            | Self::AnswerQuestions { fence, .. }
            | Self::TakeOver { fence }
            | Self::HandBack { fence }
            | Self::EffectFence { fence } => Some(&fence.owner),
            Self::SynchronizePolicy { barrier } => Some(&barrier.key.owner),
            Self::ReconcilePolicyBarrier { key } => Some(&key.owner),
            Self::PrepareNativeResult { .. }
            | Self::ReconcileNativeResult { .. }
            | Self::ConfirmNativeResult { .. } => None,
        }
    }
}
impl std::fmt::Debug for BotCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Inputs, titles, native revision bytes and capabilities must not leak
        // through a transport's derived request diagnostics.
        f.write_str("BotCommand([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotReply {
    Admission {
        admission: BotAdmissionView,
    },
    Missing,
    Barrier {
        barrier: BotAdmissionBarrier,
    },
    Run {
        view: RunView,
    },
    Trashed {
        response: TrashRunResponse,
    },
    Restored {
        response: RestoreRunResponse,
    },
    Observation {
        observation: BotObservation,
    },
    Turn {
        turn: AgentTurnId,
    },
    Applied,
    Attachment {
        attachment: Attachment,
    },
    Attachments {
        attachments: Vec<Attachment>,
    },
    EffectFence {
        fence: BotExecutionFence,
        policy: CapturedBotPolicy,
    },
    PolicyBarrier {
        receipt: BotPolicyBarrierReceipt,
    },
    NativeResult {
        intent: NativeResultIntentView,
    },
}
impl std::fmt::Debug for BotReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotReply([REDACTED])")
    }
}

/// Blocking on caller-owned preparation/provider threads, never the AI writer.
/// Implementations carry their private bootstrap authority; callers cannot
/// substitute an ordinary diagnostic principal label for it.
pub trait BotDelegatePort: Send + Sync {
    fn execute(&self, command: BotCommand) -> Result<BotReply, crate::Failure>;
}

/// Only the trusted gateway composition receives this credentialed adapter.
/// It never retries a mutation: unknown acknowledgement is reconciled by its
/// stable command identity through the same durable owner.
#[derive(Clone)]
pub struct LocalBotDelegate {
    transport: crate::LocalDelegate,
    authority: BotDelegateAuthority,
}
impl LocalBotDelegate {
    pub fn new(endpoint: koru_ipc::Endpoint, authority: BotDelegateAuthority) -> Self {
        Self {
            transport: crate::LocalDelegate::new(endpoint, "bot-owner"),
            authority,
        }
    }
}
impl BotDelegatePort for LocalBotDelegate {
    fn execute(&self, command: BotCommand) -> Result<BotReply, crate::Failure> {
        match self.transport.call(&crate::ipc::Request::Bot {
            authority: self.authority.clone(),
            command,
        })? {
            crate::ipc::Response::Bot(reply) => Ok(reply),
            crate::ipc::Response::Failure(failure) => Err(failure),
            _ => Err(crate::Failure::new(
                "delegate_unexpected_response",
                "The Bot owner received an unexpected response.",
                false,
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotPolicyBarrierKey {
    pub owner: koru_delegate_core::BotOwner,
    pub bot: koru_delegate_core::BotId,
    pub request: koru_delegate_core::BotRequestId,
}

/// P1.7 resolves this exact Effective barrier and requires its revision to
/// remain the latest installed epoch before applying a lifecycle operation.
/// A delayed Trash cannot overwrite a later Restore. P1.3 never fabricates an
/// effective receipt; both lifecycle commands remain explicitly unavailable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotLifecycleFence {
    pub barrier: BotPolicyBarrierKey,
    pub config_revision: u64,
    pub policy_revision: u64,
}
impl BotLifecycleFence {
    pub fn valid_for(&self, key: &BotRunKey) -> bool {
        self.barrier.owner == key.owner
            && self.barrier.bot == key.bot
            && self.config_revision > 0
            && self.config_revision <= i64::MAX as u64
            && self.policy_revision > 0
            && self.policy_revision <= i64::MAX as u64
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "TimelineQuery", deny_unknown_fields)]
struct StrictTimelineQuery {
    from: u64,
    len: u32,
    workspace: Option<WorkspaceId>,
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

#[cfg(test)]
mod tests;
