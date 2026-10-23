// SPDX-License-Identifier: MPL-2.0

//! The slide, and the two things about it that would be bugs.

use super::*;
use std::time::Duration;

const W: f32 = 200.0;

/// The explore pane's drag ceiling, which the theme owns.
const SIDEBAR_MAX_W: f32 = theme::SIDEBAR_MAX_W;

fn slide_time() -> Duration {
    Duration::from_secs_f32(theme::PANEL_SLIDE_MS / 1000.0)
}

#[test]
fn a_panel_at_rest_is_not_animating() {
    let shut = Panel::new(W, false);
    assert!(!shut.animating());
    assert_eq!(shut.layout_width(), 0.0);
    assert_eq!(shut.drawn_width(), 0.0);

    let open = Panel::new(W, true);
    assert!(!open.animating());
    assert_eq!(open.layout_width(), W);
    assert_eq!(open.slide_offset(), 0.0, "flush");
}

/// **The performance rule.** The layout width is final the moment the panel is
/// toggled, so the content reflows once rather than once a frame. Only the
/// drawn offset moves.
#[test]
fn the_layout_width_is_final_immediately_and_only_the_offset_animates() {
    let now = Instant::now();
    let mut p = Panel::new(W, false);
    p.toggle(now);
    assert_eq!(p.layout_width(), W, "the content gave up its space at once");
    assert_eq!(p.slide_offset(), W, "but the column is still off the edge");

    p.advance(now + slide_time() / 2);
    assert_eq!(p.layout_width(), W, "and it does not move during the slide");
    assert!(p.slide_offset() > 0.0 && p.slide_offset() < W, "mid-slide");
}

/// **The loop-safety rule.** `animating` has to reach `false`, or the shell
/// wakes at frame rate forever. Driven off a deadline so a dropped frame
/// cannot strand it.
#[test]
fn a_slide_finishes_and_stops_asking_for_frames() {
    let now = Instant::now();
    let mut p = Panel::new(W, false);
    p.toggle(now);
    assert!(p.animating());

    p.advance(now + slide_time() / 4);
    assert!(p.animating(), "still going");

    // One late frame, well past the end — it must land exactly, not overshoot.
    p.advance(now + slide_time() * 10);
    assert!(!p.animating(), "the slide ended");
    assert_eq!(p.slide_offset(), 0.0, "and landed flush");
    assert!(
        !p.advance(now + slide_time() * 20),
        "nothing left to advance"
    );
}

/// Closing gives the space back to the content at once, but the column has to
/// keep drawing — over the content — until it is actually gone.
#[test]
fn closing_frees_the_layout_but_keeps_drawing_until_it_is_out() {
    let now = Instant::now();
    let mut p = Panel::new(W, true);
    p.toggle(now);
    assert!(!p.is_open());
    assert_eq!(p.layout_width(), 0.0, "the content took its width back");
    assert_eq!(p.drawn_width(), W, "but it is still on screen");
    assert_eq!(p.slide_offset(), 0.0, "and has not begun to leave yet");

    p.advance(now + slide_time() / 2);
    assert!(p.slide_offset() > 0.0, "sliding out");

    p.advance(now + slide_time());
    assert_eq!(p.drawn_width(), 0.0, "gone");
    assert!(!p.animating());
}

/// A toggle part-way through another one has to continue from where the column
/// actually is, not snap to an end first.
#[test]
fn reversing_mid_slide_starts_from_where_it_is() {
    let now = Instant::now();
    let mut p = Panel::new(W, false);
    p.toggle(now);
    p.advance(now + slide_time() / 2);
    let midway = p.slide_offset();
    assert!(midway > 0.0 && midway < W);

    let later = now + slide_time() / 2;
    p.toggle(later);
    assert!(!p.is_open());
    // The very next frame is still near where it was, rather than jumping.
    p.advance(later);
    assert!(
        (p.slide_offset() - midway).abs() < 1.0,
        "jumped from {midway} to {}",
        p.slide_offset()
    );
}

/// Restoring a session must not animate: it is motion nobody asked for, and
/// `animating` would be true before the first frame was drawn.
#[test]
fn restoring_a_state_does_not_animate() {
    let mut p = Panel::new(W, false);
    p.set_immediately(true);
    assert!(p.is_open());
    assert!(!p.animating(), "no slide on startup");
    assert_eq!(p.slide_offset(), 0.0, "already flush");
    assert_eq!(p.layout_width(), W);
}

// ------------------------------------------------------------ resizing

/// The drawn width follows the drag immediately; the **layout** width does not
/// move until it is committed. That gap is the entire mitigation for the drag —
/// `set_viewport` dirties every row on any width change, so a layout width that
/// followed the pointer would reflow the document several times a second.
#[test]
fn a_resize_moves_the_column_before_it_moves_the_content() {
    let mut p = Panel::new(232.0, true);
    assert!(p.set_full(400.0));

    assert_eq!(p.full(), 400.0, "the column draws at the new width at once");
    assert_eq!(
        p.layout_width(),
        232.0,
        "and the content is still laid out against the old one"
    );

    assert!(p.commit_width(), "releasing moves it");
    assert_eq!(p.layout_width(), 400.0);
    assert!(
        !p.commit_width(),
        "and a second release is not another reflow"
    );
}

/// Every mouse-move of a drag calls `set_full`; only the last one changed
/// anything the content cares about. One reflow for the whole gesture.
#[test]
fn a_whole_drag_commits_once() {
    let mut p = Panel::new(232.0, true);
    for w in [240.0, 260.0, 300.0, 380.0] {
        p.set_full(w);
        assert_eq!(p.layout_width(), 232.0, "no reflow mid-drag");
    }
    assert!(p.commit_width());
    assert_eq!(p.layout_width(), 380.0);
}

/// A width restored from settings is not a drag: the content is laid out against
/// it from the first frame rather than snapping to it once something is let go.
#[test]
fn a_restored_width_is_committed_immediately() {
    let mut p = Panel::new(232.0, false);
    p.set_full(500.0);
    p.set_immediately(true);
    assert_eq!(p.layout_width(), 500.0);
    assert!(!p.commit_width(), "nothing left to catch up");
}

/// A shut column gives the layout nothing, however wide it has been dragged.
#[test]
fn a_shut_column_takes_no_width_whatever_it_was_dragged_to() {
    let mut p = Panel::new(232.0, false);
    p.set_full(SIDEBAR_MAX_W);
    p.commit_width();
    assert_eq!(p.layout_width(), 0.0);
}
