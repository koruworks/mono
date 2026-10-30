// SPDX-License-Identifier: MPL-2.0

//! Rectangles, and the column arithmetic every view shares.
//!
//! One place, because chrome geometry that exists twice drifts: a button ends up
//! clickable somewhere other than where it is drawn. `DESIGN.md` states the rule;
//! this is where the shared half of it lives.

use crate::theme;

/// A rectangle in logical pixels, positioned from its top-left.
///
/// Replaces the bare `(f32, f32, f32, f32)` that chrome used to pass around.
/// Four unlabelled floats have no way to say which is width and which is `y`,
/// and every caller had to re-destructure them to find out.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Hit-testing, inclusive on every edge. A point exactly on a boundary
    /// belongs to the rect: chrome is small, and a click that lands on the edge
    /// of a button meant to press it.
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }

    // Used by the click round-trip tests, and by chrome as it moves in.
    #[allow(dead_code)]
    // Used by the click round-trip tests, and by chrome as it moves in.
    #[allow(dead_code)]
    /// The middle of the rect — what a test clicks to hit it.
    pub fn centre(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }

    /// The smallest rect covering both — a composed control's outer boundary.
    pub fn union(&self, other: Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect::new(
            x,
            y,
            (self.x + self.w).max(other.x + other.w) - x,
            (self.y + self.h).max(other.y + other.h) - y,
        )
    }

    /// Whether two rects share any area.
    ///
    /// Touching edges do not count: a rect ending exactly where the next begins
    /// is beside it, not over it.
    ///
    /// **An empty rect intersects nothing**, including a rect it sits inside.
    /// That is not a degenerate case here — chrome says "not there" with a
    /// 0-width rect (a hidden caption toggle, a collapsed column) precisely so
    /// the room is reclaimed with no second code path, and the edge comparisons
    /// alone would report such a rect as covering whatever it was parked in.
    pub fn intersects(&self, other: Rect) -> bool {
        self.w > 0.0
            && self.h > 0.0
            && other.w > 0.0
            && other.h > 0.0
            && self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }
}

/// Horizontal offset of a row's text column, in logical pixels.
///
/// The gutter **is** the checkbox's column, so a row that draws no checkbox does
/// not reserve one — a document's prose starts at the margin, where its images
/// already do. `checkbox` is the same per-row gate that decides whether the box
/// is painted at all, not a fact about which view is drawing: a task list inside
/// a document keeps its text one gutter right of its boxes.
pub fn text_x(depth: impl Into<u32>, checkbox: bool) -> f32 {
    text_x_in(&theme::DocMetrics::BASE, depth, checkbox)
}

/// [`text_x`] at a document's text scale: the gutter grows with the checkbox,
/// the column it starts from does not.
pub fn text_x_in(metrics: &theme::DocMetrics, depth: impl Into<u32>, checkbox: bool) -> f32 {
    let gutter = if checkbox { metrics.gutter } else { 0.0 };
    checkbox_x(depth) + gutter
}

/// Horizontal offset of a row's checkbox, in logical pixels.
///
/// [`AFFORDANCE_W`](theme::AFFORDANCE_W) is part of it: the `+` and the drag
/// grip hang in that lane, left of the checkbox column, and the column has to
/// start clear of them rather than on top of them.
pub fn checkbox_x(depth: impl Into<u32>) -> f32 {
    theme::MARGIN_X + theme::AFFORDANCE_W + depth.into() as f32 * theme::INDENT_W
}

/// The right edge a list marker is aligned to: just inside the text column, one
/// [`MARKER_GAP`](theme::MARKER_GAP) left of where the text begins. A marker
/// hangs in the gutter the way a number hangs under a hanging indent, so a
/// wrapped second line does not tuck under it.
pub fn marker_right_x(depth: impl Into<u32>) -> f32 {
    marker_right_x_in(&theme::DocMetrics::BASE, depth)
}

/// [`marker_right_x`] at a document's text scale.
pub fn marker_right_x_in(metrics: &theme::DocMetrics, depth: impl Into<u32>) -> f32 {
    text_x_in(metrics, depth, true) - metrics.marker_gap
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inclusive edges, deliberately — stated here so nobody "fixes" it into
    /// exclusive and makes the right-hand pixel of every button dead.
    #[test]
    fn a_point_on_the_boundary_is_inside() {
        let r = Rect::new(10.0, 20.0, 30.0, 40.0);
        assert!(r.contains(10.0, 20.0), "top-left corner");
        assert!(r.contains(40.0, 60.0), "bottom-right corner");
        assert!(r.contains(25.0, 40.0), "middle");
        assert!(!r.contains(9.9, 40.0));
        assert!(!r.contains(25.0, 60.1));
    }

    #[test]
    fn the_centre_hits_back_inside() {
        let r = Rect::new(3.0, 7.0, 11.0, 13.0);
        let (cx, cy) = r.centre();
        assert!(r.contains(cx, cy));
    }

    /// Depth shifts the text column but not the checkbox gutter's own start,
    /// and a row that has a checkbox sits one gutter to the right of it.
    #[test]
    fn depth_shifts_both_columns_together() {
        assert_eq!(text_x(0u16, true) - checkbox_x(0u16), theme::GUTTER);
        assert_eq!(text_x(2u16, true) - text_x(1u16, true), theme::INDENT_W);
        assert_eq!(checkbox_x(0u16), theme::MARGIN_X + theme::AFFORDANCE_W);
    }

    /// The affordance lane sits before the checkbox column, and the two
    /// affordances fit in front of it without running off the left edge.
    ///
    /// The lane overhangs into `MARGIN_X` on purpose — the `+` and the grip
    /// hang in the page margin, which is what a margin is for — so the budget
    /// is `MARGIN_X + AFFORDANCE_W`, not `AFFORDANCE_W` alone. Where they
    /// actually land is `rowlayout::gutter_affordances`, which has the
    /// round-trip.
    #[test]
    fn the_affordance_lane_sits_between_the_margin_and_the_checkbox() {
        assert_eq!(checkbox_x(0u16) - theme::MARGIN_X, theme::AFFORDANCE_W);
        assert!(
            theme::MARGIN_X + theme::AFFORDANCE_W >= theme::GRIP_W + theme::AFFORDANCE_BTN + 8.0,
            "the grip, the `+` and the gaps between them fit before the text"
        );
    }

    /// A row with no checkbox does not reserve its column: prose begins where
    /// the affordance lane ends, and indenting it still moves it one level at
    /// a time.
    #[test]
    fn a_row_without_a_checkbox_closes_the_gutter() {
        assert_eq!(text_x(0u16, false), theme::MARGIN_X + theme::AFFORDANCE_W);
        assert_eq!(text_x(0u16, true) - text_x(0u16, false), theme::GUTTER);
        assert_eq!(text_x(2u16, false) - text_x(1u16, false), theme::INDENT_W);
    }

    /// A list marker hangs just inside the text column, so it stays clear of the
    /// glyphs and still travels with the indent.
    #[test]
    fn a_list_marker_sits_just_left_of_the_text() {
        assert!(marker_right_x(0u16) < text_x(0u16, true));
        assert_eq!(text_x(0u16, true) - marker_right_x(0u16), theme::MARKER_GAP);
        assert_eq!(marker_right_x(2u16) - marker_right_x(1u16), theme::INDENT_W);
    }

    /// Touching edges are not an overlap. The chrome uses 0-width and 0-height
    /// rects to mean "not there" — a hidden caption toggle, a collapsed column —
    /// and one of those must never read as covering anything.
    #[test]
    fn rects_that_only_touch_do_not_intersect() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(a.intersects(Rect::new(5.0, 5.0, 10.0, 10.0)), "overlapping");
        assert!(a.intersects(Rect::new(-1.0, -1.0, 20.0, 20.0)), "enclosing");
        assert!(
            !a.intersects(Rect::new(10.0, 0.0, 10.0, 10.0)),
            "edge to edge"
        );
        assert!(!a.intersects(Rect::new(0.0, 10.0, 10.0, 10.0)), "stacked");
        assert!(!a.intersects(Rect::new(20.0, 20.0, 5.0, 5.0)), "apart");
        assert!(
            !a.intersects(Rect::new(5.0, 5.0, 0.0, 0.0)),
            "an absent rect"
        );
    }

    /// It is symmetric, which is the property every caller assumes and none
    /// states.
    #[test]
    fn intersects_does_not_care_which_side_asks() {
        let a = Rect::new(2.0, 3.0, 8.0, 4.0);
        for b in [
            Rect::new(0.0, 0.0, 5.0, 5.0),
            Rect::new(100.0, 0.0, 5.0, 5.0),
            Rect::new(2.0, 3.0, 8.0, 4.0),
        ] {
            assert_eq!(a.intersects(b), b.intersects(a), "{a:?} vs {b:?}");
        }
    }
}
