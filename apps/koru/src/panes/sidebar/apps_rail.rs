// SPDX-License-Identifier: MPL-2.0

//! The rail: the apps down the sidebar's left edge, under the lane, and at
//! its foot Help, Settings and the profile square (the review's artboard 11,
//! QUESTIONS.md E32). Settings sits on the profile square: both are about
//! the person, Help is about Koru.
//!
//! ```text
//! ┌────┬──────────────────
//! │ ⌂  │  nav…
//! │ ▤  │
//! │ ☑  │
//! │    │
//! │ ?  │  Help
//! │ ⚙  │  Settings
//! │(S) │  the profile's tile — opens the who/where popover
//! └────┴──────────────────
//! ```
//!
//! Geometry only, from the rail's rect; drawing and hit-testing read one
//! function and the tests hit every rect back to itself. The rail replaces
//! the apps row that ran under the lane across the pane.

use koru_draw::geometry::Rect;

use crate::panes::explore::NavigatorMode;

/// An app's square, and the squares at the foot.
pub const SQUARE: f32 = 36.0;
/// Between squares.
pub const GAP: f32 = 4.0;
/// Above the first app — and above the pane's heading band beside it, which
/// is the square's height, so the app's name reads level with Home (E72).
pub const PAD_TOP: f32 = 10.0;
/// Under the profile square: what centres the square — and so the Profile's
/// tile in it — on the floor chip's tile beside it, so the two tiles share
/// a baseline (the review's note). The chip's tile is centred in the floor's
/// band; the square sits that band's half-height above the foot.
const PAD_BOTTOM: f32 = (super::floor::H - SQUARE) * 0.5;
/// The glyph inside a square.
pub const GLYPH: f32 = 18.0;
/// The profile's tile inside its square: **the floor chip's workspace tile's
/// size**, so the two tiles beside each other read as one kind of thing (the
/// review's note).
pub const PROFILE_TILE: f32 = super::floor::AVATAR;

/// Every rect in the rail. Zero-width in a collapsed column, the idiom the
/// rest of the chrome uses, so a hit test must exclude them explicitly.
#[derive(Clone, PartialEq, Debug)]
pub struct Rects {
    pub band: Rect,
    /// One square per app the build shows, in the person's order
    /// (`rail_order::RailOrder::visible`).
    /// Nothing under the last: the *Manage apps* `+` is out until the
    /// manager lands (the review's note, E68).
    pub apps: Vec<(NavigatorMode, Rect)>,
    pub settings: Rect,
    pub help: Rect,
    /// The profile square at the foot.
    pub profile: Rect,
}

/// Lay the rail out in `rail`: the band under the lane, `W` wide.
pub fn solve(rail: Rect, apps: &[NavigatorMode]) -> Rects {
    let band = Rect::new(rail.x, rail.y, rail.w.max(0.0), rail.h.max(0.0));
    let zero = |y: f32| Rect::new(band.x, y, 0.0, 0.0);
    if band.w <= 0.0 {
        return Rects {
            band,
            apps: apps.iter().map(|m| (*m, zero(band.y))).collect(),
            settings: zero(band.y),
            help: zero(band.y),
            profile: zero(band.y),
        };
    }
    let side = SQUARE.min(band.w);
    let x = band.x + (band.w - side) * 0.5;
    let mut y = band.y + PAD_TOP;
    let place = |y: &mut f32| {
        let r = Rect::new(x, *y, side, side);
        *y += side + GAP;
        r
    };
    let apps = apps.iter().map(|m| (*m, place(&mut y))).collect();
    // The foot, bottom up: the profile square on the band's floor,
    // Settings over it, Help over that.
    let foot = band.y + band.h - PAD_BOTTOM;
    let profile = Rect::new(x, foot - side, side, side);
    let settings = Rect::new(x, profile.y - GAP - side, side, side);
    let help = Rect::new(x, settings.y - GAP - side, side, side);
    Rects {
        band,
        apps,
        settings,
        help,
        profile,
    }
}

/// What is under a point in the rail.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Hit {
    App(NavigatorMode),
    Settings,
    Help,
    Profile,
}

/// Resolve a point. `None` is the band's empty part.
pub fn hit(r: &Rects, x: f32, y: f32) -> Option<Hit> {
    let owns = |rect: Rect| rect.w > 0.0 && rect.contains(x, y);
    if let Some((mode, _)) = r.apps.iter().find(|(_, rect)| owns(*rect)) {
        return Some(Hit::App(*mode));
    }
    if owns(r.settings) {
        Some(Hit::Settings)
    } else if owns(r.help) {
        Some(Hit::Help)
    } else if owns(r.profile) {
        Some(Hit::Profile)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_draw::theme;

    fn rail() -> Rect {
        Rect::new(
            0.0,
            theme::CAPTION_H,
            theme::RAIL_W,
            600.0 - theme::CAPTION_H,
        )
    }

    /// Apps from the top, the foot from the bottom, every square inside the
    /// band and none overlapping.
    #[test]
    fn the_rail_stacks_apps_from_the_top_and_the_foot_from_the_bottom() {
        let modes = NavigatorMode::visible();
        let r = solve(rail(), &modes);
        assert_eq!(r.apps.len(), modes.len());
        assert_eq!(r.apps[0].1.y, r.band.y + PAD_TOP);
        for pair in r.apps.windows(2) {
            assert_eq!(pair[1].1.y, pair[0].1.y + SQUARE + GAP);
        }
        let last = r.apps.last().unwrap().1;
        assert_eq!(r.profile.y + r.profile.h, r.band.y + r.band.h - PAD_BOTTOM);
        assert!(r.settings.y + r.settings.h <= r.profile.y);
        assert!(r.help.y + r.help.h <= r.settings.y);
        assert!(last.y + last.h <= r.help.y, "room between");
        for rect in r
            .apps
            .iter()
            .map(|(_, r)| *r)
            .chain([r.settings, r.help, r.profile])
        {
            assert_eq!(rect.w, SQUARE);
            assert!(rect.x >= r.band.x && rect.x + rect.w <= r.band.x + r.band.w);
        }
    }

    #[test]
    fn every_rail_control_hit_tests_back_to_itself() {
        let modes = NavigatorMode::visible();
        let r = solve(rail(), &modes);
        for (mode, rect) in &r.apps {
            let (x, y) = rect.centre();
            assert_eq!(hit(&r, x, y), Some(Hit::App(*mode)));
        }
        for (rect, want) in [
            (r.settings, Hit::Settings),
            (r.help, Hit::Help),
            (r.profile, Hit::Profile),
        ] {
            let (x, y) = rect.centre();
            assert_eq!(hit(&r, x, y), Some(want));
        }
        let last = r.apps.last().unwrap().1;
        let gap_y = (last.y + last.h + r.settings.y) * 0.5;
        assert_eq!(hit(&r, r.band.x + 2.0, gap_y), None, "the gap is nobody's");
    }

    #[test]
    fn a_shut_pane_has_no_rail() {
        let shut = Rect::new(0.0, theme::CAPTION_H, 0.0, 500.0);
        let r = solve(shut, &NavigatorMode::visible());
        assert!(r.apps.iter().all(|(_, rect)| rect.w == 0.0));
        for rect in [r.settings, r.help, r.profile] {
            assert_eq!(rect.w, 0.0);
        }
        assert_eq!(hit(&r, 0.0, 300.0), None);
    }
}
