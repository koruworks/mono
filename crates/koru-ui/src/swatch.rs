// SPDX-License-Identifier: MPL-2.0

//! A color swatch: a filled square that can be chosen, and a row of them.
//!
//! **The chosen one is ringed, not filled differently** — the fill *is* the
//! value, so the selection has to be drawn around it. The ring sits outside the
//! square with a gap of the page color, so a dark swatch on a dark ring still
//! reads as ringed. A swatch with no color ([`None`]) is the "automatic" or
//! "none" value and is drawn hollow.

use koru_draw::geometry::Rect;
use koru_draw::{inspect, scene, theme};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Color;

pub struct Swatch;

impl Swatch {
    pub const SIZE: f32 = 22.0;
    pub const GAP: f32 = 10.0;
    /// The square's corner; the rings follow it outwards.
    pub(crate) const RADIUS: f32 = 6.0;
    /// The ring's gap from the square, and its width.
    const RING_GAP: f32 = 2.0;
    const RING_W: f64 = 2.0;
    /// The keyboard-focus ring, outside the selection ring so a focused,
    /// chosen swatch shows both.
    const FOCUS_GAP: f32 = 4.0;
    /// How far outside the square the outermost ring's far edge reaches.
    /// Whatever sits beside a row of swatches keeps this clear.
    pub const RING_REACH: f32 = Self::FOCUS_GAP + Self::RING_W as f32;

    /// Rects for a row of `count` swatches starting at (`x`, `y`).
    pub fn row(x: f32, y: f32, count: usize) -> Vec<Rect> {
        (0..count)
            .map(|i| {
                Rect::new(
                    x + i as f32 * (Self::SIZE + Self::GAP),
                    y,
                    Self::SIZE,
                    Self::SIZE,
                )
            })
            .collect()
    }

    /// The width of a row of `count`.
    pub fn row_width(count: usize) -> f32 {
        if count == 0 {
            0.0
        } else {
            count as f32 * Self::SIZE + (count - 1) as f32 * Self::GAP
        }
    }

    #[track_caller]
    pub fn draw(
        canvas: &mut Scene,
        root: Affine,
        rect: Rect,
        color: Option<Color>,
        selected: bool,
        hovered: bool,
        focused: bool,
        alpha: f32,
    ) {
        let _component = inspect::scope(
            "KoruSwatch",
            "component",
            root,
            scene::rect(rect.x, rect.y, rect.w, rect.h),
            |r| {
                r.property(
                    "implementation",
                    "koru_ui::swatch::Swatch",
                    "shared implementation",
                );
                r.property(
                    "color",
                    color.map_or("none".to_owned(), inspect::color),
                    "caller value",
                );
                r.property(
                    "selected / hovered / focused",
                    format!("{selected} / {hovered} / {focused}"),
                    "instance state",
                );
                r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
            },
        );
        let p = theme::p();
        let shape = scene::rrect(rect.x, rect.y, rect.w, rect.h, Self::RADIUS);
        match color {
            Some(color) => scene::fill(canvas, root, color.multiply_alpha(alpha), &shape),
            None => scene::stroke(
                canvas,
                root,
                p.panel_border.multiply_alpha(alpha),
                theme::BORDER_W,
                &shape,
            ),
        }
        Self::draw_rings(
            canvas,
            root,
            rect,
            Self::RADIUS,
            selected,
            hovered,
            focused,
            alpha,
        );
    }

    /// **The rings round a chosen, hovered or focused square**, in the one
    /// place they are drawn: the selection ring (or the hover ring) a ring's
    /// gap outside `rect`, and the focus ring outside that, each following
    /// the corner `radius` outwards. The furthest reaches
    /// [`RING_REACH`](Self::RING_REACH). Every square that is chosen this way
    /// calls it — a swatch, and an emoji row's cell — so the two can never
    /// disagree about what chosen and focused look like.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_rings(
        canvas: &mut Scene,
        root: Affine,
        rect: Rect,
        radius: f32,
        selected: bool,
        hovered: bool,
        focused: bool,
        alpha: f32,
    ) {
        let p = theme::p();
        let out = Self::RING_GAP + Self::RING_W as f32 * 0.5;
        let ring = scene::rrect(
            rect.x - out,
            rect.y - out,
            rect.w + out * 2.0,
            rect.h + out * 2.0,
            radius + out,
        );
        if selected {
            scene::stroke(
                canvas,
                root,
                p.accent.multiply_alpha(alpha),
                Self::RING_W,
                &ring,
            );
        } else if hovered {
            scene::stroke(
                canvas,
                root,
                p.panel_border.multiply_alpha(alpha),
                Self::RING_W,
                &ring,
            );
        }
        if focused {
            let out = Self::FOCUS_GAP + Self::RING_W as f32 * 0.5;
            scene::stroke(
                canvas,
                root,
                p.focus_ring.multiply_alpha(alpha),
                Self::RING_W,
                &scene::rrect(
                    rect.x - out,
                    rect.y - out,
                    rect.w + out * 2.0,
                    rect.h + out * 2.0,
                    radius + out,
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_is_evenly_spaced_and_its_width_is_what_it_occupies() {
        let rects = Swatch::row(10.0, 5.0, 4);
        assert_eq!(rects.len(), 4);
        assert_eq!(rects[1].x - rects[0].x, Swatch::SIZE + Swatch::GAP);
        let last = rects[3];
        assert_eq!(last.x + last.w - 10.0, Swatch::row_width(4));
        assert_eq!(Swatch::row_width(0), 0.0);
    }
}
