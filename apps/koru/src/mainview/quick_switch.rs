// SPDX-License-Identifier: MPL-2.0

//! **Quick switch**: Ctrl+Tab's list of the open tabs, most recently used
//! first.
//!
//! **Ctrl+Tab is hold-to-cycle, release-to-switch** (owner, 2026-11-18):
//! the first press stands the list up on the tab used before this one, and
//! each further press moves the highlight; letting go of Control goes to the
//! highlighted tab and puts the list away. Enter or a click on a row goes
//! there, Escape or a click anywhere else closes it. Tab, Ctrl+Tab and the
//! arrows move the highlight, Shift the other way. It opens with the
//! highlight on the tab used before this one, so Ctrl+Tab, Enter is *back to
//! where I was*.
//!
//! **Hovering a row shows its ✕**, which closes that tab and keeps the list
//! open so several can be closed in one pass. More tabs than fit scroll: with
//! the wheel, and with the highlight.
//!
//! It stands on the scrim the panel search stands on, so the two read as the
//! same kind of thing.
//!
//! **Every tab of the window, whichever split it is in**, and in the order
//! they were last *used*: written in, or a task made on. A tab only looked
//! at has not been used, and falls after the ones that were, by when it last
//! showed (`TabStrip::by_recency`). The caller builds that order; this file
//! draws it and walks it.

use koru_draw::geometry::Rect;
use koru_draw::scene;
use koru_draw::text::TextEngine;
use koru_draw::{icons, theme};
use koru_shell_chrome::status_mark::StatusMark;
use vello::Scene;
use vello::kurbo::Affine;

const WIDTH: f32 = 440.0;
const PAD: f32 = 8.0;
const ROW_H: f32 = 38.0;
const ROW_GAP: f32 = 2.0;
const ROW_RADIUS: f32 = 7.0;
const INSET: f32 = 12.0;
const ICON: f32 = 16.0;
/// The air reserved at the right of every row for the hovered ✕, so a hover
/// does not move the label. The ✕ glyph itself is [`ICON`] wide, centred in
/// this.
const CLOSE: f32 = 24.0;
/// More tabs than this and the list scrolls.
const MAX_ROWS: usize = 10;

/// One open tab, as the list shows it.
pub(super) struct Entry {
    /// The tab's own identity, so a tab closed or moved while the list
    /// stands is still found, or found gone.
    pub tab: u64,
    /// The split it is in.
    pub group: super::panes::PanePath,
    pub label: String,
    /// A place's glyph; a document's own emoji is `emoji`.
    pub glyph: char,
    pub emoji: Option<String>,
    /// The Bot's current face and name, shared with its tab-strip identity.
    pub face: Option<(u8, String)>,
    pub current: bool,
    pub status: Option<StatusMark>,
}

pub(super) struct QuickSwitch {
    entries: Vec<Entry>,
    /// The highlighted answer: an index into `entries`.
    selected: usize,
    /// The first answer shown: an index into `entries`.
    top: usize,
    /// The answer under the pointer: an index into `entries`.
    hovered: Option<usize>,
    /// The answer whose ✕ is under the pointer: an index into `entries`.
    hovered_close: Option<usize>,
}

impl QuickSwitch {
    /// `entries` most recent first, the tab in front at their head.
    /// `delta` is the press that opened it: forward lands on the tab before
    /// this one, back on the least recent.
    pub fn new(entries: Vec<Entry>, delta: i32) -> Option<Self> {
        if entries.len() < 2 {
            return None;
        }
        let mut switch = Self {
            entries,
            selected: 0,
            top: 0,
            hovered: None,
            hovered_close: None,
        };
        switch.step(delta);
        Some(switch)
    }

    /// Move the highlight, round the ends, and keep it in view.
    pub fn step(&mut self, delta: i32) {
        let n = self.entries.len() as i32;
        if n == 0 {
            return;
        }
        self.selected = (self.selected as i32 + delta).rem_euclid(n) as usize;
        self.reveal_selected();
    }

    fn reveal_selected(&mut self) {
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + MAX_ROWS {
            self.top = self.selected + 1 - MAX_ROWS;
        }
        self.clamp_top();
    }

    fn clamp_top(&mut self) {
        self.top = self.top.min(self.entries.len().saturating_sub(MAX_ROWS));
    }

    /// The wheel: `rows` down, or up when negative. `true` when it moved.
    pub fn scroll_rows(&mut self, rows: i32) -> bool {
        let before = self.top;
        self.top = (self.top as i32 + rows).max(0) as usize;
        self.clamp_top();
        self.hovered = None;
        self.hovered_close = None;
        self.top != before
    }

    /// The tab to go to, and the split it is in.
    pub fn chosen(&self) -> Option<(super::panes::PanePath, u64)> {
        let entry = self.entries.get(self.selected)?;
        Some((entry.group.clone(), entry.tab))
    }

    /// The entry at `index`, for the caller that closes it by identity.
    pub fn entry(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index)
    }

    #[cfg(test)]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The answers in view: indices into `entries`.
    fn shown(&self) -> std::ops::Range<usize> {
        self.top..(self.top + MAX_ROWS).min(self.entries.len())
    }

    pub fn panel(&self, viewport: (f32, f32)) -> Rect {
        // The list keeps the height of what it opened with, up to the cap, so
        // the panel never jumps about as rows come and go.
        let rows = self.entries.len().min(MAX_ROWS) as f32;
        let w = WIDTH.min((viewport.0 - 32.0).max(120.0));
        let h = PAD * 2.0 + rows * ROW_H + (rows - 1.0).max(0.0) * ROW_GAP;
        Rect::new(
            ((viewport.0 - w) * 0.5).max(0.0),
            ((viewport.1 - h) * 0.33).max(0.0),
            w,
            h,
        )
    }

    fn row_rect(&self, viewport: (f32, f32), slot: usize) -> Rect {
        let panel = self.panel(viewport);
        Rect::new(
            panel.x + PAD,
            panel.y + PAD + slot as f32 * (ROW_H + ROW_GAP),
            panel.w - PAD * 2.0,
            ROW_H,
        )
    }

    /// The ✕ on a row: the right-hand [`CLOSE`] air, centred.
    fn close_rect(&self, viewport: (f32, f32), slot: usize) -> Rect {
        let r = self.row_rect(viewport, slot);
        Rect::new(
            r.x + r.w - INSET - CLOSE,
            r.y + (r.h - CLOSE) * 0.5,
            CLOSE,
            CLOSE,
        )
    }

    /// Which answer the pointer is over: an index into `entries`.
    fn row_at(&self, viewport: (f32, f32), x: f32, y: f32) -> Option<usize> {
        self.shown()
            .enumerate()
            .find(|(slot, _)| self.row_rect(viewport, *slot).contains(x, y))
            .map(|(_, index)| index)
    }

    /// Which answer's ✕ the pointer is over: an index into `entries`.
    pub fn close_at(&self, viewport: (f32, f32), x: f32, y: f32) -> Option<usize> {
        self.shown()
            .enumerate()
            .find(|(slot, _)| self.close_rect(viewport, *slot).contains(x, y))
            .map(|(_, index)| index)
    }

    /// The pointer moved. `true` when what is lit changed, so the caller
    /// repaints only then.
    pub fn hover(&mut self, viewport: (f32, f32), x: f32, y: f32) -> bool {
        let over = self.row_at(viewport, x, y);
        let over_close = self.close_at(viewport, x, y);
        let moved = over != self.hovered || over_close != self.hovered_close;
        self.hovered = over;
        self.hovered_close = over_close;
        moved
    }

    /// Whether a point is on the panel: a press there that is not on a row
    /// keeps the list open.
    pub fn contains(&self, viewport: (f32, f32), x: f32, y: f32) -> bool {
        self.panel(viewport).contains(x, y)
    }

    /// A press on a row highlights it and answers `true`: the caller goes
    /// there.
    pub fn press(&mut self, viewport: (f32, f32), x: f32, y: f32) -> bool {
        match self.row_at(viewport, x, y) {
            Some(index) => {
                self.selected = index;
                true
            }
            None => false,
        }
    }

    pub fn draw(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        viewport: (f32, f32),
    ) {
        let panel = self.panel(viewport);
        let p = theme::p();
        // Search's ground: the scrim over the whole window, and its panel.
        scene::fill(
            canvas,
            root,
            p.scrim,
            &scene::rect(0.0, 0.0, viewport.0, viewport.1),
        );
        scene::floating_panel(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(koru_search_ui::SURFACE_RADIUS),
        );

        for (slot, index) in self.shown().enumerate() {
            let entry = &self.entries[index];
            let r = self.row_rect(viewport, slot);
            let shape = scene::rrect(r.x, r.y, r.w, r.h, ROW_RADIUS);
            if index == self.selected {
                scene::fill(canvas, root, p.hover_tint, &shape);
                scene::stroke(canvas, root, p.accent, 1.5, &shape);
            } else if self.hovered == Some(index) {
                // Under the pointer: the tint alone. The ring stays the
                // keyboard's.
                scene::fill(canvas, root, p.hover_tint, &shape);
            }
            let icon_x = r.x + INSET;
            if let Some((face, name)) = &entry.face {
                koru_draw::face::Face::new(*face, name).draw_in(
                    canvas,
                    root,
                    engine,
                    Rect::new(icon_x, r.y + (r.h - ICON) * 0.5, ICON, ICON),
                    p,
                );
            } else {
                match &entry.emoji {
                    Some(emoji) => {
                        let glyph = engine.simple_layout(emoji, theme::role::row());
                        scene::draw_layout_first_line(
                            canvas,
                            root,
                            &glyph,
                            icon_x,
                            r.y + (r.h - scene::first_line_height(&glyph)) * 0.5,
                            None,
                        );
                    }
                    None => {
                        let glyph = engine.icon_layout(entry.glyph, ICON);
                        scene::draw_layout(
                            canvas,
                            root,
                            &glyph,
                            icon_x,
                            r.y + (r.h - glyph.height()) * 0.5,
                            Some(p.text_dim),
                        );
                    }
                }
            }

            // The trailing word: a chat's state, or *Current*. The ✕'s air is
            // reserved to the right of it, so the label never moves on hover.
            let mut right = r.x + r.w - INSET - CLOSE;
            if let Some(status) = entry.status {
                let (word, color) = match status {
                    StatusMark::Spinner { color, .. } => ("Running", color),
                    StatusMark::Question(color) => ("Needs you", color),
                    StatusMark::Dot(color) => ("New", color),
                };
                let text = engine.simple_layout(word, theme::role::hint());
                let chip_w = text.width() + 30.0;
                let chip = Rect::new(right - chip_w, r.y + (r.h - 22.0) * 0.5, chip_w, 22.0);
                scene::fill(
                    canvas,
                    root,
                    p.chip_bg,
                    &scene::rrect(chip.x, chip.y, chip.w, chip.h, chip.h * 0.5),
                );
                scene::fill(
                    canvas,
                    root,
                    color,
                    &scene::circle(chip.x + 11.0, chip.y + chip.h * 0.5, 3.0),
                );
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &text,
                    chip.x + 20.0,
                    chip.y + (chip.h - scene::first_line_height(&text)) * 0.5,
                    Some(p.text),
                );
                right = chip.x - 10.0;
            } else if entry.current {
                let text = engine.simple_layout("Current", theme::role::hint());
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &text,
                    right - text.width(),
                    r.y + (r.h - scene::first_line_height(&text)) * 0.5,
                    Some(p.text_dim),
                );
                right -= text.width() + 10.0;
            }

            let label_x = icon_x + ICON + 10.0;
            let label_w = (right - label_x).max(0.0);
            let line = engine.simple_layout(&entry.label, theme::role::row());
            let label_y = r.y + (r.h - scene::first_line_height(&line)) * 0.5;
            let shown = engine.elided(&entry.label, theme::role::row(), label_w);
            let label = engine.simple_layout(&shown, theme::role::row());
            scene::draw_layout_first_line(canvas, root, &label, label_x, label_y, Some(p.text));

            // The ✕, only on the row under the pointer. Darker when the
            // pointer is on the ✕ itself, like the strip's close box.
            if self.hovered == Some(index) {
                let close = self.close_rect(viewport, slot);
                let colour = if self.hovered_close == Some(index) {
                    p.text
                } else {
                    p.text_dim
                };
                let glyph = engine.icon_layout(icons::WINDOW_CLOSE, ICON);
                scene::draw_layout(
                    canvas,
                    root,
                    &glyph,
                    close.x + (close.w - glyph.width()) * 0.5,
                    close.y + (close.h - glyph.height()) * 0.5,
                    Some(colour),
                );
            }
        }

        // More answers than rows: a thumb down the panel's inside edge.
        let n = self.entries.len();
        if n > MAX_ROWS {
            let first = self.row_rect(viewport, 0);
            let track_h = MAX_ROWS as f32 * (ROW_H + ROW_GAP) - ROW_GAP;
            let thumb_h = (track_h * MAX_ROWS as f32 / n as f32).max(24.0);
            let travel = track_h - thumb_h;
            let at = self.top as f32 / (n - MAX_ROWS) as f32;
            scene::fill(
                canvas,
                root,
                p.scrollbar_thumb,
                &scene::rrect(
                    panel.x + panel.w - 6.0,
                    first.y + travel * at,
                    3.0,
                    thumb_h,
                    1.5,
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(n: u64) -> Vec<Entry> {
        (0..n)
            .map(|tab| Entry {
                tab,
                group: super::super::panes::PanePath::root(),
                label: format!("Tab {tab}"),
                glyph: icons::DOCUMENT,
                emoji: None,
                face: None,
                current: tab == 0,
                status: None,
            })
            .collect()
    }

    fn tab(switch: &QuickSwitch) -> Option<u64> {
        switch.chosen().map(|(_, tab)| tab)
    }

    /// One Ctrl+Tab is the tab before this one; further presses walk on and
    /// wrap; Shift walks back from the tab in front to the least recent.
    #[test]
    fn the_first_press_lands_on_the_tab_before_this_one() {
        let mut switch = QuickSwitch::new(entries(3), 1).unwrap();
        assert_eq!(tab(&switch), Some(1));
        switch.step(1);
        assert_eq!(tab(&switch), Some(2));
        switch.step(1);
        assert_eq!(tab(&switch), Some(0), "wraps to the tab in front");
        let back = QuickSwitch::new(entries(3), -1).unwrap();
        assert_eq!(tab(&back), Some(2));
        assert!(
            QuickSwitch::new(entries(1), 1).is_none(),
            "nothing to switch to"
        );
    }

    /// A press on a row chooses it; a press off the rows chooses nothing.
    #[test]
    fn a_press_chooses_the_row_under_it() {
        let viewport = (1000.0, 800.0);
        let mut switch = QuickSwitch::new(entries(4), 1).unwrap();
        let row = switch.row_rect(viewport, 3);
        assert!(switch.press(viewport, row.x + 5.0, row.y + 5.0));
        assert_eq!(tab(&switch), Some(3));
        assert!(!switch.press(viewport, 2.0, 2.0));
        assert!(!switch.contains(viewport, 2.0, 2.0));
    }

    /// A long list shows a window of rows that follows the highlight, and
    /// the wheel moves the window without moving the highlight.
    #[test]
    fn a_long_list_scrolls_with_the_highlight_and_the_wheel() {
        let mut switch = QuickSwitch::new(entries(25), 1).unwrap();
        assert_eq!(switch.shown(), 0..MAX_ROWS);
        for _ in 0..15 {
            switch.step(1);
        }
        assert_eq!(switch.selected(), 16);
        assert!(switch.shown().contains(&16));
        assert_eq!(switch.shown().len(), MAX_ROWS);
        assert!(switch.scroll_rows(-3));
        assert_eq!(switch.selected(), 16, "the wheel leaves the highlight");
        assert!(switch.scroll_rows(100));
        assert_eq!(switch.shown(), 15..25, "and stops at the end");
        assert!(!switch.scroll_rows(1));
    }

    /// The ✕ lives in a row's right-hand air: hovering it answers which row,
    /// and the hover itself lights the row and its ✕ together.
    #[test]
    fn the_close_box_answers_for_the_row_under_it() {
        let viewport = (1000.0, 800.0);
        let mut switch = QuickSwitch::new(entries(4), 1).unwrap();
        // First shown row's ✕: pointer on its centre.
        let close = switch.close_rect(viewport, 0);
        let (x, y) = (close.x + close.w * 0.5, close.y + close.h * 0.5);
        assert!(switch.hover(viewport, x, y));
        assert_eq!(
            switch.hovered,
            Some(0),
            "slot 0 shows entry 0, the tab in front"
        );
        assert_eq!(switch.hovered_close, Some(0));
        assert_eq!(switch.close_at(viewport, x, y), Some(0));
        // Off the ✕ but on the same row: the row is hovered, its ✕ is not.
        let row = switch.row_rect(viewport, 0);
        assert!(switch.hover(viewport, row.x + 5.0, row.y + 5.0));
        assert_eq!(switch.hovered, Some(0));
        assert_eq!(switch.hovered_close, None);
        assert_eq!(switch.close_at(viewport, row.x + 5.0, row.y + 5.0), None);
    }
}
