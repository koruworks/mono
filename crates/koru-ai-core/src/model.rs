// SPDX-License-Identifier: MPL-2.0

//! Authoritative chat, context, attempt, feedback, export, and search models.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AiErrorClass, AiProviderId, AiUsage, ChatId, DocumentExportId, DocumentId, FeedbackEventId,
    InferenceAttemptId, MessageBlockId, MessageId, ModelSelection, ProfileId, ReasoningChoice,
    RequestMeasurements, Timestamp, ToolInvocationId, WorkspaceId,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Chat {
    pub id: ChatId,
    pub title: String,
    #[serde(default)]
    pub private: bool,
    pub current_model: ModelSelection,
    pub branched_from: Option<BranchOrigin>,
    /// The folder a local agent works in for this chat, absolute; `None` is
    /// the person's home folder. Only an `agent-*` chat sets it, before its
    /// first message, and a branch copies it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_working_folder: Option<String>,
    /// The approval mode a local agent runs this chat's next message in
    /// (koru-ai D7): the agent's own id, `None` for the agent's default.
    /// Chosen at any time; a choice made during a reply applies to the next
    /// message. A branch starts with none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_mode: Option<String>,
    /// Whether a local agent runs this chat's next message in its model's
    /// Fast mode (koru-ai D11); `None` is the model's own default. Like the
    /// mode, chosen at any time and applied from the next message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_fast: Option<bool>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BranchOrigin {
    pub chat_id: ChatId,
    pub through_message_id: MessageId,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ToolMode {
    #[default]
    Manual,
    Auto,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ChatToolPolicy {
    pub chat_id: ChatId,
    pub mode: ToolMode,
    pub workspace_ids: Vec<WorkspaceId>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ToolActivityStatus {
    Proposed,
    ApprovalRequired,
    Running,
    Complete,
    Denied,
    Failed,
    Cancelled,
}

impl ToolActivityStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Complete | Self::Denied | Self::Failed | Self::Cancelled
        )
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ToolApproval {
    NotRequired,
    Pending,
    Approved,
    Denied,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorCode {
    PermissionDenied,
    ExecutionFailed,
    Cancelled,
}

/// Durable and public tool activity. Raw arguments, results, queries, titles,
/// paths, and workspace content deliberately have no field in this type. The
/// stable target ids and bounded public error summary are the only context kept
/// for an auditable, useful activity row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ToolActivity {
    pub id: ToolInvocationId,
    pub attempt_id: InferenceAttemptId,
    pub chat_id: ChatId,
    pub assistant_message_id: MessageId,
    pub tool_name: String,
    pub read_only: bool,
    pub status: ToolActivityStatus,
    pub approval: ToolApproval,
    pub proposed_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub result_bytes: Option<u64>,
    pub error_code: Option<ToolErrorCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_document_id: Option<DocumentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// A local agent's own label for the call (*Read README.md*), bounded,
    /// kept since schema 8 so a row reads the same after the turn as during
    /// it. `None` on Koru's own tool rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    /// What a local agent's permission request asked, bounded, kept since
    /// schema 8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_summary: Option<String>,
    /// What the agent asked the tool to do — a command line, a change's
    /// paths, an MCP call's arguments — bounded to [`crate::AGENT_CONTENT_CHARS`],
    /// kept since schema 8 (UC17).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_input: Option<String>,
    /// What the tool produced — output, a diff, a result — bounded to
    /// [`crate::AGENT_CONTENT_CHARS`], kept since schema 8 (UC17).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_output: Option<String>,
    /// How much of the reply's Markdown, in bytes, had arrived when the
    /// call began: the text before it was said before it, the text after
    /// it after. Kept since schema 10 on a local agent's rows, so a reopened
    /// reply can still tell the agent's running commentary from its final
    /// answer. `None` on earlier rows and on Koru's own tool rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_offset: Option<u32>,
    /// Current display text resolved from Store by a trusted local client. It
    /// is neither public wire data nor durable AI state, so renames cannot make
    /// the audit database stale.
    #[serde(skip)]
    #[cfg_attr(feature = "schema", schemars(skip))]
    pub display_target: Option<String>,
    /// A local agent's live state for this row — the choices a pending
    /// permission request offers — populated only from the worker's live
    /// turn table while the turn runs. `None` on every repository read and
    /// after the turn ends. The agent's wording lives on the row's own
    /// `agent_*` fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<crate::AgentActivityLive>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MessageState {
    Pending,
    Complete,
    Interrupted,
    Failed,
}

impl MessageState {
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        self == next || matches!(self, Self::Pending)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Message {
    pub id: MessageId,
    pub chat_id: ChatId,
    pub ordinal: u32,
    pub role: ChatRole,
    pub state: MessageState,
    pub markdown: String,
    pub origin_message_id: Option<MessageId>,
    pub provider: Option<AiProviderId>,
    pub requested_model: Option<String>,
    pub resolved_model: Option<String>,
    pub error_class: Option<AiErrorClass>,
    pub created_at: Timestamp,
    pub completed_at: Option<Timestamp>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MessageBlockKind {
    Paragraph,
    Heading,
    ListItem,
    Quote,
    Code,
    Rule,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MessageBlock {
    pub id: MessageBlockId,
    pub message_id: MessageId,
    pub ordinal: u32,
    pub kind: MessageBlockKind,
    pub markdown: String,
    pub plain_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PinnedDocument {
    pub chat_id: ChatId,
    pub document_id: DocumentId,
    pub display_title: String,
    pub pinned: bool,
    pub first_used_at: Timestamp,
    pub last_used_at: Timestamp,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DocumentRelation {
    Context,
    ExportedCopy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct StoreRevision(pub Vec<u8>);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MessageDocumentRef {
    pub message_id: MessageId,
    pub document_id: DocumentId,
    pub relation: DocumentRelation,
    pub display_title: String,
    pub source_revision: Option<StoreRevision>,
    pub used_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageBlockRef {
    pub message_id: MessageId,
    pub source_block_id: MessageBlockId,
    pub used_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextCommit {
    pub attempt_id: InferenceAttemptId,
    pub started_at: Timestamp,
    pub completed_at: Timestamp,
    pub measurements: RequestMeasurements,
    pub pinned_documents: Vec<PinnedDocument>,
    pub document_refs: Vec<MessageDocumentRef>,
    pub message_block_refs: Vec<MessageBlockRef>,
    /// When true, the commit replaces the chat's pinned documents instead of
    /// merging with them.
    pub replace_pinned_documents: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextRef {
    Document { document_id: DocumentId },
    MessageBlock { block_id: MessageBlockId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ContextItem {
    Document(DocumentContext),
    MessageBlock(MessageBlockContext),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DocumentContext {
    pub document_id: DocumentId,
    pub display_title: String,
    /// Internal Store provenance used for durable reference bookkeeping. It is
    /// deliberately omitted from the provider-neutral request serialization;
    /// models receive the document identity, title, and Markdown only.
    #[serde(skip_serializing)]
    #[cfg_attr(feature = "schema", schemars(skip))]
    pub source_revision: StoreRevision,
    pub markdown: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MessageBlockContext {
    pub chat_id: ChatId,
    pub chat_private: bool,
    pub message_id: MessageId,
    pub block_id: MessageBlockId,
    pub role: ChatRole,
    pub markdown: String,
    pub message_created_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatOverview {
    pub chat_id: ChatId,
    pub title: String,
    pub private: bool,
    pub message_count: u32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "surface", rename_all = "snake_case")]
pub enum ChatAccessLens {
    #[default]
    UserSurface,
    ToolSurface {
        calling_chat: Option<ChatId>,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AiSurface {
    Standalone,
    ContextPane,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AiIntent {
    Chat,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    Pending,
    Streaming,
    Complete,
    Interrupted,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SourceLink {
    pub url: String,
    pub title: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WebSearchPhase {
    Started,
    Completed,
}

impl AttemptStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Complete | Self::Interrupted | Self::Failed | Self::Cancelled
        )
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        match self {
            Self::Pending => true,
            Self::Streaming => !matches!(next, Self::Pending),
            Self::Complete | Self::Interrupted | Self::Failed | Self::Cancelled => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InferenceAttempt {
    pub id: InferenceAttemptId,
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub operation_id: Uuid,
    pub chat_id: Option<ChatId>,
    pub assistant_message_id: Option<MessageId>,
    pub surface: AiSurface,
    pub intent: AiIntent,
    pub provider: AiProviderId,
    pub requested_model: String,
    pub resolved_model: Option<String>,
    pub requested_reasoning: ReasoningChoice,
    pub effective_reasoning: Option<ReasoningChoice>,
    pub prompt_template_version: String,
    pub status: AttemptStatus,
    pub queued_at: Timestamp,
    pub context_started_at: Option<Timestamp>,
    pub context_completed_at: Option<Timestamp>,
    pub provider_started_at: Option<Timestamp>,
    pub first_token_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub request_payload_bytes: Option<u64>,
    #[serde(default)]
    pub web_search_requested: bool,
    #[serde(default)]
    pub web_search_calls: u32,
    pub context_payload_bytes: u64,
    pub message_count: u32,
    pub context_document_count: u32,
    pub context_message_block_count: u32,
    pub usage: AiUsage,
    pub stop_reason: Option<String>,
    pub error_class: Option<AiErrorClass>,
    pub provider_response_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingTurn {
    pub user_message: Message,
    pub assistant_message: Message,
    pub attempt: InferenceAttempt,
    /// Staged attachments bound to the user message, in order, in the same
    /// transaction.
    pub attachment_ids: Vec<crate::ChatAttachmentId>,
}

/// One retry creates a fresh assistant entry and attempt without rewriting the
/// failed or interrupted entry that prompted it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRetry {
    pub assistant_message: Message,
    pub attempt: InferenceAttempt,
}

/// A complete branch payload committed as one transaction. Every copied
/// message and block already carries its newly minted identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchCopy {
    pub chat: Chat,
    pub tool_policy: ChatToolPolicy,
    pub messages: Vec<Message>,
    pub blocks: Vec<MessageBlock>,
    pub citations: Vec<(MessageId, SourceLink)>,
    pub pinned_documents: Vec<PinnedDocument>,
    /// The copied user messages' attachments, already re-identified and
    /// bound to the copies; their folders are linked before the commit.
    pub attachments: Vec<crate::AttachmentRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryCounts {
    pub attempts: u64,
    pub messages: u64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FeedbackValue {
    Good,
    Bad,
    Cleared,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FeedbackEvent {
    pub id: FeedbackEventId,
    pub chat_id: Option<ChatId>,
    pub message_id: Option<MessageId>,
    pub attempt_id: Option<InferenceAttemptId>,
    pub value: FeedbackValue,
    pub surface: AiSurface,
    pub created_at: Timestamp,
}

/// Content-free aggregate measurements for one usage scope or reporting row.
///
/// Provider token fields remain optional so an unavailable measurement cannot
/// be mistaken for a reported zero. Context counts and bytes are measured by
/// Koru itself and therefore have exact zero values.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsageTotals {
    pub attempts: u64,
    pub attempts_with_reported_usage: u64,
    pub request_payload_bytes: Option<u64>,
    #[serde(default)]
    pub web_search_calls: u64,
    pub context_payload_bytes: u64,
    pub message_count: u64,
    pub context_document_count: u64,
    pub context_message_block_count: u64,
    pub prompt_tokens: Option<u64>,
    pub cached_prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub average_context_assembly_ms: Option<u64>,
    pub average_provider_start_ms: Option<u64>,
    pub average_time_to_first_token_ms: Option<u64>,
    pub average_generation_ms: Option<u64>,
    pub average_total_ms: Option<u64>,
    pub good_feedback: u64,
    pub bad_feedback: u64,
}

/// One row in the all-usage report. Every dimension is content-free and can
/// survive history deletion without retaining a chat or message identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsageGroup {
    pub utc_day: String,
    pub surface: AiSurface,
    pub provider: AiProviderId,
    pub model: String,
    pub reasoning: ReasoningChoice,
    pub status: AttemptStatus,
    pub prompt_template_version: String,
    pub totals: UsageTotals,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsageReport {
    pub chat_id: Option<ChatId>,
    pub current_chat: Option<UsageTotals>,
    pub groups: Vec<UsageGroup>,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageClearResult {
    pub attempts: u64,
    pub feedback_events: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DocumentDestination {
    pub profile_id: ProfileId,
    pub workspace_id: WorkspaceId,
    pub parent_document_id: Option<DocumentId>,
    pub index: Option<usize>,
    pub title: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ExportStatus {
    Pending,
    Complete,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DocumentExport {
    pub id: DocumentExportId,
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub idempotency_key: Uuid,
    pub chat_id: ChatId,
    pub through_message_id: MessageId,
    pub destination: DocumentDestination,
    pub status: ExportStatus,
    pub document_id: Option<DocumentId>,
    pub error_class: Option<AiErrorClass>,
    pub created_at: Timestamp,
    pub completed_at: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ExportChatRequest {
    pub export_id: DocumentExportId,
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub idempotency_key: Uuid,
    pub chat_id: ChatId,
    pub through_message_id: MessageId,
    pub destination: DocumentDestination,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AiSearchHit {
    pub chat_id: ChatId,
    pub message_id: MessageId,
    pub block_id: MessageBlockId,
    pub chat_title: String,
    pub snippet: String,
    pub spans: Vec<AiSearchSpan>,
    pub score: f32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AiSearchSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AiSearchQuery {
    pub text: String,
    pub limit: u16,
    pub cursor: Option<String>,
    #[serde(default)]
    pub visibility: ChatAccessLens,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AiSearchPage {
    pub hits: Vec<AiSearchHit>,
    pub next_cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_attempt_states_are_absorbing() {
        for terminal in [
            AttemptStatus::Complete,
            AttemptStatus::Interrupted,
            AttemptStatus::Failed,
            AttemptStatus::Cancelled,
        ] {
            assert!(terminal.is_terminal());
            assert!(terminal.can_transition_to(terminal));
            assert!(!terminal.can_transition_to(AttemptStatus::Streaming));
        }
    }

    #[test]
    fn pending_and_streaming_have_only_forward_transitions() {
        assert!(AttemptStatus::Pending.can_transition_to(AttemptStatus::Streaming));
        assert!(AttemptStatus::Pending.can_transition_to(AttemptStatus::Failed));
        assert!(AttemptStatus::Streaming.can_transition_to(AttemptStatus::Complete));
        assert!(!AttemptStatus::Streaming.can_transition_to(AttemptStatus::Pending));
    }

    #[test]
    fn terminal_message_states_are_absorbing() {
        assert!(MessageState::Pending.can_transition_to(MessageState::Complete));
        assert!(!MessageState::Complete.can_transition_to(MessageState::Interrupted));
        assert!(MessageState::Failed.can_transition_to(MessageState::Failed));
    }

    #[test]
    fn provider_neutral_document_context_omits_store_revision() {
        let context = DocumentContext {
            document_id: DocumentId::new(),
            display_title: "Plan".into(),
            source_revision: StoreRevision(vec![1, 2, 3]),
            markdown: "Body".into(),
        };
        let value = serde_json::to_value(context).expect("serialize document context");
        assert_eq!(value["display_title"], "Plan");
        assert_eq!(value["markdown"], "Body");
        assert!(value.get("source_revision").is_none());
    }
}
