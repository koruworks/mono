// SPDX-License-Identifier: MPL-2.0

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::time::{Duration, Instant};

use koru_ai_core::{
    AiProviderId, AttemptStatus, ChatId, InferenceAttemptId, ProfileId, ToolActivity,
    ToolInvocationId, UsageReport, WorkspaceId,
};
use koru_ai_wire::{
    BranchChatRequest, ChatTranscript, CreateChatRequest, ExportChatRequest, ModelPullEvent,
    ProviderSettingsResponse, ProviderValidationResponse, PullOllamaModelRequest,
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
/// **The scopes of a surface a host owns** (Bots P1.8 S3,
/// [`crate::owned`]): the ordinary ones and `bots.manage`, which the Bot
/// owner needs to read or write a Bot's conversation at all. The gateway
/// grants it only to a caller the Agent vouches for, so this is asked for
/// only through `Client::connect_first_party`; an ordinary surface never
/// holds it.
const OWNED_CLIENT_SCOPES: &[&str] = &[
    "ai.chat",
    "ai.manage_providers",
    "ai.read_usage",
    "store.read_documents",
    "store.edit_documents",
    "store.manage_workspaces",
    "bots.manage",
];

/// **Which connection a surface's threads make.** Ordinary for every
/// surface but one a host has bound to its own conversation, whose threads
/// connect first-party with [`OWNED_CLIENT_SCOPES`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Scopes {
    #[default]
    Ordinary,
    Owned,
}

impl Scopes {
    /// Connect as this surface connects. Ordinary is exactly what it always
    /// was ([`connect_client`]). Owned is first-party only: it does not
    /// start a gateway, and a vouching Agent that is not there leaves it
    /// without `bots.manage`, which the Bot owner answers as *not found* —
    /// the owned surface's *unavailable*, never a new chat.
    pub(crate) fn connect(self, root: Option<&Path>) -> Result<koru_api_client::Client, String> {
        match self {
            Self::Ordinary => connect_client(root),
            Self::Owned => match root {
                Some(root) => koru_api_client::Client::connect_at_first_party(
                    koru_channel::segment(),
                    root,
                    OWNED_CLIENT_SCOPES,
                ),
                None => koru_api_client::Client::connect_first_party(
                    koru_channel::segment(),
                    OWNED_CLIENT_SCOPES,
                ),
            }
            .map_err(|_| "Koru AI could not connect to the local Koru service.".to_string()),
        }
    }
}

pub(crate) enum Command {
    /// The settings, the window's profile's chats and the chat asked for
    /// (AI schema 13: the list is one profile's).
    Boot(Option<ChatId>, Option<ProfileId>),
    /// One profile's chats again: the window changed profile.
    ListChats(Option<ProfileId>),
    Create(CreateChatRequest),
    Read(ChatId),
    Rename(ChatId, RenameChatRequest),
    /// Pin the chat, or unpin it (AI schema 12).
    Pin(ChatId, koru_ai_wire::PinChatRequest),
    UpdateModel(ChatId, UpdateChatModelRequest),
    UpdatePrivacy(ChatId, UpdateChatPrivacyRequest),
    /// The folder the chat's local agent works in.
    UpdateFolder(ChatId, koru_ai_wire::UpdateChatFolderRequest),
    /// The approval mode the chat's local agent runs its next message in.
    UpdateAgentSettings(ChatId, koru_ai_wire::UpdateChatAgentSettingsRequest),
    UpdateToolPolicy(ChatId, UpdateChatToolPolicyRequest),
    AddGrants(
        ChatId,
        Option<WorkspaceId>,
        Vec<koru_ai_core::DocumentId>,
        uuid::Uuid,
    ),
    /// Restage one sent attachment for recall; the result is correlated by
    /// the newly minted id and runs off the reader's command thread.
    Restage(
        ChatId,
        koru_ai_core::ChatAttachmentId,
        koru_ai_core::ChatAttachmentId,
    ),
    CopyWorkspaceFile(ChatId, koru_ai_wire::FromWorkspaceAttachmentRequest),
    ApproveTool(ToolInvocationId),
    DenyTool(ToolInvocationId),
    /// Answer a local agent's permission request with one of the choices it
    /// offered: approve or reject as the choice means.
    DecideTool(ToolInvocationId, String, bool),
    /// Answer a local agent's questions (koru-ai D108): their row is
    /// approved with an answer per question. A skip is `DenyTool`.
    AnswerQuestions(ToolInvocationId, Vec<koru_ai_core::AgentAnswer>),
    /// The private local usage report. Nothing asks for it since `/usage`
    /// goes to the agent as a message (koru-ai D121); the report and its
    /// card are left as they were, for the maintainers to keep or retire.
    #[allow(dead_code)]
    Usage(ChatId),
    /// Clear the history, and list the window's profile's chats after.
    ClearHistory(Option<ProfileId>),
    ClearUsage(Option<ChatId>),
    Send(SendTurnRequest),
    Cancel(InferenceAttemptId),
    /// Send a queued message into the running reply (`queue`).
    Steer(InferenceAttemptId, koru_ai_wire::SteerAttemptRequest),
    Branch(ChatId, BranchChatRequest),
    Retry(ChatId, RetryTurnRequest),
    Export(ChatId, ExportChatRequest),
    UpdateOpenAi(UpdateOpenAiSettingsRequest),
    UpdateOllama(UpdateOllamaSettingsRequest),
    SelectProvider(SelectProviderRequest),
    Validate(AiProviderId),
    /// List a local agent's models for the picker's level under it.
    DiscoverModels(AiProviderId),
    /// ↻: the agent's list rebuilt first, then listed (D13).
    RefreshModels(AiProviderId),
    /// List a local agent's skills for a folder, for the composer's `$`
    /// (D121); `None` is the home folder.
    DiscoverCommands(AiProviderId, Option<String>),
    Pull(PullOllamaModelRequest),
    CancelPull,
    /// Titles for documents a sent chip names, for drawing it as a titled
    /// link. Resolved here, off the window thread, through the Store catalog.
    DocumentTitles(Vec<koru_ai_core::DocumentId>),
}

impl Command {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Boot(..) => "boot",
            Self::ListChats(_) => "list_chats",
            Self::Create(_) => "create",
            Self::Read(_) => "read",
            Self::Rename(_, _) => "rename",
            Self::Pin(_, _) => "pin",
            Self::UpdateModel(_, _) => "update_model",
            Self::UpdatePrivacy(_, _) => "update_privacy",
            Self::UpdateFolder(_, _) => "update_folder",
            Self::UpdateAgentSettings(_, _) => "update_agent_settings",
            Self::UpdateToolPolicy(_, _) => "update_tool_policy",
            Self::AddGrants(..) => "add_grants",
            Self::Restage(..) => "restage_attachment",
            Self::CopyWorkspaceFile(..) => "copy_workspace_file",
            Self::ApproveTool(_) => "approve_tool",
            Self::DenyTool(_) => "deny_tool",
            Self::DecideTool(..) => "decide_tool",
            Self::AnswerQuestions(..) => "answer_questions",
            Self::Usage(_) => "usage",
            Self::ClearHistory(_) => "clear_history",
            Self::ClearUsage(_) => "clear_usage",
            Self::Send(_) => "send",
            Self::Cancel(_) => "cancel",
            Self::Steer(..) => "steer",
            Self::Branch(_, _) => "branch",
            Self::Retry(_, _) => "retry",
            Self::Export(_, _) => "export",
            Self::UpdateOpenAi(_) => "update_openai",
            Self::UpdateOllama(_) => "update_ollama",
            Self::SelectProvider(_) => "select_provider",
            Self::Validate(_) => "validate",
            Self::DiscoverModels(_) => "discover_models",
            Self::RefreshModels(_) => "refresh_models",
            Self::DiscoverCommands(..) => "discover_commands",
            Self::Pull(_) => "pull",
            Self::CancelPull => "cancel_pull",
            Self::DocumentTitles(_) => "document_titles",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DestinationOption {
    pub profile_id: ProfileId,
    pub workspace_id: WorkspaceId,
    /// *Profile / Workspace*: the name where every profile's workspaces are
    /// listed together.
    pub label: String,
    /// The workspace's own name, without its profile: what a list of one
    /// profile's workspaces shows.
    pub name: String,
}

#[derive(Debug)]
pub(crate) enum Update {
    Boot {
        settings: ProviderSettingsResponse,
        chats: Vec<koru_ai_core::Chat>,
        transcript: Option<ChatTranscript>,
        /// The chat the boot was asked to open is gone: the service answered
        /// `not_found` for it. The surface forgets the link and starts a new
        /// chat instead.
        link_missing: bool,
        destinations: Vec<DestinationOption>,
        destination_error: Option<String>,
    },
    Created(ChatTranscript),
    /// The window's profile's chats, listed again.
    Listed(Vec<koru_ai_core::Chat>),
    Transcript(ChatTranscript),
    Renamed(koru_ai_core::Chat),
    /// A chat pinned or unpinned, in place like a rename.
    Pinned(koru_ai_core::Chat),
    ModelUpdated(koru_ai_core::Chat, Option<koru_ai_core::ReasoningControl>),
    PrivacyUpdated(koru_ai_core::Chat),
    FolderUpdated(koru_ai_core::Chat),
    AgentSettingsUpdated(koru_ai_core::Chat),
    ToolPolicy(koru_ai_core::ChatToolPolicy),
    GrantsAdded {
        chat: ChatId,
        op: uuid::Uuid,
        result: Result<koru_ai_core::ChatToolPolicy, String>,
    },
    Restaged {
        new_id: koru_ai_core::ChatAttachmentId,
        result: Result<koru_ai_core::ChatAttachment, String>,
    },
    AttachmentCopied {
        chat: ChatId,
        id: koru_ai_core::ChatAttachmentId,
        result: Result<koru_ai_core::ChatAttachment, koru_ai_wire::Failure>,
    },
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
    /// A recovery ended without its answer: why, by the service's failure.
    /// The surface says it — in the host's words when its owner names the
    /// code — since the reader thread holds no host's data.
    RecoveryFailed {
        attempt: InferenceAttemptId,
        failure: koru_ai_wire::Failure,
    },
    Settings(ProviderSettingsResponse),
    Validation(ProviderValidationResponse),
    AgentModels(koru_ai_wire::AgentModelsResponse),
    AgentModelsFailed(AiProviderId),
    /// The host is still building the agent's list.
    AgentModelsPending(AiProviderId),
    /// An agent's skills for a folder (D121): `None` when they could not be
    /// listed, and `$` then offers nothing until it is asked again.
    AgentCommands {
        provider: AiProviderId,
        folder: Option<String>,
        commands: Option<Vec<koru_ai_wire::AgentCommandView>>,
    },
    Pull(ModelPullEvent),
    /// A reply stored as a Koru document: its title, and the document,
    /// which *Open* after the status line opens (D78).
    Exported(String, Option<koru_ai_core::DocumentId>),
    /// The titles `Command::DocumentTitles` could find. A document the Store
    /// cannot place is simply absent: its link keeps the fallback word.
    DocumentTitles(Vec<(koru_ai_core::DocumentId, String)>),
    /// The running reply took a steered message.
    Steered(koru_ai_wire::SteerAttemptResponse),
    /// The running reply would not take it; the message waits in the queue.
    SteerFailed(koru_ai_wire::Failure),
    /// A send that did not go, for any reason: the surface gives the
    /// message back whole — text, chips, quotes and attachments.
    SendFailed(koru_ai_wire::Failure),
    /// The service would not make the chat a first message was waiting on.
    /// What was queued behind it for that chat is answered with the same
    /// failure, so the message comes back to the composer with the reason.
    CreateFailed(ChatId, koru_ai_wire::Failure),
    Failure(String),
    /// **An owned surface's conversation cannot be shown** (Bots P1.8 S3):
    /// the chat it boots or reads is gone, or the service did not answer.
    /// Only an owned reader sends this; an ordinary one answers as it
    /// always did. `chat` is the chat asked about, `None` for a boot that
    /// named none.
    Unavailable {
        chat: Option<ChatId>,
        reason: crate::owned::Unavailable,
    },
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
            | Self::Pinned(_)
            | Self::ModelUpdated(..)
            | Self::PrivacyUpdated(_)
            | Self::Listed(_)
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
    pub(crate) fn start(root: Option<PathBuf>, scopes: Scopes) -> Option<Self> {
        if cfg!(any(test, feature = "no-client")) {
            return None;
        }
        let (commands, command_rx) = sync_channel(COMMAND_CAPACITY);
        let (update_tx, updates) = sync_channel(UPDATE_CAPACITY);
        std::thread::Builder::new()
            .name("koru-ai-ui-client".into())
            .spawn(move || run(root, scopes, command_rx, update_tx))
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

fn run(
    root: Option<PathBuf>,
    scopes: Scopes,
    commands: Receiver<Command>,
    updates: SyncSender<Update>,
) {
    tracing::info!(target: "koru_ai::ui", "AI UI client thread started");
    let owned = scopes == Scopes::Owned;
    let mut client = scopes.connect(root.as_deref());
    if let Err(ref message) = client {
        tracing::warn!(
            target: "koru_ai::ui",
            "AI UI client thread could not connect to the local Koru service"
        );
        let _ = updates.send(Update::Failure(message.clone()));
    } else {
        tracing::info!(target: "koru_ai::ui", "AI UI connected to the local Koru service");
    }

    // A chat whose create failed, and why: the commands queued behind the
    // create for it (its folder, the first message) are answered with that
    // failure instead of reaching a service that has no such chat.
    let mut refused: Option<(ChatId, koru_ai_wire::Failure)> = None;
    // **While this lives, someone reads the updates.** It drops when the
    // surface's `Reader` is gone (its commands end this loop); a response
    // stream's recovery thread holds it weakly and stops then.
    let alive = std::sync::Arc::new(());
    while let Ok(command) = commands.recv() {
        if let Some(update) = refused_answer(refused.as_ref(), &command) {
            if updates.send(update).is_err() {
                return;
            }
            continue;
        }
        let client = match &client {
            Ok(client) => client,
            Err(_) => {
                client = scopes.connect(root.as_deref());
                match &client {
                    Ok(client) => {
                        tracing::info!(
                            target: "koru_ai::ui",
                            "AI UI reconnected to the local Koru service"
                        );
                        client
                    }
                    Err(message) => {
                        let failure = unreachable_failure(message.clone());
                        let update = match &command {
                            // An owned surface's conversation: unreachable,
                            // which it shows as such — never a new chat.
                            Command::Boot(chat, _) if owned => Update::Unavailable {
                                chat: *chat,
                                reason: crate::owned::Unavailable::Unreachable,
                            },
                            Command::Read(chat) if owned => Update::Unavailable {
                                chat: Some(*chat),
                                reason: crate::owned::Unavailable::Unreachable,
                            },
                            Command::Create(request) => {
                                refused = Some((request.chat_id, failure.clone()));
                                Update::CreateFailed(request.chat_id, failure)
                            }
                            Command::Send(_) => Update::SendFailed(failure),
                            Command::Steer(..) => Update::SteerFailed(failure),
                            _ => Update::Failure(message.clone()),
                        };
                        let _ = updates.send(update);
                        continue;
                    }
                }
            }
        };

        match command {
            Command::Boot(requested_chat, profile) => {
                let result = (|| {
                    let settings = client.ai().provider_settings()?;
                    let chats = client.ai().list_chats(profile)?.chats;
                    // **A chat asked for by name that the service no longer
                    // holds** — deleted since a tab was saved over it
                    // (ADR-483-chattabrestore) — boots without it. Failing
                    // the whole boot left the tab with no settings and no
                    // list, on an error it could do nothing about. Any other
                    // failure still fails the boot.
                    let mut link_missing = false;
                    let transcript = match requested_chat {
                        Some(chat) => match client.ai().read_chat(chat) {
                            Ok(transcript) => Some(transcript),
                            Err(failure) if authoritatively_missing(&failure) => {
                                link_missing = true;
                                None
                            }
                            Err(failure) => return Err(failure),
                        },
                        None => chats
                            .first()
                            .map(|chat| client.ai().read_chat(chat.id))
                            .transpose()?,
                    }
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
                                            name: workspace.name,
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
                        link_missing,
                        destinations,
                        destination_error,
                    })
                })();
                match result {
                    // An owned surface boots only to show its conversation:
                    // a boot that failed is that conversation unreachable.
                    Err(_) if owned => {
                        let _ = updates.send(Update::Unavailable {
                            chat: requested_chat,
                            reason: crate::owned::Unavailable::Unreachable,
                        });
                    }
                    result => send_result(&updates, result),
                }
            }
            Command::Create(request) => match client.ai().create_chat(&request) {
                Ok(created) => {
                    refused = None;
                    let result = client.ai().read_chat(created.chat.id).map(|transcript| {
                        Update::Created(decorate_transcript(&client, transcript))
                    });
                    send_result(&updates, result);
                }
                Err(failure) => {
                    refused = Some((request.chat_id, failure.clone()));
                    let _ = updates.send(Update::CreateFailed(request.chat_id, failure));
                }
            },
            Command::Read(chat) => match client.ai().read_chat(chat) {
                Ok(transcript) => {
                    let _ =
                        updates.send(Update::Transcript(decorate_transcript(&client, transcript)));
                }
                // An owned surface's read that failed says which way, typed;
                // the service's English message is never shown.
                Err(failure) if owned => {
                    let _ = updates.send(Update::Unavailable {
                        chat: Some(chat),
                        reason: if authoritatively_missing(&failure) {
                            crate::owned::Unavailable::Missing
                        } else {
                            crate::owned::Unavailable::Unreachable
                        },
                    });
                }
                Err(failure) => send_result::<koru_ai_wire::Failure>(&updates, Err(failure)),
            },
            Command::Rename(chat, request) => {
                let update = match client.ai().rename_chat(chat, &request) {
                    Ok(response) => Update::Renamed(response.chat),
                    Err(error) => Update::Failure(error.message),
                };
                if updates.send(update).is_err() {
                    return;
                }
            }
            Command::Pin(chat, request) => {
                let update = match client.ai().pin_chat(chat, &request) {
                    Ok(response) => Update::Pinned(response.chat),
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
            Command::UpdateAgentSettings(chat, request) => {
                let update = match client.ai().update_chat_agent_settings(chat, &request) {
                    Ok(response) => Update::AgentSettingsUpdated(response.chat),
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
            Command::AddGrants(chat, current, documents, op) => {
                let mut workspace_ids: Vec<WorkspaceId> = current.into_iter().collect();
                let mut failed = None;
                for document in documents {
                    match client.store().locate_document(document) {
                        Ok(location) if location.document == document => {
                            if !workspace_ids.contains(&location.workspace) {
                                workspace_ids.push(location.workspace);
                            }
                        }
                        _ => {
                            failed = Some("A mentioned document is unavailable.".to_owned());
                            break;
                        }
                    }
                }
                let result = match failed {
                    Some(message) => Err(message),
                    None => client
                        .ai()
                        .add_chat_workspace_grants(
                            chat,
                            &koru_ai_wire::AddChatWorkspaceGrantsRequest { workspace_ids },
                        )
                        .map(|response| response.policy)
                        .map_err(|error| error.message),
                };
                if updates
                    .send(Update::GrantsAdded { chat, op, result })
                    .is_err()
                {
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
            Command::AnswerQuestions(tool, answers) => {
                let update = match client.ai().answer_tool_questions(tool, answers) {
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
            Command::ListChats(profile) => {
                send_result(
                    &updates,
                    client
                        .ai()
                        .list_chats(profile)
                        .map(|response| Update::Listed(response.chats)),
                );
            }
            Command::ClearHistory(profile) => {
                let result = client.ai().clear_history().and_then(|_| {
                    history_after_mutation(&client, profile)
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
            Command::Restage(chat, source, new_id) => {
                let client = client.clone();
                let updates = updates.clone();
                off_thread("koru-ai-restage", move || {
                    let result = client
                        .ai()
                        .restage_attachment(chat, source, new_id)
                        .map(|response| response.attachment)
                        .map_err(|failure| failure.message);
                    let _ = updates.send(Update::Restaged { new_id, result });
                });
            }
            Command::CopyWorkspaceFile(chat, request) => {
                let client = client.clone();
                let updates = updates.clone();
                off_thread("koru-ai-workspace-copy", move || {
                    let id = request.id;
                    let result = client
                        .ai()
                        .from_workspace_attachment(chat, request)
                        .map(|response| response.attachment);
                    let _ = updates.send(Update::AttachmentCopied { chat, id, result });
                });
            }
            Command::Send(request) => match client.ai().send_turn(&request) {
                Ok(stream) => stream_attempt(
                    client.clone(),
                    request.chat_id,
                    stream,
                    &updates,
                    std::sync::Arc::downgrade(&alive),
                ),
                // The surface gives the message back (chat-attachments §10).
                Err(failure) => {
                    let _ = updates.send(Update::SendFailed(failure));
                }
            },
            Command::Cancel(attempt) => {
                if let Err(failure) = client.ai().cancel_attempt(attempt) {
                    let _ = updates.send(Update::Failure(failure.message));
                }
            }
            Command::Steer(attempt, request) => {
                let update = match client.ai().steer_attempt(attempt, &request) {
                    Ok(response) => Update::Steered(response),
                    Err(failure) => Update::SteerFailed(steer_failure(failure)),
                };
                let _ = updates.send(update);
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
                Ok(stream) => stream_attempt(
                    client.clone(),
                    chat,
                    stream,
                    &updates,
                    std::sync::Arc::downgrade(&alive),
                ),
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
                            Ok(Update::Exported(
                                response.export.destination.title,
                                response.export.document_id,
                            ))
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
                // An agent's list can be seconds away the first time it is
                // built. Every other command — sending a turn, reading a
                // chat, validating — must not queue behind it.
                let client = client.clone();
                let listing_updates = updates.clone();
                off_thread("koru-ai-ui-agent-models", move || {
                    let update = list_agent_models(&client, provider, false);
                    let _ = listing_updates.send(update);
                });
            }
            Command::RefreshModels(provider) => {
                let client = client.clone();
                let listing_updates = updates.clone();
                off_thread("koru-ai-ui-agent-models", move || {
                    let update = list_agent_models(&client, provider, true);
                    let _ = listing_updates.send(update);
                });
            }
            Command::DiscoverCommands(provider, folder) => {
                // The agent lists its skills from a process of its own:
                // never in front of a send.
                let client = client.clone();
                let listing_updates = updates.clone();
                off_thread("koru-ai-ui-agent-commands", move || {
                    let started = Instant::now();
                    let commands = match client
                        .ai()
                        .discover_agent_commands(&provider, folder.as_deref())
                    {
                        Ok(response) => Some(response.commands),
                        Err(failure) => {
                            tracing::info!(
                                target: "koru_ai::ui",
                                provider = %provider,
                                code = failure.code.as_str(),
                                "AI UI agent skills not listed"
                            );
                            None
                        }
                    };
                    tracing::info!(
                        target: "koru_ai::ui",
                        provider = %provider,
                        count = commands.as_ref().map_or(0, Vec::len),
                        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                        "AI UI agent skills listed"
                    );
                    let _ = listing_updates.send(Update::AgentCommands {
                        provider,
                        folder,
                        commands,
                    });
                });
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
        }
    }
}

/// Only a definite safe refusal permits a new queued send. A missing HTTP
/// acknowledgment may follow native acceptance and must preserve uncertainty.
pub(super) fn steer_failure(mut failure: koru_ai_wire::Failure) -> koru_ai_wire::Failure {
    if failure.code == "steer_unavailable" || (failure.code == "steer_failed" && !failure.retryable)
    {
        return failure;
    }
    failure.code = "steer_failed".into();
    failure.retryable = false;
    // Preserve the sanitized detail. The surface adds the uncertainty notice
    // in its current language when it pauses the affected queue item.
    failure
}

#[cfg(test)]
mod steer_tests;

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
    alive: std::sync::Weak<()>,
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
            recover_attempt(&client, chat_id, attempt_id, &stream_updates, &alive);
        })
        .ok();
}

/// How soon a recovery reads again while the attempt is not terminal yet or
/// a read failed in a way the service says will pass, doubling to
/// [`RECOVERY_MAX`] — the bounded clock the Bot surfaces read an operation
/// on (250 ms doubling to 2 s), instead of a fixed 250 ms forever.
const RECOVERY_FIRST: Duration = Duration::from_millis(250);
const RECOVERY_MAX: Duration = Duration::from_secs(2);

fn recover_attempt(
    client: &koru_api_client::Client,
    chat_id: ChatId,
    attempt_id: InferenceAttemptId,
    updates: &SyncSender<Update>,
    alive: &std::sync::Weak<()>,
) {
    let outcome = recover(
        || {
            client
                .ai()
                .read_attempt(attempt_id)
                .map(|response| response.attempt.status)
        },
        || {
            client
                .ai()
                .read_chat(chat_id)
                .map(|transcript| decorate_transcript(client, transcript))
        },
        || alive.upgrade().is_some(),
        std::thread::sleep,
        attempt_id,
    );
    if let Some(update) = outcome {
        let _ = updates.send(update);
    }
}

/// **Recover an attempt whose response stream ended early**, from the
/// service's authoritative state: read the attempt until it is terminal,
/// then its chat. The answer for the surface, or `None` when nobody reads
/// the updates any more (the surface's reader is gone), which ends it.
///
/// - **A failure the service says will pass** (`retryable`, as a lost
///   connection is) is read again on the bounded clock; an ordinary chat's
///   recovery waits through it as before.
/// - **Any other failure ends it**, handed to the surface as what it is: an
///   attempt the service authoritatively does not have reads as the
///   recovery failing; any other — a host's hold, such as a Bot
///   conversation in Trash — as the surface's sentence for its code
///   (`ChatSurface::failure_words`, the host's words first). Before, only
///   the first ended it, and a hold was read every 250 ms forever.
fn recover(
    mut read_attempt: impl FnMut() -> Result<AttemptStatus, koru_ai_wire::Failure>,
    mut read_chat: impl FnMut() -> Result<ChatTranscript, koru_ai_wire::Failure>,
    alive: impl Fn() -> bool,
    mut pause: impl FnMut(Duration),
    attempt_id: InferenceAttemptId,
) -> Option<Update> {
    const RECOVERY_LOG_EVERY: u64 = 120;
    let started = std::time::Instant::now();
    let mut polls = 0_u64;
    let mut wait = RECOVERY_FIRST;
    loop {
        if !alive() {
            tracing::debug!(
                target: "koru_ai::ui_stream",
                attempt = %attempt_id,
                recovery_polls = polls,
                "AI UI recovery stopped: nobody reads its answer any more"
            );
            return None;
        }
        polls = polls.saturating_add(1);
        let failure = match read_attempt() {
            Ok(status) if status.is_terminal() => match read_chat() {
                Ok(transcript) => {
                    tracing::info!(
                        target: "koru_ai::ui_stream",
                        attempt = %attempt_id,
                        status = ?status,
                        recovery_polls = polls,
                        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                        "AI UI recovered a terminal attempt after response stream loss"
                    );
                    return Some(Update::Recovered {
                        attempt: attempt_id,
                        status,
                        transcript,
                    });
                }
                Err(failure) => Some(failure),
            },
            Ok(_) => None,
            Err(failure) => Some(failure),
        };
        if let Some(failure) = &failure {
            if !failure.retryable {
                tracing::warn!(
                    target: "koru_ai::ui_stream",
                    attempt = %attempt_id,
                    recovery_polls = polls,
                    error_class = %failure.code,
                    "AI UI could not recover an accepted attempt from authoritative state"
                );
                return Some(Update::RecoveryFailed {
                    attempt: attempt_id,
                    failure: failure.clone(),
                });
            }
            if polls % RECOVERY_LOG_EVERY == 1 {
                tracing::debug!(
                    target: "koru_ai::ui_stream",
                    attempt = %attempt_id,
                    recovery_polls = polls,
                    error_class = %failure.code,
                    "AI UI attempt recovery is still waiting"
                );
            }
        }
        pause(wait);
        wait = (wait * 2).min(RECOVERY_MAX);
    }
}

/// The service says, and will keep saying, that the item asked about does not
/// exist: an attempt that recovery waits on, or a chat a boot was asked to
/// open. A retryable `not_found` is not this.
pub(crate) fn authoritatively_missing(failure: &koru_ai_wire::Failure) -> bool {
    failure.code == "not_found" && !failure.retryable
}

fn history_after_mutation(
    client: &koru_api_client::Client,
    profile: Option<ProfileId>,
) -> Result<(Vec<koru_ai_core::Chat>, Option<ChatTranscript>), koru_ai_wire::Failure> {
    let chats = client.ai().list_chats(profile)?.chats;
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

/// An agent's list, asked again while the host is still building it
/// (`crate::agent_catalog::discover`, the retry a catalog host shares).
/// `refresh` rebuilds it first (↻); the asks after the first only wait for
/// that rebuild.
fn list_agent_models(
    client: &koru_api_client::Client,
    provider: AiProviderId,
    refresh: bool,
) -> Update {
    let started = Instant::now();
    let answer = crate::agent_catalog::discover(client, &provider, refresh);
    match answer {
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
        // Still being built after every ask: the picker keeps saying so,
        // and the next open asks again.
        Err(failure) if crate::agent_catalog::is_still_listing(&failure) => {
            tracing::info!(
                target: "koru_ai::ui",
                provider = %provider,
                elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                "AI UI agent models still listing"
            );
            Update::AgentModelsPending(provider)
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
    }
}

/// Run `work` on a short-lived thread of its own, the way a pull's stream
/// is read, so the client thread goes straight on to the next command. A
/// thread that cannot be started runs the work here instead: late beats
/// never.
fn off_thread(name: &str, work: impl FnOnce() + Send + 'static) {
    let work = std::sync::Arc::new(std::sync::Mutex::new(Some(work)));
    let spawned = {
        let work = work.clone();
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                if let Some(work) = work.lock().ok().and_then(|mut work| work.take()) {
                    work();
                }
            })
    };
    if spawned.is_err()
        && let Some(work) = work.lock().ok().and_then(|mut work| work.take())
    {
        work();
    }
}

/// The answer for a command about a chat whose create failed, without
/// asking the service: a send comes back to be given back, anything else is
/// the create's reason. `None` for every other command.
fn refused_answer(
    refused: Option<&(ChatId, koru_ai_wire::Failure)>,
    command: &Command,
) -> Option<Update> {
    let (chat, failure) = refused?;
    let about = match command {
        Command::Send(request) => {
            return (request.chat_id == *chat).then(|| Update::SendFailed(failure.clone()));
        }
        Command::AddGrants(about, _, _, op) => {
            return (*about == *chat).then(|| Update::GrantsAdded {
                chat: *about,
                op: *op,
                result: Err(failure.message.clone()),
            });
        }
        Command::Read(about)
        | Command::Rename(about, _)
        | Command::Pin(about, _)
        | Command::UpdateModel(about, _)
        | Command::UpdatePrivacy(about, _)
        | Command::UpdateFolder(about, _)
        | Command::UpdateToolPolicy(about, _) => about,
        _ => return None,
    };
    (about == chat).then(|| Update::Failure(failure.message.clone()))
}

/// The local service could not be reached, as a failure a send can carry.
fn unreachable_failure(message: String) -> koru_ai_wire::Failure {
    koru_ai_wire::Failure {
        code: "ai_unavailable".into(),
        message,
        retryable: true,
        details: Default::default(),
    }
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
    use super::{
        CLIENT_SCOPES, Command, RECOVERY_FIRST, RECOVERY_MAX, Update, authoritatively_missing,
        off_thread, recover, refused_answer, workspace_catalog_failure,
    };

    /// **What waits behind a failed create is answered with its failure**: a
    /// first message comes back to be given back, the folder that followed
    /// says the same reason, and another chat's commands go to the service.
    #[test]
    fn commands_behind_a_failed_create_are_answered_with_its_failure() {
        let chat = koru_ai_core::ChatId::new();
        let failure = Failure {
            code: "ai_storage".into(),
            message: "The chat could not be saved.".into(),
            retryable: true,
            details: Default::default(),
        };
        let refused = (chat, failure.clone());
        let send = |chat_id| {
            Command::Send(koru_ai_wire::SendTurnRequest {
                destination_workspace: None,
                operation_id: uuid::Uuid::new_v4(),
                chat_id,
                surface: koru_ai_core::AiSurface::ContextPane,
                markdown: "hi".into(),
                explicit_refs: Vec::new(),
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
                attachment_ids: Vec::new(),
                answers: None,
                skills: Vec::new(),
            })
        };
        assert!(matches!(
            refused_answer(Some(&refused), &send(chat)),
            Some(Update::SendFailed(answered)) if answered == failure
        ));
        let folder = Command::UpdateFolder(
            chat,
            koru_ai_wire::UpdateChatFolderRequest {
                folder: Some("D:/work".into()),
            },
        );
        assert!(matches!(
            refused_answer(Some(&refused), &folder),
            Some(Update::Failure(message)) if message == failure.message
        ));
        let op = uuid::Uuid::new_v4();
        assert!(matches!(
            refused_answer(Some(&refused), &Command::AddGrants(chat, None, Vec::new(), op)),
            Some(Update::GrantsAdded { chat: answered, op: identity, result: Err(message) })
                if answered == chat && identity == op && message == failure.message
        ));
        assert!(refused_answer(Some(&refused), &send(koru_ai_core::ChatId::new())).is_none());
        assert!(refused_answer(None, &send(chat)).is_none());
        assert!(refused_answer(Some(&refused), &Command::ClearHistory(None)).is_none());
    }
    use koru_ai_wire::Failure;

    /// A listing that is still waiting on its agent does not hold the next
    /// command: the work after it finishes first.
    #[test]
    fn a_slow_listing_runs_off_the_client_thread() {
        let (release, released) = std::sync::mpsc::channel::<()>();
        let (done, finished) = std::sync::mpsc::channel::<&'static str>();
        let listing_done = done.clone();
        off_thread("fixture-agent-models", move || {
            released.recv().unwrap();
            listing_done.send("listing").unwrap();
        });
        done.send("validate").unwrap();
        assert_eq!(finished.recv().unwrap(), "validate");
        release.send(()).unwrap();
        assert_eq!(finished.recv().unwrap(), "listing");
    }

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

    /// **An ordinary surface asks for exactly what it always asked for**
    /// (Bots P1.8 S3): the owned seam added a second list, and this one is
    /// pinned word for word, `bots.manage` nowhere in it.
    #[test]
    fn the_ordinary_scopes_are_unchanged_by_the_owned_seam() {
        assert_eq!(
            CLIENT_SCOPES,
            &[
                "ai.chat",
                "ai.manage_providers",
                "ai.read_usage",
                "store.read_documents",
                "store.edit_documents",
                "store.manage_workspaces",
            ]
        );
        assert!(!CLIENT_SCOPES.contains(&"bots.manage"));
        assert_eq!(super::Scopes::default(), super::Scopes::Ordinary);
    }

    /// An owned surface holds the ordinary scopes and `bots.manage`, and
    /// nothing else: naming scopes filters rather than adds.
    #[test]
    fn the_owned_scopes_are_the_ordinary_ones_and_bots_manage() {
        let (last, ordinary) = super::OWNED_CLIENT_SCOPES
            .split_last()
            .expect("owned scopes");
        assert_eq!(ordinary, CLIENT_SCOPES);
        assert_eq!(*last, "bots.manage");
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

        assert!(authoritatively_missing(&failure("not_found", false)));
        assert!(!authoritatively_missing(&failure("not_found", true)));
        assert!(!authoritatively_missing(&failure("transport", true)));
    }

    fn refusal(code: &str, retryable: bool) -> Failure {
        Failure {
            code: code.into(),
            message: "The owner's English developer text.".into(),
            retryable,
            details: Default::default(),
        }
    }

    /// The transcript read, for a test that never reaches it.
    fn no_transcript() -> Result<koru_ai_wire::ChatTranscript, Failure> {
        panic!("the attempt is not terminal")
    }

    /// **A hold ends a recovery, handed on as the hold it is.** A Bot
    /// conversation in Trash, its Bot in Trash or archived answer the
    /// recovery's reads with their non-retryable lifecycle codes (the AI
    /// server's `hold_failure`): the recovery stops at once and hands the
    /// surface the failure, which says it in its host's words — never
    /// "recovering" every 250 ms forever.
    #[test]
    fn a_non_retryable_hold_ends_a_recovery_in_its_own_words() {
        let attempt = koru_ai_core::InferenceAttemptId::new();
        for code in [
            koru_ai_wire::bot_lifecycle_codes::BOT_TRASHED,
            koru_ai_wire::bot_lifecycle_codes::TRASHED,
            "bot_archived",
        ] {
            // Terminal attempt, refused transcript: the chat is held.
            let mut pauses = 0;
            let said = recover(
                || Ok(koru_ai_core::AttemptStatus::Cancelled),
                || Err(refusal(code, false)),
                || true,
                |_| pauses += 1,
                attempt,
            );
            let Some(Update::RecoveryFailed {
                attempt: failed,
                failure,
            }) = said
            else {
                panic!("{code}: ended");
            };
            assert_eq!(failed, attempt);
            assert_eq!(failure, refusal(code, false), "{code}: the hold itself");
            assert_eq!(pauses, 0, "{code}: not read again");
            // The attempt's own read refused the same way ends it too.
            let said = recover(
                || Err(refusal(code, false)),
                no_transcript,
                || true,
                |_| panic!("{code}: not read again"),
                attempt,
            );
            assert!(
                matches!(said, Some(Update::RecoveryFailed { .. })),
                "{code}"
            );
        }
    }

    /// **A failure the service says will pass is waited through, on a
    /// bounded clock** — an ordinary chat's recovery keeps its behaviour
    /// for a lost connection — 250 ms doubling to 2 s rather than a fixed
    /// 250 ms; an attempt the service authoritatively does not have still
    /// ends it with the recovery's own sentence.
    #[test]
    fn a_retryable_failure_is_read_again_on_a_bounded_clock() {
        let mut reads = 0;
        let mut pauses = Vec::new();
        let said = recover(
            || {
                reads += 1;
                match reads {
                    1 | 2 => Err(refusal("gateway_unavailable", true)),
                    3 | 4 => Ok(koru_ai_core::AttemptStatus::Streaming),
                    5 => Err(refusal("gateway_unavailable", true)),
                    _ => Err(refusal("not_found", false)),
                }
            },
            no_transcript,
            || true,
            |wait| pauses.push(wait),
            koru_ai_core::InferenceAttemptId::new(),
        );
        assert_eq!(reads, 6);
        let ms = |n| std::time::Duration::from_millis(n);
        assert_eq!(pauses, [ms(250), ms(500), ms(1000), ms(2000), ms(2000)]);
        assert_eq!((RECOVERY_FIRST, RECOVERY_MAX), (ms(250), ms(2000)));
        let Some(Update::RecoveryFailed { failure, .. }) = said else {
            panic!("ended");
        };
        // The surface says it as the recovery failing.
        assert!(authoritatively_missing(&failure));
    }

    /// **Nobody reading the answer ends a recovery**: once the surface's
    /// reader is gone — a Bot tab let go of its chat, a window closed — the
    /// recovery thread stops before its next read rather than polling for a
    /// receiver that no longer exists.
    #[test]
    fn a_recovery_stops_when_nobody_reads_its_answer() {
        let reads = std::cell::Cell::new(0);
        let said = recover(
            || {
                reads.set(reads.get() + 1);
                Ok(koru_ai_core::AttemptStatus::Streaming)
            },
            no_transcript,
            || reads.get() < 3,
            |_| {},
            koru_ai_core::InferenceAttemptId::new(),
        );
        assert!(said.is_none(), "nothing to say to nobody");
        assert_eq!(reads.get(), 3, "no read after the reader went");
    }
}
