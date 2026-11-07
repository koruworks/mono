// SPDX-License-Identifier: MPL-2.0

//! **KoruLabelChip — the one pill-shaped label.** Every chip a person reads
//! in the app is this component: an attribute a row wears (a tag, a due, a
//! priority, a colour swatch), a filter over a list (*Priority ≥ High*,
//! *Open ⌄*), and a status (*Updating…*, *Offline*).
//!
//! These were separate recipes — the attribute chip in `koru_draw::chip`, the
//! filter chip in `koru_ui::filter_chip`, the caption's status chip, and
//! pills drawn by hand on pages — and they had drifted: an attribute chip
//! reserved a remove lane on every surface, so on the Tasks page, where no
//! `×` is ever drawn, it read 6 px on the left and 21 px on the right. One
//! recipe is what keeps a chip a chip wherever it sits.
//!
//! ## The parts
//!
//! - **[`Size`]**: `Compact` (17 px, the meta role — a row's attributes) or
//!   `Regular` (the hint role's line plus air — filters and statuses).
//! - **[`Tone`]**: neutral, an accent token's tone, or the one inverted
//!   surface (a filter that is on, a status that is an instrument).
//! - **[`Lead`]**: nothing, a glyph (the queue a due falls in), or the quiet
//!   dot an ambient status wears.
//! - **[`Trail`]**: nothing, a caret (a selector), a clear `×` (a filter that
//!   is on), or a **remove** lane whose `×` is inked only while revealed.
//!
//! **The air is the same at both ends** unless the chip can be removed. A
//! chip reserves the remove lane only when it is the control that takes its
//! attribute off ([ADR-211]); a chip that is a reading — the Tasks page's —
//! carries no lane and no lopsided padding. The lane, when it is there, is
//! reserved whether or not its `×` is inked, so a hover never moves a chip.
//!
//! **Measuring and drawing are one arithmetic.** [`LabelChip::width`] is the
//! rectangle a caller lays out and hit-tests, and [`LabelChip::trail_rect`]
//! is where the mark is pressed; [`LabelChip::draw`] paints into exactly that.
//! A compact chip's label is shaped once into the engine's chip cache
//! (`TextEngine::attr_chips`), so the document's rows lay out nothing on a
//! frame that has drawn them before.
//!
//! **Not this component**: the settings choice chip (`koru_ui::chip`,
//! KoruChip — a squared-off segment), the shortcut chip (`key_chip`), the
//! numeric badge, the branch chip and the chip nav. Those are different
//! shapes for different jobs.
//!
//! [ADR-211]: ../../../docs/decisions/ADR-201-225.md#adr-211-chipclick--an-attribute-chip-is-the-control-for-its-attribute-and-a-tag-comes-off-at-the-chip

use koru_draw::chip::{
    CHIP_CLOSE_W, CHIP_H, CHIP_ICON_GAP, CHIP_PAD_X, ChipKind, SWATCH_D, chip_colors,
};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme};
use vello::Scene;
use vello::kurbo::{Affine, Circle};
use vello::peniko::{Color, Fill};

/// How big a chip is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Size {
    /// A row's attribute: [`CHIP_H`] high, in the meta role.
    #[default]
    Compact,
    /// A filter or a status: the hint role's line plus [`REGULAR_PAD_Y`]
    /// above and below.
    Regular,
}

/// Air either side of a regular chip's contents.
pub const REGULAR_PAD_X: f32 = 11.0;
/// Air above and below a regular chip's label.
pub const REGULAR_PAD_Y: f32 = 5.0;
/// Between a regular chip's lead or label and the next thing in it.
pub const REGULAR_GAP: f32 = 5.0;
/// The caret's and the clear `×`'s own box. Square, so the two centre in the
/// same place and a chip does not change width when it changes mark.
pub const MARK: f32 = 12.0;
/// The status dot's diameter.
pub const DOT_D: f32 = 5.0;
/// The `×`'s arm and stroke, one mark for clearing and for removing.
const X_ARM: f32 = 3.0;
const X_STROKE: f64 = 1.3;

impl Size {
    /// The chip's height.
    ///
    /// A regular chip's is derived from its role, so a density change moves
    /// the row rather than clipping the chip.
    pub fn height(self) -> f32 {
        match self {
            Self::Compact => CHIP_H,
            Self::Regular => theme::role::hint().metrics.line_height + REGULAR_PAD_Y * 2.0,
        }
    }

    fn pad_x(self) -> f32 {
        match self {
            Self::Compact => CHIP_PAD_X,
            Self::Regular => REGULAR_PAD_X,
        }
    }

    fn gap(self) -> f32 {
        match self {
            Self::Compact => CHIP_ICON_GAP,
            Self::Regular => REGULAR_GAP,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Regular => "regular",
        }
    }
}

/// What a chip is painted in.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Tone<'a> {
    /// `chip_bg` under `text_dim`: the family's quiet pair.
    #[default]
    Neutral,
    /// An accent token's tone (`theme::accent_tone`) — a tag's or a list's
    /// colour, or the warm token an overdue reading wears. A token this build
    /// does not know reads as [`Neutral`](Self::Neutral), so a peer's newer
    /// palette still renders legibly.
    Accent(&'a str),
    /// The one inverted surface, dark in both themes: a filter that is on, a
    /// status that is an instrument rather than part of the page.
    Inverted,
}

impl Tone<'_> {
    /// The fill and the ink.
    pub fn colors(self, p: &theme::Palette) -> (Color, Color) {
        match self {
            Self::Neutral => (p.chip_bg, p.text_dim),
            Self::Accent(token) => chip_colors(Some(token)),
            Self::Inverted => (p.bubble_bg, p.bubble_fg),
        }
    }

    fn name(self) -> String {
        match self {
            Self::Neutral => "neutral".into(),
            Self::Accent(token) => format!("accent {token}"),
            Self::Inverted => "inverted".into(),
        }
    }
}

/// What leads the label.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Lead {
    #[default]
    None,
    /// A glyph in the chip's ink — the queue a due falls in.
    Glyph(char),
    /// The quiet dot every ambient indicator in the window wears.
    Dot,
}

/// What trails the label.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Trail {
    #[default]
    None,
    /// A selector whose label is its current value: pressing it offers the
    /// others. Always inked.
    Caret,
    /// A filter that is switched on and comes off at the `×`. Always inked.
    Clear,
    /// The chip is the control that takes its attribute off. The lane is
    /// reserved always and its `×` inked only while [`State::reveal`] is on,
    /// so the pointer arriving moves nothing.
    Remove,
}

impl Trail {
    fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Caret => "caret",
            Self::Clear => "clear",
            Self::Remove => "remove",
        }
    }
}

/// The pointer's state over a chip.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// Tint the lozenge — a chip that is a control, under the pointer.
    pub hovered: bool,
    /// Ink a [`Trail::Remove`] lane's `×`.
    pub reveal: bool,
}

/// One chip: what it says and how it is dressed. Built per draw; a compact
/// chip's shaped label lives in the engine's cache, not here.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LabelChip<'a> {
    label: &'a str,
    size: Size,
    tone: Tone<'a>,
    lead: Lead,
    trail: Trail,
    struck: bool,
    swatch: bool,
}

impl<'a> LabelChip<'a> {
    /// A compact, neutral chip saying `label`.
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            size: Size::Compact,
            tone: Tone::Neutral,
            lead: Lead::None,
            trail: Trail::None,
            struck: false,
            swatch: false,
        }
    }

    /// A colour swatch: a bare dot in `token`'s swatch colour, no lozenge
    /// and no label. A swatch is compact; a lead or a strike changes nothing.
    pub fn swatch(token: Option<&'a str>) -> Self {
        Self {
            swatch: true,
            tone: token.map_or(Tone::Neutral, Tone::Accent),
            ..Self::new("")
        }
    }

    /// The attribute chip a node's `kind` earns: a swatch for a colour, else
    /// a compact chip in `accent`'s tone.
    pub fn attribute(kind: ChipKind, label: &'a str, accent: Option<&'a str>) -> Self {
        match kind {
            ChipKind::Swatch => Self::swatch(accent),
            ChipKind::Tag | ChipKind::Due | ChipKind::Priority => {
                Self::new(label).tone(accent.map_or(Tone::Neutral, Tone::Accent))
            }
        }
    }

    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    pub fn tone(mut self, tone: Tone<'a>) -> Self {
        self.tone = tone;
        self
    }

    pub fn lead(mut self, lead: Lead) -> Self {
        self.lead = lead;
        self
    }

    pub fn trail(mut self, trail: Trail) -> Self {
        self.trail = trail;
        self
    }

    /// Strike the label — a completed task's due. Paint, not width.
    pub fn struck(mut self, struck: bool) -> Self {
        self.struck = struck;
        self
    }

    pub fn height(&self) -> f32 {
        if self.swatch {
            CHIP_H
        } else {
            self.size.height()
        }
    }

    /// The trailing mark's lane, past the contents.
    fn trail_w(&self) -> f32 {
        match self.trail {
            Trail::None => 0.0,
            // Inside the end padding, after a gap: the mark's box ends at the
            // padding as the label starts at it.
            Trail::Caret | Trail::Clear => self.size.gap() + MARK,
            // Past the end padding, flush to the edge — the lane is a target
            // and wants the chip's whole height and its end.
            Trail::Remove => CHIP_CLOSE_W,
        }
    }

    /// The chip's whole width: what a caller lays out and hit-tests.
    pub fn width(&self, engine: &mut TextEngine) -> f32 {
        let trail = self.trail_w();
        if self.swatch {
            return SWATCH_D + trail;
        }
        let inner = match self.size {
            Size::Compact => self.compact_w(engine),
            Size::Regular => {
                let label = engine
                    .simple_layout(self.label, theme::role::hint())
                    .width();
                let lead = match self.lead {
                    Lead::None => 0.0,
                    Lead::Dot => DOT_D + self.size.gap(),
                    Lead::Glyph(glyph) => engine.icon_layout(glyph, MARK).width() + self.size.gap(),
                };
                label + lead + REGULAR_PAD_X * 2.0
            }
        };
        inner + trail
    }

    /// A compact chip's lozenge without its trail, from the engine's cache.
    fn compact_w(&self, engine: &mut TextEngine) -> f32 {
        engine.measure_attr_chip(
            ChipKind::Tag,
            self.label,
            self.cache_accent(),
            self.glyph(),
            self.struck,
        )
    }

    /// The accent a compact label is cached under. The cache bakes an ink
    /// into the layout; [`draw`](Self::draw) inks it with the tone's own
    /// anyway, so an inverted chip never shows a cached neutral one.
    fn cache_accent(&self) -> Option<&'a str> {
        match self.tone {
            Tone::Accent(token) => Some(token),
            Tone::Neutral | Tone::Inverted => None,
        }
    }

    fn glyph(&self) -> Option<char> {
        match self.lead {
            Lead::Glyph(glyph) => Some(glyph),
            _ => None,
        }
    }

    /// Where the trailing mark is inside `rect`, for a press to land on;
    /// `None` when the chip has none.
    pub fn trail_rect(&self, rect: Rect) -> Option<Rect> {
        match self.trail {
            Trail::None => None,
            Trail::Remove => Some(Rect::new(
                rect.x + rect.w - CHIP_CLOSE_W,
                rect.y,
                CHIP_CLOSE_W,
                rect.h,
            )),
            Trail::Caret | Trail::Clear => Some(Rect::new(
                rect.x + rect.w - self.size.pad_x() - MARK,
                rect.y + (rect.h - MARK) * 0.5,
                MARK,
                MARK,
            )),
        }
    }

    /// Paint the chip into `rect` — the rectangle [`width`](Self::width) and
    /// [`height`](Self::height) measured, or a wider one a caller reserved,
    /// in which case the contents are centred in it. The label is clipped to
    /// the lozenge, so a translation longer than the room never spills.
    #[track_caller]
    pub fn draw(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        state: State,
    ) {
        let _inspection = koru_draw::inspect::scope(
            "KoruLabelChip",
            "component",
            root,
            scene::rect(rect.x, rect.y, rect.w, rect.h),
            |r| {
                r.property(
                    "implementation",
                    "koru_ui::label_chip::LabelChip",
                    "shared implementation",
                );
                r.property(
                    "size / tone",
                    format!(
                        "{} / {}",
                        if self.swatch {
                            "swatch"
                        } else {
                            self.size.name()
                        },
                        self.tone.name()
                    ),
                    "instance",
                );
                r.property("trail", self.trail.name(), "instance");
                r.property(
                    "hovered / revealed",
                    format!("{} / {}", state.hovered, state.reveal),
                    "instance state",
                );
                r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
            },
        );
        let p = theme::p();
        if self.swatch {
            self.draw_swatch(canvas, root, rect, state, &p);
            return;
        }
        let (fill, ink) = self.tone.colors(&p);
        let pill = scene::rrect(rect.x, rect.y, rect.w, rect.h, rect.h * 0.5);
        scene::fill(canvas, root, fill, &pill);
        if state.hovered {
            // `bubble_hover` over the inverted surface: `hover_tint` is the
            // text colour at low alpha and vanishes against `bubble_bg`.
            let tint = if self.tone == Tone::Inverted {
                p.bubble_hover
            } else {
                p.hover_tint
            };
            scene::fill(canvas, root, tint, &pill);
        }
        canvas.push_clip_layer(Fill::NonZero, root, &pill);
        // Contents start at the padding, or centre in a rectangle wider than
        // the chip measured.
        let natural = self.width(engine);
        let x = rect.x + ((rect.w - natural) * 0.5).max(0.0) + self.size.pad_x();
        match self.size {
            Size::Compact => self.draw_compact(canvas, root, engine, rect, x, ink),
            Size::Regular => self.draw_regular(canvas, root, engine, rect, x, ink),
        }
        canvas.pop_layer();
        self.draw_trail(canvas, root, engine, rect, state, ink);
    }

    fn draw_compact(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        mut x: f32,
        ink: Color,
    ) {
        self.compact_w(engine);
        let Some(visual) =
            engine
                .attr_chips
                .get(ChipKind::Tag, self.label, self.cache_accent(), self.glyph())
        else {
            return;
        };
        // The leading glyph takes the chip's own ink rather than a colour of
        // its own: no chip glyph earns a `Palette` field.
        if let Some(lead) = &visual.lead {
            scene::draw_layout(
                canvas,
                root,
                lead,
                x,
                rect.y + (rect.h - lead.height()) * 0.5,
                Some(ink),
            );
            x += lead.width() + CHIP_ICON_GAP;
        }
        if let Some(label) = &visual.label {
            scene::draw_layout(
                canvas,
                root,
                label,
                x,
                rect.y + (rect.h - label.height()) * 0.5,
                Some(ink),
            );
            if self.struck {
                strike(canvas, root, ink, x, label.width(), rect);
            }
        }
    }

    fn draw_regular(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        mut x: f32,
        ink: Color,
    ) {
        let cy = rect.y + rect.h * 0.5;
        match self.lead {
            Lead::None => {}
            Lead::Dot => {
                scene::fill(
                    canvas,
                    root,
                    ink,
                    &Circle::new(((x + DOT_D * 0.5) as f64, cy as f64), (DOT_D * 0.5) as f64),
                );
                x += DOT_D + self.size.gap();
            }
            Lead::Glyph(glyph) => {
                let laid = engine.icon_layout(glyph, MARK);
                scene::draw_layout(canvas, root, &laid, x, cy - laid.height() * 0.5, Some(ink));
                x += laid.width() + self.size.gap();
            }
        }
        let laid = engine.simple_layout(self.label, theme::role::hint());
        scene::draw_layout_first_line(
            canvas,
            root,
            &laid,
            x,
            scene::centre_y_in(&laid, rect),
            Some(ink),
        );
        if self.struck {
            strike(canvas, root, ink, x, laid.width(), rect);
        }
    }

    fn draw_trail(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        state: State,
        ink: Color,
    ) {
        let Some(lane) = self.trail_rect(rect) else {
            return;
        };
        let (cx, cy) = (lane.x + lane.w * 0.5, lane.y + lane.h * 0.5);
        match self.trail {
            Trail::None => {}
            Trail::Caret => {
                let glyph = engine.icon_layout(icons::CARET_DOWN, MARK);
                scene::draw_layout(
                    canvas,
                    root,
                    &glyph,
                    cx - glyph.width() * 0.5,
                    cy - glyph.height() * 0.5,
                    Some(ink),
                );
            }
            Trail::Clear => draw_x(canvas, root, ink, cx, cy),
            // A hair left of the lane's centre, where the remove `×` has
            // always sat: the lane ends at the lozenge's rounded edge.
            Trail::Remove if state.reveal => draw_x(canvas, root, ink, cx - 1.0, cy),
            Trail::Remove => {}
        }
    }

    fn draw_swatch(
        &self,
        canvas: &mut Scene,
        root: Affine,
        rect: Rect,
        state: State,
        p: &theme::Palette,
    ) {
        let dot = match self.tone {
            Tone::Accent(token) => theme::accent_tone(token).map(|tone| tone.swatch),
            _ => None,
        }
        .unwrap_or(p.text_dim);
        let r = SWATCH_D * 0.5;
        scene::fill(
            canvas,
            root,
            dot,
            &Circle::new(
                ((rect.x + r) as f64, (rect.y + rect.h * 0.5) as f64),
                r as f64,
            ),
        );
        // **To the right of the dot**, in the same lane the lozenges reserve.
        // The swatch has no fill to carry a tone's ink, so the `×` takes the
        // neutral dim text colour: one in the dot's saturated hue reads as
        // part of the dot rather than as a control on it.
        if self.trail == Trail::Remove && state.reveal {
            let lane = Rect::new(rect.x + rect.w - CHIP_CLOSE_W, rect.y, CHIP_CLOSE_W, rect.h);
            draw_x(
                canvas,
                root,
                p.text_dim,
                lane.x + lane.w * 0.5 - 1.0,
                lane.y + lane.h * 0.5,
            );
        }
    }
}

/// A strike across the label's own advance — not the lozenge, whose glyph
/// and lane are not the thing that is done.
fn strike(canvas: &mut Scene, root: Affine, ink: Color, x: f32, w: f32, rect: Rect) {
    let mid = (rect.y + rect.h * 0.5) as f64;
    scene::stroke(
        canvas,
        root,
        ink,
        1.0,
        &vello::kurbo::Line::new((x as f64, mid), ((x + w) as f64, mid)),
    );
}

/// The `×`: two strokes centred on `(cx, cy)`. One mark for clearing a
/// filter and for removing an attribute — Phosphor has no `×` at this weight.
fn draw_x(canvas: &mut Scene, root: Affine, ink: Color, cx: f32, cy: f32) {
    let arm = X_ARM as f64;
    let (cx, cy) = (cx as f64, cy as f64);
    for (dx, dy) in [(arm, arm), (arm, -arm)] {
        scene::stroke(
            canvas,
            root,
            ink,
            X_STROKE,
            &vello::kurbo::Line::new((cx - dx, cy - dy), (cx + dx, cy + dy)),
        );
    }
}

#[cfg(test)]
mod tests;
