// SPDX-License-Identifier: MPL-2.0

//! Public Koru AI wire declarations.
//!
//! Request objects reject unknown fields. Response and event objects retain
//! serde's additive-field behavior so an older client can read a newer server.

use std::collections::BTreeMap;

pub use koru_ai_core::bots::{BotCommand, BotResponse};
pub mod bot_routines;
pub mod bot_threads;
use koru_ai_core::{
    AiError, AiEvent, AiProviderId, AiSearchPage, AiSurface, AiUsage, AttemptStatus, Chat, ChatId,
    ChatToolPolicy, ContextRef, DocumentDestination, DocumentExport, DocumentExportId,
    FeedbackEvent, FeedbackEventId, FeedbackValue, InferenceAttempt, InferenceAttemptId, Message,
    MessageBlock, MessageId, ModelDescriptor, ModelSelection, PinnedDocument, ProfileId,
    ReasoningChoice, SourceLink, Timestamp, ToolActivity, ToolMode, UsageReport, WebSearchPhase,
    WorkspaceId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use koru_ai_core::NeededYouReason;
pub use koru_ai_core::{
    AttachmentKind, AttachmentSource, ChatAttachment, ChatAttachmentId, ListChatsToolRequest,
    ListChatsToolResponse, ReadChatToolRequest, ReadChatToolResponse, SearchChatsToolRequest,
    SearchChatsToolResponse, WorkspaceFileOrigin,
};

/// Desktop MCP delegates only these bounded calls to an exact live attempt.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MiniAppToolRequest {
    pub capability: koru_ai_core::mini_apps::Capability,
    pub name: String,
    pub call_id: String,
    pub arguments: serde_json::Value,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MiniAppToolResponse {
    pub content: serde_json::Value,
    pub is_error: bool,
}

/// The stable failure codes of the attachment routes and of a send that
/// names attachments. Product UI renders localized copy by code.
pub mod attachment_codes {
    /// One file over `MAX_ATTACHMENT_BYTES`.
    pub const TOO_LARGE: &str = "attachment_too_large";
    /// More files, or more bytes, than one message may carry.
    pub const LIMIT: &str = "attachment_limit";
    /// A type off the allowlist, or content that is not what the name says.
    pub const TYPE_NOT_ALLOWED: &str = "attachment_type_not_allowed";
    /// No such attachment in this chat (never stored, removed or swept).
    pub const NOT_FOUND: &str = "attachment_not_found";
    /// The row is there but its stored file is gone from this computer.
    pub const MISSING: &str = "attachment_missing";
    /// It was sent with a message, which keeps it.
    pub const SENT: &str = "attachment_sent";
    /// The id was already used for different content.
    pub const CONFLICT: &str = "attachment_conflict";
    /// The chat's provider takes no attachments (only local agents do).
    pub const UNSUPPORTED: &str = "attachments_unsupported";
    /// The bytes could not be stored: the disk is full or not writable.
    pub const STORAGE: &str = "attachment_storage";
    /// The attachment has no preview (it is not a picture).
    pub const NO_PREVIEW: &str = "attachment_no_preview";
    /// An `@` file's workspace is not, or is no longer, one the chat may use.
    pub const WORKSPACE_NOT_GRANTED: &str = "workspace_not_granted";
    /// An `@` file is trashed, not a file, or its blob is offline.
    pub const WORKSPACE_FILE_UNAVAILABLE: &str = "workspace_file_unavailable";
}

/// Refusals of a Bot or a Bot conversation that lifecycle holds still
/// (P1.7). Each is returned before any mutation; none is an ordinary-chat
/// fallback.
pub mod bot_lifecycle_codes {
    /// The conversation is in Trash on its own (its Bot is not): restore the
    /// conversation first.
    pub const TRASHED: &str = "bot_conversation_trashed";
    /// The Bot is in Trash, and with it every conversation of it: restore the
    /// Bot first (additive).
    pub const BOT_TRASHED: &str = "bot_trashed";
    /// An Archive, Trash, Restore or Purge of this conversation or its Bot is
    /// still in progress or parked incomplete.
    pub const PENDING: &str = "bot_lifecycle_pending";
    /// The Bot is archived: its history reads, nothing changes it until it
    /// is unarchived.
    pub const ARCHIVED: &str = "bot_archived";
}

/// Refusals of an owner control of one Bot Routine (P4.3): enable, pause,
/// edit, Test and the access edit. Each is answered before anything is
/// written on the AI side. A client shows its own localized sentence for a
/// code and never the message. The existing Bot codes (`not_found`,
/// `wrong_owner`, `workspace_unavailable`, `revision_conflict`,
/// `request_conflict`, `bot_operation_pending`) and the lifecycle codes
/// (`bot_trashed`, `bot_archived`, `bot_lifecycle_pending`) are answered for
/// these commands too.
pub mod bot_routine_codes {
    /// The Routine's creation is not finished (`bound`).
    pub const NOT_BOUND: &str = "routine_not_bound";
    /// The Routine's Schedule was deleted or is in Trash.
    pub const NOT_LIVE: &str = "routine_not_live";
    /// Another computer owns the Routine, or a handoff is pending: change
    /// it there.
    pub const OWNER_ELSEWHERE: &str = "routine_owner_elsewhere";
    /// Another control of this Routine is not resolved yet. Retryable.
    pub const CONTROL_PENDING: &str = "routine_control_pending";
    /// The Schedule's owner did not answer in time: the control may or may
    /// not have been applied. Read the Routine again. Retryable; a retry
    /// with the same revision is safe.
    pub const CONTROL_UNKNOWN: &str = "routine_control_unknown";
    /// The Routine is already in the requested state.
    pub const STATE_CONFLICT: &str = "routine_state_conflict";
    /// The Schedule's owner is unreachable or cannot serve Routines now.
    /// Retryable.
    pub const UNAVAILABLE: &str = "routine_unavailable";
    /// Enable or Test: the Routine cannot run now. `details.reasons` lists
    /// why, in order, from [`super::bot_routine_not_ready_reasons`].
    pub const NOT_READY: &str = "routine_not_ready";
    /// Test and the access edit need the Routine paused; it is on.
    pub const MUST_BE_PAUSED: &str = "routine_must_be_paused";
    /// Test: under ten seconds are left of the current minute. Retryable at
    /// the start of the next minute.
    pub const TEST_MINUTE_ENDING: &str = "routine_test_minute_ending";
    /// Test: the Routine changed within this minute. Retryable at the start
    /// of the next minute.
    pub const TEST_TOO_SOON: &str = "routine_test_too_soon";
    /// Test: this minute already ran or was skipped.
    pub const TEST_MINUTE_TAKEN: &str = "routine_test_minute_taken";
    /// Test: another Test is starting on this computer. Retryable.
    pub const TEST_BUSY: &str = "routine_test_busy";
    /// Access edit: a Run of this Routine has not finished. Retryable.
    pub const RUN_IN_PROGRESS: &str = "routine_run_in_progress";
    /// Access edit: an earlier edit of this Routine is still being applied.
    /// Retryable.
    pub const ACCESS_PENDING: &str = "routine_access_pending";
}

/// The reasons `routine_not_ready` lists in `details.reasons`, in this order.
pub mod bot_routine_not_ready_reasons {
    /// The Bot is paused: resume the Bot first.
    pub const BOT_PAUSED: &str = "bot_paused";
    pub const BOT_ARCHIVED: &str = "bot_archived";
    pub const CREATION_NOT_BOUND: &str = "creation_not_bound";
    /// The Routine's access cannot be read: choose its access again.
    pub const ACCESS_UNAVAILABLE: &str = "access_unavailable";
    /// An access change is still being applied.
    pub const ACCESS_PENDING: &str = "access_pending";
}

/// Refusals of a Bot creation's request with a stable code of their own,
/// beside the reasons a recorded creation carries (P1.8). Each is answered
/// before anything is recorded, never retryable.
pub mod bot_creation_codes {
    /// The selected folder is Koru's own data: the AI data root or the
    /// Store's data root, or any folder inside either (every managed Bot
    /// folder is inside the AI root), compared by their canonical paths. A
    /// folder that merely contains one of them, such as the person's home
    /// folder, is not refused. A folder that does not exist or is not a
    /// folder stays `invalid_request`.
    pub const FOLDER_RESERVED: &str = "folder_reserved";
}

/// Refusals of the team pause (M8, P8.1). A client shows its own localized
/// sentence for a code and never the message. Additive: the codes of every
/// other Bot command are unchanged, and the AI wire version is not moved by
/// them.
pub mod bot_team_codes {
    /// The team is paused. New work is refused: a message to an idle Bot, a
    /// retry, a start or continue, turning a Routine on, a Routine test, and
    /// one Bot's ask to another. Reading, Stop, a message to a Bot that is
    /// working, answering a question, and every edit are not refused.
    pub const TEAM_PAUSED: &str = "team_paused";
    /// `expected_team_revision` is not the team's current revision. Read
    /// the team pause again.
    pub const TEAM_REVISION_CONFLICT: &str = "team_revision_conflict";
    /// Every code of this module.
    pub const ALL: [&str; 2] = [TEAM_PAUSED, TEAM_REVISION_CONFLICT];
}

/// **Why a local agent cannot run, as far as its readiness check knows**
/// (Bots P1.8), carried as `details.reason` beside a failure's own code. It
/// is additive: the code stays the contract, a reader that does not know the
/// field or a value reads the code alone, and a failure's `retryable` is
/// unchanged by it.
///
/// It appears on an agent's readiness (`ProviderValidationResponse.failure`,
/// beside its `agent_*` code, where a reason can be finer than the code: an
/// `agent_check_failed` that timed out, an `agent_unsupported_version` that
/// is simply too old) and on a Bot `create`, `update` or
/// `create_bot_with_role` refused because its agent cannot run (beside
/// `agent_models_failed` or `runtime_unavailable`). It is absent when
/// nothing more is known than the code says (an unanswered readiness
/// question, `agent_models_listing`).
pub mod agent_readiness_reasons {
    /// No session host answers in this AI service.
    pub const SERVICE_UNAVAILABLE: &str = "agent_service_unavailable";
    /// The session host is still checking the agent.
    pub const CHECKING: &str = "agent_checking";
    /// The agent's program is not on this computer, or the session host knows
    /// no such agent; the two are not told apart.
    pub const NOT_INSTALLED: &str = "agent_not_installed";
    /// The agent's own program is here but the program its chats run (an
    /// adapter such as `claude-agent-acp`) is not.
    pub const ADAPTER_MISSING: &str = "agent_adapter_missing";
    /// A part the agent's adapter needs to run is missing (Claude's native
    /// binary for its SDK wrapper).
    pub const RUNTIME_MISSING: &str = "agent_runtime_missing";
    /// The agent needs signing in.
    pub const SIGNED_OUT: &str = "agent_signed_out";
    /// The agent printed a version older than the release Koru accepts;
    /// updating fixes it.
    pub const VERSION_TOO_OLD: &str = "agent_version_too_old";
    /// The agent printed a version that is not on the reviewed list Koru
    /// accepts (usually newer than the newest reviewed release).
    pub const VERSION_NOT_REVIEWED: &str = "agent_version_not_reviewed";
    /// Refused for its version or its handshake with no version to compare
    /// (none could be read, or the refusal was the protocol).
    pub const UNSUPPORTED_VERSION: &str = "agent_unsupported_version";
    /// The agent's program did not start on this computer: it could not be
    /// spawned, or it exited before printing its version.
    pub const FAILED_TO_START: &str = "agent_failed_to_start";
    /// The check ran out of time.
    pub const CHECK_TIMED_OUT: &str = "agent_check_timed_out";
    /// The check failed in another way (the handshake was refused, the
    /// program exited mid-handshake, an I/O or framing failure).
    pub const CHECK_FAILED: &str = "agent_check_failed";
    /// The agent is ready, but listing its models failed.
    pub const LISTING_FAILED: &str = "agent_listing_failed";
    /// The agent is ready and listed its models; the chosen one is not
    /// among them.
    pub const MODEL_NOT_OFFERED: &str = "agent_model_not_offered";
}

pub const AI_WIRE_VERSION: u32 = 1;
pub const AI_STREAM_VERSION: u32 = 1;
pub const MODEL_PULL_STREAM_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateChatRequest {
    pub chat_id: ChatId,
    pub title: String,
    pub model: ModelSelection,
    /// **The profile the chat belongs to** (AI schema 13): the profile of
    /// the window it is started in. An older client sends none, and the
    /// service places the chat as it places one written before profiles —
    /// with nothing read yet, in the first profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<ProfileId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CreateChatResponse {
    pub chat: Chat,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameChatRequest {
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RenameChatResponse {
    pub chat: Chat,
}

/// Pin a chat, or unpin it (AI schema 12). Pinning a chat already pinned
/// keeps its pin time; like a rename it is not activity, so the chat keeps
/// its place among the others.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PinChatRequest {
    pub pinned: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PinChatResponse {
    pub chat: Chat,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateChatModelRequest {
    pub model: ModelSelection,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateChatModelResponse {
    pub chat: Chat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_control: Option<koru_ai_core::ReasoningControl>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateChatPrivacyRequest {
    pub private: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateChatPrivacyResponse {
    pub chat: Chat,
}

/// The folder a chat's local agent works in. `None` returns the chat to
/// the home-folder default. Accepted only on an `agent-*` chat with no
/// message yet: the agent starts in the folder, so a later change would
/// name a folder the conversation is not in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateChatFolderRequest {
    pub folder: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateChatFolderResponse {
    pub chat: Chat,
}

/// A local agent chat's settings for its next message (koru-ai D7). A field
/// left out keeps what the chat has. Accepted on an `agent-*` chat at any
/// time: a reply already running keeps the mode it was sent in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateChatAgentSettingsRequest {
    /// The approval mode, the agent's own id: one it lists in
    /// [`AgentModelsResponse::modes`]. An agent refuses one it does not
    /// offer when the message is sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Fast mode on or off for the chat's model (koru-ai D11), where the
    /// model offers it ([`AgentModelView::fast`]). Applied from the next
    /// message, like the mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateChatAgentSettingsResponse {
    pub chat: Chat,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ListChatsResponse {
    pub chats: Vec<Chat>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReportedContextUsage {
    pub used: u64,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MessageUsage {
    pub message_id: MessageId,
    pub usage: AiUsage,
    /// The latest active-context report from the attempt for this reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ReportedContextUsage>,
    /// Why the engine ended this reply, as a closed stable code, never
    /// provider text: one of the `bot_input_*` codes the live `failed` event
    /// carried (O36), or `bot_decision_declined` / `bot_decision_needed_you`
    /// for a Bot turn that ended as "Needed you" (O128, the live `needed_you`
    /// event). Absent on every other reply. Such a reply may report no usage,
    /// so an entry may carry only this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_code: Option<String>,
    /// For a Bot conversation's owner, the Run that wrote this reply, as
    /// `read_run` accepts it with this `message_id` and the conversation's
    /// `chat_id` (a copied reply in a branch names the Run it was copied
    /// from). Absent on every reply no Run the execution owner holds wrote,
    /// and in every ordinary chat. A reply that reports no usage may carry
    /// only this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<koru_ai_core::bots::RunId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MessageSources {
    pub message_id: MessageId,
    pub sources: Vec<SourceLink>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatTranscript {
    pub chat: Chat,
    /// The retained pending or streaming attempt for this chat's pending
    /// assistant message, including before any text or tool activity exists.
    /// Its model is the accepted turn's; `chat.current_model` is the next
    /// turn's choice. Absent on terminal history and older server responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_attempt: Option<InferenceAttempt>,
    pub tool_policy: ChatToolPolicy,
    #[serde(default)]
    pub tool_activities: Vec<ToolActivity>,
    /// A local agent's status lines beside its replies, in the order they
    /// came (koru-ai D69).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<koru_ai_core::AgentNotice>,
    pub messages: Vec<Message>,
    pub blocks: Vec<MessageBlock>,
    pub pinned_documents: Vec<PinnedDocument>,
    #[serde(default)]
    pub feedback: Vec<FeedbackEvent>,
    #[serde(default)]
    pub usage: Vec<MessageUsage>,
    #[serde(default)]
    pub sources: Vec<MessageSources>,
    /// What the chat's current model offers as an effort control, so the
    /// chip can read and set it; `None` where the model advertises none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_control: Option<koru_ai_core::ReasoningControl>,
    /// The live session's current command listing; an empty list is authoritative.
    /// `None` lets the composer use its short-lived bootstrap catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_commands: Option<Vec<AgentCommandView>>,
    /// Every attachment of the chat: those sent with a message, in message
    /// and ordinal order, then the staged ones not yet sent.
    #[serde(default)]
    pub attachments: Vec<ChatAttachment>,
}

/// One attachment stored, or found already stored under the same id with
/// the same content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatAttachmentResponse {
    pub attachment: ChatAttachment,
}

/// Copy the bytes of a sent attachment into a new staged attachment of the
/// same chat. The caller mints an id for this recall attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestageAttachmentRequest {
    pub new_id: koru_ai_core::ChatAttachmentId,
}

/// Stage a local path for an agent without reading or copying its contents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReferenceAttachmentRequest {
    pub id: koru_ai_core::ChatAttachmentId,
    pub kind: AttachmentKind,
    pub path: String,
}

/// Copy a Files document into a chat attachment. The id is minted before
/// import so a retry can finish the same operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FromWorkspaceAttachmentRequest {
    pub id: koru_ai_core::ChatAttachmentId,
    pub workspace: koru_ai_core::WorkspaceId,
    pub document: koru_ai_core::DocumentId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemoveAttachmentResponse {
    /// Whether a staged attachment existed and was removed.
    pub removed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateChatToolPolicyRequest {
    pub mode: ToolMode,
    pub workspace_ids: Vec<WorkspaceId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddChatWorkspaceGrantsRequest {
    pub workspace_ids: Vec<WorkspaceId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatToolPolicyResponse {
    pub policy: ChatToolPolicy,
}

/// The optional body of an approve or deny. A local agent offers its own
/// choices for a permission request; the chosen one's id comes back here.
/// A workspace tool offers none and takes no body. An agent's row with no
/// choice is refused, so two same-intent choices can never collapse.
///
/// **An agent's questions are answered by approving their row with
/// `answers`** (koru-ai D108), one per question by its id: an offered
/// option by label, or the person's own words where the question takes
/// them. Denying the row skips the set. A question row approved without
/// answers is refused.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolDecisionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<Vec<koru_ai_core::AgentAnswer>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolDecisionResponse {
    pub activity: ToolActivity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeedbackRequest {
    pub event_id: FeedbackEventId,
    pub value: FeedbackValue,
    pub surface: AiSurface,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FeedbackResponse {
    pub event: FeedbackEvent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UsageResponse {
    pub report: UsageReport,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeleteChatResponse {
    pub deleted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClearHistoryResponse {
    pub chats_deleted: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClearUsageResponse {
    pub attempts_deleted: u64,
    pub feedback_events_deleted: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BranchChatRequest {
    pub branch_chat_id: ChatId,
    pub through_message_id: MessageId,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BranchChatResponse {
    pub transcript: ChatTranscript,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryTurnRequest {
    /// Explicit submission destination for a Bot conversation. Ordinary chats
    /// omit it; a Bot never infers this grant from its home workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_workspace: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mini_app_host: Option<koru_ai_core::mini_apps::HostContext>,
    /// Exact captured declaration paired with the live target; never persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mini_app_refinement: Option<koru_ai_core::mini_apps::RefinementContext>,
    #[schemars(with = "String")]
    pub operation_id: Uuid,
    pub message_id: MessageId,
    pub surface: AiSurface,
    #[serde(default)]
    pub explicit_refs: Vec<ContextRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_override: Option<ReasoningChoice>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub web_search: bool,
    /// When true, existing pinned documents are ignored and only the explicit
    /// refs are used as context for this turn. Used by the contextual AI pane,
    /// where the current document should replace prior context.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace_pinned_documents: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportChatRequest {
    pub export_id: DocumentExportId,
    #[schemars(with = "String")]
    pub idempotency_key: Uuid,
    pub through_message_id: MessageId,
    pub destination: DocumentDestination,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExportChatResponse {
    pub export: DocumentExport,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AiSearchRequest {
    pub text: String,
    #[serde(default = "default_search_limit")]
    pub limit: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default)]
    pub request_seq: u64,
    /// Only this profile's chats (AI schema 13). Absent searches every
    /// profile's; each hit names its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProfileId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AiSearchResponse {
    pub page: AiSearchPage,
    pub request_seq: u64,
}

const fn default_search_limit() -> u16 {
    50
}

/// The most context references one turn may carry — pinned documents and
/// `explicit_refs` together. The worker refuses a turn over it; the chat's
/// composer stops adding `@` documents at it rather than sending a turn the
/// worker will refuse.
pub const MAX_CONTEXT_REFS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendTurnRequest {
    /// Explicit submission destination for a Bot conversation. Ordinary chats
    /// omit it; a Bot never infers this grant from its home workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_workspace: Option<WorkspaceId>,
    /// Native host provenance for this live request only. Never persisted or
    /// accepted from a model tool argument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mini_app_host: Option<koru_ai_core::mini_apps::HostContext>,
    /// Exact captured declaration paired with the live target; never persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mini_app_refinement: Option<koru_ai_core::mini_apps::RefinementContext>,
    #[schemars(with = "String")]
    pub operation_id: Uuid,
    pub chat_id: ChatId,
    pub surface: AiSurface,
    pub markdown: String,
    #[serde(default)]
    pub explicit_refs: Vec<ContextRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_override: Option<ReasoningChoice>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub web_search: bool,
    /// Debug-only, per-request replacement for Koru's bundled system prompt.
    /// Empty or omitted keeps the versioned bundled prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_override: Option<String>,
    /// When true, existing pinned documents are ignored and only the explicit
    /// refs are used as context for this turn. Used by the contextual AI pane,
    /// where the current document should replace prior context.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace_pinned_documents: bool,
    /// Attachments already stored for this chat and not yet sent, in the
    /// order they go with the message. Only an agent chat takes any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<ChatAttachmentId>,
    /// **A local agent's questions answered by this message** (koru-ai
    /// D108): a set the agent asked in its reply and moved on from, whose
    /// answers go to it as the person's next message. The message is the
    /// answers alone — `markdown`, `explicit_refs` and `attachment_ids`
    /// empty — and is recorded as no message of the person's: the answers
    /// are read on the set's card, and the agent's reply follows the one
    /// that asked. `user_message_id` in the answer is the message the
    /// asking reply answered. A set that can no longer be answered is
    /// refused (`questions_closed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<koru_ai_core::NextMessageAnswers>,
    /// **The skills the person picked** (koru-ai D121), by the name a
    /// local agent lists each under (`POST /v1/ai/providers/{provider}/
    /// commands`), in the order they appear; `markdown` says `$name` where
    /// each was. The agent is told each by name beside the text; one it no
    /// longer lists is left out, and the text goes as typed. Only an agent
    /// chat takes any. Not stored: the message keeps its `$name` text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SendTurnAccepted {
    pub attempt_id: InferenceAttemptId,
    pub user_message_id: MessageId,
    pub assistant_message_id: MessageId,
    pub replayed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttemptResponse {
    pub attempt: InferenceAttempt,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CancelAttemptResponse {
    pub attempt_id: InferenceAttemptId,
    pub status: AttemptStatus,
}

/// **Steer a running agent reply**: text the person sends into the turn
/// that is running, instead of waiting for it to end. Only a local agent
/// that takes text mid-turn accepts it (`AgentProviderView::steers`); the
/// message is recorded as the person's, after the running reply.
/// `operation_id` makes a retry an idempotent read of the first answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SteerAttemptRequest {
    pub operation_id: Uuid,
    pub markdown: String,
}

/// The steer reached the running turn: the person's message as recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SteerAttemptResponse {
    pub attempt_id: InferenceAttemptId,
    pub user_message_id: MessageId,
    /// The same `operation_id` was answered before; nothing was sent again.
    pub replayed: bool,
}

/// One normalized event on the public SSE stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StreamEvent {
    pub version: u32,
    pub sequence: u64,
    pub event: PublicAiEvent,
}

/// An event safe to expose across Koru's public AI boundary.
///
/// Provider tool calls are deliberately absent: their arguments and reasoning
/// continuity data remain inside the worker. Clients observe only the minimal
/// [`ToolActivity`] projection: stable target ids and a bounded public failure
/// summary, never raw tool arguments or general tool results. The registered
/// reference-only mini-app result has a separate bounded, transient variant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PublicAiEvent {
    /// A live-only arrangement proposal. Native revision ownership is external
    /// to the upstream A2UI envelope and must be rechecked before application.
    MiniAppUpdated {
        attempt_id: InferenceAttemptId,
        chat_id: ChatId,
        profile_id: ProfileId,
        invocation_id: koru_ai_core::ToolInvocationId,
        call_id: String,
        host: koru_ai_core::mini_apps::HostContext,
        envelope: koru_ai_core::mini_apps::UpdateEnvelope,
    },
    /// Only the registered, validated mini-app result crosses this boundary.
    /// It is transient, never reconstructed from transcript or tool activity.
    MiniAppCreated {
        attempt_id: InferenceAttemptId,
        chat_id: ChatId,
        profile_id: ProfileId,
        invocation_id: koru_ai_core::ToolInvocationId,
        call_id: String,
        host: koru_ai_core::mini_apps::HostContext,
        envelope: koru_ai_core::mini_apps::Envelope,
    },
    Started {
        attempt_id: InferenceAttemptId,
        provider_response_id: Option<String>,
        #[serde(default)]
        request_payload_bytes: Option<u64>,
        resolved_model: Option<String>,
        effective_reasoning: Option<ReasoningChoice>,
        at: Timestamp,
    },
    TextDelta {
        attempt_id: InferenceAttemptId,
        text: String,
        at: Timestamp,
    },
    ReasoningDelta {
        attempt_id: InferenceAttemptId,
        text: String,
        at: Timestamp,
    },
    Usage {
        attempt_id: InferenceAttemptId,
        usage: AiUsage,
    },
    ContextUsage {
        attempt_id: InferenceAttemptId,
        used: u64,
        size: u64,
    },
    ToolActivity {
        attempt_id: InferenceAttemptId,
        activity: ToolActivity,
    },
    /// A local agent's status line beside its reply, as the chat keeps it
    /// (koru-ai D69): not part of the reply's text.
    AgentNotice {
        attempt_id: InferenceAttemptId,
        notice: koru_ai_core::AgentNotice,
    },
    /// A local agent changed the current session's command listing.
    AgentCommands {
        attempt_id: InferenceAttemptId,
        chat: ChatId,
        commands: Vec<AgentCommandView>,
    },
    /// A one-line fact about the attempt the surface shows and nothing
    /// stores: a local agent could not be reattached and a replacement was
    /// started with the conversation so far.
    Notice {
        attempt_id: InferenceAttemptId,
        notice: koru_ai_core::AttemptNotice,
        at: Timestamp,
    },
    WebSearch {
        attempt_id: InferenceAttemptId,
        call_id: String,
        phase: WebSearchPhase,
        at: Timestamp,
    },
    Citation {
        attempt_id: InferenceAttemptId,
        source: SourceLink,
        at: Timestamp,
    },
    Completed {
        attempt_id: InferenceAttemptId,
        stop_reason: Option<String>,
        at: Timestamp,
    },
    Failed {
        attempt_id: InferenceAttemptId,
        error: AiError,
        at: Timestamp,
    },
    Cancelled {
        attempt_id: InferenceAttemptId,
        at: Timestamp,
    },
    /// A Bot turn ended because its decision wait closed without the work
    /// continuing ("Needed you", O128). Terminal and **not a failure**.
    /// `reason` is only what the closure positively knows: absent when it
    /// could not tell the reasons apart, and read as absent when a newer
    /// server sends a reason this client does not know.
    NeededYou {
        attempt_id: InferenceAttemptId,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "known_needed_you_reason"
        )]
        #[schemars(with = "Option<NeededYouReason>")]
        reason: Option<NeededYouReason>,
        at: Timestamp,
    },
}

/// A reason this client does not know reads as absent, never as an error:
/// the event stays the closing event it is.
fn known_needed_you_reason<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<NeededYouReason>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

impl PublicAiEvent {
    pub const fn attempt_id(&self) -> InferenceAttemptId {
        match self {
            Self::MiniAppCreated { attempt_id, .. } | Self::MiniAppUpdated { attempt_id, .. } => {
                *attempt_id
            }
            Self::Started { attempt_id, .. }
            | Self::Notice { attempt_id, .. }
            | Self::TextDelta { attempt_id, .. }
            | Self::ReasoningDelta { attempt_id, .. }
            | Self::Usage { attempt_id, .. }
            | Self::ContextUsage { attempt_id, .. }
            | Self::ToolActivity { attempt_id, .. }
            | Self::AgentNotice { attempt_id, .. }
            | Self::AgentCommands { attempt_id, .. }
            | Self::WebSearch { attempt_id, .. }
            | Self::Citation { attempt_id, .. }
            | Self::Completed { attempt_id, .. }
            | Self::Failed { attempt_id, .. }
            | Self::Cancelled { attempt_id, .. }
            | Self::NeededYou { attempt_id, .. } => *attempt_id,
        }
    }

    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. }
                | Self::Failed { .. }
                | Self::Cancelled { .. }
                | Self::NeededYou { .. }
        )
    }
}

impl TryFrom<AiEvent> for PublicAiEvent {
    type Error = AiEvent;

    fn try_from(event: AiEvent) -> Result<Self, Self::Error> {
        Ok(match event {
            AiEvent::Started {
                attempt_id,
                provider_response_id,
                request_payload_bytes,
                resolved_model,
                effective_reasoning,
                at,
            } => Self::Started {
                attempt_id,
                provider_response_id,
                request_payload_bytes,
                resolved_model,
                effective_reasoning,
                at,
            },
            AiEvent::TextDelta {
                attempt_id,
                text,
                at,
            } => Self::TextDelta {
                attempt_id,
                text,
                at,
            },
            AiEvent::ReasoningDelta {
                attempt_id,
                text,
                at,
            } => Self::ReasoningDelta {
                attempt_id,
                text,
                at,
            },
            AiEvent::Usage { attempt_id, usage } => Self::Usage { attempt_id, usage },
            AiEvent::ContextUsage {
                attempt_id,
                used,
                size,
            } => Self::ContextUsage {
                attempt_id,
                used,
                size,
            },
            AiEvent::ToolActivity {
                attempt_id,
                activity,
            } => Self::ToolActivity {
                attempt_id,
                activity,
            },
            AiEvent::AgentNotice { attempt_id, notice } => Self::AgentNotice { attempt_id, notice },
            AiEvent::WebSearch {
                attempt_id,
                call_id,
                phase,
                at,
            } => Self::WebSearch {
                attempt_id,
                call_id,
                phase,
                at,
            },
            AiEvent::Citation {
                attempt_id,
                source,
                at,
            } => Self::Citation {
                attempt_id,
                source,
                at,
            },
            AiEvent::Completed {
                attempt_id,
                stop_reason,
                at,
            } => Self::Completed {
                attempt_id,
                stop_reason,
                at,
            },
            AiEvent::Failed {
                attempt_id,
                mut error,
                at,
            } => {
                // Only the engine's own stable codes reach a client (O36).
                error.stop_code = error
                    .stop_code
                    .filter(|code| koru_ai_core::is_engine_stop_code(code));
                Self::Failed {
                    attempt_id,
                    error,
                    at,
                }
            }
            AiEvent::Cancelled { attempt_id, at } => Self::Cancelled { attempt_id, at },
            AiEvent::Notice {
                attempt_id,
                notice,
                at,
            } => Self::Notice {
                attempt_id,
                notice,
                at,
            },
            internal @ (AiEvent::ToolCalls { .. } | AiEvent::AgentActivity { .. }) => {
                return Err(internal);
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "AiFailure")]
pub struct Failure {
    pub code: String,
    /// Safe developer-facing detail. Product UI renders localized copy by code.
    pub message: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderSummary {
    pub id: AiProviderId,
    pub available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderSettingsResponse {
    pub selected_provider: Option<AiProviderId>,
    pub openai: OpenAiSettingsView,
    pub ollama: OllamaSettingsView,
    /// The local coding agents this AI service can drive, as chat providers
    /// (`agent-<kind>`) with the name a person knows each by. Empty where
    /// no session host exists. Readiness is asked per agent through
    /// validation, like any provider's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AgentProviderView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentProviderView {
    pub id: AiProviderId,
    pub label: String,
    /// The agent takes text into a running turn, so a client offers a native
    /// *Steer* (`POST /v1/ai/attempts/{attempt}/steer`); without it a client
    /// steers by stopping the reply and sending. A fixed fact of the agent's
    /// kind; the agent itself still decides at the moment, and a steer it
    /// cannot take is answered `steer_unavailable`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub steers: bool,
}

/// The models one local agent lists, in the agent's own ids. Empty when the
/// agent lists none: the picker then shows no model level under it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentModelsResponse {
    pub provider: AiProviderId,
    pub models: Vec<AgentModelView>,
    /// The agent's approval modes (koru-ai D7), in its order, with its own
    /// ids. Empty when it offers none: a client then shows no mode control.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<AgentModeView>,
    /// The mode the agent reports it starts in — the one a chat that chose
    /// none runs in. Absent when the agent does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_mode: Option<String>,
    /// A begun chat's level can change, applied from its next message
    /// (koru-ai D70): the agent's session host takes a level with each
    /// message. `false` — the level stays the one the chat started at —
    /// against an older host, and from an older service.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub levels_per_turn: bool,
    /// A begun chat's model can move to another of the agent's models,
    /// applied from its next message: the agent's session host takes a
    /// model with each message (session protocol v22). `false` — the model
    /// stays the one the chat started on — against an older host, and from
    /// an older service.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub models_per_turn: bool,
}

/// One approval mode an agent offers: its own id, and what it says of it.
/// A client words the ids it knows and shows these for the rest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentModeView {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// **What a local agent can be told to do by name** (koru-ai D121): Codex's
/// skills for the chat's folder, in a picker's order — the folder's, the
/// person's, the agent's own, an administrator's, then by name — enabled
/// ones only, each name once. Empty for an agent with none: a client then
/// offers nothing under `$`. Where each lives stays with the agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentCommandsResponse {
    pub provider: AiProviderId,
    pub commands: Vec<AgentCommandView>,
}

/// One entry an agent offers: the name its message says (`$name`), one
/// line about it, and the agent's own name to show when it gives one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentCommandView {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub kind: AgentCommandKindView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hint: Option<String>,
}

/// Where an entry comes from, in the order a picker lists them.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentCommandKindView {
    /// A provider command invoked by `/name`.
    Command,
    /// A skill of the folder the agent works in.
    RepoSkill,
    /// A skill of the person's own.
    UserSkill,
    /// A skill the agent ships with.
    SystemSkill,
    /// A skill an administrator installed.
    AdminSkill,
}

/// The commands route's body: the folder the chat's agent works in, the
/// person's home folder when absent — where an agent works until a folder
/// is chosen.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiscoverAgentCommandsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

/// The models route's optional body. With `refresh`, the agent's list is
/// rebuilt before it is answered: the session host re-probes the agent and
/// lists it again, as it does after a CLI update. A request already
/// rebuilding the same agent's list is joined, not repeated. With no body,
/// the list already known is answered.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiscoverAgentModelsRequest {
    #[serde(default)]
    pub refresh: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentModelView {
    /// The id the agent itself takes, e.g. `deepseek/v4-pro`.
    pub id: String,
    /// The agent's grouping of it — OpenCode's provider, Codex's one group.
    pub group: String,
    /// The agent's own name for the model, e.g. `Claude Opus 4.7`. Absent
    /// when the agent sends none; a client then formats the id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The agent's own description of the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The agent's own name for the group, e.g. `DeepSeek`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_name: Option<String>,
    /// The agent marks the model as older. Koru never infers it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub legacy: bool,
    /// The reasoning levels the model offers, in Koru's words, and the
    /// agent's stated default when it states one. Absent for a model with
    /// none: a client shows it no level control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<koru_ai_core::ReasoningControl>,
    /// A longer context window the model runs with under an id of its own
    /// (koru-ai D10). A chat chooses it by taking that id as its model.
    /// Absent for a model with no such choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<AgentModelContext>,
    /// The model offers the agent's Fast mode (koru-ai D11), and whether it
    /// runs in it by default. Absent for a model without one: a client
    /// shows no Fast control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<AgentModelFast>,
}

/// A model's Fast mode: whether the agent runs the model in it when a chat
/// has not chosen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentModelFast {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub on_by_default: bool,
}

/// A model's longer context window: the id that runs the model with it,
/// and its size in tokens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentModelContext {
    pub id: String,
    pub tokens: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OpenAiSettingsView {
    pub model: Option<String>,
    pub reasoning: ReasoningChoice,
    pub credential_configured: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OllamaSettingsView {
    pub endpoint: String,
    pub model: Option<String>,
    pub reasoning: ReasoningChoice,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectProviderRequest {
    pub provider: AiProviderId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateOpenAiSettingsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub reasoning: ReasoningChoice,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateOllamaSettingsRequest {
    pub endpoint: String,
    pub model: Option<String>,
    pub reasoning: ReasoningChoice,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderValidationResponse {
    pub provider: AiProviderId,
    pub status: ProviderValidationStatus,
    pub models: Vec<String>,
    pub selected_model: Option<ModelDescriptor>,
    pub failure: Option<Failure>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderValidationStatus {
    Unconfigured,
    Unavailable,
    NoModels,
    Ready,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PullOllamaModelRequest {
    pub model: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullOllamaModelAccepted {
    pub model: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelPullEvent {
    Progress {
        model: String,
        status: String,
        completed_bytes: Option<u64>,
        total_bytes: Option<u64>,
    },
    Completed {
        model: String,
    },
    Failed {
        failure: Failure,
    },
    Cancelled {
        model: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CancelModelPullResponse {
    pub cancelled: bool,
}

#[cfg(test)]
mod tests {
    mod active_attempt;

    #[test]
    fn m8_team_codes_are_distinct_stable_and_match_the_owners_refusals() {
        use koru_ai_core::bots::BotError;
        assert_eq!(crate::AI_WIRE_VERSION, 1);
        assert_eq!(
            crate::bot_team_codes::ALL,
            ["team_paused", "team_revision_conflict"]
        );
        assert_eq!(
            BotError::TeamPaused.code(),
            crate::bot_team_codes::TEAM_PAUSED
        );
        assert_eq!(
            BotError::TeamRevisionConflict.code(),
            crate::bot_team_codes::TEAM_REVISION_CONFLICT
        );
        // Not the code a paused Bot's own refusal carries today.
        assert_ne!(BotError::TeamPaused.code(), BotError::Forbidden.code());
        for code in crate::bot_team_codes::ALL {
            assert!(
                code.len() <= 64 && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{code}"
            );
        }
    }

    use super::*;

    fn transcript() -> ChatTranscript {
        let chat_id = ChatId::new();
        ChatTranscript {
            active_attempt: None,
            chat: Chat {
                id: chat_id,
                title: "Compatibility fixture".into(),
                private: false,
                current_model: ModelSelection {
                    provider: AiProviderId::parse("mock").unwrap(),
                    requested_model: "mock-v1".into(),
                    default_reasoning: ReasoningChoice::Default,
                },
                branched_from: None,
                agent_working_folder: None,
                agent_mode: None,
                agent_fast: None,
                pinned_at: None,
                profile_id: None,
                bot: None,
                created_at: Timestamp::from_millis(1),
                updated_at: Timestamp::from_millis(1),
            },
            tool_policy: ChatToolPolicy {
                chat_id,
                mode: ToolMode::Manual,
                workspace_ids: Vec::new(),
            },
            tool_activities: Vec::new(),
            notices: Vec::new(),
            messages: Vec::new(),
            blocks: Vec::new(),
            pinned_documents: Vec::new(),
            feedback: Vec::new(),
            usage: Vec::new(),
            sources: Vec::new(),
            reasoning_control: None,
            agent_commands: None,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn attachments_are_additive_on_the_send_and_the_transcript() {
        let mut response = serde_json::to_value(transcript()).unwrap();
        response.as_object_mut().unwrap().remove("attachments");
        assert!(
            serde_json::from_value::<ChatTranscript>(response)
                .unwrap()
                .attachments
                .is_empty()
        );

        let base = serde_json::json!({
            "operation_id": Uuid::new_v4(),
            "chat_id": ChatId::new(),
            "surface": "standalone",
            "markdown": "Question"
        });
        let decoded = serde_json::from_value::<SendTurnRequest>(base.clone()).unwrap();
        assert!(decoded.attachment_ids.is_empty());
        assert!(
            serde_json::to_value(&decoded)
                .unwrap()
                .get("attachment_ids")
                .is_none(),
            "a client that sends no attachment is byte-for-byte what it was"
        );
        let mut with_empty = base;
        with_empty["attachment_ids"] = serde_json::json!([]);
        assert!(serde_json::from_value::<SendTurnRequest>(with_empty).is_ok());
    }

    #[test]
    fn no_public_attachment_schema_carries_a_path() {
        fn has_property(value: &serde_json::Value, name: &str) -> bool {
            match value {
                serde_json::Value::Object(object) => {
                    object
                        .get("properties")
                        .and_then(serde_json::Value::as_object)
                        .is_some_and(|properties| properties.contains_key(name))
                        || object.values().any(|value| has_property(value, name))
                }
                serde_json::Value::Array(values) => {
                    values.iter().any(|value| has_property(value, name))
                }
                _ => false,
            }
        }
        for schema in [
            serde_json::to_value(schemars::schema_for!(ChatTranscript)).unwrap(),
            serde_json::to_value(schemars::schema_for!(ChatAttachmentResponse)).unwrap(),
            serde_json::to_value(schemars::schema_for!(SendTurnRequest)).unwrap(),
            serde_json::to_value(schemars::schema_for!(StreamEvent)).unwrap(),
        ] {
            for private in ["path", "native", "content_blake3"] {
                assert!(!has_property(&schema, private), "{private}");
            }
        }
    }

    #[test]
    fn requests_refuse_unknown_fields() {
        let request = serde_json::json!({
            "chat_id": ChatId::new(),
            "title": "Test",
            "model": {
                "provider": "openai",
                "requested_model": "test-model",
                "default_reasoning": "default"
            },
            "unknown": true
        });
        assert!(serde_json::from_value::<CreateChatRequest>(request).is_err());
    }

    #[test]
    fn responses_accept_additive_fields() {
        let response = serde_json::json!({
            "attempt_id": InferenceAttemptId::new(),
            "status": "complete",
            "future_field": 42
        });
        assert!(serde_json::from_value::<CancelAttemptResponse>(response).is_ok());
    }

    /// An ordinary chat's transcript, pinned byte for byte: what the Bots
    /// API adds for a Bot conversation's replies must leave these bytes
    /// exactly as they were.
    #[test]
    fn an_ordinary_transcript_keeps_its_golden_bytes() {
        let id = |n: u8| format!("00000000-0000-4000-8000-0000000000{n:02x}");
        let chat_id = ChatId::parse(&id(1)).unwrap();
        let mut transcript = transcript();
        transcript.chat.id = chat_id;
        transcript.tool_policy.chat_id = chat_id;
        let message = |n: u8, ordinal, role, markdown: &str| Message {
            id: MessageId::parse(&id(n)).unwrap(),
            chat_id,
            ordinal,
            role,
            state: koru_ai_core::MessageState::Complete,
            markdown: markdown.into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: Timestamp::from_millis(2),
            completed_at: Some(Timestamp::from_millis(3)),
        };
        transcript.messages = vec![
            message(2, 0, koru_ai_core::ChatRole::User, "Question"),
            message(3, 1, koru_ai_core::ChatRole::Assistant, "Answer"),
        ];
        transcript.usage = vec![MessageUsage {
            message_id: MessageId::parse(&id(3)).unwrap(),
            usage: AiUsage {
                prompt_tokens: Some(12),
                completion_tokens: Some(34),
                ..AiUsage::default()
            },
            context: Some(ReportedContextUsage { used: 5, size: 10 }),
            stop_code: None,
            run_id: None,
        }];
        let bytes = serde_json::to_string(&transcript).unwrap();
        assert_eq!(bytes, ORDINARY_TRANSCRIPT);
        assert_eq!(
            serde_json::from_str::<ChatTranscript>(ORDINARY_TRANSCRIPT).unwrap(),
            transcript
        );
        // An entry written without the optional fields still reads: losing
        // a `#[serde(default)]` would fail here, where the bytes above
        // cannot tell.
        let mut sparse: serde_json::Value = serde_json::from_str(ORDINARY_TRANSCRIPT).unwrap();
        sparse["usage"][0]
            .as_object_mut()
            .unwrap()
            .remove("context");
        let read = serde_json::from_value::<ChatTranscript>(sparse).unwrap();
        assert_eq!(read.usage[0].context, None);
        assert_eq!(read.usage[0].stop_code, None);
        assert_eq!(read.usage[0].run_id, None);
    }

    const ORDINARY_TRANSCRIPT: &str = r#"{"chat":{"id":"00000000-0000-4000-8000-000000000001","title":"Compatibility fixture","private":false,"current_model":{"provider":"mock","requested_model":"mock-v1","default_reasoning":"default"},"branched_from":null,"created_at":1,"updated_at":1},"tool_policy":{"chat_id":"00000000-0000-4000-8000-000000000001","mode":"manual","workspace_ids":[]},"tool_activities":[],"messages":[{"id":"00000000-0000-4000-8000-000000000002","chat_id":"00000000-0000-4000-8000-000000000001","ordinal":0,"role":"user","state":"complete","markdown":"Question","origin_message_id":null,"provider":null,"requested_model":null,"resolved_model":null,"error_class":null,"created_at":2,"completed_at":3},{"id":"00000000-0000-4000-8000-000000000003","chat_id":"00000000-0000-4000-8000-000000000001","ordinal":1,"role":"assistant","state":"complete","markdown":"Answer","origin_message_id":null,"provider":null,"requested_model":null,"resolved_model":null,"error_class":null,"created_at":2,"completed_at":3}],"blocks":[],"pinned_documents":[],"feedback":[],"usage":[{"message_id":"00000000-0000-4000-8000-000000000003","usage":{"prompt_tokens":12,"cached_prompt_tokens":null,"completion_tokens":34,"reasoning_tokens":null,"total_tokens":null,"source":"unavailable"},"context":{"used":5,"size":10}}],"sources":[],"attachments":[]}"#;

    #[test]
    fn transcripts_without_usage_remain_compatible() {
        let mut response = serde_json::to_value(transcript()).unwrap();
        response.as_object_mut().unwrap().remove("usage");

        let decoded = serde_json::from_value::<ChatTranscript>(response).unwrap();
        assert!(decoded.usage.is_empty());
    }

    #[test]
    fn older_message_usage_without_context_still_decodes() {
        let entry = MessageUsage {
            message_id: MessageId::new(),
            usage: AiUsage::default(),
            context: None,
            stop_code: None,
            run_id: None,
        };
        let mut value = serde_json::to_value(&entry).unwrap();
        assert!(value.get("context").is_none());
        assert!(value.get("stop_code").is_none());
        let decoded = serde_json::from_value::<MessageUsage>(value.clone()).unwrap();
        assert_eq!(decoded, entry);

        value["context"] = serde_json::json!({"used": 48_000, "size": 258_400});
        let decoded = serde_json::from_value::<MessageUsage>(value.clone()).unwrap();
        assert_eq!(
            decoded.context,
            Some(ReportedContextUsage {
                used: 48_000,
                size: 258_400,
            })
        );
        // A refused reply's stable code reads back; an older reader ignores it.
        value["stop_code"] = serde_json::json!("bot_input_pictures_refused");
        let decoded = serde_json::from_value::<MessageUsage>(value.clone()).unwrap();
        assert_eq!(
            decoded.stop_code.as_deref(),
            Some("bot_input_pictures_refused")
        );
        // A Bot reply's Run reads back, and a reader that predates it, which
        // rejects no unknown field, ignores it.
        let run = koru_ai_core::bots::RunId::mint();
        value["run_id"] = serde_json::json!(run);
        let decoded = serde_json::from_value::<MessageUsage>(value.clone()).unwrap();
        assert_eq!(decoded.run_id, Some(run));
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct Older {
            message_id: MessageId,
            usage: AiUsage,
            stop_code: Option<String>,
        }
        assert!(serde_json::from_value::<Older>(value).is_ok());
    }

    #[test]
    fn web_search_request_fields_default_off_and_serialize_only_when_armed() {
        let base = serde_json::json!({
            "operation_id": Uuid::new_v4(),
            "chat_id": ChatId::new(),
            "surface": "standalone",
            "markdown": "Question"
        });
        let decoded = serde_json::from_value::<SendTurnRequest>(base).unwrap();
        assert!(!decoded.web_search);
        let unarmed = serde_json::to_value(&decoded).unwrap();
        assert!(unarmed.get("web_search").is_none());

        let armed = SendTurnRequest {
            web_search: true,
            skills: Vec::new(),
            ..decoded
        };
        assert_eq!(serde_json::to_value(armed).unwrap()["web_search"], true);
    }

    #[test]
    fn mini_app_refinement_defaults_absent_and_round_trips_only_on_turn_requests() {
        use koru_ai_core::mini_apps::{
            Declaration, RefinementContext, RefinementTarget, composition,
        };
        let mut send: SendTurnRequest = serde_json::from_value(serde_json::json!({
            "operation_id": Uuid::new_v4(), "chat_id": ChatId::new(),
            "surface":"standalone", "markdown":"Change the spacing"
        }))
        .unwrap();
        let mut retry: RetryTurnRequest = serde_json::from_value(serde_json::json!({
            "operation_id": Uuid::new_v4(), "message_id": MessageId::new(), "surface":"standalone"
        }))
        .unwrap();
        assert!(send.mini_app_refinement.is_none());
        assert!(retry.mini_app_refinement.is_none());
        assert!(
            serde_json::to_value(&send)
                .unwrap()
                .get("mini_app_refinement")
                .is_none()
        );
        assert!(
            serde_json::to_value(&retry)
                .unwrap()
                .get("mini_app_refinement")
                .is_none()
        );
        let target = RefinementTarget {
            tab_id: 5,
            surface_id: Uuid::new_v4(),
            base_revision: 3,
        };
        let mut declaration = Declaration::parse(composition::FIXTURE.as_bytes())
            .unwrap()
            .envelope;
        declaration.surface.surface_id = target.surface_name();
        let captured = RefinementContext {
            target,
            declaration,
        };
        send.mini_app_refinement = Some(captured.clone());
        retry.mini_app_refinement = Some(captured);
        assert_eq!(
            serde_json::from_value::<SendTurnRequest>(serde_json::to_value(&send).unwrap())
                .unwrap(),
            send
        );
        assert_eq!(
            serde_json::from_value::<RetryTurnRequest>(serde_json::to_value(&retry).unwrap())
                .unwrap(),
            retry
        );
        assert!(
            serde_json::to_value(transcript())
                .unwrap()
                .get("mini_app_refinement")
                .is_none()
        );
    }

    #[test]
    fn transcripts_without_sources_remain_compatible() {
        let mut response = serde_json::to_value(transcript()).unwrap();
        response.as_object_mut().unwrap().remove("sources");
        assert!(
            serde_json::from_value::<ChatTranscript>(response)
                .unwrap()
                .sources
                .is_empty()
        );
    }

    #[test]
    fn chats_without_a_privacy_field_remain_public() {
        let mut response = serde_json::to_value(transcript()).unwrap();
        response["chat"].as_object_mut().unwrap().remove("private");

        let decoded = serde_json::from_value::<ChatTranscript>(response).unwrap();
        assert!(!decoded.chat.private);
    }

    #[test]
    fn privacy_requests_are_strict_and_responses_are_additive() {
        assert!(
            serde_json::from_value::<UpdateChatPrivacyRequest>(serde_json::json!({
                "private": true,
                "unexpected": false
            }))
            .is_err()
        );
        let response = serde_json::json!({
            "chat": serde_json::to_value(transcript().chat).unwrap(),
            "future_field": 1
        });
        assert!(serde_json::from_value::<UpdateChatPrivacyResponse>(response).is_ok());
    }

    #[test]
    fn chat_tool_routes_round_trip_strict_requests_and_additive_responses() {
        let chat_id = ChatId::new();
        let request = ReadChatToolRequest {
            chat_id,
            cursor: Some("12".into()),
            page_size: Some(7),
        };
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(
            serde_json::from_value::<ReadChatToolRequest>(encoded).unwrap(),
            request
        );
        assert!(
            serde_json::from_value::<ListChatsToolRequest>(serde_json::json!({
                "future_request_field": true
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<SearchChatsToolRequest>(serde_json::json!({
                "query": "roadmap",
                "unexpected": 1
            }))
            .is_err()
        );

        let response = serde_json::json!({
            "chat_id": chat_id,
            "title": "English fixture",
            "total_messages": 0,
            "messages": [],
            "next_cursor": null,
            "future_response_field": true
        });
        assert!(serde_json::from_value::<ReadChatToolResponse>(response).is_ok());
    }

    #[test]
    fn missing_required_request_fields_are_errors() {
        let request = serde_json::json!({
            "operation_id": Uuid::new_v4(),
            "chat_id": ChatId::new(),
            "surface": "standalone"
        });
        assert!(serde_json::from_value::<SendTurnRequest>(request).is_err());
    }

    #[test]
    fn public_id_schemas_are_uuid_strings() {
        let schema = serde_json::to_value(schemars::schema_for!(CreateChatRequest)).unwrap();
        let definitions = schema["$defs"].as_object().expect("schema definitions");
        assert_eq!(definitions["ChatId"]["format"], "uuid");
    }

    #[test]
    fn provider_wire_types_carry_no_secret_or_vault_identifier() {
        let settings = ProviderSettingsResponse {
            selected_provider: Some(AiProviderId::parse("openai").unwrap()),
            openai: OpenAiSettingsView {
                model: Some("gpt-5.6".into()),
                reasoning: ReasoningChoice::Default,
                credential_configured: true,
            },
            ollama: OllamaSettingsView {
                endpoint: "http://localhost:11434/".into(),
                model: None,
                reasoning: ReasoningChoice::Default,
            },
            agents: Vec::new(),
        };
        let value = serde_json::to_value(settings).unwrap();
        let serialized = value.to_string();
        assert!(!serialized.contains("api_key"));
        assert!(!serialized.contains("key_account"));
        assert!(!serialized.contains("vault"));
        assert_eq!(value["openai"]["credential_configured"], true);

        for schema in [
            serde_json::to_value(schemars::schema_for!(ProviderSettingsResponse)).unwrap(),
            serde_json::to_value(schemars::schema_for!(UpdateOpenAiSettingsRequest)).unwrap(),
        ] {
            let schema = schema.to_string();
            assert!(!schema.contains("api_key"));
            assert!(!schema.contains("key_account"));
            assert!(!schema.contains("vault_label"));
        }
    }

    #[test]
    fn openai_settings_can_request_model_discovery_before_selection() {
        let request = UpdateOpenAiSettingsRequest {
            model: None,
            reasoning: ReasoningChoice::Default,
        };
        let value = serde_json::to_value(request).unwrap();
        assert!(value.get("model").is_none());
        assert_eq!(value["reasoning"], "default");
    }

    #[test]
    fn public_stream_schema_cannot_expose_provider_tool_payloads() {
        fn has_property(value: &serde_json::Value, name: &str) -> bool {
            match value {
                serde_json::Value::Object(object) => {
                    object
                        .get("properties")
                        .and_then(serde_json::Value::as_object)
                        .is_some_and(|properties| properties.contains_key(name))
                        || object.values().any(|value| has_property(value, name))
                }
                serde_json::Value::Array(values) => {
                    values.iter().any(|value| has_property(value, name))
                }
                _ => false,
            }
        }

        let schema = serde_json::to_value(schemars::schema_for!(StreamEvent)).unwrap();
        for private_property in ["arguments", "thought_signatures", "tool_calls"] {
            assert!(
                !has_property(&schema, private_property),
                "{private_property}"
            );
        }

        let attempt_id = InferenceAttemptId::new();
        let internal = AiEvent::ToolCalls {
            attempt_id,
            calls: vec![],
            stop_reason: None,
            at: Timestamp::now(),
        };
        assert!(PublicAiEvent::try_from(internal).is_err());
    }

    #[test]
    fn reasoning_delta_round_trips_and_is_skipped_by_unknown_clients() {
        let attempt_id = InferenceAttemptId::new();
        let event = PublicAiEvent::ReasoningDelta {
            attempt_id,
            text: "plan".into(),
            at: Timestamp::now(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "reasoning_delta");
        let back: PublicAiEvent = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(back, event);
        assert_eq!(value["text"], "plan");
    }

    /// O128: the "Needed you" closing event is terminal, is not a failure,
    /// and round-trips with its positively known reason.
    #[test]
    fn needed_you_is_a_terminal_closing_event_not_a_failure() {
        let attempt_id = InferenceAttemptId::new();
        let event = PublicAiEvent::NeededYou {
            attempt_id,
            reason: Some(NeededYouReason::Declined),
            at: Timestamp::from_millis(7),
        };
        assert!(event.is_terminal());
        assert_eq!(event.attempt_id(), attempt_id);
        assert!(!matches!(event, PublicAiEvent::Failed { .. }));
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "needed_you");
        assert_eq!(value["reason"], "declined");
        assert_eq!(
            serde_json::from_value::<PublicAiEvent>(value).unwrap(),
            event
        );
        let unknown = PublicAiEvent::NeededYou {
            attempt_id,
            reason: None,
            at: Timestamp::from_millis(7),
        };
        assert!(
            serde_json::to_value(&unknown)
                .unwrap()
                .get("reason")
                .is_none()
        );
    }

    /// A reason this client does not know, or none at all, still decodes as
    /// the closing event, with no reason: never an invalid stream event.
    #[test]
    fn needed_you_with_an_unknown_or_absent_reason_still_decodes() {
        let attempt_id = InferenceAttemptId::new();
        for reason in [
            Some(serde_json::json!("a_reason_from_a_newer_server")),
            Some(serde_json::json!(null)),
            None,
        ] {
            let mut value = serde_json::json!({
                "type": "needed_you",
                "attempt_id": attempt_id,
                "at": 7,
            });
            if let Some(reason) = reason {
                value["reason"] = reason;
            }
            assert_eq!(
                serde_json::from_value::<PublicAiEvent>(value).unwrap(),
                PublicAiEvent::NeededYou {
                    attempt_id,
                    reason: None,
                    at: Timestamp::from_millis(7),
                }
            );
        }
        let value = serde_json::json!({
            "type": "needed_you", "attempt_id": attempt_id, "reason": "not_answered", "at": 7,
        });
        assert_eq!(
            serde_json::from_value::<PublicAiEvent>(value).unwrap(),
            PublicAiEvent::NeededYou {
                attempt_id,
                reason: Some(NeededYouReason::NotAnswered),
                at: Timestamp::from_millis(7),
            }
        );
    }
}
