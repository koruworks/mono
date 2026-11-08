// SPDX-License-Identifier: MPL-2.0

//! What a press and a key do — **the interaction contract both hosts share**
//! (`docs/features/koruui/CONTRIBUTING.md`, *Chats*):
//!
//! - focus enters on open ([`focus_first`](ChatsSurface::focus_first)) and
//!   returns on close — Escape gives the keyboard back;
//! - Up/Down move the keyboard a row, Home/End to either end;
//! - Enter (or Space) opens; Delete asks — for the picked chats when the
//!   keyboard's row is one of them; the menu key (or `Shift+F10`) opens the
//!   row's menu, the one a right-click and its ⋯ open (D91);
//! - a plain press picks one chat and opens it, Ctrl toggles a chat in the
//!   pick, Shift extends it from the anchor (the library's rules, the review's
//!   note);
//! - a row chosen by the pointer and one chosen by the keyboard mean the same
//!   thing, because both reach [`activate`](ChatsSurface::activate).
//!
//! **Only those keys are claimed.** Everything else falls through, so a
//! focused list never makes the window feel stuck.

use koru_draw::geometry::Rect;

use crate::rows::Row;
use crate::{ChatsSurface, Focus, Press};

impl ChatsSurface {
    /// A press. `true` when something changed and a repaint is owed.
    ///
    /// A press on the list's background is still ours: it must not fall
    /// through to whatever is beside or behind the list.
    pub fn mouse_down(&mut self, area: Rect, top: f32, x: f32, y: f32) -> bool {
        self.mouse_down_with(area, top, x, y, false, false)
    }

    /// A press with the selection keys: Ctrl toggles the chat in the pick,
    /// Shift extends it from the anchor, and neither opens; a plain press
    /// picks the chat alone and opens it.
    pub fn mouse_down_with(
        &mut self,
        area: Rect,
        top: f32,
        x: f32,
        y: f32,
        ctrl: bool,
        shift: bool,
    ) -> bool {
        let Some((row @ Row::Chat(index), rect)) = self.row_at(area, top, x, y) else {
            return true;
        };
        let Some(chat) = self.chats.get(index).map(|chat| chat.id) else {
            return true;
        };
        // **The row's own buttons** (D86) do what they say and do not also
        // choose the row: the pin pins, ⋯ opens the menu under itself.
        match self.button_at(row, rect, x, y) {
            Some(crate::RowButton::Pin) => {
                let pinned = !self.is_pinned(chat);
                self.ask_pin(chat, pinned);
                return true;
            }
            Some(crate::RowButton::More) => {
                let more = self.row_parts(row, rect).more;
                self.ask_menu(chat, (more.x, more.y + more.h));
                return true;
            }
            None => {}
        }
        if shift {
            self.select_through(chat);
        } else if ctrl {
            self.toggle_selected(chat);
        } else {
            self.activate(Row::Chat(index));
        }
        true
    }

    /// The chat a press here would choose — a chat row, off its own pin and
    /// ⋯ — without choosing it: nothing is picked and nothing opens. What a
    /// host asks before giving a modified press a meaning of its own (the
    /// navigator's Alt+click, which asks where the chat opens).
    pub fn chat_pressed_at(
        &mut self,
        area: Rect,
        top: f32,
        x: f32,
        y: f32,
    ) -> Option<crate::ChatId> {
        let (row @ Row::Chat(index), rect) = self.row_at(area, top, x, y)? else {
            return None;
        };
        if self.button_at(row, rect, x, y).is_some() {
            return None;
        }
        self.chats.get(index).map(|chat| chat.id)
    }

    /// A right-click. Names the chats the host's menu is about: the pick,
    /// when the chat under the pointer is in it, else that chat alone, which
    /// becomes the pick. The keyboard lands on the chat under the pointer.
    /// `None` off a chat row.
    pub fn context_menu(
        &mut self,
        area: Rect,
        top: f32,
        x: f32,
        y: f32,
    ) -> Option<Vec<crate::ChatId>> {
        let Some((Row::Chat(index), _)) = self.row_at(area, top, x, y) else {
            return None;
        };
        let chat = self.chats.get(index)?.id;
        self.focus = Some(Focus::Chat(chat));
        if !self.selection.contains(&chat) {
            self.select_only(chat);
        }
        Some(self.selected())
    }

    /// The chat under a point, if the point is on a chat row — what `F2`
    /// over the list renames. Nothing changes: pointing is not choosing.
    pub fn chat_at(&mut self, area: Rect, top: f32, x: f32, y: f32) -> Option<crate::ChatId> {
        let Some((Row::Chat(index), _)) = self.row_at(area, top, x, y) else {
            return None;
        };
        self.chats.get(index).map(|chat| chat.id)
    }

    /// A pointer move. `true` when the hover changed — the row, or the
    /// row's button under it.
    pub fn mouse_move(&mut self, area: Rect, top: f32, x: f32, y: f32) -> bool {
        let at = self
            .row_at(area, top, x, y)
            .filter(|(row, _)| row.pressable());
        let hover = at.map(|(row, _)| row);
        let moved = self.hover != hover;
        self.hover = hover;
        let button = at.and_then(|(row, rect)| self.button_at(row, rect, x, y));
        let pressed_on = self.hover_button != button;
        self.hover_button = button;
        moved || pressed_on
    }

    /// The pointer has left the list.
    pub fn clear_hover(&mut self) -> bool {
        self.hover_button = None;
        self.hover.take().is_some()
    }

    pub fn hover(&self) -> Option<Row> {
        self.hover
    }

    pub fn focus(&self) -> Option<Focus> {
        self.focus
    }

    /// The chat the keyboard is on, if it is on one.
    pub fn focused_chat(&self) -> Option<crate::ChatId> {
        let Focus::Chat(chat) = self.focus?;
        Some(chat)
    }

    /// Whether the surface holds the keyboard.
    pub fn has_keyboard(&self) -> bool {
        self.focus.is_some()
    }

    /// Give the keyboard back. `true` when the surface had it.
    pub fn clear_focus(&mut self) -> bool {
        self.focus.take().is_some()
    }

    /// Put the keyboard on one row — an accessibility client's *Focus*.
    pub fn set_focus(&mut self, focus: Focus, area: Rect, top: f32) -> bool {
        self.focus = Some(focus);
        if let Some(row) = self.focus_row() {
            self.scroll_into_view(row, area, top);
            return true;
        }
        false
    }

    /// **Focus enters on open.** The chat the host has open when the list
    /// holds it — so opening the list from a chat starts you on that chat —
    /// else the newest chat. Nothing changes when the keyboard is already
    /// here, and an empty list has nowhere to land (`false`).
    pub fn focus_first(&mut self, area: Rect, top: f32) -> bool {
        if self.focus.is_some() {
            return false;
        }
        // Shown before the list has answered: land on the open chat when it
        // arrives rather than leave the keyboard on *New chat* for good.
        if let Some(current) = self.current
            && !self.chats.iter().any(|chat| chat.id == current)
            && self.load != crate::Load::Ready
        {
            self.pending_reveal = Some(current);
        }
        let Some(landing) = self
            .current
            .filter(|current| self.chats.iter().any(|chat| chat.id == *current))
            .or_else(|| self.chats.first().map(|chat| chat.id))
        else {
            return false;
        };
        self.set_focus(Focus::Chat(landing), area, top);
        true
    }

    /// A key. `true` when the surface answered it; `false` falls through.
    pub fn key(&mut self, area: Rect, top: f32, press: Press) -> bool {
        let moving = matches!(press, Press::Up | Press::Down | Press::Home | Press::End);
        if self.focus.is_none() && !moving {
            return false;
        }
        if press == Press::Escape {
            // The keyboard goes back where it came from. A list that keeps
            // the arrows after you have left it is a window that has quietly
            // stopped answering the keys you are pressing.
            return self.clear_focus();
        }
        if press == Press::Menu {
            return self.menu_for_focus(area, top);
        }
        let rows = self.pressable_rows(area, top);
        if rows.is_empty() {
            return false;
        }
        let at = self
            .focus_row()
            .and_then(|focus| rows.iter().position(|row| *row == focus));
        let last = rows.len() - 1;
        let next = match press {
            Press::Down => at.map_or(0, |at| (at + 1).min(last)),
            Press::Up => at.map_or(0, |at| at.saturating_sub(1)),
            Press::Home => 0,
            Press::End => last,
            Press::Enter | Press::Space => {
                return match self.focus_row() {
                    Some(row) => {
                        self.activate(row);
                        true
                    }
                    None => false,
                };
            }
            Press::Delete => return self.ask_delete_selected(),
            Press::Escape | Press::Menu => unreachable!("answered above"),
        };
        let row = rows[next];
        self.focus = self.focus_of(row);
        self.scroll_into_view(row, area, top);
        true
    }

    /// **The keyboard row's menu** (D91): what a right-click and the row's ⋯
    /// open, about the pick when the row is in it, hung where the ⋯ would
    /// sit — under the row's end. The buttons are the pointer's alone, so
    /// this is the keyboard's way to the same actions.
    fn menu_for_focus(&mut self, area: Rect, top: f32) -> bool {
        let Some(row @ Row::Chat(index)) = self.focus_row() else {
            return false;
        };
        let Some(chat) = self.chats.get(index).map(|chat| chat.id) else {
            return false;
        };
        self.scroll_into_view(row, area, top);
        let Some((_, rect)) = self
            .layout(area, top)
            .into_iter()
            .find(|(candidate, _)| *candidate == row)
        else {
            return false;
        };
        let end = rect.x + rect.w - koru_draw::theme::NAV_PAD - crate::rows::END_W;
        self.ask_menu(chat, (end, rect.y + rect.h));
        true
    }

    /// What a row does when it is chosen, however it was chosen: it becomes
    /// the pick, and it opens.
    fn activate(&mut self, row: Row) {
        match row {
            Row::Chat(index) => {
                if let Some(chat) = self.chats.get(index).map(|chat| chat.id) {
                    self.select_only(chat);
                    self.ask_open(chat);
                }
            }
            Row::Notice(_) | Row::Note(_) | Row::PinnedHeading | Row::GroupGap => {}
        }
    }

    fn focus_of(&self, row: Row) -> Option<Focus> {
        match row {
            Row::Chat(index) => self.chats.get(index).map(|chat| Focus::Chat(chat.id)),
            _ => self.focus,
        }
    }

    /// The rows the keyboard steps through, in order.
    fn pressable_rows(&mut self, area: Rect, top: f32) -> Vec<Row> {
        self.laid_out(area, top)
            .into_iter()
            .map(|(row, _)| row)
            .filter(Row::pressable)
            .collect()
    }
}
