// SPDX-License-Identifier: MPL-2.0

//! One line of editable text inside a panel.
//!
//! The name prompt and the link prompt both had ~50 lines of near-identical
//! `PlainEditor` driving — backspace, word-delete, arrows, Home/End, Ctrl+A,
//! printable text. They differed only in whether Shift extended the selection,
//! and that difference was an accident rather than a decision.
//!
//! `PlainEditor` is the right tool here and the wrong one for a row: it has no
//! inline-box API, so it cannot carry a pill, but a prompt field never needs to.
//!
//! ## Why a struct with a private editor
//!
//! Undo. parley has none — no history, no snapshot, no transaction — so it is
//! hand-rolled below, and a history is only correct if the snapshot is taken by
//! the code that performs the mutation, immediately before it. Keeping `editor`
//! private is what stops a sibling module reaching for
//! `driver().insert_or_replace_selection(..)` and silently skipping a step.
//! That failure would not show up until somebody pressed Ctrl+Z months later.
//!
//! ## Why not the document's key map
//!
//! `koru_editor::input::keymap` resolves to `Action`, which is the
//! *document's* vocabulary: `Indent`, `Reorder`, `SplitLine`, `DeleteRows`,
//! `ToggleSharing`. A field would have to accept a dozen and silently drop the
//! rest, `resolve` would turn Tab into `Indent(1)` when the panel needs Tab,
//! and Ctrl+Shift+↑ would resolve to `Reorder` in a box with no rows. The match
//! below is longer than a table lookup and says only true things.
//!
//! ## Why it is here and not in `koru-editor`
//!
//! It was there, but generic prompts could not reach it without linking
//! `koru-core`, `koru-workspace` and `koru-blocks` — the node model, the
//! persistence layer and the syntax engine. A text box needs none of those.
//!
//! It belongs here by `koru-ui`'s own dividing question (README: *does it have
//! state, or is it a function of its arguments?*): it has an undo history, a
//! scroll offset and a selection, and it names no node, no workspace and no
//! document. `koru-editor` re-exports it, so the name prompt and the link
//! prompt say nothing new.

use parley::editing::PlainEditor;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use koru_draw::text::TextEngine;
use koru_draw::theme::Brush;

/// The gap between a field's border and its text.
///
/// Here rather than beside the drawing, because hit-testing needs the same
/// number: a click lands on the character it looks like it lands on only while
/// both agree about where the text starts.
pub const PAD: f32 = 7.0;

/// How tall a field is when a floating panel wears one above its rows.
pub const SEARCH_H: f32 = 30.0;

/// The margin around that field — **not** [`PAD`], which is the gap inside its
/// border. Two numbers, two jobs, and confusing them moves the caret.
const SEARCH_PAD: f32 = 8.0;

/// Where a panel's field sits, in panel-local coordinates.
///
/// Here rather than in one of the panels because four of them wear the same
/// box — the slash menu, the tag chooser, the emoji picker's cousin and the
/// due grid's phrase field — and the only way two boxes stay one shape is for
/// both to ask one function. The due grid asks from `koru-ui` and the command
/// menu asks through its own `search_rect`, which is this.
pub fn search_rect(panel_w: f32) -> koru_draw::geometry::Rect {
    koru_draw::geometry::Rect::new(
        SEARCH_PAD,
        SEARCH_PAD,
        (panel_w - SEARCH_PAD * 2.0).max(1.0),
        SEARCH_H,
    )
}

/// **The panel search box, drawn**: the box [`search_rect`] places — the
/// page's background, the focus ring, a 6 px corner — and the field in it,
/// with `placeholder` while it is empty. One recipe for every panel that
/// wears it: the slash menu, the date grid's phrase field and the quick
/// workspace switcher (2026-11-10, the user: *reuse that input*). They used
/// to copy these lines; a copy is a shape that drifts.
#[allow(clippy::too_many_arguments)]
pub fn draw_panel_search(
    scene: &mut Scene,
    root: Affine,
    field: &mut TextField,
    engine: &mut TextEngine,
    placeholder: &str,
    panel_x: f32,
    panel_y: f32,
    panel_w: f32,
) {
    let ghost = engine.simple_layout(placeholder, theme::role::ui());
    let r = search_rect(panel_w);
    let shape = koru_draw::scene::rrect(panel_x + r.x, panel_y + r.y, r.w, r.h, 6.0);
    koru_draw::scene::fill(scene, root, theme::p().bg, &shape);
    // The focus ring, not the panel border: the caret is in here the moment
    // the panel opens, so the box looks like the thing being typed into.
    koru_draw::scene::stroke(scene, root, theme::p().focus_ring, theme::BORDER_W, &shape);
    // `shows_placeholder`, never `is_empty`: an authored-empty field holding
    // a dictation transcript is still empty, and the ghost would be drawn
    // underneath the words (ADR-272).
    let empty = field.shows_placeholder();
    draw_field(
        scene,
        root,
        field,
        engine,
        empty.then_some(&ghost),
        (panel_x + r.x, panel_y + r.y, r.w, r.h),
        true,
    );
}

/// What that box costs a panel: the margin, the box, the margin again.
///
/// A caller measuring from *inside* the panel padding subtracts
/// `theme::PANEL_PAD` from it — which is what `command_menu::rows_top` is.
pub fn search_band() -> f32 {
    SEARCH_PAD + SEARCH_H + SEARCH_PAD
}

/// What the field did with a key, so the caller knows whether to look further.
///
/// `Passed` is documentation rather than control flow today — both prompts
/// pre-empt those keys before calling down, and neither reads the return. It
/// stays because the list below is the one statement anywhere of *which keys
/// belong to the panel*, and because `NamePrompt` claims only Enter: without
/// the list a Tab would reach the typing arm and be caught only by the
/// incidental control-character guard.
#[derive(PartialEq, Eq, Debug)]
pub enum Typed {
    /// The field took it.
    Handled,
    /// Not a text-editing key — the panel should decide (Enter, Tab, Escape).
    Passed,
}

// ------------------------------------------------------------------- undo

/// One point the field can be put back to.
///
/// Text *and* selection, because undoing "select all, type x" has to put the
/// old text back **and** leave it selected — otherwise the next Ctrl+Z lands
/// somewhere the user was never standing. Byte indices rather than a
/// `Selection`: `PlainEditor::set_selection` is private, so the only way back
/// in is `select_byte_range(anchor, focus)`, which takes exactly this pair and
/// keeps a selection that was dragged backwards pointing backwards.
#[derive(Clone)]
struct Snapshot {
    text: String,
    anchor: usize,
    focus: usize,
}

impl Snapshot {
    fn of(editor: &PlainEditor<Brush>) -> Self {
        let sel = editor.raw_selection();
        Self {
            text: editor.raw_text().to_owned(),
            anchor: sel.anchor().index(),
            focus: sel.focus().index(),
        }
    }
}

/// What kind of edit opened the step currently being collected.
///
/// A run of typing is one Ctrl+Z and a run of backspaces is another, but the
/// two must never merge: deleting a word you have just typed and pressing undo
/// should give the word back, not empty the box.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Edit {
    Typing,
    Deleting,
    /// Never coalesces — a paste or a cut is always a step of its own.
    Other,
}

/// Deep enough that nobody reaches the end of it in a one-line prompt, and
/// shallow enough that the memory is not worth thinking about. A field holds
/// tens of bytes, so whole-text snapshots are cheaper than an inverse-op log
/// would be to get right.
const DEPTH: usize = 128;

#[derive(Default)]
struct History {
    past: Vec<Snapshot>,
    future: Vec<Snapshot>,
    /// The step still accepting more of the same edit. `None` is a closed
    /// boundary: the next edit opens a fresh step whatever kind it is.
    open: Option<Edit>,
}

impl History {
    /// Note that an edit of `kind` is about to happen, and report whether that
    /// opened a new step.
    ///
    /// Idempotent within a run of the same kind — which is what makes a burst
    /// of typing one Ctrl+Z. The same shape as the document's `begin_group`
    /// (ADR-16), for the same reason.
    fn begin(&mut self, editor: &PlainEditor<Brush>, kind: Edit) -> bool {
        if self.open == Some(kind) && kind != Edit::Other {
            return false;
        }
        self.past.push(Snapshot::of(editor));
        if self.past.len() > DEPTH {
            self.past.remove(0);
        }
        self.future.clear();
        self.open = Some(kind);
        true
    }

    /// Close the step, so the next edit starts a fresh one. Every boundary: a
    /// space, a paste, a cut, a caret move, a click, a field switch.
    fn checkpoint(&mut self) {
        self.open = None;
    }

    /// Take back a step the edit turned out not to need — backspace at offset
    /// zero, delete at the end. Without this those keys leave a step that does
    /// nothing when it is reached, which reads as undo being broken.
    ///
    /// `opened` must be what [`begin`] returned. A no-op edit that did *not*
    /// open a step must never discard one an earlier edit did.
    ///
    /// [`begin`]: Self::begin
    fn drop_if_unchanged(&mut self, opened: bool, editor: &PlainEditor<Brush>) {
        if !opened {
            return;
        }
        if self
            .past
            .last()
            .is_some_and(|s| s.text == editor.raw_text())
        {
            self.past.pop();
            self.open = None;
        }
    }

    /// Record a snapshot somebody else took as one closed step.
    ///
    /// [`begin`] cannot serve a dictation: it snapshots **at begin time**, and
    /// a composition's step is opened when the *final* transcript arrives —
    /// by then the editor holds the last partial, so `begin` would record that
    /// and Ctrl+Z would restore a half-heard sentence instead of the text the
    /// person had authored. The composition keeps the pre-composition snapshot
    /// itself and hands it here (ADR-272).
    ///
    /// [`begin`]: Self::begin
    fn record(&mut self, snap: Snapshot) {
        self.past.push(snap);
        if self.past.len() > DEPTH {
            self.past.remove(0);
        }
        self.future.clear();
        self.open = None;
    }

    fn undo(&mut self, editor: &mut PlainEditor<Brush>, engine: &mut TextEngine) -> bool {
        let Some(prev) = self.past.pop() else {
            return false;
        };
        self.future.push(Snapshot::of(editor));
        restore(&prev, editor, engine);
        self.open = None;
        true
    }

    fn redo(&mut self, editor: &mut PlainEditor<Brush>, engine: &mut TextEngine) -> bool {
        let Some(next) = self.future.pop() else {
            return false;
        };
        self.past.push(Snapshot::of(editor));
        restore(&next, editor, engine);
        self.open = None;
        true
    }
}

/// Put the editor back to a snapshot.
///
/// **The two calls must stay adjacent.** `set_text` clears the buffer and marks
/// the layout dirty, leaving the selection pointing into the *old* layout, and
/// parley offers no `set_selection` to correct that from outside.
/// `select_byte_range` refreshes the layout itself before it builds cursors, so
/// the stale selection is never read — but only if nothing reads it in between.
/// The indices are always valid, since they were recorded against this text.
fn restore(snap: &Snapshot, editor: &mut PlainEditor<Brush>, engine: &mut TextEngine) {
    editor.set_text(&snap.text);
    editor
        .driver(&mut engine.font_cx, &mut engine.layout_cx)
        .select_byte_range(snap.anchor, snap.focus);
}

// ------------------------------------------------------- voice composition

/// An open dictation, and everything needed to take it back.
///
/// **The representation is the retained snapshot.** Three facts in the code
/// above rule out the obvious alternative of simply typing each partial into
/// the editor and remembering a range:
///
/// - [`TextField::raw_text`] hands out a `&str` borrowed straight from the
///   editor, so it cannot omit an interior run;
/// - [`History::begin`] snapshots at *begin* time, so opening the undo step at
///   commit would record the last partial;
/// - `draw_field`'s `ghost` draws at the field origin and only while the field
///   is empty — it is a placeholder, not an inline slot.
///
/// So the provisional text *is* written into the editor, because it has to lay
/// out, scroll and be measured exactly like real text — and every **authored**
/// read is served from `before` instead. See the query table in
/// [ADR-248](../../../docs/decisions/ADR-226-250.md#adr-248-voicecomposition--dictation-is-an-ephemeral-composition-and-only-the-final-transcript-is-a-transaction).
struct Composition {
    /// Text, anchor and focus as they stood before the first provisional byte.
    /// This is what authored reads answer from, what cancel restores, and what
    /// commit records as the one undo step.
    before: Snapshot,
    /// The authored byte range `before.text` had selected, which the dictation
    /// replaces — collapsed when it was a caret. Ordered, unlike the snapshot's
    /// anchor/focus pair, because it is used to slice.
    start: usize,
    end: usize,
    /// The last provisional transcript. Kept so an ordinary local event can
    /// settle the composition without the caller having to supply it again.
    text: String,
}

impl Composition {
    /// The whole line as it stands with `provisional` in place of the authored
    /// selection. Built from `before` every time rather than patched, so ten
    /// revisions cannot drift a byte away from one.
    fn line_with(&self, provisional: &str) -> String {
        let mut s = String::with_capacity(
            self.before.text.len() - (self.end - self.start) + provisional.len(),
        );
        s.push_str(&self.before.text[..self.start]);
        s.push_str(provisional);
        s.push_str(&self.before.text[self.end..]);
        s
    }
}

// ------------------------------------------------------------------ field

/// One line of editable plain text, with its own undo and its own view onto
/// itself.
pub struct TextField {
    editor: PlainEditor<Brush>,
    history: History,
    /// The open dictation, if there is one. `None` for the whole life of a
    /// field nobody has spoken into, which is every field today.
    composition: Option<Composition>,
    /// How far this field is scrolled along its one line, in logical pixels.
    ///
    /// Per field, because which part of a line is showing is a property of the
    /// line. [`prepare`] moves it; drawing and hit-testing both read it, which
    /// is what keeps a click landing on the character it looks like.
    ///
    /// [`prepare`]: Self::prepare
    scroll: f32,
}

impl TextField {
    pub fn new(engine: &mut TextEngine, style: koru_draw::theme::TextStyle) -> Self {
        let size = style.metrics.size;
        let mut editor = PlainEditor::<Brush>::new(size);
        // **No wrap width.** A field is one line however long its contents
        // are, and a URL is the case that proves it: given a width, parley
        // wrapped a long one onto a second line that drew through the box and
        // over the hint below. Unwrapped, the box becomes a window onto the
        // line and `prepare` scrolls it to follow the caret.
        editor.set_width(None);
        editor
            .edit_styles()
            .insert(parley::style::StyleProperty::Brush(Brush(
                koru_draw::theme::p().text,
            )));
        editor
            .edit_styles()
            .insert(parley::style::StyleProperty::FontFamily(
                parley::style::FontFamily::Source(std::borrow::Cow::Borrowed(
                    koru_draw::theme::ui_stack(),
                )),
            ));
        // A prompt field is chrome, so its weight comes from the role's
        // `TextStyle` (the caller passes `role::ui()` or `role::body()`). It
        // uses `PlainEditor` rather than a `RangedBuilder`, so it sets the
        // weight through the same style map instead of `push_axes`; `opsz` is
        // left at the default here, which is right for a one-line field.
        editor
            .edit_styles()
            .insert(parley::style::StyleProperty::FontWeight(
                parley::style::FontWeight::new(style.weight.value()),
            ));
        editor.refresh_layout(&mut engine.font_cx, &mut engine.layout_cx);
        Self {
            editor,
            history: History::default(),
            composition: None,
            scroll: 0.0,
        }
    }

    /// A field already holding `text`, with the caret at the end of it.
    pub fn with_text(
        engine: &mut TextEngine,
        style: koru_draw::theme::TextStyle,
        text: &str,
    ) -> Self {
        let mut field = Self::new(engine, style);
        field.set_text(engine, text);
        field
    }

    /// Replace the contents, caret at the end.
    ///
    /// **Clears the history**: text a prompt opened with is not something
    /// Ctrl+Z should be able to erase, the same rule `editor::prepare` applies
    /// to a workspace's first line.
    pub fn set_text(&mut self, engine: &mut TextEngine, text: &str) {
        // A wholesale replacement is not an interruption to settle: the value
        // being dictated into is going away, so the composition goes with it
        // and records nothing — the same reasoning that clears the history.
        self.composition = None;
        self.editor.set_text(text);
        self.editor
            .driver(&mut engine.font_cx, &mut engine.layout_cx)
            .move_to_text_end();
        self.history = History::default();
        self.scroll = 0.0;
    }

    /// The contents, trimmed — what every caller actually wants.
    ///
    /// **Authored**: never the provisional text of an open dictation. What the
    /// field *contains* is what the person authored, and a form must not act on
    /// half a sentence somebody is still saying.
    pub fn value(&self) -> String {
        self.raw_text().trim().to_string()
    }

    /// The contents, untrimmed. **Authored** — see [`value`](Self::value).
    pub fn raw_text(&self) -> &str {
        match &self.composition {
            Some(c) => &c.before.text,
            None => self.editor.raw_text(),
        }
    }

    /// Whether the field holds no **authored** text.
    ///
    /// A field whose only content is a provisional transcript answers `true`,
    /// which is right — nothing has been authored — and is the reason
    /// [`shows_placeholder`](Self::shows_placeholder) exists. Asking this
    /// directly to decide whether to draw a placeholder paints it under the
    /// dictated words.
    pub fn is_empty(&self) -> bool {
        self.raw_text().is_empty()
    }

    /// The caret's byte offset.
    ///
    /// **On screen**, not authored: while a dictation is open this indexes the
    /// line the person can see, provisional run included, because every caller
    /// of it is placing something against what is drawn.
    pub fn caret(&self) -> usize {
        self.editor.raw_selection().focus().index()
    }

    /// What is selected, or `None` when the caret is collapsed.
    ///
    /// **Authored**: while a dictation is open this is the selection the
    /// dictation is replacing, not the provisional run the editor has
    /// highlighted nothing of.
    pub fn selected_text(&self) -> Option<&str> {
        match &self.composition {
            Some(c) => (c.start < c.end).then(|| &c.before.text[c.start..c.end]),
            None => self.editor.selected_text(),
        }
    }

    /// The line **as drawn**, provisional run included.
    ///
    /// Almost nothing wants this: [`raw_text`](Self::raw_text) is what the
    /// field *contains* and is what a form, a filter or a save must read. This
    /// is for the two callers that are painting rather than acting — a masked
    /// field counting bullets, and anything measuring the line — and putting
    /// its answer into stored value is the leak the whole composition design
    /// exists to prevent.
    pub fn visible_text(&self) -> &str {
        self.editor.raw_text()
    }

    /// Whether there is a selection **on screen** to paint or to navigate out
    /// of. Not the same question as [`selected_text`](Self::selected_text),
    /// which is about stored value.
    pub fn has_visible_selection(&self) -> bool {
        self.editor.selected_text().is_some()
    }

    /// Whether an unmodified Right press has nowhere farther to move, asked of
    /// the line **on screen** — a caret sitting before a provisional run is not
    /// at the end of anything.
    pub fn caret_at_end(&self) -> bool {
        self.editor.selected_text().is_none()
            && self.editor.raw_selection().focus().index() == self.editor.raw_text().len()
    }

    /// **The one condition a placeholder may be drawn under.**
    ///
    /// The named trap of the composition design: an authored-empty field
    /// holding a provisional transcript is `is_empty() == true`, so a host that
    /// asks that alone draws its ghost text *underneath* the words being
    /// dictated. One function rather than a rule everybody has to remember, for
    /// the same reason `scene::floating_panel` is one function.
    pub fn shows_placeholder(&self) -> bool {
        self.is_empty() && self.composition.is_none()
    }

    pub fn select_all(&mut self, engine: &mut TextEngine) {
        self.settle(engine);
        self.history.checkpoint();
        self.editor
            .driver(&mut engine.font_cx, &mut engine.layout_cx)
            .select_all();
    }

    /// Select one exact UTF-8 byte range, clamped to valid character
    /// boundaries. Used when a non-row text surface (for example a window
    /// title rename field) takes over an existing authored selection.
    pub fn select_range(&mut self, engine: &mut TextEngine, range: std::ops::Range<usize>) {
        self.settle(engine);
        let text = self.editor.raw_text();
        let mut start = range.start.min(text.len());
        let mut end = range.end.min(text.len());
        while start > 0 && !text.is_char_boundary(start) {
            start -= 1;
        }
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        self.history.checkpoint();
        self.editor
            .driver(&mut engine.font_cx, &mut engine.layout_cx)
            .select_byte_range(start, end);
    }

    /// Collapse an authored selection at its active end without moving the
    /// caret somewhere unrelated. A field calls this when it loses focus so a
    /// stale selection is not still painted when another control owns input.
    pub fn collapse_selection(&mut self, engine: &mut TextEngine) {
        self.settle(engine);
        let caret = self.caret();
        self.history.checkpoint();
        self.editor
            .driver(&mut engine.font_cx, &mut engine.layout_cx)
            .select_byte_range(caret, caret);
    }

    pub fn move_to_end(&mut self, engine: &mut TextEngine) {
        self.settle(engine);
        self.history.checkpoint();
        self.editor
            .driver(&mut engine.font_cx, &mut engine.layout_cx)
            .move_to_text_end();
    }

    fn insert_text(&mut self, engine: &mut TextEngine, text: &str) {
        if text.is_empty() {
            return;
        }
        self.settle(engine);
        self.history.begin(&self.editor, Edit::Other);
        self.editor
            .driver(&mut engine.font_cx, &mut engine.layout_cx)
            .insert_or_replace_selection(text);
        self.history.checkpoint();
    }

    // ------------------------------------------------------------- mouse
    //
    // Every one takes a **text-local** x: only the caller knows the panel
    // geometry, and it has already taken the field rect and the scroll off.
    // The y is dropped throughout — there is one line, so a gesture that
    // wanders above or below the box should keep working along it.

    pub fn click(&mut self, engine: &mut TextEngine, tx: f32) {
        self.settle(engine);
        self.history.checkpoint();
        self.editor
            .driver(&mut engine.font_cx, &mut engine.layout_cx)
            .move_to_point(tx, 0.0);
    }

    /// Extend the selection to where the pointer has been dragged.
    pub fn drag_to(&mut self, engine: &mut TextEngine, tx: f32) {
        self.settle(engine);
        self.history.checkpoint();
        self.editor
            .driver(&mut engine.font_cx, &mut engine.layout_cx)
            .extend_selection_to_point(tx, 0.0);
    }

    pub fn select_word_at(&mut self, engine: &mut TextEngine, tx: f32) {
        self.settle(engine);
        self.history.checkpoint();
        self.editor
            .driver(&mut engine.font_cx, &mut engine.layout_cx)
            .select_word_at_point(tx, 0.0);
    }

    // ------------------------------------------------------------ drawing

    /// Lay the line out, move the window so the caret is inside it, and hand
    /// back what drawing needs.
    ///
    /// The window moves **only as far as it must**, which is what stops the
    /// text lurching about while the caret is in the middle of a long URL. An
    /// unfocused field is shown from its start: there is no caret to follow,
    /// and the beginning of a URL is the part worth seeing.
    ///
    /// Returns the editor and the offset together so the caller does not have
    /// to borrow `self` twice.
    pub fn prepare(
        &mut self,
        engine: &mut TextEngine,
        window: f32,
        focused: bool,
    ) -> (&PlainEditor<Brush>, f32) {
        self.editor
            .refresh_layout(&mut engine.font_cx, &mut engine.layout_cx);

        if focused {
            let text_w = self.editor.try_layout().map(|l| l.width()).unwrap_or(0.0);
            if let Some(bb) = self.editor.cursor_geometry(koru_draw::theme::caret_w()) {
                // Left of the window, come back to the caret; right of it, put
                // the caret at the far edge.
                let cx = bb.x0 as f32;
                self.scroll = self.scroll.min(cx).max(cx - window);
            }
            // And never past the end, so a field with room to spare shows its
            // text rather than a blank with the words off to the left.
            self.scroll = self.scroll.clamp(0.0, (text_w - window).max(0.0));
        } else {
            self.scroll = 0.0;
        }
        (&self.editor, self.scroll)
    }

    /// Where this field's window currently starts, for turning a click into a
    /// text-local x.
    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    // --------------------------------------------------------------- keys

    /// Drive the field with one key press.
    ///
    /// Vertical arrows are deliberately *not* handled: in a one-line field they
    /// mean "move between fields", which only the panel knows how to do.
    pub fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Typed {
        // Any ordinary local event settles an open dictation before it is
        // handled — including Enter, which a panel reads `value()` after. See
        // `settle`.
        self.settle(engine);
        let named = match key {
            Key::Named(n) => Some(*n),
            _ => None,
        };
        // Ctrl+I arrives as `NamedKey::Tab` on Windows, so it lands here and
        // the panel does nothing with it. That is the right outcome — plain
        // text has no italic — but it is only right because this list matches
        // on the key whatever the modifiers are.
        if matches!(
            named,
            Some(NamedKey::Enter) | Some(NamedKey::Tab) | Some(NamedKey::Escape)
        ) || matches!(named, Some(NamedKey::ArrowUp) | Some(NamedKey::ArrowDown))
        {
            return Typed::Passed;
        }

        // Cmd on macOS is what Ctrl is everywhere else, and `Editing::key`
        // already tests both. Testing only `control_key` here is why Cmd+S
        // typed an `s` into a document's name.
        let chord = koru_keys::is_chord(mods);
        let shift = mods.shift_key();

        match named {
            Some(NamedKey::Backspace) => {
                let opened = self.history.begin(&self.editor, Edit::Deleting);
                let mut d = self
                    .editor
                    .driver(&mut engine.font_cx, &mut engine.layout_cx);
                if chord {
                    d.backdelete_word()
                } else {
                    d.backdelete()
                }
                self.history.drop_if_unchanged(opened, &self.editor);
            }
            Some(NamedKey::Delete) => {
                let opened = self.history.begin(&self.editor, Edit::Deleting);
                let mut d = self
                    .editor
                    .driver(&mut engine.font_cx, &mut engine.layout_cx);
                if chord {
                    d.delete_word()
                } else {
                    d.delete()
                }
                self.history.drop_if_unchanged(opened, &self.editor);
            }

            // Motion never edits, so it closes the step instead of opening one.
            Some(NamedKey::ArrowLeft) => {
                self.history.checkpoint();
                let mut d = self
                    .editor
                    .driver(&mut engine.font_cx, &mut engine.layout_cx);
                match (chord, shift) {
                    (true, true) => d.select_word_left(),
                    (true, false) => d.move_word_left(),
                    (false, true) => d.select_left(),
                    (false, false) => d.move_left(),
                }
            }
            Some(NamedKey::ArrowRight) => {
                self.history.checkpoint();
                let mut d = self
                    .editor
                    .driver(&mut engine.font_cx, &mut engine.layout_cx);
                match (chord, shift) {
                    (true, true) => d.select_word_right(),
                    (true, false) => d.move_word_right(),
                    (false, true) => d.select_right(),
                    (false, false) => d.move_right(),
                }
            }
            // Line and text are the same thing in a field with one unwrapped
            // line. Both are bound because fingers do not know that.
            Some(NamedKey::Home) => {
                self.history.checkpoint();
                let mut d = self
                    .editor
                    .driver(&mut engine.font_cx, &mut engine.layout_cx);
                match (chord, shift) {
                    (true, true) => d.select_to_text_start(),
                    (true, false) => d.move_to_text_start(),
                    (false, true) => d.select_to_line_start(),
                    (false, false) => d.move_to_line_start(),
                }
            }
            Some(NamedKey::End) => {
                self.history.checkpoint();
                let mut d = self
                    .editor
                    .driver(&mut engine.font_cx, &mut engine.layout_cx);
                match (chord, shift) {
                    (true, true) => d.select_to_text_end(),
                    (true, false) => d.move_to_text_end(),
                    (false, true) => d.select_to_line_end(),
                    (false, false) => d.move_to_line_end(),
                }
            }

            // Guarded rather than reordered: Ctrl+Space must fall through to
            // the chord arm and be swallowed there. `keymap::DEFAULT`
            // deliberately binds nothing to it, and a field that typed a space
            // anyway would disagree with the document.
            Some(NamedKey::Space) if !chord => {
                self.history.begin(&self.editor, Edit::Typing);
                self.editor
                    .driver(&mut engine.font_cx, &mut engine.layout_cx)
                    .insert_or_replace_selection(" ");
                // A word boundary closes the step, the same way `type_text`
                // checkpoints the document on whitespace.
                self.history.checkpoint();
            }

            _ if chord => self.chord(engine, key, text, shift),

            _ => {
                if let Some(t) = text {
                    // Control codes arrive as `text` on some platforms; typing
                    // one would put an unprintable character in a title.
                    if !t.chars().any(|c| c.is_control()) {
                        self.history.begin(&self.editor, Edit::Typing);
                        self.editor
                            .driver(&mut engine.font_cx, &mut engine.layout_cx)
                            .insert_or_replace_selection(t);
                        if t.chars().any(char::is_whitespace) {
                            self.history.checkpoint();
                        }
                    }
                }
            }
        }
        Typed::Handled
    }

    /// The chords a field claims. Everything else with Ctrl held is swallowed
    /// rather than typed, or Ctrl+S would put an `s` in the box.
    fn chord(&mut self, engine: &mut TextEngine, key: &Key, text: Option<&str>, shift: bool) {
        match crate::keys::ctrl_letter(key, text) {
            Some('a') => self.select_all(engine),
            // A URL is the thing people paste into a prompt, and without this
            // the field ate Ctrl+V and did nothing.
            Some('v') => {
                if let Some(pasted) = crate::clipboard::one_line() {
                    self.history.begin(&self.editor, Edit::Other);
                    self.editor
                        .driver(&mut engine.font_cx, &mut engine.layout_cx)
                        .insert_or_replace_selection(&pasted);
                    self.history.checkpoint();
                }
            }
            Some('c') => self.copy(),
            Some('x') => self.cut(engine),
            // Undo is the field's own, and stops here: reaching the document
            // would move or delete the row this panel is anchored to.
            Some('z') if shift => {
                self.history.redo(&mut self.editor, engine);
            }
            Some('z') => {
                self.history.undo(&mut self.editor, engine);
            }
            _ => {}
        }
    }

    /// Copy the selection, or the whole field when nothing is selected — the
    /// same rule `clipboard::what_to_copy` applies to an unselected row.
    fn copy(&self) {
        let taken = self
            .editor
            .selected_text()
            .unwrap_or(self.editor.raw_text());
        if !taken.is_empty() {
            crate::clipboard::put_plain(taken);
        }
    }

    fn cut(&mut self, engine: &mut TextEngine) {
        let collapsed = self.editor.selected_text().is_none();
        let taken = self
            .editor
            .selected_text()
            .unwrap_or(self.editor.raw_text())
            .to_owned();
        if taken.is_empty() {
            return;
        }
        crate::clipboard::put_plain(&taken);
        let opened = self.history.begin(&self.editor, Edit::Other);
        let mut d = self
            .editor
            .driver(&mut engine.font_cx, &mut engine.layout_cx);
        if collapsed {
            d.select_all();
        }
        d.delete_selection();
        self.history.drop_if_unchanged(opened, &self.editor);
        self.history.checkpoint();
    }

    // ---------------------------------------------------------- dictation
    //
    // The four verbs of `crate::dictation::DictationTarget`, as inherent
    // methods so the field can be driven without importing the trait, and
    // because `settle` below has to call one of them from inside the type.
    // The trait impl is below and delegates here, the same shape `Leaf` uses
    // for its two traits.

    /// Whether a dictation is open. Nothing else in the type is allowed to
    /// infer this from the text.
    pub fn is_composing(&self) -> bool {
        self.composition.is_some()
    }

    /// Open a dictation over the current selection, or at the caret when it is
    /// collapsed — speech replaces a selection exactly as typing does.
    ///
    /// Refuses (`false`) if one is already open: two compositions in one field
    /// would need two snapshots and there is only one thing to put back.
    pub fn begin_composition(&mut self, engine: &mut TextEngine) -> bool {
        if self.composition.is_some() {
            return false;
        }
        // Close whatever step was being collected, so the burst of typing
        // before the dictation and the dictation itself are two Ctrl+Zs.
        self.history.checkpoint();
        // So `composition_rect` can answer from the first partial onwards:
        // `try_layout` returns `None` while the layout is dirty, and nothing
        // else on the `&self` read path may refresh it.
        self.editor
            .refresh_layout(&mut engine.font_cx, &mut engine.layout_cx);
        let before = Snapshot::of(&self.editor);
        let (start, end) = (
            before.anchor.min(before.focus),
            before.anchor.max(before.focus),
        );
        self.composition = Some(Composition {
            before,
            start,
            end,
            text: String::new(),
        });
        true
    }

    /// Replace the provisional text. **Touches no history.**
    ///
    /// The line is rebuilt from the retained snapshot rather than patched, so
    /// the tenth revision is byte-for-byte what the first would have been had
    /// the recogniser said it straight away.
    pub fn update_composition(&mut self, engine: &mut TextEngine, text: &str) {
        let (line, caret) = {
            let Some(c) = self.composition.as_mut() else {
                return;
            };
            // A recogniser repeats itself; relaying the same line would cost a
            // parley layout for no visible change.
            if c.text == text {
                return;
            }
            c.text.clear();
            c.text.push_str(text);
            (c.line_with(text), c.start + text.len())
        };
        self.set_line(engine, &line, caret);
    }

    /// Make `text` real: one edit, one undo step, composition closed.
    ///
    /// The step recorded is the **retained** snapshot, not one taken here —
    /// which is the whole reason the snapshot is retained. Undo therefore
    /// restores the authored text *and* the selection that preceded the
    /// dictation, including when the dictation opened over a selection.
    ///
    /// A dictation that produced nothing records nothing: an undo step that
    /// changes no text reads to a user as undo being broken, which is the rule
    /// `drop_if_unchanged` already applies to a backspace at offset zero.
    pub fn commit_composition(&mut self, engine: &mut TextEngine, text: &str) {
        let Some(c) = self.composition.take() else {
            return;
        };
        let caret = c.start + text.len();
        let line = c.line_with(text);
        self.set_line(engine, &line, caret);
        self.history.record(c.before);
        self.history.drop_if_unchanged(true, &self.editor);
    }

    /// Throw the dictation away: no edit, no undo entry, no trace.
    ///
    /// **An interruption discards** — losing focus, closing the window,
    /// sleeping. Half a sentence landing in something you were not looking at
    /// is worse than losing it.
    pub fn cancel_composition(&mut self, engine: &mut TextEngine) {
        let Some(c) = self.composition.take() else {
            return;
        };
        restore(&c.before, &mut self.editor, engine);
    }

    /// Where the provisional run is, in **text-local** coordinates: the same
    /// space [`click`](Self::click) takes, measured along the laid-out line
    /// from its start, before this field's horizontal scroll and before
    /// whatever box a host has drawn around it. A host converts with
    /// `rect.x + PAD - field.scroll()`.
    ///
    /// A field owns no rectangle — that is [`TextInput`](crate::TextInput)'s
    /// job, and even there the rect arrives per call — so window coordinates
    /// are not a thing this type could answer honestly.
    pub fn composition_rect(&self) -> Option<koru_draw::geometry::Rect> {
        let c = self.composition.as_ref()?;
        let layout = self.editor.try_layout()?;
        let (start, end) = (c.start, c.start + c.text.len());
        let cursor = |at: usize, affinity| parley::Cursor::from_byte_index(layout, at, affinity);
        if start == end {
            // Nothing has been said yet. A zero-width rect at the insertion
            // point is still a place a listening indicator can be hung, and is
            // honest about there being no run to underline. Not
            // `PlainEditor::cursor_geometry`, which answers `None` whenever the
            // caret is hidden — an unfocused field would then report nothing at
            // all while visibly holding a composition.
            let bb = cursor(start, parley::Affinity::Downstream).geometry(layout, 0.0);
            return Some(koru_draw::geometry::Rect::new(
                bb.x0 as f32,
                bb.y0 as f32,
                0.0,
                (bb.y1 - bb.y0) as f32,
            ));
        }
        // A run can be more than one rectangle — a bidi line puts an Arabic
        // insertion into a Latin sentence in two visual pieces — so the answer
        // is their union rather than the first of them.
        let selection = parley::Selection::new(
            cursor(start, parley::Affinity::Downstream),
            cursor(end, parley::Affinity::Upstream),
        );
        let mut bounds: Option<(f64, f64, f64, f64)> = None;
        selection.geometry_with(layout, |bb, _| {
            bounds = Some(match bounds {
                None => (bb.x0, bb.y0, bb.x1, bb.y1),
                Some((x0, y0, x1, y1)) => {
                    (x0.min(bb.x0), y0.min(bb.y0), x1.max(bb.x1), y1.max(bb.y1))
                }
            });
        });
        bounds.map(|(x0, y0, x1, y1)| {
            koru_draw::geometry::Rect::new(x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32)
        })
    }

    /// End an open dictation because an ordinary local event arrived — a
    /// click, a drag, a keystroke, a selection change.
    ///
    /// **Commits rather than discards, and is never split.** A caret dropped
    /// inside a provisional run would be thrown away by the next revision,
    /// which rebuilds the line; and a person who has just dictated a sentence
    /// and then clicked has not asked to lose it. This is the field's own rule
    /// and the reason every mutating entry point calls it first. The
    /// controller above may pre-empt it — an event arriving while the worker is
    /// still finalising cancels instead — but by then there is no open
    /// composition left here to settle.
    /// The provisional run in the visible line, for the drawing path.
    fn composition_span(&self) -> Option<std::ops::Range<usize>> {
        let c = self.composition.as_ref()?;
        Some(c.start..c.start + c.text.len())
    }

    fn settle(&mut self, engine: &mut TextEngine) {
        if let Some(text) = self.composition.as_ref().map(|c| c.text.clone()) {
            self.commit_composition(engine, &text);
        }
    }

    /// Replace the whole line and collapse the caret at `caret`.
    ///
    /// **The two calls must stay adjacent**, for exactly the reason [`restore`]
    /// gives: `set_text` leaves the selection pointing into the old layout and
    /// parley offers no `set_selection`, so nothing may read it in between.
    fn set_line(&mut self, engine: &mut TextEngine, line: &str, caret: usize) {
        self.editor.set_text(line);
        self.editor
            .driver(&mut engine.font_cx, &mut engine.layout_cx)
            .select_byte_range(caret, caret);
    }
}

impl crate::dictation::DictationTarget for TextField {
    type Cx<'a> = &'a mut TextEngine;

    fn begin_composition(&mut self, cx: Self::Cx<'_>) -> bool {
        TextField::begin_composition(self, cx)
    }

    fn update_composition(&mut self, cx: Self::Cx<'_>, text: &str) {
        TextField::update_composition(self, cx, text);
    }

    fn commit_composition(&mut self, cx: Self::Cx<'_>, text: &str) {
        TextField::commit_composition(self, cx, text);
    }

    fn cancel_composition(&mut self, cx: Self::Cx<'_>) {
        TextField::cancel_composition(self, cx);
    }

    fn composition_rect(&self) -> Option<koru_draw::geometry::Rect> {
        TextField::composition_rect(self)
    }
}

/// A wrapped plain-text composer built from the same editing engine as
/// [`TextField`]. It adds vertical geometry and preserves line breaks on paste.
///
/// Enter inserts a newline. Callers that want Enter to send (the AI composer)
/// intercept it before calling [`key`](Self::key).
pub struct TextArea {
    field: TextField,
    style: koru_draw::theme::TextStyle,
    dragging: bool,
    scroll_y: f32,
}

impl TextArea {
    pub fn new(engine: &mut TextEngine, style: koru_draw::theme::TextStyle) -> Self {
        Self {
            field: TextField::new(engine, style),
            style,
            dragging: false,
            scroll_y: 0.0,
        }
    }

    pub fn raw_text(&self) -> &str {
        self.field.raw_text()
    }

    /// Whether a provisional dictation is currently visible in the composer.
    pub fn is_composing(&self) -> bool {
        self.field.is_composing()
    }

    /// Open a composition over the current selection, or at the caret.
    pub fn begin_composition(&mut self, engine: &mut TextEngine) -> bool {
        self.field.begin_composition(engine)
    }

    /// Replace the provisional transcript without changing authored text.
    pub fn update_composition(&mut self, engine: &mut TextEngine, text: &str) {
        self.field.update_composition(engine, text);
    }

    /// Make the final transcript one authored edit and close the composition.
    pub fn commit_composition(&mut self, engine: &mut TextEngine, text: &str) {
        self.field.commit_composition(engine, text);
    }

    /// Discard the provisional transcript and restore the original selection.
    pub fn cancel_composition(&mut self, engine: &mut TextEngine) {
        self.field.cancel_composition(engine);
    }

    /// The provisional run in the area's own text-layout coordinate space.
    pub fn composition_rect(&self) -> Option<koru_draw::geometry::Rect> {
        self.field.composition_rect()
    }

    pub fn value(&self) -> String {
        self.field.value()
    }

    /// Replace the composer contents while preserving the caller's ownership
    /// of the text. Used when a process-wide typography choice changes and the
    /// editor must be rebuilt with the new family and metrics.
    pub fn set_text(&mut self, engine: &mut TextEngine, text: &str) {
        self.field.set_text(engine, text);
        self.scroll_y = 0.0;
    }

    pub fn is_empty(&self) -> bool {
        self.field.is_empty()
    }

    pub fn clear(&mut self, engine: &mut TextEngine) {
        self.field.set_text(engine, "");
        self.scroll_y = 0.0;
    }

    pub fn insert_newline(&mut self, engine: &mut TextEngine) {
        self.field.insert_text(engine, "\n");
    }

    pub fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Typed {
        if koru_keys::is_chord(mods) && crate::keys::ctrl_letter(key, text) == Some('v') {
            if let Some(pasted) = crate::clipboard::plain() {
                self.field.insert_text(engine, &pasted);
            }
            return Typed::Handled;
        }
        if matches!(key, Key::Named(NamedKey::Enter)) && !koru_keys::is_chord(mods) {
            self.insert_newline(engine);
            return Typed::Handled;
        }
        self.field.key(engine, key, text, mods)
    }

    pub fn desired_height(
        &mut self,
        engine: &mut TextEngine,
        width: f32,
        line_height: f32,
        min_lines: usize,
        max_lines: usize,
    ) -> f32 {
        self.prepare(engine, width);
        let height = self
            .field
            .editor
            .try_layout()
            .map_or(line_height, |layout| layout.height());
        let lines = (height / line_height).ceil().max(min_lines as f32);
        lines.min(max_lines as f32) * line_height + PAD * 2.0
    }

    pub fn click(
        &mut self,
        engine: &mut TextEngine,
        rect: koru_draw::geometry::Rect,
        x: f32,
        y: f32,
    ) -> bool {
        if !rect.contains(x, y) {
            return false;
        }
        self.prepare(engine, rect.w - PAD * 2.0);
        self.field.history.checkpoint();
        self.field
            .editor
            .driver(&mut engine.font_cx, &mut engine.layout_cx)
            .move_to_point(x - rect.x - PAD, y - rect.y - PAD + self.scroll_y);
        self.dragging = true;
        true
    }

    pub fn drag(
        &mut self,
        engine: &mut TextEngine,
        rect: koru_draw::geometry::Rect,
        x: f32,
        y: f32,
    ) -> bool {
        if !self.dragging {
            return false;
        }
        self.prepare(engine, rect.w - PAD * 2.0);
        self.field
            .editor
            .driver(&mut engine.font_cx, &mut engine.layout_cx)
            .extend_selection_to_point(x - rect.x - PAD, y - rect.y - PAD + self.scroll_y);
        true
    }

    pub fn release(&mut self) {
        self.dragging = false;
    }

    /// Scroll overflowing content. Draw clamps to the laid-out height.
    pub fn scroll_by(&mut self, dy: f32) {
        self.scroll_y = (self.scroll_y + dy).max(0.0);
    }

    fn prepare(&mut self, engine: &mut TextEngine, width: f32) {
        self.field.editor.set_width(Some(width.max(1.0)));
        self.field
            .editor
            .refresh_layout(&mut engine.font_cx, &mut engine.layout_cx);
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: koru_draw::geometry::Rect,
        placeholder: Option<&str>,
        focused: bool,
    ) {
        let composition = self.field.composition_span();
        let width = (rect.w - PAD * 2.0).max(1.0);
        self.prepare(engine, width);
        let layout_height = self
            .field
            .editor
            .try_layout()
            .map_or(0.0, |layout| layout.height());
        if focused {
            if let Some(cursor) = self.field.editor.cursor_geometry(theme::caret_w()) {
                self.scroll_y = self
                    .scroll_y
                    .min(cursor.y0 as f32)
                    .max(cursor.y1 as f32 - (rect.h - PAD * 2.0));
            }
        }
        self.scroll_y = self
            .scroll_y
            .clamp(0.0, (layout_height - (rect.h - PAD * 2.0)).max(0.0));
        let x = rect.x + PAD;
        let y = rect.y + PAD - self.scroll_y;
        let clip = Rect::new(
            rect.x as f64 + 1.0,
            rect.y as f64 + 1.0,
            (rect.x + rect.w) as f64 - 1.0,
            (rect.y + rect.h) as f64 - 1.0,
        );
        scene.push_clip_layer(Fill::NonZero, root, &clip);
        self.field.editor.selection_geometry_with(|box_, _| {
            let selection = Rect::new(
                box_.x0 + x as f64,
                box_.y0 + y as f64,
                box_.x1 + x as f64,
                box_.y1 + y as f64,
            );
            scene.fill(
                Fill::NonZero,
                root,
                theme::selection_color(),
                None,
                &selection,
            );
        });
        if let Some(layout) = self.field.editor.try_layout() {
            draw_layout(scene, root, layout, x, y, Some(theme::p().text));
            if let Some(span) = composition {
                draw_composition(scene, root, layout, span, x, y);
            }
        }
        if self.field.shows_placeholder()
            && let Some(placeholder) = placeholder
        {
            let ghost = engine.simple_layout_wrapped(placeholder, self.style, width);
            draw_layout(scene, root, &ghost, x, y, Some(theme::p().placeholder));
        }
        if focused
            && let (Some(cursor), Some(layout)) = (
                self.field.editor.cursor_geometry(theme::caret_w()),
                self.field.editor.try_layout(),
            )
        {
            let (baseline, ascent, descent) = TextEngine::line_ink(layout, cursor);
            let quad = theme::caret_quad(cursor, baseline, ascent, descent, x, y, 0.0);
            koru_draw::scene::fill_caret_quad(scene, root, theme::caret_color().color(), quad);
        }
        scene.pop_layer();
        let content_h = layout_height + PAD * 2.0;
        if let Some(geom) =
            crate::scrollbar::ScrollbarGeom::for_content(rect, content_h, self.scroll_y)
        {
            crate::scrollbar::draw(scene, root, geom, false);
        }
    }
}

impl crate::dictation::DictationTarget for TextArea {
    type Cx<'a> = &'a mut TextEngine;

    fn begin_composition(&mut self, cx: Self::Cx<'_>) -> bool {
        TextArea::begin_composition(self, cx)
    }

    fn update_composition(&mut self, cx: Self::Cx<'_>, text: &str) {
        TextArea::update_composition(self, cx, text);
    }

    fn commit_composition(&mut self, cx: Self::Cx<'_>, text: &str) {
        TextArea::commit_composition(self, cx, text);
    }

    fn cancel_composition(&mut self, cx: Self::Cx<'_>) {
        TextArea::cancel_composition(self, cx);
    }

    fn composition_rect(&self) -> Option<koru_draw::geometry::Rect> {
        TextArea::composition_rect(self)
    }
}

// ------------------------------------------------------------------ drawing
//
// Drawing lives beside the field rather than beside the panels that hold one:
// the selection, the scroll window and the caret are all facts about *this*
// type, and a caller that had to reproduce them would get the scroll wrong
// first. It draws only the text — no box, no border — so a surface frames it
// however it wants.

use koru_draw::scene::draw_layout;
use koru_draw::theme;
use parley::Layout;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Fill;

/// Paint a provisional run over an already-drawn plain-editor layout.
///
/// `PlainEditor` has no ranged-style API, so the same layout is clipped to the
/// run's geometry and drawn a second time with the provisional brush. The
/// underline is ours too: parley computes decoration geometry but never
/// rasterises it.
fn draw_composition(
    scene: &mut Scene,
    root: Affine,
    layout: &Layout<Brush>,
    span: std::ops::Range<usize>,
    x: f32,
    y: f32,
) {
    if span.is_empty() {
        return;
    }
    let selection = parley::Selection::new(
        parley::Cursor::from_byte_index(layout, span.start, parley::Affinity::Downstream),
        parley::Cursor::from_byte_index(layout, span.end, parley::Affinity::Upstream),
    );
    selection.geometry_with(layout, |bb, _| {
        let clip = Rect::new(
            bb.x0 + x as f64,
            bb.y0 + y as f64,
            bb.x1 + x as f64,
            bb.y1 + y as f64,
        );
        scene.push_clip_layer(Fill::NonZero, root, &clip);
        draw_layout(scene, root, layout, x, y, Some(theme::p().text_dim));
        scene.pop_layer();
        let underline = Rect::new(clip.x0, clip.y1 - 1.5, clip.x1, clip.y1 - 0.5);
        scene.fill(Fill::NonZero, root, theme::p().text_dim, None, &underline);
    });
}

/// The `y` that puts a laid-out line's **baseline** in the middle of a box.
///
/// Not `y + (h - layout.height()) * 0.5`. A layout's height is its line box,
/// and an emoji drags in a fallback face whose metrics inflate that box — so
/// centring it drops the thing you can actually see off the box's centre. The
/// baseline is what the eye measures from, so it is what gets centred.
///
/// Falls back to the line box when nothing has been laid out, which is the same
/// answer for a line with no fallback in it.
pub fn baseline_centred_y(layout: &Layout<Brush>, y: f32, h: f32) -> f32 {
    match layout.lines().next().map(|l| l.metrics().clone()) {
        Some(m) => y + (h + m.ascent - m.descent) * 0.5 - m.baseline,
        None => y + (h - layout.height()) * 0.5,
    }
}

/// Which caret a one-line field draws.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum FieldCaret {
    /// The person's own caret — the width and height they chose in Settings,
    /// the same bar a document draws.
    #[default]
    Preference,
    /// A form's caret: [`FORM_CARET_W`] wide, from the top of an `A` to the
    /// bottom of a `g` in the field's font. Measured from the **font**, not
    /// from what the field holds, so every field in a form draws the same bar
    /// — an empty one, a filled one, and a masked one whose text is bullets.
    Form,
    /// Focused, but in the off half of a blink: no bar this frame. The field
    /// still scrolls and selects as a focused one, so only the bar goes.
    Hidden,
}

/// The caret [`draw_field_with_caret`] paints, already resolved.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum CaretPaint {
    /// The person's own caret, as a document draws it.
    Preference,
    /// A [`FieldCaret::Form`] bar spanning `(top, bottom)`.
    Form((f32, f32)),
    /// No bar: the blink is off.
    Hidden,
}

/// Width of a [`FieldCaret::Form`] caret, in logical pixels. Thick enough to
/// find at a glance in a box with nothing else in it yet.
pub const FORM_CARET_W: f32 = 3.0;
/// How far a [`FieldCaret::Form`] caret reaches past the font's cap height and
/// descent at each end, so it reads as a caret and not as a letter.
pub const FORM_CARET_OVERHANG: f32 = 1.0;

/// The `(top, bottom)` of a [`FieldCaret::Form`] caret in a box of height `h`
/// at `y`: the font's cap height above the baseline and its full descent below,
/// with the baseline where [`baseline_centred_y`] puts a field's text.
pub fn form_caret_span(
    engine: &mut TextEngine,
    style: koru_draw::theme::TextStyle,
    y: f32,
    h: f32,
) -> (f32, f32) {
    let probe = engine.simple_layout("Ag", style);
    let top = baseline_centred_y(&probe, y, h);
    let Some(line) = probe.lines().next() else {
        return (y + h * 0.25, y + h * 0.75);
    };
    let m = line.metrics();
    let baseline = top + m.baseline;
    let (cap, descent) = line.runs().next().map_or((m.ascent, m.descent), |run| {
        let rm = run.metrics();
        (
            rm.cap_height.filter(|c| *c > 0.5).unwrap_or(rm.ascent),
            rm.descent,
        )
    });
    (
        (baseline - cap).round() - FORM_CARET_OVERHANG,
        (baseline + descent).round() + FORM_CARET_OVERHANG,
    )
}

/// Fill a [`FieldCaret::Form`] caret whose left edge is at `x`, centred on it
/// so the thicker bar sits between two letters rather than on the next one.
pub fn fill_form_caret(scene: &mut Scene, root: Affine, x: f32, (top, bottom): (f32, f32)) {
    koru_draw::scene::fill(
        scene,
        root,
        theme::caret_color().color(),
        &koru_draw::scene::rect(
            x - FORM_CARET_W * 0.5,
            top,
            FORM_CARET_W,
            (bottom - top).max(1.0),
        ),
    );
}

/// Draw one line of prompt: its selection, text, ghost and caret — clipped to
/// the box, and scrolled along so the caret is always inside it.
///
/// Fields are laid out **unwrapped**, so a long URL is one line that runs past
/// the right-hand edge rather than a second line drawn through whatever is
/// below. Clipping is what makes that safe; `TextField::prepare` is what makes
/// it usable, and owns the scrolling — where the window sits is a property of
/// the line, not of this frame.
///
/// Public because a [`TextField`] is not only ever inside an overlay: the
/// container puts one over a switcher row to rename a workspace in place. It
/// draws only the text — no box, no border — so a caller can frame it however
/// the surface it sits on wants.
pub fn draw_field(
    scene: &mut Scene,
    root: Affine,
    field: &mut TextField,
    engine: &mut TextEngine,
    ghost: Option<&Layout<Brush>>,
    rect: (f32, f32, f32, f32),
    focused: bool,
) {
    draw_field_with_caret(
        scene,
        root,
        field,
        engine,
        ghost,
        rect,
        focused,
        CaretPaint::Preference,
    );
}

/// [`draw_field`] with a [`FieldCaret::Form`] caret spanning `form_caret`
/// (from [`form_caret_span`]) in place of the person's own.
#[allow(clippy::too_many_arguments)]
pub fn draw_field_with_caret(
    scene: &mut Scene,
    root: Affine,
    field: &mut TextField,
    engine: &mut TextEngine,
    ghost: Option<&Layout<Brush>>,
    (fx, fy, fw, fh): (f32, f32, f32, f32),
    focused: bool,
    caret: CaretPaint,
) {
    let composition = field.composition_span();
    let window = (fw - PAD * 2.0).max(1.0);
    let (input, scroll) = field.prepare(engine, window, focused);
    // **Centred on the baseline, not on the line box.** Centring the box was
    // right until a field held an emoji: that glyph comes from a fallback face
    // with its own metrics, the line box grows to hold it, and centring *that*
    // pushed the Latin text off its own centre — text that visibly sat too high
    // the moment you typed one.
    //
    // The baseline is what the eye reads a line's position from, so it is what
    // gets placed: half the ink height above the box's middle, then back up by
    // however far the baseline sits inside the layout. Falls back to the line
    // box when a line has not been laid out yet, which is the same answer for
    // text with no fallback in it.
    let tx = fx + PAD;
    let ty = input
        .try_layout()
        .map_or(fy, |l| baseline_centred_y(l, fy, fh));
    let cursor = input.cursor_geometry(theme::caret_w());
    let dx = tx - scroll;

    // Inside the border, so the stroke stays crisp against what it clips.
    let inner = Rect::new(
        fx as f64 + 1.0,
        fy as f64 + 1.0,
        (fx + fw) as f64 - 1.0,
        (fy + fh) as f64 - 1.0,
    );
    // `push_clip_layer` rather than a `push_layer` with a blend mode: vello 0.8
    // has a dedicated call for a pure clip, and `peniko::Mix` has no `Clip`
    // variant to reach for.
    scene.push_clip_layer(Fill::NonZero, root, &inner);

    input.selection_geometry_with(|bb, _| {
        let r = Rect::new(
            bb.x0 + dx as f64,
            bb.y0 + ty as f64,
            bb.x1 + dx as f64,
            bb.y1 + ty as f64,
        );
        scene.fill(Fill::NonZero, root, theme::selection_color(), None, &r);
    });
    if let Some(text_layout) = input.try_layout() {
        draw_layout(scene, root, text_layout, dx, ty, Some(theme::p().text));
        if let Some(span) = composition {
            draw_composition(scene, root, text_layout, span, dx, ty);
        }
    }
    if let Some(ghost) = ghost {
        draw_layout(scene, root, ghost, dx, ty, Some(theme::p().placeholder));
    }
    if focused {
        if let (Some(bb), CaretPaint::Form(span)) = (cursor, caret) {
            fill_form_caret(scene, root, bb.x0 as f32 + dx, span);
        } else if caret == CaretPaint::Hidden {
        } else if let (Some(bb), Some(layout)) = (cursor, input.try_layout()) {
            let (baseline, ascent, descent) = TextEngine::line_ink(layout, bb);
            let quad = theme::caret_quad(bb, baseline, ascent, descent, dx, ty, 0.0);
            koru_draw::scene::fill_caret_quad(scene, root, theme::caret_color().color(), quad);
        }
    }

    scene.pop_layer();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(text: &str) -> (TextField, TextEngine) {
        let mut engine = TextEngine::new();
        let f = TextField::with_text(&mut engine, koru_draw::theme::role::body(), text);
        (f, engine)
    }

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }

    use koru_keys::{NONE, PRIMARY};

    /// A form caret runs from the top of a capital to the bottom of a
    /// descender, inside the box, whatever the field holds.
    #[test]
    fn a_form_caret_spans_cap_height_to_descent_inside_the_box() {
        let mut engine = TextEngine::new();
        let style = koru_draw::theme::role::row();
        let (top, bottom) = form_caret_span(&mut engine, style, 100.0, 40.0);
        let probe = engine.simple_layout("Ag", style);
        let baseline = baseline_centred_y(&probe, 100.0, 40.0)
            + probe.lines().next().unwrap().metrics().baseline;
        assert!(top > 100.0 && bottom < 140.0, "{top}..{bottom}");
        assert!(
            top < baseline && baseline < bottom,
            "{top} {baseline} {bottom}"
        );
        // Taller than the cap height alone: the descent is below the baseline.
        assert!(bottom - baseline >= 2.0, "{baseline}..{bottom}");
        assert!(bottom - top > style.metrics.size * 0.8, "{top}..{bottom}");
    }

    fn press(f: &mut TextField, e: &mut TextEngine, k: NamedKey, mods: ModifiersState) {
        f.key(e, &Key::Named(k), None, mods);
    }

    fn typed(f: &mut TextField, e: &mut TextEngine, s: &str) {
        for c in s.chars() {
            let t = c.to_string();
            if c == ' ' {
                press(f, e, NamedKey::Space, NONE);
            } else {
                f.key(e, &ch(&t), Some(&t), NONE);
            }
        }
    }

    #[test]
    fn typing_appends_and_backspace_removes() {
        let (mut f, mut e) = field("spec");
        assert_eq!(f.key(&mut e, &ch("s"), Some("s"), NONE), Typed::Handled);
        assert_eq!(f.value(), "specs");

        press(&mut f, &mut e, NamedKey::Backspace, NONE);
        assert_eq!(f.value(), "spec");
    }

    /// Enter, Tab and Escape belong to the panel — a field that swallowed them
    /// would make the prompt impossible to submit or leave.
    #[test]
    fn the_panels_own_keys_pass_through() {
        let (mut f, mut e) = field("spec");
        for k in [NamedKey::Enter, NamedKey::Tab, NamedKey::Escape] {
            assert_eq!(
                f.key(&mut e, &Key::Named(k), None, NONE),
                Typed::Passed,
                "{k:?} is the panel's"
            );
        }
        assert_eq!(f.value(), "spec", "and none of them typed anything");
    }

    /// Vertical arrows move between fields, which one field cannot do.
    #[test]
    fn vertical_arrows_pass_through() {
        let (mut f, mut e) = field("spec");
        for k in [NamedKey::ArrowUp, NamedKey::ArrowDown] {
            assert_eq!(f.key(&mut e, &Key::Named(k), None, NONE), Typed::Passed);
        }
    }

    /// A chord must never leave its letter in the box.
    #[test]
    fn a_ctrl_chord_types_nothing() {
        let (mut f, mut e) = field("spec");
        f.key(&mut e, &ch("s"), Some("s"), PRIMARY);
        assert_eq!(f.value(), "spec");
    }

    /// The same for Cmd on macOS. Testing only `control_key` is why Cmd+S used
    /// to put an `s` in the box.
    #[test]
    fn a_cmd_chord_types_nothing() {
        let (mut f, mut e) = field("spec");
        f.key(&mut e, &ch("s"), Some("s"), ModifiersState::SUPER);
        assert_eq!(f.value(), "spec");
    }

    /// Space is a character key, so a chord on it competes with typing one.
    /// The document binds nothing to Ctrl+Space and the field must agree.
    #[test]
    fn ctrl_space_types_nothing() {
        let (mut f, mut e) = field("spec");
        press(&mut f, &mut e, NamedKey::Space, PRIMARY);
        assert_eq!(f.raw_text(), "spec");
    }

    /// Windows delivers some keys as control codes in `text`; typing one would
    /// put an unprintable character in a document's name.
    #[test]
    fn control_codes_are_not_typed() {
        let (mut f, mut e) = field("spec");
        f.key(&mut e, &ch("\u{b}"), Some("\u{b}"), NONE);
        assert_eq!(f.value(), "spec");
    }

    #[test]
    fn value_is_trimmed() {
        let (f, _) = field("  spec.md  ");
        assert_eq!(f.value(), "spec.md");
    }

    // -------------------------------------------------------- word motion

    /// parley stops at word *starts* going left and word *ends* going right,
    /// which is the convention a paragraph already uses — both go through the
    /// same primitive, so the field and the document agree.
    #[test]
    fn ctrl_arrow_moves_by_word() {
        let (mut f, mut e) = field("one two three");
        assert_eq!(f.caret(), 13);

        let mut left = Vec::new();
        for _ in 0..3 {
            press(&mut f, &mut e, NamedKey::ArrowLeft, PRIMARY);
            left.push(f.caret());
        }
        assert_eq!(left, vec![8, 4, 0], "the start of each word");

        let mut right = Vec::new();
        for _ in 0..3 {
            press(&mut f, &mut e, NamedKey::ArrowRight, PRIMARY);
            right.push(f.caret());
        }
        assert_eq!(right, vec![3, 7, 13], "the end of each word");
    }

    #[test]
    fn ctrl_shift_arrow_selects_by_word() {
        let (mut f, mut e) = field("one two three");
        press(
            &mut f,
            &mut e,
            NamedKey::ArrowLeft,
            PRIMARY | ModifiersState::SHIFT,
        );
        assert_eq!(f.selected_text(), Some("three"));
    }

    #[test]
    fn shift_home_and_shift_end_select_to_the_ends() {
        let (mut f, mut e) = field("one two");
        press(&mut f, &mut e, NamedKey::Home, ModifiersState::SHIFT);
        assert_eq!(f.selected_text(), Some("one two"));

        press(&mut f, &mut e, NamedKey::Home, NONE);
        press(&mut f, &mut e, NamedKey::End, ModifiersState::SHIFT);
        assert_eq!(f.selected_text(), Some("one two"));
    }

    /// Aliases of Home/End in a one-line field. Pinned so they are not deleted
    /// as duplicates — a user's fingers do not know there is only one line.
    #[test]
    fn ctrl_home_and_ctrl_end_reach_the_ends() {
        let (mut f, mut e) = field("one two");
        press(&mut f, &mut e, NamedKey::Home, PRIMARY);
        assert_eq!(f.caret(), 0);
        press(&mut f, &mut e, NamedKey::End, PRIMARY);
        assert_eq!(f.caret(), 7);
    }

    // --------------------------------------------------------------- undo

    #[test]
    fn a_burst_of_typing_is_one_undo() {
        let (mut f, mut e) = field("");
        typed(&mut f, &mut e, "spec");
        assert_eq!(f.raw_text(), "spec");

        f.key(&mut e, &ch("z"), None, PRIMARY);
        assert_eq!(f.raw_text(), "", "the whole burst, not one letter");
    }

    #[test]
    fn a_space_ends_the_undo_step() {
        let (mut f, mut e) = field("");
        typed(&mut f, &mut e, "spec md");

        f.key(&mut e, &ch("z"), None, PRIMARY);
        assert_eq!(f.raw_text(), "spec ");
        f.key(&mut e, &ch("z"), None, PRIMARY);
        assert_eq!(f.raw_text(), "");
    }

    /// Undoing "select all, type x" has to put the text back **and** leave it
    /// selected, or the next Ctrl+Z lands somewhere the user never was.
    #[test]
    fn undo_restores_what_was_selected() {
        let (mut f, mut e) = field("spec.md");
        f.key(&mut e, &ch("a"), None, PRIMARY);
        typed(&mut f, &mut e, "x");
        assert_eq!(f.raw_text(), "x");

        f.key(&mut e, &ch("z"), None, PRIMARY);

        assert_eq!(f.raw_text(), "spec.md");
        assert_eq!(f.selected_text(), Some("spec.md"));
    }

    /// A selection dragged backwards points backwards, and `select_byte_range`
    /// takes anchor then focus — which is why a snapshot stores both.
    #[test]
    fn undo_restores_a_backwards_selection() {
        let (mut f, mut e) = field("spec.md");
        for _ in 0..3 {
            press(&mut f, &mut e, NamedKey::ArrowLeft, ModifiersState::SHIFT);
        }
        typed(&mut f, &mut e, "x");
        f.key(&mut e, &ch("z"), None, PRIMARY);

        assert_eq!(f.raw_text(), "spec.md");
        let sel = f.editor.raw_selection();
        assert!(
            sel.anchor().index() > sel.focus().index(),
            "still pointing backwards"
        );
    }

    #[test]
    fn redo_puts_it_back() {
        let (mut f, mut e) = field("");
        typed(&mut f, &mut e, "spec");
        f.key(&mut e, &ch("z"), None, PRIMARY);
        assert_eq!(f.raw_text(), "");

        f.key(&mut e, &ch("z"), None, PRIMARY | ModifiersState::SHIFT);
        assert_eq!(f.raw_text(), "spec");
    }

    #[test]
    fn a_new_edit_clears_the_redo_stack() {
        let (mut f, mut e) = field("");
        typed(&mut f, &mut e, "spec");
        f.key(&mut e, &ch("z"), None, PRIMARY);
        typed(&mut f, &mut e, "other");

        f.key(&mut e, &ch("z"), None, PRIMARY | ModifiersState::SHIFT);
        assert_eq!(f.raw_text(), "other", "there is nothing to redo");
    }

    /// Deleting a word you have just typed and undoing gives the word back —
    /// it must not merge the two into one step and empty the box.
    #[test]
    fn typing_and_deleting_are_separate_steps() {
        let (mut f, mut e) = field("");
        typed(&mut f, &mut e, "spec");
        press(&mut f, &mut e, NamedKey::Backspace, NONE);
        press(&mut f, &mut e, NamedKey::Backspace, NONE);
        assert_eq!(f.raw_text(), "sp");

        f.key(&mut e, &ch("z"), None, PRIMARY);
        assert_eq!(f.raw_text(), "spec", "the run of backspaces, in one step");
        f.key(&mut e, &ch("z"), None, PRIMARY);
        assert_eq!(f.raw_text(), "");
    }

    #[test]
    fn a_caret_move_ends_the_undo_step() {
        let (mut f, mut e) = field("");
        typed(&mut f, &mut e, "ab");
        press(&mut f, &mut e, NamedKey::ArrowLeft, NONE);
        typed(&mut f, &mut e, "c");

        f.key(&mut e, &ch("z"), None, PRIMARY);
        assert_eq!(f.raw_text(), "ab");
    }

    /// Backspace at offset zero changes nothing, and must not leave a step that
    /// does nothing when it is reached — which reads as undo being broken.
    #[test]
    fn backspace_at_the_start_leaves_no_empty_undo_step() {
        let (mut f, mut e) = field("");
        typed(&mut f, &mut e, "spec");
        press(&mut f, &mut e, NamedKey::Home, NONE);
        press(&mut f, &mut e, NamedKey::Backspace, NONE);
        assert_eq!(f.raw_text(), "spec");

        f.key(&mut e, &ch("z"), None, PRIMARY);
        assert_eq!(f.raw_text(), "", "the first press undid the typing");
    }

    /// Text a prompt opened with is not something Ctrl+Z should erase — and it
    /// still reports `Handled`, so it cannot fall through to the document.
    #[test]
    fn undo_stops_at_the_text_the_prompt_opened_with() {
        let (mut f, mut e) = field("spec.md");
        assert_eq!(f.key(&mut e, &ch("z"), None, PRIMARY), Typed::Handled);
        assert_eq!(f.raw_text(), "spec.md");
    }

    #[test]
    fn the_history_is_capped() {
        let (mut f, mut e) = field("");
        // Each pair is one step: a letter, then a caret move to close it.
        for _ in 0..DEPTH + 10 {
            typed(&mut f, &mut e, "x");
            press(&mut f, &mut e, NamedKey::Home, NONE);
            press(&mut f, &mut e, NamedKey::End, NONE);
        }
        for _ in 0..DEPTH + 20 {
            f.key(&mut e, &ch("z"), None, PRIMARY);
        }
        assert!(
            !f.raw_text().is_empty(),
            "the oldest steps fell off the end"
        );
    }

    // ---------------------------------------------------------- clipboard

    #[test]
    fn ctrl_c_then_ctrl_v_round_trips_the_selection() {
        let (mut f, mut e) = field("spec.md");
        f.key(&mut e, &ch("a"), None, PRIMARY);
        f.key(&mut e, &ch("c"), None, PRIMARY);

        press(&mut f, &mut e, NamedKey::End, NONE);
        f.key(&mut e, &ch("v"), None, PRIMARY);

        assert_eq!(f.raw_text(), "spec.mdspec.md");
    }

    /// Nothing selected copies the whole field, the way Ctrl+C on an
    /// unselected row copies the whole task.
    #[test]
    fn ctrl_c_with_nothing_selected_copies_the_whole_field() {
        let (mut f, mut e) = field("spec.md");
        f.key(&mut e, &ch("c"), None, PRIMARY);

        let (mut other, mut e2) = field("");
        other.key(&mut e2, &ch("v"), None, PRIMARY);
        assert_eq!(other.raw_text(), "spec.md");
    }

    #[test]
    fn ctrl_x_cuts_and_ctrl_z_brings_it_back() {
        let (mut f, mut e) = field("spec.md");
        f.key(&mut e, &ch("x"), None, PRIMARY);
        assert_eq!(f.raw_text(), "");

        f.key(&mut e, &ch("z"), None, PRIMARY);
        assert_eq!(f.raw_text(), "spec.md");
    }

    // ---------------------------------------------------------- dictation
    //
    // One sentence, eight languages, and the mixed line last — the same
    // fixture shape the store's suites use, so "it round-trips" means one
    // thing across the tree.

    const SPOKEN: [&str; 9] = [
        "Draft the release notes",
        "Redactar las notas de la version",
        "Rascunhar as notas da versao",
        "Die Versionshinweise entwerfen",
        "Rediger les notes de version",
        "\u{62a}\u{62d}\u{631}\u{64a}\u{631} \u{645}\u{644}\u{627}\u{62d}\u{638}\u{627}\u{62a} \u{627}\u{644}\u{625}\u{635}\u{62f}\u{627}\u{631}",
        "\u{8d77}\u{8349}\u{53d1}\u{884c}\u{8bf4}\u{660e}",
        "\u{30ea}\u{30ea}\u{30fc}\u{30b9}\u{30ce}\u{30fc}\u{30c8}\u{3092}\u{4e0b}\u{66f8}\u{304d}\u{3059}\u{308b}",
        // Mixed LTR/RTL on one line, which is where a byte range and a visual
        // run stop being the same shape.
        "Meeting \u{645}\u{639} \u{627}\u{644}\u{641}\u{631}\u{64a}\u{642} at 14:00",
    ];

    /// Half of a sentence, on a character boundary — a recogniser revises whole
    /// words, and slicing bytes would panic on seven of the nine fixtures.
    fn half(s: &str) -> String {
        s.chars().take(s.chars().count() / 2).collect()
    }

    /// **Ten revisions are not ten edits.** A recogniser rewrites itself
    /// several times a second; if each one opened a step, one Ctrl+Z would
    /// walk back through a sentence nobody typed.
    #[test]
    fn partials_leave_the_history_untouched() {
        let (mut f, mut e) = field("spec");
        let steps = f.history.past.len();
        assert!(f.begin_composition(&mut e));
        for i in 1..=10 {
            f.update_composition(&mut e, &"word ".repeat(i));
        }
        assert_eq!(f.history.past.len(), steps, "no step was opened");
        assert!(f.history.future.is_empty());
    }

    /// A second dictation into one field has nowhere to keep its snapshot, so
    /// it is refused rather than silently replacing the first.
    #[test]
    fn a_second_begin_is_refused() {
        let (mut f, mut e) = field("spec");
        assert!(f.begin_composition(&mut e));
        assert!(!f.begin_composition(&mut e));
    }

    /// One dictation is one undo step, and undo lands on the text that
    /// preceded it rather than on the last thing the recogniser guessed.
    #[test]
    fn a_commit_is_exactly_one_undo_step() {
        let (mut f, mut e) = field("spec");
        f.begin_composition(&mut e);
        for partial in ["hel", "hell", "hello"] {
            f.update_composition(&mut e, partial);
        }
        f.commit_composition(&mut e, "hello");

        assert_eq!(f.raw_text(), "spechello");
        assert_eq!(f.history.past.len(), 1, "one step, not one per partial");

        f.key(&mut e, &ch("z"), None, PRIMARY);
        assert_eq!(f.raw_text(), "spec");
    }

    /// Dictation replaces a selection exactly as typing does — and undo has to
    /// put the *selection* back too, or the next Ctrl+Z lands somewhere the
    /// person was never standing. This is the case the retained snapshot
    /// exists for: a snapshot taken at commit time would hold the last partial.
    #[test]
    fn a_commit_over_a_selection_undoes_to_the_selection() {
        let (mut f, mut e) = field("spec.md");
        f.select_all(&mut e);
        f.begin_composition(&mut e);
        f.update_composition(&mut e, "note");
        assert_eq!(f.raw_text(), "spec.md", "still authored");
        assert_eq!(f.selected_text(), Some("spec.md"), "and still selected");

        f.commit_composition(&mut e, "notes");
        assert_eq!(f.raw_text(), "notes");

        f.key(&mut e, &ch("z"), None, PRIMARY);
        assert_eq!(f.raw_text(), "spec.md");
        assert_eq!(f.selected_text(), Some("spec.md"));
    }

    /// Cancel is not an edit that undoes itself — it leaves no trace at all,
    /// down to a selection that was dragged backwards still pointing backwards.
    #[test]
    fn cancel_leaves_the_field_byte_identical() {
        let (mut f, mut e) = field("spec.md");
        for _ in 0..3 {
            press(&mut f, &mut e, NamedKey::ArrowLeft, ModifiersState::SHIFT);
        }
        let text = f.raw_text().to_owned();
        let selected = f.selected_text().map(str::to_owned);
        let caret = f.caret();
        let steps = f.history.past.len();

        f.begin_composition(&mut e);
        f.update_composition(&mut e, "x");
        f.update_composition(&mut e, "x y");
        f.cancel_composition(&mut e);

        assert_eq!(f.raw_text(), text);
        assert_eq!(f.selected_text().map(str::to_owned), selected);
        assert_eq!(f.caret(), caret);
        assert_eq!(f.history.past.len(), steps, "and recorded nothing");
        assert!(!f.is_composing());
    }

    /// A dictation in the middle of a line has to land where typing would —
    /// same text, same caret. Compared against the typing path rather than
    /// against arithmetic written out again.
    #[test]
    fn a_commit_mid_text_matches_what_typing_would_do() {
        let (mut spoken, mut e1) = field("one three");
        spoken.select_range(&mut e1, 4..4);
        spoken.begin_composition(&mut e1);
        spoken.update_composition(&mut e1, "tw");
        spoken.commit_composition(&mut e1, "two ");

        let (mut typing, mut e2) = field("one three");
        typing.select_range(&mut e2, 4..4);
        typed(&mut typing, &mut e2, "two ");

        assert_eq!(spoken.raw_text(), typing.raw_text());
        assert_eq!(spoken.caret(), typing.caret());
        assert_eq!(spoken.raw_text(), "one two three");
    }

    /// **The trap this design names.** An authored-empty field holding a
    /// provisional transcript *is* empty, so a host asking `is_empty` draws its
    /// placeholder under the dictated words. Every query in ADR-272's table is
    /// checked here in that one state, because it is the state where the two
    /// sides disagree about everything.
    #[test]
    fn an_authored_empty_field_holding_a_partial_draws_no_placeholder() {
        let (mut f, mut e) = field("");
        assert!(f.shows_placeholder(), "empty and quiet: the ghost belongs");

        f.begin_composition(&mut e);
        f.update_composition(&mut e, "hello world");

        // Authored side.
        assert!(f.is_empty(), "nothing has been authored");
        assert_eq!(f.raw_text(), "");
        assert_eq!(f.value(), "");
        assert_eq!(f.selected_text(), None);
        // ...and therefore:
        assert!(
            !f.shows_placeholder(),
            "the ghost would be drawn under the dictation"
        );

        // On-screen side.
        assert_eq!(f.visible_text(), "hello world");
        assert_eq!(f.caret(), "hello world".len());
        assert!(f.caret_at_end());
        assert!(!f.has_visible_selection());
        let (input, _) = f.prepare(&mut e, 200.0, true);
        assert_eq!(
            input.raw_text(),
            "hello world",
            "prepare lays out what is on screen"
        );
        assert!(f.composition_rect().is_some(), "and it has a place");
    }

    /// A dictation is never left half-open across an ordinary event: a click
    /// settles it first, so the caret cannot land inside a run the next
    /// revision is about to rebuild.
    #[test]
    fn a_click_settles_an_open_composition() {
        let (mut f, mut e) = field("");
        f.begin_composition(&mut e);
        f.update_composition(&mut e, "spec");
        f.click(&mut e, 0.0);

        assert!(!f.is_composing());
        assert_eq!(f.raw_text(), "spec");
        assert_eq!(f.history.past.len(), 1, "one step, not a split one");

        f.key(&mut e, &ch("z"), None, PRIMARY);
        assert_eq!(f.raw_text(), "");
    }

    /// A dictation nobody spoke into is not an edit, and must not leave a step
    /// that does nothing when it is reached — the rule a backspace at offset
    /// zero already follows.
    #[test]
    fn a_silent_dictation_records_nothing() {
        let (mut f, mut e) = field("spec");
        f.begin_composition(&mut e);
        f.commit_composition(&mut e, "");
        assert_eq!(f.raw_text(), "spec");
        assert!(f.history.past.is_empty());
    }

    #[test]
    fn there_is_no_composition_rect_when_nothing_is_being_said() {
        let (mut f, mut e) = field("spec");
        assert!(f.composition_rect().is_none());
        f.begin_composition(&mut e);
        let empty = f
            .composition_rect()
            .expect("an open composition has a place");
        assert_eq!(empty.w, 0.0, "nothing said yet");
        f.update_composition(&mut e, "hello");
        let run = f.composition_rect().expect("and now a run");
        assert!(run.w > 0.0);
    }

    /// Eight languages and a mixed LTR/RTL line, byte-exact through commit,
    /// through undo, and through cancel. Length limits and slicing count
    /// characters here for the same reason they do in the store.
    #[test]
    fn every_language_round_trips_byte_exactly() {
        for spoken in SPOKEN {
            let (mut f, mut e) = field("");
            f.begin_composition(&mut e);
            f.update_composition(&mut e, &half(spoken));
            assert_eq!(f.raw_text(), "", "{spoken}: a partial is not authored");
            f.commit_composition(&mut e, spoken);
            assert_eq!(f.raw_text(), spoken, "{spoken}: committed");
            assert_eq!(f.caret(), spoken.len(), "{spoken}: caret past it");

            f.key(&mut e, &ch("z"), None, PRIMARY);
            assert_eq!(f.raw_text(), "", "{spoken}: one Ctrl+Z takes it back");
        }
    }

    #[test]
    fn every_language_cancels_byte_exactly() {
        for spoken in SPOKEN {
            let (mut f, mut e) = field(spoken);
            f.begin_composition(&mut e);
            f.update_composition(&mut e, &half(spoken));
            f.cancel_composition(&mut e);
            assert_eq!(f.raw_text(), spoken, "{spoken}: untouched");
            assert_eq!(f.caret(), spoken.len());
            assert!(f.history.past.is_empty());
        }
    }

    #[test]
    fn a_text_area_inserts_a_newline_on_enter() {
        let mut engine = TextEngine::new();
        let mut area = TextArea::new(&mut engine, theme::role::ui());
        area.set_text(&mut engine, "hello");
        assert_eq!(
            area.key(&mut engine, &Key::Named(NamedKey::Enter), None, NONE),
            Typed::Handled
        );
        assert_eq!(area.raw_text(), "hello\n");
    }

    /// The wrapped composer is not a second editing implementation. It must
    /// preserve line breaks, keep partials provisional, and commit once through
    /// the same field history used by keyboard input.
    #[test]
    fn a_text_area_composes_a_multiline_transcript_as_one_edit() {
        let mut engine = TextEngine::new();
        let mut area = TextArea::new(&mut engine, theme::role::ui());
        assert!(area.begin_composition(&mut engine));
        area.update_composition(&mut engine, "First line");
        area.update_composition(&mut engine, "First line\nSecond line");

        assert_eq!(area.raw_text(), "", "partials are not authored");
        assert_eq!(area.field.visible_text(), "First line\nSecond line");
        assert_eq!(area.field.caret(), "First line\nSecond line".len());
        assert_eq!(
            area.field.composition_span(),
            Some(0.."First line\nSecond line".len()),
            "drawing receives the exact provisional run to dim and underline"
        );

        area.commit_composition(&mut engine, "First line\nSecond line.");
        assert_eq!(area.raw_text(), "First line\nSecond line.");
        assert_eq!(area.field.history.past.len(), 1);
    }
}
