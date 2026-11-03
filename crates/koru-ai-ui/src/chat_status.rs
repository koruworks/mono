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
        self.reply_streaming() || self.message_on_its_way()
    }

    /// **A reply stopped to ask** — a call waits for its approval, or an
    /// agent for a permission answer, in the chat on screen.
    pub fn needs_you(&self) -> bool {
        self.transcript.as_ref().is_some_and(|transcript| {
            transcript
                .tool_activities
                .iter()
                .any(|activity| activity.status == ToolActivityStatus::ApprovalRequired)
        })
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

    /// Count a send or a reply's end, and move the chat up this window's
    /// own Chats pane at once (the standalone window's).
    pub(crate) fn note_activity(&mut self) {
        self.activity = self.activity.wrapping_add(1);
        if let Some(chat) = self.shown_chat_id() {
            self.chats_pane
                .touch(chat, koru_ai_core::Timestamp::now().millis());
        }
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
mod tests {
    use super::*;

    #[test]
    fn the_sweep_crosses_once_and_starts_again() {
        assert_eq!(sweep_phase(0), 0.0);
        assert!((sweep_phase(SWEEP_MS / 2) - 0.5).abs() < 1e-6);
        assert_eq!(sweep_phase(SWEEP_MS), 0.0);
        assert!(sweep_phase(-1) > 0.99);
    }
}
