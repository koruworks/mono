// SPDX-License-Identifier: MPL-2.0

//! A segmented control: two to a handful of mutually exclusive words in one
//! track, the chosen one raised.
//!
//! **One control, not a row of chips.** Where a row of separate chips reads as
//! several things to press, a track reads as one setting with several values —
//! which is what *Compact / Normal / Comfortable* is. The track is the
//! component's boundary, so the inspector shows one `KoruSegmented` with its
//! segments as parts rather than loose boxes.
//!
//! Geometry is measured once by [`Segmented::layout`] and both drawing and the
//! caller's hit test use the rects it returns — there is no second copy of the
//! arithmetic to drift. What a segment *means* is the caller's: it gets back an
//! index.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{inspect, scene, theme};
use vello::Scene;
use vello::kurbo::Affine;

/// A measured segmented control.
#[derive(Clone, Debug, PartialEq)]
pub struct Segmented {
    /// The track.
    pub bounds: Rect,
    /// One rect per label, inside the track, in the order given.
    pub segments: Vec<Rect>,
}

impl Segmented {
    /// The track's height.
    pub const H: f32 = 30.0;
    /// The inset between the track and a raised segment.
    pub const INSET: f32 = 2.0;
    /// Horizontal padding either side of a segment's word.
    pub const PAD_X: f32 = 12.0;
    /// The narrowest a segment is drawn, so *Ask* is not a sliver beside
    /// *Keep in background*.
    pub const MIN_W: f32 = 48.0;
    const RADIUS: f32 = 7.0;

    /// The width the control needs for `labels`.
    pub fn width(engine: &mut TextEngine, labels: &[&str]) -> f32 {
        labels
            .iter()
            .map(|label| Self::segment_width(engine, label))
            .sum::<f32>()
            + Self::INSET * 2.0
    }

    fn segment_width(engine: &mut TextEngine, label: &str) -> f32 {
        (engine.simple_layout(label, theme::role::row()).width() + Self::PAD_X * 2.0)
            .max(Self::MIN_W)
    }

    /// Lay the control out with its top-left at (`x`, `y`).
    pub fn layout(engine: &mut TextEngine, labels: &[&str], x: f32, y: f32) -> Self {
        let mut segments = Vec::with_capacity(labels.len());
        let mut cursor = x + Self::INSET;
        for label in labels {
            let w = Self::segment_width(engine, label);
            segments.push(Rect::new(
                cursor,
                y + Self::INSET,
                w,
                Self::H - Self::INSET * 2.0,
            ));
            cursor += w;
        }
        let w = cursor + Self::INSET - x;
        Self {
            bounds: Rect::new(x, y, w, Self::H),
            segments,
        }
    }

    /// The segment under (`x`, `y`).
    pub fn segment_at(&self, x: f32, y: f32) -> Option<usize> {
        self.segments.iter().position(|r| r.contains(x, y))
    }

    /// Paint the control. `alpha` dims the whole control for a read-only
    /// surface without changing where anything is.
    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    pub fn draw(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        labels: &[&str],
        selected: Option<usize>,
        hovered: Option<usize>,
        alpha: f32,
    ) {
        self.draw_focused(
            canvas, root, engine, labels, selected, hovered, false, alpha,
        );
    }

    /// Paint the control with an optional keyboard-focus ring round its
    /// track.
    ///
    /// The control is one stop for the keyboard — the arrows move the choice
    /// inside it — so the mark is the track's, not a segment's. It is the
    /// shared controls' ring ([`crate::chip::Chip`]'s and
    /// [`crate::toggle::Toggle`]'s), drawn on the track's own edge: nothing
    /// moves and no room is needed round the control.
    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    pub fn draw_focused(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        labels: &[&str],
        selected: Option<usize>,
        hovered: Option<usize>,
        focused: bool,
        alpha: f32,
    ) {
        self.draw_tinted(
            canvas, root, engine, labels, selected, hovered, focused, alpha, None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_tinted(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        labels: &[&str],
        selected: Option<usize>,
        hovered: Option<usize>,
        focused: bool,
        alpha: f32,
        dangerous: Option<usize>,
    ) {
        self.draw_highlighted(
            canvas, root, engine, labels, selected, hovered, focused, alpha, dangerous, "",
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_highlighted(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        labels: &[&str],
        selected: Option<usize>,
        hovered: Option<usize>,
        focused: bool,
        alpha: f32,
        dangerous: Option<usize>,
        query: &str,
    ) {
        let b = self.bounds;
        let _component = inspect::scope(
            "KoruSegmented",
            "component",
            root,
            scene::rect(b.x, b.y, b.w, b.h),
            |r| {
                r.property(
                    "implementation",
                    "koru_ui::segmented::Segmented",
                    "shared implementation",
                );
                r.property("segments", labels.len(), "caller labels");
                r.property(
                    "selected",
                    selected.map_or("none".to_owned(), |i| i.to_string()),
                    "instance state",
                );
                r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
            },
        );
        let p = theme::p();
        let track = scene::rrect(b.x, b.y, b.w, b.h, Self::RADIUS);
        scene::fill(canvas, root, p.chip_bg.multiply_alpha(alpha), &track);
        if focused {
            crate::button::focus_stroke(canvas, root, p, b, Self::RADIUS, alpha);
        }
        for (index, (rect, label)) in self.segments.iter().zip(labels).enumerate() {
            let chosen = selected == Some(index);
            let _segment = inspect::scope(
                "Segment",
                "part",
                root,
                scene::rect(rect.x, rect.y, rect.w, rect.h),
                |r| {
                    r.property("label", *label, "caller label");
                    r.property("selected", chosen, "instance state");
                },
            );
            let shape = scene::rrect(rect.x, rect.y, rect.w, rect.h, Self::RADIUS - 2.0);
            if chosen {
                scene::fill(canvas, root, p.panel_bg.multiply_alpha(alpha), &shape);
                scene::stroke(
                    canvas,
                    root,
                    p.panel_border.multiply_alpha(alpha),
                    theme::BORDER_W,
                    &shape,
                );
            } else if hovered == Some(index) {
                scene::fill(canvas, root, p.hover_tint.multiply_alpha(alpha), &shape);
            }
            if dangerous == Some(index) {
                scene::fill(
                    canvas,
                    root,
                    p.close_hover
                        .multiply_alpha(if chosen { 0.18 } else { 0.08 } * alpha),
                    &shape,
                );
            }
            let layout = engine.simple_layout(label, theme::role::row());
            crate::text_paragraph::highlight_layout(
                canvas,
                root,
                &layout,
                label,
                query,
                rect.x + (rect.w - layout.width()) * 0.5,
                crate::typography::control_label_y(engine, *rect),
            );
            crate::typography::line(
                "Segment label",
                canvas,
                root,
                &layout,
                rect.x + (rect.w - layout.width()) * 0.5,
                crate::typography::control_label_y(engine, *rect),
                theme::role::row(),
                "theme::role::row()",
                if dangerous == Some(index) {
                    p.close_hover
                } else if chosen {
                    p.text
                } else {
                    p.text_dim
                }
                .multiply_alpha(alpha),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_tile_the_track_and_are_measured_not_gridded() {
        let mut engine = TextEngine::new();
        let labels = ["Keep in background", "Ask", "Quit"];
        let control = Segmented::layout(&mut engine, &labels, 10.0, 20.0);
        assert_eq!(control.segments.len(), 3);
        assert!(control.segments[0].w > control.segments[1].w * 2.0);
        assert!(control.segments[1].w >= Segmented::MIN_W);
        for pair in control.segments.windows(2) {
            assert_eq!(pair[0].x + pair[0].w, pair[1].x, "no gap, no overlap");
        }
        let last = control.segments[2];
        assert_eq!(
            last.x + last.w + Segmented::INSET,
            control.bounds.x + control.bounds.w
        );
        assert_eq!(
            control.bounds.w,
            Segmented::width(&mut engine, &labels),
            "width() is the laid-out width"
        );
    }

    fn painted(focused: Option<bool>) -> Scene {
        let mut engine = TextEngine::new();
        let labels = ["Light", "Dark"];
        let control = Segmented::layout(&mut engine, &labels, 4.0, 4.0);
        let mut canvas = Scene::new();
        let root = Affine::IDENTITY;
        match focused {
            None => control.draw(&mut canvas, root, &mut engine, &labels, Some(0), None, 1.0),
            Some(focused) => control.draw_focused(
                &mut canvas,
                root,
                &mut engine,
                &labels,
                Some(0),
                None,
                focused,
                1.0,
            ),
        }
        canvas
    }

    /// `draw` is `draw_focused` without the ring, path for path, so no
    /// existing caller changed; the ring adds one stroke and moves nothing.
    #[test]
    fn the_focus_ring_is_one_stroke_more_and_only_when_asked_for() {
        let (plain, unfocused, focused) =
            (painted(None), painted(Some(false)), painted(Some(true)));
        let (a, b, c) = (plain.encoding(), unfocused.encoding(), focused.encoding());
        assert!(a.path_tags == b.path_tags);
        assert_eq!(a.path_data, b.path_data);
        assert!(a.draw_tags == b.draw_tags);
        assert_eq!(a.draw_data, b.draw_data);
        assert_eq!(a.styles, b.styles);
        assert_eq!(c.draw_tags.len(), a.draw_tags.len() + 1);
        assert!(c.path_data.len() > a.path_data.len());
    }

    #[test]
    fn a_press_finds_its_segment() {
        let mut engine = TextEngine::new();
        let control = Segmented::layout(&mut engine, &["Light", "Dark"], 0.0, 0.0);
        let (x, y) = control.segments[1].centre();
        assert_eq!(control.segment_at(x, y), Some(1));
        assert_eq!(control.segment_at(-5.0, y), None);
    }
}
