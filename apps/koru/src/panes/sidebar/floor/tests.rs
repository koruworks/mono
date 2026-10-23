// SPDX-License-Identifier: MPL-2.0

use super::*;

fn area() -> Rect {
    Rect::new(0.0, 0.0, theme::SIDEBAR_W, 600.0)
}

/// The band is the foot of the pane; everything in it is inside it, in
/// order: avatar, text, chevron inside the chip, then the `?`.
#[test]
fn the_floor_sits_at_the_foot_and_its_parts_nest_in_order() {
    let a = area();
    let r = solve(a);
    assert_eq!(r.band.y + r.band.h, a.y + a.h);
    assert_eq!(r.band.h, H);
    assert_eq!(r.band.w, a.w);
    for rect in [r.chip, r.help] {
        assert!(rect.y >= r.band.y && rect.y + rect.h <= r.band.y + r.band.h);
        assert!(rect.x >= r.band.x && rect.x + rect.w <= r.band.x + r.band.w);
    }
    assert!(
        r.chip.x + r.chip.w <= r.help.x,
        "the chip ends before the ?"
    );
    assert!(r.avatar.x >= r.chip.x);
    assert!(r.text.x >= r.avatar.x + r.avatar.w);
    assert!(r.text.x + r.text.w <= r.chevron.x);
    assert!(r.chevron.x + r.chevron.w <= r.chip.x + r.chip.w);
    assert_eq!(r.avatar.w, AVATAR);
    assert_eq!(r.help.w, HELP);
}

/// Every control hit-tests back to itself; the band's empty part is nobody's.
#[test]
fn every_floor_control_hit_tests_back_to_itself() {
    let a = area();
    let r = solve(a);
    let (x, y) = r.chip.centre();
    assert_eq!(hit(a, x, y), Some(Hit::Chip));
    let (x, y) = r.avatar.centre();
    assert_eq!(
        hit(a, x, y),
        Some(Hit::Chip),
        "the avatar is part of the chip"
    );
    let (x, y) = r.help.centre();
    assert_eq!(hit(a, x, y), Some(Hit::Help));
    assert_eq!(
        hit(a, r.band.x + 1.0, r.band.y + 1.0),
        None,
        "the band's edge is nobody's"
    );
    assert_eq!(
        hit(a, x, r.band.y - 5.0),
        None,
        "above the band is not the floor"
    );
}

/// A collapsed column has no floor to hit, and nothing goes negative.
#[test]
fn a_shut_pane_has_no_floor() {
    let shut = Rect::new(0.0, 0.0, 0.0, 600.0);
    let r = solve(shut);
    for rect in [r.chip, r.avatar, r.text, r.chevron, r.help] {
        assert_eq!(rect.w, 0.0);
    }
    assert_eq!(hit(shut, 0.0, 580.0), None);
    let narrow = Rect::new(0.0, 0.0, 40.0, 600.0);
    let r = solve(narrow);
    assert!(r.chip.w >= 0.0 && r.text.w >= 0.0 && r.help.w <= 40.0);
}
