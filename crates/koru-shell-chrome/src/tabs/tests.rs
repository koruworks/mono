// SPDX-License-Identifier: MPL-2.0

//! Tab geometry. Layout is pure arithmetic over preferred widths — tests pass
//! synthetic widths and need no font.

use super::*;

fn strip() -> Rect {
    Rect::new(40.0, 0.0, 900.0, theme::CAPTION_H)
}

fn uniform(n: usize, w: f32) -> Vec<f32> {
    vec![w; n]
}

#[test]
fn no_tabs_means_no_rects() {
    assert!(layout(strip(), &[], 0.0).is_empty());
    assert!(layout(Rect::new(0.0, 0.0, 0.0, 0.0), &uniform(3, 100.0), 0.0).is_empty());
}

#[test]
fn tabs_run_left_to_right_and_never_overlap() {
    for count in 1..=20 {
        let widths = uniform(count, theme::TAB_MIN_W);
        let rects = layout(strip(), &widths, 0.0);
        assert_eq!(rects.len(), count);
        for pair in rects.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            assert!(
                a.body.x + a.body.w <= b.body.x + f32::EPSILON,
                "{count} tabs: {} overlaps {}",
                a.index,
                b.index
            );
        }
    }
}

/// The strip's left edge is a wall with the same air as between two tabs: one
/// `TAB_GAP` before the first tab, exactly the gap between any two.
#[test]
fn the_first_tab_gets_the_same_gap_as_the_gap_between_tabs() {
    let s = strip();
    let rects = layout(s, &uniform(4, 100.0), 0.0);
    let leading = rects[0].body.x - s.x;
    assert_eq!(
        leading,
        theme::TAB_GAP,
        "the wall a tab leans on is a gap too"
    );
    for pair in rects.windows(2) {
        assert_eq!(pair[1].body.x - (pair[0].body.x + pair[0].body.w), leading);
    }
    // It is content, not chrome: it scrolls away with the tabs.
    let many = uniform(20, 100.0);
    let scrolled = layout(s, &many, 10.0);
    assert_eq!(scrolled[0].body.x, s.x + theme::TAB_GAP - 10.0);
    // And it is counted in the scrolled width, or the last tab would run
    // short of the end by exactly it.
    assert_eq!(
        content_width(&uniform(4, 100.0)),
        4.0 * 100.0 + 4.0 * theme::TAB_GAP
    );
}

/// When content exceeds the strip, tabs keep their preferred width and scroll
/// rather than shrinking. Visible bodies stay inside the strip after clip.
#[test]
fn crowded_tabs_keep_min_width_and_overflow() {
    let s = strip();
    let widths = uniform(20, theme::TAB_MIN_W);
    assert!(content_width(&widths) > s.w);
    let rects = layout(s, &widths, 0.0);
    for t in &rects {
        assert!(
            (t.body.w - theme::TAB_MIN_W).abs() < 0.01,
            "tab {} shrank below preferred",
            t.index
        );
    }
    assert!(max_scroll(s, &widths) > 0.0);
    // Scrolled to the end, the last tab's left edge is still within content.
    let scroll = max_scroll(s, &widths);
    let last = *layout(s, &widths, scroll).last().unwrap();
    assert!(last.body.x < s.x + s.w);
}

#[test]
fn a_single_tab_is_capped_at_its_preferred_not_stretched() {
    let widths = [theme::TAB_MAX_W];
    let rects = layout(strip(), &widths, 0.0);
    assert_eq!(rects[0].body.w, theme::TAB_MAX_W);
}

#[test]
fn a_close_box_sits_inside_its_own_tab() {
    let widths = uniform(4, theme::TAB_MAX_W);
    for t in layout(strip(), &widths, 0.0) {
        assert!(t.close.w > 0.0, "a wide strip leaves room");
        assert!(t.close.x >= t.pill.x);
        assert!(t.close.x + t.close.w <= t.pill.x + t.pill.w);
        assert!(t.close.y >= t.pill.y);
        assert!(t.close.y + t.close.h <= t.pill.y + t.pill.h);
    }
}

#[test]
fn a_tab_is_drawn_clear_of_the_row_it_sits_in() {
    let s = strip();
    let widths = uniform(4, 120.0);
    for t in layout(s, &widths, 0.0) {
        assert!(t.pill.y > t.body.y, "tab {} touches the top", t.index);
        assert!(
            t.pill.y + t.pill.h < s.y + s.h - 1.0,
            "tab {} sits on the hairline",
            t.index
        );
        assert_eq!(t.body.h, s.h);
        assert_eq!(t.pill.x, t.body.x);
        assert_eq!(t.pill.w, t.body.w);
    }
}

#[test]
fn the_pill_is_inset_evenly_apart_from_the_hairline() {
    let s = strip();
    let t = layout(s, &uniform(3, 120.0), 0.0)[0];
    let top = t.pill.y - t.body.y;
    let bottom = (t.body.y + t.body.h) - (t.pill.y + t.pill.h);
    assert_eq!(top, theme::TAB_INSET_TOP);
    assert_eq!(bottom, theme::TAB_INSET_BOTTOM);
    assert_eq!(
        bottom - top,
        1.0,
        "the only asymmetry is the hairline; anything more is a design change"
    );
}

#[test]
fn a_row_shorter_than_its_insets_gives_a_flat_pill() {
    let squat = Rect::new(0.0, 0.0, 900.0, 4.0);
    for t in layout(squat, &uniform(3, 100.0), 0.0) {
        assert_eq!(t.pill.h, 0.0);
        assert_eq!(t.close.w, 0.0, "and no close box to click");
    }
}

#[test]
fn scroll_shifts_every_tab_by_the_same_amount() {
    let s = strip();
    // Enough tabs that content overflows, so scroll is not clamped to 0.
    let widths = uniform(20, theme::TAB_MIN_W);
    assert!(max_scroll(s, &widths) >= 40.0);
    let a = layout(s, &widths, 0.0);
    let b = layout(s, &widths, 40.0);
    for (u, v) in a.iter().zip(b.iter()) {
        assert!((u.body.x - v.body.x - 40.0).abs() < 0.01);
    }
}

/// The `+` travels with the tabs while there is room: it sits one gap after the
/// last one, so the button marks where the row ends.
#[test]
fn the_new_tab_button_follows_the_last_tab() {
    let s = strip();
    let mut previous = f32::MIN;
    for count in 1..=4 {
        let widths = uniform(count, theme::TAB_MIN_W);
        let last = *layout(s, &widths, 0.0).last().unwrap();
        let plus = new_tab(s, &widths, 0.0);
        assert!(
            (plus.x - (last.body.x + last.body.w + theme::TAB_GAP)).abs() < 0.01,
            "{count} tabs: + at {}, last tab ends at {}",
            plus.x,
            last.body.x + last.body.w,
        );
        assert!(plus.x > previous, "the + must move right as tabs are added");
        previous = plus.x;
    }
}

/// ...and stops when it reaches its lane, rather than being pushed out under
/// the window menu. This is the overflow case in the report.
#[test]
fn the_new_tab_button_anchors_once_the_tabs_overflow() {
    let s = strip();
    let widths = uniform(30, theme::TAB_MIN_W);
    for scroll in [0.0, 200.0, max_scroll(s, &widths)] {
        let plus = new_tab(s, &widths, scroll);
        assert!(
            (plus.x + plus.w - (s.x + s.w)).abs() < 0.01,
            "scrolled {scroll}: + should be anchored at the strip's end",
        );
    }
}

/// The invariant the overlap violated: no tab is ever drawn where the `+` is.
#[test]
fn no_tab_reaches_into_the_new_tab_lane() {
    let s = strip();
    for count in [1, 3, 8, 30] {
        let widths = uniform(count, theme::TAB_MIN_W);
        for scroll in [0.0, 120.0, max_scroll(s, &widths)] {
            let plus = new_tab(s, &widths, scroll);
            let clip = lane(s);
            for t in layout(s, &widths, scroll) {
                // A tab may extend past the lane, but only where it is clipped
                // away — the visible part must stop before the button.
                let visible_right = (t.body.x + t.body.w).min(clip.x + clip.w);
                assert!(
                    visible_right <= plus.x + 0.01,
                    "{count} tabs at {scroll}: tab {} reaches {visible_right}, + at {}",
                    t.index,
                    plus.x,
                );
            }
        }
    }
}

/// Scrolled fully right, the last tab comes entirely into view — the lane must
/// be reserved out of the viewport, not counted twice.
#[test]
fn the_last_tab_is_fully_visible_at_the_end_of_the_scroll() {
    let s = strip();
    let widths = uniform(20, theme::TAB_MIN_W);
    let rects = layout(s, &widths, max_scroll(s, &widths));
    let last = rects.last().unwrap();
    let clip = lane(s);
    assert!(
        last.body.x >= s.x - 0.01 && last.body.x + last.body.w <= clip.x + clip.w + 0.01,
        "last tab {}..{} outside lane {}..{}",
        last.body.x,
        last.body.x + last.body.w,
        clip.x,
        clip.x + clip.w,
    );
}

#[test]
fn ensure_visible_brings_a_far_tab_into_view() {
    let s = strip();
    let widths = uniform(15, theme::TAB_MIN_W);
    let scroll = ensure_visible(s, &widths, 0.0, 14);
    assert!(scroll > 0.0);
    let rects = layout(s, &widths, scroll);
    let t = rects[14].body;
    let view_right = s.x + (s.w - theme::TAB_NEW_W).max(0.0);
    assert!(t.x >= s.x - 0.5);
    assert!(t.x + t.w <= view_right + 0.5);
}

#[test]
fn ensure_visible_scrolls_left_when_a_tab_is_off_the_left() {
    let s = strip();
    let widths = uniform(15, theme::TAB_MIN_W);
    let far = ensure_visible(s, &widths, 0.0, 14);
    // From the far end, bring the first tab fully back.
    let scroll = ensure_visible(s, &widths, far, 0);
    assert!(scroll < far);
    let t = layout(s, &widths, scroll)[0].body;
    assert!(t.x >= s.x - 0.5, "left edge on screen");
    assert!(t.x + t.w <= s.x + s.w + 0.5);
}

#[test]
fn ensure_visible_is_a_no_op_when_the_tab_already_fits() {
    let s = strip();
    let widths = uniform(3, theme::TAB_MIN_W);
    let scroll = ensure_visible(s, &widths, 0.0, 1);
    assert_eq!(scroll, 0.0);
}

#[test]
fn an_icon_only_pin_is_tab_pinned_w_not_tab_min_w() {
    let mut engine = koru_draw::text::TextEngine::new();
    let w = preferred_width(&mut engine, "", true, false);
    assert_eq!(w, theme::TAB_PINNED_W);
    assert!(
        w < theme::TAB_MIN_W,
        "a pin with no title is a square, not an empty titled tab"
    );
    let titled = preferred_width(&mut engine, "Review Notes", true, false);
    assert!(titled >= theme::TAB_MIN_W);
}

#[test]
fn ensure_visible_tolerates_a_minimised_zero_width_strip() {
    let widths = uniform(3, theme::TAB_MIN_W);
    let minimised = Rect::new(0.0, 0.0, 0.0, theme::CAPTION_H);
    let scroll = ensure_visible(minimised, &widths, 24.0, 0);
    assert_eq!(scroll, 24.0);
}

#[test]
fn the_overflow_thumb_is_a_hairline_under_the_strip() {
    let s = strip();
    let widths = uniform(20, theme::TAB_MIN_W);
    let track = scroll_track(s, &widths).expect("crowded strip has a track");
    assert!(
        (track.h - theme::TAB_SCROLLBAR_H).abs() < 0.01,
        "thumb band is TAB_SCROLLBAR_H"
    );
    assert!((track.h - theme::SCROLLBAR_W).abs() < 0.01);
    let thumb = scroll_thumb(s, &widths, 0.0).expect("thumb");
    assert!((thumb.h - theme::TAB_SCROLLBAR_H).abs() < 0.01);
    assert!(
        thumb.y + thumb.h <= s.y + s.h + 0.5,
        "stays inside the caption row"
    );
}

// --------------------------------------------------------------- drag-reorder

#[test]
fn a_tab_lands_in_the_slot_its_centre_is_standing_in() {
    let s = strip();
    let widths = uniform(5, 120.0);
    let rects = layout(s, &widths, 0.0);
    for t in &rects {
        let (cx, _) = t.body.centre();
        for probe in [t.body.x, cx, t.body.x + t.body.w] {
            assert_eq!(
                drop_index(s, &widths, 0.0, 0, probe),
                t.index,
                "{probe} in {}",
                t.index
            );
        }
    }
    assert_eq!(drop_index(s, &widths, 0.0, 0, s.x + s.w + 50.0), 4);
    assert_eq!(drop_index(s, &widths, 0.0, 4, s.x - 50.0), 0);
}

#[test]
fn the_travel_to_swap_is_half_a_tab_and_the_same_in_both_directions() {
    let s = strip();
    let widths = uniform(5, 120.0);
    let rects = layout(s, &widths, 0.0);
    let w = rects[0].body.w;
    let (cx, _) = rects[2].body.centre();
    let reach = w * 0.5 + theme::TAB_GAP;

    assert_eq!(
        drop_index(s, &widths, 0.0, 2, cx + reach - 1.0),
        2,
        "short, rightward"
    );
    assert_eq!(
        drop_index(s, &widths, 0.0, 2, cx - reach + 1.0),
        2,
        "short, leftward"
    );
    assert_eq!(drop_index(s, &widths, 0.0, 2, cx + reach), 3, "rightward");
    assert_eq!(drop_index(s, &widths, 0.0, 2, cx - reach), 1, "leftward");
}

#[test]
fn reversing_a_drag_swaps_back_over_the_same_distance() {
    let s = strip();
    let widths = uniform(5, 120.0);
    let rects = layout(s, &widths, 0.0);
    let w = rects[0].body.w;
    let reach = w * 0.5 + theme::TAB_GAP;

    let (start, _) = rects[3].body.centre();
    assert_eq!(
        drop_index(s, &widths, 0.0, 3, start - reach),
        2,
        "half a tab left"
    );
    let (settled, _) = rects[2].body.centre();
    assert_eq!(
        drop_index(s, &widths, 0.0, 2, settled),
        2,
        "and stays there"
    );

    assert_eq!(
        drop_index(s, &widths, 0.0, 2, settled + reach),
        3,
        "half a tab right"
    );
    assert_eq!(
        drop_index(s, &widths, 0.0, 2, settled + reach - 1.0),
        2,
        "and not before"
    );
    assert_eq!(drop_index(s, &widths, 0.0, 2, start), 3);
}

#[test]
fn nudging_a_tab_does_not_reorder_it() {
    let s = strip();
    let widths = uniform(5, 120.0);
    let rects = layout(s, &widths, 0.0);
    for from in 0..5 {
        let (cx, _) = rects[from].body.centre();
        for nudge in [-9.0, -1.0, 0.0, 1.0, 9.0] {
            assert_eq!(
                drop_index(s, &widths, 0.0, from, cx + nudge),
                from,
                "tab {from} nudged by {nudge}"
            );
        }
    }
}

#[test]
fn a_tab_thrown_across_the_strip_lands_at_the_far_end() {
    let s = strip();
    let widths = uniform(6, 120.0);
    let rects = layout(s, &widths, 0.0);
    let (far, _) = rects[4].body.centre();
    assert_eq!(drop_index(s, &widths, 0.0, 0, far), 4);
    let (near, _) = rects[1].body.centre();
    assert_eq!(drop_index(s, &widths, 0.0, 5, near), 1);
}

#[test]
fn a_tab_in_the_gap_between_two_slots_keeps_its_index() {
    let s = strip();
    let widths = uniform(5, 120.0);
    let rects = layout(s, &widths, 0.0);
    let between = rects[2].body.x + rects[2].body.w + theme::TAB_GAP * 0.5;
    assert_eq!(
        drop_index(s, &widths, 0.0, 2, between),
        2,
        "coming from the left"
    );
    assert_eq!(
        drop_index(s, &widths, 0.0, 3, between),
        3,
        "coming from the right"
    );
}

#[test]
fn a_drop_index_is_always_a_real_index() {
    let s = strip();
    for count in 1..=10 {
        let widths = uniform(count, 100.0);
        for x in [-500.0, 0.0, 40.0, 500.0, 5000.0] {
            let to = drop_index(s, &widths, 0.0, 0, x);
            assert!(to < count, "{count} tabs at x={x} gave {to}");
        }
    }
}

#[test]
fn variable_widths_pack_left_to_right() {
    let s = strip();
    let widths = [80.0_f32, 120.0, 200.0];
    let rects = layout(s, &widths, 0.0);
    assert!((rects[0].body.w - 80.0).abs() < 0.01);
    assert!((rects[1].body.x - (rects[0].body.x + 80.0 + theme::TAB_GAP)).abs() < 0.01);
    assert!((rects[2].body.w - 200.0).abs() < 0.01);
}

/// **An icon-only pin centres its icon** (the CTO's note): no label and no
/// close box, so nothing is left of or right of it to make room for. A titled
/// tab's icon starts at the label's padding as before.
#[test]
fn an_icon_only_pin_centres_its_icon() {
    let pill = Rect::new(100.0, 0.0, theme::TAB_PINNED_W, 24.0);
    let x = icon_slot_x(pill, true);
    assert_eq!(
        x + crate::PAGE_ICON_SLOT * 0.5,
        pill.x + pill.w * 0.5,
        "the slot's centre is the pill's"
    );
    assert_eq!(icon_slot_x(pill, false), pill.x + theme::PANEL_PAD);
    let titled = Tab {
        label: "Brief",
        icon: None,
        closable: true,
        slide: 0.0,
        dragging: false,
        gap_after: 0.0,
    };
    assert!(!titled.is_icon_only());
    let pin = Tab {
        label: "",
        closable: false,
        ..titled
    };
    assert!(pin.is_icon_only());
}
