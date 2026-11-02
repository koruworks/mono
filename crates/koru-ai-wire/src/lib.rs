// SPDX-License-Identifier: MPL-2.0

//! Public Koru AI wire declarations.
//!
//! Request objects reject unknown fields. Response and event objects retain
//! serde's additive-field behavior so an older client can read a newer server.

use std::collections::BTreeMap;

use koru_ai_core::{
    AiError, AiEvent, AiProviderId, AiSearchPage, AiSurface, AiUsage, AttemptStatus, Chat, ChatId,
    ChatToolPolicy, ContextRef, DocumentDestination, DocumentExport, DocumentExportId,
    FeedbackEvent, FeedbackEventId, FeedbackValue, InferenceAttempt, InferenceAttemptId, Message,
    MessageBlock, MessageId, ModelDescriptor, ModelSelection, PinnedDocument, ReasoningChoice,
    SourceLink, Timestamp, ToolActivity, ToolMode, UsageReport, WebSearchPhase, WorkspaceId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use koru_ai_core::{
    AttachmentKind, AttachmentSource, ChatAttachment, ChatAttachmentId, ListChatsToolRequest,
    ListChatsToolResponse, ReadChatToolRequest, ReadChatToolResponse, SearchChatsToolRequest,
    SearchChatsToolResponse, WorkspaceFileOrigin,
};

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

pub const AI_WIRE_VERSION: u32 = 1;
pub const AI_STREAM_VERSION: u32 = 1;
pub const MODEL_PULL_STREAM_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateChatRequest {
    pub chat_id: ChatId,
    pub title: String,
    pub model: ModelSelection,
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
pub struct MessageUsage {
    pub message_id: MessageId,
    pub usage: AiUsage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MessageSources {
    pub message_id: MessageId,
    pub sources: Vec<SourceLink>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatTranscript {
    pub chat: Chat,
    pub tool_policy: ChatToolPolicy,
    #[serde(default)]
    pub tool_activities: Vec<ToolActivity>,
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
pub struct ChatToolPolicyResponse {
    pub policy: ChatToolPolicy,
}

/// The optional body of an approve or deny. A local agent offers its own
/// choices for a permission request; the chosen one's id comes back here.
/// A workspace tool offers none and takes no body. An agent's row with no
/// choice is refused, so two same-intent choices can never collapse.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolDecisionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
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
/// summary, never raw tool arguments or results.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PublicAiEvent {
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
    ToolActivity {
        attempt_id: InferenceAttemptId,
        activity: ToolActivity,
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
}

impl PublicAiEvent {
    pub const fn attempt_id(&self) -> InferenceAttemptId {
        match self {
            Self::Started { attempt_id, .. }
            | Self::Notice { attempt_id, .. }
            | Self::TextDelta { attempt_id, .. }
            | Self::ReasoningDelta { attempt_id, .. }
            | Self::Usage { attempt_id, .. }
            | Self::ToolActivity { attempt_id, .. }
            | Self::WebSearch { attempt_id, .. }
            | Self::Citation { attempt_id, .. }
            | Self::Completed { attempt_id, .. }
            | Self::Failed { attempt_id, .. }
            | Self::Cancelled { attempt_id, .. } => *attempt_id,
        }
    }

    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Failed { .. } | Self::Cancelled { .. }
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
            AiEvent::ToolActivity {
                attempt_id,
                activity,
            } => Self::ToolActivity {
                attempt_id,
                activity,
            },
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
                error,
                at,
            } => Self::Failed {
                attempt_id,
                error,
                at,
            },
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
    use super::*;

    fn transcript() -> ChatTranscript {
        let chat_id = ChatId::new();
        ChatTranscript {
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
                created_at: Timestamp::from_millis(1),
                updated_at: Timestamp::from_millis(1),
            },
            tool_policy: ChatToolPolicy {
                chat_id,
                mode: ToolMode::Manual,
                workspace_ids: Vec::new(),
            },
            tool_activities: Vec::new(),
            messages: Vec::new(),
            blocks: Vec::new(),
            pinned_documents: Vec::new(),
            feedback: Vec::new(),
            usage: Vec::new(),
            sources: Vec::new(),
            reasoning_control: None,
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

    #[test]
    fn transcripts_without_usage_remain_compatible() {
        let mut response = serde_json::to_value(transcript()).unwrap();
        response.as_object_mut().unwrap().remove("usage");

        let decoded = serde_json::from_value::<ChatTranscript>(response).unwrap();
        assert!(decoded.usage.is_empty());
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
            ..decoded
        };
        assert_eq!(serde_json::to_value(armed).unwrap()["web_search"], true);
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
}
