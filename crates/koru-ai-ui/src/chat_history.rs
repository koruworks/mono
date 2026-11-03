// SPDX-License-Identifier: MPL-2.0

//! **Back and forward over the chats a surface showed in place** (koru-ai
//! D102), and the scroll a chat is put back at when it comes back.
//!
//! The workspace window already has a history: a list of places across its
//! tabs (`apps/koru/src/mainview/nav.rs`), walked by `Alt`+←/→ (`⌘[`/`⌘]` on
//! macOS) and the mouse's thumb buttons. A chat tab is one of those places,
//! and its surface never reads another chat into its own place — choosing a
//! chat there focuses or opens a tab — so a tab's surface keeps no list here.
//!
//! **The standalone window has one surface and no tabs.** Every way it moves
//! to another chat — a row of its Chats pane, `Ctrl+Shift+↑`/`↓`, *New chat*,
//! a chat link from elsewhere — reads that chat into the same place, so its
//! back and forward walk this list instead. The rules are the window's: a new
//! move drops the forward branch (the browser rule), a chat that has gone is
//! skipped rather than stopped on, the list is capped, and a step reads the
//! chat back at the scroll it was left at. Nothing is stopped: a reply running
//! in the chat left behind keeps running in the service, as it does when a
//! row is chosen.

use crate::{ChatId, ChatSurface, Command};

/// How many chats back a window can go — the workspace window's depth.
const CAP: usize = 50;

#[derive(Default)]
pub(crate) struct ChatHistory {
    stops: Vec<Stop>,
    /// The stop the surface stands on, or `stops.len()` past the newest.
    at: usize,
    /// A scroll to put back once `chat` is laid out: a step back to it, or a
    /// host reopening it ([`ChatSurface::restore_conversation_scroll`]).
    restore: Option<(ChatId, f32)>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Stop {
    chat: ChatId,
    scroll: f32,
}

impl ChatSurface {
    /// Where the conversation is scrolled to, as somewhere to come back to —
    /// a scroll still waiting for its chat to load counts, so a chat left
    /// before its read lands is not recorded at the top.
    pub fn conversation_scroll(&self) -> f32 {
        match self.chat_history.restore {
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
        self.chat_history.restore = Some((chat, scroll));
        self.messages_dirty = true;
    }

    /// The scroll owed to the chat now laid out, taken once. `draw_messages`
    /// asks after its own decisions, so this one wins.
    pub(crate) fn take_restored_scroll(&mut self) -> Option<f32> {
        let (chat, scroll) = self.chat_history.restore?;
        if self.transcript.as_ref().map(|t| t.chat.id) != Some(chat) {
            return None;
        }
        self.chat_history.restore = None;
        Some(scroll)
    }

    /// **Note the chat on screen, because another is about to be read into
    /// its place.** Called at the top of every in-place move; only the
    /// standalone window moves in place, so only it records.
    pub(crate) fn leave_chat_in_place(&mut self) {
        if !self.hosts_chats_pane() {
            return;
        }
        let here = self.here();
        // A scroll owed to the chat being left is moot now.
        self.chat_history.restore = None;
        let Some(here) = here else {
            return;
        };
        let history = &mut self.chat_history;
        history.stops.truncate(history.at);
        // Standing still is not a move: the same chat twice is one stop.
        if history.stops.last().map(|stop| stop.chat) == Some(here.chat) {
            history.stops.pop();
        }
        history.stops.push(here);
        if history.stops.len() > CAP {
            history.stops.remove(0);
        }
        history.at = history.stops.len();
    }

    /// **Back (`-1`) or forward (`+1`) through the chats shown in place.**
    /// `false` at either end, and in a surface that does not move in place —
    /// a host with tabs walks its own history.
    pub fn step_chat_history(&mut self, delta: i32) -> bool {
        if delta == 0 || !self.hosts_chats_pane() {
            return false;
        }
        let here = self.here();
        let history = &mut self.chat_history;
        // Going back from the newest place puts the place we stand in the
        // list first, or there is nothing to come forward to.
        if delta < 0 && history.at >= history.stops.len() {
            history.at = history.stops.len();
            if let Some(here) = here {
                if history.stops.last().map(|stop| stop.chat) == Some(here.chat) {
                    history.stops.pop();
                }
                history.stops.push(here);
                history.at = history.stops.len() - 1;
            }
        } else if let Some(here) = here
            && let Some(stop) = history.stops.get_mut(history.at)
            && stop.chat == here.chat
        {
            // The stop we stand on is kept as we leave it.
            stop.scroll = here.scroll;
        }
        let step = delta.signum() as isize;
        let mut at = history.at as isize;
        loop {
            at += step;
            let Some(stop) = usize::try_from(at)
                .ok()
                .and_then(|at| self.chat_history.stops.get(at))
                .copied()
            else {
                return false;
            };
            // Skipped, not removed: removing would shift the cursor, and the
            // walk carries on in the same direction.
            if Some(stop.chat) == here.map(|here| here.chat) || self.chat_gone(stop.chat) {
                continue;
            }
            self.chat_history.at = at as usize;
            self.close_chats_pane();
            self.scroll_loaded_chat_to_bottom = false;
            self.send_command(Command::Read(stop.chat));
            self.chat_history.restore = Some((stop.chat, stop.scroll));
            self.messages_dirty = true;
            return true;
        }
    }

    /// **A reply running in the chat on screen**, for a host's tests: what a
    /// send the service took leaves behind, with no service to take it.
    #[cfg(feature = "test-support")]
    pub fn pretend_reply_running(&mut self) {
        self.attempt = Some(koru_ai_core::InferenceAttemptId::new());
    }

    /// The chat on screen as a stop, when it is a chat the service holds.
    fn here(&self) -> Option<Stop> {
        self.current_chat_id().map(|chat| Stop {
            chat,
            scroll: self.conversation_scroll(),
        })
    }

    /// A chat the listing no longer holds — deleted since. Only a listing
    /// that has answered can say so; before it, nothing is gone.
    fn chat_gone(&self, chat: ChatId) -> bool {
        !self.chats.is_empty() && !self.chats.iter().any(|listed| listed.id == chat)
    }
}

#[cfg(test)]
mod tests;
