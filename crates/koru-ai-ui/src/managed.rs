// SPDX-License-Identifier: MPL-2.0
//! Presentation state for a conversation whose engine owns its turns.
//! Input acceptance, live observations and published messages are separate
//! facts. This module opens no service and creates no inference-attempt ids.
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use koru_ai_core::{ChatId, ChatRole, Message, MessageId, MessageState, Timestamp};
use koru_ai_wire::{ChatTranscript, ProviderSettingsResponse, bot_threads as wire};
use uuid::Uuid;

pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_millis(500);
const MAX_LIVE_BYTES: usize = 1024 * 1024;

pub(crate) struct Opened {
    pub snapshot: wire::Snapshot,
    pub messages: Vec<wire::Message>,
    pub before: Option<u64>,
    pub after: u64,
    pub settings: Option<ProviderSettingsResponse>,
    pub questions: Vec<wire::Question>,
    pub attention: crate::managed_attention::Page,
}

#[cfg(test)]
pub(crate) mod tests;
pub(crate) struct Feed {
    pub boot: Uuid,
    pub cursor: u64,
    pub gap: bool,
    pub entries: Vec<wire::Event>,
}
pub(crate) struct Refreshed {
    pub snapshot: wire::Snapshot,
    pub messages: Vec<wire::Message>,
    pub after: u64,
    pub feed: Feed,
    pub questions: Vec<wire::Question>,
    pub attention: crate::managed_attention::Page,
}
pub(crate) struct Poll {
    pub thread: ChatId,
    pub after: u64,
    pub boot: Option<Uuid>,
    pub cursor: u64,
    pub attention_after: Option<wire::OperationRef>,
}

pub(crate) struct Live {
    pub turn: wire::Turn,
    pub reply: MessageId,
    pub bot: Uuid,
    pub text: String,
    /// False after a gap until a genuine turn-start observation establishes
    /// that subsequent deltas include the beginning of this reply.
    pub complete_prefix: bool,
    pub markdown: crate::live_markdown::LiveAnswer,
}

pub(crate) struct State {
    pub snapshot: wire::Snapshot,
    pub attention: crate::managed_attention::State,
    pub questions: Vec<wire::Question>,
    pub requests_dirty: bool,
    pub messages: BTreeMap<u64, wire::Message>,
    /// Local rendering blocks, kept stable between deltas. These ids are not
    /// engine history identities and must never be used as durable links.
    blocks: BTreeMap<u64, Vec<koru_ai_core::MessageBlock>>,
    pub live: HashMap<Uuid, Live>,
    pub before: Option<u64>,
    pub history_pending: bool,
    pub history_anchor: Option<(MessageId, f32)>,
    pub after: u64,
    pub boot: Option<Uuid>,
    pub cursor: u64,
    pub gap: bool,
    pub poll_at: Instant,
    pub polling: bool,
    pub history_changed: bool,
    pub pending_send: Option<wire::Command>,
    pub retry_send: bool,
    pub retry_at: Instant,
}

macro_rules! redacted {
    ($($ty:ty),+ $(,)?) => { $(impl std::fmt::Debug for $ty {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("ManagedConversation(<redacted>)")
        }
    })+ };
}
redacted!(Opened, Feed, Refreshed, Poll, State);

impl State {
    pub fn new(opened: &mut Opened, now: Instant) -> Self {
        let blocks = opened
            .messages
            .iter()
            .map(|m| (m.sequence, render_blocks(m)))
            .collect();
        Self {
            snapshot: opened.snapshot.clone(),
            attention: crate::managed_attention::State::new(std::mem::take(&mut opened.attention)),
            questions: std::mem::take(&mut opened.questions),
            requests_dirty: true,
            messages: std::mem::take(&mut opened.messages)
                .into_iter()
                .map(|m| (m.sequence, m))
                .collect(),
            blocks,
            live: HashMap::new(),
            before: opened.before,
            history_pending: false,
            history_anchor: None,
            after: opened.after,
            boot: None,
            cursor: 0,
            gap: false,
            poll_at: now,
            polling: false,
            history_changed: true,
            pending_send: None,
            retry_send: false,
            retry_at: now,
        }
    }
    pub fn thread(&self) -> ChatId {
        self.snapshot.conversation.chat.id
    }
    pub fn poll(&mut self, now: Instant) -> Option<Poll> {
        if self.polling || now < self.poll_at {
            return None;
        }
        self.polling = true;
        Some(Poll {
            thread: self.thread(),
            after: self.after,
            boot: self.boot,
            cursor: self.cursor,
            attention_after: self.attention.next.clone(),
        })
    }
    pub fn settled(&mut self, now: Instant) {
        self.polling = false;
        self.poll_at = now + REFRESH_INTERVAL;
    }
    pub fn primary_turn(&self) -> Option<wire::Turn> {
        let bot = self.snapshot.conversation.label.bot_id.as_uuid();
        self.snapshot
            .turns
            .iter()
            .find(|t| {
                t.bot == bot && matches!(t.state.as_str(), "running" | "needs_you" | "interrupted")
            })
            .and_then(|t| {
                Some(wire::Turn {
                    id: t.id,
                    generation: t.generation,
                    worker: t.worker?,
                    session: t.session?,
                })
            })
    }
    pub fn running(&self) -> bool {
        self.snapshot.turns.iter().any(|t| t.state == "running")
    }

    pub fn refresh(&mut self, mut update: Refreshed, now: Instant) -> bool {
        if update.snapshot.conversation.chat.id != self.thread() {
            return false;
        }
        self.settled(now);
        // A read already in flight may finish after a settings receipt.
        // It cannot roll the accepted model/mode revision back.
        if update.snapshot.settings_revision < self.snapshot.settings_revision {
            update.snapshot.settings_revision = self.snapshot.settings_revision;
            update.snapshot.conversation.chat.current_model =
                self.snapshot.conversation.chat.current_model.clone();
            update.snapshot.conversation.chat.agent_mode =
                self.snapshot.conversation.chat.agent_mode.clone();
            update.snapshot.conversation.chat.agent_fast =
                self.snapshot.conversation.chat.agent_fast;
        }
        self.history_changed = false;
        let old = &self.snapshot;
        let new = &update.snapshot;
        let mut changed = old.conversation.chat != new.conversation.chat
            || old.waiting_messages != new.waiting_messages
            || old.paused != new.paused
            || old.team_paused != new.team_paused
            || old.turns.len() != new.turns.len()
            || old.turns.iter().zip(&new.turns).any(|(a, b)| {
                a.id != b.id
                    || a.state != b.state
                    || a.generation != b.generation
                    || a.worker != b.worker
                    || a.session != b.session
                    || a.reply_message != b.reply_message
                    || a.details != b.details
            });
        self.snapshot = update.snapshot;
        changed |= self.attention.ingest(update.attention);
        self.requests_dirty |= self.questions != update.questions;
        changed |= self.questions != update.questions;
        self.questions = update.questions;
        for message in update.messages {
            self.history_changed |= self.ingest(message);
        }
        changed |= self.history_changed;
        self.requests_dirty |= self.history_changed;
        self.after = self.after.max(update.after);
        let count = self.live.len();
        self.live.retain(|id, live| {
            self.snapshot.turns.iter().any(|t| {
                t.id == *id
                    && t.generation == live.turn.generation
                    && t.worker == Some(live.turn.worker)
                    && t.session == Some(live.turn.session)
            }) && !self.messages.values().any(|m| m.id == live.reply.as_uuid())
        });
        changed |= count != self.live.len();
        changed |= self.observe(update.feed);
        changed
    }

    fn observe(&mut self, feed: Feed) -> bool {
        let reset = feed.gap || self.boot != Some(feed.boot);
        let mut changed = false;
        if reset {
            changed = !self.live.is_empty();
            self.live.clear();
            self.cursor = 0;
        }
        self.boot = Some(feed.boot);
        let previous_count = self.live.len();
        for current in &self.snapshot.turns {
            let (Some(reply), Some(worker), Some(session)) =
                (current.reply_message, current.worker, current.session)
            else {
                continue;
            };
            if self.messages.values().any(|m| m.id == reply) {
                continue;
            }
            self.live.entry(current.id).or_insert_with(|| Live {
                turn: wire::Turn {
                    id: current.id,
                    generation: current.generation,
                    worker,
                    session,
                },
                reply: MessageId::from_uuid(reply),
                bot: current.bot,
                text: String::new(),
                complete_prefix: false,
                markdown: Default::default(),
            });
        }
        changed |= previous_count != self.live.len();
        for event in feed.entries {
            if event.cursor <= self.cursor {
                continue;
            }
            self.cursor = event.cursor;
            let observation = event.observation;
            if observation.thread != self.thread().as_uuid() {
                continue;
            }
            let turn = observation.turn;
            let Some(current) = self.snapshot.turns.iter().find(|t| {
                t.id == turn.id
                    && t.bot == observation.bot
                    && t.generation == turn.generation
                    && t.worker == Some(turn.worker)
                    && t.session == Some(turn.session)
            }) else {
                continue;
            };
            let Some(reply) = current.reply_message else {
                continue;
            };
            if self.messages.values().any(|m| m.id == reply) {
                continue;
            }
            let live = self.live.entry(turn.id).or_insert_with(|| Live {
                turn,
                reply: MessageId::from_uuid(reply),
                bot: observation.bot,
                text: String::new(),
                complete_prefix: false,
                markdown: Default::default(),
            });
            match observation.event {
                wire::AgentEvent::TurnStarted { .. } => {
                    live.complete_prefix = true;
                }
                wire::AgentEvent::MessageDelta { text, .. } => {
                    if live.complete_prefix
                        && live.text.len().saturating_add(text.len()) <= MAX_LIVE_BYTES
                    {
                        live.text.push_str(&text);
                        changed |= !text.is_empty();
                    } else {
                        changed |= !live.text.is_empty();
                        live.text.clear();
                        live.complete_prefix = false;
                    }
                }
                _ => {}
            }
        }
        self.cursor = self.cursor.max(feed.cursor);
        let gap = self.live.values().any(|live| !live.complete_prefix);
        changed |= self.gap != gap;
        self.gap = gap;
        changed
    }

    /// This is a rendering projection. No ordinary AI repository is written,
    /// and provider/model facts absent from the engine history stay absent.
    pub fn transcript(&self) -> ChatTranscript {
        let messages = self
            .messages
            .values()
            .filter(|m| {
                m.kind == "words"
                    && m.sender != "engine"
                    && !(m.content.body.is_empty()
                        && m.content.references.iter().any(|r| {
                            r.get("answer").is_some()
                                || (!m.sender.starts_with("person:")
                                    && r.get("notice").and_then(|v| v.as_str())
                                        == Some("turn_stopped"))
                        }))
            })
            .enumerate()
            .map(|(index, m)| Message {
                id: MessageId::from_uuid(m.id),
                chat_id: self.thread(),
                ordinal: index as u32,
                role: if m.sender.starts_with("person:") {
                    ChatRole::User
                } else {
                    ChatRole::Assistant
                },
                state: MessageState::Complete,
                markdown: m.content.body.clone(),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: Timestamp::from_millis(m.created_at_ms),
                completed_at: Some(Timestamp::from_millis(m.created_at_ms)),
            })
            .collect();
        ChatTranscript {
            chat: self.snapshot.conversation.chat.clone(),
            tool_policy: koru_ai_core::ChatToolPolicy {
                chat_id: self.thread(),
                mode: koru_ai_core::ToolMode::Manual,
                workspace_ids: vec![],
            },
            messages,
            tool_activities: vec![],
            notices: vec![],
            blocks: self.blocks.values().flatten().cloned().collect(),
            pinned_documents: vec![],
            feedback: vec![],
            usage: vec![],
            sources: vec![],
            reasoning_control: None,
            agent_commands: None,
            attachments: vec![],
        }
    }

    pub fn ingest(&mut self, message: wire::Message) -> bool {
        if self.messages.get(&message.sequence).is_some_and(|old| {
            old.id == message.id
                && old.content.body == message.content.body
                && old.content.references == message.content.references
        }) {
            return false;
        }
        self.blocks
            .insert(message.sequence, render_blocks(&message));
        self.messages.insert(message.sequence, message);
        self.requests_dirty = true;
        true
    }
}

fn render_blocks(message: &wire::Message) -> Vec<koru_ai_core::MessageBlock> {
    if message.kind != "words"
        || message.sender == "engine"
        || message.sender.starts_with("person:")
    {
        return vec![];
    }
    koru_ai_core::finalize_markdown(MessageId::from_uuid(message.id), &message.content.body)
}
