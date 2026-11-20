// SPDX-License-Identifier: MPL-2.0

//! Local navigation: stable order, row actions and conversation metadata.
//! Read markers remain in the Chats navigator's shared SeenBook.

use super::*;
use koru_ai_core::{
    ChatId,
    bots::{BotChatCursor, BotChatPage},
};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Default)]
pub(super) struct Navigation {
    pub menu: Option<(Option<BotId>, Option<(f32, f32)>)>,
    pub drag: Option<Drag>,
    order: Vec<BotId>,
    pub unread: HashSet<BotId>,
    pub current: Option<ChatId>,
    chats: HashMap<BotId, Vec<(ChatId, i64)>>,
    queue: VecDeque<BotId>,
    reading: Option<(u64, BotId, Option<BotChatCursor>)>,
    received: Vec<(ChatId, i64)>,
    nonempty: HashSet<ChatId>,
    arrivals: Vec<(ChatId, i64)>,
    seq: u64,
    refresh: bool,
}

pub(super) struct Drag {
    bot: BotId,
    start: (f32, f32),
    original: Vec<BotId>,
    armed: bool,
    grab: f32,
}

pub(crate) fn more_key(bot: BotId) -> Key {
    Key::model(bot.as_uuid().as_u128(), "bot-more")
}

impl BotsRoster {
    pub(crate) fn conversation_subject(&self, chat: ChatId) -> Option<(BotId, ChatId)> {
        self.bots
            .iter()
            .find(|bot| self.conversation_ids(bot.id).contains(&chat))
            .map(|bot| (bot.id, chat))
    }

    pub(crate) fn focused_conversation(&self) -> Option<(BotId, ChatId)> {
        self.conversation_ids_all()
            .into_iter()
            .find(|chat| Some(activity_key(*chat)) == self.focus)
            .and_then(|chat| self.conversation_subject(chat))
    }

    pub(crate) fn focus_conversation(&mut self, chat: ChatId) -> bool {
        if self.conversation_subject(chat).is_none() {
            return false;
        }
        self.focus = Some(activity_key(chat));
        self.reveal_owed = true;
        true
    }

    pub(crate) fn conversation_at(
        &self,
        area: Rect,
        top: f32,
        x: f32,
        y: f32,
    ) -> Option<(BotId, ChatId)> {
        self.activity_at(area, top, x, y)
            .and_then(|chat| self.conversation_subject(chat))
    }

    pub(crate) fn conversation_label_rect(
        &self,
        area: Rect,
        top: f32,
        chat: ChatId,
    ) -> Option<Rect> {
        self.with_input_frame(area, top, |_, frame| {
            let parent = frame
                .nodes
                .iter()
                .position(|node| node.key == Some(activity_key(chat)))?;
            frame
                .nodes
                .iter()
                .find(|node| {
                    node.parent == Some(parent) && node.name == "Bot activity conversation"
                })
                .map(|node| node.rect)
        })
    }

    #[cfg(test)]
    pub(crate) fn conversations_for_test(&mut self, bot: BotId, page: BotChatPage) {
        self.navigation.reading = Some((1, bot, None));
        assert!(self.take_conversation_page(1, bot, Ok(page)));
    }

    pub(crate) fn set_current_conversation(&mut self, current: Option<(BotId, ChatId)>) -> bool {
        let chat = current.map(|(_, chat)| chat);
        let bot = current.map(|(bot, _)| bot);
        let changed = self.navigation.current != chat || self.selected != bot;
        self.navigation.current = chat;
        self.selected = bot;
        changed
    }

    pub(crate) fn current_conversation(&self) -> Option<ChatId> {
        self.navigation.current
    }
    /// Main first, then the owner's paginated conversations, including idle ones.
    pub(crate) fn conversation_ids(&self, bot: BotId) -> Vec<ChatId> {
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        if let Some(row) = self.row(bot) {
            ids.push(row.main_chat);
            seen.insert(row.main_chat);
        }
        for chat in self
            .navigation
            .chats
            .get(&bot)
            .into_iter()
            .flatten()
            .map(|(id, _)| *id)
            .chain(
                self.activity
                    .iter()
                    .filter(|entry| entry.bot_id == bot)
                    .map(|entry| entry.chat_id),
            )
        {
            if seen.insert(chat) {
                ids.push(chat);
            }
        }
        ids
    }

    pub(crate) fn conversation_ids_all(&self) -> Vec<ChatId> {
        self.bots
            .iter()
            .flat_map(|bot| self.conversation_ids(bot.id))
            .collect()
    }

    pub(crate) fn conversation_name(&self, bot: &RosterBot, chat: ChatId) -> String {
        self.activity
            .iter()
            .find(|entry| entry.chat_id == chat)
            .map_or_else(
                || self.conversation_words(bot, chat),
                |entry| self.activity_name(bot, entry),
            )
    }

    pub(crate) fn menu_row(&self, bot: BotId) -> Option<RosterBot> {
        self.row(bot).cloned().or_else(|| self.archived_row(bot))
    }

    pub(crate) fn request_menu(&mut self, at: (f32, f32)) {
        self.navigation.menu = Some((None, Some(at)));
    }

    pub(crate) fn take_menu(&mut self) -> Option<(Option<BotId>, Option<(f32, f32)>)> {
        self.navigation.menu.take()
    }

    pub(super) fn more_button(&self, bot: BotId) -> El {
        let shown = self.hover == Some(bot)
            || self.hover_archived == Some(archived_key(bot))
            || self.hover_archived == Some(more_key(bot))
            || self.focus == Some(row_key(bot))
            || self.focus == Some(more_key(bot))
            || self.focus == Some(archived_key(bot));
        koru_draw::el::boxed("Bot actions")
            .id(more_key(bot))
            .w(metrics::TRAILING_LANE_W)
            .h(metrics::NAV_ROW_MIN)
            .hit()
            .focusable()
            .action(Action::Click)
            .action(Action::Focus)
            .semantics(Role::Button)
            .name(Words::A11yMoreActions)
            .radius(Radius::S)
            .states(&BOT_ROW_STATES)
            .child(if shown {
                icon(icons::DOTS_THREE, IconSize::S).decorative()
            } else {
                icon(icons::DESKTOP, IconSize::S)
                    .alt(Words::BotsRunsOnThisComputer)
                    .id(runs_here_key(bot))
                    .ink(Ink::Hint)
            })
    }

    pub(super) fn unread_mark(&self) -> El {
        koru_draw::el::leaf(
            "Bot unread conversations",
            MarkLeaf(ChatStatus::Unread, false),
        )
        .decorative()
    }

    pub(crate) fn pointer_active(&self) -> bool {
        self.navigation.drag.is_some()
    }

    pub(crate) fn drag_armed(&self) -> bool {
        self.navigation.drag.as_ref().is_some_and(|drag| drag.armed)
    }

    pub(super) fn begin_drag(&mut self, bot: BotId, area: Rect, top: f32, x: f32, y: f32) {
        if self.bots.iter().any(|row| row.id == bot) {
            let grab = self.with_input_frame(area, top, |_, frame| {
                frame.node(row_key(bot)).map_or(0.0, |node| y - node.rect.y)
            });
            self.navigation.drag = Some(Drag {
                bot,
                start: (x, y),
                original: self.bots.iter().map(|row| row.id).collect(),
                armed: false,
                grab,
            });
        } else {
            self.ask_open(bot);
        }
    }

    pub(super) fn move_drag(&mut self, area: Rect, top: f32, x: f32, y: f32) -> bool {
        let Some(drag) = self.navigation.drag.as_mut() else {
            return false;
        };
        if !drag.armed && (x - drag.start.0).hypot(y - drag.start.1) < 5.0 {
            return false;
        }
        drag.armed = true;
        let bot = drag.bot;
        let grab = drag.grab;
        let Some(from) = self.bots.iter().position(|row| row.id == bot) else {
            return self.cancel_drag();
        };
        let band = Self::band(area, top);
        let edge = metrics::NAV_ROW_MIN.value;
        if y < band.y + edge {
            self.scroll_by(edge / 2.0);
        } else if y > band.y + band.h - edge {
            self.scroll_by(-edge / 2.0);
        }
        let target = self.with_input_frame(area, top, |_, frame| {
            let slots: Vec<_> = self
                .bots
                .iter()
                .filter_map(|row| {
                    frame
                        .node(row_key(row.id))
                        .map(|node| (node.rect.y, node.rect.h))
                })
                .collect();
            if slots.len() != self.bots.len() {
                return from;
            }
            koru_shell_chrome::reorder::drop_index(&slots, from, y - grab + slots[from].1 / 2.0)
        });
        if target != from {
            let row = self.bots.remove(from);
            self.bots.insert(target, row);
        }
        true
    }

    pub(crate) fn mouse_up(&mut self) -> bool {
        let Some(drag) = self.navigation.drag.take() else {
            return false;
        };
        if drag.armed {
            // Keep other workspaces' order as well; Bot IDs are global.
            let ids: HashSet<_> = self.bots.iter().map(|row| row.id).collect();
            self.navigation.order.retain(|id| !ids.contains(id));
            self.navigation
                .order
                .extend(self.bots.iter().map(|row| row.id));
        } else {
            self.ask_open(drag.bot);
        }
        true
    }

    pub(crate) fn cancel_drag(&mut self) -> bool {
        let Some(drag) = self.navigation.drag.take() else {
            return false;
        };
        self.bots.sort_by_key(|row| {
            drag.original
                .iter()
                .position(|id| *id == row.id)
                .unwrap_or(usize::MAX)
        });
        true
    }

    pub(super) fn apply_order(&mut self) {
        self.bots.sort_by_key(|row| {
            self.navigation
                .order
                .iter()
                .position(|id| *id == row.id)
                .unwrap_or(usize::MAX)
        });
    }

    pub(crate) fn conversation_metadata(&self) -> impl Iterator<Item = (BotId, ChatId, i64)> + '_ {
        self.navigation
            .chats
            .iter()
            .flat_map(|(bot, chats)| chats.iter().map(|(chat, at)| (*bot, *chat, *at)))
    }

    pub(crate) fn take_conversation_arrivals(&mut self) -> Vec<(ChatId, i64)> {
        std::mem::take(&mut self.navigation.arrivals)
    }

    pub(super) fn navigation_workspace_changed(&mut self) {
        self.navigation.drag = None;
        self.navigation.menu = None;
        self.navigation.chats.clear();
        self.navigation.queue.clear();
        self.navigation.reading = None;
        self.navigation.received.clear();
        self.navigation.nonempty.clear();
        self.navigation.arrivals.clear();
        self.navigation.unread.clear();
        self.navigation.current = None;
    }

    pub(super) fn carried_key(&self) -> Option<Key> {
        self.navigation
            .drag
            .as_ref()
            .filter(|drag| drag.armed)
            .map(|drag| row_key(drag.bot))
    }

    pub(crate) fn has_unread(&self, bot: BotId) -> bool {
        self.navigation.unread.contains(&bot)
    }

    pub(super) fn navigation_awaiting(&self) -> bool {
        self.navigation.reading.is_some()
            || !self.navigation.queue.is_empty()
            || self.navigation.refresh
    }

    pub(crate) fn set_unread(&mut self, unread: HashSet<BotId>) -> bool {
        if self.navigation.unread == unread {
            return false;
        }
        self.navigation.unread = unread;
        true
    }

    pub(super) fn refresh_conversation_metadata(&mut self) {
        self.navigation.refresh = true;
    }

    pub(super) fn ask_conversation_metadata(&mut self) -> bool {
        if self.navigation.reading.is_some() {
            return false;
        }
        if self.navigation.queue.is_empty() && self.navigation.refresh {
            self.navigation.refresh = false;
            self.navigation
                .queue
                .extend(self.bots.iter().map(|bot| bot.id));
            self.navigation
                .chats
                .retain(|bot, _| self.bots.iter().any(|row| row.id == *bot));
        }
        let Some(bot) = self.navigation.queue.pop_front() else {
            return false;
        };
        self.navigation.received.clear();
        self.navigation.nonempty.clear();
        self.send_conversation_page(bot, None)
    }

    fn send_conversation_page(&mut self, bot: BotId, cursor: Option<BotChatCursor>) -> bool {
        self.navigation.seq = self.navigation.seq.wrapping_add(1);
        let seq = self.navigation.seq;
        if self.link.as_ref().is_some_and(|link| {
            link.send(Command::ListChats {
                seq,
                bot,
                cursor: cursor.clone(),
                limit: 100,
            })
        }) {
            self.navigation.reading = Some((seq, bot, cursor));
            true
        } else {
            false
        }
    }

    pub(super) fn take_conversation_page(
        &mut self,
        seq: u64,
        bot: BotId,
        result: Result<BotChatPage, Failure>,
    ) -> bool {
        let Some((expected, owner, cursor)) = &self.navigation.reading else {
            return false;
        };
        if *expected != seq || *owner != bot {
            return false;
        }
        let cursor = cursor.clone();
        self.navigation.reading = None;
        if let Ok(page) = result {
            for row in &page.chats {
                if !row.chat.title.trim().is_empty() {
                    self.titles.insert(row.chat.id, row.chat.title.clone());
                }
            }
            self.navigation.nonempty.extend(
                page.chats
                    .iter()
                    .filter(|row| row.chat.updated_at > row.chat.created_at)
                    .map(|row| row.chat.id),
            );
            self.navigation.received.extend(
                page.chats
                    .iter()
                    .map(|row| (row.chat.id, row.chat.updated_at.millis())),
            );
            if let Some(next) = page.next_cursor {
                if cursor.as_ref() != Some(&next) {
                    self.send_conversation_page(bot, Some(next));
                }
            } else {
                if let Some(previous) = self.navigation.chats.get(&bot) {
                    self.navigation.arrivals.extend(
                        self.navigation
                            .received
                            .iter()
                            .filter(|(chat, _)| {
                                self.navigation.nonempty.contains(chat)
                                    && !previous.iter().any(|(id, _)| id == chat)
                            })
                            .copied(),
                    );
                }
                self.navigation
                    .chats
                    .insert(bot, std::mem::take(&mut self.navigation.received));
            }
        }
        true
    }
}
