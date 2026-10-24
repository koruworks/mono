// SPDX-License-Identifier: MPL-2.0

//! The floor: one chip for where and who, at the foot of the sidebar
//! (`docs/features/workspace-foundation/TECHNICAL.final.md` §5.4).
//!
//! ```text
//! ────────────────────────────────  hairline
//!  (AL) Acme Labs                 ⌄
//!       Steven Liechti · Syncing
//! ```
//!
//! The chip is one control that opens one popover — the two-page who/where
//! switcher the pane already has — anchored **above** it. The `?` that sat
//! beside it moved into that popover's toolbar (the CTO's artboard,
//! QUESTIONS.md E30), so the chip has the band to itself. It hides with the
//! pane (frame 9): who/where stays one chord away, and the floor needs no
//! chrome of its own in the caption.
//!
//! Geometry only, from the pane's rect; drawing and hit-testing read one
//! function and the tests hit every rect back to itself.

use koru_draw::geometry::Rect;
use koru_draw::theme;

/// The band's height, with the hairline on its top edge.
pub const H: f32 = 48.0;
/// The chip inside it.
pub const CHIP_H: f32 = 34.0;
/// The workspace's tile at the chip's left.
pub const AVATAR: f32 = 24.0;
/// The chevron at the chip's right.
pub const CHEVRON: f32 = 14.0;
/// Inside the band and inside the chip.
const PAD: f32 = 6.0;
const CHIP_PAD: f32 = 7.0;
const GAP: f32 = 8.0;

/// Every rect in the floor. Zero-width in a collapsed column, the idiom the
/// rest of the chrome uses, so a hit test must exclude them explicitly.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Rects {
    pub band: Rect,
    pub chip: Rect,
    pub avatar: Rect,
    /// The two text lines: the workspace's name over the person's.
    pub text: Rect,
    pub chevron: Rect,
}

/// Lay the floor out at the foot of `area`.
pub fn solve(area: Rect) -> Rects {
    let band = Rect::new(area.x, area.y + area.h - H, area.w.max(0.0), H);
    if area.w <= 0.0 {
        let z = Rect::new(area.x, band.y, 0.0, 0.0);
        return Rects {
            band: Rect::new(area.x, band.y, 0.0, H),
            chip: z,
            avatar: z,
            text: z,
            chevron: z,
        };
    }
    let right = band.x + band.w;
    let chip_x = band.x + PAD;
    let chip_w = (right - PAD - chip_x).max(0.0);
    let chip = Rect::new(chip_x, band.y + (H - CHIP_H) * 0.5, chip_w, CHIP_H);
    let avatar = Rect::new(
        chip.x + CHIP_PAD,
        chip.y + (CHIP_H - AVATAR) * 0.5,
        AVATAR.min(chip.w),
        AVATAR,
    );
    let chevron = Rect::new(
        (chip.x + chip.w - CHIP_PAD - CHEVRON).max(avatar.x + avatar.w),
        chip.y,
        CHEVRON.min(chip.w),
        chip.h,
    );
    let text_x = avatar.x + avatar.w + GAP;
    let text = Rect::new(
        text_x,
        chip.y,
        (chevron.x - theme::NAV_PAD * 0.5 - text_x).max(0.0),
        chip.h,
    );
    Rects {
        band,
        chip,
        avatar,
        text,
        chevron,
    }
}

/// What is under a point in the floor.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Hit {
    Chip,
}

/// Resolve a point. `None` is the band's empty part, which is the pane's.
pub fn hit(area: Rect, x: f32, y: f32) -> Option<Hit> {
    let r = solve(area);
    let owns = |rect: Rect| rect.w > 0.0 && rect.contains(x, y);
    if owns(r.chip) { Some(Hit::Chip) } else { None }
}

#[cfg(test)]
mod tests;
