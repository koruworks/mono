// SPDX-License-Identifier: MPL-2.0

//! **Quick switch**: Ctrl+Tab's list of the open tabs, most recently used
//! first.
//!
//! **It stays until something is chosen or it is put away** (owner,
//! 2026-11-16): Enter or a click on a row goes there, Escape or a click
//! anywhere else closes it. Tab, Ctrl+Tab and the arrows move the highlight,
//! Shift the other way. Letting go of Control does nothing. It opens with
//! the highlight on the tab used before this one, so Ctrl+Tab, Enter is *back
//! to where I was*.
//!
//! **Typing narrows it** to the tabs whose name holds every word typed, the
//! sidebar quick filter's own matching, and the highlight goes to the first
//! answer. More tabs than fit scroll: with the wheel, and with the highlight.
//!
//! It stands on the scrim and the panel search stands on, so the two read as
//! the same kind of thing.
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
use koru_ui::text_input::{InputStyle, TextInput};
use vello::Scene;
use vello::kurbo::Affine;

use crate::panes::explore::quick_filter::{spans, words};

const WIDTH: f32 = 440.0;
const PAD: f32 = 8.0;
/// The search row at the top, and the air under its hairline. **It is the
/// heading**: the panel used to carry a *Quick Switch* title with its chord
/// above it, which said nothing the list does not (owner, 2026-11-16).
const FIELD_H: f32 = 44.0;
const ROW_H: f32 = 38.0;
const ROW_GAP: f32 = 2.0;
const ROW_RADIUS: f32 = 7.0;
const INSET: f32 = 12.0;
const ICON: f32 = 16.0;
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
    /// The entries the typed words let through, in order: indices into
    /// `entries`. All of them while nothing is typed.
    matching: Vec<usize>,
    /// The highlighted answer: an index into `matching`.
    selected: usize,
    /// The first answer shown: an index into `matching`.
    top: usize,
    /// The answer under the pointer: an index into `matching`.
    hovered: Option<usize>,
    /// What is typed, in the shared one-line input. Made at the first key
    /// that types, because making one needs the text engine.
    input: Option<TextInput>,
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
            matching: (0..entries.len()).collect(),
            entries,
            selected: 0,
            top: 0,
            hovered: None,
            input: None,
        };
        switch.step(delta);
        Some(switch)
    }

    /// Move the highlight, round the ends, and keep it in view.
    pub fn step(&mut self, delta: i32) {
        let n = self.matching.len() as i32;
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
        self.top = self.top.min(self.matching.len().saturating_sub(MAX_ROWS));
    }

    /// The wheel: `rows` down, or up when negative. `true` when it moved.
    pub fn scroll_rows(&mut self, rows: i32) -> bool {
        let before = self.top;
        self.top = (self.top as i32 + rows).max(0) as usize;
        self.clamp_top();
        self.hovered = None;
        self.top != before
    }

    /// The tab to go to, and the split it is in. `None` when nothing typed
    /// matches a tab.
    pub fn chosen(&self) -> Option<(super::panes::PanePath, u64)> {
        let entry = &self.entries[*self.matching.get(self.selected)?];
        Some((entry.group.clone(), entry.tab))
    }

    fn query(&self) -> String {
        self.input
            .as_ref()
            .map(TextInput::value)
            .unwrap_or_default()
    }

    /// **A key that is not the list's own**: it edits what is typed, as any
    /// field does, and the list narrows. `true` when the field took it.
    pub fn type_key(
        &mut self,
        engine: &mut TextEngine,
        key: &winit::keyboard::Key,
        text: Option<&str>,
        mods: winit::keyboard::ModifiersState,
    ) -> bool {
        let before = self.query();
        let input = self
            .input
            .get_or_insert_with(|| TextInput::new(engine, theme::role::row()));
        if matches!(
            input.key(engine, key, text, mods),
            koru_ui::text_field::Typed::Passed
        ) {
            return false;
        }
        if self.query() != before {
            self.refilter();
        }
        true
    }

    fn refilter(&mut self) {
        let typed = words(&self.query());
        self.matching = (0..self.entries.len())
            .filter(|&i| typed.is_empty() || spans(&self.entries[i].label, &typed).is_some())
            .collect();
        // The first answer — or, with nothing typed, the tab before this
        // one again.
        self.selected = if typed.is_empty() && self.matching.len() > 1 {
            1
        } else {
            0
        };
        self.top = 0;
        self.hovered = None;
    }

    #[cfg(test)]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The answers in view: indices into `matching`.
    fn shown(&self) -> std::ops::Range<usize> {
        self.top..(self.top + MAX_ROWS).min(self.matching.len())
    }

    pub fn panel(&self, viewport: (f32, f32)) -> Rect {
        // The list keeps the height of what it opened with, up to the cap,
        // so typing narrows the rows without the panel jumping about.
        let rows = self.entries.len().min(MAX_ROWS) as f32;
        let w = WIDTH.min((viewport.0 - 32.0).max(120.0));
        let h = PAD * 2.0 + FIELD_H + rows * ROW_H + (rows - 1.0).max(0.0) * ROW_GAP;
        Rect::new(
            ((viewport.0 - w) * 0.5).max(0.0),
            ((viewport.1 - h) * 0.33).max(0.0),
            w,
            h,
        )
    }

    fn field_rect(&self, viewport: (f32, f32)) -> Rect {
        let panel = self.panel(viewport);
        Rect::new(
            panel.x + PAD,
            panel.y + PAD,
            panel.w - PAD * 2.0,
            FIELD_H - 8.0,
        )
    }

    fn row_rect(&self, viewport: (f32, f32), slot: usize) -> Rect {
        let panel = self.panel(viewport);
        Rect::new(
            panel.x + PAD,
            panel.y + PAD + FIELD_H + slot as f32 * (ROW_H + ROW_GAP),
            panel.w - PAD * 2.0,
            ROW_H,
        )
    }

    /// Which answer the pointer is over: an index into `matching`.
    fn row_at(&self, viewport: (f32, f32), x: f32, y: f32) -> Option<usize> {
        self.shown()
            .enumerate()
            .find(|(slot, _)| self.row_rect(viewport, *slot).contains(x, y))
            .map(|(_, index)| index)
    }

    /// The pointer moved. `true` when the row under it changed, so the
    /// caller repaints only then.
    pub fn hover(&mut self, viewport: (f32, f32), x: f32, y: f32) -> bool {
        let over = self.row_at(viewport, x, y);
        let moved = over != self.hovered;
        self.hovered = over;
        moved
    }

    /// Whether a point is on the panel: a press there that is not on a row
    /// (the heading, the field) keeps the list open.
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

        // What is typed. No box of its own: the panel is the field, as it
        // is in search.
        let field = self.field_rect(viewport);
        let glass = engine.icon_layout(icons::SEARCH, 14.0);
        scene::draw_layout(
            canvas,
            root,
            &glass,
            field.x + INSET + 1.0,
            field.y + (field.h - glass.height()) * 0.5,
            Some(p.text_dim),
        );
        let text_rect = Rect::new(
            field.x + INSET + ICON + 10.0,
            field.y,
            (field.w - INSET * 2.0 - ICON - 10.0).max(0.0),
            field.h,
        );
        // The hairline under the search row, edge to edge, as the heading's
        // was.
        scene::fill(
            canvas,
            root,
            p.panel_border,
            &scene::rect(panel.x, field.y + field.h + 3.0, panel.w, 1.0),
        );
        let input = self
            .input
            .get_or_insert_with(|| TextInput::new(engine, theme::role::row()));
        input.draw(
            canvas,
            root,
            engine,
            text_rect,
            InputStyle::bare(theme::role::row()),
            Some("Search open tabs"),
            true,
        );

        let typed = words(&self.query());
        if self.matching.is_empty() {
            let none = engine.simple_layout("No open tab matches", theme::role::row());
            let r = self.row_rect(viewport, 0);
            scene::draw_layout_first_line(
                canvas,
                root,
                &none,
                r.x + INSET,
                r.y + (r.h - scene::first_line_height(&none)) * 0.5,
                Some(p.text_dim),
            );
        }
        for (slot, index) in self.shown().enumerate() {
            let entry = &self.entries[self.matching[index]];
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

            // The trailing word: a chat's state, or *Current*.
            let mut right = r.x + r.w - INSET;
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
            match spans(&entry.label, &typed) {
                Some(found) => koru_ui::highlighted_text::draw_highlighted_line(
                    canvas,
                    root,
                    engine,
                    &entry.label,
                    found,
                    label_x,
                    label_y,
                    label_w,
                    theme::role::row(),
                    p.text,
                    p.text,
                    p.highlights[0].1,
                ),
                None => {
                    let shown = engine.elided(&entry.label, theme::role::row(), label_w);
                    let label = engine.simple_layout(&shown, theme::role::row());
                    scene::draw_layout_first_line(
                        canvas,
                        root,
                        &label,
                        label_x,
                        label_y,
                        Some(p.text),
                    );
                }
            }
        }

        // More answers than rows: a thumb down the panel's inside edge.
        let n = self.matching.len();
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
        let field = switch.field_rect(viewport);
        assert!(switch.contains(viewport, field.x + 5.0, field.y + 5.0));
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

    /// Typing narrows the list to the names that hold it and puts the
    /// highlight on the first answer; emptied, the list is whole again.
    #[test]
    fn typing_narrows_the_list_to_matching_names() {
        use winit::keyboard::{Key, ModifiersState, NamedKey};
        let mut engine = TextEngine::new();
        let mut list = entries(12);
        list[7].label = "Roadmap".into();
        let mut switch = QuickSwitch::new(list, 1).unwrap();
        for ch in ["r", "o", "a"] {
            assert!(switch.type_key(
                &mut engine,
                &Key::Character(ch.into()),
                Some(ch),
                ModifiersState::empty()
            ));
        }
        assert_eq!(switch.matching, vec![7]);
        assert_eq!(tab(&switch), Some(7));
        assert!(switch.type_key(
            &mut engine,
            &Key::Character("z".into()),
            Some("z"),
            ModifiersState::empty()
        ));
        assert_eq!(tab(&switch), None, "nothing matches, nothing to go to");
        // Ctrl+Backspace takes the word, as in any field.
        assert!(switch.type_key(
            &mut engine,
            &Key::Named(NamedKey::Backspace),
            None,
            ModifiersState::CONTROL
        ));
        assert_eq!(switch.matching.len(), 12);
        assert_eq!(tab(&switch), Some(1), "back on the tab before this one");
    }
}
