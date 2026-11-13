// SPDX-License-Identifier: MPL-2.0

//! Document-local find and replace.
//!
//! The controller belongs to one document tab. Matches are derived from the
//! editor's visible row cache; only source ranges and stable undo anchors cross
//! into the collaborative document.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::time::{Duration, Instant};

use koru_core::schema::WorkspaceDoc;
use koru_core::{NodeId, StableTextRange};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme};
use koru_editor::{Editing, Editor, SearchHighlight};
use koru_ui::text_field::{Typed, baseline_centred_y};
use koru_ui::toast::Outcome as ToastOutcome;
use koru_ui::{ActionButton, ActionStyle, InputStyle, TextInput, Toast, Tooltip, TooltipContent};
use unicode_segmentation::UnicodeSegmentation;
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

const PANEL_W: f32 = 430.0;
const PANEL_MARGIN: f32 = 16.0;
const ROW_H: f32 = 40.0;
const CONTROL_W: f32 = koru_ui::icon_button::Metrics::COMPACT.control_width();
const CONTROL_H: f32 = koru_ui::icon_button::Metrics::COMPACT.control_height();
const COUNT_W: f32 = 102.0;
const INPUT_MIN_W: f32 = 96.0;
const REPLACE_ROW_H: f32 = 48.0;
const OPTION_ROW_H: f32 = 30.0;
const WRAP_HOLD: Duration = Duration::from_millis(1_500);
const PULSE: Duration = Duration::from_millis(140);

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Subpanel {
    None,
    Replace,
    Options,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Field {
    Query,
    Replacement,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Hit {
    Query,
    Previous,
    Next,
    ReplaceToggle,
    OptionsToggle,
    Close,
    Replacement,
    ReplaceAll,
    ReplaceOne,
    MatchCase,
    WholeWords,
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct TableTarget {
    row: usize,
    column: usize,
    display: Range<usize>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct Match {
    node: NodeId,
    source: Range<usize>,
    table: Option<TableTarget>,
    caption: bool,
    /// **A pill's caption matched, and `source` is its anchor** — the one
    /// `U+2060` the pill is drawn over, never the words that matched.
    ///
    /// The caption is not in the node's text at all (it is resolved from the
    /// document a reference points at, or from the attribute's label), so there
    /// is no range in the text to replace. That is what this flag is for:
    /// [`replace_one`](Find::replace_one) and
    /// [`replace_all`](Find::replace_all) step past these, because writing the
    /// replacement over `source` would delete the anchor and take the pill with
    /// it. Renaming a document is not something the find bar may do.
    pill: bool,
    order: usize,
}

impl Match {
    fn same_target(&self, other: &Self) -> bool {
        self.node == other.node
            && self.source == other.source
            && self.caption == other.caption
            && self.pill == other.pill
            && self.table.as_ref().map(|t| (t.row, t.column, &t.display))
                == other.table.as_ref().map(|t| (t.row, t.column, &t.display))
    }
}

struct UndoEdit {
    range: StableTextRange,
    original: String,
}

struct UndoBatch {
    replacement: String,
    edits: Vec<UndoEdit>,
}

#[derive(Clone)]
struct Layout {
    panel: Rect,
    query: Rect,
    previous: Rect,
    next: Rect,
    replace_toggle: Rect,
    options_toggle: Rect,
    divider: Rect,
    close: Rect,
    replacement: Option<Rect>,
    replace_all: Option<Rect>,
    replace_one: Option<Rect>,
    match_case: Option<Rect>,
    whole_words: Option<Rect>,
}

pub struct LocalSearch {
    open: bool,
    subpanel: Subpanel,
    focus: Field,
    query: TextInput,
    replacement: TextInput,
    matches: Vec<Match>,
    active: Option<usize>,
    match_case: bool,
    whole_words: bool,
    origin: Option<(usize, usize)>,
    activated: Option<Instant>,
    wrapped_until: Option<Instant>,
    hover: Option<Hit>,
    tooltip: Tooltip,
    toast: Option<Toast>,
    undo: Option<UndoBatch>,
    standalone_title: Option<(NodeId, String)>,
    /// How many times [`refresh`](Self::refresh) has been asked to re-scan.
    ///
    /// Counted rather than inferred: a re-scan is idempotent, so running it
    /// twice for one document change leaves no trace in the matches — and
    /// "exactly as often as before" is what Q-B4's fix had to keep true while
    /// the two hosts' adopt paths were reconciled. Incremented before the
    /// `!self.open` guard, so it counts *asks* rather than scans.
    refreshes: u64,
}

impl LocalSearch {
    pub fn new(engine: &mut TextEngine) -> Self {
        Self {
            open: false,
            subpanel: Subpanel::None,
            focus: Field::Query,
            query: TextInput::new(engine, theme::role::row()),
            replacement: TextInput::new(engine, theme::role::row()),
            matches: Vec::new(),
            active: None,
            match_case: false,
            whole_words: false,
            origin: None,
            activated: None,
            wrapped_until: None,
            hover: None,
            tooltip: Tooltip::default(),
            toast: None,
            undo: None,
            standalone_title: None,
            refreshes: 0,
        }
    }

    /// How many times a re-scan has been asked for. See [`refreshes`].
    ///
    /// [`refreshes`]: Self::refreshes
    pub fn refreshes(&self) -> u64 {
        self.refreshes
    }

    pub fn set_standalone_title(&mut self, title: Option<(NodeId, String)>) {
        self.standalone_title = title;
    }

    fn refresh_standalone_title(&mut self, ws: &WorkspaceDoc) {
        let Some((node, _)) = self.standalone_title.as_ref() else {
            return;
        };
        if let Some(title) = ws
            .read(*node)
            .ok()
            .and_then(|node| node.content.as_text().map(str::to_owned))
        {
            self.standalone_title = Some((*node, title));
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Which of the two fields a typed character would go into, or `None` while
    /// the bar is shut.
    ///
    /// **Not the same question as [`is_open`](Self::is_open).** The bar is not
    /// a modal — it takes the keys it recognises and reports the rest as
    /// unhandled, so the document beneath still sees them — but a *character*
    /// always ends in whichever of query and replacement has focus, and that is
    /// the one thing a dictation needs to know. Answered as
    /// [`koru_workspace_host::routing::FieldId`] rather than this module's private
    /// `Field` so the enum stays private and the caller keeps one vocabulary.
    pub fn focused_field(&self) -> Option<koru_workspace_host::routing::FieldId> {
        use koru_workspace_host::routing::FieldId;
        self.open.then(|| match self.focus {
            Field::Query => FieldId::Find,
            Field::Replacement => FieldId::Replace,
        })
    }

    pub(crate) fn ime(&mut self, editor: &mut Editor, event: &winit::event::Ime) -> bool {
        if !self.open {
            return false;
        }
        let before = self.query.raw_text().to_owned();
        let handled = match self.focus {
            Field::Query => self.query.ime(&mut editor.view.engine, event),
            Field::Replacement => self.replacement.ime(&mut editor.view.engine, event),
        };
        if before != self.query.raw_text() {
            self.refresh(editor);
        }
        handled
    }

    pub(crate) fn ime_cancel(&mut self, editor: &mut Editor) -> bool {
        let query = self.query.ime_cancel(&mut editor.view.engine);
        self.replacement.ime_cancel(&mut editor.view.engine) || query
    }

    pub(crate) fn ime_cursor_area(&self, editor: &Editor) -> Option<Rect> {
        if !self.open {
            return None;
        }
        let layout = self.layout(editor);
        match self.focus {
            Field::Query => self.query.ime_cursor_area(layout.query),
            Field::Replacement => self.replacement.ime_cursor_area(layout.replacement?),
        }
    }

    /// Whichever of query and replacement `field` names, for a caller that is
    /// about to compose a dictation into it.
    ///
    /// **Deliberately not "the focused one"**: the focus can move while a
    /// dictation is open, and re-asking would let a partial land in a box the
    /// utterance did not start in. The caller names the field it recorded when
    /// the composition began.
    pub fn field_mut(
        &mut self,
        field: koru_workspace_host::routing::FieldId,
    ) -> Option<&mut koru_ui::TextInput> {
        use koru_workspace_host::routing::FieldId;
        match field {
            FieldId::Find => Some(&mut self.query),
            FieldId::Replace => Some(&mut self.replacement),
            _ => None,
        }
    }

    /// The panel's rect while it is open, in view coordinates.
    ///
    /// Exposed because the panel floats over the top-right of the content,
    /// which is where the title row parks its page-furniture button — see
    /// `Leaf::title_action_rect`, which asks this so the two do not draw over
    /// each other or fight for the same click.
    pub fn panel(&self, editor: &Editor) -> Option<Rect> {
        self.open.then(|| self.layout(editor).panel)
    }

    pub fn panel_contains(&self, editor: &Editor, x: f32, y: f32) -> bool {
        self.panel(editor).is_some_and(|panel| panel.contains(x, y))
    }

    pub fn open(&mut self, editor: &mut Editor, replace: bool) {
        let selected = editor.body.focused().and_then(|row| {
            let range = row.range();
            (!range.is_empty()).then(|| row.text[range].to_owned())
        });
        let seeded_from_selection = selected.is_some();
        if let Some(selected) = selected {
            self.query.set_text(&mut editor.view.engine, &selected);
        }
        if seeded_from_selection {
            self.query.select_all(&mut editor.view.engine);
        } else {
            self.query.move_to_end(&mut editor.view.engine);
        }
        self.open = true;
        self.subpanel = if replace {
            Subpanel::Replace
        } else {
            Subpanel::None
        };
        self.focus = Field::Query;
        self.origin = editor
            .body
            .focused()
            .map(|row| (editor.body.focus, row.caret()));
        editor.view.overlay = koru_editor::components::Overlay::None;
        editor.view.active = false;
        self.refresh(editor);
    }

    pub fn close(&mut self, ws: &WorkspaceDoc, editor: &mut Editor, select: bool) {
        if !self.open {
            return;
        }
        if select {
            self.select_active(ws, editor);
        }
        self.open = false;
        self.subpanel = Subpanel::None;
        self.focus = Field::Query;
        self.replacement.set_text(&mut editor.view.engine, "");
        self.matches.clear();
        self.active = None;
        self.hover = None;
        self.tooltip.clear();
        self.query.blur(&mut editor.view.engine);
        self.replacement.blur(&mut editor.view.engine);
        editor.view.search_highlights.clear();
        if !select {
            editor.view.active = true;
        }
    }

    pub fn key(
        &mut self,
        ws: &mut WorkspaceDoc,
        editor: &mut Editor,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        if self.open {
            koru_ui::keys::debug::write(format_args!(
                "[Koru][local-search] key focus={:?} key={} text={} effective_mods={} query_selected={} replacement_selected={} query_dragging={} replacement_dragging={}",
                self.focus,
                koru_ui::keys::debug::key_of(key),
                koru_ui::keys::debug::text_of(text),
                koru_ui::keys::debug::mods_of(mods),
                self.query.has_selection(),
                self.replacement.has_selection(),
                self.query.is_dragging(),
                self.replacement.is_dragging(),
            ));
        }
        // Windows can retain U+0008 in `text` even after the shell has
        // recovered the logical key to Backspace. Shortcut decoding would
        // otherwise read that byte as Ctrl+H and toggle Replace instead of
        // deleting a word. A reusable document view may also receive U+0008
        // as the logical key itself, so recognise both shapes here.
        let backspace = matches!(key, Key::Named(NamedKey::Backspace))
            || matches!(key, Key::Character(value) if value.contains('\u{8}'))
            || text.is_some_and(|value| value.contains('\u{8}'));
        let letter = (!backspace)
            .then(|| koru_ui::ctrl_letter(key, text))
            .flatten();
        // Through the primary modifier, not a literal Ctrl: this used to test
        // `control_key()`, which made find Ctrl+F on a Mac — the one platform
        // where that is not the find chord. `has_foreign_primary` is refused
        // so ⌃F on macOS stays free for what the system means by it.
        let plain_primary = koru_keys::has_primary(mods) && !koru_keys::has_foreign_primary(mods);
        let open_find =
            plain_primary && !mods.shift_key() && !mods.alt_key() && letter == Some('f');
        let open_replace = plain_primary
            && !mods.shift_key()
            && ((mods.alt_key() && letter == Some('f'))
                || (!mods.alt_key() && letter == Some('h')));
        if !self.open {
            if open_find || open_replace {
                self.open(editor, open_replace);
                return true;
            }
            return false;
        }

        if open_find {
            self.focus(editor, Field::Query);
            self.query.select_all(&mut editor.view.engine);
            return true;
        }
        if open_replace {
            if self.subpanel == Subpanel::Replace {
                self.hide_replace(editor);
            } else {
                self.subpanel = Subpanel::Replace;
                self.focus(editor, Field::Replacement);
                self.replacement.move_to_end(&mut editor.view.engine);
            }
            return true;
        }
        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.close(ws, editor, true);
            return true;
        }
        if mods.control_key() && matches!(key, Key::Named(NamedKey::Enter)) {
            if self.subpanel == Subpanel::Replace {
                self.replace_all(ws, editor);
            }
            return true;
        }
        if self.subpanel == Subpanel::Replace
            && self.focus == Field::Replacement
            && backspace
            && self.replacement.is_empty()
        {
            self.hide_replace(editor);
            return true;
        }
        if self.subpanel == Subpanel::Replace
            && self.focus == Field::Replacement
            && matches!(key, Key::Named(NamedKey::Tab))
            && mods.shift_key()
            && !mods.control_key()
            && !mods.alt_key()
        {
            if self.replacement.is_empty() {
                self.hide_replace(editor);
            } else {
                self.focus(editor, Field::Query);
                self.query.move_to_end(&mut editor.view.engine);
            }
            return true;
        }
        if self.subpanel == Subpanel::Replace
            && self.focus == Field::Query
            && matches!(key, Key::Named(NamedKey::Tab))
            && !mods.control_key()
            && !mods.shift_key()
            && !mods.alt_key()
        {
            self.focus(editor, Field::Replacement);
            if self.replacement.is_empty() {
                self.replacement.move_to_end(&mut editor.view.engine);
            } else {
                self.replacement.select_all(&mut editor.view.engine);
            }
            return true;
        }
        match key {
            Key::Named(NamedKey::ArrowUp) => {
                self.step(editor, -1);
                return true;
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.step(editor, 1);
                return true;
            }
            Key::Named(NamedKey::Enter) if self.focus == Field::Replacement => {
                self.replace_one(ws, editor, if mods.shift_key() { -1 } else { 1 });
                return true;
            }
            Key::Named(NamedKey::Enter) => {
                self.step(editor, if mods.shift_key() { -1 } else { 1 });
                return true;
            }
            _ => {}
        }

        let before = self.query.raw_text().to_owned();
        let handled = match self.focus {
            Field::Query => self.query.key(&mut editor.view.engine, key, text, mods),
            Field::Replacement => self
                .replacement
                .key(&mut editor.view.engine, key, text, mods),
        };
        if self.focus == Field::Query && before != self.query.raw_text() {
            self.refresh(editor);
        }
        handled == Typed::Handled
    }

    pub fn mouse_down(
        &mut self,
        ws: &mut WorkspaceDoc,
        editor: &mut Editor,
        x: f32,
        y: f32,
    ) -> bool {
        if self.toast_mouse_down(ws, editor, x, y) {
            return true;
        }
        if !self.open {
            return false;
        }
        let layout = self.layout(editor);
        if !layout.panel.contains(x, y) {
            return false;
        }
        let Some(hit) = hit_at(&layout, x, y) else {
            return true;
        };
        self.tooltip.clear();
        match hit {
            Hit::Query => {
                self.focus(editor, Field::Query);
                self.query
                    .click(&mut editor.view.engine, layout.query, x, y);
            }
            Hit::Previous => self.step(editor, -1),
            Hit::Next => self.step(editor, 1),
            Hit::ReplaceToggle => {
                if self.subpanel == Subpanel::Replace {
                    self.hide_replace(editor);
                } else {
                    self.subpanel = Subpanel::Replace;
                    self.focus(editor, Field::Replacement);
                    self.replacement.move_to_end(&mut editor.view.engine);
                }
            }
            Hit::OptionsToggle => {
                self.subpanel = if self.subpanel == Subpanel::Options {
                    Subpanel::None
                } else {
                    Subpanel::Options
                };
                self.focus(editor, Field::Query);
            }
            Hit::Close => self.close(ws, editor, true),
            Hit::Replacement => {
                self.focus(editor, Field::Replacement);
                if let Some(rect) = layout.replacement {
                    self.replacement.click(&mut editor.view.engine, rect, x, y);
                }
            }
            Hit::ReplaceAll => self.replace_all(ws, editor),
            Hit::ReplaceOne => self.replace_one(ws, editor, 1),
            Hit::MatchCase => {
                self.match_case = !self.match_case;
                self.refresh(editor);
            }
            Hit::WholeWords => {
                self.whole_words = !self.whole_words;
                self.refresh(editor);
            }
        }
        koru_ui::keys::debug::write(format_args!(
            "[Koru][local-search] mouse_down hit={hit:?} focus={:?} view_mods={} query_selected={} replacement_selected={} query_dragging={} replacement_dragging={}",
            self.focus,
            koru_ui::keys::debug::mods_of(editor.view.mods),
            self.query.has_selection(),
            self.replacement.has_selection(),
            self.query.is_dragging(),
            self.replacement.is_dragging(),
        ));
        true
    }

    pub fn mouse_move(&mut self, editor: &mut Editor, x: f32, y: f32) -> bool {
        if self.toast_mouse_move(editor, x, y) {
            return true;
        }
        if !self.open {
            return false;
        }
        let layout = self.layout(editor);
        if self.query.drag(&mut editor.view.engine, layout.query, x) {
            koru_ui::keys::debug::write(format_args!(
                "[Koru][local-search] mouse_move -> extend query selection view_mods={} selected={} dragging={}",
                koru_ui::keys::debug::mods_of(editor.view.mods),
                self.query.has_selection(),
                self.query.is_dragging(),
            ));
            return true;
        }
        if let Some(rect) = layout.replacement
            && self.replacement.drag(&mut editor.view.engine, rect, x)
        {
            koru_ui::keys::debug::write(format_args!(
                "[Koru][local-search] mouse_move -> extend replacement selection view_mods={} selected={} dragging={}",
                koru_ui::keys::debug::mods_of(editor.view.mods),
                self.replacement.has_selection(),
                self.replacement.is_dragging(),
            ));
            return true;
        }
        let hover = hit_at(&layout, x, y);
        let changed = hover != self.hover;
        self.hover = hover;
        let content = hover.and_then(|hit| tooltip_for(hit, &layout));
        self.tooltip.set_hover(content, Instant::now());
        changed || layout.panel.contains(x, y)
    }

    pub fn mouse_up(&mut self) {
        if self.open || self.query.is_dragging() || self.replacement.is_dragging() {
            koru_ui::keys::debug::write(format_args!(
                "[Koru][local-search] mouse_up query_dragging={} replacement_dragging={} query_selected={} replacement_selected={}",
                self.query.is_dragging(),
                self.replacement.is_dragging(),
                self.query.has_selection(),
                self.replacement.has_selection(),
            ));
        }
        self.query.release();
        self.replacement.release();
    }

    fn hide_replace(&mut self, editor: &mut Editor) {
        self.subpanel = Subpanel::None;
        self.focus(editor, Field::Query);
        self.query.move_to_end(&mut editor.view.engine);
    }

    fn focus(&mut self, editor: &mut Editor, field: Field) {
        if self.focus == field {
            return;
        }
        match self.focus {
            Field::Query => self.query.blur(&mut editor.view.engine),
            Field::Replacement => self.replacement.blur(&mut editor.view.engine),
        }
        self.focus = field;
    }

    fn match_options_active(&self) -> bool {
        self.subpanel == Subpanel::Options || self.match_case || self.whole_words
    }

    pub fn double_click(&mut self, editor: &mut Editor, x: f32, y: f32) -> bool {
        if !self.open {
            return false;
        }
        let layout = self.layout(editor);
        if self
            .query
            .double_click(&mut editor.view.engine, layout.query, x, y)
        {
            self.focus(editor, Field::Query);
            return true;
        }
        if let Some(rect) = layout.replacement
            && self
                .replacement
                .double_click(&mut editor.view.engine, rect, x, y)
        {
            self.focus(editor, Field::Replacement);
            return true;
        }
        layout.panel.contains(x, y)
    }

    pub fn cursor_icon(&self, editor: &Editor) -> Option<CursorIcon> {
        let (x, y) = editor.view.pointer;
        // The toast owns the pointer while it is **over the chip**, so the
        // I-beam of the row underneath does not show through it. This used to
        // ask whether the toast's cursor was the hand, which worked only while
        // a press target had a shape of its own; it does not now, and asking
        // the same question would have made the toast claim the pointer for
        // the whole window. `over_chip` is the question that was always meant
        // — it is what `MainView::cursor_icon` asks for the same toast.
        if let Some(toast) = &self.toast
            && toast.over_chip()
        {
            return Some(toast.cursor());
        }
        if !self.open {
            return None;
        }
        let layout = self.layout(editor);
        if layout.query.contains(x, y) || layout.replacement.is_some_and(|rect| rect.contains(x, y))
        {
            Some(CursorIcon::Text)
        } else if hit_at(&layout, x, y).is_some() {
            Some(koru_ui::cursor::PRESS)
        } else if layout.panel.contains(x, y) {
            Some(CursorIcon::Default)
        } else {
            Some(CursorIcon::Default)
        }
    }

    pub fn draw(&mut self, canvas: &mut Scene, root: Affine, editor: &mut Editor) {
        let now = Instant::now();
        if self.open {
            let layout = self.layout(editor);
            scene::floating_panel(
                canvas,
                root,
                layout.panel.x,
                layout.panel.y,
                layout.panel.w,
                layout.panel.h,
                &scene::Panel::popover(10.0),
            );
            self.query.draw(
                canvas,
                root,
                &mut editor.view.engine,
                layout.query,
                InputStyle::bare(theme::role::row()),
                Some(query_placeholder(self.subpanel)),
                self.focus == Field::Query,
            );
            draw_count(canvas, root, &mut editor.view.engine, &layout, self, now);
            draw_icon_controls(canvas, root, &mut editor.view.engine, &layout, self);
            if self.subpanel != Subpanel::None {
                scene::stroke(
                    canvas,
                    root,
                    theme::p().panel_border,
                    theme::BORDER_W,
                    &vello::kurbo::Line::new(
                        (layout.panel.x as f64, (layout.panel.y + ROW_H) as f64),
                        (
                            (layout.panel.x + layout.panel.w) as f64,
                            (layout.panel.y + ROW_H) as f64,
                        ),
                    ),
                );
            }
            match self.subpanel {
                Subpanel::Replace => self.draw_replace(canvas, root, editor, &layout),
                Subpanel::Options => self.draw_options(canvas, root, editor, &layout),
                Subpanel::None => {}
            }
            self.tooltip.draw(
                canvas,
                root,
                &mut editor.view.engine,
                Rect::new(0.0, 0.0, editor.view.viewport.0, editor.view.viewport.1),
                now,
            );
        }
        if let Some(toast) = &self.toast
            && let Some(alpha) = toast.alpha(now)
        {
            let (over, floor) = toast_geometry(editor);
            toast.draw(
                canvas,
                root,
                &mut editor.view.engine,
                over,
                floor,
                alpha,
                now,
            );
        }
    }

    pub fn refresh(&mut self, editor: &mut Editor) {
        self.refreshes = self.refreshes.saturating_add(1);
        if !self.open {
            return;
        }
        let matches = self.scan(editor);
        let origin = self.origin.unwrap_or((editor.body.focus, 0));
        self.matches = matches;
        self.active = self
            .matches
            .iter()
            .position(|found| {
                found.order > origin.0
                    || (found.order == origin.0 && found.source.start >= origin.1)
            })
            .or_else(|| (!self.matches.is_empty()).then_some(0));
        self.activated = self.active.map(|_| Instant::now());
        self.sync_highlights(editor);
        self.reveal_active(editor);
    }

    pub fn animating(&self, now: Instant) -> bool {
        self.activated
            .is_some_and(|started| now.saturating_duration_since(started) < PULSE)
            || self.tooltip.animating(now)
            || self
                .toast
                .as_ref()
                .is_some_and(|toast| toast.animating(now))
    }

    pub fn maintenance_deadline(&self) -> Option<Instant> {
        [self.tooltip.maintenance_deadline(), self.wrapped_until]
            .into_iter()
            .flatten()
            .min()
    }

    pub fn maintain(&mut self, now: Instant) -> bool {
        let mut changed = self.tooltip.maintain(now);
        if self.wrapped_until.is_some_and(|until| now >= until) {
            self.wrapped_until = None;
            changed = true;
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| toast.alpha(now).is_none())
        {
            self.toast = None;
            self.undo = None;
            changed = true;
        }
        changed
    }

    fn layout(&self, editor: &Editor) -> Layout {
        let view = &editor.view;
        let right = match view.view_mode {
            theme::ViewMode::Full => view.viewport.0,
            theme::ViewMode::Narrow | theme::ViewMode::Page => {
                view.content_x + view.view_mode.effective_w(view.viewport.0)
            }
        };
        let width = PANEL_W
            .min((right - PANEL_MARGIN * 2.0).max(0.0))
            .min((view.viewport.0 - PANEL_MARGIN * 2.0).max(0.0));
        let height = match self.subpanel {
            Subpanel::None => ROW_H,
            Subpanel::Replace => ROW_H + REPLACE_ROW_H,
            Subpanel::Options => ROW_H + OPTION_ROW_H * 2.0 + 8.0,
        };
        let x = (right - PANEL_MARGIN - width).max(PANEL_MARGIN);
        let y = view.chrome_h + 12.0;
        let panel = Rect::new(x, y, width, height);
        let control_y = y + (ROW_H - CONTROL_H) * 0.5;
        let close = Rect::new(x + width - 8.0 - CONTROL_W, control_y, CONTROL_W, CONTROL_H);
        let divider = Rect::new(
            close.x - koru_ui::divider::VERTICAL_LANE_W,
            y,
            koru_ui::divider::VERTICAL_LANE_W,
            ROW_H,
        );
        let options_toggle = Rect::new(divider.x - CONTROL_W, control_y, CONTROL_W, CONTROL_H);
        let replace_toggle = Rect::new(
            options_toggle.x - CONTROL_W,
            control_y,
            CONTROL_W,
            CONTROL_H,
        );
        let next = Rect::new(
            replace_toggle.x - CONTROL_W,
            control_y,
            CONTROL_W,
            CONTROL_H,
        );
        let previous = Rect::new(next.x - CONTROL_W, control_y, CONTROL_W, CONTROL_H);
        let count_x = previous.x - COUNT_W;
        let query_w = (count_x - (x + 12.0) - 8.0).max(INPUT_MIN_W);
        let query = Rect::new(x + 12.0, y + 3.0, query_w, 34.0);
        let (replacement, replace_all, replace_one) = if self.subpanel == Subpanel::Replace {
            let row_y = y + ROW_H + (REPLACE_ROW_H - ActionButton::H) * 0.5;
            // Both labels are close to 70 px including an optional icon; the
            // shared action contributes the requested 6 px on either side.
            let one_w = 82.0;
            let all_w = 82.0;
            let one = Rect::new(x + width - 12.0 - one_w, row_y, one_w, ActionButton::H);
            let all = Rect::new(one.x - 8.0 - all_w, row_y, all_w, ActionButton::H);
            let input = Rect::new(
                x + 12.0,
                row_y,
                (all.x - x - 20.0).max(80.0),
                ActionButton::H,
            );
            (Some(input), Some(all), Some(one))
        } else {
            (None, None, None)
        };
        let (match_case, whole_words) = if self.subpanel == Subpanel::Options {
            (
                Some(Rect::new(
                    x + 10.0,
                    y + ROW_H + 4.0,
                    width - 20.0,
                    OPTION_ROW_H,
                )),
                Some(Rect::new(
                    x + 10.0,
                    y + ROW_H + 4.0 + OPTION_ROW_H,
                    width - 20.0,
                    OPTION_ROW_H,
                )),
            )
        } else {
            (None, None)
        };
        Layout {
            panel,
            query,
            previous,
            next,
            replace_toggle,
            options_toggle,
            divider,
            close,
            replacement,
            replace_all,
            replace_one,
            match_case,
            whole_words,
        }
    }

    fn draw_replace(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        editor: &mut Editor,
        layout: &Layout,
    ) {
        let Some(input) = layout.replacement else {
            return;
        };
        self.replacement.draw(
            canvas,
            root,
            &mut editor.view.engine,
            input,
            InputStyle::bare(theme::role::row()),
            Some("Replace with..."),
            self.focus == Field::Replacement,
        );
        if let Some(rect) = layout.replace_all {
            ActionButton::new("Replace all", ActionStyle::Ghost, None).draw(
                canvas,
                root,
                &mut editor.view.engine,
                rect,
                self.hover == Some(Hit::ReplaceAll),
            );
        }
        if let Some(rect) = layout.replace_one {
            ActionButton::new("Replace", ActionStyle::Primary, Some(icons::ENTER)).draw(
                canvas,
                root,
                &mut editor.view.engine,
                rect,
                self.hover == Some(Hit::ReplaceOne),
            );
        }
    }

    fn draw_options(&self, canvas: &mut Scene, root: Affine, editor: &mut Editor, layout: &Layout) {
        for (rect, label, checked, hit) in [
            (
                layout.match_case,
                "Match case",
                self.match_case,
                Hit::MatchCase,
            ),
            (
                layout.whole_words,
                "Whole words only",
                self.whole_words,
                Hit::WholeWords,
            ),
        ] {
            let Some(rect) = rect else { continue };
            if self.hover == Some(hit) {
                scene::fill(
                    canvas,
                    root,
                    theme::p().hover_tint,
                    &scene::rrect(rect.x, rect.y, rect.w, rect.h, 5.0),
                );
            }
            let glyph = if checked {
                icons::CHECK_SQUARE
            } else {
                icons::SQUARE
            };
            let icon = editor.view.engine.icon_layout(glyph, 16.0);
            scene::draw_layout(
                canvas,
                root,
                &icon,
                rect.x + 4.0 + (16.0 - icon.width()) * 0.5,
                rect.y + (rect.h - icon.height()) * 0.5,
                Some(if checked {
                    theme::p().accent
                } else {
                    theme::p().text_dim
                }),
            );
            let text = editor.view.engine.simple_layout(label, theme::role::row());
            scene::draw_layout_first_line(
                canvas,
                root,
                &text,
                rect.x + 28.0,
                baseline_centred_y(&text, rect.y, rect.h),
                Some(theme::p().text),
            );
        }
    }

    fn scan(&self, editor: &Editor) -> Vec<Match> {
        let query = self.query.raw_text();
        if query.is_empty() {
            return Vec::new();
        }
        let mut found = Vec::new();
        let mut order_offset = 0usize;
        if !editor.body.title_row
            && let Some((node, title)) = &self.standalone_title
        {
            found.extend(
                find_ranges(title, query, self.match_case, self.whole_words)
                    .into_iter()
                    .map(|range| Match {
                        node: *node,
                        source: range,
                        table: None,
                        caption: true,
                        pill: false,
                        order: 0,
                    }),
            );
            order_offset = 1;
        }
        for (row_order, row) in editor.body.rows.iter().enumerate() {
            let order = row_order + order_offset;
            if row.image.is_some() || row.kind == koru_core::Kind::Divider {
                continue;
            }
            if row.kind == koru_core::Kind::Table
                && editor.view.table_source != Some(row.id)
                && let Some(table) = row.table.as_ref()
                && let Some(parsed) = table.grid.as_ref().map(|grid| &grid.parsed)
            {
                for (table_row, cells) in std::iter::once(&parsed.header)
                    .chain(parsed.rows.iter())
                    .enumerate()
                {
                    for (column, cell) in cells.iter().enumerate() {
                        for display in
                            find_ranges(&cell.text, query, self.match_case, self.whole_words)
                        {
                            if let Some(source) = cell.source_range_for_display(display.clone()) {
                                found.push(Match {
                                    node: row.id,
                                    source,
                                    table: Some(TableTarget {
                                        row: table_row,
                                        column,
                                        display,
                                    }),
                                    caption: false,
                                    pill: false,
                                    order,
                                });
                            }
                        }
                    }
                }
            } else {
                let mut in_row: Vec<Match> =
                    find_ranges(&row.text, query, self.match_case, self.whole_words)
                        .into_iter()
                        .map(|range| Match {
                            node: row.id,
                            source: range,
                            table: None,
                            caption: false,
                            pill: false,
                            order,
                        })
                        .collect();
                in_row.extend(self.pill_matches(row, query, order));
                // Merged rather than appended: a row's matches are stepped
                // through in the order they are read, and a pill sitting
                // mid-sentence must be reached between the words either side of
                // it rather than after all of them.
                in_row.sort_by_key(|found| found.source.start);
                found.extend(in_row);
            }
        }
        found
    }

    /// **The pill captions in one row that match.**
    ///
    /// A pill — a link to another document, a URL, an attachment — occupies
    /// exactly one `koru_core::text::ANCHOR` (`U+2060 WORD JOINER`) in the
    /// node's text. The words you can *see* on it are resolved elsewhere and
    /// cached on the row, so searching `row.text` alone could never find a
    /// document by the name printed on the reference to it. This is the half
    /// that was missing.
    ///
    /// **One match per pill, not one per occurrence in its caption.** A pill is
    /// a single cluster by construction (one arrow key steps over it, one
    /// backspace deletes it), so there is no such thing as landing on half of
    /// one — and `source` is that whole cluster's range, which is what makes
    /// Enter select the pill rather than a byte inside a word that is not in
    /// the document.
    fn pill_matches<'a>(
        &'a self,
        row: &'a koru_editor::Paragraph,
        query: &'a str,
        order: usize,
    ) -> impl Iterator<Item = Match> + 'a {
        row.runs.iter().filter_map(move |run| {
            let info = row.pills.get(&run.pill?)?;
            let hit =
                !find_ranges(&info.label, query, self.match_case, self.whole_words).is_empty();
            hit.then(|| Match {
                node: row.id,
                source: run.range(),
                table: None,
                caption: false,
                pill: true,
                order,
            })
        })
    }

    fn step(&mut self, editor: &mut Editor, delta: i32) {
        if self.matches.is_empty() {
            return;
        }
        let current = self.active.unwrap_or(0) as i32;
        let raw = current + delta;
        let len = self.matches.len() as i32;
        let wrapped = raw < 0 || raw >= len;
        self.active = Some(raw.rem_euclid(len) as usize);
        self.activated = Some(Instant::now());
        self.wrapped_until = wrapped.then(|| Instant::now() + WRAP_HOLD);
        self.sync_highlights(editor);
        self.reveal_active(editor);
    }

    fn sync_highlights(&self, editor: &mut Editor) {
        editor.view.search_highlights = self
            .matches
            .iter()
            .enumerate()
            .map(|(index, found)| SearchHighlight {
                node: found.node,
                range: found
                    .table
                    .as_ref()
                    .map_or_else(|| found.source.clone(), |table| table.display.clone()),
                table_cell: found.table.as_ref().map(|table| (table.row, table.column)),
                active: self.active == Some(index),
                activated: (self.active == Some(index))
                    .then_some(self.activated)
                    .flatten(),
            })
            .collect();
    }

    fn reveal_active(&self, editor: &mut Editor) {
        let Some(found) = self.active.and_then(|index| self.matches.get(index)) else {
            return;
        };
        if found.caption {
            // The caption is fixed viewport chrome; it is already visible.
        } else if let Some(table) = &found.table {
            editor.reveal_table_range(found.node, table.row, table.column);
        } else {
            editor.reveal_text_range(found.node, found.source.clone());
        }
        editor.view.active = false;
    }

    fn select_active(&self, ws: &WorkspaceDoc, editor: &mut Editor) {
        let Some(found) = self.active.and_then(|index| self.matches.get(index)) else {
            editor.view.active = true;
            return;
        };
        if found.caption {
            editor.start_rename_selection(ws, found.source.clone());
        } else {
            editor.select_text_range(
                found.node,
                found.source.clone(),
                found.table.as_ref().map(|table| (table.row, table.column)),
            );
        }
    }

    fn replace_one(&mut self, ws: &mut WorkspaceDoc, editor: &mut Editor, direction: i32) {
        let Some(active) = self
            .active
            .and_then(|index| self.matches.get(index))
            .cloned()
        else {
            return;
        };
        // **A pill is found, never replaced.** Its caption is not in the text —
        // `source` is the anchor the pill is drawn over — so writing the
        // replacement there would delete the anchor and take the reference with
        // it. Enter walks on to the next match instead of doing nothing, which
        // is what makes Replace usable in a document full of links; `direction`
        // is the same ±1 the replacing path uses, so Shift+Enter still walks
        // backwards past one.
        if active.pill {
            self.step(editor, direction);
            return;
        }
        let current = self.scan(editor);
        let Some(valid) = current.into_iter().find(|found| found.same_target(&active)) else {
            self.refresh(editor);
            return;
        };
        let replacement = self.replacement.raw_text().to_owned();
        // Every Enter is one undo step, even when it follows another Replace
        // immediately. Its metadata brings ordinary Ctrl+Z back to the word's
        // original start after the search panel has gone away.
        ws.checkpoint();
        if ws
            .set_next_undo_caret(valid.node, valid.source.start)
            .is_err()
        {
            return;
        }
        if ws
            .replace_text(valid.node, valid.source.clone(), &replacement)
            .is_err()
        {
            ws.clear_next_undo_caret();
            return;
        }
        ws.checkpoint();
        editor.body.refresh(ws, valid.node);
        editor.reflow();
        self.refresh_standalone_title(ws);
        let after = valid.source.start + replacement.len();
        self.origin = Some((
            valid.order,
            if direction < 0 {
                valid.source.start
            } else {
                after
            },
        ));
        self.matches = self.scan(editor);
        let next = if direction < 0 {
            self.matches.iter().rposition(|found| {
                found.order < valid.order
                    || (found.order == valid.order && found.source.start < valid.source.start)
            })
        } else {
            self.matches.iter().position(|found| {
                found.order > valid.order
                    || (found.order == valid.order && found.source.start >= after)
            })
        };
        let wrapped = next.is_none() && !self.matches.is_empty();
        self.active = next.or_else(|| {
            (!self.matches.is_empty()).then_some(if direction < 0 {
                self.matches.len() - 1
            } else {
                0
            })
        });
        self.activated = self.active.map(|_| Instant::now());
        self.wrapped_until = wrapped.then(|| Instant::now() + WRAP_HOLD);
        self.sync_highlights(editor);
        self.reveal_active(editor);
    }

    fn replace_all(&mut self, ws: &mut WorkspaceDoc, editor: &mut Editor) {
        // Pills are dropped here rather than filtered inside the loop, so the
        // "nothing to do" check below is about what will actually be replaced:
        // a document of nothing but links must not report replacing anything.
        // See [`Match::pill`].
        let matches: Vec<Match> = self
            .scan(editor)
            .into_iter()
            .filter(|found| !found.pill)
            .collect();
        if matches.is_empty() {
            return;
        }
        let replacement = self.replacement.raw_text().to_owned();
        let active_target = self
            .active
            .and_then(|index| self.matches.get(index))
            .cloned();
        let mut seen = HashSet::new();
        let mut edits = Vec::new();
        for found in matches {
            let key = (found.node, found.source.start, found.source.end);
            if !seen.insert(key) {
                continue;
            }
            let Some(original) = ws
                .read(found.node)
                .ok()
                .and_then(|node| node.content.as_text().map(str::to_owned))
                .and_then(|text| text.get(found.source.clone()).map(str::to_owned))
            else {
                self.refresh(editor);
                return;
            };
            edits.push((found, original));
        }
        edits.sort_by(|(a, _), (b, _)| (b.order, b.source.start).cmp(&(a.order, a.source.start)));
        ws.checkpoint();
        ws.begin_group();
        for (found, _) in &edits {
            if ws
                .replace_text(found.node, found.source.clone(), &replacement)
                .is_err()
            {
                ws.checkpoint();
                return;
            }
        }
        ws.checkpoint();

        let mut by_node: HashMap<NodeId, Vec<(Match, String)>> = HashMap::new();
        for (found, original) in edits.iter().cloned() {
            by_node
                .entry(found.node)
                .or_default()
                .push((found, original));
        }
        let mut undo_edits = Vec::new();
        let mut selection = None;
        for (node, list) in &mut by_node {
            list.sort_by_key(|(found, _)| found.source.start);
            let mut delta = 0isize;
            for (found, original) in list.iter() {
                let final_start = found.source.start.saturating_add_signed(delta);
                let final_end = final_start + replacement.len();
                if active_target
                    .as_ref()
                    .is_some_and(|active| active.same_target(found))
                {
                    selection = Some((
                        *node,
                        final_start..final_end,
                        found.table.as_ref().map(|table| (table.row, table.column)),
                        found.caption,
                    ));
                }
                if let Ok(range) = ws.capture_text_range(*node, final_start, final_end) {
                    undo_edits.push(UndoEdit {
                        range,
                        original: original.clone(),
                    });
                }
                delta += replacement.len() as isize - found.source.len() as isize;
            }
            editor.body.refresh(ws, *node);
        }
        editor.reflow();
        self.refresh_standalone_title(ws);

        let count = edits.len();
        self.open = false;
        self.subpanel = Subpanel::None;
        self.matches.clear();
        self.active = None;
        self.tooltip.clear();
        editor.view.search_highlights.clear();
        self.replacement.set_text(&mut editor.view.engine, "");
        if let Some((node, range, table, caption)) = selection {
            if caption {
                editor.start_rename_selection(ws, range);
            } else {
                editor.select_text_range(node, range, table);
            }
        } else {
            editor.view.active = true;
        }
        self.toast = Some(Toast::new(
            format!("Replaced {count} matches"),
            (!undo_edits.is_empty()).then_some("Undo"),
            Instant::now(),
        ));
        self.undo = (!undo_edits.is_empty()).then_some(UndoBatch {
            replacement,
            edits: undo_edits,
        });
    }

    fn toast_mouse_down(
        &mut self,
        ws: &mut WorkspaceDoc,
        editor: &mut Editor,
        x: f32,
        y: f32,
    ) -> bool {
        let Some(toast) = &mut self.toast else {
            return false;
        };
        let (over, floor) = toast_geometry(editor);
        if !toast.contains(&mut editor.view.engine, over, floor, x, y) {
            return false;
        }
        match toast.mouse_down(&mut editor.view.engine, over, floor, x, y) {
            ToastOutcome::Action => self.undo_replace_all(ws, editor),
            // This toast offers one action; a second is nothing here.
            ToastOutcome::Secondary => {}
            ToastOutcome::Dismiss => {
                self.toast = None;
                self.undo = None;
            }
            ToastOutcome::Miss => {}
        }
        true
    }

    fn toast_mouse_move(&mut self, editor: &mut Editor, x: f32, y: f32) -> bool {
        let Some(toast) = &mut self.toast else {
            return false;
        };
        let (over, floor) = toast_geometry(editor);
        let contains = toast.contains(&mut editor.view.engine, over, floor, x, y);
        toast.mouse_move(&mut editor.view.engine, over, floor, x, y);
        contains
    }

    fn undo_replace_all(&mut self, ws: &mut WorkspaceDoc, editor: &mut Editor) {
        let Some(batch) = self.undo.take() else {
            return;
        };
        let mut resolved = Vec::with_capacity(batch.edits.len());
        for edit in batch.edits {
            let Ok((start, end)) = ws.resolve_text_range(&edit.range) else {
                self.undo_failed();
                return;
            };
            let node = edit.range.anchor.node();
            let valid = ws
                .read(node)
                .ok()
                .and_then(|node| node.content.as_text().map(str::to_owned))
                .and_then(|text| text.get(start.min(end)..start.max(end)).map(str::to_owned))
                .is_some_and(|current| current == batch.replacement);
            if !valid {
                self.undo_failed();
                return;
            }
            resolved.push((node, start.min(end)..start.max(end), edit.original));
        }
        resolved.sort_by(|a, b| (b.0, b.1.start).cmp(&(a.0, a.1.start)));
        ws.checkpoint();
        ws.begin_group();
        for (node, range, original) in &resolved {
            if ws.replace_text(*node, range.clone(), original).is_err() {
                ws.checkpoint();
                self.undo_failed();
                return;
            }
        }
        ws.checkpoint();
        let nodes: HashSet<_> = resolved.iter().map(|(node, _, _)| *node).collect();
        for node in nodes {
            editor.body.refresh(ws, node);
        }
        editor.reflow();
        self.refresh_standalone_title(ws);
        self.toast = None;
    }

    fn undo_failed(&mut self) {
        self.toast = Some(Toast::new(
            "Couldn't undo replacements after other edits".to_owned(),
            None,
            Instant::now(),
        ));
    }
}

fn find_ranges(text: &str, query: &str, match_case: bool, whole_words: bool) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let boundaries = whole_words.then(|| {
        let mut points = HashSet::from([0, text.len()]);
        for (at, part) in text.split_word_bound_indices() {
            points.insert(at);
            points.insert(at + part.len());
        }
        points
    });
    let mut ranges: Vec<Range<usize>> = if match_case {
        text.match_indices(query)
            .map(|(start, value)| start..start + value.len())
            .collect()
    } else {
        let (haystack, units) = fold_with_source(text);
        let needle: String = query.chars().flat_map(char::to_lowercase).collect();
        haystack
            .match_indices(&needle)
            .filter_map(|(start, value)| {
                let end = start + value.len();
                let first = units.iter().find(|unit| unit.0.start == start)?;
                let last = units.iter().find(|unit| unit.0.end == end)?;
                Some(first.1.start..last.1.end)
            })
            .collect()
    };
    if let Some(boundaries) = boundaries {
        ranges.retain(|range| boundaries.contains(&range.start) && boundaries.contains(&range.end));
    }
    ranges
}

fn query_placeholder(subpanel: Subpanel) -> &'static str {
    if subpanel == Subpanel::Replace {
        "Find or replace..."
    } else {
        "Find"
    }
}

fn fold_with_source(text: &str) -> (String, Vec<(Range<usize>, Range<usize>)>) {
    let mut folded = String::new();
    let mut units = Vec::new();
    for (start, ch) in text.char_indices() {
        let folded_start = folded.len();
        folded.extend(ch.to_lowercase());
        units.push((folded_start..folded.len(), start..start + ch.len_utf8()));
    }
    (folded, units)
}

fn hit_at(layout: &Layout, x: f32, y: f32) -> Option<Hit> {
    [
        (Hit::Query, Some(layout.query)),
        (Hit::Previous, Some(layout.previous)),
        (Hit::Next, Some(layout.next)),
        (Hit::ReplaceToggle, Some(layout.replace_toggle)),
        (Hit::OptionsToggle, Some(layout.options_toggle)),
        (Hit::Close, Some(layout.close)),
        (Hit::Replacement, layout.replacement),
        (Hit::ReplaceAll, layout.replace_all),
        (Hit::ReplaceOne, layout.replace_one),
        (Hit::MatchCase, layout.match_case),
        (Hit::WholeWords, layout.whole_words),
    ]
    .into_iter()
    .find_map(|(hit, rect)| rect.filter(|rect| rect.contains(x, y)).map(|_| hit))
}

/// The find bar's chips.
///
/// **`Literal`, and one of them is a debt rather than a platform chord.** The
/// arrows and `Esc` are the platform's and will never have a registry row; the
/// Replace toggle's chord is this app's own binding, resolved by hand a few
/// hundred lines above (through the *primary* modifier now — ⌘ on a Mac) and
/// written out by hand here — which is what the registry exists to stop. It
/// still needs a table change rather than a row (one action with two chords),
/// so it stays in `docs/contributing/techdebt/README.md`; the literal below at least follows the
/// platform so the tooltip cannot disagree with the binding.
fn tooltip_for(hit: Hit, layout: &Layout) -> Option<TooltipContent> {
    use koru_keys::{Hinted, Section, show_hint};
    let hint = |text| show_hint(Hinted::Literal(text), Section::Tooltip);
    match hit {
        Hit::Previous => Some(
            TooltipContent::new(1, layout.previous, "Previous match")
                .with_optional_shortcut(hint("Up")),
        ),
        Hit::Next => Some(
            TooltipContent::new(2, layout.next, "Next match").with_optional_shortcut(hint("Down")),
        ),
        Hit::ReplaceToggle => Some(
            TooltipContent::new(3, layout.replace_toggle, "Replace").with_optional_shortcut(hint(
                if cfg!(target_os = "macos") {
                    "⌘H / ⌘⌥F"
                } else {
                    "Ctrl+H / Ctrl+Alt+F"
                },
            )),
        ),
        Hit::OptionsToggle => Some(TooltipContent::new(
            4,
            layout.options_toggle,
            "Match options",
        )),
        Hit::Close => {
            Some(TooltipContent::new(5, layout.close, "Close").with_optional_shortcut(hint("Esc")))
        }
        _ => None,
    }
}

fn draw_icon_controls(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    layout: &Layout,
    search: &LocalSearch,
) {
    koru_ui::divider::draw_vertical(canvas, root, layout.divider, 10.0);
    let metrics = koru_ui::icon_button::Metrics::COMPACT;
    for (hit, rect, glyph, active) in [
        (Hit::Previous, layout.previous, icons::CARET_UP, false),
        (Hit::Next, layout.next, icons::CARET_DOWN, false),
        (
            Hit::ReplaceToggle,
            layout.replace_toggle,
            icons::REPLACE,
            search.subpanel == Subpanel::Replace,
        ),
        (Hit::Close, layout.close, icons::WINDOW_CLOSE, false),
    ] {
        koru_ui::icon_button::draw_with(
            canvas,
            root,
            engine,
            rect,
            glyph,
            koru_ui::icon_button::State {
                hovered: search.hover == Some(hit),
                active,
                disabled: search.matches.is_empty() && matches!(hit, Hit::Previous | Hit::Next),
            },
            metrics,
        );
    }
    // Collapsing the rows must not hide that non-default matching is active.
    let options_active = search.match_options_active();
    koru_ui::icon_button::draw_label_with(
        canvas,
        root,
        engine,
        layout.options_toggle,
        "Aa",
        theme::role::row(),
        koru_ui::icon_button::State {
            hovered: search.hover == Some(Hit::OptionsToggle),
            active: options_active,
            disabled: false,
        },
        metrics,
    );
}

fn draw_count(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    layout: &Layout,
    search: &LocalSearch,
    now: Instant,
) {
    let label = if search.query.is_empty() {
        String::new()
    } else if search.matches.is_empty() {
        "No results".to_owned()
    } else {
        let mut label = format!(
            "{} of {}",
            search.active.unwrap_or(0) + 1,
            search.matches.len()
        );
        if search.wrapped_until.is_some_and(|until| now < until) {
            label.push_str(" \u{00B7} back to top");
        }
        label
    };
    if label.is_empty() {
        return;
    }
    let text = engine.simple_layout(&label, theme::role::hint());
    let right = layout.previous.x - 6.0;
    scene::draw_layout_first_line(
        canvas,
        root,
        &text,
        (right - text.width()).max(layout.query.x + layout.query.w + 4.0),
        baseline_centred_y(&text, layout.panel.y, ROW_H),
        Some(theme::p().text_dim),
    );
}

fn toast_geometry(editor: &Editor) -> (Rect, f32) {
    let view = &editor.view;
    let width = view.view_mode.effective_w(view.viewport.0);
    let over = Rect::new(view.content_x, 0.0, width, view.viewport.1);
    (over, view.viewport.1 - view.status_h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_core::WorkspaceId;
    use koru_core::node::Node;
    use koru_core::schema::Parent;
    use koru_editor::Editing;

    fn document_editor(rows: &[Node]) -> (WorkspaceDoc, Editor, LocalSearch, Vec<NodeId>) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "search").unwrap();
        let document = ws
            .insert(Parent::Root, Node::document("work spec").unwrap())
            .unwrap();
        let ids: Vec<_> = rows
            .iter()
            .cloned()
            .map(|row| ws.insert(Parent::Node(document), row).unwrap())
            .collect();
        ws.enable_undo();
        let mut editor = Editor::rooted_at(&ws, document, true);
        editor.body.title_row = true;
        editor.body.rebuild(&ws);
        editor.view.tables_enabled = true;
        editor.reflow();
        let search = LocalSearch::new(&mut editor.view.engine);
        (ws, editor, search, ids)
    }

    #[test]
    fn matching_is_literal_case_aware_and_unicode_safe() {
        assert_eq!(
            find_ranges("Work work", "work", false, false),
            vec![0..4, 5..9]
        );
        assert_eq!(find_ranges("Work work", "work", true, false), vec![5..9]);
        assert_eq!(
            find_ranges("\u{130}x i\u{307}x", "i\u{307}", false, false),
            vec![0..2, 4..7]
        );
    }

    #[test]
    fn whole_words_reject_substrings() {
        assert_eq!(
            find_ranges("workflow work", "work", false, true),
            vec![9..13]
        );
    }

    #[test]
    fn find_copy_and_reopen_state_are_plain_ascii_and_collapse_old_selection() {
        let (ws, mut editor, mut search, _) = document_editor(&[Node::paragraph("work").unwrap()]);
        assert_eq!(query_placeholder(Subpanel::None), "Find");
        assert_eq!(query_placeholder(Subpanel::Options), "Find");
        assert_eq!(query_placeholder(Subpanel::Replace), "Find or replace...");

        search.query.set_text(&mut editor.view.engine, "work");
        search.open(&mut editor, false);
        search.query.select_all(&mut editor.view.engine);
        search.close(&ws, &mut editor, false);
        search.open(&mut editor, false);
        assert!(
            search.query.caret_at_end(),
            "reopening a remembered query must not retain its old selection"
        );
    }

    #[test]
    fn ctrl_h_toggles_replace_and_backspace_control_codes_never_open_search() {
        let (mut ws, mut editor, mut search, _) =
            document_editor(&[Node::paragraph("hello world").unwrap()]);
        let ctrl = koru_keys::PRIMARY;
        let raw_backspace = Key::Character("\u{8}".into());

        assert!(!search.key(&mut ws, &mut editor, &raw_backspace, Some("\u{8}"), ctrl,));
        assert!(!search.is_open(), "Ctrl+Backspace is not Ctrl+H");
        assert!(!search.key(
            &mut ws,
            &mut editor,
            &Key::Named(NamedKey::Backspace),
            Some("\u{8}"),
            ctrl,
        ));
        assert!(
            !search.is_open(),
            "a shell-repaired Backspace retaining U+0008 is not Ctrl+H"
        );

        let h = Key::Character("h".into());
        assert!(search.key(&mut ws, &mut editor, &h, Some("h"), ctrl));
        assert_eq!(search.subpanel, Subpanel::Replace);
        assert!(search.key(&mut ws, &mut editor, &h, Some("h"), ctrl));
        assert_eq!(search.subpanel, Subpanel::None);
        assert_eq!(search.focus, Field::Query);
        assert!(search.key(&mut ws, &mut editor, &h, Some("h"), ctrl));
        assert_eq!(search.subpanel, Subpanel::Replace);
        assert_eq!(search.focus, Field::Replacement);
    }

    #[test]
    fn empty_replace_backspace_and_shift_tab_return_to_find() {
        let (mut ws, mut editor, mut search, _) =
            document_editor(&[Node::paragraph("hello").unwrap()]);
        search.query.set_text(&mut editor.view.engine, "hello");
        search.open(&mut editor, false);
        let h = Key::Character("h".into());
        assert!(search.key(&mut ws, &mut editor, &h, Some("h"), koru_keys::PRIMARY,));
        assert_eq!(search.focus, Field::Replacement);

        assert!(search.key(
            &mut ws,
            &mut editor,
            &Key::Named(NamedKey::Backspace),
            None,
            ModifiersState::empty(),
        ));
        assert_eq!(search.subpanel, Subpanel::None);
        assert_eq!(search.focus, Field::Query);

        assert!(search.key(&mut ws, &mut editor, &h, Some("h"), koru_keys::PRIMARY,));
        assert!(search.key(
            &mut ws,
            &mut editor,
            &Key::Named(NamedKey::Tab),
            None,
            ModifiersState::SHIFT,
        ));
        assert_eq!(search.subpanel, Subpanel::None);
        assert_eq!(search.focus, Field::Query);
    }

    #[test]
    fn shift_tab_keeps_non_empty_replace_open_and_returns_to_find() {
        let (mut ws, mut editor, mut search, _) =
            document_editor(&[Node::paragraph("hello").unwrap()]);
        search.query.set_text(&mut editor.view.engine, "hello");
        search.open(&mut editor, true);
        search
            .replacement
            .set_text(&mut editor.view.engine, "world");
        search.focus = Field::Replacement;

        assert!(search.key(
            &mut ws,
            &mut editor,
            &Key::Named(NamedKey::Tab),
            None,
            ModifiersState::SHIFT,
        ));
        assert_eq!(search.subpanel, Subpanel::Replace);
        assert_eq!(search.focus, Field::Query);
        assert_eq!(search.replacement.raw_text(), "world");
    }

    #[test]
    fn tab_moves_from_find_to_replace_and_selects_existing_text() {
        let (mut ws, mut editor, mut search, _) =
            document_editor(&[Node::paragraph("hello").unwrap()]);
        search.query.set_text(&mut editor.view.engine, "hello");
        search.open(&mut editor, true);
        search
            .replacement
            .set_text(&mut editor.view.engine, "existing");
        search.focus = Field::Query;

        assert!(search.key(
            &mut ws,
            &mut editor,
            &Key::Named(NamedKey::Tab),
            None,
            ModifiersState::empty(),
        ));
        assert_eq!(search.focus, Field::Replacement);
        assert!(search.key(
            &mut ws,
            &mut editor,
            &Key::Character("n".into()),
            Some("n"),
            ModifiersState::empty(),
        ));
        assert_eq!(search.replacement.raw_text(), "n");
    }

    #[test]
    fn changing_find_replace_focus_collapses_the_field_left_behind() {
        let (mut ws, mut editor, mut search, _) =
            document_editor(&[Node::paragraph("hello").unwrap()]);
        search.query.set_text(&mut editor.view.engine, "hello");
        search.open(&mut editor, false);
        search.query.select_all(&mut editor.view.engine);

        let (x, y) = search.layout(&editor).replace_toggle.centre();
        assert!(search.mouse_down(&mut ws, &mut editor, x, y));
        assert_eq!(search.focus, Field::Replacement);
        assert!(
            !search.query.has_selection(),
            "Find deselects when Replace takes focus"
        );

        search
            .replacement
            .set_text(&mut editor.view.engine, "world");
        search.replacement.select_all(&mut editor.view.engine);
        let (x, y) = search.layout(&editor).query.centre();
        assert!(search.mouse_down(&mut ws, &mut editor, x, y));
        assert_eq!(search.focus, Field::Query);
        assert!(
            !search.replacement.has_selection(),
            "Replace deselects when Find takes focus"
        );
    }

    #[test]
    fn shift_enter_replaces_then_moves_to_the_previous_match() {
        let (mut ws, mut editor, mut search, ids) =
            document_editor(&[Node::paragraph("hello hello hello").unwrap()]);
        search.query.set_text(&mut editor.view.engine, "hello");
        search.open(&mut editor, true);
        search
            .replacement
            .set_text(&mut editor.view.engine, "world");
        search.focus = Field::Replacement;
        search.active = Some(1);

        assert!(search.key(
            &mut ws,
            &mut editor,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::SHIFT,
        ));
        assert_eq!(
            ws.read(ids[0]).unwrap().content.as_text(),
            Some("hello world hello")
        );
        let previous = search
            .active
            .and_then(|index| search.matches.get(index))
            .expect("the previous match remains active");
        assert_eq!(previous.source, 0..5);
    }

    #[test]
    fn collapsed_aa_stays_active_for_non_default_match_options() {
        let (_, mut editor, mut search, _) = document_editor(&[Node::paragraph("work").unwrap()]);
        search.open(&mut editor, false);
        assert!(!search.match_options_active());
        search.match_case = true;
        assert!(search.match_options_active());
        search.match_case = false;
        search.whole_words = true;
        assert!(search.match_options_active());
    }

    #[test]
    fn every_top_row_icon_has_shared_tooltip_content_and_hover_state() {
        let (_, mut editor, mut search, _) = document_editor(&[Node::paragraph("work").unwrap()]);
        search.query.set_text(&mut editor.view.engine, "work");
        search.open(&mut editor, false);
        let layout = search.layout(&editor);
        for hit in [
            Hit::Previous,
            Hit::Next,
            Hit::ReplaceToggle,
            Hit::OptionsToggle,
            Hit::Close,
        ] {
            assert!(
                tooltip_for(hit, &layout).is_some(),
                "missing {hit:?} tooltip"
            );
        }
        let x = layout.previous.x + layout.previous.w * 0.5;
        let y = layout.previous.y + layout.previous.h * 0.5;
        assert!(search.mouse_move(&mut editor, x, y));
        assert_eq!(search.hover, Some(Hit::Previous));
        assert!(search.tooltip.is_present());

        let top_centre = layout.panel.y + ROW_H * 0.5;
        for rect in [
            layout.query,
            layout.previous,
            layout.next,
            layout.replace_toggle,
            layout.options_toggle,
            layout.close,
        ] {
            assert!((rect.y + rect.h * 0.5 - top_centre).abs() < 0.01);
        }
        let line_x = layout.divider.x + layout.divider.w * 0.5;
        assert_eq!(
            line_x - (layout.options_toggle.x + layout.options_toggle.w),
            8.0
        );
        assert_eq!(layout.close.x - line_x, 8.0);
        assert_eq!(layout.divider.w, koru_ui::divider::VERTICAL_LANE_W);
        assert_eq!(layout.previous.w, CONTROL_W);
        assert_eq!(layout.previous.h, CONTROL_H);
    }

    #[test]
    fn undo_follows_single_replacements_backwards_to_each_word_start() {
        let (mut ws, mut editor, mut search, ids) =
            document_editor(&[Node::paragraph("hello hello").unwrap()]);
        search.query.set_text(&mut editor.view.engine, "hello");
        search.open(&mut editor, true);
        search
            .replacement
            .set_text(&mut editor.view.engine, "world");
        search.replace_one(&mut ws, &mut editor, 1);
        search.replace_one(&mut ws, &mut editor, 1);
        search.close(&ws, &mut editor, true);
        assert_eq!(
            ws.read(ids[0]).unwrap().content.as_text(),
            Some("world world")
        );

        let undo = Key::Character("z".into());
        assert!(editor.key(&mut ws, &undo, Some("z"), koru_keys::PRIMARY));
        assert_eq!(
            ws.read(ids[0]).unwrap().content.as_text(),
            Some("world hello")
        );
        assert_eq!(editor.body.focused_id(), Some(ids[0]));
        assert_eq!(editor.body.focused().unwrap().caret(), 6);

        assert!(editor.key(&mut ws, &undo, Some("z"), koru_keys::PRIMARY));
        assert_eq!(
            ws.read(ids[0]).unwrap().content.as_text(),
            Some("hello hello")
        );
        assert_eq!(editor.body.focused_id(), Some(ids[0]));
        assert_eq!(editor.body.focused().unwrap().caret(), 0);

        assert!(editor.key(
            &mut ws,
            &undo,
            Some("z"),
            koru_keys::PRIMARY | ModifiersState::SHIFT,
        ));
        assert!(editor.key(
            &mut ws,
            &undo,
            Some("z"),
            koru_keys::PRIMARY | ModifiersState::SHIFT,
        ));
        assert_eq!(
            ws.read(ids[0]).unwrap().content.as_text(),
            Some("world world")
        );
        assert!(editor.key(&mut ws, &undo, Some("z"), koru_keys::PRIMARY));
        assert_eq!(editor.body.focused_id(), Some(ids[0]));
        assert_eq!(editor.body.focused().unwrap().caret(), 6);
    }

    /// Give `row` a document pill captioned `label`, at the end of its text —
    /// the real path, through `insert_pill`, so the anchor, the mark and the
    /// attribute are all the ones the app writes.
    fn add_document_pill(ws: &mut WorkspaceDoc, row: NodeId, label: &str) {
        let at = ws.read(row).unwrap().content.as_text().map_or(0, str::len);
        let attr = koru_core::node::Attr::labelled(
            "document",
            koru_core::node::AttrValue::Document(koru_core::DocumentReference::at(
                koru_core::DocumentId::new(),
                koru_core::NodeId::new(),
            )),
            label.to_owned(),
        );
        ws.insert_pill(row, at, &attr).expect("a pill");
    }

    /// **A document reference is findable by the name printed on it.**
    ///
    /// A pill occupies one `U+2060` in the node's text and its caption is
    /// resolved elsewhere, so scanning `row.text` alone found nothing —
    /// searching "dashboard" in a page linking to Dashboard said "No results"
    /// while the word was on screen.
    #[test]
    fn a_pill_is_found_by_the_caption_printed_on_it() {
        let (mut ws, mut editor, mut search, ids) =
            document_editor(&[Node::paragraph("see ").unwrap()]);
        add_document_pill(&mut ws, ids[0], "Dashboard");
        editor.body.rebuild(&ws);
        editor.reflow();

        search.query.set_text(&mut editor.view.engine, "dashboard");
        search.open(&mut editor, false);

        assert_eq!(search.matches.len(), 1, "the caption matched, case-folded");
        let found = &search.matches[0];
        assert!(found.pill);
        assert_eq!(found.node, ids[0]);
        assert_eq!(
            &ws.read(ids[0]).unwrap().content.as_text().unwrap()[found.source.clone()],
            koru_core::text::ANCHOR_STR,
            "the range is the pill's anchor, not a slice of a word in the text"
        );
    }

    /// One match per pill, whatever its caption says, and the row's matches are
    /// stepped in reading order rather than text-then-pills.
    #[test]
    fn a_pill_is_one_match_and_sits_in_reading_order() {
        let (mut ws, mut editor, mut search, ids) =
            document_editor(&[Node::paragraph("aa ").unwrap()]);
        add_document_pill(&mut ws, ids[0], "aa aa");
        ws.replace_text(ids[0], 6..6, " aa").expect("trailing text");
        editor.body.rebuild(&ws);
        editor.reflow();

        search.query.set_text(&mut editor.view.engine, "aa");
        search.open(&mut editor, false);

        let body: Vec<bool> = search
            .matches
            .iter()
            .filter(|found| !found.caption)
            .map(|found| found.pill)
            .collect();
        assert_eq!(
            body,
            vec![false, true, false],
            "text, then the pill once however many times its caption matched, then text"
        );
    }

    /// **Replace never touches a pill.** Its `source` is the anchor, so writing
    /// over it would delete the reference; and renaming another document is not
    /// something the find bar may do. The pill still counts as a match — you
    /// can find it, step to it and select it.
    #[test]
    fn replace_all_leaves_a_pill_and_its_caption_alone() {
        let (mut ws, mut editor, mut search, ids) =
            document_editor(&[Node::paragraph("dashboard ").unwrap()]);
        add_document_pill(&mut ws, ids[0], "Dashboard");
        editor.body.rebuild(&ws);
        editor.reflow();

        search.query.set_text(&mut editor.view.engine, "dashboard");
        search.open(&mut editor, true);
        assert_eq!(search.matches.len(), 2, "the word and the pill");
        search
            .replacement
            .set_text(&mut editor.view.engine, "board");
        search.replace_all(&mut ws, &mut editor);

        let text = ws
            .read(ids[0])
            .unwrap()
            .content
            .as_text()
            .unwrap()
            .to_owned();
        assert_eq!(
            text,
            format!("board {}", koru_core::text::ANCHOR_STR),
            "the word changed and the anchor survived"
        );
        let attrs = ws.read(ids[0]).unwrap().content.attrs().to_vec();
        assert_eq!(attrs.len(), 1, "the pill's attribute is still there");
        assert_eq!(attrs[0].label.as_deref(), Some("Dashboard"));
        assert_eq!(
            search.toast.as_ref().unwrap().message(),
            "Replaced 1 matches",
            "the pill is not counted as replaced"
        );
    }

    /// Replace on a pill walks to the next match rather than sitting there
    /// doing nothing — otherwise Enter would stall in a page full of links.
    #[test]
    fn replace_one_on_a_pill_steps_past_it() {
        let (mut ws, mut editor, mut search, ids) = document_editor(&[
            Node::paragraph("").unwrap(),
            Node::paragraph("dashboard").unwrap(),
        ]);
        add_document_pill(&mut ws, ids[0], "Dashboard");
        editor.body.rebuild(&ws);
        editor.reflow();

        search.query.set_text(&mut editor.view.engine, "dashboard");
        search.open(&mut editor, true);
        let pill = search
            .matches
            .iter()
            .position(|found| found.pill)
            .expect("a pill match");
        search.active = Some(pill);
        search
            .replacement
            .set_text(&mut editor.view.engine, "board");

        search.replace_one(&mut ws, &mut editor, 1);
        assert_ne!(search.active, Some(pill), "Enter walked forward past it");
        search.active = Some(pill);
        search.replace_one(&mut ws, &mut editor, -1);
        assert_ne!(search.active, Some(pill), "and Shift+Enter walked back");

        assert_eq!(
            ws.read(ids[1]).unwrap().content.as_text(),
            Some("dashboard"),
            "and nothing was replaced by the step"
        );
        assert_eq!(
            ws.read(ids[0]).unwrap().content.as_text(),
            Some(koru_core::text::ANCHOR_STR),
            "least of all the pill"
        );
    }

    #[test]
    fn replace_all_includes_the_title_and_forward_undo_restores_every_source() {
        let (mut ws, mut editor, mut search, ids) = document_editor(&[
            Node::paragraph("work one").unwrap(),
            Node::paragraph("other work").unwrap(),
        ]);
        search.query.set_text(&mut editor.view.engine, "work");
        search.open(&mut editor, true);
        assert_eq!(search.matches.len(), 3, "title plus two body rows");
        search.replacement.set_text(&mut editor.view.engine, "job");
        search.replace_all(&mut ws, &mut editor);
        assert_eq!(
            ws.read(editor.body.root.unwrap())
                .unwrap()
                .content
                .as_text(),
            Some("job spec")
        );
        assert_eq!(ws.read(ids[0]).unwrap().content.as_text(), Some("job one"));
        assert_eq!(
            ws.read(ids[1]).unwrap().content.as_text(),
            Some("other job")
        );
        assert_eq!(
            search.toast.as_ref().unwrap().message(),
            "Replaced 3 matches"
        );

        search.undo_replace_all(&mut ws, &mut editor);
        assert_eq!(
            ws.read(editor.body.root.unwrap())
                .unwrap()
                .content
                .as_text(),
            Some("work spec")
        );
        assert_eq!(ws.read(ids[0]).unwrap().content.as_text(), Some("work one"));
        assert_eq!(
            ws.read(ids[1]).unwrap().content.as_text(),
            Some("other work")
        );
    }

    #[test]
    fn rendered_table_match_replaces_the_exact_escaped_markdown_source() {
        let (mut ws, mut editor, mut search, ids) =
            document_editor(&[Node::table("| Value |\n| --- |\n| a\\|b |").unwrap()]);
        search.query.set_text(&mut editor.view.engine, "|");
        search.open(&mut editor, true);
        assert_eq!(search.matches.len(), 1);
        assert!(search.matches[0].table.is_some());
        search
            .replacement
            .set_text(&mut editor.view.engine, " pipe ");
        search.replace_one(&mut ws, &mut editor, 1);
        assert_eq!(
            ws.read(ids[0]).unwrap().content.as_text(),
            Some("| Value |\n| --- |\n| a pipe b |")
        );
    }

    #[test]
    fn releasing_a_query_click_stops_later_moves_extending_the_selection() {
        let (mut ws, mut editor, mut search, _) =
            document_editor(&[Node::paragraph("hello").unwrap()]);
        search.query.set_text(&mut editor.view.engine, "hello");
        search.open(&mut editor, false);
        let layout = search.layout(&editor);
        let x = layout.query.x + 20.0;
        let y = layout.query.y + layout.query.h * 0.5;

        assert!(search.mouse_down(&mut ws, &mut editor, x, y));
        assert!(search.query.is_dragging());
        search.mouse_up();
        assert!(!search.query.is_dragging());

        assert!(search.mouse_move(&mut editor, x + 80.0, y));
        assert!(
            !search.query.has_selection(),
            "a hover after release is not a drag selection"
        );
    }

    #[test]
    fn stepping_to_an_off_screen_match_scrolls_it_into_view() {
        let rows: Vec<_> = (0..40)
            .map(|i| Node::paragraph(&format!("line {i} findme")).unwrap())
            .collect();
        let (_ws, mut editor, mut search, _) = document_editor(&rows);
        editor.set_viewport(900.0, 300.0);
        search.query.set_text(&mut editor.view.engine, "findme");
        search.open(&mut editor, false);
        let initial_scroll = editor.view.scroll;

        let mut scrolled = false;
        for _ in 0..search.matches.len().min(35) {
            search.step(&mut editor, 1);
            if editor.view.scroll > initial_scroll + 1.0 {
                scrolled = true;
                break;
            }
        }
        assert!(
            scrolled,
            "stepping to an off-screen match must scroll the document"
        );
    }

    #[test]
    fn opening_find_on_a_far_match_scrolls_to_it() {
        let rows: Vec<_> = (0..200)
            .map(|i| {
                if i == 180 {
                    Node::paragraph("the onboarding target").unwrap()
                } else {
                    Node::paragraph(&format!("line {i}")).unwrap()
                }
            })
            .collect();
        let (_ws, mut editor, mut search, _) = document_editor(&rows);
        editor.set_viewport(900.0, 300.0);
        search.query.set_text(&mut editor.view.engine, "onboarding");
        search.open(&mut editor, false);
        assert!(
            editor.view.scroll > 1.0,
            "opening find on a far match must scroll to it"
        );
    }
}
