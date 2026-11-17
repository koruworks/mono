// SPDX-License-Identifier: MPL-2.0

//! The copy as it is written and as it is read: a page of an owner's feed
//! turned into rows ([`page`]), and the linked computers' Bots as the
//! routing port asks for them ([`koru_ai_client::LinkedBots`]).
//!
//! **Reads are answered from the copy and from nothing else.** A read
//! never dials, and it answers the same whether the owner is reachable or
//! not; how current the copy is travels with it (`state`,
//! `last_confirmed_at_ms`). The admission port is asked before each read,
//! so the copy of an owner it stopped naming is not read while it is being
//! deleted.
//!
//! **A change is sent, never applied here.** The four changes of a Bot the
//! owner takes from a linked computer (pause, resume, the settings it may
//! change, another conversation) and the conversation's own (a message, a
//! retry, Stop, a steer, an answer to "Needs you") go to the owner as
//! commands. Any other change of a copied Bot is `executing_computer_only`,
//! and a read of something a copy does not hold is `not_found`. Nothing
//! edits a copy to look as if a command had been applied: the copy changes
//! when the owner's feed says so.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use koru_ai_api::{AiResponseStream, AiScope, RequestContext};
use koru_ai_client::{LinkedBots, LinkedSubject};
use koru_ai_core::bots::{
    Bot, BotChatCursor, BotChatLabel, BotChatPage, BotCommand, BotConversation, BotId,
    BotRequestId, BotResponse, DeviceId, LinkedBotsView, LinkedOwnerState, LinkedOwnerView,
    TeamPauseView,
};
use koru_ai_core::{
    AiPersistence, ChatId, ChatToolPolicy, InferenceAttemptId, MessageId, MessageState,
    ToolInvocationId, ToolMode,
};
use koru_ai_sqlite::{MirrorChange, MirrorPage, MirrorPlace, MirrorSubject};
use koru_ai_wire::{
    CancelAttemptResponse, ChatTranscript, Failure, RetryTurnRequest, SendTurnRequest,
    SteerAttemptRequest, SteerAttemptResponse, ToolDecisionRequest, ToolDecisionResponse,
};
use koru_bot_client_wire::{
    BotRecord, Changes, ChatRecord, ClientCommand, ClientQuery, MessageRecord, REPLICA_FORMAT,
    Record, TeamPauseRecord, entry_kind,
};
use serde::Serialize;
use uuid::Uuid;

use super::send::answer;
use super::{BotMirror, Shared, lock};
use crate::worker::bots::bot_failure;
use crate::worker::bots::lifecycle::bot_read_command;
use crate::worker::{authorize, failure, not_found};

/// The replies and pending calls a page's records name, with their
/// conversation.
#[derive(Default)]
pub(super) struct Learned {
    pub(super) attempts: Vec<(InferenceAttemptId, ChatId)>,
    pub(super) tools: Vec<(ToolInvocationId, ChatId)>,
}

impl Learned {
    /// A reply that is still being written can be stopped or steered, and
    /// its pending calls answered. A reply that ended can take neither.
    fn from_message(&mut self, record: &MessageRecord) {
        if record.message.state != MessageState::Pending {
            return;
        }
        let chat = record.message.chat_id;
        for activity in &record.tool_activities {
            self.tools.push((activity.id, chat));
            self.attempts.push((activity.attempt_id, chat));
        }
        for notice in &record.notices {
            self.attempts.push((notice.attempt_id, chat));
        }
    }
}

/// What a stored message record names, read again at start.
pub(super) fn learn_stored(record: &str) -> Learned {
    let mut learned = Learned::default();
    if let Some(record) = message(record) {
        learned.from_message(&record);
    }
    learned
}

pub(super) fn message(record: &str) -> Option<MessageRecord> {
    serde_json::from_str(record).ok()
}

fn bot(record: &str) -> Option<Bot> {
    serde_json::from_str::<BotRecord>(record)
        .ok()
        .map(|record| record.bot)
}

fn chat(record: &str) -> Option<ChatRecord> {
    serde_json::from_str(record).ok()
}

/// A value that serializes as one word, as that word: a lifecycle, a
/// purpose.
fn word<T: Serialize>(value: &T) -> Result<String, ()> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(word)) => Ok(word),
        _ => Err(()),
    }
}

fn json<T: Serialize>(value: &T) -> Result<String, ()> {
    serde_json::to_string(value).map_err(|_| ())
}

/// One page of `owner`'s feed as the rows it writes, or `Err` when a
/// record is not its entry's (contract 5.2, "validation before a row is
/// written"):
///
/// - a record is in the format this build reads and of its entry's kind;
/// - a Bot's, a conversation's and a message's id is the entry's id;
/// - a conversation's Bot and a message's conversation are the entry's;
/// - a Bot and a team pause name this owner, and no other computer;
/// - a private conversation is never accepted.
///
/// The transport checks of the page (its cursors, its feed and start) were
/// made before this; the checks only the database can make are made when
/// the rows are written.
pub(super) fn page(
    owner: DeviceId,
    protocol_version: u32,
    page: &Changes,
) -> Result<(MirrorPage, Learned), ()> {
    let mut changes = Vec::with_capacity(page.entries.len());
    let mut learned = Learned::default();
    for entry in &page.entries {
        let seq = entry.seq;
        let change = match (&entry.record, entry.removed) {
            (None, true) => match entry.kind.as_str() {
                entry_kind::BOT => MirrorChange::RemoveBot {
                    bot_id: entry.id.parse().map_err(|_| ())?,
                },
                entry_kind::CHAT => MirrorChange::RemoveChat {
                    chat_id: ChatId::parse(&entry.id).ok_or(())?,
                },
                entry_kind::MESSAGE => MirrorChange::RemoveMessage {
                    message_id: MessageId::parse(&entry.id).ok_or(())?,
                },
                entry_kind::TEAM_PAUSE => MirrorChange::RemoveTeam,
                _ => return Err(()),
            },
            (Some(record), false) => {
                if record.format() != REPLICA_FORMAT || record.kind() != entry.kind {
                    return Err(());
                }
                match record {
                    Record::Bot(record) => {
                        let named: BotId = entry.id.parse().map_err(|_| ())?;
                        if record.bot.id != named || record.bot.owner_device != owner {
                            return Err(());
                        }
                        MirrorChange::Bot {
                            seq,
                            bot_id: named,
                            home_workspace: record.bot.home_workspace,
                            lifecycle: word(&record.bot.lifecycle)?,
                            revision: record.bot.revision,
                            record_json: json(record)?,
                        }
                    }
                    Record::Chat(record) => {
                        let named = ChatId::parse(&entry.id).ok_or(())?;
                        let of: BotId =
                            entry.bot_id.as_deref().ok_or(())?.parse().map_err(|_| ())?;
                        if record.chat.id != named || record.bot_id != of || record.chat.private {
                            return Err(());
                        }
                        MirrorChange::Chat {
                            seq,
                            chat_id: named,
                            bot_id: of,
                            purpose: word(&record.purpose)?,
                            trashed: record.trashed,
                            updated_at_ms: record.chat.updated_at.millis(),
                            record_json: json(record)?,
                        }
                    }
                    Record::Message(record) => {
                        let named = MessageId::parse(&entry.id).ok_or(())?;
                        let of = ChatId::parse(entry.chat_id.as_deref().ok_or(())?).ok_or(())?;
                        if record.message.id != named || record.message.chat_id != of {
                            return Err(());
                        }
                        let bot_id = entry
                            .bot_id
                            .as_deref()
                            .map(|bot| bot.parse::<BotId>().map_err(|_| ()))
                            .transpose()?;
                        learned.from_message(record);
                        MirrorChange::Message {
                            seq,
                            message_id: named,
                            chat_id: of,
                            bot_id,
                            ordinal: record.message.ordinal,
                            record_json: json(record)?,
                        }
                    }
                    Record::TeamPause(record) => {
                        if record.team.owner_device != owner {
                            return Err(());
                        }
                        MirrorChange::Team {
                            seq,
                            record_json: json(record)?,
                        }
                    }
                }
            }
            _ => return Err(()),
        };
        changes.push(change);
    }
    Ok((
        MirrorPage {
            owner_device: owner,
            feed_id: page.feed_id.clone(),
            boot_id: page.boot_id.clone(),
            protocol_version,
            replica_format: REPLICA_FORMAT,
            from_cursor: page.from_cursor,
            to_cursor: page.to_cursor,
            reset: page.reset,
            caught_up: page.caught_up.then(|| page.withheld_bots.unwrap_or(0)),
            changes,
        },
        learned,
    ))
}

/// A read of something a copy does not hold.
fn not_in_copy() -> Failure {
    failure(
        "not_found",
        "This is kept on the computer that runs the Bot.",
        false,
    )
}

/// The Bot a command names, read from the command as it is written: the
/// member every command that names one calls `bot_id`.
fn named_bot(command: &BotCommand) -> Option<BotId> {
    serde_json::to_value(command)
        .ok()?
        .get("bot_id")?
        .as_str()?
        .parse()
        .ok()
}

/// What a lookup names.
#[derive(Clone, Copy)]
enum Subject {
    Bot(BotId),
    Chat(ChatId),
    Message(MessageId),
    Attempt(InferenceAttemptId),
    Tool(ToolInvocationId),
}

impl Shared {
    /// The owner whose visible copy holds `subject`, and the conversation
    /// it is or belongs to, or `None` when no copy does. This computer's
    /// own tables are looked at first, so a copy never takes a call meant
    /// for a Bot that runs here.
    async fn place(&self, subject: Subject) -> Option<(DeviceId, Option<ChatId>)> {
        // What the last run left is looked at before the first answer.
        let mut ready = self.ready.subscribe();
        if !*ready.borrow_and_update() {
            let _ = tokio::time::timeout(self.timing.hello, ready.changed()).await;
        }
        if self.copies.load(Ordering::Acquire) == 0 {
            return None;
        }
        let (owner, chat) = match subject {
            Subject::Bot(_) | Subject::Chat(_) | Subject::Message(_) => {
                let subject = match subject {
                    Subject::Bot(bot) => MirrorSubject::Bot(bot),
                    Subject::Chat(chat) => MirrorSubject::Chat(chat),
                    Subject::Message(message) => MirrorSubject::Message(message),
                    _ => return None,
                };
                match self
                    .link
                    .stored(move |state| state.repository.mirror_place(subject))
                    .await?
                {
                    MirrorPlace::Linked {
                        owner_device,
                        chat_id,
                    } => (owner_device, chat_id),
                    MirrorPlace::Local | MirrorPlace::Nowhere => return None,
                }
            }
            Subject::Attempt(attempt) => {
                let (owner, chat) = lock(&self.index).attempts.get(&attempt).copied()?;
                // A reply of this computer is this computer's, whatever a
                // copy says.
                let local = self
                    .link
                    .on_writer(move |state| !matches!(state.repository.attempt(attempt), Ok(None)))
                    .await
                    .ok()?;
                if local {
                    return None;
                }
                (owner, Some(chat))
            }
            Subject::Tool(tool) => {
                let (owner, chat) = lock(&self.index).tools.get(&tool).copied()?;
                let local = self
                    .link
                    .on_writer(move |state| {
                        !matches!(state.repository.tool_activity(tool), Ok(None))
                    })
                    .await
                    .ok()?;
                if local {
                    return None;
                }
                (owner, Some(chat))
            }
        };
        self.visible(&owner).then_some((owner, chat))
    }

    /// Every owner this computer keeps a copy of, with its team pause.
    async fn owners(&self) -> Result<Vec<LinkedOwnerView>, Failure> {
        let rows = self
            .link
            .stored(|state| {
                state
                    .repository
                    .mirror_owners()?
                    .into_iter()
                    .map(|owner| {
                        let team = state.repository.mirror_team(owner.owner_device)?;
                        Ok((owner, team))
                    })
                    .collect::<Result<Vec<_>, koru_ai_core::bots::BotError>>()
            })
            .await
            .ok_or_else(|| bot_failure(koru_ai_core::bots::BotError::Unavailable))?;
        Ok(rows
            .into_iter()
            .filter(|(owner, _)| self.visible(&owner.owner_device))
            .map(|(owner, team)| LinkedOwnerView {
                owner_device: owner.owner_device,
                // This computer keeps no name of another one here; the
                // window names its linked devices from its own list.
                display_name: None,
                state: owner.state,
                last_confirmed_at_ms: owner.last_confirmed_at_ms,
                team: team
                    .and_then(|record| serde_json::from_str::<TeamPauseRecord>(&record).ok())
                    .map(|record| record.team),
                withheld_bots: owner.withheld_bots,
            })
            .collect())
    }

    /// One owner's Bots as last received.
    async fn bots_of(
        &self,
        owner: DeviceId,
        workspace: Option<koru_ai_core::WorkspaceId>,
    ) -> Result<LinkedBotsView, Failure> {
        let nothing = LinkedBotsView {
            owner_device: owner,
            state: LinkedOwnerState::Never,
            last_confirmed_at_ms: None,
            bots: Vec::new(),
        };
        if !self.visible(&owner) {
            return Ok(nothing);
        }
        let read = self
            .link
            .stored(move |state| {
                let Some(row) = state.repository.mirror_owner(owner)? else {
                    return Ok(None);
                };
                Ok(Some((row, state.repository.mirror_bots(owner, workspace)?)))
            })
            .await
            .ok_or_else(|| bot_failure(koru_ai_core::bots::BotError::Unavailable))?;
        Ok(match read {
            Some((row, _)) if row.state == LinkedOwnerState::Withdrawing => nothing,
            Some((row, records)) => LinkedBotsView {
                owner_device: owner,
                state: row.state,
                last_confirmed_at_ms: row.last_confirmed_at_ms,
                bots: records.iter().filter_map(|record| bot(record)).collect(),
            },
            None => nothing,
        })
    }

    async fn copied_bot(&self, owner: DeviceId, id: BotId) -> Result<Bot, Failure> {
        self.link
            .stored(move |state| state.repository.mirror_bot(owner, id))
            .await
            .flatten()
            .and_then(|record| bot(&record))
            .ok_or_else(not_found)
    }

    /// One page of a copied Bot's conversations outside Trash, in the
    /// order the owner lists them.
    async fn chats_of(
        &self,
        owner: DeviceId,
        id: BotId,
        cursor: Option<BotChatCursor>,
        limit: u16,
    ) -> Result<BotChatPage, Failure> {
        if !(1..=100).contains(&limit) {
            return Err(failure(
                "invalid_request",
                "A page holds 1 to 100 conversations.",
                false,
            ));
        }
        let of = self.copied_bot(owner, id).await?;
        let records = self
            .link
            .stored(move |state| state.repository.mirror_chats(owner, id, false))
            .await
            .ok_or_else(not_found)?;
        let all: Vec<ChatRecord> = records.iter().filter_map(|record| chat(record)).collect();
        let start = match cursor {
            None => 0,
            Some(cursor) => all
                .iter()
                .position(|record| record.chat.id == cursor.chat_id)
                .map_or(all.len(), |index| index + 1),
        };
        let limit = usize::from(limit);
        let more = all.len() > start + limit;
        let chats: Vec<BotConversation> = all
            .into_iter()
            .skip(start)
            .take(limit)
            .map(|record| conversation(&of, record))
            .collect();
        let next_cursor = more
            .then(|| chats.last())
            .flatten()
            .map(|last| BotChatCursor {
                main: word(&last.label.purpose).is_ok_and(|purpose| purpose == "main"),
                updated_at_ms: last.chat.updated_at.millis(),
                chat_id: last.chat.id,
            });
        Ok(BotChatPage { chats, next_cursor })
    }

    /// A copied conversation as the transcript a window reads.
    async fn transcript(&self, owner: DeviceId, id: ChatId) -> Result<ChatTranscript, Failure> {
        let (record, records) = self
            .link
            .stored(move |state| {
                Ok((
                    state.repository.mirror_chat(owner, id)?,
                    state.repository.mirror_messages(owner, id)?,
                ))
            })
            .await
            .ok_or_else(not_found)?;
        let record = record
            .and_then(|record| chat(&record))
            .ok_or_else(not_found)?;
        let of = self.copied_bot(owner, record.bot_id).await?;
        let mut transcript = ChatTranscript {
            chat: conversation(&of, record).chat,
            // A copy does not hold the conversation's tool mode or grants.
            tool_policy: ChatToolPolicy {
                chat_id: id,
                mode: ToolMode::default(),
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
        };
        for record in records.iter().filter_map(|record| message(record)) {
            transcript.messages.push(record.message);
            transcript.blocks.extend(record.blocks);
            transcript.tool_activities.extend(record.tool_activities);
            transcript.notices.extend(record.notices);
            transcript.feedback.extend(record.feedback);
            transcript.usage.extend(record.usage);
            transcript.sources.extend(record.sources);
        }
        Ok(transcript)
    }
}

/// A copied conversation with the label its owner's own reads give it. A
/// record carries no label, so it is filled from the copied Bot.
fn conversation(of: &Bot, record: ChatRecord) -> BotConversation {
    let label = BotChatLabel {
        bot_id: of.id,
        bot_name: of.config.name.clone(),
        purpose: record.purpose,
    };
    let mut chat = record.chat;
    chat.bot = Some(label.clone());
    BotConversation { chat, label }
}

impl BotMirror {
    /// Run `work` on the mirror's runtime and wait for it, as a caller of
    /// the local worker waits for the worker. A caller that waited as long
    /// as it may is told the outcome is not confirmed: whatever was sent
    /// is still settled by the mirror.
    fn wait<T: Send + 'static>(
        &self,
        work: impl Future<Output = Result<T, Failure>> + Send + 'static,
    ) -> Result<T, Failure> {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        self.shared.runtime.spawn(async move {
            let _ = reply.send(work.await);
        });
        answer
            .recv_timeout(self.shared.timing.call)
            .unwrap_or_else(|_| {
                Err(super::unreachable(
                    koru_ai_wire::bot_linked_codes::DELIVERY_NOT_CONFIRMED,
                ))
            })
    }

    /// Whether any copy could hold anything, without reading anything.
    fn nothing_copied(&self) -> bool {
        *self.shared.ready.borrow() && self.shared.copies.load(Ordering::Acquire) == 0
    }

    fn placed(&self, subject: Subject) -> Option<(DeviceId, Option<ChatId>)> {
        if self.nothing_copied() {
            return None;
        }
        let shared = Arc::clone(&self.shared);
        self.wait(async move { Ok(shared.place(subject).await) })
            .ok()
            .flatten()
    }

    /// The owner of a copied conversation, or `not_found`.
    fn owner_of_chat(&self, chat: ChatId) -> Result<DeviceId, Failure> {
        self.placed(Subject::Chat(chat))
            .map(|(owner, _)| owner)
            .ok_or_else(not_found)
    }
}

impl LinkedBots for BotMirror {
    fn holds(&self, subject: LinkedSubject) -> bool {
        self.placed(match subject {
            LinkedSubject::Chat(chat) => Subject::Chat(chat),
            LinkedSubject::Attempt(attempt) => Subject::Attempt(attempt),
            LinkedSubject::Tool(tool) => Subject::Tool(tool),
            LinkedSubject::Message(message) => Subject::Message(message),
        })
        .is_some()
    }

    fn takes(&self, command: &BotCommand) -> bool {
        match command {
            BotCommand::ListLinkedOwners {} | BotCommand::ListLinkedBots { .. } => true,
            // With no copy nothing is read, and the command is not even
            // looked into for the Bot it names.
            command => {
                !self.nothing_copied()
                    && named_bot(command)
                        .is_some_and(|bot| self.placed(Subject::Bot(bot)).is_some())
            }
        }
    }

    fn bots(&self, context: RequestContext, command: BotCommand) -> Result<BotResponse, Failure> {
        authorize(&context, AiScope::ManageBots)?;
        let shared = Arc::clone(&self.shared);
        self.wait(async move {
            let command = match command {
                BotCommand::ListLinkedOwners {} => {
                    return Ok(BotResponse::LinkedOwners {
                        owners: shared.owners().await?,
                    });
                }
                BotCommand::ListLinkedBots {
                    owner_device,
                    workspace,
                } => {
                    return Ok(BotResponse::LinkedBots {
                        linked: shared.bots_of(owner_device, workspace).await?,
                    });
                }
                command => command,
            };
            let named = named_bot(&command).ok_or_else(not_found)?;
            let (owner, _) = shared
                .place(Subject::Bot(named))
                .await
                .ok_or_else(not_found)?;
            // The changes the owner takes from a linked computer.
            let change = match &command {
                BotCommand::Pause {
                    request_id,
                    bot_id,
                    expected_revision,
                } => Some(ClientCommand::PauseBot {
                    request_id: *request_id,
                    bot_id: *bot_id,
                    expected_revision: *expected_revision,
                }),
                BotCommand::Resume {
                    request_id,
                    bot_id,
                    expected_revision,
                } => Some(ClientCommand::ResumeBot {
                    request_id: *request_id,
                    bot_id: *bot_id,
                    expected_revision: *expected_revision,
                }),
                BotCommand::Update {
                    request_id,
                    bot_id,
                    expected_revision,
                    config,
                } => Some(ClientCommand::UpdateBot {
                    request_id: *request_id,
                    bot_id: *bot_id,
                    expected_revision: *expected_revision,
                    config: config.clone(),
                }),
                BotCommand::CreateChat { request_id, bot_id } => Some(ClientCommand::CreateChat {
                    request_id: *request_id,
                    bot_id: *bot_id,
                }),
                _ => None,
            };
            if let Some(change) = change {
                let request = change.embedded_request_id().unwrap_or_else(Uuid::new_v4);
                let receipt = shared.command(owner, request, change).await?;
                shared.settled(owner, &receipt).await;
                return answer(receipt);
            }
            match command {
                BotCommand::Read { bot_id } => Ok(BotResponse::Bot {
                    bot: shared.copied_bot(owner, bot_id).await?,
                }),
                BotCommand::ListBotChats {
                    bot_id,
                    cursor,
                    limit,
                } => Ok(BotResponse::Chats {
                    page: shared.chats_of(owner, bot_id, cursor, limit).await?,
                }),
                // The two reads the owner answers itself.
                BotCommand::ReadRun {
                    bot_id,
                    chat_id,
                    message_id,
                    run_id,
                    from,
                    len,
                } => {
                    shared
                        .query(
                            owner,
                            ClientQuery::ReadRun {
                                bot_id,
                                chat_id,
                                message_id,
                                run_id,
                                from,
                                len,
                            },
                        )
                        .await
                }
                BotCommand::ReadOperation { request_id, bot_id } => {
                    shared
                        .query(owner, ClientQuery::ReadOperation { request_id, bot_id })
                        .await
                }
                // Anything else of a copied Bot: a read of what a copy
                // does not hold, or a change only its own computer makes.
                command if bot_read_command(&command) => Err(not_in_copy()),
                _ => Err(koru_ai_client::executing_computer_only()),
            }
        })
    }

    fn read_chat(&self, context: RequestContext, chat: ChatId) -> Result<ChatTranscript, Failure> {
        authorize(&context, AiScope::Chat)?;
        let owner = self.owner_of_chat(chat)?;
        let shared = Arc::clone(&self.shared);
        self.wait(async move { shared.transcript(owner, chat).await })
    }

    fn send_turn(
        &self,
        context: RequestContext,
        request: SendTurnRequest,
    ) -> Result<AiResponseStream, Failure> {
        authorize(&context, AiScope::Chat)?;
        let chat = request.chat_id;
        let owner = self.owner_of_chat(chat)?;
        let shared = Arc::clone(&self.shared);
        self.wait(async move {
            let operation = request.operation_id;
            shared
                .turn(owner, chat, operation, ClientCommand::SendTurn { request })
                .await
        })
    }

    fn retry_turn(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: RetryTurnRequest,
    ) -> Result<AiResponseStream, Failure> {
        authorize(&context, AiScope::Chat)?;
        let owner = self.owner_of_chat(chat)?;
        let shared = Arc::clone(&self.shared);
        self.wait(async move {
            let operation = request.operation_id;
            shared
                .turn(
                    owner,
                    chat,
                    operation,
                    ClientCommand::RetryTurn {
                        chat_id: chat,
                        request,
                    },
                )
                .await
        })
    }

    fn cancel_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
    ) -> Result<CancelAttemptResponse, Failure> {
        authorize(&context, AiScope::Chat)?;
        let (owner, _) = self
            .placed(Subject::Attempt(attempt))
            .ok_or_else(not_found)?;
        let shared = Arc::clone(&self.shared);
        self.wait(async move {
            let command = ClientCommand::CancelAttempt {
                attempt_id: attempt,
            };
            answer(shared.command(owner, Uuid::new_v4(), command).await?)
        })
    }

    fn steer_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
        request: SteerAttemptRequest,
    ) -> Result<SteerAttemptResponse, Failure> {
        authorize(&context, AiScope::Chat)?;
        let (owner, _) = self
            .placed(Subject::Attempt(attempt))
            .ok_or_else(not_found)?;
        let shared = Arc::clone(&self.shared);
        self.wait(async move {
            let operation = request.operation_id;
            let command = ClientCommand::Steer {
                attempt_id: attempt,
                request,
            };
            answer(shared.command(owner, operation, command).await?)
        })
    }

    fn decide_tool_call(
        &self,
        context: RequestContext,
        tool: ToolInvocationId,
        approve: bool,
        decision: ToolDecisionRequest,
    ) -> Result<ToolDecisionResponse, Failure> {
        authorize(&context, AiScope::Chat)?;
        let (owner, _) = self.placed(Subject::Tool(tool)).ok_or_else(not_found)?;
        let shared = Arc::clone(&self.shared);
        self.wait(async move {
            let command = ClientCommand::AnswerDecision {
                tool_id: tool,
                approve,
                decision,
            };
            let receipt = shared.command(owner, Uuid::new_v4(), command).await?;
            shared.settled(owner, &receipt).await;
            answer(receipt)
        })
    }

    /// The public team commands name no computer and reach this computer's
    /// own team only; this is how a window reaches another one's (D4). The
    /// owner must be one this computer keeps a copy of, and connected.
    fn team_pause(
        &self,
        context: RequestContext,
        owner: DeviceId,
        pause: bool,
        request_id: BotRequestId,
        expected_team_revision: u64,
    ) -> Result<TeamPauseView, Failure> {
        authorize(&context, AiScope::ManageBots)?;
        let command = if pause {
            ClientCommand::PauseTeam {
                request_id,
                expected_team_revision,
            }
        } else {
            ClientCommand::ResumeTeam {
                request_id,
                expected_team_revision,
            }
        };
        let shared = Arc::clone(&self.shared);
        self.wait(async move {
            let receipt = shared.command(owner, request_id.as_uuid(), command).await?;
            shared.settled(owner, &receipt).await;
            match answer::<BotResponse>(receipt)? {
                BotResponse::TeamPause { team } => Ok(team),
                _ => Err(not_in_copy()),
            }
        })
    }
}
