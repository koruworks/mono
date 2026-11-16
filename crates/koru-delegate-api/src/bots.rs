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
    PrepareNativeResult, RoutinePolicyBarrier,
};
use serde::{Deserialize, Serialize};

mod decisions;
mod routine_control;
pub use decisions::{DecisionAnswerRequest, DecisionEditProposal, DecisionReceiptView};
pub use koru_delegate_core::{
    ROUTINE_CONTROL_WIRE_LIMIT, RoutineControlLifecycle, RoutineControlOutcome,
};
pub use routine_control::{
    ROUTINE_CONTROL_FRAME_LIMIT, RoutineControlAction, routine_control_frame_oversized,
};

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
    /// Derive the separate decision-bridge capability. Its derive-key context
    /// and header scheme differ from the Routine bridge's, so neither
    /// credential authorizes the other's routes.
    pub fn decision_bridge_authority(&self) -> crate::decision_bridge::DecisionBridgeAuthority {
        crate::decision_bridge::DecisionBridgeAuthority::from_parent_bytes(self.0.as_bytes())
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
    /// IPC 15: independent durable winner evidence, never caller assertion.
    ReadRoutineClaim {
        slot: koru_delegate_core::RoutineSlot,
    },
    AcknowledgeRoutineLink {
        acknowledgement: koru_delegate_core::RoutineLinkAcknowledgement,
    },
    /// IPC 15: retain the deny-only Schedule route before Store creation.
    RegisterRoutineRoute {
        identity: koru_delegate_core::RoutineRouteIdentity,
    },
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
    /// IPC 15: independent current admission metadata, including positive absence.
    ReadAdmissionBarrier {
        owner: koru_delegate_core::BotOwner,
        bot: koru_delegate_core::BotId,
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
    /// IPC 16 (O124): `request` is the caller's stable key for this one
    /// answer, forwarded unchanged to the session host so a retry of the same
    /// answer is the same request. Refused while a decision owns the Run.
    AnswerApproval {
        fence: BotExecutionFence,
        request: koru_delegate_core::IdempotencyKey,
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
    SynchronizeRoutinePolicy {
        barrier: RoutinePolicyBarrier,
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
    /// IPC 16: prepare a native approval decision for the exact live
    /// consumer's pending prompt from its retained offered choices. `boot` is
    /// the delegate boot this owner learned from `Hello`.
    PrepareNativeDecision {
        fence: BotExecutionFence,
        approval: ApprovalId,
        proof: koru_delegate_core::FreshBotPolicyProof,
        boot: koru_delegate_core::DelegateBootId,
    },
    /// IPC 16: prepare a one-use Koru edit decision for an edit refused only
    /// by selected-page write access. No intent, lease or Store effect.
    PrepareEditDecision {
        fence: BotExecutionFence,
        edit: DecisionEditProposal,
        proof: koru_delegate_core::FreshBotPolicyProof,
        boot: koru_delegate_core::DelegateBootId,
    },
    /// IPC 16, human route only: a person's exact answer. A native answer is
    /// forwarded with the ledger's journaled key; a retry replays it.
    AnswerDecision {
        answer: DecisionAnswerRequest,
        proof: koru_delegate_core::FreshBotPolicyProof,
        boot: koru_delegate_core::DelegateBootId,
    },
    /// IPC 16: the one-use consume by the exact acknowledged successor. The
    /// reply carries `Execute` only on the first fresh commit.
    ConsumeDecisionEdit {
        fence: BotExecutionFence,
        request: PrepareNativeResult,
        payload: koru_delegate_core::decisions::DecisionPayload,
        proof: koru_delegate_core::FreshBotPolicyProof,
        boot: koru_delegate_core::DelegateBootId,
    },
    /// IPC 16: the exact retained decision-wait observation and the current
    /// retained resume (O126). Historical; never permission to stop.
    ReadBotDecisionWait {
        owner: koru_delegate_core::BotOwner,
        bot: koru_delegate_core::BotId,
        run: koru_delegate_core::RunId,
        wait_id: koru_delegate_core::decisions::BotWaitId,
        decision_id: koru_delegate_core::decisions::DecisionId,
    },
    /// IPC 17: one synchronous, revision-checked commit of a Bot Routine's
    /// Schedule definition (enable, pause, or edit of timing and name). The
    /// scheduler coordinator serves it; the actor never does. `route` is
    /// compared with the route retained in the ledger and with the live
    /// header at its exact document; it grants nothing by being decoded.
    ControlRoutine {
        route: koru_delegate_core::RoutineRouteIdentity,
        expected_revision: u32,
        action: RoutineControlAction,
    },
    /// IPC 17: the owner Test of a Routine that is exactly Paused(Manual).
    /// Accepted at once; it completes later as one occurrence of the
    /// delegate's current minute, claimed through its own authority.
    TestRoutine {
        route: koru_delegate_core::RoutineRouteIdentity,
        expected_revision: u32,
        occurrence_ms: i64,
    },
}
impl BotCommand {
    /// IPC 17 owner controls only: whether the command is well formed and
    /// within its 4 KiB encoded bound. `None` for every other command. The
    /// sender checks it while encoding and the delegate before it acts.
    pub fn routine_control_valid(&self) -> Option<bool> {
        let (route, expected_revision, minute) = match self {
            Self::ControlRoutine {
                route,
                expected_revision,
                ..
            } => (route, *expected_revision, None),
            Self::TestRoutine {
                route,
                expected_revision,
                occurrence_ms,
            } => (route, *expected_revision, Some(*occurrence_ms)),
            _ => return None,
        };
        Some(
            route.valid()
                && koru_delegate_core::routine_control_revision_valid(expected_revision)
                && minute.is_none_or(koru_delegate_core::routine_test_minute_valid)
                && routine_control::within_wire_limit(self),
        )
    }
    /// The explicit owner asserted by the trusted AI owner. Native-result
    /// commands instead resolve their immutable Run binding inside the actor.
    pub fn owner(&self) -> Option<&koru_delegate_core::BotOwner> {
        match self {
            Self::ReadRoutineClaim { slot } => Some(&slot.owner),
            Self::AcknowledgeRoutineLink { acknowledgement } => {
                Some(&acknowledgement.claim.slot.owner)
            }
            Self::RegisterRoutineRoute { identity } => Some(&identity.owner),
            Self::Hello { owner, .. }
            | Self::PendingNativeEffects { owner, .. }
            | Self::ReadAdmissionBarrier { owner, .. }
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
            | Self::EffectFence { fence }
            | Self::PrepareNativeDecision { fence, .. }
            | Self::PrepareEditDecision { fence, .. }
            | Self::ConsumeDecisionEdit { fence, .. } => Some(&fence.owner),
            Self::AnswerDecision { answer, .. } => Some(&answer.owner),
            Self::ReadBotDecisionWait { owner, .. } => Some(owner),
            Self::ControlRoutine { route, .. } | Self::TestRoutine { route, .. } => {
                Some(&route.owner)
            }
            Self::SynchronizePolicy { barrier } => Some(&barrier.key.owner),
            Self::SynchronizeRoutinePolicy { barrier } => Some(&barrier.owner),
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
    RoutineClaim {
        observation: koru_delegate_core::RoutineClaimObservation,
    },
    RoutineLinkAcknowledged {
        record: koru_delegate_core::RoutineClaimRecord,
    },
    RoutineRoute {
        identity: koru_delegate_core::RoutineRouteIdentity,
    },
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
    AdmissionBarrier {
        owner: koru_delegate_core::BotOwner,
        bot: koru_delegate_core::BotId,
        admission: koru_delegate_core::BotBootAdmission,
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
    RoutinePolicyBarrier {
        barrier: RoutinePolicyBarrier,
        state: BotPolicyBarrierState,
    },
    NativeResult {
        intent: NativeResultIntentView,
    },
    MemoryEditAllocation {
        allocation: MemoryEditAllocation,
    },
    /// IPC 16: the exact retained decision after a prepare or answer.
    Decision {
        decision: DecisionReceiptView,
    },
    /// IPC 16: the observation and the current retained resume (O126). An
    /// absent observation answers `Missing` only after the exact owner/subject
    /// read succeeded.
    DecisionWait {
        observation: koru_delegate_core::decisions::BotDecisionWaitObservation,
        resume: Option<koru_delegate_core::decisions::DecisionResume>,
    },
    /// IPC 17: the Routine after one control. `applied` is answered only
    /// after the Store acknowledged the commit and its event was made Ready;
    /// `already_applied` is the exact replay of that commit. Every field is
    /// always present.
    RoutineControlled {
        route: koru_delegate_core::RoutineRouteIdentity,
        outcome: RoutineControlOutcome,
        name: koru_delegate_schedule::ScheduleName,
        definition: koru_delegate_core::RoutineDefinitionIdentity,
        effective_at_ms: i64,
        lifecycle: RoutineControlLifecycle,
    },
    /// IPC 17: a Test was accepted for this slot under this paused
    /// definition. It is not a claim: the claim is read separately.
    RoutineTestAccepted {
        slot: koru_delegate_core::RoutineSlot,
        definition: koru_delegate_core::RoutineDefinitionIdentity,
    },
}
impl BotReply {
    /// IPC 17 owner-control replies only: well formed and within the 4 KiB
    /// encoded bound. `None` for every other reply.
    pub fn routine_control_valid(&self) -> Option<bool> {
        let formed = match self {
            Self::RoutineControlled {
                route, definition, ..
            } => route.valid() && definition.valid(),
            Self::RoutineTestAccepted { slot, definition } => {
                slot.valid()
                    && definition.valid()
                    && koru_delegate_core::routine_test_minute_valid(slot.occurrence_ms)
            }
            _ => return None,
        };
        Some(formed && routine_control::within_wire_limit(self))
    }
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
    /// Read the exact subject once within the remaining caller budget. Paused
    /// metadata is not readiness. A legacy port may ignore its I/O bound; this
    /// helper rejects a late answer but cannot interrupt that implementation.
    fn read_admission_barrier(
        &self,
        owner: koru_delegate_core::BotOwner,
        bot: koru_delegate_core::BotId,
        within: std::time::Duration,
    ) -> Result<koru_delegate_core::BotBootAdmission, crate::Failure> {
        let started = std::time::Instant::now();
        let unknown = || crate::Failure::unavailable();
        if within.is_zero()
            || owner.device.as_uuid().is_nil()
            || koru_delegate_core::DeviceId::parse(&owner.device.to_storage()) != Some(owner.device)
            || bot.to_storage().parse::<koru_delegate_core::BotId>() != Ok(bot)
        {
            return Err(unknown());
        }
        let remaining = within
            .checked_sub(started.elapsed())
            .filter(|left| !left.is_zero())
            .ok_or_else(unknown)?;
        let answer =
            self.execute_within(BotCommand::ReadAdmissionBarrier { owner, bot }, remaining)?;
        if started.elapsed() >= within {
            return Err(unknown());
        }
        match answer {
            BotReply::AdmissionBarrier {
                owner: observed_owner,
                bot: observed_bot,
                admission,
            } if observed_owner == owner && observed_bot == bot => match &admission {
                koru_delegate_core::BotBootAdmission::Missing {} => Ok(admission),
                koru_delegate_core::BotBootAdmission::Present { barrier }
                    if barrier.owner == owner
                        && barrier.bot == bot
                        && barrier.epoch > 0
                        && barrier.epoch <= i64::MAX as u64
                        && barrier
                            .request
                            .to_storage()
                            .parse::<koru_delegate_core::BotRequestId>()
                            == Ok(barrier.request) =>
                {
                    Ok(admission)
                }
                _ => Err(unknown()),
            },
            _ => Err(unknown()),
        }
    }
    /// IPC 16: the exact decision-wait observation and current resume within
    /// the caller's remaining budget. `Ok(None)` only for the delegate's
    /// positive `Missing` after its exact owner/subject read; a transport,
    /// timeout, late, malformed or foreign-subject answer is unavailable,
    /// never Missing. A legacy port may ignore the bound; a late answer is
    /// still rejected here.
    #[allow(clippy::type_complexity)]
    fn read_bot_decision_wait(
        &self,
        owner: koru_delegate_core::BotOwner,
        bot: koru_delegate_core::BotId,
        run: koru_delegate_core::RunId,
        wait: koru_delegate_core::decisions::BotWaitId,
        decision: koru_delegate_core::decisions::DecisionId,
        within: std::time::Duration,
    ) -> Result<
        Option<(
            koru_delegate_core::decisions::BotDecisionWaitObservation,
            Option<koru_delegate_core::decisions::DecisionResume>,
        )>,
        crate::Failure,
    > {
        use koru_delegate_core::decisions::DecisionValidate;
        let started = std::time::Instant::now();
        let unknown = || crate::Failure::unavailable();
        if within.is_zero() {
            return Err(unknown());
        }
        let answer = self.execute_within(
            BotCommand::ReadBotDecisionWait {
                owner,
                bot,
                run,
                wait_id: wait,
                decision_id: decision,
            },
            within,
        )?;
        if started.elapsed() >= within {
            return Err(unknown());
        }
        match answer {
            BotReply::Missing => Ok(None),
            BotReply::DecisionWait {
                observation,
                resume,
            } if observation.validate().is_ok()
                && observation.key.owner == owner
                && observation.key.bot == bot
                && observation.key.run == run
                && observation.key.wait_id == wait
                && observation.key.decision_id == decision
                && resume.as_ref().is_none_or(|resume| {
                    resume.validate().is_ok()
                        && resume.decision == decision
                        && resume.origin == observation.cleanup_intent.origin()
                }) =>
            {
                Ok(Some((observation, resume)))
            }
            _ => Err(unknown()),
        }
    }
    /// The same, waiting at most `within` for the answer; past it the call
    /// fails `delegate_unavailable` (retryable) and whatever the owner does
    /// with it is not reported, so only reads use it, and the AI owner's
    /// `stop_task`, whose Stop is idempotent and whose retry reads the
    /// owner's state afresh. Routine route registration uses it too: unknown
    /// acknowledgement forbids Store creation and retries the same immutable
    /// route. Boot replay also uses it: timeout leaves replay
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
    /// An owner control over its bound, or malformed, is refused before a
    /// frame is written. Every other command is unaffected.
    fn encodable(command: &BotCommand) -> Result<(), crate::Failure> {
        if command.routine_control_valid() == Some(false) {
            return Err(crate::Failure::new(
                "invalid_request",
                "The Routine control request is malformed or too large.",
                false,
            ));
        }
        Ok(())
    }
    fn answer(response: crate::ipc::Response) -> Result<BotReply, crate::Failure> {
        match response {
            crate::ipc::Response::Bot(reply) if reply.routine_control_valid() != Some(false) => {
                Ok(reply)
            }
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
        Self::encodable(&command)?;
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
        Self::encodable(&command)?;
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
