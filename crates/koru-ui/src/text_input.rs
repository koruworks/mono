// SPDX-License-Identifier: MPL-2.0

//! A one-line text box you can put anywhere and style how you like.
//!
//! [`TextField`] is the *editing* — selection, undo, paste, `Ctrl+Backspace`
//! deleting a word, and the platform key behaviour that took two bugs to get
//! right. It deliberately owns no rectangle, because the slash menu's field, a
//! rename prompt's and a table's search box are all in different places.
//!
//! What each caller then wrote for itself was the **same three lines**:
//!
//! ```ignore
//! let tx = x - rect.x - text_field::PAD + field.scroll();
//! field.click(engine, tx);
//! ```
//!
//! …plus a box to draw behind it. Four copies of that arithmetic had appeared,
//! and it is exactly the kind that goes wrong quietly: get it wrong and the
//! caret lands a few pixels from where you clicked, which nobody reports as a
//! bug because it reads as imprecision.
//!
//! So this is the pairing of a field with a rect. It adds no editing behaviour
//! of its own and forwards every key to `TextField` — a box that behaved
//! differently from the slash menu's would be the thing worth avoiding.
//!
//! ## Styling
//!
//! [`InputStyle`] is deliberately small and all-optional: a fill, a border, a
//! radius, a text style. A search box is a filled chip with no border; a rename
//! field over a row is a filled box *with* one; the slash menu's sits on a panel
//! that has already drawn its own. Anything a caller needs that is not here
//! belongs drawn by the caller around this, not as a fifth field.
//!
//! ## Why it is here rather than in `koru-editor`
//!
//! The move [`TextField`] already made, for the same reason and one step later:
//! the search overlay is a box you type into over a list of results, and it has
//! to be hostable by a process that links no node model, no workspace and no
//! syntax engine. `koru-editor` re-exports it under its old path, so no call
//! site changed ([ADR-105](../../../docs/decisions/ADR-101-125.md#adr-105-ipc--mechanism-is-pushed-down-and-domain-behaviour-stays-with-its-domain)).

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Color;
use winit::keyboard::{Key, ModifiersState};

use crate::text_field::{self, CaretPaint, FieldCaret, TextField, Typed, draw_field_with_caret};

/// How the box around the text looks. Everything is optional, because "no box
/// at all" is a real case — the slash menu's panel has already drawn one.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct InputStyle {
    pub fill: Option<Color>,
    pub border: Option<Color>,
    pub radius: f32,
    /// The text style. Kept here rather than on the field so one input can be
    /// restyled without being rebuilt.
    pub style: theme::TextStyle,
    /// Which caret the field draws while focused.
    pub caret: FieldCaret,
}

impl InputStyle {
    /// A filled chip with no border — a search box.
    pub fn search() -> Self {
        Self {
            fill: Some(theme::p().chip_bg),
            border: None,
            radius: 6.0,
            style: theme::role::row(),
            caret: FieldCaret::Preference,
        }
    }

    /// A filled box with a focus-coloured border — a field standing in for a
    /// row that is being edited in place.
    pub fn inline() -> Self {
        Self {
            fill: Some(theme::p().bg),
            border: Some(theme::p().focus_ring),
            radius: 4.0,
            style: theme::role::row(),
            caret: FieldCaret::Preference,
        }
    }

    /// A form's field — a panel-coloured box whose border turns the accent
    /// while `focused`, with a [`FieldCaret::Form`] caret. What first run's
    /// fields are, so every one of them draws the same box and the same bar.
    pub fn form(focused: bool) -> Self {
        let p = theme::p();
        Self {
            fill: Some(p.panel_bg),
            border: Some(if focused { p.accent } else { p.control_border }),
            radius: 8.0,
            style: theme::role::row(),
            caret: FieldCaret::Form,
        }
    }

    /// The same, complaining. What an unacceptable value looks like — the box
    /// itself, so the message beside it is confirming rather than announcing.
    pub fn invalid(self) -> Self {
        Self {
            border: Some(theme::p().close_hover),
            ..self
        }
    }

    /// The same field in the off half of its caret's blink when `on` is
    /// false. The caller owns the clock; the field only draws the phase.
    pub fn caret_shown(self, on: bool) -> Self {
        if on {
            self
        } else {
            Self {
                caret: FieldCaret::Hidden,
                ..self
            }
        }
    }

    /// No box: the surface underneath has drawn one.
    pub fn bare(style: theme::TextStyle) -> Self {
        Self {
            fill: None,
            border: None,
            radius: 0.0,
            style,
            caret: FieldCaret::Preference,
        }
    }
}

/// A control inside the input's right end. The input lays it out, draws it,
/// hit-tests it and performs what it does, so every field that wants one gets
/// the same one.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EndIcon {
    /// The eye: shows or hides a [secret](TextInput::secret) field's text.
    /// Wears `ph-eye` while hidden and `ph-eye-slash` while showing.
    Reveal,
}

/// The end icon's button, and the glyph inside it.
const END_SIDE: f32 = 32.0;
const END_INSET: f32 = 6.0;
const END_GLYPH: f32 = 18.0;

pub struct TextInput {
    field: TextField,
    /// A drag that began inside the box stays this input's, even once the
    /// pointer has left it — the same capture rule every other field here has.
    dragging: bool,
    /// Drawn as one bullet per character unless revealed.
    secret: bool,
    revealed: bool,
    end_icon: Option<EndIcon>,
    /// The pointer is over the end icon / the keyboard is on it. Set by the
    /// caller, who owns hover and focus, before drawing.
    end_icon_hot: bool,
    end_icon_focused: bool,
}

impl TextInput {
    /// OS composition shares TextField's authored-value and undo contract.
    pub fn ime(&mut self, engine: &mut TextEngine, event: &winit::event::Ime) -> bool {
        self.field.ime(engine, event)
    }

    pub fn ime_cancel(&mut self, engine: &mut TextEngine) -> bool {
        self.field.ime_cancel(engine)
    }

    /// Candidate anchor for the same box passed to draw, in caller coordinates.
    pub fn ime_cursor_area(&self, rect: Rect) -> Option<Rect> {
        self.field.ime_cursor_area(rect)
    }

    pub fn new(engine: &mut TextEngine, style: theme::TextStyle) -> Self {
        Self::from_field(TextField::new(engine, style))
    }

    pub fn with_text(engine: &mut TextEngine, style: theme::TextStyle, text: &str) -> Self {
        Self::from_field(TextField::with_text(engine, style, text))
    }

    fn from_field(field: TextField) -> Self {
        Self {
            field,
            dragging: false,
            secret: false,
            revealed: false,
            end_icon: None,
            end_icon_hot: false,
            end_icon_focused: false,
        }
    }

    /// A credential field: [`draw`](Self::draw) masks it until revealed. The
    /// authored value never enters the scene while masked.
    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    /// Put `icon` inside the box's right end. The text scrolls and clips
    /// short of it.
    pub fn with_end_icon(mut self, icon: EndIcon) -> Self {
        self.end_icon = Some(icon);
        self
    }

    pub fn end_icon(&self) -> Option<EndIcon> {
        self.end_icon
    }

    /// Whether the text is drawn as bullets right now.
    pub fn masked(&self) -> bool {
        self.secret && !self.revealed
    }

    /// Whether a secret field is showing its text.
    pub fn revealed(&self) -> bool {
        self.revealed
    }

    /// Mask a revealed secret again — when the value it showed is cleared,
    /// say, so the next one does not start out on screen.
    pub fn conceal(&mut self) {
        self.revealed = false;
    }

    /// Where the end icon sits in a box at `r`, if there is one.
    pub fn end_icon_rect(&self, r: Rect) -> Option<Rect> {
        self.end_icon.map(|_| {
            Rect::new(
                r.x + r.w - END_INSET - END_SIDE,
                r.y + (r.h - END_SIDE) * 0.5,
                END_SIDE,
                END_SIDE,
            )
        })
    }

    /// Do what the end icon does, and say which it was. A press, Enter or a
    /// client's invoke all come here, so they cannot disagree.
    pub fn press_end_icon(&mut self) -> Option<EndIcon> {
        let icon = self.end_icon?;
        match icon {
            EndIcon::Reveal => self.revealed = !self.revealed,
        }
        Some(icon)
    }

    /// The caller's hover and keyboard focus on the end icon, for drawing.
    pub fn set_end_icon_state(&mut self, hot: bool, focused: bool) {
        self.end_icon_hot = hot;
        self.end_icon_focused = focused;
    }

    /// The room the text keeps clear at the right end.
    fn trailing(&self) -> f32 {
        if self.end_icon.is_some() {
            END_SIDE + END_INSET
        } else {
            0.0
        }
    }

    /// The text, trimmed — what a caller almost always wants.
    pub fn value(&self) -> String {
        self.field.value()
    }

    pub fn is_empty(&self) -> bool {
        self.field.is_empty()
    }

    /// Whether a pointer press is still extending this field's selection.
    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// Whether there is a selection on screen.
    ///
    /// Answers from what is drawn rather than from stored value, because every
    /// caller is either painting it or deciding a navigation against it
    /// (ADR-272).
    pub fn has_selection(&self) -> bool {
        self.field.has_visible_selection()
    }

    /// The authored contents, without the value-level trimming used by forms.
    /// Never the provisional text of an open dictation.
    pub fn raw_text(&self) -> &str {
        self.field.raw_text()
    }

    /// Whether an unmodified Right press has nowhere farther to move.
    ///
    /// Search uses this to retain Right-to-preview without stealing ordinary
    /// caret motion or Shift+Right selection from the shared field.
    pub fn caret_at_end(&self) -> bool {
        self.field.caret_at_end()
    }

    /// The caret's byte offset into [`raw_text`](Self::raw_text).
    ///
    /// Exposed for a caller that completes the *word under the caret* rather
    /// than the whole box — the search surface's `#tag` completion is the
    /// first, and it pairs this with [`select_range`](Self::select_range) to
    /// replace one token.
    pub fn caret(&self) -> usize {
        self.field.caret()
    }

    pub fn set_text(&mut self, engine: &mut TextEngine, text: &str) {
        self.field.set_text(engine, text);
    }

    /// Select everything — what a rename opens with, since renaming usually
    /// means replacing.
    pub fn select_all(&mut self, engine: &mut TextEngine) {
        self.field.select_all(engine);
    }

    pub fn select_range(&mut self, engine: &mut TextEngine, range: std::ops::Range<usize>) {
        self.field.select_range(engine, range);
    }

    /// Lose text focus: stop any pointer drag and collapse selection at the
    /// active caret end while retaining the field's authored value.
    pub fn blur(&mut self, engine: &mut TextEngine) {
        self.dragging = false;
        self.field.collapse_selection(engine);
    }

    /// Collapse any transient selection at the end of the remembered value.
    pub fn move_to_end(&mut self, engine: &mut TextEngine) {
        self.field.move_to_end(engine);
    }

    // ---------------------------------------------------------- dictation
    //
    // **Forwarded explicitly, one method at a time.** The field is private, so
    // an input does *not* inherit its methods — and a box that silently
    // refused dictation while the slash menu's took it would be exactly the
    // kind of divergence this type exists to prevent.

    /// Whether a dictation is open in this box.
    pub fn is_composing(&self) -> bool {
        self.field.is_composing()
    }

    /// **The one condition a placeholder may be drawn under** — see
    /// [`TextField::shows_placeholder`](crate::TextField::shows_placeholder).
    /// [`draw`](Self::draw) and [`draw_masked`](Self::draw_masked) already ask
    /// it; this is for a caller drawing its own ghost beside the box.
    pub fn shows_placeholder(&self) -> bool {
        self.field.shows_placeholder()
    }

    pub fn begin_composition(&mut self, engine: &mut TextEngine) -> bool {
        self.field.begin_composition(engine)
    }

    pub fn update_composition(&mut self, engine: &mut TextEngine, text: &str) {
        self.field.update_composition(engine, text);
    }

    pub fn commit_composition(&mut self, engine: &mut TextEngine, text: &str) {
        self.field.commit_composition(engine, text);
    }

    pub fn cancel_composition(&mut self, engine: &mut TextEngine) {
        self.field.cancel_composition(engine);
    }

    /// Where the provisional run is, in the **field's** text-local space — the
    /// same space this type's `tx` converts a window x into. A caller holding
    /// the box's rect converts back with `r.x + text_field::PAD - scroll`.
    pub fn composition_rect(&self) -> Option<Rect> {
        self.field.composition_rect()
    }

    /// **The arithmetic this type exists for.** A window x, as an x in the
    /// field's own text — the box's origin and its horizontal scroll taken off.
    fn tx(&self, r: Rect, x: f32) -> f32 {
        x - r.x - text_field::PAD + self.field.scroll()
    }

    /// A press. Returns whether it landed in the box; a caller uses that to
    /// decide whether the press was theirs at all.
    pub fn click(&mut self, engine: &mut TextEngine, r: Rect, x: f32, y: f32) -> bool {
        if !r.contains(x, y) {
            return false;
        }
        // The end icon is pressed, not typed into: no caret move, no drag.
        if self
            .end_icon_rect(r)
            .is_some_and(|icon| icon.contains(x, y))
        {
            self.press_end_icon();
            return true;
        }
        self.dragging = true;
        let tx = self.tx(r, x);
        self.field.click(engine, tx);
        true
    }

    /// Carry a selection. **No hit test**: a drag that has left the box is still
    /// this input's until the button comes up.
    pub fn drag(&mut self, engine: &mut TextEngine, r: Rect, x: f32) -> bool {
        if !self.dragging {
            return false;
        }
        let tx = self.tx(r, x);
        self.field.drag_to(engine, tx);
        true
    }

    pub fn release(&mut self) {
        self.dragging = false;
    }

    pub fn double_click(&mut self, engine: &mut TextEngine, r: Rect, x: f32, y: f32) -> bool {
        if !r.contains(x, y) {
            return false;
        }
        let tx = self.tx(r, x);
        self.field.select_word_at(engine, tx);
        true
    }

    /// Every key goes to the field through the shared platform-key recovery.
    ///
    /// Including `Ctrl+Backspace`, `Ctrl+Z`, word-wise arrows and paste: this
    /// type adds no key behaviour of its own, so a box built with it behaves
    /// exactly like the slash menu's, which is the point.
    pub fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Typed {
        let recovered = crate::keys::recover_modified_editing_key(key, mods);
        self.field
            .key(engine, recovered.as_ref().unwrap_or(key), text, mods)
    }

    /// Draw the box and the text in it.
    ///
    /// `placeholder` shows only while the field is empty — it is a hint about
    /// what to type, so it has no business being there once you have.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        r: Rect,
        style: InputStyle,
        placeholder: Option<&str>,
        focused: bool,
    ) {
        if self.masked() {
            let _input = input_scope(root, r, focused, true);
            self.draw_masked(scene, root, engine, r, style, placeholder, focused);
            return;
        }
        let trailing = self.trailing();
        draw_text_input(
            scene,
            root,
            &mut self.field,
            engine,
            r,
            style,
            placeholder,
            focused,
            trailing,
        );
        self.draw_end_icon(scene, root, engine, r);
    }

    fn draw_end_icon(&self, scene: &mut Scene, root: Affine, engine: &mut TextEngine, r: Rect) {
        let (Some(icon), Some(at)) = (self.end_icon, self.end_icon_rect(r)) else {
            return;
        };
        let p = theme::p();
        let glyph = match icon {
            EndIcon::Reveal if self.revealed => koru_draw::icons::EYE_SLASH,
            EndIcon::Reveal => koru_draw::icons::EYE,
        };
        let _scope = koru_draw::inspect::scope(
            "KoruInputEndIcon",
            "component",
            root,
            scene::rect(at.x, at.y, at.w, at.h),
            |record| {
                record.property("icon", format!("{icon:?}"), "koru_ui::text_input::EndIcon");
                record.property("revealed", self.revealed, "input state");
            },
        );
        let shape = scene::rrect(at.x, at.y, at.w, at.h, 6.0);
        if self.end_icon_hot {
            scene::fill(scene, root, p.chip_bg, &shape);
        }
        if self.end_icon_focused {
            scene::stroke(scene, root, p.focus_ring, theme::BORDER_W, &shape);
        }
        let layout = engine.icon_layout(glyph, END_GLYPH);
        scene::draw_layout_first_line(
            scene,
            root,
            &layout,
            at.x + (at.w - layout.width()) * 0.5,
            at.y + (at.h - layout.height()) * 0.5,
            Some(if self.end_icon_hot {
                p.text
            } else {
                p.text_dim
            }),
        );
    }

    /// Draw a credential field without putting its authored value in the
    /// scene. The editor still owns the real text for selection, paste, and
    /// undo, but pixels expose only one bullet per Unicode scalar.
    pub fn draw_masked(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        r: Rect,
        style: InputStyle,
        placeholder: Option<&str>,
        focused: bool,
    ) {
        if let Some(fill) = style.fill {
            scene::fill(
                scene,
                root,
                fill,
                &scene::rrect(r.x, r.y, r.w, r.h, style.radius),
            );
        }
        if let Some(border) = style.border {
            scene::stroke(
                scene,
                root,
                border,
                theme::CONTROL_BORDER_W,
                &scene::rrect(r.x, r.y, r.w, r.h, style.radius),
            );
        }
        // One bullet per scalar **on screen**, so a dictation in progress is
        // as many bullets as it has letters; and the placeholder is gated on
        // `shows_placeholder`, or it would be drawn instead of them.
        let ghosting = self.field.shows_placeholder();
        let display = if ghosting {
            placeholder.unwrap_or_default().to_owned()
        } else {
            "\u{2022}".repeat(self.field.visible_text().chars().count())
        };
        let layout = engine.simple_layout(&display, style.style);
        let colour = if ghosting {
            theme::p().placeholder
        } else {
            theme::p().text
        };
        // Keep the end (and therefore the caret) visible for long secrets.
        // The authored value never enters the scene, so TextField's ordinary
        // horizontal scroll cannot be reused here.
        let right = r.x + (r.w - self.trailing()).max(0.0) - text_field::PAD;
        // Short of the edge by the caret's width, so a long secret's caret,
        // drawn just past its last bullet, is not clipped away.
        let x =
            (r.x + text_field::PAD).min(right - layout.width() - text_field::FORM_CARET_W - 2.0);
        let y = text_field::baseline_centred_y(&layout, r.y, r.h);
        let clip = vello::kurbo::Rect::new(
            (r.x + text_field::PAD) as f64,
            r.y as f64,
            right as f64,
            (r.y + r.h) as f64,
        );
        scene.push_clip_layer(vello::peniko::Fill::NonZero, root, &clip);
        scene::draw_layout_first_line(scene, root, &layout, x, y, Some(colour));
        // The caret the unmasked field would draw — measured from the line,
        // not from the box, or a masked field's bar is a different height
        // from the plain field above it.
        if focused {
            let caret_x = x + layout.width();
            match style.caret {
                FieldCaret::Hidden => {}
                FieldCaret::Form => text_field::fill_form_caret(
                    scene,
                    root,
                    // Clear of the last bullet: the bar is centred on `x`, and
                    // a bullet's ink runs right up to the layout's width.
                    if ghosting {
                        caret_x
                    } else {
                        caret_x + text_field::FORM_CARET_W * 0.5 + 1.0
                    },
                    text_field::form_caret_span(engine, style.style, r.y, r.h),
                ),
                FieldCaret::Preference => {
                    if let Some(line) = layout.lines().next() {
                        let m = line.metrics();
                        let bb = parley::BoundingBox::new(
                            layout.width() as f64,
                            m.block_min_coord as f64,
                            layout.width() as f64,
                            m.block_max_coord as f64,
                        );
                        let (baseline, ascent, descent) = TextEngine::line_ink(&layout, bb);
                        let quad = theme::caret_quad(bb, baseline, ascent, descent, x, y, 0.0);
                        scene::fill_caret_quad(scene, root, theme::caret_color().color(), quad);
                    }
                }
            }
        }
        scene.pop_layer();
        self.draw_end_icon(scene, root, engine, r);
    }
}

/// The inspector record every text input is drawn inside.
fn input_scope(root: Affine, r: Rect, focused: bool, masked: bool) -> koru_draw::inspect::Scope {
    koru_draw::inspect::scope(
        "KoruTextInput",
        "component",
        root,
        scene::rect(r.x, r.y, r.w, r.h),
        |record| {
            record.property(
                "implementation",
                "koru_ui::text_input",
                "shared implementation",
            );
            record.property("focused", focused, "caller state");
            record.property("masked", masked, "input state");
            record.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
        },
    )
}

/// Draw a text input — its box, text, placeholder and caret — around a
/// [`TextField`] that lives somewhere other than a [`TextInput`].
///
/// [`TextInput::draw`] draws through this, and so does a surface whose own
/// editing model owns a bare field (the emoji picker's search), so the two
/// cannot look different. `trailing` is room kept clear at the right end.
#[allow(clippy::too_many_arguments)]
#[track_caller]
pub fn draw_text_input(
    scene: &mut Scene,
    root: Affine,
    field: &mut TextField,
    engine: &mut TextEngine,
    r: Rect,
    style: InputStyle,
    placeholder: Option<&str>,
    focused: bool,
    trailing: f32,
) {
    let _input = input_scope(root, r, focused, false);
    if let Some(fill) = style.fill {
        scene::fill(
            scene,
            root,
            fill,
            &scene::rrect(r.x, r.y, r.w, r.h, style.radius),
        );
    }
    if let Some(border) = style.border {
        scene::stroke(
            scene,
            root,
            border,
            theme::CONTROL_BORDER_W,
            &scene::rrect(r.x, r.y, r.w, r.h, style.radius),
        );
    }
    // `shows_placeholder`, never `is_empty`: an authored-empty field
    // holding a provisional transcript is empty, and drawing the ghost
    // then paints it under the dictated words.
    let ghost = placeholder
        .filter(|_| field.shows_placeholder())
        .map(|g| engine.simple_layout(g, style.style));
    let caret = match style.caret {
        FieldCaret::Preference => CaretPaint::Preference,
        FieldCaret::Form => {
            CaretPaint::Form(text_field::form_caret_span(engine, style.style, r.y, r.h))
        }
        FieldCaret::Hidden => CaretPaint::Hidden,
    };
    draw_field_with_caret(
        scene,
        root,
        field,
        engine,
        ghost.as_ref(),
        (r.x, r.y, (r.w - trailing).max(0.0), r.h),
        focused,
        caret,
    );
}

impl crate::dictation::DictationTarget for TextInput {
    type Cx<'a> = &'a mut TextEngine;

    fn begin_composition(&mut self, cx: Self::Cx<'_>) -> bool {
        TextInput::begin_composition(self, cx)
    }

    fn update_composition(&mut self, cx: Self::Cx<'_>, text: &str) {
        TextInput::update_composition(self, cx, text);
    }

    fn commit_composition(&mut self, cx: Self::Cx<'_>, text: &str) {
        TextInput::commit_composition(self, cx, text);
    }

    fn cancel_composition(&mut self, cx: Self::Cx<'_>) {
        TextInput::cancel_composition(self, cx);
    }

    fn composition_rect(&self) -> Option<Rect> {
        TextInput::composition_rect(self)
    }
}

#[cfg(test)]
mod tests;
