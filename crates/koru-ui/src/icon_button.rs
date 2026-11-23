// SPDX-License-Identifier: MPL-2.0

//! Shared treatment for standalone icon-only buttons.
//!
//! Geometry and paint deliberately match the caption `+` and pane toggles:
//! one square hover chip, a dim resting glyph, a normal hover glyph, and the
//! selected pack's filled variant or heavier outline for an on state.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme};
use vello::Scene;
use vello::kurbo::Affine;

#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub struct State {
    pub hovered: bool,
    pub active: bool,
    pub disabled: bool,
}

/// The hover treatment. `Default` is the neutral tint; `Danger` is the
/// caption Close's red, kept as a tone of the same button rather than a
/// second component.
///
/// `Quiet` is the neutral tint with **an on state said by the glyph alone**:
/// an `active` button still shows its filled glyph, and nothing else — no
/// accent plate under it and no accent ink. For a toggle that sits among
/// other chrome and is on most of the time, where a lit one would be the
/// loudest thing in the strip: a pane toggle in a caption. What `active`
/// means is unchanged; the tone only says how loudly it is drawn.
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Default,
    Danger,
    Quiet,
}

/// An icon control with a typed click result.
///
/// The widget deliberately returns an action instead of storing a callback:
/// drawing must stay independent from the application that gives a click its
/// meaning.  A caller can still read naturally as
/// `IconButton::new(Icon::Pane, rect).on_click(Action::ToggleContext)` while
/// the UI crate remains unaware of windows, documents, and menus.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct IconButton<Action = ()> {
    glyph: char,
    slot: Rect,
    action: Action,
    state: State,
    tone: Tone,
}

impl IconButton {
    pub const fn new(glyph: char, slot: Rect) -> Self {
        Self {
            glyph,
            slot,
            action: (),
            state: State {
                hovered: false,
                active: false,
                disabled: false,
            },
            tone: Tone::Default,
        }
    }
}

impl<Action> IconButton<Action> {
    pub fn on_click<Next>(self, action: Next) -> IconButton<Next> {
        IconButton {
            glyph: self.glyph,
            slot: self.slot,
            action,
            state: self.state,
            tone: self.tone,
        }
    }

    pub const fn with_state(mut self, state: State) -> Self {
        self.state = state;
        self
    }

    pub const fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    pub const fn rect(&self) -> Rect {
        self.slot
    }

    /// The glyph the button shows — for a caller, or a test, reading back
    /// what a toolbar is drawing.
    pub const fn glyph(&self) -> char {
        self.glyph
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.slot.contains(x, y)
    }

    pub fn action_at(self, x: f32, y: f32) -> Option<Action>
    where
        Action: Copy,
    {
        (!self.state.disabled && self.contains(x, y)).then_some(self.action)
    }

    #[track_caller]
    pub fn draw(&self, canvas: &mut Scene, root: Affine, engine: &mut TextEngine) {
        draw_toned(
            canvas, root, engine, self.slot, self.glyph, self.state, self.tone,
        );
    }
}

/// Geometry and type size for an icon-only control.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Metrics {
    pub icon_size: f32,
    /// Recommended clear space on either horizontal side of the glyph.
    pub padding_x: f32,
    /// Recommended clear space above and below the glyph.
    pub padding_y: f32,
    pub inset_top: f32,
    pub inset_bottom: f32,
    pub radius: f32,
}

impl Metrics {
    pub const STANDARD: Self = Self {
        icon_size: theme::CAPTION_ICON,
        padding_x: 0.0,
        padding_y: 0.0,
        inset_top: theme::CAPTION_INSET_TOP,
        inset_bottom: theme::CAPTION_INSET_BOTTOM,
        radius: 6.0,
    };

    /// A control inside a page or a row rather than a strip: the standard
    /// glyph, with none of the tab strip's insets, so its hover square is
    /// the slot it is given — a page's `…`, a list row's `…`.
    pub const INLINE: Self = Self {
        inset_top: 0.0,
        inset_bottom: 0.0,
        ..Self::STANDARD
    };

    /// Search-panel controls: 10% smaller glyphs with 4 px on every side.
    pub const COMPACT: Self = Self {
        icon_size: theme::CAPTION_ICON * 0.90,
        padding_x: 4.0,
        padding_y: 4.0,
        inset_top: 0.0,
        inset_bottom: 0.0,
        radius: 6.0,
    };

    pub const fn control_width(self) -> f32 {
        self.icon_size + self.padding_x * 2.0
    }

    pub const fn control_height(self) -> f32 {
        self.icon_size + self.padding_y * 2.0
    }
}

/// The visual chip inside a button's full hit slot.
pub fn hover_chip(slot: Rect) -> Rect {
    hover_chip_with(slot, Metrics::STANDARD)
}

pub fn hover_chip_with(slot: Rect, metrics: Metrics) -> Rect {
    let h = (slot.h - metrics.inset_top - metrics.inset_bottom).max(0.0);
    let side = h.min(slot.w);
    Rect::new(
        slot.x + (slot.w - side) * 0.5,
        slot.y + metrics.inset_top + (h - side) * 0.5,
        side,
        side,
    )
}

#[track_caller]
pub fn draw(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    slot: Rect,
    glyph: char,
    state: State,
) {
    draw_with(canvas, root, engine, slot, glyph, state, Metrics::STANDARD);
}

/// The same control with an explicit [`Tone`] — the caption Close's danger
/// hover without a second component.
#[track_caller]
pub fn draw_toned(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    slot: Rect,
    glyph: char,
    state: State,
    tone: Tone,
) {
    paint(
        canvas,
        root,
        engine,
        slot,
        glyph,
        state,
        Metrics::STANDARD,
        tone,
        false,
        theme::p(),
    );
}

/// The control with everything a caller can say about it: its metrics, its
/// [`Tone`], and whether the keyboard is on it.
///
/// While `focused`, the button's chip — the square its hover plate fills —
/// is ringed in the palette's `focus_ring` at twice a border's weight: the
/// mark [`crate::chip::Chip`] and [`crate::toggle::Toggle`] draw. A button
/// that is switched off shows no ring: the keyboard does not stop on it.
#[allow(clippy::too_many_arguments)]
#[track_caller]
pub fn draw_focused(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    slot: Rect,
    glyph: char,
    state: State,
    metrics: Metrics,
    tone: Tone,
    focused: bool,
) {
    paint(
        canvas,
        root,
        engine,
        slot,
        glyph,
        state,
        metrics,
        tone,
        focused,
        theme::p(),
    );
}

#[allow(clippy::too_many_arguments)]
#[track_caller]
pub fn draw_with(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    slot: Rect,
    glyph: char,
    state: State,
    metrics: Metrics,
) {
    paint(
        canvas,
        root,
        engine,
        slot,
        glyph,
        state,
        metrics,
        Tone::Default,
        false,
        theme::p(),
    );
}

/// [`draw_with`] in `palette`, the table of the theme the control is shown
/// under: an element subtree with its own theme paints its buttons in it.
#[allow(clippy::too_many_arguments)]
#[track_caller]
pub fn draw_with_in(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    slot: Rect,
    glyph: char,
    state: State,
    metrics: Metrics,
    palette: &theme::Palette,
) {
    paint(
        canvas,
        root,
        engine,
        slot,
        glyph,
        state,
        metrics,
        Tone::Default,
        false,
        palette,
    );
}

#[allow(clippy::too_many_arguments)]
fn paint(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    slot: Rect,
    glyph: char,
    state: State,
    metrics: Metrics,
    tone: Tone,
    focused: bool,
    palette: &theme::Palette,
) {
    let _inspection = inspect_icon(root, slot, state, metrics, tone, Some(glyph));
    let chip = hover_chip_with(slot, metrics);
    let danger = tone == Tone::Danger;
    // A quiet button's on state is its filled glyph and nothing else.
    let lit = state.active && tone != Tone::Quiet;
    if lit && !state.disabled {
        scene::fill(
            canvas,
            root,
            palette
                .accent
                .multiply_alpha(if state.hovered { 0.20 } else { 0.12 }),
            &scene::rrect(chip.x, chip.y, chip.w, chip.h, metrics.radius),
        );
    } else if state.hovered && !state.disabled {
        scene::fill(
            canvas,
            root,
            if danger {
                palette.close_hover
            } else {
                palette.hover_tint
            },
            &scene::rrect(chip.x, chip.y, chip.w, chip.h, metrics.radius),
        );
    }
    if focused && !state.disabled {
        crate::button::focus_stroke(canvas, root, palette, chip, metrics.radius, 1.0);
    }
    let layout = if state.active {
        engine.icon_layout_filled(glyph, metrics.icon_size)
    } else {
        engine.icon_layout(glyph, metrics.icon_size)
    };
    let colour = if state.disabled {
        palette.text_dim.multiply_alpha(0.45)
    } else if lit {
        palette.accent
    } else if state.hovered {
        if danger {
            palette.close_hover_fg
        } else {
            palette.text
        }
    } else {
        palette.text_dim
    };
    scene::draw_layout(
        canvas,
        root,
        &layout,
        slot.x + (slot.w - layout.width()) * 0.5,
        slot.y + (slot.h - layout.height()) * 0.5,
        Some(colour),
    );
}

/// Text vocabulary such as `Aa` with the same chip, state, and centring as an
/// icon button. The label is content; the control geometry remains shared.
#[allow(clippy::too_many_arguments)]
#[track_caller]
pub fn draw_label_with(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    slot: Rect,
    label: &str,
    style: theme::TextStyle,
    state: State,
    metrics: Metrics,
) {
    let _inspection = inspect_icon(root, slot, state, metrics, Tone::Default, None);
    let chip = hover_chip_with(slot, metrics);
    if state.active && !state.disabled {
        scene::fill(
            canvas,
            root,
            theme::p()
                .accent
                .multiply_alpha(if state.hovered { 0.20 } else { 0.12 }),
            &scene::rrect(chip.x, chip.y, chip.w, chip.h, metrics.radius),
        );
    } else if state.hovered && !state.disabled {
        scene::fill(
            canvas,
            root,
            theme::p().hover_tint,
            &scene::rrect(chip.x, chip.y, chip.w, chip.h, metrics.radius),
        );
    }
    let text = engine.simple_layout(label, style);
    let colour = if state.disabled {
        theme::p().text_dim.multiply_alpha(0.45)
    } else if state.active {
        theme::p().accent
    } else if state.hovered {
        theme::p().text
    } else {
        theme::p().text_dim
    };
    scene::draw_layout_first_line(
        canvas,
        root,
        &text,
        slot.x + (slot.w - text.width()) * 0.5,
        crate::text_field::baseline_centred_y(&text, slot.y, slot.h),
        Some(colour),
    );
}

/// Close is vocabulary, not a separate visual treatment.
#[track_caller]
fn inspect_icon(
    root: Affine,
    slot: Rect,
    state: State,
    metrics: Metrics,
    tone: Tone,
    glyph: Option<char>,
) -> koru_draw::inspect::Scope {
    koru_draw::inspect::scope(
        "KoruIconButton",
        "component",
        root,
        scene::rect(slot.x, slot.y, slot.w, slot.h),
        |r| {
            r.property(
                "implementation",
                "koru_ui::icon_button",
                "shared implementation",
            );
            r.property(
                "state",
                format!("{state:?}"),
                "instance state; focus not exposed by this API",
            );
            if tone != Tone::Default {
                r.property(
                    "tone",
                    format!("{tone:?}"),
                    "caller property; the danger hover is the caption Close's",
                );
            }
            r.property(
                "metrics",
                format!("{metrics:?}"),
                if metrics == Metrics::STANDARD {
                    "Metrics::STANDARD"
                } else if metrics == Metrics::COMPACT {
                    "Metrics::COMPACT"
                } else {
                    "caller override; review unknown"
                },
            );
            r.property(
                "hit slot",
                format!("{slot:?}"),
                "caller rectangle; host semantics not implied",
            );
            r.property(
                "paint chip",
                format!("{:?}", hover_chip_with(slot, metrics)),
                "derived by hover_chip_with",
            );
            if let Some(glyph) = glyph {
                if let Some((name, _)) =
                    icons::ALL.iter().find(|(_, codepoint)| *codepoint == glyph)
                {
                    r.property("icon name", name, "icons::ALL; aliases may share a glyph");
                }
                r.property(
                    "icon codepoint",
                    format!("U+{:04X}", glyph as u32),
                    "caller icon; docs/features/koruui/ICONS.md",
                );
                if let Some(def) = icons::vector_def(glyph) {
                    r.property(
                        "icon pack",
                        icons::rendered_pack().label(),
                        "selected icon pack",
                    );
                    r.property("icon source", def.source, "artwork provenance");
                }
            }
            r.property("layout", "centered glyph or label", "shared implementation");
            r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
        },
    )
}

pub const CLOSE: char = icons::WINDOW_CLOSE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chip_is_square_centered_and_clear_of_the_caption_hairline() {
        let slot = Rect::new(10.0, 4.0, 40.0, theme::CAPTION_H);
        let chip = hover_chip(slot);
        assert_eq!(chip.w, chip.h);
        assert!((chip.x + chip.w / 2.0 - (slot.x + slot.w / 2.0)).abs() < 0.01);
        assert!(chip.y > slot.y);
        assert!(chip.y + chip.h < slot.y + slot.h);
    }

    #[test]
    fn compact_metrics_reduce_glyph_and_use_four_pixel_padding() {
        assert_eq!(
            Metrics::COMPACT.icon_size,
            Metrics::STANDARD.icon_size * 0.90
        );
        assert_eq!(Metrics::COMPACT.padding_x, 4.0);
        assert_eq!(Metrics::COMPACT.padding_y, 4.0);
        assert_eq!(
            Metrics::COMPACT.control_width(),
            Metrics::COMPACT.icon_size + 8.0
        );
        assert_eq!(
            Metrics::COMPACT.control_height(),
            Metrics::COMPACT.icon_size + 8.0
        );
    }

    const SLOT: Rect = Rect {
        x: 4.0,
        y: 4.0,
        w: 26.0,
        h: 26.0,
    };

    fn state(hovered: bool, active: bool) -> State {
        State {
            hovered,
            active,
            disabled: false,
        }
    }

    fn painted(state: State, tone: Tone, focused: bool) -> Scene {
        let mut engine = TextEngine::new();
        let mut canvas = Scene::new();
        draw_focused(
            &mut canvas,
            Affine::IDENTITY,
            &mut engine,
            SLOT,
            icons::PANEL_LEFT,
            state,
            Metrics::INLINE,
            tone,
            focused,
        );
        canvas
    }

    fn same(a: &Scene, b: &Scene) -> bool {
        let (a, b) = (a.encoding(), b.encoding());
        a.path_tags == b.path_tags
            && a.path_data == b.path_data
            && a.draw_tags == b.draw_tags
            && a.draw_data == b.draw_data
            && a.styles == b.styles
    }

    /// The older entry points are `draw_focused` without the ring, path for
    /// path, so no existing caller changed.
    #[test]
    fn the_existing_entry_points_paint_what_they_painted() {
        for (hovered, active) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut engine = TextEngine::new();
            let mut canvas = Scene::new();
            draw_with(
                &mut canvas,
                Affine::IDENTITY,
                &mut engine,
                SLOT,
                icons::PANEL_LEFT,
                state(hovered, active),
                Metrics::INLINE,
            );
            assert!(same(
                &canvas,
                &painted(state(hovered, active), Tone::Default, false)
            ));
        }
    }

    /// A quiet button that is on is the filled glyph in the ordinary ink:
    /// no plate at rest, the neutral plate under the pointer, exactly as
    /// when it is off but for the glyph.
    #[test]
    fn a_quiet_button_says_on_with_its_glyph_alone() {
        let plates = |scene: &Scene| scene.encoding().draw_tags.len();
        let on = painted(state(false, true), Tone::Quiet, false);
        let off = painted(state(false, false), Tone::Quiet, false);
        let lit = painted(state(false, true), Tone::Default, false);
        assert_eq!(plates(&on), plates(&off), "no plate under a quiet on state");
        assert_eq!(
            plates(&lit),
            plates(&on) + 1,
            "the accent plate is Default's"
        );
        assert!(!same(&on, &off), "the glyph is the filled one");
        // The ink is the one an off button has: the same colors are drawn.
        assert_eq!(on.encoding().draw_data, off.encoding().draw_data);
        assert_ne!(on.encoding().draw_data, lit.encoding().draw_data);
        // Under the pointer it lights as an off button does.
        let hot_on = painted(state(true, true), Tone::Quiet, false);
        let hot_off = painted(state(true, false), Tone::Quiet, false);
        assert_eq!(plates(&hot_on), plates(&on) + 1);
        assert_eq!(
            hot_on.encoding().draw_data,
            hot_off.encoding().draw_data,
            "the neutral plate and the hover ink, on or off"
        );
        // Off, a quiet button is a default one.
        assert!(same(
            &off,
            &painted(state(false, false), Tone::Default, false)
        ));
    }

    #[test]
    fn the_focus_ring_is_one_stroke_more_and_none_on_a_disabled_button() {
        let plain = painted(state(false, false), Tone::Quiet, false);
        let ringed = painted(state(false, false), Tone::Quiet, true);
        assert_eq!(
            ringed.encoding().draw_tags.len(),
            plain.encoding().draw_tags.len() + 1
        );
        let off = State {
            disabled: true,
            ..State::default()
        };
        assert!(same(
            &painted(off, Tone::Default, true),
            &painted(off, Tone::Default, false)
        ));
    }

    #[test]
    fn typed_click_action_only_arrives_inside_an_enabled_button() {
        #[derive(Copy, Clone, Debug, PartialEq)]
        enum Action {
            TogglePane,
        }

        let button = IconButton::new(icons::PANEL_RIGHT, Rect::new(10.0, 20.0, 28.0, 28.0))
            .on_click(Action::TogglePane);
        assert_eq!(button.action_at(12.0, 22.0), Some(Action::TogglePane));
        assert_eq!(button.action_at(9.0, 22.0), None);

        let disabled = button.with_state(State {
            disabled: true,
            ..State::default()
        });
        assert_eq!(disabled.action_at(12.0, 22.0), None);
    }
}
