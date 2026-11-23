// SPDX-License-Identifier: MPL-2.0

//! A stepper: a value between a *less* and a *more* button.
//!
//! **For an ordered scale with a sensible middle** — a zoom, a size — where the
//! person nudges rather than picks, and six chips would show six values when
//! one is in force. The readout is never pressable: a stepper whose number is
//! also a button has three things to click and no way to tell them apart.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{inspect, scene, theme};
use vello::Scene;
use vello::kurbo::Affine;

/// A measured stepper.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Stepper {
    pub bounds: Rect,
    pub less: Rect,
    pub more: Rect,
    pub readout: Rect,
}

/// Which button.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Less,
    More,
}

impl Stepper {
    pub const H: f32 = 30.0;
    const BUTTON: f32 = 30.0;
    const READOUT_PAD: f32 = 10.0;
    const RADIUS: f32 = 7.0;

    /// Lay the stepper out at (`x`, `y`) with a readout wide enough for
    /// `widest` — the longest value it will ever show, so it never resizes.
    pub fn layout(engine: &mut TextEngine, widest: &str, x: f32, y: f32) -> Self {
        let readout_w =
            engine.simple_layout(widest, theme::role::row()).width() + Self::READOUT_PAD * 2.0;
        let less = Rect::new(x, y, Self::BUTTON, Self::H);
        let readout = Rect::new(x + Self::BUTTON, y, readout_w, Self::H);
        let more = Rect::new(readout.x + readout.w, y, Self::BUTTON, Self::H);
        Self {
            bounds: Rect::new(x, y, Self::BUTTON * 2.0 + readout_w, Self::H),
            less,
            more,
            readout,
        }
    }

    pub fn width(engine: &mut TextEngine, widest: &str) -> f32 {
        Self::layout(engine, widest, 0.0, 0.0).bounds.w
    }

    pub fn step_at(&self, x: f32, y: f32) -> Option<Step> {
        if self.less.contains(x, y) {
            Some(Step::Less)
        } else if self.more.contains(x, y) {
            Some(Step::More)
        } else {
            None
        }
    }

    /// `can_less` / `can_more` dim a button at the end of the scale.
    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    pub fn draw(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        value: &str,
        can_less: bool,
        can_more: bool,
        hovered: Option<Step>,
        alpha: f32,
    ) {
        let b = self.bounds;
        let _component = inspect::scope(
            "KoruStepper",
            "component",
            root,
            scene::rect(b.x, b.y, b.w, b.h),
            |r| {
                r.property(
                    "implementation",
                    "koru_ui::stepper::Stepper",
                    "shared implementation",
                );
                r.property("value", value, "caller value");
                r.property(
                    "less / more enabled",
                    format!("{can_less} / {can_more}"),
                    "caller state",
                );
                r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
            },
        );
        let p = theme::p();
        let shape = scene::rrect(b.x, b.y, b.w, b.h, Self::RADIUS);
        scene::fill(canvas, root, p.panel_bg.multiply_alpha(alpha), &shape);
        scene::stroke(
            canvas,
            root,
            p.panel_border.multiply_alpha(alpha),
            theme::BORDER_W,
            &shape,
        );
        for (rect, step, enabled, glyph, name) in [
            (self.less, Step::Less, can_less, "\u{2212}", "Stepper less"),
            (self.more, Step::More, can_more, "+", "Stepper more"),
        ] {
            let _part = inspect::scope(
                name,
                "part",
                root,
                scene::rect(rect.x, rect.y, rect.w, rect.h),
                |r| r.property("enabled", enabled, "caller state"),
            );
            if enabled && hovered == Some(step) {
                scene::fill(
                    canvas,
                    root,
                    p.hover_tint.multiply_alpha(alpha),
                    &scene::rrect(rect.x + 2.0, rect.y + 2.0, rect.w - 4.0, rect.h - 4.0, 5.0),
                );
            }
            let layout = engine.simple_layout(glyph, theme::role::ui());
            let tone = if enabled { p.text } else { p.placeholder };
            scene::draw_layout(
                canvas,
                root,
                &layout,
                rect.x + (rect.w - layout.width()) * 0.5,
                rect.y + (rect.h - scene::first_line_height(&layout)) * 0.5,
                Some(tone.multiply_alpha(alpha)),
            );
        }
        let layout = engine.simple_layout(value, theme::role::row());
        crate::typography::line(
            "Stepper value",
            canvas,
            root,
            &layout,
            self.readout.x + (self.readout.w - layout.width()) * 0.5,
            scene::centre_y_in(&layout, self.readout),
            theme::role::row(),
            "theme::role::row()",
            p.text.multiply_alpha(alpha),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buttons_flank_a_readout_that_is_not_pressable() {
        let mut engine = TextEngine::new();
        let s = Stepper::layout(&mut engine, "150%", 10.0, 0.0);
        assert_eq!(s.less.x, 10.0);
        assert_eq!(s.more.x + s.more.w, s.bounds.x + s.bounds.w);
        let (lx, ly) = s.less.centre();
        let (mx, my) = s.more.centre();
        let (rx, ry) = s.readout.centre();
        assert_eq!(s.step_at(lx, ly), Some(Step::Less));
        assert_eq!(s.step_at(mx, my), Some(Step::More));
        assert_eq!(s.step_at(rx, ry), None);
        assert_eq!(Stepper::width(&mut engine, "150%"), s.bounds.w);
    }
}
