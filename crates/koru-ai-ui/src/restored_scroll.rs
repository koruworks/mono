// SPDX-License-Identifier: MPL-2.0

//! **The scroll a chat is put back at when it comes back** (koru-ai D102).
//!
//! The workspace window keeps a history: a list of places across its tabs
//! (`apps/koru/src/mainview/nav.rs`), walked by `Alt`+←/→ (`⌘[`/`⌘]` on
//! macOS) and the mouse's thumb buttons. A chat tab is one of those places,
//! and its surface never reads another chat into its own place — choosing a
//! chat focuses or opens a tab — so the surface keeps no list. What it keeps
//! is the one scroll a host asked to put back, until the chat it belongs to
//! is laid out.

use crate::{ChatId, ChatSurface};

impl ChatSurface {
    /// Where the conversation is scrolled to, as somewhere to come back to —
    /// a scroll still waiting for its chat to load counts, so a chat left
    /// before its read lands is not recorded at the top.
    pub fn conversation_scroll(&self) -> f32 {
        match self.restored_scroll {
            Some((chat, scroll)) if Some(chat) == self.shown_chat_id() => scroll,
            _ => self.conversation_scroll,
        }
    }

    /// Put the conversation back at `scroll`: at once when the chat on
    /// screen is laid out, else at its first layout, ahead of the scroll a
    /// loaded chat would otherwise open at (its end, or its NEW divider).
    pub fn restore_conversation_scroll(&mut self, scroll: f32) {
        let Some(chat) = self.shown_chat_id() else {
            return;
        };
        self.restored_scroll = Some((chat, scroll));
        self.messages_dirty = true;
    }

    /// The scroll owed to the chat now laid out, taken once. `draw_messages`
    /// asks after its own decisions, so this one wins.
    pub(crate) fn take_restored_scroll(&mut self) -> Option<f32> {
        let (chat, scroll): (ChatId, f32) = self.restored_scroll?;
        if self.transcript.as_ref().map(|t| t.chat.id) != Some(chat) {
            return None;
        }
        self.restored_scroll = None;
        Some(scroll)
    }

    /// **A reply running in the chat on screen**, for a host's tests: what a
    /// send the service took leaves behind, with no service to take it.
    #[cfg(feature = "test-support")]
    pub fn pretend_reply_running(&mut self) {
        self.attempt = Some(koru_ai_core::InferenceAttemptId::new());
    }
}

#[cfg(test)]
mod tests;
