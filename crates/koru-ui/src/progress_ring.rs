// SPDX-License-Identifier: MPL-2.0

//! A compact, determinate progress outline around caller-owned content.

use koru_draw::geometry::Rect;
use koru_draw::{scene, theme};
use vello::Scene;
use vello::kurbo::{Affine, BezPath};

/// An absent fraction leaves the track visible without implying zero usage.
pub struct ProgressRing {
    fraction: Option<f32>,
}

impl ProgressRing {
    pub fn new(fraction: Option<f32>) -> Self {
        Self {
            fraction: fraction
                .filter(|value| value.is_finite())
                .map(|value| value.clamp(0.0, 1.0)),
        }
    }

    pub fn fraction(&self) -> Option<f32> {
        self.fraction
    }

    pub fn draw(&self, canvas: &mut Scene, root: Affine, rect: Rect) {
        let center = (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
        let radius =
            (rect.w.min(rect.h) * 0.5 - theme::Space::S0_5.px(theme::Profile::Desktop)).max(0.0);
        let width = theme::Space::S0_5.px(theme::Profile::Desktop);
        scene::stroke(
            canvas,
            root,
            theme::p().panel_border,
            f64::from(width),
            &scene::circle(center.0, center.1, radius),
        );
        if let Some(fraction) = self.fraction.filter(|value| *value > 0.0) {
            let steps = (fraction * 64.0).ceil().max(1.0) as usize;
            let mut arc = BezPath::new();
            for step in 0..=steps {
                let angle = -std::f32::consts::FRAC_PI_2
                    + std::f32::consts::TAU * fraction * step as f32 / steps as f32;
                let point = (
                    f64::from(center.0 + radius * angle.cos()),
                    f64::from(center.1 + radius * angle.sin()),
                );
                if step == 0 {
                    arc.move_to(point);
                } else {
                    arc.line_to(point);
                }
            }
            scene::stroke(canvas, root, theme::p().accent, f64::from(width), &arc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_progress_is_distinct_from_zero_and_values_are_bounded() {
        assert_eq!(ProgressRing::new(None).fraction(), None);
        assert_eq!(ProgressRing::new(Some(0.0)).fraction(), Some(0.0));
        assert_eq!(ProgressRing::new(Some(2.0)).fraction(), Some(1.0));
        assert_eq!(ProgressRing::new(Some(f32::NAN)).fraction(), None);
    }
}
