// SPDX-License-Identifier: MPL-2.0

//! **The one geometry function.** Drawing, hit-testing, the hover, the
//! keyboard, the scroll clamp and the accessibility projection all come from
//! [`ChatsSurface::layout`], so a row cannot be clickable somewhere other than
//! where it is drawn, nor announced somewhere other than where it is.
//!
//! **One clipped rectangle.** [`rows`](ChatsSurface::rows) returns only what
//! falls inside the band between `top` and the area's floor — which is both
//! the draw's cull and the hit test's bounds — and the draw clips to the same
//! band. A row scrolled half under the heading is half drawn and half
//! clickable, never drawn in one place and pressable in another.

use koru_draw::geometry::Rect;
use koru_draw::theme;

use crate::{ChatsSurface, Focus, Load};

/// A chat row: its title on one line, the library row's height, so the list
/// reads at the Documents list's pitch (the CTO's note). Who answered and
/// when left the row; the accessible name still says it.
pub(crate) const CHAT_H: f32 = theme::NAV_ROW_H;
/// A sentence under a row — *Deleting…*, or why a delete was refused.
const NOTE_H: f32 = 17.0;
/// Air above a notice.
const NOTICE_GAP: f32 = 4.0;
/// A private chat's mark, left of its title in the slot a page icon takes —
/// only on a private row, so every other title starts at the pad, as a
/// Documents label starts after its icon (the CTO's note).
const LEAD_W: f32 = koru_shell_chrome::PAGE_ICON_SLOT;
const LEAD_GAP: f32 = 4.0;
/// The status mark's slot at a row's end (D83), an icon's width.
pub(crate) const END_W: f32 = 20.0;
/// Air between a title and the mark after it.
const END_GAP: f32 = 4.0;
/// Air under the last row, so the list does not end flush against the edge.
pub(crate) const FOOTER_PAD: f32 = 8.0;
/// The *Pinned* heading's line.
const HEADING_H: f32 = 24.0;
/// Air between the pinned chats and the rest.
const GROUP_GAP: f32 = 8.0;

/// Which state the list is in, when it is in one rather than showing rows.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Notice {
    Loading,
    /// Never drawn as an empty list.
    Unavailable,
    Empty,
}

/// One line of the list. Indices rather than ids because a row is a
/// *position*; what survives a refresh — the keyboard, the open chat — is
/// held by id on the surface.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Row {
    Notice(Notice),
    /// *Pinned*, over the pinned chats (koru-ai D85). Only when one is.
    PinnedHeading,
    /// Air between the pinned chats and the rest.
    GroupGap,
    Chat(usize),
    /// The line under a chat whose delete is being sent or was refused.
    Note(usize),
}

impl Row {
    /// Whether the keyboard and the pointer can choose this row.
    pub fn pressable(&self) -> bool {
        matches!(self, Self::Chat(_))
    }

    fn height(&self) -> f32 {
        match self {
            Self::Chat(_) => CHAT_H,
            Self::Note(_) => NOTE_H,
            Self::Notice(_) => NOTE_H * 2.0,
            Self::PinnedHeading => HEADING_H,
            Self::GroupGap => GROUP_GAP,
        }
    }

    fn gap_above(&self) -> f32 {
        match self {
            Self::Notice(_) => NOTICE_GAP,
            _ => 0.0,
        }
    }
}

/// Every rect inside a row. See [`ChatsSurface::row_parts`].
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct RowParts {
    /// A private chat's mark. Zero-width on every other row.
    pub lead: Rect,
    /// The title — a chat's title, a note's sentence.
    pub title: Rect,
    /// The status mark's slot at the row's end (D83). Zero-width on a row
    /// with nothing to show there, so a title keeps the whole width.
    pub end: Rect,
    /// The pin (D86): on a hovered or keyboard row, and always on a pinned
    /// one. Zero-width where it is not shown.
    pub pin: Rect,
    /// **⋯**, which opens the row's menu (D86): on a hovered or keyboard
    /// row only, where it takes the status mark's slot. Zero-width
    /// elsewhere.
    pub more: Rect,
}

/// A button at a row's end (D86).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RowButton {
    Pin,
    More,
}

impl ChatsSurface {
    /// Every row, in order, with its rect — **before** the band is applied,
    /// because the scroll extent and a screen reader both need the rows that
    /// are off screen. `&self`: an accessibility projection reads this.
    pub fn layout(&self, area: Rect, top: f32) -> Vec<(Row, Rect)> {
        let plan = self.plan();
        let mut out = Vec::with_capacity(plan.len());
        let mut y = top - self.scroll;
        for row in plan {
            y += row.gap_above();
            let h = row.height();
            out.push((row, Rect::new(area.x, y, area.w.max(0.0), h)));
            y += h;
        }
        out
    }

    /// [`layout`](Self::layout), counted.
    pub(crate) fn laid_out(&mut self, area: Rect, top: f32) -> Vec<(Row, Rect)> {
        let rows = self.layout(area, top);
        self.work.rows = rows.len();
        rows
    }

    /// The rows, with nothing said about where they are.
    fn plan(&self) -> Vec<Row> {
        let mut plan = Vec::new();
        match self.load {
            Load::Cold | Load::Loading if self.chats.is_empty() => {
                plan.push(Row::Notice(Notice::Loading));
                return plan;
            }
            Load::Unavailable => plan.push(Row::Notice(Notice::Unavailable)),
            _ => {}
        }
        if self.chats.is_empty() {
            if self.load == Load::Ready {
                plan.push(Row::Notice(Notice::Empty));
            }
            return plan;
        }
        // **Pinned first, under its heading** (D85) — the chats are already
        // in that order (`sort_for_list`); the heading and the air after the
        // group are only drawn when a chat is pinned.
        let pinned = self
            .chats
            .iter()
            .take_while(|chat| chat.pinned_at.is_some())
            .count();
        if pinned > 0 {
            plan.push(Row::PinnedHeading);
        }
        for (index, chat) in self.chats.iter().enumerate() {
            if pinned > 0 && index == pinned {
                plan.push(Row::GroupGap);
            }
            plan.push(Row::Chat(index));
            if self.deleting.contains_key(&chat.id) {
                plan.push(Row::Note(index));
            }
        }
        plan
    }

    /// The band rows are drawn and pressed inside: from `top` to the area's
    /// floor, less the footer's air.
    pub fn band(&self, area: Rect, top: f32) -> Rect {
        let bottom = area.y + area.h - FOOTER_PAD;
        Rect::new(area.x, top, area.w.max(0.0), (bottom - top).max(0.0))
    }

    /// The rows inside the band — the cull, and the hit test's bounds.
    pub fn rows(&mut self, area: Rect, top: f32) -> Vec<(Row, Rect)> {
        if area.w <= 0.0 {
            self.work.rows = 0;
            return Vec::new();
        }
        let band = self.band(area, top);
        self.laid_out(area, top)
            .into_iter()
            .filter(|(_, r)| r.y + r.h > band.y && r.y < band.y + band.h)
            .collect()
    }

    /// Every rect inside a row: the full width inside the pad, the way a
    /// Documents row is laid, with the private mark's slot taken only on a
    /// private row.
    pub fn row_parts(&self, row: Row, r: Rect) -> RowParts {
        let inner_x = r.x + theme::NAV_PAD;
        let inner_w = (r.w - theme::NAV_PAD * 2.0).max(0.0);
        let private = match row {
            Row::Chat(index) => self.chats.get(index).is_some_and(|chat| chat.private),
            _ => false,
        };
        // The row's end, right to left: slot 1 at the edge, slot 2 before it
        // (D86). A hovered or keyboard row shows ⋯ in slot 1 — over the
        // status mark — and the pin in slot 2; any other row shows its mark
        // in slot 1 and, when it is pinned, its pin beside it (or in slot 1
        // when it has no mark).
        let (buttons, pinned, marked) = match row {
            Row::Chat(index) => self.chats.get(index).map_or((false, false, false), |chat| {
                (
                    self.shows_row_buttons(row),
                    chat.pinned_at.is_some(),
                    self.status(chat.id) != crate::ChatStatus::Idle,
                )
            }),
            _ => (false, false, false),
        };
        let right = inner_x + inner_w;
        let slot = |n: f32| {
            let w = END_W.min(inner_w / n);
            Rect::new(right - w * n, r.y, w, r.h)
        };
        let none = Rect::new(right, r.y, 0.0, r.h);
        let (end, pin, more) = if buttons {
            (none, slot(2.0), slot(1.0))
        } else if marked {
            (slot(1.0), if pinned { slot(2.0) } else { none }, none)
        } else {
            (none, if pinned { slot(1.0) } else { none }, none)
        };
        let taken = [end, pin, more]
            .iter()
            .filter(|part| part.w > 0.0)
            .map(|part| part.x)
            .fold(right, f32::min);
        let lead_w = if private { LEAD_W.min(inner_w) } else { 0.0 };
        let lead = Rect::new(inner_x, r.y, lead_w, r.h);
        let text_x = if private {
            inner_x + lead_w + LEAD_GAP
        } else {
            inner_x
        };
        let text_end = if taken < right {
            taken - END_GAP
        } else {
            right
        };
        let text_w = (text_end - text_x).max(0.0);
        RowParts {
            lead,
            title: Rect::new(text_x, r.y, text_w, r.h),
            end,
            pin,
            more,
        }
    }

    /// Whether a row shows its ⋯ and pin: under the pointer, or under the
    /// keyboard (D86).
    pub(crate) fn shows_row_buttons(&self, row: Row) -> bool {
        matches!(row, Row::Chat(_)) && (self.hover == Some(row) || self.focus_row() == Some(row))
    }

    /// Which of a row's buttons is under a point, if one is shown there.
    pub(crate) fn button_at(&self, row: Row, r: Rect, x: f32, y: f32) -> Option<RowButton> {
        let parts = self.row_parts(row, r);
        if parts.more.w > 0.0 && parts.more.contains(x, y) {
            Some(RowButton::More)
        } else if parts.pin.w > 0.0 && parts.pin.contains(x, y) {
            Some(RowButton::Pin)
        } else {
            None
        }
    }

    /// The row the keyboard is on, if it is on one that exists.
    pub(crate) fn focus_row(&self) -> Option<Row> {
        let Focus::Chat(id) = self.focus?;
        self.chats
            .iter()
            .position(|chat| chat.id == id)
            .map(Row::Chat)
    }

    /// How tall the list is at this width, unscrolled.
    fn content_h(&self, area: Rect, top: f32) -> f32 {
        self.layout(area, top)
            .last()
            .map(|(_, r)| r.y + r.h + self.scroll - top)
            .unwrap_or(0.0)
    }

    pub(crate) fn clamp_scroll(&mut self, area: Rect, top: f32) {
        let band = self.band(area, top).h;
        let max = (self.content_h(area, top) - band).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    pub fn scroll_by(&mut self, area: Rect, top: f32, dy: f32) {
        self.scroll -= dy;
        self.clamp_scroll(area, top);
    }

    /// Bring one row into the band, moving as little as it takes — nothing
    /// when it is already there. A focus ring below the fold is a focus a
    /// person cannot see.
    pub(crate) fn scroll_into_view(&mut self, row: Row, area: Rect, top: f32) {
        let Some((_, r)) = self
            .layout(area, top)
            .into_iter()
            .find(|(candidate, _)| *candidate == row)
        else {
            return;
        };
        let band = self.band(area, top);
        if r.y < band.y {
            self.scroll -= band.y - r.y;
        } else if r.y + r.h > band.y + band.h {
            self.scroll += r.y + r.h - (band.y + band.h);
        }
        self.clamp_scroll(area, top);
    }

    /// Which row is under a point, if any — inside the band only.
    pub fn row_at(&mut self, area: Rect, top: f32, x: f32, y: f32) -> Option<(Row, Rect)> {
        if !self.band(area, top).contains(x, y) {
            return None;
        }
        self.rows(area, top)
            .into_iter()
            .find(|(_, r)| y >= r.y && y < r.y + r.h)
    }

    /// Where a chat's title sits — what a host draws its rename field over,
    /// so the field covers the words it replaces. Inside the band only, for
    /// the hit test's reason: `None` when the list does not hold the chat or
    /// its row is scrolled out of view.
    pub fn title_rect(&self, area: Rect, top: f32, chat: crate::ChatId) -> Option<Rect> {
        let index = self
            .chats
            .iter()
            .position(|candidate| candidate.id == chat)?;
        let band = self.band(area, top);
        let (row, rect) = self
            .layout(area, top)
            .into_iter()
            .find(|(row, _)| *row == Row::Chat(index))?;
        (rect.y + rect.h > band.y && rect.y < band.y + band.h)
            .then(|| self.row_parts(row, rect).title)
    }

    /// Every chat and where its row sits, unculled and in order — what a
    /// screen reader is told, including the rows scrolled out of the band.
    pub fn chat_rects(&self, area: Rect, top: f32) -> Vec<(crate::ChatId, Rect)> {
        self.layout(area, top)
            .into_iter()
            .filter_map(|(row, rect)| match row {
                Row::Chat(index) => self.chats.get(index).map(|chat| (chat.id, rect)),
                _ => None,
            })
            .collect()
    }
}
