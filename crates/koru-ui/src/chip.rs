// SPDX-License-Identifier: MPL-2.0

//! Compact semantic choices with one measured, painted, and focused shape.
//!
//! Callers own flow and hit geometry; this component owns the visual contract
//! shared by settings choices and filter chips. Labels are intrinsically
//! measured and remain one line. A caller that must clamp an exceptionally
//! long localized label gets a hard clip at the chip boundary rather than text
//! leaking into its neighbour.
//!
//! A chip may reserve a **leading slot** before its label
//! ([`Chip::leading`]) for a mark the caller draws there — an agent's brand
//! mark on *Agent providers*' *Default agent* row. The label then starts after
//! the slot instead of being centred, and the width counts the slot.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Fill;

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Chip<'a> {
    label: &'a str,
    selected: bool,
    /// Room before the label for a mark the caller draws; `0.0` for none.
    leading: f32,
}

impl<'a> Chip<'a> {
    pub const H: f32 = 28.0;
    pub const PAD_X: f32 = 10.0;
    pub const RADIUS: f32 = 6.0;

    pub const fn new(label: &'a str, selected: bool) -> Self {
        Self {
            label,
            selected,
            leading: 0.0,
        }
    }

    /// Reserve `w` before the label for a mark the caller draws in
    /// [`leading_rect`](Self::leading_rect). The label starts after it.
    pub const fn leading(mut self, w: f32) -> Self {
        self.leading = w;
        self
    }

    pub fn width(self, engine: &mut TextEngine) -> f32 {
        engine.simple_layout(self.label, theme::role::row()).width()
            + Self::PAD_X * 2.0
            + self.leading
    }

    /// Where the caller draws its leading mark inside `rect`: the reserved
    /// slot, from the chip's left inset, the chip's full height.
    pub fn leading_rect(self, rect: Rect) -> Rect {
        Rect::new(rect.x + Self::PAD_X, rect.y, self.leading, rect.h)
    }

    #[track_caller]
    pub fn draw(
        self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        hovered: bool,
        focused: bool,
    ) {
        let _inspection = koru_draw::inspect::scope(
            "KoruChip",
            "component",
            root,
            scene::rect(rect.x, rect.y, rect.w, rect.h),
            |r| {
                r.property(
                    "implementation",
                    "koru_ui::chip::Chip",
                    "shared implementation",
                );
                r.property(
                    "selected / hovered / focused",
                    format!("{} / {} / {}", self.selected, hovered, focused),
                    "instance state",
                );
                r.property("leading slot", self.leading, "instance state");
                r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
            },
        );
        let p = theme::p();
        let shape = scene::rrect(rect.x, rect.y, rect.w, rect.h, Self::RADIUS);
        scene::fill(
            canvas,
            root,
            if self.selected {
                p.row_selected
            } else {
                p.chip_bg
            },
            &shape,
        );
        if hovered {
            scene::fill(canvas, root, p.hover_tint, &shape);
        }
        scene::stroke(
            canvas,
            root,
            if focused {
                p.focus_ring
            } else {
                p.panel_border
            },
            if focused {
                theme::BORDER_W * 2.0
            } else {
                theme::BORDER_W
            },
            &shape,
        );

        let text = engine.simple_layout(self.label, theme::role::row());
        canvas.push_clip_layer(Fill::NonZero, root, &shape);
        // Centred alone; after the slot when the chip leads with a mark.
        let x = if self.leading > 0.0 {
            rect.x + Self::PAD_X + self.leading
        } else {
            rect.x + (rect.w - text.width()) * 0.5
        };
        scene::draw_layout_first_line(
            canvas,
            root,
            &text,
            x,
            crate::text_field::baseline_centred_y(&text, rect.y, rect.h),
            Some(if self.selected { p.text } else { p.text_dim }),
        );
        canvas.pop_layer();
    }
}

/// Minimum width for choices arranged by [`wrap`]. Individual chips retain
/// their intrinsic width so reminder filters do not acquire a form's width floor.
pub const MIN_W: f32 = 64.0;
/// Gap between choices arranged by [`wrap`].
pub const GAP: f32 = 6.0;

/// Place a wrapping row of chips. Rects are in the same order as `labels`.
pub fn wrap(
    engine: &mut TextEngine,
    x: f32,
    y: f32,
    right: f32,
    labels: impl IntoIterator<Item = impl AsRef<str>>,
) -> Vec<Rect> {
    let mut cx = x;
    let mut cy = y;
    let mut used = false;
    let mut out = Vec::new();
    for label in labels {
        let w = Chip::new(label.as_ref(), false).width(engine).max(MIN_W);
        if used && cx + w > right {
            cx = x;
            cy += Chip::H + GAP;
        }
        out.push(Rect::new(cx, cy, w, Chip::H));
        cx += w + GAP;
        used = true;
    }
    out
}

/// The bottom edge of a wrapping row, or `origin_y` when it was empty.
pub fn bottom(rects: &[Rect], origin_y: f32) -> f32 {
    rects
        .iter()
        .map(|rect| rect.y + rect.h)
        .fold(origin_y, f32::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measured_width_contains_the_label_and_both_pads() {
        let mut engine = TextEngine::new();
        let label = engine.simple_layout("Coming up", theme::role::row());
        let width = Chip::new("Coming up", false).width(&mut engine);
        assert!((width - label.width() - Chip::PAD_X * 2.0).abs() < 0.01);
    }

    #[test]
    fn a_leading_slot_widens_the_chip_and_sits_inside_its_inset() {
        let mut engine = TextEngine::new();
        let plain = Chip::new("Claude", false).width(&mut engine);
        let marked = Chip::new("Claude", false).leading(22.0);
        assert!((marked.width(&mut engine) - plain - 22.0).abs() < 0.01);
        let rect = Rect::new(10.0, 4.0, marked.width(&mut engine), Chip::H);
        let slot = marked.leading_rect(rect);
        assert_eq!(slot.x, rect.x + Chip::PAD_X);
        assert_eq!((slot.w, slot.h), (22.0, Chip::H));
    }

    #[test]
    fn a_short_label_still_gets_the_floor() {
        let mut engine = TextEngine::new();
        assert_eq!(wrap(&mut engine, 0.0, 0.0, 500.0, ["On"])[0].w, MIN_W);
    }

    #[test]
    fn a_long_label_grows_the_chip() {
        let mut engine = TextEngine::new();
        let chips = wrap(&mut engine, 0.0, 0.0, 500.0, ["Once", "Every time"]);
        let once = chips[0].w;
        let every = chips[1].w;
        assert!(every > once);
        assert!(every > MIN_W);
    }

    #[test]
    fn a_row_wraps_at_the_right_edge() {
        let mut engine = TextEngine::new();
        let rects = wrap(
            &mut engine,
            0.0,
            10.0,
            140.0,
            ["Alpha", "Bravo", "Charlie", "Delta"],
        );
        assert_eq!(rects.len(), 4);
        assert!(
            rects.iter().any(|rect| (rect.y - 10.0).abs() > 0.5),
            "four measured chips do not fit on a 140px row"
        );
        assert_eq!(
            bottom(&rects, 10.0),
            rects.iter().map(|r| r.y + r.h).fold(0.0, f32::max)
        );
    }
}
