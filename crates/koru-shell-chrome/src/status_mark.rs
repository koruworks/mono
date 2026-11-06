// SPDX-License-Identifier: MPL-2.0

//! **A small status mark** — the spinning ring a running chat wears, the
//! question mark that says *this is waiting on your answer*, and the dot
//! that says *something new*, drawn the same way on a tab and at the end of
//! a Chats row (koru-ai D83).
//!
//! Spelled in colours and a phase, never in a chat's words: what a mark
//! *means* is the host's. The ring's turn is read off one process clock
//! ([`spin_phase`]), so every ring on screen turns together, and a host asks
//! for a frame no oftener than [`FRAME`] while a ring is showing — never
//! while none is. Under reduced motion a host passes no phase and the ring
//! holds still.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme};
use vello::Scene;
use vello::kurbo::{Affine, Arc, Point, Stroke, Vec2};
use vello::peniko::Color;

/// How often a turning ring is redrawn: about thirty frames a second, which
/// a 14 px ring needs and nothing smaller does.
pub const FRAME: Duration = Duration::from_millis(33);

/// One turn of the ring.
const TURN_MS: f32 = 1_000.0;

/// What a mark shows.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum StatusMark {
    /// Something is running. `phase` is how far round the ring is, `0..1`;
    /// `None` holds it still (reduced motion).
    Spinner { phase: Option<f32>, color: Color },
    /// The question glyph: the chat stopped to ask the person something —
    /// a question, an approval, a permission. Like the ring it takes the
    /// tab icon's place, because it is what the person can act on.
    Question(Color),
    /// A dot: something arrived unseen.
    Dot(Color),
}

impl StatusMark {
    /// Whether this mark turns, and so wants frames.
    pub fn animating(&self) -> bool {
        matches!(self, Self::Spinner { phase: Some(_), .. })
    }
}

/// How far round a ring is at `now`, `0..1`, on the one clock every ring in
/// the process shares.
pub fn spin_phase(now: Instant) -> f32 {
    static START: OnceLock<Instant> = OnceLock::new();
    let start = *START.get_or_init(|| now);
    let ms = now.saturating_duration_since(start).as_secs_f32() * 1_000.0;
    (ms / TURN_MS).fract()
}

/// The mark, centred in `slot` — a Chats row's end, or a tab's icon slot
/// in place of the icon. A ring and the question glyph are as wide as a
/// 14 px icon; a dot is 7.
pub fn draw(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    slot: Rect,
    mark: StatusMark,
) {
    let cx = slot.x + slot.w * 0.5;
    let cy = slot.y + slot.h * 0.5;
    match mark {
        StatusMark::Spinner { phase, color } => {
            let r = (slot.w.min(slot.h) * 0.5 - 1.0).clamp(2.0, 6.0);
            draw_ring(scene, root, cx, cy, r, phase.unwrap_or(0.0), color);
        }
        StatusMark::Question(color) => {
            let glyph = engine.icon_layout(icons::QUESTION, theme::CAPTION_ICON);
            scene::draw_layout(
                scene,
                root,
                &glyph,
                cx - glyph.width() * 0.5,
                cy - glyph.height() * 0.5,
                Some(color),
            );
        }
        StatusMark::Dot(color) => {
            scene::fill(scene, root, color, &scene::circle(cx, cy, 3.5));
        }
    }
}

/// A dot over an icon's top end corner — the tab's way of saying *needs
/// you* or *something new* without hiding whose chat it is. `icon` is the
/// square the icon is drawn in.
pub fn draw_badge(scene: &mut Scene, root: Affine, icon: Rect, color: Color) {
    let r = 3.0;
    scene::fill(
        scene,
        root,
        color,
        &scene::circle(icon.x + icon.w - r * 0.5, icon.y + r * 0.5, r),
    );
}

/// The ring: a faint full circle and a quarter-and-a-bit of it in full
/// colour, turned by `phase`.
fn draw_ring(scene: &mut Scene, root: Affine, cx: f32, cy: f32, r: f32, phase: f32, color: Color) {
    let width = 1.6;
    scene::stroke(
        scene,
        root,
        color.multiply_alpha(0.25),
        width,
        &scene::circle(cx, cy, r),
    );
    let start = (phase as f64) * std::f64::consts::TAU - std::f64::consts::FRAC_PI_2;
    let arc = Arc::new(
        Point::new(cx as f64, cy as f64),
        Vec2::new(r as f64, r as f64),
        start,
        std::f64::consts::FRAC_PI_2 * 1.2,
        0.0,
    );
    scene.stroke(
        &Stroke::new(width).with_caps(vello::kurbo::Cap::Round),
        root,
        color,
        None,
        &arc,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One clock for every ring, so two drawn in the same frame agree, and a
    /// turn is a second.
    #[test]
    fn the_phase_turns_once_a_second_on_one_clock() {
        let now = Instant::now();
        let a = spin_phase(now);
        let b = spin_phase(now);
        assert_eq!(a, b);
        let later = spin_phase(now + Duration::from_millis(250));
        let turned = (later - a).rem_euclid(1.0);
        assert!((turned - 0.25).abs() < 0.01, "{turned}");
    }

    /// A still ring and a dot ask for no frames.
    #[test]
    fn only_a_turning_ring_animates() {
        let c = Color::from_rgb8(0, 0, 0);
        assert!(
            StatusMark::Spinner {
                phase: Some(0.5),
                color: c
            }
            .animating()
        );
        assert!(
            !StatusMark::Spinner {
                phase: None,
                color: c
            }
            .animating()
        );
        assert!(!StatusMark::Dot(c).animating());
        assert!(!StatusMark::Question(c).animating());
    }
}
