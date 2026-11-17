// SPDX-License-Identifier: MPL-2.0

//! Owner-aware routing (M8, P8.3): one port in front of the local AI worker
//! that also answers for the Bots another of the person's computers runs.
//!
//! A window keeps talking to its local gateway. Each call that names a Bot,
//! a conversation, a reply, a pending call or a message is looked up:
//!
//! - **in a copy of another computer's Bots**: a read is answered from the
//!   copy, and a change is sent to the computer that runs the Bot, the only
//!   one that may apply it. A change that computer does not take from a
//!   linked computer is `executing_computer_only`; a read of something a
//!   copy does not hold is `not_found`;
//! - **anywhere else**: the local worker is called, unchanged. An id nobody
//!   has is therefore answered by the local worker, as before: `not_found`.
//!
//! The lookup is [`LinkedBots::holds`] and [`LinkedBots::takes`]. With no
//! copy on this computer both answer `false` without reading anything, so
//! the local path costs nothing more than it did.
//!
//! **A copy never runs anything.** Nothing here hands a copied Bot, a copied
//! conversation or a copied message to the local worker.

use std::sync::Arc;

use koru_ai_api::{
    AiResponseStream, AiServerPort, AttachmentContent, AttachmentUpload, ModelPullResponseStream,
    RequestContext, Result,
};
use koru_ai_core::bots::{BotRequestId, DeviceId, TeamPauseView};
use koru_ai_core::{
    AiProviderId, ChatAttachmentId, ChatId, DocumentExportId, InferenceAttemptId, MessageId,
    ToolInvocationId,
};
use koru_ai_wire::{
    AiSearchRequest, AiSearchResponse, AttemptResponse, BotCommand, BotResponse, BranchChatRequest,
    BranchChatResponse, CancelAttemptResponse, CancelModelPullResponse, ChatAttachmentResponse,
    ChatToolPolicyResponse, ChatTranscript, ClearHistoryResponse, ClearUsageResponse,
    CreateChatRequest, CreateChatResponse, DeleteChatResponse, ExportChatRequest,
    ExportChatResponse, Failure, FeedbackRequest, FeedbackResponse, ListChatsToolRequest,
    ListChatsToolResponse, OllamaSettingsView, OpenAiSettingsView, ProviderSettingsResponse,
    ProviderValidationResponse, PullOllamaModelRequest, ReadChatToolRequest, ReadChatToolResponse,
    RemoveAttachmentResponse, RenameChatRequest, RenameChatResponse, SearchChatsToolRequest,
    SearchChatsToolResponse, SelectProviderRequest, SendTurnRequest, SteerAttemptRequest,
    SteerAttemptResponse, ToolDecisionRequest, ToolDecisionResponse, UpdateChatModelRequest,
    UpdateChatModelResponse, UpdateChatPrivacyRequest, UpdateChatPrivacyResponse,
    UpdateChatToolPolicyRequest, UpdateOllamaSettingsRequest, UpdateOpenAiSettingsRequest,
    UsageResponse, bot_linked_codes,
};

/// What a call names, for the lookup that says whether a copy holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkedSubject {
    Chat(ChatId),
    /// A reply that is running, or was.
    Attempt(InferenceAttemptId),
    /// A pending call ("Needs you").
    Tool(ToolInvocationId),
    Message(MessageId),
}

/// The Bots other computers of this person run, as this computer keeps a
/// copy of them, and the way to those computers.
///
/// Every method may wait as long as a local call may. None of them runs
/// anything on this computer.
pub trait LinkedBots: Send + Sync {
    /// Whether `subject` belongs to a Bot another computer runs, as a copy
    /// on this computer shows it. `false` for anything this computer owns
    /// and for an id nobody has. With no copy at all it answers `false`
    /// without reading anything.
    fn holds(&self, subject: LinkedSubject) -> bool;

    /// Whether `command` is one this side answers: the two linked reads,
    /// and any command that names a Bot a copy holds.
    fn takes(&self, command: &BotCommand) -> bool;

    /// A command [`Self::takes`] answered `true` for: a read from the copy,
    /// or a change sent to the computer that runs the Bot.
    fn bots(&self, context: RequestContext, command: BotCommand) -> Result<BotResponse>;

    /// A copied conversation as its transcript.
    fn read_chat(&self, context: RequestContext, chat: ChatId) -> Result<ChatTranscript>;

    /// Send a message into a copied conversation: sent to the computer that
    /// runs the Bot. The stream carries the reply while that computer
    /// forwards it, and otherwise ends once the reply is in the copy.
    fn send_turn(
        &self,
        context: RequestContext,
        request: SendTurnRequest,
    ) -> Result<AiResponseStream>;

    fn retry_turn(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::RetryTurnRequest,
    ) -> Result<AiResponseStream>;

    fn cancel_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
    ) -> Result<CancelAttemptResponse>;

    fn steer_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
        request: SteerAttemptRequest,
    ) -> Result<SteerAttemptResponse>;

    /// Approve or deny a pending call of a copied conversation.
    fn decide_tool_call(
        &self,
        context: RequestContext,
        tool: ToolInvocationId,
        approve: bool,
        decision: ToolDecisionRequest,
    ) -> Result<ToolDecisionResponse>;

    /// Pause (`pause`) or resume every Bot of one linked computer. Sent
    /// only while that computer is connected; never kept for later.
    fn team_pause(
        &self,
        context: RequestContext,
        owner_device: DeviceId,
        pause: bool,
        request_id: BotRequestId,
        expected_team_revision: u64,
    ) -> Result<TeamPauseView>;
}

/// A change only the computer that runs the Bot may make.
pub fn executing_computer_only() -> Failure {
    Failure {
        code: bot_linked_codes::EXECUTING_COMPUTER_ONLY.into(),
        message: "This change is made on the computer that runs the Bot.".into(),
        retryable: false,
        details: Default::default(),
    }
}

/// A read of something a copy does not hold.
fn not_in_copy() -> Failure {
    Failure {
        code: "not_found".into(),
        message: "This is kept on the computer that runs the Bot.".into(),
        retryable: false,
        details: Default::default(),
    }
}

/// The local AI worker's port with the linked computers' Bots beside it.
pub struct RoutingPort {
    local: Arc<dyn AiServerPort>,
    linked: Arc<dyn LinkedBots>,
}

impl RoutingPort {
    pub fn new(local: Arc<dyn AiServerPort>, linked: Arc<dyn LinkedBots>) -> Self {
        Self { local, linked }
    }

    fn linked_chat(&self, chat: ChatId) -> bool {
        self.linked.holds(LinkedSubject::Chat(chat))
    }

    /// A change of a conversation that only its own computer makes.
    fn local_chat_change<T>(
        &self,
        chat: ChatId,
        local: impl FnOnce(&dyn AiServerPort) -> Result<T>,
    ) -> Result<T> {
        if self.linked_chat(chat) {
            return Err(executing_computer_only());
        }
        local(self.local.as_ref())
    }

    /// A read of something a copy does not hold.
    fn local_chat_read<T>(
        &self,
        chat: ChatId,
        local: impl FnOnce(&dyn AiServerPort) -> Result<T>,
    ) -> Result<T> {
        if self.linked_chat(chat) {
            return Err(not_in_copy());
        }
        local(self.local.as_ref())
    }
}

impl AiServerPort for RoutingPort {
    fn bots(&self, context: RequestContext, request: BotCommand) -> Result<BotResponse> {
        if self.linked.takes(&request) {
            self.linked.bots(context, request)
        } else {
            self.local.bots(context, request)
        }
    }

    fn provider_settings(&self, context: RequestContext) -> Result<ProviderSettingsResponse> {
        self.local.provider_settings(context)
    }

    fn select_provider(
        &self,
        context: RequestContext,
        request: SelectProviderRequest,
    ) -> Result<ProviderSettingsResponse> {
        self.local.select_provider(context, request)
    }

    fn update_openai_settings(
        &self,
        context: RequestContext,
        request: UpdateOpenAiSettingsRequest,
    ) -> Result<OpenAiSettingsView> {
        self.local.update_openai_settings(context, request)
    }

    fn update_ollama_settings(
        &self,
        context: RequestContext,
        request: UpdateOllamaSettingsRequest,
    ) -> Result<OllamaSettingsView> {
        self.local.update_ollama_settings(context, request)
    }

    fn validate_provider(
        &self,
        context: RequestContext,
        provider: AiProviderId,
    ) -> Result<ProviderValidationResponse> {
        self.local.validate_provider(context, provider)
    }

    fn discover_agent_models(
        &self,
        context: RequestContext,
        provider: AiProviderId,
        request: koru_ai_wire::DiscoverAgentModelsRequest,
    ) -> Result<koru_ai_wire::AgentModelsResponse> {
        self.local.discover_agent_models(context, provider, request)
    }

    fn discover_agent_commands(
        &self,
        context: RequestContext,
        provider: AiProviderId,
        request: koru_ai_wire::DiscoverAgentCommandsRequest,
    ) -> Result<koru_ai_wire::AgentCommandsResponse> {
        self.local
            .discover_agent_commands(context, provider, request)
    }

    fn pull_ollama_model(
        &self,
        context: RequestContext,
        request: PullOllamaModelRequest,
    ) -> Result<ModelPullResponseStream> {
        self.local.pull_ollama_model(context, request)
    }

    fn cancel_ollama_pull(&self, context: RequestContext) -> Result<CancelModelPullResponse> {
        self.local.cancel_ollama_pull(context)
    }

    fn create_chat(
        &self,
        context: RequestContext,
        request: CreateChatRequest,
    ) -> Result<CreateChatResponse> {
        self.local.create_chat(context, request)
    }

    fn list_chats(
        &self,
        context: RequestContext,
        profile: Option<koru_ai_core::ProfileId>,
    ) -> Result<koru_ai_wire::ListChatsResponse> {
        self.local.list_chats(context, profile)
    }

    fn read_chat(&self, context: RequestContext, chat: ChatId) -> Result<ChatTranscript> {
        if self.linked_chat(chat) {
            self.linked.read_chat(context, chat)
        } else {
            self.local.read_chat(context, chat)
        }
    }

    fn rename_chat(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: RenameChatRequest,
    ) -> Result<RenameChatResponse> {
        self.local_chat_change(chat, |local| local.rename_chat(context, chat, request))
    }

    fn pin_chat(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::PinChatRequest,
    ) -> Result<koru_ai_wire::PinChatResponse> {
        self.local_chat_change(chat, |local| local.pin_chat(context, chat, request))
    }

    fn update_chat_model(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: UpdateChatModelRequest,
    ) -> Result<UpdateChatModelResponse> {
        self.local_chat_change(chat, |local| {
            local.update_chat_model(context, chat, request)
        })
    }

    fn update_chat_privacy(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: UpdateChatPrivacyRequest,
    ) -> Result<UpdateChatPrivacyResponse> {
        self.local_chat_change(chat, |local| {
            local.update_chat_privacy(context, chat, request)
        })
    }

    fn update_chat_folder(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::UpdateChatFolderRequest,
    ) -> Result<koru_ai_wire::UpdateChatFolderResponse> {
        self.local_chat_change(chat, |local| {
            local.update_chat_folder(context, chat, request)
        })
    }

    fn update_chat_agent_settings(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::UpdateChatAgentSettingsRequest,
    ) -> Result<koru_ai_wire::UpdateChatAgentSettingsResponse> {
        self.local_chat_change(chat, |local| {
            local.update_chat_agent_settings(context, chat, request)
        })
    }

    // The three chat tools are the model's own reads of this computer's
    // chats. A copy is not searchable and is never offered to a model.
    fn tool_list_chats(
        &self,
        context: RequestContext,
        request: ListChatsToolRequest,
    ) -> Result<ListChatsToolResponse> {
        self.local.tool_list_chats(context, request)
    }

    fn tool_search_chats(
        &self,
        context: RequestContext,
        request: SearchChatsToolRequest,
    ) -> Result<SearchChatsToolResponse> {
        self.local.tool_search_chats(context, request)
    }

    fn tool_read_chat(
        &self,
        context: RequestContext,
        request: ReadChatToolRequest,
    ) -> Result<ReadChatToolResponse> {
        self.local.tool_read_chat(context, request)
    }

    fn read_chat_tool_policy(
        &self,
        context: RequestContext,
        chat: ChatId,
    ) -> Result<ChatToolPolicyResponse> {
        self.local_chat_read(chat, |local| local.read_chat_tool_policy(context, chat))
    }

    fn update_chat_tool_policy(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: UpdateChatToolPolicyRequest,
    ) -> Result<ChatToolPolicyResponse> {
        self.local_chat_change(chat, |local| {
            local.update_chat_tool_policy(context, chat, request)
        })
    }

    fn add_chat_workspace_grants(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::AddChatWorkspaceGrantsRequest,
    ) -> Result<ChatToolPolicyResponse> {
        self.local_chat_change(chat, |local| {
            local.add_chat_workspace_grants(context, chat, request)
        })
    }

    fn usage(&self, context: RequestContext, chat: Option<ChatId>) -> Result<UsageResponse> {
        match chat {
            Some(chat) => self.local_chat_read(chat, |local| local.usage(context, Some(chat))),
            None => self.local.usage(context, None),
        }
    }

    fn delete_chat(&self, context: RequestContext, chat: ChatId) -> Result<DeleteChatResponse> {
        self.local_chat_change(chat, |local| local.delete_chat(context, chat))
    }

    fn clear_history(&self, context: RequestContext) -> Result<ClearHistoryResponse> {
        self.local.clear_history(context)
    }

    fn clear_usage(&self, context: RequestContext) -> Result<ClearUsageResponse> {
        self.local.clear_usage(context)
    }

    fn search(
        &self,
        context: RequestContext,
        request: AiSearchRequest,
    ) -> Result<AiSearchResponse> {
        self.local.search(context, request)
    }

    fn feedback(
        &self,
        context: RequestContext,
        message: MessageId,
        request: FeedbackRequest,
    ) -> Result<FeedbackResponse> {
        if self.linked.holds(LinkedSubject::Message(message)) {
            return Err(executing_computer_only());
        }
        self.local.feedback(context, message, request)
    }

    fn branch_chat(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: BranchChatRequest,
    ) -> Result<BranchChatResponse> {
        self.local_chat_change(chat, |local| local.branch_chat(context, chat, request))
    }

    fn send_turn(
        &self,
        context: RequestContext,
        request: SendTurnRequest,
    ) -> Result<AiResponseStream> {
        if self.linked_chat(request.chat_id) {
            self.linked.send_turn(context, request)
        } else {
            self.local.send_turn(context, request)
        }
    }

    fn read_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
    ) -> Result<AttemptResponse> {
        if self.linked.holds(LinkedSubject::Attempt(attempt)) {
            return Err(not_in_copy());
        }
        self.local.read_attempt(context, attempt)
    }

    fn cancel_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
    ) -> Result<CancelAttemptResponse> {
        if self.linked.holds(LinkedSubject::Attempt(attempt)) {
            self.linked.cancel_attempt(context, attempt)
        } else {
            self.local.cancel_attempt(context, attempt)
        }
    }

    fn steer_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
        request: SteerAttemptRequest,
    ) -> Result<SteerAttemptResponse> {
        if self.linked.holds(LinkedSubject::Attempt(attempt)) {
            self.linked.steer_attempt(context, attempt, request)
        } else {
            self.local.steer_attempt(context, attempt, request)
        }
    }

    fn approve_tool_call(
        &self,
        context: RequestContext,
        tool: ToolInvocationId,
        decision: ToolDecisionRequest,
    ) -> Result<ToolDecisionResponse> {
        if self.linked.holds(LinkedSubject::Tool(tool)) {
            self.linked.decide_tool_call(context, tool, true, decision)
        } else {
            self.local.approve_tool_call(context, tool, decision)
        }
    }

    fn deny_tool_call(
        &self,
        context: RequestContext,
        tool: ToolInvocationId,
        choice: Option<String>,
    ) -> Result<ToolDecisionResponse> {
        if self.linked.holds(LinkedSubject::Tool(tool)) {
            self.linked.decide_tool_call(
                context,
                tool,
                false,
                ToolDecisionRequest {
                    choice,
                    ..ToolDecisionRequest::default()
                },
            )
        } else {
            self.local.deny_tool_call(context, tool, choice)
        }
    }

    fn retry_turn(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::RetryTurnRequest,
    ) -> Result<AiResponseStream> {
        if self.linked_chat(chat) {
            self.linked.retry_turn(context, chat, request)
        } else {
            self.local.retry_turn(context, chat, request)
        }
    }

    fn export_chat(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: ExportChatRequest,
    ) -> Result<ExportChatResponse> {
        self.local_chat_change(chat, |local| local.export_chat(context, chat, request))
    }

    fn read_export(
        &self,
        context: RequestContext,
        export: DocumentExportId,
    ) -> Result<ExportChatResponse> {
        self.local.read_export(context, export)
    }

    fn put_attachment(
        &self,
        context: RequestContext,
        chat: ChatId,
        attachment: ChatAttachmentId,
        upload: AttachmentUpload,
    ) -> Result<ChatAttachmentResponse> {
        self.local_chat_change(chat, |local| {
            local.put_attachment(context, chat, attachment, upload)
        })
    }

    fn reference_attachment(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::ReferenceAttachmentRequest,
    ) -> Result<ChatAttachmentResponse> {
        self.local_chat_change(chat, |local| {
            local.reference_attachment(context, chat, request)
        })
    }

    fn from_workspace_attachment(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::FromWorkspaceAttachmentRequest,
    ) -> Result<ChatAttachmentResponse> {
        self.local_chat_change(chat, |local| {
            local.from_workspace_attachment(context, chat, request)
        })
    }

    fn restage_attachment(
        &self,
        context: RequestContext,
        chat: ChatId,
        source: ChatAttachmentId,
        request: koru_ai_wire::RestageAttachmentRequest,
    ) -> Result<ChatAttachmentResponse> {
        self.local_chat_change(chat, |local| {
            local.restage_attachment(context, chat, source, request)
        })
    }

    fn remove_attachment(
        &self,
        context: RequestContext,
        chat: ChatId,
        attachment: ChatAttachmentId,
    ) -> Result<RemoveAttachmentResponse> {
        self.local_chat_change(chat, |local| {
            local.remove_attachment(context, chat, attachment)
        })
    }

    fn read_attachment(
        &self,
        context: RequestContext,
        chat: ChatId,
        attachment: ChatAttachmentId,
    ) -> Result<AttachmentContent> {
        self.local_chat_read(chat, |local| {
            local.read_attachment(context, chat, attachment)
        })
    }

    fn read_attachment_thumbnail(
        &self,
        context: RequestContext,
        chat: ChatId,
        attachment: ChatAttachmentId,
    ) -> Result<Vec<u8>> {
        self.local_chat_read(chat, |local| {
            local.read_attachment_thumbnail(context, chat, attachment)
        })
    }
}
