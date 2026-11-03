// SPDX-License-Identifier: MPL-2.0

//! Typed internal AI-server boundary.
//!
//! The public gateway supplies a principal and moves owned DTOs through this
//! port. No bearer, HTTP request, database row, or provider SDK value crosses.

use std::sync::mpsc::Receiver;

use koru_ai_core::{
    AttachmentSource, ChatAttachmentId, ChatId, DocumentExportId, InferenceAttemptId, MessageId,
    ToolInvocationId,
};
use koru_ai_wire::{
    AiSearchRequest, AiSearchResponse, AttemptResponse, BranchChatRequest, BranchChatResponse,
    CancelAttemptResponse, CancelModelPullResponse, ChatAttachmentResponse, ChatToolPolicyResponse,
    ChatTranscript, ClearHistoryResponse, ClearUsageResponse, CreateChatRequest,
    CreateChatResponse, DeleteChatResponse, ExportChatRequest, ExportChatResponse, Failure,
    FeedbackRequest, FeedbackResponse, ListChatsToolRequest, ListChatsToolResponse, ModelPullEvent,
    OllamaSettingsView, OpenAiSettingsView, ProviderSettingsResponse, ProviderValidationResponse,
    PullOllamaModelAccepted, PullOllamaModelRequest, ReadChatToolRequest, ReadChatToolResponse,
    RemoveAttachmentResponse, RenameChatRequest, RenameChatResponse, SearchChatsToolRequest,
    SearchChatsToolResponse, SelectProviderRequest, SendTurnAccepted, SendTurnRequest, StreamEvent,
    ToolDecisionResponse, UpdateChatModelRequest, UpdateChatModelResponse,
    UpdateChatPrivacyRequest, UpdateChatPrivacyResponse, UpdateChatToolPolicyRequest,
    UpdateOllamaSettingsRequest, UpdateOpenAiSettingsRequest, UsageResponse,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiPrincipal {
    pub label: String,
    pub scopes: Vec<AiScope>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AiScope {
    Chat,
    ManageProviders,
    ReadUsage,
}

impl AiScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "ai.chat",
            Self::ManageProviders => "ai.manage_providers",
            Self::ReadUsage => "ai.read_usage",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ai.chat" => Some(Self::Chat),
            "ai.manage_providers" => Some(Self::ManageProviders),
            "ai.read_usage" => Some(Self::ReadUsage),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestContext {
    pub principal: AiPrincipal,
    pub trace_id: String,
    pub deadline_millis: Option<i64>,
}

pub struct AiResponseStream {
    pub accepted: SendTurnAccepted,
    pub events: Receiver<StreamEvent>,
}

pub struct ModelPullResponseStream {
    pub accepted: PullOllamaModelAccepted,
    pub events: Receiver<ModelPullEvent>,
}

pub type Result<T> = std::result::Result<T, Failure>;

/// One upload's bytes and what the caller says about them. The name is a
/// claim the service checks against the content; no path ever crosses.
pub struct AttachmentUpload {
    pub file_name: String,
    pub source: AttachmentSource,
    pub bytes: bytes::Bytes,
}

/// A stored attachment's original, as it is served back.
pub struct AttachmentContent {
    pub file_name: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// Host-independent AI server contract used by the gateway's internal client.
pub trait AiServerPort: Send + Sync {
    fn provider_settings(&self, context: RequestContext) -> Result<ProviderSettingsResponse>;
    fn select_provider(
        &self,
        context: RequestContext,
        request: SelectProviderRequest,
    ) -> Result<ProviderSettingsResponse>;
    fn update_openai_settings(
        &self,
        context: RequestContext,
        request: UpdateOpenAiSettingsRequest,
    ) -> Result<OpenAiSettingsView>;
    fn update_ollama_settings(
        &self,
        context: RequestContext,
        request: UpdateOllamaSettingsRequest,
    ) -> Result<OllamaSettingsView>;
    fn validate_provider(
        &self,
        context: RequestContext,
        provider: koru_ai_core::AiProviderId,
    ) -> Result<ProviderValidationResponse>;
    /// The models a local agent lists, for the picker's level under it;
    /// rebuilt first when the request asks for a refresh.
    fn discover_agent_models(
        &self,
        context: RequestContext,
        provider: koru_ai_core::AiProviderId,
        request: koru_ai_wire::DiscoverAgentModelsRequest,
    ) -> Result<koru_ai_wire::AgentModelsResponse>;
    fn pull_ollama_model(
        &self,
        context: RequestContext,
        request: PullOllamaModelRequest,
    ) -> Result<ModelPullResponseStream>;
    fn cancel_ollama_pull(&self, context: RequestContext) -> Result<CancelModelPullResponse>;
    fn create_chat(
        &self,
        context: RequestContext,
        request: CreateChatRequest,
    ) -> Result<CreateChatResponse>;
    fn list_chats(&self, context: RequestContext) -> Result<koru_ai_wire::ListChatsResponse>;
    fn read_chat(&self, context: RequestContext, chat: ChatId) -> Result<ChatTranscript>;
    fn rename_chat(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: RenameChatRequest,
    ) -> Result<RenameChatResponse>;
    fn update_chat_model(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: UpdateChatModelRequest,
    ) -> Result<UpdateChatModelResponse>;
    fn update_chat_privacy(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: UpdateChatPrivacyRequest,
    ) -> Result<UpdateChatPrivacyResponse>;
    fn update_chat_folder(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::UpdateChatFolderRequest,
    ) -> Result<koru_ai_wire::UpdateChatFolderResponse>;
    fn update_chat_agent_settings(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::UpdateChatAgentSettingsRequest,
    ) -> Result<koru_ai_wire::UpdateChatAgentSettingsResponse>;
    fn tool_list_chats(
        &self,
        context: RequestContext,
        request: ListChatsToolRequest,
    ) -> Result<ListChatsToolResponse>;
    fn tool_search_chats(
        &self,
        context: RequestContext,
        request: SearchChatsToolRequest,
    ) -> Result<SearchChatsToolResponse>;
    fn tool_read_chat(
        &self,
        context: RequestContext,
        request: ReadChatToolRequest,
    ) -> Result<ReadChatToolResponse>;
    fn read_chat_tool_policy(
        &self,
        context: RequestContext,
        chat: ChatId,
    ) -> Result<ChatToolPolicyResponse>;
    fn update_chat_tool_policy(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: UpdateChatToolPolicyRequest,
    ) -> Result<ChatToolPolicyResponse>;
    fn usage(&self, context: RequestContext, chat: Option<ChatId>) -> Result<UsageResponse>;
    fn delete_chat(&self, context: RequestContext, chat: ChatId) -> Result<DeleteChatResponse>;
    fn clear_history(&self, context: RequestContext) -> Result<ClearHistoryResponse>;
    fn clear_usage(&self, context: RequestContext) -> Result<ClearUsageResponse>;
    fn search(&self, context: RequestContext, request: AiSearchRequest)
    -> Result<AiSearchResponse>;
    fn feedback(
        &self,
        context: RequestContext,
        message: MessageId,
        request: FeedbackRequest,
    ) -> Result<FeedbackResponse>;
    fn branch_chat(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: BranchChatRequest,
    ) -> Result<BranchChatResponse>;
    fn send_turn(
        &self,
        context: RequestContext,
        request: SendTurnRequest,
    ) -> Result<AiResponseStream>;
    fn read_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
    ) -> Result<AttemptResponse>;
    fn cancel_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
    ) -> Result<CancelAttemptResponse>;
    /// Send text into a running local agent reply (a steer). Answered once
    /// the agent took it, with the person's message as recorded after the
    /// reply; `steer_unavailable` when the reply cannot take text now.
    fn steer_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
        request: koru_ai_wire::SteerAttemptRequest,
    ) -> Result<koru_ai_wire::SteerAttemptResponse>;
    fn approve_tool_call(
        &self,
        context: RequestContext,
        tool: ToolInvocationId,
        choice: Option<String>,
    ) -> Result<ToolDecisionResponse>;
    fn deny_tool_call(
        &self,
        context: RequestContext,
        tool: ToolInvocationId,
        choice: Option<String>,
    ) -> Result<ToolDecisionResponse>;
    fn retry_turn(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::RetryTurnRequest,
    ) -> Result<AiResponseStream>;
    fn export_chat(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: ExportChatRequest,
    ) -> Result<ExportChatResponse>;
    fn read_export(
        &self,
        context: RequestContext,
        export: DocumentExportId,
    ) -> Result<ExportChatResponse>;
    /// Store one attachment for a chat, staged until a send names it.
    /// Idempotent by id: the same content answers the same attachment.
    fn put_attachment(
        &self,
        context: RequestContext,
        chat: ChatId,
        attachment: ChatAttachmentId,
        upload: AttachmentUpload,
    ) -> Result<ChatAttachmentResponse>;
    /// Remove a staged attachment; a sent one answers `attachment_sent`.
    fn remove_attachment(
        &self,
        context: RequestContext,
        chat: ChatId,
        attachment: ChatAttachmentId,
    ) -> Result<RemoveAttachmentResponse>;
    fn read_attachment(
        &self,
        context: RequestContext,
        chat: ChatId,
        attachment: ChatAttachmentId,
    ) -> Result<AttachmentContent>;
    /// The PNG thumbnail of a picture attachment.
    fn read_attachment_thumbnail(
        &self,
        context: RequestContext,
        chat: ChatId,
        attachment: ChatAttachmentId,
    ) -> Result<Vec<u8>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_tokens_are_namespaced_and_round_trip() {
        for scope in [AiScope::Chat, AiScope::ManageProviders, AiScope::ReadUsage] {
            assert!(scope.as_str().starts_with("ai."));
            assert_eq!(AiScope::parse(scope.as_str()), Some(scope));
        }
        assert_eq!(AiScope::parse("store.read_documents"), None);
    }
}
