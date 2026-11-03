// SPDX-License-Identifier: MPL-2.0

//! **Which chats are working, waiting on the person, or holding something
//! they have not seen** (koru-ai D83) — the window's half of the status a
//! chat tab, a Chats row and the chat itself all wear.
//!
//! A chat surface knows whether its reply runs and whether it stopped to
//! ask; only the window knows **what the person is looking at** — the chat
//! in front in each pane, and the side pane's while it is open. One pass,
//! on the maintenance tick every chat's news already arrives on:
//!
//! 1. chat tabs behind another hear their news (`drain_in_background`), so
//!    a ring stops when its reply ends wherever the tab is;
//! 2. a chat that came into view is **seen**, and so is one that left it;
//!    something that arrived in a chat nobody was looking at makes it
//!    **unread** ([`SeenBook`]);
//! 3. every tab and every Chats row takes its status from
//!    [`ChatStatus::of`].
//!
//! **What is seen is device-local**, by the person's choice: one book per
//! process, kept in the session file's `chat_seen=` line beside the tabs
//! (`Settings::chat_seen`), which an older build ignores.
//!
//! **Frames only while a ring turns.** [`MainView::chat_status_deadline`]
//! asks for one every `status::FRAME` while a working chat's ring is on
//! screen and motion is not reduced, and for a background tab's news only
//! while its reply is on its way; an idle window asks for nothing here.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Instant;

use koru_ai_ui::{ChatId, MessageId};
use koru_chats_ui::{ChatStatus, SeenBook};

use super::{MainView, TabView, epoch_millis};

/// **The process's book.** One, because every window of the process shows
/// the same chats to the same person.
static BOOK: Mutex<Option<SeenBook>> = Mutex::new(None);

fn with_book<R>(f: impl FnOnce(&mut SeenBook) -> R) -> R {
    let mut guard = BOOK.lock().unwrap_or_else(|error| error.into_inner());
    f(guard.get_or_insert_with(SeenBook::default))
}

/// Fold in the session file's line. **A merge, never a replace**: a second
/// window restoring the line the first wrote changes nothing, and nothing a
/// window restores can make a chat already seen read as unread again.
pub(super) fn restore(line: &str) {
    if line.is_empty() {
        return;
    }
    let read = SeenBook::parse(line);
    with_book(|book| book.merge(read));
}

/// The line for the session file.
pub(super) fn capture() -> String {
    with_book(|book| book.to_storage())
}

/// A chat that was deleted keeps no mark.
pub(super) fn forget(chat: ChatId) {
    with_book(|book| book.forget(chat));
}

/// What one open chat says, gathered across every surface showing it.
#[derive(Default)]
struct Open {
    working: bool,
    needs_you: bool,
    arrived: bool,
    newest: Option<MessageId>,
}

impl MainView {
    /// **The status pass** — see the module docs. `true` when a mark moved
    /// or a ring turned, and a repaint is owed.
    pub(super) fn maintain_chat_status(&mut self) -> bool {
        let now_ms = epoch_millis();
        let reduced = self.reduced_motion;
        let mut changed = false;

        // 1. Every chat tab but the one the view's own pass maintains hears
        //    its news, and says what it is doing.
        let focused = self.panes.focused_path().clone();
        let mut open: HashMap<ChatId, Open> = HashMap::new();
        let mut looking: HashSet<ChatId> = HashSet::new();
        for (path, group) in self.panes.iter_groups_mut() {
            let active = group.active();
            for (index, tab) in group.tabs_mut().iter_mut().enumerate() {
                let TabView::AiChat(view) = &mut tab.view else {
                    continue;
                };
                if !(path == focused && index == active) {
                    changed |= view.drain_in_background();
                }
                let Some(chat) = view.chat_id() else {
                    continue;
                };
                if index == active {
                    looking.insert(chat);
                }
                let facts = open.entry(chat).or_default();
                facts.working |= view.is_working();
                facts.needs_you |= view.needs_you();
                facts.arrived |= view.take_arrival();
                facts.newest = facts.newest.or(view.newest_seen_message());
            }
        }
        // The side pane: looked at while it is open.
        if let Some(chat) = self.ai_chat.current_chat_id() {
            if self.ai_visible() {
                looking.insert(chat);
            }
            let facts = open.entry(chat).or_default();
            facts.working |= self.ai_chat.is_working();
            facts.needs_you |= self.ai_chat.needs_you();
            facts.arrived |= self.ai_chat.take_arrival();
            facts.newest = facts.newest.or(self.ai_chat.newest_seen_message());
        }
        self.ai_chat.set_reduced_motion(reduced);

        // 2. Looks begun, continued and ended; news nobody was looking at.
        let listed: HashMap<ChatId, i64> = self
            .explore
            .chats_mut()
            .chats()
            .iter()
            .map(|chat| (chat.id, chat.updated_at.millis()))
            .collect();
        let was_looking = std::mem::take(&mut self.chats_looking);
        // A chat that came into view, and where its NEW divider goes: after
        // the message seen last, when something arrived since (D84). Read
        // before the look marks it seen.
        let mut came_into_view: Vec<(ChatId, Option<MessageId>)> = Vec::new();
        let statuses = with_book(|book| {
            for (chat, facts) in &open {
                if looking.contains(chat) {
                    if !was_looking.contains(chat) {
                        let from = book
                            .is_unread(*chat, listed.get(chat).copied())
                            .then(|| book.last_seen(*chat))
                            .flatten();
                        came_into_view.push((*chat, from));
                        book.seen(*chat, now_ms, facts.newest);
                    } else if let Some(newest) = facts.newest {
                        book.saw_message(*chat, newest);
                    }
                } else if facts.arrived {
                    book.arrived(*chat, now_ms, facts.newest);
                }
            }
            for chat in was_looking.difference(&looking) {
                let newest = open.get(chat).and_then(|facts| facts.newest);
                book.seen(*chat, now_ms, newest);
            }
            // 3. Every chat's status: what its surfaces say, and unread when
            //    nobody is looking at it and something arrived.
            let unread = |chat: ChatId| {
                !looking.contains(&chat) && book.is_unread(chat, listed.get(&chat).copied())
            };
            let mut statuses: HashMap<ChatId, ChatStatus> = listed
                .keys()
                .map(|chat| (*chat, ChatStatus::of(false, false, unread(*chat))))
                .collect();
            for (chat, facts) in &open {
                statuses.insert(
                    *chat,
                    ChatStatus::of(facts.working, facts.needs_you, unread(*chat)),
                );
            }
            statuses
        });
        self.chats_looking = looking;
        let divider_for = |chat: Option<ChatId>| {
            chat.and_then(|chat| {
                came_into_view
                    .iter()
                    .find(|(seen, _)| *seen == chat)
                    .map(|(_, from)| (chat, *from))
            })
        };

        let mut turning = false;
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::AiChat(view) = &mut tab.view {
                    // Opened on what is new, or the divider of a look
                    // before put away.
                    if let Some((chat, from)) = divider_for(view.chat_id()) {
                        changed |= view.show_new_from(chat, from);
                    }
                    let status = view
                        .chat_id()
                        .and_then(|chat| statuses.get(&chat).copied())
                        .unwrap_or_default();
                    changed |= view.set_status(status, reduced);
                    turning |= status.animating(reduced);
                }
            }
        }
        if let Some((chat, from)) = divider_for(self.ai_chat.current_chat_id()) {
            changed |= self.ai_chat.show_new_from(chat, from);
        }
        let rows = self.explore.chats_mut();
        changed |= rows.set_reduced_motion(reduced);
        changed |= rows.set_statuses(statuses);
        if self.explore.showing_chats() {
            turning |= self.explore.chats_mut().animating();
        }
        changed || turning
    }

    /// **When the status pass is next owed** — at the status frame while a
    /// ring turns on screen, and at a background chat tab's own poll while
    /// its reply is on its way. `None` for an idle window.
    pub(super) fn chat_status_deadline(&self) -> Option<Instant> {
        let focused = self.panes.focused_path().clone();
        let mut deadline: Option<Instant> = None;
        let mut turning = false;
        for (path, group) in self.panes.iter_groups() {
            let active = group.active();
            for (index, tab) in group.tabs().iter().enumerate() {
                let TabView::AiChat(view) = &tab.view else {
                    continue;
                };
                turning |= view.status().animating(self.reduced_motion);
                if !(path == focused && index == active)
                    && let Some(at) = view.background_deadline()
                {
                    deadline = Some(deadline.map_or(at, |was| was.min(at)));
                }
            }
        }
        turning |= self.explore.showing_chats() && self.explore.chats_turning();
        if turning {
            let at = Instant::now() + koru_chats_ui::status::FRAME;
            deadline = Some(deadline.map_or(at, |was| was.min(at)));
        }
        deadline
    }
}

#[cfg(test)]
mod tests;
