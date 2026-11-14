// SPDX-License-Identifier: MPL-2.0

//! The private AI-owner to delegate boundary. Possessing a Run ID or an
//! ordinary `runs.*` scope does not grant access to this port.
use crate::wire::{RestoreRunResponse, RunView, TimelineQuery, TrashRunResponse};
use koru_delegate_core::{
    AgentEventObservation, AgentTurnId, ApprovalChoiceId, ApprovalDecision, ApprovalId, Attachment,
    AttachmentId, AttachmentSource, BotAdmissionBarrier, BotDeliveryReceipt, BotExecutionFence,
    BotSubmission, CapturedBotPolicy, PendingInput, QuestionAnswer, QuestionRequestId,
    StreamSequence, WorkspaceId,
};
pub use koru_delegate_core::{
    AllocateMemoryEditRequest, BotAdmissionState, BotAdmissionView, BotBootCursor, BotBootReceipt,
    BotBootScan, BotBootState, BotHello, BotRunKey, BotSubmissionKey, BotUnsettledAdmission,
    BotUnsettledWork, MemoryEditAllocation, MemoryEditAuthorityV1,
};
pub use koru_delegate_core::{
    BotPolicyBarrier, BotPolicyBarrierKey, BotPolicyBarrierReceipt, BotPolicyBarrierState,
    ConfirmNativeResult, NativeDrainReason, NativePendingEffect, NativePendingEffects,
    NativeRecoveryCursor, NativeResultDisposition, NativeResultIntentView, NativeResultKey,
    NativeResultObservation, NativeResultOperation, NativeResultRefusal, NativeResultStage,
    PrepareNativeResult,
};
use serde::{Deserialize, Serialize};

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
    /// Derive the private reverse-bridge capability without exposing this
    /// bootstrap secret. A model/session scope does not supply this trusted
    /// bootstrap capability.
    pub fn routine_bridge_authority(&self) -> crate::routines::RoutineBridgeAuthority {
        crate::routines::RoutineBridgeAuthority::from_parent_bytes(self.0.as_bytes())
    }
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
    Hello {
        owner: koru_delegate_core::BotOwner,
        scan: BotBootScan,
    },
    SynchronizeBootBot {
        boot: koru_delegate_core::DelegateBootId,
        barrier: BotAdmissionBarrier,
        policy: BotPolicyBarrier,
    },
    PendingNativeEffects {
        owner: koru_delegate_core::BotOwner,
        bot: Option<koru_delegate_core::BotId>,
        after: Option<NativeRecoveryCursor>,
        limit: u16,
    },
    CheckNativeEffectDrain {
        key: NativeResultKey,
        reason: NativeDrainReason,
    },
    ResolveRunAuthority {
        run: koru_delegate_core::RunId,
        generation: koru_delegate_core::AttemptGeneration,
    },
    Submit {
        submission: BotSubmission,
    },
    Reconcile {
        key: BotSubmissionKey,
    },
    Barrier {
        barrier: BotAdmissionBarrier,
    },
    /// IPC 12: one Bot's unsettled admissions, read from the ledger now (at
    /// most one reserved or executing, then up to `limit` queued, 1..=64).
    UnsettledWork {
        owner: koru_delegate_core::BotOwner,
        bot: koru_delegate_core::BotId,
        limit: u16,
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
    /// Automatic AI interruption never borrows explicit owner Stop authority.
    Interrupt {
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
    SkipQuestions {
        fence: BotExecutionFence,
        request: QuestionRequestId,
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
    ReadMemoryEditAllocation {
        run: koru_delegate_core::RunId,
        operation_id: uuid::Uuid,
        caller_fingerprint: [u8; 32],
    },
    AllocateMemoryEdit {
        request: AllocateMemoryEditRequest,
    },
    ResolveMemoryEditBase {
        key: NativeResultKey,
        caller_fingerprint: [u8; 32],
        authority: MemoryEditAuthorityV1,
        expected_version: koru_delegate_core::DocumentVersionId,
        expected_revision: Vec<u8>,
        current_config_revision: u64,
        current_policy_revision: u64,
    },
    PrepareMemoryEdit {
        request: PrepareNativeResult,
        caller_fingerprint: [u8; 32],
        authority: MemoryEditAuthorityV1,
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
            Self::Hello { owner, .. }
            | Self::PendingNativeEffects { owner, .. }
            | Self::UnsettledWork { owner, .. } => Some(owner),
            Self::Submit { submission } => Some(&submission.provenance.owner),
            Self::Reconcile { key } => Some(&key.owner),
            Self::Barrier { barrier } | Self::SynchronizeBootBot { barrier, .. } => {
                Some(&barrier.owner)
            }
            Self::Read { key, .. }
            | Self::Trash { key, .. }
            | Self::Restore { key, .. }
            | Self::Stop { key, .. }
            | Self::Interrupt { key, .. }
            | Self::AcknowledgeDelivery { key, .. } => Some(&key.owner),
            Self::Observe { fence, .. }
            | Self::BeginTurn { fence, .. }
            | Self::Steer { fence, .. }
            | Self::IngestAttachment { fence, .. }
            | Self::RemoveAttachment { fence, .. }
            | Self::ListAttachments { fence }
            | Self::AnswerApproval { fence, .. }
            | Self::AnswerQuestions { fence, .. }
            | Self::SkipQuestions { fence, .. }
            | Self::TakeOver { fence }
            | Self::HandBack { fence }
            | Self::EffectFence { fence } => Some(&fence.owner),
            Self::SynchronizePolicy { barrier } => Some(&barrier.key.owner),
            Self::ReconcilePolicyBarrier { key } => Some(&key.owner),
            Self::ResolveRunAuthority { .. }
            | Self::ReadMemoryEditAllocation { .. }
            | Self::AllocateMemoryEdit { .. }
            | Self::ResolveMemoryEditBase { .. }
            | Self::PrepareMemoryEdit { .. }
            | Self::CheckNativeEffectDrain { .. }
            | Self::PrepareNativeResult { .. }
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
    Hello {
        hello: BotHello,
    },
    BootSynchronized {
        receipt: BotBootReceipt,
    },
    NativePendingEffects {
        page: NativePendingEffects,
    },
    NativeDrainEligibility {
        target: Option<NativePendingEffect>,
    },
    RunAuthority {
        authority: koru_delegate_core::RunToolAuthority,
    },
    Admission {
        admission: BotAdmissionView,
    },
    Missing,
    Barrier {
        barrier: BotAdmissionBarrier,
    },
    UnsettledWork {
        work: BotUnsettledWork,
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
    MemoryEditAllocation {
        allocation: MemoryEditAllocation,
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
    /// The same, waiting at most `within` for the answer; past it the call
    /// fails `delegate_unavailable` (retryable) and whatever the owner does
    /// with it is not reported, so only reads use it, and the AI owner's
    /// `stop_task`, whose Stop is idempotent and whose retry reads the
    /// owner's state afresh. Boot replay also uses it: timeout leaves replay
    /// owed, and a retry must recapture current canonical barriers. The default ignores the bound: a port that can
    /// block must bound it itself.
    fn execute_within(
        &self,
        command: BotCommand,
        within: std::time::Duration,
    ) -> Result<BotReply, crate::Failure> {
        let _ = within;
        self.execute(command)
    }
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
impl LocalBotDelegate {
    fn answer(response: crate::ipc::Response) -> Result<BotReply, crate::Failure> {
        match response {
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
impl BotDelegatePort for LocalBotDelegate {
    fn execute(&self, command: BotCommand) -> Result<BotReply, crate::Failure> {
        Self::answer(self.transport.call(&crate::ipc::Request::Bot {
            authority: self.authority.clone(),
            command,
        })?)
    }
    fn execute_within(
        &self,
        command: BotCommand,
        within: std::time::Duration,
    ) -> Result<BotReply, crate::Failure> {
        Self::answer(self.transport.call_within(
            &crate::ipc::Request::Bot {
                authority: self.authority.clone(),
                command,
            },
            within,
        )?)
    }
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
#[cfg(test)]
mod tests;
