// SPDX-License-Identifier: MPL-2.0

use super::*;

fn lane() -> Rect {
    Rect::new(0.0, 0.0, theme::SIDEBAR_W, theme::CAPTION_H)
}

fn all() -> Leading {
    Leading {
        mark: Mark::Mark,
        search: true,
        new: true,
    }
}

/// Every rect hit-tests back to itself, and the gap between the leading slot
/// and the first square is the empty lane.
#[test]
fn every_lane_rect_hit_tests_back_to_itself() {
    let l = lane();
    let r = solve(l, all());
    let (x, y) = r.toggle.centre();
    assert_eq!(hit(l, all(), x, y), Some(Hit::Toggle));
    let (x, y) = r.new.centre();
    assert_eq!(hit(l, all(), x, y), Some(Hit::New));
    let (x, y) = r.search.centre();
    assert_eq!(hit(l, all(), x, y), Some(Hit::Search));
    let (x, y) = r.leading.centre();
    assert_eq!(hit(l, all(), x, y), None, "the mark is not a verb");
    let gap_x = (r.leading.x + r.leading.w + r.search.x) * 0.5;
    assert_eq!(hit(l, all(), gap_x, y), None, "the gap drags the window");
}

/// Squares pack against the right edge in the order search · new · toggle,
/// each the band's height, and the toggle owns the corner.
#[test]
fn the_squares_pack_against_the_right_edge_in_order() {
    let l = lane();
    let r = solve(l, all());
    let s = square(l);
    assert_eq!(r.toggle.x + r.toggle.w, l.x + l.w);
    assert_eq!(r.toggle.w, s);
    assert_eq!(r.new.x + r.new.w, r.toggle.x);
    assert_eq!(r.search.x + r.search.w, r.new.x);
    assert_eq!(r.leading.x, l.x);
    assert_eq!(r.leading.w, s, "the mark takes one square");
}

/// A slot the window does not offer is zero-width at its place, so the
/// neighbours close up and nothing is hit there.
#[test]
fn an_unoffered_slot_is_zero_width_and_its_neighbours_close_up() {
    let l = lane();
    let no_new = Leading {
        new: false,
        ..all()
    };
    let r = solve(l, no_new);
    assert_eq!(r.new.w, 0.0);
    assert_eq!(r.new.x, r.toggle.x);
    assert_eq!(r.search.x + r.search.w, r.toggle.x);
    let (x, y) = r.new.centre();
    assert_eq!(
        hit(l, no_new, x, y),
        Some(Hit::Toggle),
        "the edge belongs to the toggle"
    );

    let none = solve(l, Leading::NONE);
    assert_eq!(none.leading.w, 0.0);
    assert_eq!(none.search.w, 0.0);
    assert_eq!(none.new.w, 0.0);
    assert_eq!(none.toggle.w, square(l), "the toggle is always offered");
}

/// The lights reserve what the platform says, and the mark reserves a square.
#[test]
fn the_leading_slot_is_the_marks_width() {
    let l = lane();
    assert_eq!(leading_w(l, Mark::None), 0.0);
    assert_eq!(leading_w(l, Mark::Mark), square(l));
    assert_eq!(leading_w(l, Mark::Lights(72.0)), 72.0);
    let r = solve(
        l,
        Leading {
            mark: Mark::Lights(72.0),
            ..all()
        },
    );
    assert_eq!(r.leading.w, 72.0);
}

/// A narrow lane gives way from the left: search goes first, then new, and
/// the toggle keeps the corner. Nothing goes negative and nothing overlaps.
#[test]
fn a_narrow_lane_gives_way_from_the_left_and_keeps_the_toggle() {
    let s = square(lane());
    // Room for the mark, the toggle and one whole square: new stays, search
    // declines.
    let narrow = Rect::new(0.0, 0.0, s * 3.0, theme::CAPTION_H);
    let r = solve(narrow, all());
    assert_eq!(r.toggle.w, s);
    assert_eq!(r.new.w, s);
    assert_eq!(r.search.w, 0.0, "search gave way first");
    assert_eq!(r.new.x, r.leading.x + r.leading.w);
    // Half a square short of that: a square is whole or absent, never clipped.
    let narrower = Rect::new(0.0, 0.0, s * 2.5, theme::CAPTION_H);
    let r = solve(narrower, all());
    assert_eq!(r.toggle.w, s);
    assert_eq!(r.new.w, 0.0, "new declines rather than being clipped");
    assert_eq!(r.search.w, 0.0);

    let tiny = Rect::new(0.0, 0.0, s * 0.5, theme::CAPTION_H);
    let r = solve(tiny, all());
    assert_eq!(
        r.toggle.w, 0.0,
        "nothing fits past the mark, nothing is negative"
    );
    assert_eq!(r.new.w, 0.0);
    assert_eq!(r.search.w, 0.0);

    let shut = Rect::new(0.0, 0.0, 0.0, theme::CAPTION_H);
    let r = solve(shut, all());
    for rect in [r.leading, r.search, r.new, r.toggle] {
        assert_eq!(rect.w, 0.0, "a shut pane has no lane");
    }
}
