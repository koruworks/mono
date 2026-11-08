// SPDX-License-Identifier: MPL-2.0

//! Shared motion recipes: tooltip timing, and the sidebar's fold. Interaction
//! delays are distinct from animation time.
use std::time::Duration;

use crate::anim::ease_out_cubic;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tooltip {
    pub hover_delay: Duration,
    pub enter_duration: Duration,
    pub exit_duration: Duration,
    pub warm_window: Duration,
    /// Initial vertical offset in desktop logical units.
    pub rise: f32,
}

/// Current desktop recipe; consumers retain their existing easing and lifecycle.
pub const fn tooltip() -> Tooltip {
    Tooltip {
        hover_delay: Duration::from_millis(400),
        enter_duration: Duration::from_millis(100),
        exit_duration: Duration::from_millis(75),
        warm_window: Duration::from_millis(300),
        rise: 2.0,
    }
}

/// **The pane's fold** — the sidebar hiding and showing as one tween
/// (`docs/features/koruui/ANIMATION.md`, "The pane fold"). The slide is the
/// column's own ([`Panel`] in `koru-app-shell`, `PANEL_SLIDE_MS`); this
/// recipe adds what the window draws over it: the pane's contents fade out
/// over the first `fade` of a collapse and in over the last `fade` of an
/// expand, while the caption's leading chain rides the drawn pane's edge.
/// Reduced motion lands both states at once and skips the fade; the host
/// decides that, because the recipe has no clock of its own.
///
/// [`Panel`]: ../../../../koru-app-shell/src/panel.rs
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneFold {
    pub slide: Duration,
    pub fade: Duration,
}

/// The desktop recipe: `PANEL_SLIDE_MS` for the slide, `POPOVER_FADE_MS`
/// for the fade, ease-out cubic for both.
pub const fn pane_fold() -> PaneFold {
    PaneFold {
        slide: Duration::from_millis(super::PANEL_SLIDE_MS as u64),
        fade: Duration::from_millis(super::POPOVER_FADE_MS as u64),
    }
}

impl PaneFold {
    /// How opaque the pane's contents are `elapsed` into a fold. Collapsing,
    /// they go over the first `fade`; expanding, they arrive over the last.
    /// Stateless — the caller keeps the clock — so an interrupted fold reads
    /// its new direction from its own start.
    pub fn content_alpha(&self, elapsed: Duration, opening: bool) -> f32 {
        // The ends are exact, not a float's idea of them: a frame at or past
        // the end draws the resting state and nothing fractional.
        if opening && elapsed >= self.slide {
            return 1.0;
        }
        if !opening && elapsed >= self.fade {
            return 0.0;
        }
        if opening && elapsed <= self.slide.saturating_sub(self.fade) {
            return 0.0;
        }
        let fade = self.fade.as_secs_f32().max(f32::EPSILON);
        let elapsed = elapsed.as_secs_f32();
        if opening {
            let from = (self.slide.as_secs_f32() - fade).max(0.0);
            ease_out_cubic(((elapsed - from) / fade).clamp(0.0, 1.0))
        } else {
            1.0 - ease_out_cubic((elapsed / fade).clamp(0.0, 1.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fold_fades_out_first_and_in_last() {
        let fold = pane_fold();
        let ms = Duration::from_millis;
        // Collapse: gone by the end of the fade, long before the slide ends.
        assert_eq!(fold.content_alpha(ms(0), false), 1.0);
        assert!(fold.content_alpha(ms(45), false) < 1.0);
        assert_eq!(fold.content_alpha(fold.fade, false), 0.0);
        assert_eq!(fold.content_alpha(fold.slide, false), 0.0);
        // Expand: nothing until the last `fade`, fully there at the end.
        assert_eq!(fold.content_alpha(ms(0), true), 0.0);
        assert_eq!(fold.content_alpha(fold.slide - fold.fade, true), 0.0);
        assert!(fold.content_alpha(fold.slide - ms(20), true) > 0.0);
        assert_eq!(fold.content_alpha(fold.slide, true), 1.0);
        assert_eq!(fold.content_alpha(ms(10_000), true), 1.0);
    }
}
