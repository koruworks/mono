// SPDX-License-Identifier: MPL-2.0

//! A reusable determinate or indeterminate progress bar.
//!
//! The component owns only its animation clock and progress mode. Its caller
//! chooses the rectangle, so the same bar can live in a toast, dialog, or full
//! page without inheriting any of their layout policy.

use std::time::Instant;

use koru_draw::geometry::Rect;
use koru_draw::{anim, scene, theme};
use vello::Scene;
use vello::kurbo::Affine;

/// What the bar can say about the work it represents.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum ProgressMode {
    /// Work is happening, but its total cannot be measured yet.
    Indeterminate,
    /// A fraction from zero through one. Values outside that range are clamped.
    Determinate(f32),
}

pub struct ProgressBar {
    mode: ProgressMode,
    started: Instant,
}

impl ProgressBar {
    pub fn new(mode: ProgressMode, now: Instant) -> Self {
        Self {
            mode: normalized(mode),
            started: now,
        }
    }

    pub fn mode(&self) -> ProgressMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: ProgressMode, now: Instant) {
        let mode = normalized(mode);
        if self.mode != mode {
            self.mode = mode;
            self.started = now;
        }
    }

    /// Only an indeterminate bar moves without an external update.
    pub fn animating(&self) -> bool {
        self.mode == ProgressMode::Indeterminate
    }

    pub fn draw(&self, canvas: &mut Scene, root: Affine, rect: Rect, now: Instant, alpha: f32) {
        let p = theme::p();
        scene::fill(
            canvas,
            root,
            p.panel_border.multiply_alpha(alpha),
            &scene::rrect(rect.x, rect.y, rect.w, rect.h, rect.h * 0.5),
        );
        let fill = match self.mode {
            ProgressMode::Determinate(fraction) => {
                Rect::new(rect.x, rect.y, rect.w * fraction, rect.h)
            }
            ProgressMode::Indeterminate => {
                let segment = rect.w * 0.32;
                let travel = rect.w + segment;
                let phase = anim::loop_phase(self.started, now, theme::PULSE_LOOP_MS);
                let left = (rect.x - segment + travel * phase).max(rect.x);
                let right = (rect.x - segment + travel * phase + segment).min(rect.x + rect.w);
                Rect::new(left, rect.y, (right - left).max(0.0), rect.h)
            }
        };
        if fill.w > 0.0 {
            scene::fill(
                canvas,
                root,
                p.accent.multiply_alpha(alpha),
                &scene::rrect(fill.x, fill.y, fill.w, fill.h, fill.h * 0.5),
            );
        }
    }
}

fn normalized(mode: ProgressMode) -> ProgressMode {
    match mode {
        ProgressMode::Determinate(fraction) if fraction.is_finite() => {
            ProgressMode::Determinate(fraction.clamp(0.0, 1.0))
        }
        ProgressMode::Determinate(_) => ProgressMode::Determinate(0.0),
        ProgressMode::Indeterminate => ProgressMode::Indeterminate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_indeterminate_progress_animates() {
        let now = Instant::now();
        assert!(ProgressBar::new(ProgressMode::Indeterminate, now).animating());
        assert!(!ProgressBar::new(ProgressMode::Determinate(0.5), now).animating());
    }

    #[test]
    fn mode_updates_are_observable() {
        let now = Instant::now();
        let mut bar = ProgressBar::new(ProgressMode::Indeterminate, now);
        bar.set_mode(ProgressMode::Determinate(0.25), now);
        assert_eq!(bar.mode(), ProgressMode::Determinate(0.25));
    }

    #[test]
    fn determinate_values_are_safe_for_geometry() {
        let now = Instant::now();
        assert_eq!(
            ProgressBar::new(ProgressMode::Determinate(2.0), now).mode(),
            ProgressMode::Determinate(1.0)
        );
        assert_eq!(
            ProgressBar::new(ProgressMode::Determinate(f32::NAN), now).mode(),
            ProgressMode::Determinate(0.0)
        );
    }
}
