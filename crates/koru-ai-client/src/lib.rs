// SPDX-License-Identifier: MPL-2.0

//! Internal gateway-to-AI-server client.
//!
//! This is not the public product SDK. It wraps an injected server port and
//! owns no discovery, bearer, endpoint, or credential renewal behavior.

use std::sync::Arc;

use koru_ai_api::{
    AiResponseStream, AiServerPort, ModelPullResponseStream, RequestContext, Result,
};
use koru_ai_core::{
    AiProviderId, ChatId, DocumentExportId, InferenceAttemptId, MessageId, ToolInvocationId,
};
use koru_ai_wire::{
    AiSearchRequest, AiSearchResponse, AttemptResponse, BranchChatRequest, BranchChatResponse,
    CancelAttemptResponse, CancelModelPullResponse, ChatToolPolicyResponse, ChatTranscript,
    ClearHistoryResponse, ClearUsageResponse, CreateChatRequest, CreateChatResponse,
    DeleteChatResponse, ExportChatRequest, ExportChatResponse, FeedbackRequest, FeedbackResponse,
    ListChatsToolRequest, ListChatsToolResponse, OllamaSettingsView, OpenAiSettingsView,
    ProviderSettingsResponse, ProviderValidationResponse, PullOllamaModelRequest,
    ReadChatToolRequest, ReadChatToolResponse, RenameChatRequest, RenameChatResponse,
    SearchChatsToolRequest, SearchChatsToolResponse, SelectProviderRequest, SendTurnRequest,
    ToolDecisionResponse, UpdateChatModelRequest, UpdateChatModelResponse,
    UpdateChatPrivacyRequest, UpdateChatPrivacyResponse, UpdateChatToolPolicyRequest,
    UpdateOllamaSettingsRequest, UpdateOpenAiSettingsRequest, UsageResponse,
};

#[derive(Clone)]
pub struct Client {
    server: Arc<dyn AiServerPort>,
}

impl Client {
    pub fn new(server: Arc<dyn AiServerPort>) -> Self {
        Self { server }
    }

    pub fn provider_settings(&self, context: RequestContext) -> Result<ProviderSettingsResponse> {
        self.server.provider_settings(context)
    }

    pub fn select_provider(
        &self,
        context: RequestContext,
        request: SelectProviderRequest,
    ) -> Result<ProviderSettingsResponse> {
        self.server.select_provider(context, request)
    }

    pub fn update_openai_settings(
        &self,
        context: RequestContext,
        request: UpdateOpenAiSettingsRequest,
    ) -> Result<OpenAiSettingsView> {
        self.server.update_openai_settings(context, request)
    }

    pub fn update_ollama_settings(
        &self,
        context: RequestContext,
        request: UpdateOllamaSettingsRequest,
    ) -> Result<OllamaSettingsView> {
        self.server.update_ollama_settings(context, request)
    }

    pub fn validate_provider(
        &self,
        context: RequestContext,
        provider: AiProviderId,
    ) -> Result<ProviderValidationResponse> {
        self.server.validate_provider(context, provider)
    }

    pub fn discover_agent_models(
        &self,
        context: RequestContext,
        provider: AiProviderId,
        request: koru_ai_wire::DiscoverAgentModelsRequest,
    ) -> Result<koru_ai_wire::AgentModelsResponse> {
        self.server
            .discover_agent_models(context, provider, request)
    }

    pub fn discover_agent_commands(
        &self,
        context: RequestContext,
        provider: AiProviderId,
        request: koru_ai_wire::DiscoverAgentCommandsRequest,
    ) -> Result<koru_ai_wire::AgentCommandsResponse> {
        self.server
            .discover_agent_commands(context, provider, request)
    }

    pub fn pull_ollama_model(
        &self,
        context: RequestContext,
        request: PullOllamaModelRequest,
    ) -> Result<ModelPullResponseStream> {
        self.server.pull_ollama_model(context, request)
    }

    pub fn cancel_ollama_pull(&self, context: RequestContext) -> Result<CancelModelPullResponse> {
        self.server.cancel_ollama_pull(context)
    }

    pub fn create_chat(
        &self,
        context: RequestContext,
        request: CreateChatRequest,
    ) -> Result<CreateChatResponse> {
        self.server.create_chat(context, request)
    }

    pub fn list_chats(
        &self,
        context: RequestContext,
        profile: Option<koru_ai_core::ProfileId>,
    ) -> Result<koru_ai_wire::ListChatsResponse> {
        self.server.list_chats(context, profile)
    }

    pub fn read_chat(&self, context: RequestContext, chat: ChatId) -> Result<ChatTranscript> {
        self.server.read_chat(context, chat)
    }

    pub fn rename_chat(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: RenameChatRequest,
    ) -> Result<RenameChatResponse> {
        self.server.rename_chat(context, chat, request)
    }

    pub fn pin_chat(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::PinChatRequest,
    ) -> Result<koru_ai_wire::PinChatResponse> {
        self.server.pin_chat(context, chat, request)
    }

    pub fn update_chat_model(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: UpdateChatModelRequest,
    ) -> Result<UpdateChatModelResponse> {
        self.server.update_chat_model(context, chat, request)
    }

    pub fn update_chat_privacy(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: UpdateChatPrivacyRequest,
    ) -> Result<UpdateChatPrivacyResponse> {
        self.server.update_chat_privacy(context, chat, request)
    }

    pub fn update_chat_folder(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::UpdateChatFolderRequest,
    ) -> Result<koru_ai_wire::UpdateChatFolderResponse> {
        self.server.update_chat_folder(context, chat, request)
    }

    pub fn update_chat_agent_settings(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::UpdateChatAgentSettingsRequest,
    ) -> Result<koru_ai_wire::UpdateChatAgentSettingsResponse> {
        self.server
            .update_chat_agent_settings(context, chat, request)
    }

    pub fn tool_list_chats(
        &self,
        context: RequestContext,
        request: ListChatsToolRequest,
    ) -> Result<ListChatsToolResponse> {
        self.server.tool_list_chats(context, request)
    }

    pub fn tool_search_chats(
        &self,
        context: RequestContext,
        request: SearchChatsToolRequest,
    ) -> Result<SearchChatsToolResponse> {
        self.server.tool_search_chats(context, request)
    }

    pub fn tool_read_chat(
        &self,
        context: RequestContext,
        request: ReadChatToolRequest,
    ) -> Result<ReadChatToolResponse> {
        self.server.tool_read_chat(context, request)
    }

    pub fn update_chat_tool_policy(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: UpdateChatToolPolicyRequest,
    ) -> Result<ChatToolPolicyResponse> {
        self.server.update_chat_tool_policy(context, chat, request)
    }

    pub fn add_chat_workspace_grants(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::AddChatWorkspaceGrantsRequest,
    ) -> Result<ChatToolPolicyResponse> {
        self.server
            .add_chat_workspace_grants(context, chat, request)
    }

    pub fn read_chat_tool_policy(
        &self,
        context: RequestContext,
        chat: ChatId,
    ) -> Result<ChatToolPolicyResponse> {
        self.server.read_chat_tool_policy(context, chat)
    }

    pub fn usage(&self, context: RequestContext, chat: Option<ChatId>) -> Result<UsageResponse> {
        self.server.usage(context, chat)
    }

    pub fn delete_chat(&self, context: RequestContext, chat: ChatId) -> Result<DeleteChatResponse> {
        self.server.delete_chat(context, chat)
    }

    pub fn clear_history(&self, context: RequestContext) -> Result<ClearHistoryResponse> {
        self.server.clear_history(context)
    }

    pub fn clear_usage(&self, context: RequestContext) -> Result<ClearUsageResponse> {
        self.server.clear_usage(context)
    }

    pub fn search(
        &self,
        context: RequestContext,
        request: AiSearchRequest,
    ) -> Result<AiSearchResponse> {
        self.server.search(context, request)
    }

    pub fn feedback(
        &self,
        context: RequestContext,
        message: MessageId,
        request: FeedbackRequest,
    ) -> Result<FeedbackResponse> {
        self.server.feedback(context, message, request)
    }

    pub fn branch_chat(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: BranchChatRequest,
    ) -> Result<BranchChatResponse> {
        self.server.branch_chat(context, chat, request)
    }

    pub fn send_turn(
        &self,
        context: RequestContext,
        request: SendTurnRequest,
    ) -> Result<AiResponseStream> {
        self.server.send_turn(context, request)
    }

    pub fn read_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
    ) -> Result<AttemptResponse> {
        self.server.read_attempt(context, attempt)
    }

    pub fn cancel_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
    ) -> Result<CancelAttemptResponse> {
        self.server.cancel_attempt(context, attempt)
    }

    pub fn steer_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
        request: koru_ai_wire::SteerAttemptRequest,
    ) -> Result<koru_ai_wire::SteerAttemptResponse> {
        self.server.steer_attempt(context, attempt, request)
    }

    pub fn approve_tool_call(
        &self,
        context: RequestContext,
        tool: ToolInvocationId,
        decision: koru_ai_wire::ToolDecisionRequest,
    ) -> Result<ToolDecisionResponse> {
        self.server.approve_tool_call(context, tool, decision)
    }

    pub fn deny_tool_call(
        &self,
        context: RequestContext,
        tool: ToolInvocationId,
        choice: Option<String>,
    ) -> Result<ToolDecisionResponse> {
        self.server.deny_tool_call(context, tool, choice)
    }

    pub fn retry_turn(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::RetryTurnRequest,
    ) -> Result<AiResponseStream> {
        self.server.retry_turn(context, chat, request)
    }

    pub fn export_chat(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: ExportChatRequest,
    ) -> Result<ExportChatResponse> {
        self.server.export_chat(context, chat, request)
    }

    pub fn read_export(
        &self,
        context: RequestContext,
        export: DocumentExportId,
    ) -> Result<ExportChatResponse> {
        self.server.read_export(context, export)
    }

    pub fn put_attachment(
        &self,
        context: RequestContext,
        chat: ChatId,
        attachment: koru_ai_core::ChatAttachmentId,
        upload: koru_ai_api::AttachmentUpload,
    ) -> Result<koru_ai_wire::ChatAttachmentResponse> {
        self.server
            .put_attachment(context, chat, attachment, upload)
    }

    pub fn restage_attachment(
        &self,
        context: RequestContext,
        chat: ChatId,
        source: koru_ai_core::ChatAttachmentId,
        request: koru_ai_wire::RestageAttachmentRequest,
    ) -> Result<koru_ai_wire::ChatAttachmentResponse> {
        self.server
            .restage_attachment(context, chat, source, request)
    }

    pub fn reference_attachment(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::ReferenceAttachmentRequest,
    ) -> Result<koru_ai_wire::ChatAttachmentResponse> {
        self.server.reference_attachment(context, chat, request)
    }

    pub fn from_workspace_attachment(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::FromWorkspaceAttachmentRequest,
    ) -> Result<koru_ai_wire::ChatAttachmentResponse> {
        self.server
            .from_workspace_attachment(context, chat, request)
    }

    pub fn remove_attachment(
        &self,
        context: RequestContext,
        chat: ChatId,
        attachment: koru_ai_core::ChatAttachmentId,
    ) -> Result<koru_ai_wire::RemoveAttachmentResponse> {
        self.server.remove_attachment(context, chat, attachment)
    }

    pub fn read_attachment(
        &self,
        context: RequestContext,
        chat: ChatId,
        attachment: koru_ai_core::ChatAttachmentId,
    ) -> Result<koru_ai_api::AttachmentContent> {
        self.server.read_attachment(context, chat, attachment)
    }

    pub fn read_attachment_thumbnail(
        &self,
        context: RequestContext,
        chat: ChatId,
        attachment: koru_ai_core::ChatAttachmentId,
    ) -> Result<Vec<u8>> {
        self.server
            .read_attachment_thumbnail(context, chat, attachment)
    }
}
