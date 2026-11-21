// SPDX-License-Identifier: MPL-2.0

//! Local navigation: stable order, row actions and conversation metadata.
//! Read markers remain in the Chats navigator's shared SeenBook.

use super::*;
use koru_ai_core::ChatId;
use std::collections::HashSet;

#[derive(Default)]
pub(super) struct Navigation {
    pub menu: Option<(Option<BotId>, Option<(f32, f32)>)>,
    pub drag: Option<Drag>,
    order: Vec<BotId>,
    pub unread: HashSet<BotId>,
    pub current: Option<ChatId>,
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
    pub(crate) fn set_current_conversation(&mut self, current: Option<(BotId, ChatId)>) -> bool {
        let chat = current.map(|(_, chat)| chat);
        let bot = current.map(|(bot, _)| bot);
        let changed = self.navigation.current != chat || self.selected != bot;
        self.navigation.current = chat;
        self.selected = bot;
        changed
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

    pub(super) fn navigation_workspace_changed(&mut self) {
        self.navigation.drag = None;
        self.navigation.menu = None;
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
        self.row(bot)
            .is_some_and(|row| summary::needs_attention(row, self.navigation.unread.contains(&bot)))
    }

    pub(crate) fn set_unread(&mut self, unread: HashSet<BotId>) -> bool {
        if self.navigation.unread == unread {
            return false;
        }
        self.navigation.unread = unread;
        true
    }
}
