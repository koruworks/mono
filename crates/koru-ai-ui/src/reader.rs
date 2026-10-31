// SPDX-License-Identifier: MPL-2.0

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::time::{Duration, Instant};

use koru_ai_core::{
    AiProviderId, AttemptStatus, ChatId, InferenceAttemptId, MessageId, ProfileId, ToolActivity,
    ToolInvocationId, UsageReport, WorkspaceId,
};
use koru_ai_wire::{
    BranchChatRequest, ChatTranscript, CreateChatRequest, ExportChatRequest, FeedbackRequest,
    ModelPullEvent, ProviderSettingsResponse, ProviderValidationResponse, PullOllamaModelRequest,
    RenameChatRequest, RetryTurnRequest, SelectProviderRequest, SendTurnAccepted, SendTurnRequest,
    StreamEvent, UpdateChatModelRequest, UpdateChatPrivacyRequest, UpdateChatToolPolicyRequest,
    UpdateOllamaSettingsRequest, UpdateOpenAiSettingsRequest,
};

const COMMAND_CAPACITY: usize = 8;
const UPDATE_CAPACITY: usize = 32;
const CLIENT_SCOPES: &[&str] = &[
    "ai.chat",
    "ai.manage_providers",
    "ai.read_usage",
    "store.read_documents",
    "store.edit_documents",
    "store.manage_workspaces",
];

pub(crate) enum Command {
    Boot(Option<ChatId>),
    Create(CreateChatRequest),
    Read(ChatId),
    Rename(ChatId, RenameChatRequest),
    UpdateModel(ChatId, UpdateChatModelRequest),
    UpdatePrivacy(ChatId, UpdateChatPrivacyRequest),
    /// The folder the chat's local agent works in.
    UpdateFolder(ChatId, koru_ai_wire::UpdateChatFolderRequest),
    UpdateToolPolicy(ChatId, UpdateChatToolPolicyRequest),
    ApproveTool(ToolInvocationId),
    DenyTool(ToolInvocationId),
    /// Answer a local agent's permission request with one of the choices it
    /// offered: approve or reject as the choice means.
    DecideTool(ToolInvocationId, String, bool),
    Usage(ChatId),
    DeleteChat(ChatId),
    ClearHistory,
    ClearUsage(Option<ChatId>),
    Send(SendTurnRequest),
    Cancel(InferenceAttemptId),
    Feedback(MessageId, FeedbackRequest),
    Branch(ChatId, BranchChatRequest),
    Retry(ChatId, RetryTurnRequest),
    Export(ChatId, ExportChatRequest),
    UpdateOpenAi(UpdateOpenAiSettingsRequest),
    UpdateOllama(UpdateOllamaSettingsRequest),
    SelectProvider(SelectProviderRequest),
    Validate(AiProviderId),
    /// List a local agent's models for the picker's level under it.
    DiscoverModels(AiProviderId),
    Pull(PullOllamaModelRequest),
    CancelPull,
    /// Titles for documents a sent chip names, for drawing it as a titled
    /// link. Resolved here, off the window thread, through the Store catalog.
    DocumentTitles(Vec<koru_ai_core::DocumentId>),
    /// A picture attachment's thumbnail, for a tile on screen
    /// (chat-attachments F2): fetched here, decoded off the window thread.
    AttachmentThumbnail(ChatId, koru_ai_core::ChatAttachmentId),
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Self::Boot(_) => "boot",
            Self::Create(_) => "create",
            Self::Read(_) => "read",
            Self::Rename(_, _) => "rename",
            Self::UpdateModel(_, _) => "update_model",
            Self::UpdatePrivacy(_, _) => "update_privacy",
            Self::UpdateFolder(_, _) => "update_folder",
            Self::UpdateToolPolicy(_, _) => "update_tool_policy",
            Self::ApproveTool(_) => "approve_tool",
            Self::DenyTool(_) => "deny_tool",
            Self::DecideTool(..) => "decide_tool",
            Self::Usage(_) => "usage",
            Self::DeleteChat(_) => "delete_chat",
            Self::ClearHistory => "clear_history",
            Self::ClearUsage(_) => "clear_usage",
            Self::Send(_) => "send",
            Self::Cancel(_) => "cancel",
            Self::Feedback(_, _) => "feedback",
            Self::Branch(_, _) => "branch",
            Self::Retry(_, _) => "retry",
            Self::Export(_, _) => "export",
            Self::UpdateOpenAi(_) => "update_openai",
            Self::UpdateOllama(_) => "update_ollama",
            Self::SelectProvider(_) => "select_provider",
            Self::Validate(_) => "validate",
            Self::DiscoverModels(_) => "discover_models",
            Self::Pull(_) => "pull",
            Self::CancelPull => "cancel_pull",
            Self::DocumentTitles(_) => "document_titles",
            Self::AttachmentThumbnail(..) => "attachment_thumbnail",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DestinationOption {
    pub profile_id: ProfileId,
    pub workspace_id: WorkspaceId,
    pub label: String,
}

#[derive(Debug)]
pub(crate) enum Update {
    Boot {
        settings: ProviderSettingsResponse,
        chats: Vec<koru_ai_core::Chat>,
        transcript: Option<ChatTranscript>,
        destinations: Vec<DestinationOption>,
        destination_error: Option<String>,
    },
    Created(ChatTranscript),
    Transcript(ChatTranscript),
    Renamed(koru_ai_core::Chat),
    ModelUpdated(koru_ai_core::Chat, Option<koru_ai_core::ReasoningControl>),
    PrivacyUpdated(koru_ai_core::Chat),
    FolderUpdated(koru_ai_core::Chat),
    ToolPolicy(koru_ai_core::ChatToolPolicy),
    ToolActivity(koru_ai_core::ToolActivity),
    Usage(UsageReport),
    HistoryChanged {
        chats: Vec<koru_ai_core::Chat>,
        transcript: Option<ChatTranscript>,
    },
    Accepted(SendTurnAccepted),
    Event(StreamEvent),
    RecoveryStarted(InferenceAttemptId),
    Recovered {
        attempt: InferenceAttemptId,
        status: AttemptStatus,
        transcript: ChatTranscript,
    },
    RecoveryFailed {
        attempt: InferenceAttemptId,
        message: String,
    },
    Settings(ProviderSettingsResponse),
    Validation(ProviderValidationResponse),
    AgentModels(koru_ai_wire::AgentModelsResponse),
    AgentModelsFailed(AiProviderId),
    Pull(ModelPullEvent),
    Exported(String),
    /// The titles `Command::DocumentTitles` could find. A document the Store
    /// cannot place is simply absent: its link keeps the fallback word.
    DocumentTitles(Vec<(koru_ai_core::DocumentId, String)>),
    /// The PNG bytes of a thumbnail, or none when it could not be read.
    AttachmentThumbnail {
        id: koru_ai_core::ChatAttachmentId,
        bytes: Option<Vec<u8>>,
    },
    /// A send refused because of one of its attachments, with the code
    /// and the attachment the service named.
    SendRefused(koru_ai_wire::Failure),
    Failure(String),
}

impl Update {
    /// **Whether this answer changes the chat list** a Chats view shows: a
    /// chat made, renamed, deleted or cleared, re-pointed at another model,
    /// made private, or answered — which moves it to the top. The host is
    /// told on exactly these, and nothing polls (AGENTUX I5).
    pub(crate) fn changes_chat_list(&self) -> bool {
        match self {
            Self::Boot { .. }
            | Self::Created(_)
            | Self::Renamed(_)
            | Self::ModelUpdated(..)
            | Self::PrivacyUpdated(_)
            | Self::HistoryChanged { .. } => true,
            Self::Event(event) => event.event.is_terminal(),
            _ => false,
        }
    }
}

pub(crate) struct Reader {
    commands: SyncSender<Command>,
    updates: Receiver<Update>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum SendError {
    Full,
    Disconnected,
}

impl Reader {
    pub(crate) fn start(root: Option<PathBuf>) -> Option<Self> {
        if cfg!(any(test, feature = "no-client")) {
            return None;
        }
        let (commands, command_rx) = sync_channel(COMMAND_CAPACITY);
        let (update_tx, updates) = sync_channel(UPDATE_CAPACITY);
        std::thread::Builder::new()
            .name("koru-ai-ui-client".into())
            .spawn(move || run(root, command_rx, update_tx))
            .ok()?;
        Some(Self { commands, updates })
    }

    pub(crate) fn send(&self, command: Command) -> Result<(), SendError> {
        let name = command.name();
        match self.commands.try_send(command) {
            Ok(()) => {
                tracing::info!(
                    target: "koru_ai::ui",
                    command = name,
                    "AI UI queued a command"
                );
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                tracing::warn!(
                    target: "koru_ai::ui",
                    command = name,
                    reason = "full",
                    "AI UI command queue is full"
                );
                Err(SendError::Full)
            }
            Err(TrySendError::Disconnected(_)) => {
                tracing::warn!(
                    target: "koru_ai::ui",
                    command = name,
                    reason = "disconnected",
                    "AI UI client thread is gone"
                );
                Err(SendError::Disconnected)
            }
        }
    }

    pub(crate) fn try_update(&self) -> Option<Update> {
        match self.updates.try_recv() {
            Ok(update) => Some(update),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_pair() -> (Self, Receiver<Command>, SyncSender<Update>) {
        let (commands, command_rx) = sync_channel(COMMAND_CAPACITY);
        let (update_tx, updates) = sync_channel(UPDATE_CAPACITY);
        (Self { commands, updates }, command_rx, update_tx)
    }
}

pub(crate) fn connect_client(root: Option<&Path>) -> Result<koru_api_client::Client, String> {
    match root {
        Some(root) => koru_api_client::Client::open_at_with_scopes(
            koru_channel::segment(),
            root,
            CLIENT_SCOPES,
        ),
        None => koru_api_client::Client::ensure_with_scopes(koru_channel::segment(), CLIENT_SCOPES),
    }
    .map_err(|_| "Koru AI could not connect to the local Koru service.".to_string())
}

fn run(root: Option<PathBuf>, commands: Receiver<Command>, updates: SyncSender<Update>) {
    tracing::info!(target: "koru_ai::ui", "AI UI client thread started");
    let mut client = connect_client(root.as_deref());
    if let Err(ref message) = client {
        tracing::warn!(
            target: "koru_ai::ui",
            "AI UI client thread could not connect to the local Koru service"
        );
        let _ = updates.send(Update::Failure(message.clone()));
    } else {
        tracing::info!(target: "koru_ai::ui", "AI UI connected to the local Koru service");
    }

    while let Ok(command) = commands.recv() {
        let client = match &client {
            Ok(client) => client,
            Err(_) => {
                client = connect_client(root.as_deref());
                match &client {
                    Ok(client) => {
                        tracing::info!(
                            target: "koru_ai::ui",
                            "AI UI reconnected to the local Koru service"
                        );
                        client
                    }
                    Err(message) => {
                        let _ = updates.send(Update::Failure(message.clone()));
                        continue;
                    }
                }
            }
        };

        match command {
            Command::Boot(requested_chat) => {
                let result = (|| {
                    let settings = client.ai().provider_settings()?;
                    let chats = client.ai().list_chats()?.chats;
                    let transcript = requested_chat
                        .or_else(|| chats.first().map(|chat| chat.id))
                        .map(|chat| client.ai().read_chat(chat))
                        .transpose()?
                        .map(|transcript| decorate_transcript(&client, transcript));
                    let (destinations, destination_error) =
                        match client.store().profiles().and_then(|profiles| {
                            client
                                .store()
                                .workspaces()
                                .map(|workspaces| (profiles, workspaces))
                        }) {
                            Ok((profiles, workspaces)) => (
                                workspaces
                                    .into_iter()
                                    .map(|workspace| {
                                        let profile_name = profiles
                                            .iter()
                                            .find(|profile| profile.id == workspace.owner)
                                            .map(|profile| profile.name.as_str());
                                        DestinationOption {
                                            profile_id: workspace.owner,
                                            workspace_id: workspace.id,
                                            label: profile_name.map_or_else(
                                                || workspace.name.clone(),
                                                |profile| format!("{profile} / {}", workspace.name),
                                            ),
                                        }
                                    })
                                    .collect(),
                                None,
                            ),
                            Err(_) => {
                                let failure = workspace_catalog_failure();
                                (Vec::new(), Some(failure.message))
                            }
                        };
                    Ok::<_, koru_ai_wire::Failure>(Update::Boot {
                        settings,
                        chats,
                        transcript,
                        destinations,
                        destination_error,
                    })
                })();
                send_result(&updates, result);
            }
            Command::Create(request) => {
                let result = client.ai().create_chat(&request).and_then(|created| {
                    client
                        .ai()
                        .read_chat(created.chat.id)
                        .map(|transcript| Update::Created(decorate_transcript(&client, transcript)))
                });
                send_result(&updates, result);
            }
            Command::Read(chat) => {
                send_result(
                    &updates,
                    client.ai().read_chat(chat).map(|transcript| {
                        Update::Transcript(decorate_transcript(&client, transcript))
                    }),
                );
            }
            Command::Rename(chat, request) => {
                let update = match client.ai().rename_chat(chat, &request) {
                    Ok(response) => Update::Renamed(response.chat),
                    Err(error) => Update::Failure(error.message),
                };
                if updates.send(update).is_err() {
                    return;
                }
            }
            Command::UpdateModel(chat, request) => {
                let update = match client.ai().update_chat_model(chat, &request) {
                    Ok(response) => Update::ModelUpdated(response.chat, response.reasoning_control),
                    Err(error) => Update::Failure(error.message),
                };
                if updates.send(update).is_err() {
                    return;
                }
            }
            Command::UpdatePrivacy(chat, request) => {
                let update = match client.ai().update_chat_privacy(chat, &request) {
                    Ok(response) => Update::PrivacyUpdated(response.chat),
                    Err(error) => Update::Failure(error.message),
                };
                if updates.send(update).is_err() {
                    return;
                }
            }
            Command::UpdateFolder(chat, request) => {
                let update = match client.ai().update_chat_folder(chat, &request) {
                    Ok(response) => Update::FolderUpdated(response.chat),
                    Err(error) => Update::Failure(error.message),
                };
                if updates.send(update).is_err() {
                    return;
                }
            }
            Command::UpdateToolPolicy(chat, request) => {
                let update = match client.ai().update_chat_tool_policy(chat, &request) {
                    Ok(response) => Update::ToolPolicy(response.policy),
                    Err(error) => Update::Failure(error.message),
                };
                if updates.send(update).is_err() {
                    return;
                }
            }
            Command::ApproveTool(tool) => {
                let update = match client.ai().approve_tool_call(tool, None) {
                    Ok(response) => {
                        Update::ToolActivity(decorate_tool_activity(&client, response.activity))
                    }
                    Err(error) => Update::Failure(error.message),
                };
                if updates.send(update).is_err() {
                    return;
                }
            }
            Command::DenyTool(tool) => {
                let update = match client.ai().deny_tool_call(tool, None) {
                    Ok(response) => {
                        Update::ToolActivity(decorate_tool_activity(&client, response.activity))
                    }
                    Err(error) => Update::Failure(error.message),
                };
                if updates.send(update).is_err() {
                    return;
                }
            }
            Command::DecideTool(tool, choice, approve) => {
                let update = match if approve {
                    client.ai().approve_tool_call(tool, Some(choice))
                } else {
                    client.ai().deny_tool_call(tool, Some(choice))
                } {
                    Ok(response) => {
                        Update::ToolActivity(decorate_tool_activity(&client, response.activity))
                    }
                    Err(error) => Update::Failure(error.message),
                };
                if updates.send(update).is_err() {
                    return;
                }
            }
            Command::Usage(chat) => {
                send_result(
                    &updates,
                    client
                        .ai()
                        .usage(Some(chat))
                        .map(|response| Update::Usage(response.report)),
                );
            }
            Command::DeleteChat(chat) => {
                let result = client.ai().delete_chat(chat).and_then(|_| {
                    history_after_mutation(&client)
                        .map(|(chats, transcript)| Update::HistoryChanged { chats, transcript })
                });
                send_result(&updates, result);
            }
            Command::ClearHistory => {
                let result = client.ai().clear_history().and_then(|_| {
                    history_after_mutation(&client)
                        .map(|(chats, transcript)| Update::HistoryChanged { chats, transcript })
                });
                send_result(&updates, result);
            }
            Command::ClearUsage(chat) => {
                let result = client
                    .ai()
                    .clear_usage()
                    .and_then(|_| client.ai().usage(chat))
                    .map(|response| Update::Usage(response.report));
                send_result(&updates, result);
            }
            Command::Send(request) => match client.ai().send_turn(&request) {
                Ok(stream) => stream_attempt(client.clone(), request.chat_id, stream, &updates),
                // Refused for one of its attachments: the surface puts the
                // drafts back and says which (chat-attachments §10).
                Err(failure) if refused_for_attachment(&failure) => {
                    let _ = updates.send(Update::SendRefused(failure));
                }
                Err(failure) => {
                    let _ = updates.send(Update::Failure(failure.message));
                }
            },
            Command::Cancel(attempt) => {
                if let Err(failure) = client.ai().cancel_attempt(attempt) {
                    let _ = updates.send(Update::Failure(failure.message));
                }
            }
            Command::Feedback(message, request) => {
                let result = client.ai().feedback(message, &request).and_then(|event| {
                    let chat = event.event.chat_id.ok_or_else(|| koru_ai_wire::Failure {
                        code: "invalid_response".into(),
                        message: "Feedback lost its chat.".into(),
                        retryable: true,
                        details: Default::default(),
                    })?;
                    client.ai().read_chat(chat).map(|transcript| {
                        Update::Transcript(decorate_transcript(&client, transcript))
                    })
                });
                send_result(&updates, result);
            }
            Command::Branch(chat, request) => {
                send_result(
                    &updates,
                    client.ai().branch_chat(chat, &request).map(|response| {
                        Update::Created(decorate_transcript(&client, response.transcript))
                    }),
                );
            }
            Command::Retry(chat, request) => match client.ai().retry_turn(chat, &request) {
                Ok(stream) => stream_attempt(client.clone(), chat, stream, &updates),
                Err(failure) => {
                    let _ = updates.send(Update::Failure(failure.message));
                }
            },
            Command::Export(chat, request) => {
                let result = client
                    .ai()
                    .export_chat(chat, &request)
                    .and_then(|mut response| {
                        for _ in 0..200 {
                            if response.export.status != koru_ai_core::ExportStatus::Pending {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(50));
                            response = client.ai().read_export(response.export.id)?;
                        }
                        if response.export.status == koru_ai_core::ExportStatus::Complete {
                            Ok(Update::Exported(response.export.destination.title))
                        } else {
                            Err(koru_ai_wire::Failure {
                                code: "export_failed".into(),
                                message: "The Koru document could not be created.".into(),
                                retryable: true,
                                details: Default::default(),
                            })
                        }
                    });
                send_result(&updates, result);
            }
            Command::UpdateOpenAi(request) => {
                let result = client
                    .ai()
                    .update_openai_settings(&request)
                    .and_then(|_| client.ai().provider_settings().map(Update::Settings));
                send_result(&updates, result);
            }
            Command::UpdateOllama(request) => {
                let result = client
                    .ai()
                    .update_ollama_settings(&request)
                    .and_then(|_| client.ai().provider_settings().map(Update::Settings));
                send_result(&updates, result);
            }
            Command::SelectProvider(request) => {
                let started = Instant::now();
                tracing::info!(
                    target: "koru_ai::ui",
                    provider = %request.provider,
                    "AI UI select_provider started"
                );
                let result = client.ai().select_provider(&request).map(Update::Settings);
                match &result {
                    Ok(_) => tracing::info!(
                        target: "koru_ai::ui",
                        provider = %request.provider,
                        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                        "AI UI select_provider succeeded"
                    ),
                    Err(failure) => tracing::warn!(
                        target: "koru_ai::ui",
                        provider = %request.provider,
                        code = failure.code.as_str(),
                        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                        "AI UI select_provider failed"
                    ),
                }
                send_result(&updates, result);
            }
            Command::DiscoverModels(provider) => {
                let started = Instant::now();
                let update = match client.ai().discover_agent_models(&provider) {
                    Ok(response) => {
                        tracing::info!(
                            target: "koru_ai::ui",
                            provider = %provider,
                            model_count = response.models.len(),
                            elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                            "AI UI agent models listed"
                        );
                        Update::AgentModels(response)
                    }
                    Err(failure) => {
                        tracing::warn!(
                            target: "koru_ai::ui",
                            provider = %provider,
                            code = failure.code.as_str(),
                            elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                            "AI UI agent models failed"
                        );
                        Update::AgentModelsFailed(provider)
                    }
                };
                send_result(&updates, Ok::<_, koru_ai_wire::Failure>(update));
            }
            Command::Validate(provider) => {
                let started = Instant::now();
                tracing::info!(
                    target: "koru_ai::ui",
                    provider = %provider,
                    "AI UI validate started"
                );
                let result = client
                    .ai()
                    .validate_provider(&provider)
                    .map(Update::Validation);
                match &result {
                    Ok(Update::Validation(response)) => tracing::info!(
                        target: "koru_ai::ui",
                        provider = %provider,
                        status = ?response.status,
                        model_count = response.models.len(),
                        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                        "AI UI validate finished"
                    ),
                    Ok(_) => {}
                    Err(failure) => tracing::warn!(
                        target: "koru_ai::ui",
                        provider = %provider,
                        code = failure.code.as_str(),
                        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                        "AI UI validate failed"
                    ),
                }
                send_result(&updates, result);
            }
            Command::Pull(request) => match client.ai().pull_ollama_model(&request) {
                Ok(stream) => {
                    let stream_updates = updates.clone();
                    std::thread::Builder::new()
                        .name("koru-ai-ui-model-pull".into())
                        .spawn(move || {
                            for event in stream.events {
                                match event {
                                    Ok(event) => {
                                        if stream_updates.send(Update::Pull(event)).is_err() {
                                            break;
                                        }
                                    }
                                    Err(failure) => {
                                        let _ =
                                            stream_updates.send(Update::Failure(failure.message));
                                        break;
                                    }
                                }
                            }
                        })
                        .ok();
                }
                Err(failure) => {
                    let _ = updates.send(Update::Failure(failure.message));
                }
            },
            Command::CancelPull => {
                if let Err(failure) = client.ai().cancel_ollama_pull() {
                    let _ = updates.send(Update::Failure(failure.message));
                }
            }
            Command::DocumentTitles(documents) => {
                // Sent even when empty: the surface is polling for an answer.
                let titles = document_titles(client, documents);
                if updates.send(Update::DocumentTitles(titles)).is_err() {
                    return;
                }
            }
            Command::AttachmentThumbnail(chat, id) => {
                let bytes = client.ai().attachment_thumbnail(chat, id).ok();
                if updates
                    .send(Update::AttachmentThumbnail { id, bytes })
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

fn refused_for_attachment(failure: &koru_ai_wire::Failure) -> bool {
    use koru_ai_wire::attachment_codes as code;
    [
        code::TOO_LARGE,
        code::LIMIT,
        code::TYPE_NOT_ALLOWED,
        code::NOT_FOUND,
        code::MISSING,
        code::SENT,
        code::UNSUPPORTED,
        code::WORKSPACE_NOT_GRANTED,
        code::WORKSPACE_FILE_UNAVAILABLE,
    ]
    .contains(&failure.code.as_str())
}

fn workspace_catalog_failure() -> koru_ai_wire::Failure {
    koru_ai_wire::Failure {
        code: "workspace_catalog_unavailable".into(),
        message: "Koru AI could not load the available workspaces. Try again.".into(),
        retryable: true,
        details: Default::default(),
    }
}

fn stream_attempt(
    client: koru_api_client::Client,
    chat_id: ChatId,
    stream: koru_api_client::AiResponseStream,
    updates: &SyncSender<Update>,
) {
    let accepted = stream.accepted.clone();
    let attempt_id = accepted.attempt_id;
    if updates.send(Update::Accepted(accepted)).is_err() {
        return;
    }
    tracing::debug!(
        target: "koru_ai::ui_stream",
        attempt = %attempt_id,
        chat = %chat_id,
        "AI UI response stream accepted"
    );
    let stream_updates = updates.clone();
    std::thread::Builder::new()
        .name("koru-ai-ui-stream".into())
        .spawn(move || {
            let mut event_count = 0_u64;
            let mut terminal = false;
            let mut failure_code = None;
            for event in stream.events {
                match event {
                    Ok(mut event) => {
                        event_count = event_count.saturating_add(1);
                        terminal = event.event.is_terminal();
                        if let koru_ai_wire::PublicAiEvent::ToolActivity { activity, .. } =
                            &mut event.event
                        {
                            *activity = decorate_tool_activity(&client, activity.clone());
                        }
                        if stream_updates.send(Update::Event(event)).is_err() {
                            return;
                        }
                        if terminal {
                            break;
                        }
                    }
                    Err(failure) => {
                        failure_code = Some(failure.code);
                        break;
                    }
                }
            }
            if terminal {
                tracing::debug!(
                    target: "koru_ai::ui_stream",
                    attempt = %attempt_id,
                    event_count,
                    "AI UI response stream reached a terminal event"
                );
                return;
            }
            tracing::warn!(
                target: "koru_ai::ui_stream",
                attempt = %attempt_id,
                chat = %chat_id,
                event_count,
                error_class = failure_code.as_deref().unwrap_or("unexpected_eof"),
                "AI UI response stream ended before a terminal event; recovering authoritative state"
            );
            if stream_updates
                .send(Update::RecoveryStarted(attempt_id))
                .is_err()
            {
                return;
            }
            recover_attempt(&client, chat_id, attempt_id, &stream_updates);
        })
        .ok();
}

fn recover_attempt(
    client: &koru_api_client::Client,
    chat_id: ChatId,
    attempt_id: InferenceAttemptId,
    updates: &SyncSender<Update>,
) {
    const RECOVERY_POLL: Duration = Duration::from_millis(250);
    const RECOVERY_LOG_EVERY: u64 = 120;

    let started = std::time::Instant::now();
    let mut polls = 0_u64;
    loop {
        polls = polls.saturating_add(1);
        match client.ai().read_attempt(attempt_id) {
            Ok(response) if response.attempt.status.is_terminal() => {
                match client.ai().read_chat(chat_id) {
                    Ok(transcript) => {
                        tracing::info!(
                            target: "koru_ai::ui_stream",
                            attempt = %attempt_id,
                            chat = %chat_id,
                            status = ?response.attempt.status,
                            recovery_polls = polls,
                            elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                            "AI UI recovered a terminal attempt after response stream loss"
                        );
                        let _ = updates.send(Update::Recovered {
                            attempt: attempt_id,
                            status: response.attempt.status,
                            transcript: decorate_transcript(client, transcript),
                        });
                        return;
                    }
                    Err(failure) if polls % RECOVERY_LOG_EVERY == 1 => {
                        tracing::debug!(
                            target: "koru_ai::ui_stream",
                            attempt = %attempt_id,
                            recovery_polls = polls,
                            error_class = %failure.code,
                            "AI UI terminal transcript recovery is still waiting"
                        );
                    }
                    Err(_) => {}
                }
            }
            Ok(_) => {}
            Err(failure) if authoritative_attempt_missing(&failure) => {
                tracing::warn!(
                    target: "koru_ai::ui_stream",
                    attempt = %attempt_id,
                    chat = %chat_id,
                    recovery_polls = polls,
                    "AI UI could not recover an accepted attempt from authoritative state"
                );
                let lang = koru_i18n::Lang::for_system(
                    koru_window::system_locale::system_locale().as_deref(),
                );
                let _ = updates.send(Update::RecoveryFailed {
                    attempt: attempt_id,
                    message: koru_i18n::t(lang, koru_i18n::Key::AiResponseRecoveryFailed).into(),
                });
                return;
            }
            Err(failure) if polls % RECOVERY_LOG_EVERY == 1 => {
                tracing::debug!(
                    target: "koru_ai::ui_stream",
                    attempt = %attempt_id,
                    recovery_polls = polls,
                    error_class = %failure.code,
                    "AI UI attempt recovery is still waiting"
                );
            }
            Err(_) => {}
        }
        std::thread::sleep(RECOVERY_POLL);
    }
}

fn authoritative_attempt_missing(failure: &koru_ai_wire::Failure) -> bool {
    failure.code == "not_found" && !failure.retryable
}

fn history_after_mutation(
    client: &koru_api_client::Client,
) -> Result<(Vec<koru_ai_core::Chat>, Option<ChatTranscript>), koru_ai_wire::Failure> {
    let chats = client.ai().list_chats()?.chats;
    let transcript = chats
        .first()
        .map(|chat| client.ai().read_chat(chat.id))
        .transpose()?
        .map(|transcript| decorate_transcript(client, transcript));
    Ok((chats, transcript))
}

/// Each document's title, through the Store's catalog: where it lives, then
/// that workspace's listing. One listing per workspace however many of its
/// documents are asked about.
fn document_titles(
    client: &koru_api_client::Client,
    documents: Vec<koru_ai_core::DocumentId>,
) -> Vec<(koru_ai_core::DocumentId, String)> {
    let mut listings: std::collections::HashMap<
        WorkspaceId,
        Vec<(koru_ai_core::DocumentId, String)>,
    > = std::collections::HashMap::new();
    let mut titles = Vec::new();
    for document in documents {
        let Ok(location) = client.store().locate_document(document) else {
            continue;
        };
        let listing = listings.entry(location.workspace).or_insert_with(|| {
            client
                .store()
                .documents(location.workspace)
                .map(|entries| {
                    entries
                        .into_iter()
                        .map(|entry| (entry.id, entry.title))
                        .collect()
                })
                .unwrap_or_default()
        });
        if let Some((_, title)) = listing.iter().find(|(id, _)| *id == document) {
            titles.push((document, title.clone()));
        }
    }
    titles
}

fn decorate_transcript(
    client: &koru_api_client::Client,
    mut transcript: ChatTranscript,
) -> ChatTranscript {
    for activity in &mut transcript.tool_activities {
        *activity = decorate_tool_activity(client, activity.clone());
    }
    transcript
}

fn decorate_tool_activity(
    client: &koru_api_client::Client,
    mut activity: ToolActivity,
) -> ToolActivity {
    if activity.status != koru_ai_core::ToolActivityStatus::ApprovalRequired
        && !activity.status.is_terminal()
    {
        return activity;
    }
    let Some(workspace) = activity.target_workspace_id else {
        return activity;
    };
    let Some(document) = activity.target_document_id else {
        return activity;
    };
    activity.display_target = client
        .store()
        .documents(workspace)
        .ok()
        .and_then(|documents| {
            documents
                .into_iter()
                .find(|candidate| candidate.id == document)
                .map(|candidate| candidate.title)
        });
    activity
}

fn send_result<T>(updates: &SyncSender<Update>, result: Result<Update, T>)
where
    T: IntoFailure,
{
    let update = result.unwrap_or_else(|failure| Update::Failure(failure.message()));
    let _ = updates.send(update);
}

trait IntoFailure {
    fn message(self) -> String;
}

impl IntoFailure for koru_ai_wire::Failure {
    fn message(self) -> String {
        self.message
    }
}

pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(16);

#[cfg(test)]
mod tests {
    use super::{CLIENT_SCOPES, authoritative_attempt_missing, workspace_catalog_failure};
    use koru_ai_wire::Failure;

    #[test]
    fn client_requests_gateway_namespaced_store_scopes() {
        assert!(CLIENT_SCOPES.contains(&"store.read_documents"));
        assert!(CLIENT_SCOPES.contains(&"store.edit_documents"));
        assert!(CLIENT_SCOPES.contains(&"store.manage_workspaces"));
        assert!(
            CLIENT_SCOPES
                .iter()
                .all(|scope| scope.starts_with("ai.") || scope.starts_with("store."))
        );
    }

    #[test]
    fn workspace_catalog_failure_is_visible_and_retryable() {
        let failure = workspace_catalog_failure();
        assert_eq!(failure.code, "workspace_catalog_unavailable");
        assert!(failure.message.contains("available workspaces"));
        assert!(failure.retryable);
    }

    #[test]
    fn only_definitive_not_found_ends_authoritative_recovery() {
        let failure = |code: &str, retryable| Failure {
            code: code.into(),
            message: "safe".into(),
            retryable,
            details: Default::default(),
        };

        assert!(authoritative_attempt_missing(&failure("not_found", false)));
        assert!(!authoritative_attempt_missing(&failure("not_found", true)));
        assert!(!authoritative_attempt_missing(&failure("transport", true)));
    }
}
