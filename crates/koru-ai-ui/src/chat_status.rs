// SPDX-License-Identifier: MPL-2.0

//! **What this chat is doing, for the marks outside it** (koru-ai D83).
//!
//! A tab, a Chats row and the chat's own *Working for 25s* say the same
//! three things — working, needs you, unread — and they are decided in one
//! place, [`koru_chats_ui::ChatStatus::of`]. This module answers the two
//! inputs only a chat surface knows: **whether a reply is running** (a turn
//! sent and not yet ended) and **whether one stopped to ask** (an approval
//! or a permission waits). Unread is the host's: it knows what the person
//! is looking at, which a surface does not.
//!
//! The host also reads two counts off a surface — replies that ended and
//! sends the service took — to notice an arrival in a chat nobody is
//! looking at and to move the chat up its list, without a callback and
//! without a poll of its own.

use koru_ai_core::{MessageId, MessageState, ToolActivityStatus};
use koru_chats_ui::ChatStatus;

use crate::ChatSurface;

/// One crossing of the light over *Working for 25s*, in milliseconds.
const SWEEP_MS: i64 = 1_600;

/// How far the light has crossed, `0..1`, at `now_ms`.
pub(crate) fn sweep_phase(now_ms: i64) -> f32 {
    now_ms.rem_euclid(SWEEP_MS) as f32 / SWEEP_MS as f32
}

impl ChatSurface {
    /// The person asked for less motion (the system setting): the light over
    /// *Working for …* does not sweep and asks for no frames.
    pub fn set_reduced_motion(&mut self, reduced: bool) -> bool {
        let changed = self.reduced_motion != reduced;
        self.reduced_motion = reduced;
        changed
    }

    /// **A reply is running in this chat** — sent and not yet taken by the
    /// service, or taken and its stream not ended.
    pub fn is_working(&self) -> bool {
        if let Some(state) = &self.managed {
            return state.running() || state.pending_send.is_some();
        }
        self.reply_streaming() || self.message_on_its_way()
    }

    /// **A reply stopped to ask** — a call waits for its approval, or an
    /// agent for a permission answer, in the chat on screen.
    pub fn needs_you(&self) -> bool {
        if let Some(state) = &self.managed {
            return state
                .snapshot
                .turns
                .iter()
                .any(|turn| matches!(turn.state.as_str(), "needs_you" | "interrupted"));
        }
        self.transcript.as_ref().is_some_and(|transcript| {
            transcript
                .tool_activities
                .iter()
                .any(|activity| activity.status == ToolActivityStatus::ApprovalRequired)
        })
    }

    /// **This surface knows nothing runs in the chat on screen**: it has
    /// read that chat, and no reply runs or waits in it — none it sent or
    /// follows, none its read left in progress (a reply started somewhere
    /// else), no question waiting. *Not working* alone is no such evidence:
    /// a surface that has not read its chat yet is not working either. For
    /// a host asking before it stops work.
    pub fn knows_nothing_runs(&self) -> bool {
        let Some(transcript) = &self.transcript else {
            return false;
        };
        // An owned surface moved to another conversation has not read that
        // one yet: what it read is the last one's.
        let read_bound = self
            .owned()
            .is_none_or(|owned| owned.chat == transcript.chat.id);
        if let Some(state) = &self.managed {
            return read_bound
                && state.thread() == transcript.chat.id
                && state.pending_send.is_none()
                && state.snapshot.waiting_messages == 0
                && !state.snapshot.turns.iter().any(|turn| {
                    matches!(
                        turn.state.as_str(),
                        "queued" | "running" | "needs_you" | "parked" | "interrupted"
                    )
                });
        }
        self.current_chat_made()
            && read_bound
            && !self.is_working()
            && !self.reply_pending()
            && !self.needs_you()
            && transcript
                .messages
                .iter()
                .all(|message| message.state != MessageState::Pending)
    }

    /// The chat `chat` read, as the service answers a read of a chat with
    /// nothing in it: a host's tests arm what only the service could
    /// otherwise say.
    #[cfg(feature = "test-support")]
    pub fn pretend_read(&mut self, chat: koru_ai_core::ChatId) {
        let now = koru_ai_core::Timestamp::from_millis(1);
        let transcript = koru_ai_wire::ChatTranscript {
            active_attempt: None,
            sources: Vec::new(),
            chat: koru_ai_core::Chat {
                id: chat,
                title: crate::DEFAULT_CHAT_TITLE.into(),
                private: false,
                current_model: koru_ai_core::ModelSelection {
                    provider: koru_ai_core::AiProviderId::parse(koru_ai_core::AiProviderId::OPENAI)
                        .expect("a provider id"),
                    requested_model: "fixture".into(),
                    default_reasoning: koru_ai_core::ReasoningChoice::Default,
                },
                branched_from: None,
                agent_working_folder: None,
                agent_mode: None,
                agent_fast: None,
                pinned_at: None,
                profile_id: None,
                bot: None,
                created_at: now,
                updated_at: now,
            },
            tool_policy: koru_ai_core::ChatToolPolicy {
                chat_id: chat,
                mode: koru_ai_core::ToolMode::Manual,
                workspace_ids: Vec::new(),
            },
            tool_activities: Vec::new(),
            notices: Vec::new(),
            messages: Vec::new(),
            blocks: Vec::new(),
            pinned_documents: Vec::new(),
            feedback: Vec::new(),
            usage: Vec::new(),
            reasoning_control: None,
            agent_commands: None,
            attachments: Vec::new(),
        };
        self.apply_update(crate::reader::Update::Transcript(transcript));
    }

    /// This chat's status, given whether the host counts it unread.
    pub fn chat_status(&self, unread: bool) -> ChatStatus {
        ChatStatus::of(self.is_working(), self.needs_you(), unread)
    }

    /// How many replies have ended here — completed, failed, stopped or
    /// recovered. Only ever grows; the host compares it with the last count
    /// it saw to notice one that ended while nobody was looking.
    pub fn finished_replies(&self) -> u32 {
        self.replies_finished
    }

    /// **Something arrived since the last ask** — a reply ended, or a
    /// question started waiting. The host asks once a pass, and marks the
    /// chat unread when nobody was looking at it (D83).
    pub fn take_arrival(&mut self) -> bool {
        let ended = self.replies_finished != self.arrivals_reported;
        let asked = self.needs_you();
        let new_question = asked && !self.asked_reported;
        self.arrivals_reported = self.replies_finished;
        self.asked_reported = asked;
        ended || new_question
    }

    /// **This chat moved** (D85) — a send the service took, or a reply that
    /// ended — since the last ask. The host moves the chat to the top of its
    /// Chats list on it, without asking the service again.
    pub fn take_activity(&mut self) -> bool {
        let moved = self.activity != self.activity_reported;
        self.activity_reported = self.activity;
        moved
    }

    /// Count a send or a reply's end ([`take_activity`](Self::take_activity)).
    pub(crate) fn note_activity(&mut self) {
        self.activity = self.activity.wrapping_add(1);
    }

    /// **The newest message the person can see here that is done** — what
    /// a look at this chat has seen up to. A reply still streaming is not
    /// it: a look before it ends has not seen how it ends.
    pub fn newest_seen_message(&self) -> Option<MessageId> {
        let transcript = self.transcript.as_ref()?;
        transcript
            .messages
            .iter()
            .filter(|message| message.state != MessageState::Pending)
            .max_by_key(|message| message.ordinal)
            .map(|message| message.id)
    }
}

#[cfg(test)]
mod tests;
