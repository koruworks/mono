// SPDX-License-Identifier: MPL-2.0

//! **The quick filter**: type in the sidebar and its rows narrow to the ones
//! whose name holds what was typed.
//!
//! A projection over the rows the pane already holds — no query, no store,
//! no index — so it is instant, and it is about *names*: what is inside a
//! document is search's to find.
//!
//! **When typing filters.** While the sidebar holds the keyboard and is
//! *armed*. It is armed by coming in on the keyboard (Ctrl+E, the arrows) or
//! by a press on the field or on a row that does not hand the keyboard to a
//! page; it is disarmed by opening a document or a chat with a click or
//! Enter, because then the next letters are meant for that page (owner,
//! 2026-11-16). The filter itself stays, with its rows, until it is cleared:
//! Escape, the `×`, or another view. Escape does that whenever the pane holds
//! the keyboard, armed or not — a press on the pane's empty ground (a filter
//! that matched nothing has no row to land on) is what gives it the keyboard
//! back, so the next Escape can put the filter away.
//!
//! **What shows.** A row whose name holds every word typed, whatever the
//! case, and above it the rows it is inside, dim, so a match in a shut branch
//! is found where it lives. Folds are not touched: clearing the filter shows
//! the tree exactly as it was.

use super::*;
use koru_ui::highlighted_text::Span;

/// The field's height, and the band it takes above the rows with its air.
const FIELD_H: f32 = 30.0;
pub(super) const BAND_H: f32 = FIELD_H + 8.0;
const CLEAR_W: f32 = 26.0;

/// What was typed, as the words a name must hold: lowercased, split at
/// whitespace.
pub(crate) fn words(query: &str) -> Vec<Vec<char>> {
    query
        .split_whitespace()
        .map(|word| word.chars().map(lower).collect())
        .collect()
}

fn lower(ch: char) -> char {
    ch.to_lowercase().next().unwrap_or(ch)
}

/// Where `label` holds every one of `words`, or `None` when it does not hold
/// them all. The spans are byte ranges into `label`, in order, not
/// overlapping. No words is no match.
pub(crate) fn spans(label: &str, words: &[Vec<char>]) -> Option<Vec<Span>> {
    if words.is_empty() {
        return None;
    }
    let chars: Vec<(usize, char)> = label
        .char_indices()
        .map(|(at, ch)| (at, lower(ch)))
        .collect();
    let mut found: Vec<Span> = Vec::with_capacity(words.len());
    for word in words {
        let at = (0..chars.len()).find(|&start| {
            chars.len() - start >= word.len()
                && word
                    .iter()
                    .enumerate()
                    .all(|(i, ch)| chars[start + i].1 == *ch)
        })?;
        let end = chars
            .get(at + word.len())
            .map_or(label.len(), |(byte, _)| *byte);
        found.push(Span {
            start: chars[at].0,
            end,
        });
    }
    found.sort_by_key(|span| span.start);
    let mut merged: Vec<Span> = Vec::with_capacity(found.len());
    for span in found {
        match merged.last_mut() {
            Some(last) if span.start <= last.end => last.end = last.end.max(span.end),
            _ => merged.push(span),
        }
    }
    Some(merged)
}

/// Given each row's depth and whether it matches, which rows stay: a match,
/// and every row it is inside. `(keep, dim)` — `dim` is kept without matching.
pub(crate) fn keep_with_ancestors(rows: &[(u16, bool)]) -> (Vec<bool>, Vec<bool>) {
    let mut keep = vec![false; rows.len()];
    let mut above: Vec<usize> = Vec::new();
    for (i, (depth, matches)) in rows.iter().enumerate() {
        while above.last().is_some_and(|&top| rows[top].0 >= *depth) {
            above.pop();
        }
        if *matches {
            keep[i] = true;
            for &ancestor in &above {
                keep[ancestor] = true;
            }
        }
        above.push(i);
    }
    let dim = keep
        .iter()
        .zip(rows)
        .map(|(kept, (_, matches))| *kept && !*matches)
        .collect();
    (keep, dim)
}

impl ExploreView {
    /// What is typed in the filter, while it is showing.
    pub fn quick_filter(&self) -> Option<&str> {
        self.quick.as_deref()
    }

    pub(super) fn quick_words(&self) -> Vec<Vec<char>> {
        self.quick.as_deref().map(words).unwrap_or_default()
    }

    /// Whether this view has rows a name can narrow.
    fn quick_filterable(&self) -> bool {
        matches!(
            self.mode,
            NavigatorMode::Documents
                | NavigatorMode::Files
                | NavigatorMode::Canvases
                | NavigatorMode::Tasks
                | NavigatorMode::Chats
        )
    }

    /// Arm or disarm type-to-filter. See the module comment for who calls
    /// this with what.
    pub fn arm_quick_filter(&mut self, armed: bool) {
        self.quick_armed = armed;
    }

    #[cfg(test)]
    pub fn quick_filter_armed(&self) -> bool {
        self.quick_armed
    }

    /// The room the field takes above the rows.
    pub(super) fn quick_band(&self) -> f32 {
        if self.quick.is_some() { BAND_H } else { 0.0 }
    }

    fn quick_field_rect(&self, area: Rect) -> Rect {
        Rect::new(
            area.x + theme::NAV_PAD,
            self.heading_band(area).y + HEADER_H + theme::NAV_PAD,
            (area.w - theme::NAV_PAD * 2.0).max(0.0),
            FIELD_H,
        )
    }

    fn quick_clear_rect(&self, area: Rect) -> Rect {
        let field = self.quick_field_rect(area);
        Rect::new(field.x + field.w - CLEAR_W - 2.0, field.y, CLEAR_W, field.h)
    }

    fn quick_search_rect(&self, area: Rect) -> Rect {
        Rect::new(
            area.x + theme::NAV_PAD,
            self.rows_top(area) + theme::NAV_ROW_H,
            (area.w - theme::NAV_PAD * 2.0).max(0.0),
            theme::NAV_ROW_H,
        )
    }

    /// **A key, while the sidebar holds the keyboard.** `true` when the
    /// filter took it.
    ///
    /// The field is the shared one-line input, so it edits as every other
    /// field does: word and line deletes, the caret keys, selection, paste.
    /// The vertical arrows, Enter and Tab are never its own: they stay the
    /// rows' keys. Before anything is typed only a letter starts it.
    pub fn quick_filter_key(
        &mut self,
        engine: &mut TextEngine,
        key: &winit::keyboard::Key,
        text: Option<&str>,
        mods: winit::keyboard::ModifiersState,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if !self.quick_filterable() {
            return false;
        }
        // A filter that is showing goes away on Escape whether or not the
        // next letter would come here. The second Escape is not the filter's:
        // with nothing left to clear, it falls through and gives the keyboard
        // back.
        if self.quick.is_some() && matches!(key, Key::Named(NamedKey::Escape)) {
            self.clear_quick_filter();
            return true;
        }
        if !self.quick_armed {
            return false;
        }
        if matches!(
            key,
            Key::Named(NamedKey::Enter | NamedKey::Tab | NamedKey::ArrowUp | NamedKey::ArrowDown)
        ) {
            return false;
        }
        if self.quick.is_none() {
            // Nothing typed yet: only what types starts it, and never a
            // chord or a lone space (Space is a row's own key).
            let types =
                text.is_some_and(|t| t.chars().any(|ch| !ch.is_control() && !ch.is_whitespace()));
            if !types || mods.control_key() || mods.alt_key() || mods.super_key() {
                return false;
            }
            self.quick_input = Some(koru_ui::text_input::TextInput::new(
                engine,
                theme::role::row(),
            ));
        }
        let Some(input) = self.quick_input.as_mut() else {
            return false;
        };
        if matches!(
            input.key(engine, key, text, mods),
            koru_ui::text_field::Typed::Passed
        ) {
            return false;
        }
        let value = input.value();
        let next = (!value.is_empty()).then_some(value);
        if next.is_none() {
            // Emptied by hand is put away: there is nothing to filter by.
            self.quick_input = None;
        }
        if next != self.quick {
            self.quick = next;
            self.quick_changed();
        }
        true
    }

    /// Put the filter away. `true` when there was one.
    pub fn clear_quick_filter(&mut self) -> bool {
        self.quick_holds_keyboard = false;
        if self.quick.take().is_none() {
            return false;
        }
        self.quick_input = None;
        self.quick_changed();
        true
    }

    /// The filter's words moved: tell the two views that keep their own
    /// rows, and build this one's again.
    fn quick_changed(&mut self) {
        self.scroll = 0.0;
        let typed = self.quick_words();
        let on = self.quick.is_some() && !typed.is_empty();
        self.rail.set_filter(on.then(|| typed.clone()));
        self.chats
            .set_filter(on.then(|| self.quick.clone().unwrap_or_default()));
        self.quick_stale = true;
        if self.mode.kind().is_some() && !self.manifest_entries.is_empty() {
            self.rebuild_manifest_rows();
        }
        // **The keyboard goes to the first answer**, so Enter opens it and
        // the arrows walk on from it.
        if on {
            match self.mode {
                NavigatorMode::Tasks => self.rail.focus_first_match(),
                NavigatorMode::Chats => self.chats.focus_first_match(),
                _ => {}
            }
        }
    }

    /// Whether the rows have to be built again for the filter — a caller
    /// that builds them from the document asks this.
    pub fn take_quick_filter_stale(&mut self) -> bool {
        std::mem::take(&mut self.quick_stale)
    }

    /// Whether the folds are to be read as all open: while filtering, a
    /// match in a shut branch must be reachable.
    pub(super) fn quick_opens_folds(&self) -> bool {
        self.quick.is_some() && !self.quick_words().is_empty()
    }

    /// **Narrow the rows just built to the filter.** Called by every path
    /// that builds `rows`, after the keyboard's row is restored.
    pub(super) fn apply_quick_filter(&mut self) {
        self.quick_dim.clear();
        self.quick_matches = 0;
        let typed = self.quick_words();
        if typed.is_empty() || self.mode.kind().is_none() {
            return;
        }
        let facts: Vec<(u16, bool)> = self
            .rows
            .iter()
            .map(|row| (row.depth, spans(&row.label, &typed).is_some()))
            .collect();
        let (keep, dim) = keep_with_ancestors(&facts);
        self.quick_matches = facts.iter().filter(|(_, matches)| *matches).count();
        let mut i = 0;
        self.rows.retain(|_| {
            i += 1;
            keep[i - 1]
        });
        let mut kept = keep
            .iter()
            .zip(&dim)
            .filter(|(keep, _)| **keep)
            .map(|(_, dim)| *dim);
        for row in &self.rows {
            if kept.next() == Some(true) {
                self.quick_dim.insert(row.node);
            }
        }
        // The keyboard sits on a match: the first, unless it is on one.
        if self.focus.is_some() || self.focus_parked.is_some() {
            let on_match = self.focus.is_some_and(|node| {
                self.rows.iter().any(|row| row.node == node) && !self.quick_dim.contains(&node)
            });
            if !on_match {
                self.focus = self
                    .rows
                    .iter()
                    .find(|row| !self.quick_dim.contains(&row.node))
                    .map(|row| row.node)
                    .or(self.focus);
            }
        }
    }

    /// Where a row's name holds the filter, for the highlight.
    pub(super) fn quick_spans(&self, label: &str) -> Vec<Span> {
        let typed = self.quick_words();
        spans(label, &typed).unwrap_or_default()
    }

    /// How many rows the filter matched, in whichever view is showing.
    pub fn quick_match_count(&self) -> usize {
        match self.mode {
            NavigatorMode::Tasks => self.rail.filter_matches(),
            NavigatorMode::Chats => self.chats.filter_matches(),
            _ => self.quick_matches,
        }
    }

    /// A press while the filter shows. `Some` when it was the filter's: the
    /// field (which arms typing), the `×`, or *Search everywhere*.
    pub(super) fn quick_press(&mut self, area: Rect, x: f32, y: f32) -> Option<Outcome> {
        self.quick.as_ref()?;
        if self.quick_clear_rect(area).contains(x, y) {
            self.clear_quick_filter();
            return Some(Outcome::Consumed);
        }
        if self.quick_field_rect(area).contains(x, y) {
            self.quick_armed = true;
            self.focus_tree(area);
            return Some(Outcome::Consumed);
        }
        if self.quick_match_count() == 0 && self.quick_search_rect(area).contains(x, y) {
            self.wants_search = self.quick.clone();
            return Some(Outcome::Consumed);
        }
        None
    }

    /// *Search everywhere for …* was pressed: the text to open search with.
    pub fn take_wants_search(&mut self) -> Option<String> {
        self.wants_search.take()
    }

    pub(super) fn draw_quick_filter(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
    ) {
        let Some(typed) = self.quick.clone() else {
            return;
        };
        let typed = typed.as_str();
        let p = theme::p();
        let field = self.quick_field_rect(area);
        let shape = scene::rrect(field.x, field.y, field.w, field.h, 7.0);
        scene::fill(scene, root, p.panel_bg, &shape);
        // The accent ring says the letters are coming here.
        let typing = self.quick_armed && self.has_focus();
        scene::stroke(
            scene,
            root,
            if typing { p.accent } else { p.panel_border },
            if typing { 1.5 } else { 1.0 },
            &shape,
        );
        let glass = engine.icon_layout(icons::SEARCH, 14.0);
        scene::draw_layout(
            scene,
            root,
            &glass,
            field.x + 9.0,
            field.y + (field.h - glass.height()) * 0.5,
            Some(p.text_dim),
        );
        let clear = self.quick_clear_rect(area);
        let cross = engine.icon_layout(icons::WINDOW_CLOSE, 12.0);
        scene::draw_layout(
            scene,
            root,
            &cross,
            clear.x + (clear.w - cross.width()) * 0.5,
            clear.y + (clear.h - cross.height()) * 0.5,
            Some(p.text_dim),
        );
        let count =
            engine.simple_layout(&self.quick_match_count().to_string(), theme::role::hint());
        let count_x = clear.x - count.width() - 4.0;
        scene::draw_layout_first_line(
            scene,
            root,
            &count,
            count_x,
            field.y + (field.h - scene::first_line_height(&count)) * 0.5,
            Some(p.text_dim),
        );
        // The text is the shared input: its caret, selection and scrolling.
        let text_x = field.x + 28.0;
        let text_rect = Rect::new(text_x, field.y, (count_x - 6.0 - text_x).max(0.0), field.h);
        if let Some(input) = self.quick_input.as_mut() {
            input.draw(
                scene,
                root,
                engine,
                text_rect,
                koru_ui::text_input::InputStyle::bare(theme::role::row()),
                None,
                typing,
            );
        }

        // Nothing matched: say so, and offer the search that looks inside.
        if self.quick_match_count() == 0 {
            let none = engine.simple_layout("No matches", theme::role::row());
            scene::draw_layout_first_line(
                scene,
                root,
                &none,
                area.x + theme::NAV_PAD + 4.0,
                self.rows_top(area) + (theme::NAV_ROW_H - scene::first_line_height(&none)) * 0.5,
                Some(p.text_dim),
            );
            let link = self.quick_search_rect(area);
            let words = engine.elided(
                &format!("Search everywhere for \u{201c}{typed}\u{201d}"),
                theme::role::row(),
                (link.w - 8.0).max(0.0),
            );
            let offer = engine.simple_layout(&words, theme::role::row());
            scene::draw_layout_first_line(
                scene,
                root,
                &offer,
                link.x + 4.0,
                link.y + (link.h - scene::first_line_height(&offer)) * 0.5,
                Some(p.accent),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: usize, end: usize) -> Span {
        Span { start, end }
    }

    #[test]
    fn a_name_matches_when_it_holds_every_word_whatever_the_case() {
        let typed = words("DES sys");
        assert_eq!(
            spans("Design system", &typed),
            Some(vec![span(0, 3), span(7, 10)])
        );
        assert_eq!(spans("Redesign", &words("design")), Some(vec![span(2, 8)]));
        assert_eq!(spans("Design", &typed), None, "every word");
        assert_eq!(spans("Design", &words("  ")), None, "nothing typed");
        // Bytes, not characters: the span lands on the right letters.
        assert_eq!(
            spans("Añadir diseño", &words("dise")),
            Some(vec![span(8, 12)])
        );
        // Two words over the same letters are one highlight.
        assert_eq!(spans("Design", &words("des esi")), Some(vec![span(0, 4)]));
    }

    #[test]
    fn a_match_keeps_the_rows_it_is_inside_and_dims_them() {
        // Notes > Design > Old ; Other ; Sidebar design
        let rows = [(0, false), (1, true), (2, false), (0, false), (0, true)];
        let (keep, dim) = keep_with_ancestors(&rows);
        assert_eq!(keep, [true, true, false, false, true]);
        assert_eq!(dim, [true, false, false, false, false]);
    }
}
