// SPDX-License-Identifier: MPL-2.0

//! One overlay scrollbar shared by document and navigation views, and by
//! the element layer's scroll viewport (`el::scroll_y(..).scrollbar()`).
//!
//! The track is a hit target, not paint: only the rounded thumb is drawn. The
//! gutter is wider than the thumb so it remains easy to grab without making the
//! interface wear a heavier bar.
//!
//! It lives here rather than in `koru_ui` so the element layer, which
//! `koru_ui` builds on, draws the same bar; `koru_ui::scrollbar` re-exports
//! it for the controls that always used it there (QUESTIONS M1).

use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Color;

use crate::geometry::Rect;
use crate::{scene, theme};

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ScrollbarGeom {
    pub thumb_x: f32,
    pub thumb_w: f32,
    pub thumb_top: f32,
    pub thumb_h: f32,
    pub gutter_x: f32,
    pub gutter_w: f32,
    pub track_top: f32,
    pub track_bottom: f32,
    pub max_scroll: f32,
}

impl ScrollbarGeom {
    pub fn for_content(area: Rect, content_h: f32, scroll: f32) -> Option<Self> {
        let max_scroll = (content_h - area.h).max(0.0);
        if max_scroll <= 0.0 || area.h <= 0.0 || area.w <= 0.0 {
            return None;
        }
        let thumb_h = (area.h * area.h / content_h)
            .max(theme::SCROLLBAR_MIN_THUMB)
            .min(area.h);
        let travel = (area.h - thumb_h).max(0.0);
        let thumb_top = area.y + travel * (scroll / max_scroll).clamp(0.0, 1.0);
        let gutter_w = theme::SCROLLBAR_GUTTER_W.min(area.w);
        let gutter_x = (area.x + area.w - theme::SCROLLBAR_EDGE_CLEAR - gutter_w).max(area.x);
        let gutter_w = gutter_w.min(area.x + area.w - gutter_x).max(0.0);
        let thumb_w = theme::SCROLLBAR_W.min(gutter_w);
        let thumb_x = gutter_x + gutter_w - thumb_w;
        Some(Self {
            thumb_x,
            thumb_w,
            thumb_top,
            thumb_h,
            gutter_x,
            gutter_w,
            track_top: area.y,
            track_bottom: area.y + area.h,
            max_scroll,
        })
    }

    pub fn on_thumb(self, x: f32, y: f32) -> bool {
        x >= self.gutter_x
            && x <= self.gutter_x + self.gutter_w
            && y >= self.thumb_top
            && y <= self.thumb_top + self.thumb_h
    }

    pub fn in_gutter(self, x: f32, y: f32) -> bool {
        x >= self.gutter_x
            && x <= self.gutter_x + self.gutter_w
            && y >= self.track_top
            && y <= self.track_bottom
    }

    pub fn scroll_for_pointer(self, pointer_y: f32, grab_offset: f32) -> f32 {
        let travel = (self.track_bottom - self.track_top - self.thumb_h).max(0.0);
        if travel <= 0.0 {
            return 0.0;
        }
        let top = (pointer_y - self.track_top - grab_offset).clamp(0.0, travel);
        top / travel * self.max_scroll
    }

    /// Where a press in the gutter at `y` holds the thumb: where it was
    /// pressed on the thumb, else its middle (the thumb jumps there).
    pub fn grab_at(self, x: f32, y: f32) -> f32 {
        if self.on_thumb(x, y) {
            y - self.thumb_top
        } else {
            self.thumb_h * 0.5
        }
    }

    /// The gutter: where the bar is pressed and dragged.
    pub fn gutter(self) -> Rect {
        Rect::new(
            self.gutter_x,
            self.track_top,
            self.gutter_w,
            self.track_bottom - self.track_top,
        )
    }

    /// The thumb, as drawn.
    pub fn thumb(self) -> Rect {
        Rect::new(self.thumb_x, self.thumb_top, self.thumb_w, self.thumb_h)
    }
}

pub fn draw(scene: &mut Scene, root: Affine, geom: ScrollbarGeom, active: bool) {
    draw_thumb(scene, root, geom.thumb(), active);
}

/// Paint just the thumb. Views with established geometry can share the exact
/// same visual without translating their interaction state.
pub fn draw_thumb(scene: &mut Scene, root: Affine, thumb: Rect, active: bool) {
    draw_thumb_in(scene, root, thumb, theme::scrollbar_thumb_color(active));
}

/// The thumb in `colour`: what an element resolves from its own palette
/// ([`theme::scrollbar_thumb_color_in`]).
pub fn draw_thumb_in(scene: &mut Scene, root: Affine, thumb: Rect, colour: Color) {
    let radius = (thumb.w * 0.5).min(thumb.h * 0.5);
    scene::fill(
        scene,
        root,
        colour,
        &scene::rrect(thumb.x, thumb.y, thumb.w, thumb.h, radius),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_maps_scroll_and_pointer_through_the_same_thumb() {
        let area = Rect::new(20.0, 40.0, 200.0, 300.0);
        let top = ScrollbarGeom::for_content(area, 900.0, 0.0).unwrap();
        let bottom = ScrollbarGeom::for_content(area, 900.0, 600.0).unwrap();
        assert_eq!(top.thumb_top, area.y);
        assert!((bottom.thumb_top + bottom.thumb_h - (area.y + area.h)).abs() < 0.01);
        assert!((top.scroll_for_pointer(bottom.thumb_top, 0.0) - 600.0).abs() < 0.01);
        assert_eq!(top.grab_at(top.thumb_x, top.thumb_top + 5.0), 5.0);
        assert_eq!(top.grab_at(top.thumb_x, area.y + 290.0), top.thumb_h * 0.5);
    }
}
