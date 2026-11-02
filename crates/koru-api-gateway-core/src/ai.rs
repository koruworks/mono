// SPDX-License-Identifier: MPL-2.0

//! Authenticated public AI routes and the bounded SSE transport adapter.

use std::convert::Infallible;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, Extension, Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get, patch, post};
use axum::{Json, Router};
use bytes::Bytes;
use futures_util::stream;
use koru_ai_api::{AiPrincipal, AiScope, RequestContext};
use koru_ai_core::{
    AiProviderId, AttachmentSource, ChatAttachmentId, ChatId, DocumentExportId, InferenceAttemptId,
    MessageId, ToolInvocationId,
};
use koru_ai_wire::{
    AiSearchRequest, AiSearchResponse, AttemptResponse, BranchChatRequest, BranchChatResponse,
    CancelAttemptResponse, CancelModelPullResponse, ChatAttachmentResponse, ChatToolPolicyResponse,
    ChatTranscript, ClearHistoryResponse, ClearUsageResponse, CreateChatRequest,
    CreateChatResponse, DeleteChatResponse, ExportChatRequest, ExportChatResponse, Failure,
    FeedbackRequest, FeedbackResponse, ListChatsResponse, ListChatsToolRequest,
    ListChatsToolResponse, ModelPullEvent, OllamaSettingsView, OpenAiSettingsView,
    ProviderSettingsResponse, ProviderValidationResponse, PullOllamaModelAccepted,
    PullOllamaModelRequest, ReadChatToolRequest, ReadChatToolResponse, RemoveAttachmentResponse,
    RenameChatRequest, RenameChatResponse, RetryTurnRequest, SearchChatsToolRequest,
    SearchChatsToolResponse, SelectProviderRequest, SendTurnAccepted, SendTurnRequest, StreamEvent,
    ToolDecisionRequest, ToolDecisionResponse, UpdateChatModelRequest, UpdateChatModelResponse,
    UpdateChatPrivacyRequest, UpdateChatPrivacyResponse, UpdateChatToolPolicyRequest,
    UpdateOllamaSettingsRequest, UpdateOpenAiSettingsRequest, UsageResponse,
};
use schemars::{Schema, SchemaGenerator};

use crate::{GatewayPrincipal, GatewayState};

type SchemaFor = fn(&mut SchemaGenerator) -> Schema;
pub const SSE_CAPACITY: usize = 32;
/// How long an AI stream may stay silent before the gateway sends an SSE
/// comment. An agent thinking, waiting on a permission or running a long
/// command can say nothing for minutes; without traffic the window's read
/// limit ([`koru_api_client`]'s stream client) took the quiet for a dropped
/// connection and showed *Connection interrupted*. Clients skip comment
/// lines, so a build that predates this reads the stream unchanged.
const SSE_KEEP_ALIVE: std::time::Duration = std::time::Duration::from_secs(15);
const SSE_KEEP_ALIVE_FRAME: &[u8] = b": keep-alive\n\n";

pub struct AiRoute {
    pub operation_id: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub scope: AiScope,
    pub summary: &'static str,
    pub returns: &'static str,
    pub handler: fn() -> MethodRouter<Arc<GatewayState>>,
    pub request: AiBody,
    pub response: AiBody,
    pub stream: bool,
    pub path_parameters: &'static [&'static str],
}

/// What a route's request or response body is: none, a JSON value with its
/// schema, or raw bytes (an attachment's content) of the named media type.
#[derive(Clone, Copy)]
pub enum AiBody {
    None,
    Json(SchemaFor),
    Bytes(&'static str),
}

/// The most an attachment upload may carry, with room for nothing else: the
/// body is the file. The service checks the exact limit again.
const ATTACHMENT_BODY_LIMIT: usize =
    koru_ai_core::attachments::MAX_ATTACHMENT_BYTES as usize + 64 * 1024;

pub const ROUTES: &[AiRoute] = &[
    AiRoute {
        operation_id: "ai_provider_settings",
        method: "GET",
        path: "/v1/ai/providers/settings",
        scope: AiScope::ManageProviders,
        summary: "Read non-secret AI provider settings",
        returns: "The selected provider, model, reasoning, endpoint, and credential-presence flag. Secrets and vault labels are never returned.",
        handler: || get(provider_settings),
        request: AiBody::None,
        response: AiBody::Json(|generator| generator.subschema_for::<ProviderSettingsResponse>()),
        stream: false,
        path_parameters: &[],
    },
    AiRoute {
        operation_id: "ai_select_provider",
        method: "PUT",
        path: "/v1/ai/providers/selection",
        scope: AiScope::ManageProviders,
        summary: "Select the active AI provider",
        returns: "The complete persisted non-secret provider settings.",
        handler: || axum::routing::put(select_provider),
        request: AiBody::Json(|generator| generator.subschema_for::<SelectProviderRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<ProviderSettingsResponse>()),
        stream: false,
        path_parameters: &[],
    },
    AiRoute {
        operation_id: "ai_update_openai_settings",
        method: "PUT",
        path: "/v1/ai/providers/openai",
        scope: AiScope::ManageProviders,
        summary: "Update non-secret OpenAI settings",
        returns: "The saved OpenAI model, reasoning choice, and credential-presence flag.",
        handler: || axum::routing::put(update_openai_settings),
        request: AiBody::Json(|generator| generator.subschema_for::<UpdateOpenAiSettingsRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<OpenAiSettingsView>()),
        stream: false,
        path_parameters: &[],
    },
    AiRoute {
        operation_id: "ai_update_ollama_settings",
        method: "PUT",
        path: "/v1/ai/providers/ollama",
        scope: AiScope::ManageProviders,
        summary: "Update Ollama settings",
        returns: "The saved Ollama endpoint, model, and reasoning choice.",
        handler: || axum::routing::put(update_ollama_settings),
        request: AiBody::Json(|generator| generator.subschema_for::<UpdateOllamaSettingsRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<OllamaSettingsView>()),
        stream: false,
        path_parameters: &[],
    },
    AiRoute {
        operation_id: "ai_validate_provider",
        method: "POST",
        path: "/v1/ai/providers/{provider}/validate",
        scope: AiScope::ManageProviders,
        summary: "Validate an AI provider and selected model",
        returns: "A safe provider state, available model names, and selected-model capabilities when ready.",
        handler: || post(validate_provider),
        request: AiBody::None,
        response: AiBody::Json(|generator| generator.subschema_for::<ProviderValidationResponse>()),
        stream: false,
        path_parameters: &["provider"],
    },
    AiRoute {
        operation_id: "ai_discover_agent_models",
        method: "POST",
        path: "/v1/ai/providers/{provider}/models",
        scope: AiScope::ManageProviders,
        summary: "List the models a local agent offers",
        returns: "The agent's models in its own ids, each with the agent's grouping; empty when it lists none. With the optional body `{ \"refresh\": true }` the list is rebuilt first.",
        handler: || post(discover_agent_models),
        request: AiBody::Json(|generator| {
            generator.subschema_for::<koru_ai_wire::DiscoverAgentModelsRequest>()
        }),
        response: AiBody::Json(|generator| {
            generator.subschema_for::<koru_ai_wire::AgentModelsResponse>()
        }),
        stream: false,
        path_parameters: &["provider"],
    },
    AiRoute {
        operation_id: "ai_pull_ollama_model",
        method: "POST",
        path: "/v1/ai/providers/ollama/pulls",
        scope: AiScope::ManageProviders,
        summary: "Pull an Ollama model with bounded progress streaming",
        returns: "An SSE stream beginning with the accepted model followed by normalized progress and one terminal event.",
        handler: || post(pull_ollama_model),
        request: AiBody::Json(|generator| generator.subschema_for::<PullOllamaModelRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<PullOllamaModelAccepted>()),
        stream: true,
        path_parameters: &[],
    },
    AiRoute {
        operation_id: "ai_cancel_ollama_pull",
        method: "POST",
        path: "/v1/ai/providers/ollama/pulls/cancel",
        scope: AiScope::ManageProviders,
        summary: "Cancel the active Ollama model pull",
        returns: "Whether an active pull was signalled for cancellation.",
        handler: || post(cancel_ollama_pull),
        request: AiBody::None,
        response: AiBody::Json(|generator| generator.subschema_for::<CancelModelPullResponse>()),
        stream: false,
        path_parameters: &[],
    },
    AiRoute {
        operation_id: "ai_create_chat",
        method: "POST",
        path: "/v1/ai/chats",
        scope: AiScope::Chat,
        summary: "Create an AI chat",
        returns: "The durable chat created with the client-minted identity.",
        handler: || post(create_chat),
        request: AiBody::Json(|generator| generator.subschema_for::<CreateChatRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<CreateChatResponse>()),
        stream: false,
        path_parameters: &[],
    },
    AiRoute {
        operation_id: "ai_list_chats",
        method: "GET",
        path: "/v1/ai/chats",
        scope: AiScope::Chat,
        summary: "List AI chats",
        returns: "Durable chats ordered by most recent activity.",
        handler: || get(list_chats),
        request: AiBody::None,
        response: AiBody::Json(|generator| generator.subschema_for::<ListChatsResponse>()),
        stream: false,
        path_parameters: &[],
    },
    AiRoute {
        operation_id: "ai_read_chat",
        method: "GET",
        path: "/v1/ai/chats/{chat}",
        scope: AiScope::Chat,
        summary: "Read an authoritative AI chat transcript",
        returns: "The chat, ordered messages, finalized blocks, and pinned document references.",
        handler: || get(read_chat),
        request: AiBody::None,
        response: AiBody::Json(|generator| generator.subschema_for::<ChatTranscript>()),
        stream: false,
        path_parameters: &["chat"],
    },
    AiRoute {
        operation_id: "ai_rename_chat",
        method: "PATCH",
        path: "/v1/ai/chats/{chat}",
        scope: AiScope::Chat,
        summary: "Rename an AI chat",
        returns: "The durable chat with its updated title and activity timestamp.",
        handler: || patch(rename_chat),
        request: AiBody::Json(|generator| generator.subschema_for::<RenameChatRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<RenameChatResponse>()),
        stream: false,
        path_parameters: &["chat"],
    },
    AiRoute {
        operation_id: "ai_update_chat_model",
        method: "PATCH",
        path: "/v1/ai/chats/{chat}/model",
        scope: AiScope::Chat,
        summary: "Change the model used by an AI chat",
        returns: "The durable chat with its updated provider, model, reasoning default, and activity timestamp.",
        handler: || patch(update_chat_model),
        request: AiBody::Json(|generator| generator.subschema_for::<UpdateChatModelRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<UpdateChatModelResponse>()),
        stream: false,
        path_parameters: &["chat"],
    },
    AiRoute {
        operation_id: "ai_update_chat_privacy",
        method: "PATCH",
        path: "/v1/ai/chats/{chat}/privacy",
        scope: AiScope::Chat,
        summary: "Change whether agents and connected assistants can discover a chat",
        returns: "The durable chat with its updated privacy flag and activity timestamp.",
        handler: || patch(update_chat_privacy),
        request: AiBody::Json(|generator| generator.subschema_for::<UpdateChatPrivacyRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<UpdateChatPrivacyResponse>()),
        stream: false,
        path_parameters: &["chat"],
    },
    AiRoute {
        operation_id: "ai_update_chat_folder",
        method: "PATCH",
        path: "/v1/ai/chats/{chat}/folder",
        scope: AiScope::Chat,
        summary: "Choose the folder a chat's local agent works in",
        returns: "The durable chat with its working folder; accepted only on an agent chat with no message yet.",
        handler: || patch(update_chat_folder),
        request: AiBody::Json(|generator| {
            generator.subschema_for::<koru_ai_wire::UpdateChatFolderRequest>()
        }),
        response: AiBody::Json(|generator| {
            generator.subschema_for::<koru_ai_wire::UpdateChatFolderResponse>()
        }),
        stream: false,
        path_parameters: &["chat"],
    },
    AiRoute {
        operation_id: "ai_update_chat_agent_settings",
        method: "PATCH",
        path: "/v1/ai/chats/{chat}/agent-settings",
        scope: AiScope::Chat,
        summary: "Choose the approval mode a chat's local agent runs its next message in",
        returns: "The durable chat with its agent settings; accepted on an agent chat at any time, a running reply keeping the mode it was sent in.",
        handler: || patch(update_chat_agent_settings),
        request: AiBody::Json(|generator| {
            generator.subschema_for::<koru_ai_wire::UpdateChatAgentSettingsRequest>()
        }),
        response: AiBody::Json(|generator| {
            generator.subschema_for::<koru_ai_wire::UpdateChatAgentSettingsResponse>()
        }),
        stream: false,
        path_parameters: &["chat"],
    },
    AiRoute {
        operation_id: "ai_tool_list_chats",
        method: "POST",
        path: "/v1/ai/tools/chats/list",
        scope: AiScope::Chat,
        summary: "List non-private AI chats for a trusted tool caller",
        returns: "A bounded newest-first page of non-private chat summaries.",
        handler: || post(tool_list_chats),
        request: AiBody::Json(|generator| generator.subschema_for::<ListChatsToolRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<ListChatsToolResponse>()),
        stream: false,
        path_parameters: &[],
    },
    AiRoute {
        operation_id: "ai_tool_search_chats",
        method: "POST",
        path: "/v1/ai/tools/chats/search",
        scope: AiScope::Chat,
        summary: "Search non-private AI chats for a trusted tool caller",
        returns: "A bounded relevance-ranked page of finalized response matches from non-private chats.",
        handler: || post(tool_search_chats),
        request: AiBody::Json(|generator| generator.subschema_for::<SearchChatsToolRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<SearchChatsToolResponse>()),
        stream: false,
        path_parameters: &[],
    },
    AiRoute {
        operation_id: "ai_tool_read_chat",
        method: "POST",
        path: "/v1/ai/tools/chats/read",
        scope: AiScope::Chat,
        summary: "Read a non-private AI chat for a trusted tool caller",
        returns: "A byte-bounded recent page of terminal chat messages; private and missing chats share chat_not_found.",
        handler: || post(tool_read_chat),
        request: AiBody::Json(|generator| generator.subschema_for::<ReadChatToolRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<ReadChatToolResponse>()),
        stream: false,
        path_parameters: &[],
    },
    AiRoute {
        operation_id: "ai_update_chat_tool_policy",
        method: "PUT",
        path: "/v1/ai/chats/{chat}/tool-policy",
        scope: AiScope::Chat,
        summary: "Replace a chat's workspace-tool policy",
        returns: "The durable Manual or Auto mode and complete selected-workspace grant set. An empty set grants no workspace access.",
        handler: || axum::routing::put(update_chat_tool_policy),
        request: AiBody::Json(|generator| generator.subschema_for::<UpdateChatToolPolicyRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<ChatToolPolicyResponse>()),
        stream: false,
        path_parameters: &["chat"],
    },
    AiRoute {
        operation_id: "ai_read_chat_tool_policy",
        method: "GET",
        path: "/v1/ai/chats/{chat}/tool-policy",
        scope: AiScope::Chat,
        summary: "Read a chat's workspace-tool policy",
        returns: "The durable Manual or Auto mode and complete selected-workspace grant set.",
        handler: || get(read_chat_tool_policy),
        request: AiBody::None,
        response: AiBody::Json(|generator| generator.subschema_for::<ChatToolPolicyResponse>()),
        stream: false,
        path_parameters: &["chat"],
    },
    AiRoute {
        operation_id: "ai_delete_chat",
        method: "DELETE",
        path: "/v1/ai/chats/{chat}",
        scope: AiScope::Chat,
        summary: "Delete an AI chat while retaining anonymous usage",
        returns: "Whether the chat existed. Content, search rows, and chat/message identifiers are removed while text-free attempt and feedback metrics survive.",
        handler: || axum::routing::delete(delete_chat),
        request: AiBody::None,
        response: AiBody::Json(|generator| generator.subschema_for::<DeleteChatResponse>()),
        stream: false,
        path_parameters: &["chat"],
    },
    AiRoute {
        operation_id: "ai_clear_history",
        method: "DELETE",
        path: "/v1/ai/chats",
        scope: AiScope::Chat,
        summary: "Clear all AI chat history",
        returns: "The number of chats deleted. Anonymous text-free usage and feedback aggregates remain until cleared separately.",
        handler: || axum::routing::delete(clear_history),
        request: AiBody::None,
        response: AiBody::Json(|generator| generator.subschema_for::<ClearHistoryResponse>()),
        stream: false,
        path_parameters: &[],
    },
    AiRoute {
        operation_id: "ai_usage",
        method: "GET",
        path: "/v1/ai/usage",
        scope: AiScope::ReadUsage,
        summary: "Read private local AI usage",
        returns: "Content-free local aggregates grouped by day, surface, provider, model, effort, status, and prompt version. Missing provider usage remains unavailable.",
        handler: || get(usage),
        request: AiBody::None,
        response: AiBody::Json(|generator| generator.subschema_for::<UsageResponse>()),
        stream: false,
        path_parameters: &[],
    },
    AiRoute {
        operation_id: "ai_chat_usage",
        method: "GET",
        path: "/v1/ai/chats/{chat}/usage",
        scope: AiScope::ReadUsage,
        summary: "Read current-chat and all private local AI usage",
        returns: "Current-chat totals followed by the same content-free all-usage aggregates as the local usage report.",
        handler: || get(chat_usage),
        request: AiBody::None,
        response: AiBody::Json(|generator| generator.subschema_for::<UsageResponse>()),
        stream: false,
        path_parameters: &["chat"],
    },
    AiRoute {
        operation_id: "ai_clear_usage",
        method: "DELETE",
        path: "/v1/ai/usage",
        scope: AiScope::ReadUsage,
        summary: "Clear private local AI usage and feedback",
        returns: "The number of text-free attempts and feedback events deleted. Chat history is unchanged.",
        handler: || axum::routing::delete(clear_usage),
        request: AiBody::None,
        response: AiBody::Json(|generator| generator.subschema_for::<ClearUsageResponse>()),
        stream: false,
        path_parameters: &[],
    },
    AiRoute {
        operation_id: "ai_search",
        method: "POST",
        path: "/v1/ai/search",
        scope: AiScope::Chat,
        summary: "Search finalized AI chat responses",
        returns: "Best matching finalized block per response with exact chat, message, and block identity.",
        handler: || post(search),
        request: AiBody::Json(|generator| generator.subschema_for::<AiSearchRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<AiSearchResponse>()),
        stream: false,
        path_parameters: &[],
    },
    AiRoute {
        operation_id: "ai_rate_response",
        method: "PUT",
        path: "/v1/ai/messages/{message}/feedback",
        scope: AiScope::Chat,
        summary: "Append response feedback",
        returns: "The retry-safe append-only feedback event.",
        handler: || axum::routing::put(rate_response),
        request: AiBody::Json(|generator| generator.subschema_for::<FeedbackRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<FeedbackResponse>()),
        stream: false,
        path_parameters: &["message"],
    },
    AiRoute {
        operation_id: "ai_branch_chat",
        method: "POST",
        path: "/v1/ai/chats/{chat}/branches",
        scope: AiScope::Chat,
        summary: "Branch an AI chat",
        returns: "A durable chat containing newly identified copies through the chosen response.",
        handler: || post(branch_chat),
        request: AiBody::Json(|generator| generator.subschema_for::<BranchChatRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<BranchChatResponse>()),
        stream: false,
        path_parameters: &["chat"],
    },
    AiRoute {
        operation_id: "ai_retry_turn",
        method: "POST",
        path: "/v1/ai/chats/{chat}/retries",
        scope: AiScope::Chat,
        summary: "Retry a failed AI response",
        returns: "A durable fresh assistant entry and attempt, streamed without replacing failed history.",
        handler: || post(retry_turn),
        request: AiBody::Json(|generator| generator.subschema_for::<RetryTurnRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<SendTurnAccepted>()),
        stream: true,
        path_parameters: &["chat"],
    },
    AiRoute {
        operation_id: "ai_export_chat",
        method: "POST",
        path: "/v1/ai/chats/{chat}/exports",
        scope: AiScope::Chat,
        summary: "Store a chat prefix as a Koru document",
        returns: "The durable retry-safe export operation; poll it until terminal.",
        handler: || post(export_chat),
        request: AiBody::Json(|generator| generator.subschema_for::<ExportChatRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<ExportChatResponse>()),
        stream: false,
        path_parameters: &["chat"],
    },
    AiRoute {
        operation_id: "ai_read_export",
        method: "GET",
        path: "/v1/ai/exports/{export}",
        scope: AiScope::Chat,
        summary: "Read an AI document export",
        returns: "The authoritative pending, complete, or failed export operation.",
        handler: || get(read_export),
        request: AiBody::None,
        response: AiBody::Json(|generator| generator.subschema_for::<ExportChatResponse>()),
        stream: false,
        path_parameters: &["export"],
    },
    AiRoute {
        operation_id: "ai_send_turn",
        method: "POST",
        path: "/v1/ai/chats/{chat}/turns",
        scope: AiScope::Chat,
        summary: "Commit and stream an AI turn",
        returns: "An ordered SSE stream beginning with durable acceptance, followed by normalized Koru AI events. Recover a lost stream by reading the attempt and chat.",
        handler: || post(send_turn),
        request: AiBody::Json(|generator| generator.subschema_for::<SendTurnRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<SendTurnAccepted>()),
        stream: true,
        path_parameters: &["chat"],
    },
    AiRoute {
        operation_id: "ai_read_attempt",
        method: "GET",
        path: "/v1/ai/attempts/{attempt}",
        scope: AiScope::Chat,
        summary: "Read authoritative AI attempt state",
        returns: "The durable attempt state used to recover after stream loss.",
        handler: || get(read_attempt),
        request: AiBody::None,
        response: AiBody::Json(|generator| generator.subschema_for::<AttemptResponse>()),
        stream: false,
        path_parameters: &["attempt"],
    },
    AiRoute {
        operation_id: "ai_cancel_attempt",
        method: "POST",
        path: "/v1/ai/attempts/{attempt}/cancel",
        scope: AiScope::Chat,
        summary: "Cancel an AI attempt",
        returns: "The current terminal state; cancelling an already-terminal attempt is a successful no-op.",
        handler: || post(cancel_attempt),
        request: AiBody::None,
        response: AiBody::Json(|generator| generator.subschema_for::<CancelAttemptResponse>()),
        stream: false,
        path_parameters: &["attempt"],
    },
    AiRoute {
        operation_id: "ai_approve_tool_call",
        method: "POST",
        path: "/v1/ai/tool-calls/{tool}/approve",
        scope: AiScope::Chat,
        summary: "Approve a pending workspace-tool call",
        returns: "The durable content-free tool activity. Repeating the same approval is an idempotent read of that state.",
        handler: || post(approve_tool_call),
        request: AiBody::Json(|generator| generator.subschema_for::<ToolDecisionRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<ToolDecisionResponse>()),
        stream: false,
        path_parameters: &["tool"],
    },
    AiRoute {
        operation_id: "ai_deny_tool_call",
        method: "POST",
        path: "/v1/ai/tool-calls/{tool}/deny",
        scope: AiScope::Chat,
        summary: "Deny a pending workspace-tool call",
        returns: "The durable denied activity. The provider receives a structured permission-denied result and may continue.",
        handler: || post(deny_tool_call),
        request: AiBody::Json(|generator| generator.subschema_for::<ToolDecisionRequest>()),
        response: AiBody::Json(|generator| generator.subschema_for::<ToolDecisionResponse>()),
        stream: false,
        path_parameters: &["tool"],
    },
    AiRoute {
        operation_id: "ai_put_attachment",
        method: "PUT",
        path: "/v1/ai/chats/{chat}/attachments/{attachment}",
        scope: AiScope::Chat,
        summary: "Store a picture or file for a local agent chat's next message",
        returns: "The stored attachment's metadata, staged until a send names it. Idempotent by id: the same content answers the same attachment, different content answers attachment_conflict.",
        handler: || {
            axum::routing::put(put_attachment).layer(DefaultBodyLimit::max(ATTACHMENT_BODY_LIMIT))
        },
        request: AiBody::Bytes("application/octet-stream"),
        response: AiBody::Json(|generator| generator.subschema_for::<ChatAttachmentResponse>()),
        stream: false,
        path_parameters: &["chat", "attachment"],
    },
    AiRoute {
        operation_id: "ai_remove_attachment",
        method: "DELETE",
        path: "/v1/ai/chats/{chat}/attachments/{attachment}",
        scope: AiScope::Chat,
        summary: "Remove a staged attachment",
        returns: "Whether a staged attachment was removed. A sent attachment stays with its message and answers attachment_sent.",
        handler: || axum::routing::delete(remove_attachment),
        request: AiBody::None,
        response: AiBody::Json(|generator| generator.subschema_for::<RemoveAttachmentResponse>()),
        stream: false,
        path_parameters: &["chat", "attachment"],
    },
    AiRoute {
        operation_id: "ai_read_attachment",
        method: "GET",
        path: "/v1/ai/chats/{chat}/attachments/{attachment}",
        scope: AiScope::Chat,
        summary: "Read an attachment's stored original",
        returns: "The original bytes, with the stored media type and the file name as an attachment disposition.",
        handler: || get(read_attachment),
        request: AiBody::None,
        response: AiBody::Bytes("application/octet-stream"),
        stream: false,
        path_parameters: &["chat", "attachment"],
    },
    AiRoute {
        operation_id: "ai_read_attachment_thumbnail",
        method: "GET",
        path: "/v1/ai/chats/{chat}/attachments/{attachment}/thumbnail",
        scope: AiScope::Chat,
        summary: "Read a picture attachment's thumbnail",
        returns: "A PNG at most 512 pixels on its long edge; attachment_no_preview for anything but a picture.",
        handler: || get(read_attachment_thumbnail),
        request: AiBody::None,
        response: AiBody::Bytes("image/png"),
        stream: false,
        path_parameters: &["chat", "attachment"],
    },
];

pub fn router() -> Router<Arc<GatewayState>> {
    ROUTES.iter().fold(Router::new(), |router, route| {
        router.route(route.path, (route.handler)())
    })
}

pub fn extend_openapi(document: &mut serde_json::Value) {
    use serde_json::{Map, Value, json};

    let mut schemas = schemars::generate::SchemaSettings::openapi3().into_generator();
    let failure = schemas.subschema_for::<Failure>();
    let stream_event = schemas.subschema_for::<StreamEvent>();
    let pull_event = schemas.subschema_for::<ModelPullEvent>();
    let paths = document["paths"]
        .as_object_mut()
        .expect("gateway OpenAPI paths");
    for route in ROUTES {
        let mut operation = json!({
            "operationId": route.operation_id,
            "summary": route.summary,
            "description": route.returns,
            "security": [{"bearerAuth": [route.scope.as_str()]}],
            "responses": {
                "200": {"description": route.returns},
                "400": {"description": "The request body or path identity is invalid", "content": {"application/json": {"schema": failure}}},
                "401": {"description": "The gateway bearer is absent or invalid", "content": {"application/json": {"schema": failure}}},
                "403": {"description": "The bearer lacks the operation's declared AI scope", "content": {"application/json": {"schema": failure}}},
                "409": {"description": "The operation conflicts with durable chat state", "content": {"application/json": {"schema": failure}}},
                "503": {"description": "The AI worker is unavailable or saturated", "content": {"application/json": {"schema": failure}}}
            }
        });
        match route.request {
            AiBody::None => {}
            AiBody::Json(request) => {
                operation["requestBody"] = json!({
                    "required": true,
                    "content": {"application/json": {"schema": request(&mut schemas)}}
                });
            }
            AiBody::Bytes(media_type) => {
                operation["requestBody"] = json!({
                    "required": true,
                    "content": {media_type: {"schema": {"type": "string", "format": "binary"}}}
                });
            }
        }
        let mut parameters: Vec<Value> = route
            .path_parameters
            .iter()
            .map(|parameter| {
                let schema = if *parameter == "provider" {
                    json!({"type": "string", "pattern": "^[a-z0-9][a-z0-9-]{0,63}$"})
                } else {
                    json!({"type": "string", "format": "uuid"})
                };
                json!({"name": parameter, "in": "path", "required": true, "schema": schema})
            })
            .collect();
        if route.operation_id == "ai_put_attachment" {
            parameters.push(json!({
                "name": "name", "in": "query", "required": true,
                "description": "The file's name, percent-encoded. Checked against the content.",
                "schema": {"type": "string", "minLength": 1}
            }));
            parameters.push(json!({
                "name": "source", "in": "query", "required": true,
                "schema": {"type": "string", "enum": ["clipboard", "drop", "file_picker"]}
            }));
        }
        if !parameters.is_empty() {
            operation["parameters"] = Value::Array(parameters);
        }
        if route.path.contains("/attachments/") {
            for (status, description) in [
                ("404", "The attachment, or the stored file, is not there"),
                ("413", "The attachment is larger than the limit"),
                (
                    "415",
                    "The attachment's type is not allowed, or its content is not what its name says",
                ),
                ("507", "The attachment could not be stored on this computer"),
            ] {
                operation["responses"][status] = json!({"description": description, "content": {"application/json": {"schema": failure}}});
            }
        }
        operation["responses"]["200"]["content"] = match route.response {
            AiBody::Json(response) if route.stream => {
                let response = response(&mut schemas);
                let event = if route.operation_id == "ai_pull_ollama_model" {
                    pull_event.clone()
                } else {
                    stream_event.clone()
                };
                json!({"text/event-stream": {"schema": {"oneOf": [response, event]}}})
            }
            AiBody::Json(response) => {
                json!({"application/json": {"schema": response(&mut schemas)}})
            }
            AiBody::Bytes(media_type) => {
                json!({media_type: {"schema": {"type": "string", "format": "binary"}}})
            }
            AiBody::None => json!({}),
        };
        let entry = paths
            .entry(route.path.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        entry
            .as_object_mut()
            .expect("AI OpenAPI path")
            .insert(route.method.to_lowercase(), operation);
    }
    let components = document["components"]["schemas"]
        .as_object_mut()
        .expect("gateway OpenAPI schemas");
    components.extend(
        schemas
            .take_definitions(true)
            .into_iter()
            .map(|(name, schema)| (name, serde_json::to_value(schema).expect("AI schema"))),
    );
}

async fn provider_settings(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
) -> Response {
    let (client, context) = match admitted(&state, principal, AiScope::ManageProviders) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.provider_settings(context)).await)
}

async fn select_provider(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    payload: Result<Json<SelectProviderRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::ManageProviders) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.select_provider(context, request)).await)
}

async fn update_openai_settings(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    payload: Result<Json<UpdateOpenAiSettingsRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::ManageProviders) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.update_openai_settings(context, request)).await)
}

async fn update_ollama_settings(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    payload: Result<Json<UpdateOllamaSettingsRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::ManageProviders) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.update_ollama_settings(context, request)).await)
}

async fn validate_provider(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(provider): Path<String>,
) -> Response {
    let Ok(provider) = AiProviderId::parse(provider) else {
        return invalid_identity("provider");
    };
    let (client, context) = match admitted(&state, principal, AiScope::ManageProviders) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.validate_provider(context, provider)).await)
}

async fn discover_agent_models(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(provider): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(provider) = AiProviderId::parse(provider) else {
        return invalid_identity("provider");
    };
    let request = match agent_models_request(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::ManageProviders) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.discover_agent_models(context, provider, request)).await)
}

/// The models route's optional body. An empty body asks for the list
/// already known, so every client that predates the body keeps working.
fn agent_models_request(body: &[u8]) -> Result<koru_ai_wire::DiscoverAgentModelsRequest, Response> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Default::default());
    }
    serde_json::from_slice(body).map_err(|_| {
        ai_failure(
            StatusCode::BAD_REQUEST,
            Failure {
                code: "invalid_request".into(),
                message: "The agent models request body is invalid.".into(),
                retryable: false,
                details: Default::default(),
            },
        )
    })
}

async fn pull_ollama_model(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    payload: Result<Json<PullOllamaModelRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::ManageProviders) {
        Ok(values) => values,
        Err(response) => return response,
    };
    match call(move || client.pull_ollama_model(context, request)).await {
        Ok(stream) => model_pull_sse_response(stream),
        Err(failure) => ai_failure(status_for(&failure), failure),
    }
}

async fn cancel_ollama_pull(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
) -> Response {
    let (client, context) = match admitted(&state, principal, AiScope::ManageProviders) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.cancel_ollama_pull(context)).await)
}

async fn create_chat(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    payload: Result<Json<CreateChatRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.create_chat(context, request)).await)
}

async fn list_chats(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
) -> Response {
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.list_chats(context)).await)
}

async fn read_chat(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(chat): Path<String>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.read_chat(context, chat)).await)
}

async fn rename_chat(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(chat): Path<String>,
    payload: Result<Json<RenameChatRequest>, JsonRejection>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.rename_chat(context, chat, request)).await)
}

async fn update_chat_model(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(chat): Path<String>,
    payload: Result<Json<UpdateChatModelRequest>, JsonRejection>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.update_chat_model(context, chat, request)).await)
}

async fn update_chat_privacy(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(chat): Path<String>,
    payload: Result<Json<UpdateChatPrivacyRequest>, JsonRejection>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.update_chat_privacy(context, chat, request)).await)
}

async fn update_chat_agent_settings(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(chat): Path<String>,
    payload: Result<Json<koru_ai_wire::UpdateChatAgentSettingsRequest>, JsonRejection>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.update_chat_agent_settings(context, chat, request)).await)
}

async fn update_chat_folder(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(chat): Path<String>,
    payload: Result<Json<koru_ai_wire::UpdateChatFolderRequest>, JsonRejection>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.update_chat_folder(context, chat, request)).await)
}

async fn tool_list_chats(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    payload: Result<Json<ListChatsToolRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.tool_list_chats(context, request)).await)
}

async fn tool_search_chats(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    payload: Result<Json<SearchChatsToolRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.tool_search_chats(context, request)).await)
}

async fn tool_read_chat(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    payload: Result<Json<ReadChatToolRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.tool_read_chat(context, request)).await)
}

async fn update_chat_tool_policy(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(chat): Path<String>,
    payload: Result<Json<UpdateChatToolPolicyRequest>, JsonRejection>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.update_chat_tool_policy(context, chat, request)).await)
}

async fn read_chat_tool_policy(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(chat): Path<String>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.read_chat_tool_policy(context, chat)).await)
}

async fn delete_chat(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(chat): Path<String>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.delete_chat(context, chat)).await)
}

async fn clear_history(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
) -> Response {
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.clear_history(context)).await)
}

async fn usage(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
) -> Response {
    let (client, context) = match admitted(&state, principal, AiScope::ReadUsage) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.usage(context, None)).await)
}

async fn chat_usage(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(chat): Path<String>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let (client, context) = match admitted(&state, principal, AiScope::ReadUsage) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.usage(context, Some(chat))).await)
}

async fn clear_usage(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
) -> Response {
    let (client, context) = match admitted(&state, principal, AiScope::ReadUsage) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.clear_usage(context)).await)
}

async fn search(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    payload: Result<Json<AiSearchRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.search(context, request)).await)
}

async fn rate_response(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(message): Path<String>,
    payload: Result<Json<FeedbackRequest>, JsonRejection>,
) -> Response {
    let Some(message) = MessageId::parse(&message) else {
        return invalid_identity("message");
    };
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.feedback(context, message, request)).await)
}

async fn branch_chat(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(chat): Path<String>,
    payload: Result<Json<BranchChatRequest>, JsonRejection>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.branch_chat(context, chat, request)).await)
}

async fn retry_turn(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(chat): Path<String>,
    payload: Result<Json<RetryTurnRequest>, JsonRejection>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    match call(move || client.retry_turn(context, chat, request)).await {
        Ok(stream) => sse_response(stream),
        Err(failure) => ai_failure(status_for(&failure), failure),
    }
}

async fn export_chat(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(chat): Path<String>,
    payload: Result<Json<ExportChatRequest>, JsonRejection>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.export_chat(context, chat, request)).await)
}

async fn read_export(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(export): Path<String>,
) -> Response {
    let Some(export) = DocumentExportId::parse(&export) else {
        return invalid_identity("export");
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.read_export(context, export)).await)
}

async fn send_turn(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(chat): Path<String>,
    payload: Result<Json<SendTurnRequest>, JsonRejection>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    if request.chat_id != chat {
        return ai_failure(
            StatusCode::BAD_REQUEST,
            Failure {
                code: "invalid_request".into(),
                message: "The chat path and request identity differ.".into(),
                retryable: false,
                details: Default::default(),
            },
        );
    }
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    match call(move || client.send_turn(context, request)).await {
        Ok(stream) => sse_response(stream),
        Err(failure) => ai_failure(status_for(&failure), failure),
    }
}

async fn read_attempt(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(attempt): Path<String>,
) -> Response {
    let Some(attempt) = InferenceAttemptId::parse(&attempt) else {
        return invalid_identity("attempt");
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.read_attempt(context, attempt)).await)
}

async fn cancel_attempt(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(attempt): Path<String>,
) -> Response {
    let Some(attempt) = InferenceAttemptId::parse(&attempt) else {
        return invalid_identity("attempt");
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.cancel_attempt(context, attempt)).await)
}

async fn approve_tool_call(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(tool): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let Some(tool) = ToolInvocationId::parse(&tool) else {
        return invalid_identity("tool");
    };
    let choice = match tool_decision_choice(&body) {
        Ok(choice) => choice,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.approve_tool_call(context, tool, choice)).await)
}

/// The optional decision body: absent for a workspace tool, the chosen
/// option for a local agent's permission request. An empty body is no
/// choice, so the clients that predate the body keep working unchanged.
fn tool_decision_choice(body: &[u8]) -> Result<Option<String>, Response> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice::<ToolDecisionRequest>(body)
        .map(|request| request.choice)
        .map_err(|_| {
            ai_failure(
                StatusCode::BAD_REQUEST,
                Failure {
                    code: "invalid_request".into(),
                    message: "The tool decision body is invalid.".into(),
                    retryable: false,
                    details: Default::default(),
                },
            )
        })
}

async fn deny_tool_call(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(tool): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let Some(tool) = ToolInvocationId::parse(&tool) else {
        return invalid_identity("tool");
    };
    let choice = match tool_decision_choice(&body) {
        Ok(choice) => choice,
        Err(response) => return response,
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.deny_tool_call(context, tool, choice)).await)
}

/// The query of an upload: the file's name and how it was added.
#[derive(serde::Deserialize)]
struct UploadQuery {
    name: String,
    source: String,
}

async fn put_attachment(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((chat, attachment)): Path<(String, String)>,
    query: Result<Query<UploadQuery>, QueryRejection>,
    body: Bytes,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let Some(attachment) = ChatAttachmentId::parse(&attachment) else {
        return invalid_identity("attachment");
    };
    let Ok(Query(query)) = query else {
        return invalid_request("The upload names no file name or source.");
    };
    let source = match AttachmentSource::parse(&query.source) {
        Some(source) if source != AttachmentSource::Workspace => source,
        _ => return invalid_request("The upload source is not one a person adds a file by."),
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    let upload = koru_ai_api::AttachmentUpload {
        file_name: query.name,
        source,
        bytes: body,
    };
    answer(call(move || client.put_attachment(context, chat, attachment, upload)).await)
}

async fn remove_attachment(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((chat, attachment)): Path<(String, String)>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let Some(attachment) = ChatAttachmentId::parse(&attachment) else {
        return invalid_identity("attachment");
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || client.remove_attachment(context, chat, attachment)).await)
}

async fn read_attachment(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((chat, attachment)): Path<(String, String)>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let Some(attachment) = ChatAttachmentId::parse(&attachment) else {
        return invalid_identity("attachment");
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    match call(move || client.read_attachment(context, chat, attachment)).await {
        Ok(content) => {
            let disposition = format!(
                "attachment; filename*=UTF-8''{}",
                percent_encode_name(&content.file_name)
            );
            bytes_response(&content.media_type, Some(&disposition), content.bytes)
        }
        Err(failure) => ai_failure(status_for(&failure), failure),
    }
}

async fn read_attachment_thumbnail(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((chat, attachment)): Path<(String, String)>,
) -> Response {
    let Some(chat) = ChatId::parse(&chat) else {
        return invalid_identity("chat");
    };
    let Some(attachment) = ChatAttachmentId::parse(&attachment) else {
        return invalid_identity("attachment");
    };
    let (client, context) = match admitted(&state, principal, AiScope::Chat) {
        Ok(values) => values,
        Err(response) => return response,
    };
    match call(move || client.read_attachment_thumbnail(context, chat, attachment)).await {
        Ok(bytes) => bytes_response("image/png", None, bytes),
        Err(failure) => ai_failure(status_for(&failure), failure),
    }
}

fn bytes_response(media_type: &str, disposition: Option<&str>, bytes: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(media_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    if let Some(value) = disposition.and_then(|value| HeaderValue::from_str(value).ok()) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// RFC 5987 `attr-char`: everything else in the name is percent-encoded.
fn percent_encode_name(name: &str) -> String {
    let mut encoded = String::with_capacity(name.len());
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn invalid_request(message: &str) -> Response {
    ai_failure(
        StatusCode::BAD_REQUEST,
        Failure {
            code: "invalid_request".into(),
            message: message.into(),
            retryable: false,
            details: Default::default(),
        },
    )
}

fn admitted(
    state: &GatewayState,
    principal: GatewayPrincipal,
    required: AiScope,
) -> Result<(koru_ai_client::Client, RequestContext), Response> {
    if !principal
        .scopes
        .iter()
        .any(|scope| scope == required.as_str())
    {
        return Err(ai_failure(
            StatusCode::FORBIDDEN,
            Failure {
                code: "forbidden".into(),
                message: "The gateway session does not permit this AI operation.".into(),
                retryable: false,
                details: Default::default(),
            },
        ));
    }
    let Some(client) = state.ai_client() else {
        return Err(ai_failure(
            StatusCode::SERVICE_UNAVAILABLE,
            unavailable_failure(),
        ));
    };
    let scopes = principal
        .scopes
        .iter()
        .filter_map(|scope| AiScope::parse(scope))
        .collect();
    Ok((
        client,
        RequestContext {
            principal: AiPrincipal {
                label: principal.label,
                scopes,
            },
            trace_id: uuid::Uuid::new_v4().to_string(),
            deadline_millis: None,
        },
    ))
}

async fn call<T, F>(operation: F) -> Result<T, Failure>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, Failure> + Send + 'static,
{
    let started = std::time::Instant::now();
    let result = tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| unavailable_failure())?;
    tracing::debug!(
        target: "koru_ai::gateway",
        response_type = std::any::type_name::<T>(),
        outcome = if result.is_ok() { "ok" } else { "failure" },
        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
        "AI facade delegation completed"
    );
    result
}

fn answer<T: serde::Serialize>(result: Result<T, Failure>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(failure) => ai_failure(status_for(&failure), failure),
    }
}

fn json_request<T>(payload: Result<Json<T>, JsonRejection>) -> Result<T, Response> {
    payload.map(|Json(value)| value).map_err(|_| {
        ai_failure(
            StatusCode::BAD_REQUEST,
            Failure {
                code: "invalid_request".into(),
                message: "The AI request body is invalid.".into(),
                retryable: false,
                details: Default::default(),
            },
        )
    })
}

fn sse_response(response: koru_ai_api::AiResponseStream) -> Response {
    let attempt_id = response.accepted.attempt_id;
    let first = sse_bytes("accepted", &response.accepted);
    let (sender, receiver) = tokio::sync::mpsc::channel(SSE_CAPACITY);
    tokio::task::spawn_blocking(move || {
        let mut event_count = 0_u64;
        let mut terminal = false;
        while let Ok(event) = response.events.recv() {
            event_count = event_count.saturating_add(1);
            terminal |= event.event.is_terminal();
            if sender.blocking_send(sse_bytes("event", &event)).is_err() {
                tracing::warn!(
                    target: "koru_ai::stream",
                    attempt = %attempt_id,
                    event_count,
                    terminal,
                    "AI gateway response client disconnected"
                );
                return;
            }
        }
        if terminal {
            tracing::debug!(
                target: "koru_ai::stream",
                attempt = %attempt_id,
                event_count,
                "AI gateway response stream reached a terminal event"
            );
        } else {
            tracing::warn!(
                target: "koru_ai::stream",
                attempt = %attempt_id,
                event_count,
                "AI gateway response source ended before a terminal event"
            );
        }
    });
    struct State {
        first: Option<Bytes>,
        receiver: tokio::sync::mpsc::Receiver<Bytes>,
    }
    let body = Body::from_stream(stream::unfold(
        State {
            first: Some(first),
            receiver,
        },
        |mut state| async move {
            let next = match state.first.take() {
                Some(first) => Some(first),
                None => match tokio::time::timeout(SSE_KEEP_ALIVE, state.receiver.recv()).await {
                    Ok(next) => next,
                    Err(_) => Some(Bytes::from_static(SSE_KEEP_ALIVE_FRAME)),
                },
            };
            next.map(|bytes| (Ok::<_, Infallible>(bytes), state))
        },
    ));
    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn model_pull_sse_response(response: koru_ai_api::ModelPullResponseStream) -> Response {
    let first = sse_bytes("accepted", &response.accepted);
    let (sender, receiver) = tokio::sync::mpsc::channel(SSE_CAPACITY);
    tokio::task::spawn_blocking(move || {
        while let Ok(event) = response.events.recv() {
            if sender.blocking_send(sse_bytes("event", &event)).is_err() {
                break;
            }
        }
    });
    struct State {
        first: Option<Bytes>,
        receiver: tokio::sync::mpsc::Receiver<Bytes>,
    }
    let body = Body::from_stream(stream::unfold(
        State {
            first: Some(first),
            receiver,
        },
        |mut state| async move {
            let next = match state.first.take() {
                Some(first) => Some(first),
                None => match tokio::time::timeout(SSE_KEEP_ALIVE, state.receiver.recv()).await {
                    Ok(next) => next,
                    Err(_) => Some(Bytes::from_static(SSE_KEEP_ALIVE_FRAME)),
                },
            };
            next.map(|bytes| (Ok::<_, Infallible>(bytes), state))
        },
    ));
    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn sse_bytes<T: serde::Serialize>(name: &str, value: &T) -> Bytes {
    let json = serde_json::to_string(value).expect("AI SSE values serialize");
    Bytes::from(format!("event: {name}\ndata: {json}\n\n"))
}

fn invalid_identity(kind: &str) -> Response {
    ai_failure(
        StatusCode::BAD_REQUEST,
        Failure {
            code: "invalid_id".into(),
            message: format!("The {kind} identity is invalid."),
            retryable: false,
            details: Default::default(),
        },
    )
}

fn status_for(failure: &Failure) -> StatusCode {
    use koru_ai_wire::attachment_codes as attachment;
    match failure.code.as_str() {
        "invalid_request" | "invalid_id" | attachment::LIMIT => StatusCode::BAD_REQUEST,
        "forbidden" | attachment::WORKSPACE_NOT_GRANTED => StatusCode::FORBIDDEN,
        "not_found"
        | "chat_not_found"
        | attachment::NOT_FOUND
        | attachment::MISSING
        | attachment::NO_PREVIEW => StatusCode::NOT_FOUND,
        attachment::TOO_LARGE => StatusCode::PAYLOAD_TOO_LARGE,
        attachment::TYPE_NOT_ALLOWED => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        attachment::SENT
        | attachment::CONFLICT
        | attachment::UNSUPPORTED
        | attachment::WORKSPACE_FILE_UNAVAILABLE => StatusCode::CONFLICT,
        attachment::STORAGE => StatusCode::INSUFFICIENT_STORAGE,
        "chat_busy" | "idempotency_conflict" | "model_pull_active" => StatusCode::CONFLICT,
        "deadline_exceeded" | "ai_worker_timeout" => StatusCode::GATEWAY_TIMEOUT,
        "ai_worker_saturated"
        | "ai_unavailable"
        | "ai_context_unavailable"
        | "ai_provider_management_unavailable"
        | "agent_models_listing" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn unavailable_failure() -> Failure {
    Failure {
        code: "ai_unavailable".into(),
        message: "The AI service is not available.".into(),
        retryable: true,
        details: Default::default(),
    }
}

fn ai_failure(status: StatusCode, failure: Failure) -> Response {
    let mut response = Json(failure).into_response();
    *response.status_mut() = status;
    response
}
