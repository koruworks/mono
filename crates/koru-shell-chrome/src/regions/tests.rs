// SPDX-License-Identifier: MPL-2.0

//! What the window's arithmetic has to be true of, whatever size it is.
//!
//! Nothing here reads `theme::p()` or builds a `TextEngine`, so none of it needs
//! the process-wide theme mutex the editor's tests take, and none of it needs
//! `assets/fonts/*.ttf` to be present. That is deliberate — see the crate doc.

use super::*;

fn widths(n: usize) -> Vec<f32> {
    vec![theme::TAB_MIN_W; n]
}

fn at(r: &Regions, n: usize, x: f32, y: f32) -> Hit {
    hit(
        r,
        &widths(n),
        0.0,
        false,
        true,
        lane::Leading::NONE,
        false,
        false,
        x,
        y,
    )
}

fn at_wb(r: &Regions, n: usize, window_buttons: bool, context: bool, x: f32, y: f32) -> Hit {
    hit(
        r,
        &widths(n),
        0.0,
        window_buttons,
        context,
        lane::Leading::NONE,
        false,
        false,
        x,
        y,
    )
}

/// The workspace window's lane controls, with the pane open (unfolded).
fn all_leading() -> lane::Leading {
    lane::Leading {
        mark: lane::Mark::Mark,
        search: true,
        new: true,
        folded: false,
    }
}

/// The same controls with the pane shut: folded into the caption.
fn all_folded() -> lane::Leading {
    lane::Leading {
        folded: true,
        ..all_leading()
    }
}

fn at_leading(r: &Regions, n: usize, leading: lane::Leading, x: f32, y: f32) -> Hit {
    hit(r, &widths(n), 0.0, true, true, leading, false, false, x, y)
}

/// **The lane, with the pane open.** Its squares answer for themselves, its
/// empty part and its mark drag the window, and the pane below it is still
/// the pane.
#[test]
fn the_lane_owns_its_squares_and_drags_by_the_rest() {
    let r = open();
    assert_eq!(r.lane, Rect::new(0.0, 0.0, r.sidebar.w, r.caption.h));
    let l = lane::solve(r.lane, all_leading());
    let (x, y) = l.toggle.centre();
    assert_eq!(at_leading(&r, 3, all_leading(), x, y), Hit::ToggleSidebar);
    let (x, y) = l.search.centre();
    assert_eq!(at_leading(&r, 3, all_leading(), x, y), Hit::Search);
    let (x, y) = l.new.centre();
    assert_eq!(at_leading(&r, 3, all_leading(), x, y), Hit::New);
    let (x, y) = l.leading.centre();
    assert_eq!(
        at_leading(&r, 3, all_leading(), x, y),
        Hit::Caption,
        "the mark drags"
    );
    let gap = (l.leading.x + l.leading.w + l.search.x) * 0.5;
    assert_eq!(
        at_leading(&r, 3, all_leading(), gap, y),
        Hit::Caption,
        "the gap drags"
    );
    assert_eq!(
        at_leading(&r, 3, all_leading(), 4.0, r.caption.h + 4.0),
        Hit::Sidebar,
        "below the lane is the pane"
    );
}

/// **The chain, with the pane shut.** The same controls fold into the
/// caption's left end in the canvas's order, and the tabs begin after the
/// divider.
#[test]
fn the_shut_caption_folds_the_lane_into_its_leading_chain() {
    let r = shut();
    assert_eq!(r.lane.w, 0.0, "a shut pane has no lane");
    let chain = caption::leading_chain(r.caption, all_folded());
    assert_eq!(chain.leading.x, r.caption.x);
    assert_eq!(chain.toggle.x, chain.leading.x + chain.leading.w);
    assert_eq!(chain.search.x, chain.toggle.x + chain.toggle.w);
    assert_eq!(chain.new.x, chain.search.x + chain.search.w);
    assert_eq!(chain.divider.x, chain.new.x + chain.new.w);
    assert_eq!(chain.end, chain.divider.x + chain.divider.w);
    let (x, y) = chain.toggle.centre();
    assert_eq!(at_leading(&r, 3, all_folded(), x, y), Hit::ToggleSidebar);
    let (x, y) = chain.search.centre();
    assert_eq!(at_leading(&r, 3, all_folded(), x, y), Hit::Search);
    let (x, y) = chain.new.centre();
    assert_eq!(at_leading(&r, 3, all_folded(), x, y), Hit::New);
    let (x, y) = chain.leading.centre();
    assert_eq!(
        at_leading(&r, 3, all_folded(), x, y),
        Hit::Caption,
        "the mark drags"
    );
    let strip = caption::tab_strip_with_leading(r.caption, true, true, false, all_folded());
    assert_eq!(strip.x, chain.end, "the tabs begin after the divider");
    let first = tabs::layout(strip, &widths(3), 0.0)[0].body;
    assert_eq!(
        at_leading(&r, 3, all_folded(), first.x + 4.0, first.centre().1),
        Hit::Tab(0)
    );
}

/// **The open caption's left end is bare.** With the pane open the lane holds
/// the controls, the chain is empty, and the tabs begin at the caption's
/// edge; the pane's lane answers for the controls instead.
#[test]
fn an_open_pane_leaves_the_captions_left_end_bare() {
    let r = open();
    let chain = caption::leading_chain(r.caption, all_leading());
    for rect in [
        chain.leading,
        chain.toggle,
        chain.search,
        chain.new,
        chain.divider,
    ] {
        assert_eq!(rect.w, 0.0);
        assert_eq!(rect.x, r.caption.x);
    }
    assert_eq!(chain.end, r.caption.x);
    let strip = caption::tab_strip_with_leading(r.caption, true, true, false, all_leading());
    assert_eq!(strip.x, r.caption.x, "the tabs begin at the caption's edge");
    assert_eq!(
        at_leading(
            &r,
            3,
            all_leading(),
            r.caption.x + 4.0,
            r.caption.centre().1
        ),
        Hit::Tab(0),
        "no toggle at the caption's left when the pane is open"
    );
}

/// With nothing folded, the chain is the toggle alone and the strip is where
/// it always was — the regression every other test in this file relies on.
#[test]
fn no_leading_leaves_the_caption_where_it_was() {
    for r in [open(), shut()] {
        let chain = caption::leading_chain(r.caption, lane::Leading::NONE);
        assert_eq!(chain.toggle, caption::sidebar_toggle(r.caption));
        assert_eq!(chain.leading.w, 0.0);
        assert_eq!(chain.search.w, 0.0);
        assert_eq!(chain.new.w, 0.0);
        assert_eq!(chain.divider.w, 0.0);
        assert_eq!(chain.end, chain.toggle.x + chain.toggle.w);
        assert_eq!(
            caption::tab_strip_with_leading(r.caption, true, false, false, lane::Leading::NONE),
            caption::tab_strip_with_host_action(r.caption, true, false, false)
        );
    }
}

/// The lane and the caption share one band, so the row reads as one whether
/// the pane is open or shut.
#[test]
fn the_lane_and_the_caption_are_one_band() {
    let r = open();
    assert_eq!(r.lane.y, r.caption.y);
    assert_eq!(r.lane.h, r.caption.h);
    assert_eq!(r.lane.x + r.lane.w, r.caption.x);
    assert_eq!(r.lane.w, r.sidebar.w);
}

#[test]
fn host_action_owns_the_slot_immediately_before_overflow() {
    let r = shut();
    let button = caption::host_action_with_context(r.caption, true, true, true);
    let (x, y) = button.centre();
    let tab_widths = widths(20);

    assert_eq!(
        hit(
            &r,
            &tab_widths,
            120.0,
            true,
            true,
            lane::Leading::NONE,
            true,
            true,
            x,
            y,
        ),
        Hit::HostAction,
    );
    assert_ne!(
        hit(
            &r,
            &tab_widths,
            120.0,
            true,
            true,
            lane::Leading::NONE,
            false,
            true,
            x,
            y,
        ),
        Hit::HostAction,
    );
}

/// A comfortable window with both columns open — the shape most tests want.
fn open() -> Regions {
    solve(1200.0, 700.0, theme::SIDEBAR_W, theme::CONTEXT_W)
}

/// The same window with both columns shut, which is how it starts and how most
/// of the container's own tests drive it. The caption is the full width here, so
/// running the furniture tests over both is what proves they follow `caption.x`
/// rather than assuming zero.
fn shut() -> Regions {
    solve(1200.0, 700.0, 0.0, 0.0)
}

#[test]
fn the_regions_tile_the_window() {
    let r = open();
    // The caption begins where the pane ends.
    assert_eq!(r.caption.x, theme::SIDEBAR_W);
    assert_eq!(r.caption.x + r.caption.w, 1200.0);
    assert_eq!(r.sidebar.w + r.content.w + r.context.w, 1200.0);
    // No horizontal gap between the three columns.
    assert_eq!(r.sidebar.x + r.sidebar.w, r.content.x);
    assert_eq!(r.content.x + r.content.w, r.context.x);
    // The two columns beside the content fill the height below the caption.
    for band in [r.content, r.context] {
        assert_eq!(band.y, r.caption.h);
        assert_eq!(band.y + band.h, 700.0);
    }
}

/// The pane runs the whole height of the window, and the caption starts after
/// it — which is the shape the whole redesign is: the pane is a place, not a
/// drawer that opens underneath a title bar.
#[test]
fn the_pane_owns_the_full_height_and_the_caption_starts_after_it() {
    let r = open();
    assert_eq!(r.sidebar.x, 0.0);
    assert_eq!(r.sidebar.y, 0.0, "it starts at the very top");
    assert_eq!(r.sidebar.h, 700.0, "and runs to the very bottom");
    assert_eq!(r.caption.x, r.sidebar.w, "the caption begins where it ends");
    assert_eq!(
        r.caption.x, r.content.x,
        "and above the content, not the pane"
    );
}

/// With the pane open the top-left belongs to the lane — the window's own
/// band, which drags where it has no button — and the pane begins under it;
/// with it shut, to the toggle. Both have to be true, and the second is the
/// one an ordering mistake breaks silently — a zero-width sidebar or lane
/// tested first would swallow `x == 0` down the whole window.
#[test]
fn the_top_left_corner_is_the_pane_when_it_is_open_and_its_toggle_when_it_is_not() {
    let r = open();
    assert_eq!(
        at_wb(&r, 3, true, true, 4.0, 4.0),
        Hit::Caption,
        "the lane drags"
    );
    assert_eq!(
        at_wb(&r, 3, true, true, 4.0, r.lane.h + 4.0),
        Hit::Sidebar,
        "and the pane is under it"
    );

    let r = shut();
    assert_eq!(at_wb(&r, 3, true, true, 4.0, 4.0), Hit::ToggleSidebar);
    assert_eq!(
        at_wb(&r, 3, true, true, 0.0, 400.0),
        Hit::Content,
        "and below it"
    );
}

/// Everything in the caption is derived from `caption.x`, so opening the pane
/// carries the logo and the tabs along with it for free.
#[test]
fn the_caption_furniture_travels_with_the_pane() {
    let (o, s) = (open(), shut());
    assert_eq!(
        caption::sidebar_toggle(o.caption).x - caption::sidebar_toggle(s.caption).x,
        theme::SIDEBAR_W
    );
    assert_eq!(
        caption::tab_strip(o.caption, true, true).x - caption::tab_strip(s.caption, true, true).x,
        theme::SIDEBAR_W
    );
}

/// The window buttons are anchored to the far edge, which does not move — so
/// they must **not** travel, or they would walk left every time the pane opened.
#[test]
fn the_window_buttons_do_not_move_when_the_pane_opens() {
    let (o, s) = (open(), shut());
    for (a, b) in koru_ui::caption::controls(o.caption)
        .into_iter()
        .zip(koru_ui::caption::controls(s.caption))
    {
        assert_eq!(a.action, b.action);
        assert_eq!(a.rect.x, b.rect.x, "{:?}", a.action);
    }
}

/// A collapsed column is a zero-width rect rather than an absence, so the
/// arithmetic above holds unchanged at both ends of a slide.
#[test]
fn a_collapsed_column_has_no_width() {
    let r = shut();
    assert_eq!(r.sidebar.w, 0.0);
    assert_eq!(r.context.w, 0.0);
    assert_eq!(r.content.x, 0.0);
    assert_eq!(r.content.w, 1200.0);
    // With no pane to start after, the caption is the whole width again — the
    // geometry degrades to exactly what it was before it grew a pane.
    assert_eq!(r.caption.x, 0.0);
    assert_eq!(r.caption.w, 1200.0);
    assert_eq!(r.sidebar.h, 700.0, "full height even at zero width");
}

/// Mid-slide is not a special case: a fractional width is just a width.
#[test]
fn a_half_open_column_takes_half_the_room() {
    let half = theme::SIDEBAR_W / 2.0;
    let r = solve(1200.0, 700.0, half, 0.0);
    assert_eq!(r.sidebar.w, half);
    assert_eq!(r.content.x, half);
    assert_eq!(r.content.w, 1200.0 - half);
    // Mid-slide is the case this most easily gets wrong: the caption tracks the
    // pane's *layout* width the whole way, not just at the two ends.
    assert_eq!(r.caption.x, half);
    assert_eq!(r.caption.w, 1200.0 - half);
}

/// Content is what the window is for, so the columns give way to it — and the
/// **order** matters: the context column goes before the explore column,
/// because navigation is more often what you are mid-thought about.
#[test]
fn content_keeps_its_minimum_and_context_gives_way_first() {
    let w = theme::CONTENT_MIN_W + theme::SIDEBAR_W + 40.0;
    let r = solve(w, 700.0, theme::SIDEBAR_W, theme::CONTEXT_W);
    assert_eq!(r.sidebar.w, theme::SIDEBAR_W, "the sidebar is kept");
    assert_eq!(
        r.context.w, 40.0,
        "the context column is cut to what is left"
    );
    assert_eq!(r.content.w, theme::CONTENT_MIN_W);

    // Narrower still and the sidebar follows it.
    let r = solve(
        theme::CONTENT_MIN_W + 20.0,
        700.0,
        theme::SIDEBAR_W,
        theme::CONTEXT_W,
    );
    assert_eq!(r.context.w, 0.0);
    assert_eq!(r.sidebar.w, 20.0);
    assert_eq!(r.content.w, theme::CONTENT_MIN_W);
}

/// A `Resized(1, 1)` really does arrive — mid-drag, and when a window is
/// restored from minimised. Every rect has to stay finite and non-negative
/// rather than going inside out.
#[test]
fn a_tiny_window_still_produces_sane_rects() {
    for (w, h) in [
        (0.0, 0.0),
        (1.0, 1.0),
        (120.0, 80.0),
        (theme::CONTENT_MIN_W, 10.0),
    ] {
        let r = solve(w, h, theme::SIDEBAR_W, theme::CONTEXT_W);
        for (name, rect) in [
            ("caption", r.caption),
            ("sidebar", r.sidebar),
            ("content", r.content),
            ("context", r.context),
        ] {
            assert!(
                rect.w.is_finite() && rect.h.is_finite(),
                "{name} is not finite at {w}x{h}"
            );
            assert!(
                rect.w >= 0.0 && rect.h >= 0.0,
                "{name} is negative at {w}x{h}"
            );
        }
        assert!(
            r.caption.h <= h.max(0.0),
            "the caption never exceeds the window"
        );
    }
}

// ------------------------------------------------------------------ hit-testing

#[test]
fn the_columns_and_the_content_hit_test_back_to_themselves() {
    let r = open();
    let at = |p: (f32, f32)| at(&r, 2, p.0, p.1);
    assert_eq!(at(r.sidebar.centre()), Hit::Sidebar);
    assert_eq!(at(r.content.centre()), Hit::Content);
    assert_eq!(at(r.context.centre()), Hit::Context);
}

/// `Rect::contains` is inclusive on every edge, so a zero-width column sitting
/// at the content's left edge would otherwise swallow clicks meant for the
/// first character of every line.
#[test]
fn a_collapsed_column_cannot_be_hit() {
    let r = solve(1200.0, 700.0, 0.0, 0.0);
    assert_eq!(at(&r, 1, 0.0, 400.0), Hit::Content);
    assert_eq!(at(&r, 1, 1200.0, 400.0), Hit::Content);
}

#[test]
fn every_tab_hit_tests_back_to_its_own_index() {
    let r = open();
    for count in 1..=12 {
        let strip = caption::tab_strip(r.caption, false, true);
        let view_right = strip.x + (strip.w - theme::TAB_NEW_W).max(0.0);
        for t in tabs::layout(strip, &widths(count), 0.0) {
            // Tabs scrolled under the pinned `+` are not hit — the button wins.
            if t.body.x + 4.0 >= view_right {
                continue;
            }
            let (cx, cy) = t.body.centre();
            // The centre of a narrow tab can be inside its close box, which
            // outranks it by design — aim left of it.
            let x = if t.close.w > 0.0 { t.body.x + 4.0 } else { cx };
            assert_eq!(
                at(&r, count, x, cy),
                Hit::Tab(t.index),
                "{count} tabs, tab {}",
                t.index
            );
        }
    }
}

/// The close box sits inside the tab, so it has to be asked first or it is
/// unreachable.
#[test]
fn the_close_box_outranks_the_tab_it_sits_in() {
    let r = open();
    let strip = caption::tab_strip(r.caption, false, true);
    let t = tabs::layout(strip, &widths(3), 0.0)
        .into_iter()
        .nth(1)
        .expect("three tabs");
    assert!(
        t.close.w > 0.0,
        "three tabs in a wide window have close boxes"
    );
    let (cx, cy) = t.close.centre();
    assert_eq!(at(&r, 3, cx, cy), Hit::TabClose(1));
}

#[test]
fn the_new_tab_button_and_both_toggles_are_reachable() {
    // Both shapes: with the pane open the caption no longer starts at zero, and
    // a rect that assumed it would only be caught here.
    for r in [open(), shut()] {
        let strip = caption::tab_strip(r.caption, false, true);
        let (px, py) = tabs::new_tab(strip, &widths(3), 0.0).centre();
        assert_eq!(at(&r, 3, px, py), Hit::NewTab);

        let (sx, sy) = caption::sidebar_toggle(r.caption).centre();
        assert_eq!(at(&r, 3, sx, sy), Hit::ToggleSidebar);

        let (cx, cy) = caption::context_toggle(r.caption, false, true).centre();
        assert_eq!(at(&r, 3, cx, cy), Hit::ToggleContext);
    }
}

/// Empty caption is what you drag the window by, so it must not be claimed by
/// anything else. The `+` now follows the last tab, so the empty band is the
/// room *after* the button rather than the gap before it.
#[test]
fn empty_caption_is_draggable() {
    for r in [open(), shut()] {
        let strip = caption::tab_strip(r.caption, false, true);
        let plus = tabs::new_tab(strip, &widths(1), 0.0);
        let right = strip.x + strip.w;
        let x = (plus.x + plus.w + right) * 0.5;
        assert!(x > plus.x + plus.w && x < right, "band after the +");
        assert_eq!(at(&r, 1, x, r.caption.centre().1), Hit::Caption);
    }
}

/// With the OS titlebar gone, the four window controls take the right end —
/// close must own the very last pixel, so the pointer can be thrown into the
/// corner without aiming, and overflow occupies the slot the pin used to.
///
/// This was two tests, one per state of an experimental flag. The flag is gone
/// ([ADR-150](../../../../docs/decisions/README.md)), so they were the same test twice
/// and the second one only looked like extra coverage.
#[test]
fn the_window_buttons_own_the_corner_only_when_we_draw_them() {
    let r = open();
    let corner = (1200.0, 0.0);
    assert_eq!(
        at_wb(&r, 3, true, true, corner.0, corner.1),
        Hit::Window(koru_ui::caption::Action::Close)
    );
    for control in koru_ui::caption::controls(r.caption) {
        let (x, y) = control.rect.centre();
        assert_eq!(at_wb(&r, 3, true, true, x, y), Hit::Window(control.action));
    }
    let menu = koru_ui::caption::controls(r.caption)
        .into_iter()
        .find(|control| control.action == koru_ui::caption::Action::WindowMenu)
        .expect("overflow takes the slot the pin had")
        .rect;
    let (x, y) = menu.centre();
    assert_eq!(
        at_wb(&r, 3, true, true, x, y),
        Hit::Window(koru_ui::caption::Action::WindowMenu)
    );
    // While the OS still draws the titlebar, every custom control goes back to
    // it — that same corner is ours to drag, and overflow is not there at all.
    assert_ne!(
        at(&r, 3, corner.0, corner.1),
        Hit::Window(koru_ui::caption::Action::Close)
    );
    assert_ne!(
        at(&r, 3, corner.0, corner.1),
        Hit::Window(koru_ui::caption::Action::WindowMenu)
    );
}

/// With the column not offered, nothing in the caption answers as its toggle —
/// and the pixels it used to own belong to the tabs, not to nothing.
#[test]
fn a_hidden_context_toggle_is_not_hit_and_its_room_is_the_strips() {
    let r = open();
    let where_it_was = caption::context_toggle(r.caption, true, true).centre();

    assert_eq!(
        at_wb(&r, 3, true, true, where_it_was.0, where_it_was.1),
        Hit::ToggleContext,
        "offered, it is still there"
    );
    assert_ne!(
        at_wb(&r, 3, true, false, where_it_was.0, where_it_was.1),
        Hit::ToggleContext,
        "not offered, nothing claims that point as the toggle"
    );
    // It is caption — draggable — rather than a dead spot, unless a tab has
    // grown into it, which is the whole point of giving the room back.
    let landed = at_wb(&r, 3, true, false, where_it_was.0, where_it_was.1);
    assert!(
        matches!(
            landed,
            Hit::Caption | Hit::Tab(_) | Hit::TabClose(_) | Hit::NewTab
        ),
        "{landed:?}"
    );
}

/// An 800 px pane in a window with no room for it **degrades rather than
/// squeezing the document to nothing**. `solve` takes widths, not flags, and
/// protects the content with `CONTENT_MIN_W` — which is what makes a wide pane
/// safe to persist and reopen on a smaller screen.
#[test]
fn a_very_wide_pane_gives_way_to_the_content_in_a_narrow_window() {
    let r = solve(900.0, 700.0, 800.0, 0.0);
    assert!(
        r.content.w >= theme::CONTENT_MIN_W,
        "the document kept its floor: {}",
        r.content.w
    );
    assert!(r.sidebar.w < 800.0, "the pane gave way instead");
    assert_eq!(
        r.sidebar.w + r.content.w + r.context.w,
        900.0,
        "and the window is still fully accounted for"
    );
}

/// With room for it, a wide pane simply gets it.
#[test]
fn a_wide_pane_is_honoured_when_the_window_can_take_it() {
    let r = solve(1920.0, 1080.0, 800.0, 0.0);
    assert_eq!(r.sidebar.w, 800.0);
    assert_eq!(r.caption.x, 800.0, "and the caption still begins after it");
}

// ----------------------------------------------------------------- the banner
//
// The lane starts at `banner::end`, so a tab index resolved against the wrong
// start would select a different tab from the one under the pointer. These are
// the roundtrip tests for that, and the one that says nothing moved for a window
// with no banner.

fn banner_slots(label_w: f32) -> banner::Slots {
    banner::Slots {
        profile: true,
        workspace: banner::Workspace::Named(label_w),
        settings: true,
    }
}

fn at_banner(r: &Regions, n: usize, slots: banner::Slots, x: f32, y: f32) -> Hit {
    hit_with_banner(r, &widths(n), 0.0, true, false, false, false, slots, x, y)
}

/// **The regression.** With no banner offered, `hit_with_banner` and `hit`
/// answer the same thing everywhere — which is what lets `apps/koru` keep
/// calling `hit` and draw exactly the caption it drew yesterday.
#[test]
fn a_window_with_no_banner_hits_exactly_as_it_did_before() {
    for r in [open(), shut()] {
        for n in 1..=6 {
            let strip = caption::tab_strip(r.caption, true, false);
            let ys = r.caption.centre().1;
            let mut xs: Vec<f32> = vec![r.caption.x, r.caption.x + r.caption.w - 1.0];
            xs.extend(
                tabs::layout(strip, &widths(n), 0.0)
                    .iter()
                    .map(|t| t.body.x + 4.0),
            );
            xs.push(tabs::new_tab(strip, &widths(n), 0.0).centre().0);
            for x in xs {
                assert_eq!(
                    at_banner(&r, n, banner::Slots::NONE, x, ys),
                    hit(
                        &r,
                        &widths(n),
                        0.0,
                        true,
                        false,
                        lane::Leading::NONE,
                        false,
                        false,
                        x,
                        ys
                    ),
                    "x={x} n={n}"
                );
            }
        }
    }
}

/// With a banner showing, every tab still hit-tests back to its own index —
/// against a lane that now starts further right.
#[test]
fn every_tab_hit_tests_back_to_its_index_behind_a_banner() {
    let r = open();
    let slots = banner_slots(90.0);
    let strip = caption::tab_strip_with_banner(r.caption, true, false, false, slots);
    assert!(
        strip.x > caption::tab_strip(r.caption, true, false).x,
        "the banner really did move the lane"
    );
    let view_right = strip.x + (strip.w - theme::TAB_NEW_W).max(0.0);
    for count in 1..=4 {
        for t in tabs::layout(strip, &widths(count), 0.0) {
            if t.body.x + 4.0 >= view_right {
                continue;
            }
            let (cx, cy) = t.body.centre();
            let x = if t.close.w > 0.0 { t.body.x + 4.0 } else { cx };
            assert_eq!(
                at_banner(&r, count, slots, x, cy),
                Hit::Tab(t.index),
                "{count} tabs, tab {}",
                t.index
            );
        }
    }
}

/// A point inside a banner slot belongs to the banner, and `regions::hit` must
/// not claim it as a tab. It answers `Caption` — the drag band — because the
/// container asks `banner::hit` first, which is DESIGN's stated precedence.
#[test]
fn a_banner_slot_is_never_claimed_by_the_lane() {
    let r = open();
    let slots = banner_slots(90.0);
    for slot in [
        banner::profile_slot(r.caption, slots),
        banner::workspace_slot(r.caption, slots),
    ] {
        let (x, y) = slot.centre();
        assert_eq!(at_banner(&r, 4, slots, x, y), Hit::Caption);
        assert!(
            banner::hit(r.caption, true, slots, x, y).is_some(),
            "and the banner does claim it, asked first"
        );
    }
}

/// **Both right-hand toggles at once** (SP-12). The Settings square answers
/// through `banner::hit` and the context toggle through `regions::hit`, each
/// over its own rect, and neither claims the other's pixel.
#[test]
fn a_window_offering_settings_and_a_context_column_hits_both() {
    let r = open();
    let slots = banner_slots(90.0);
    let settings = banner::settings_slot(r.caption, true, slots);
    let toggle = caption::context_toggle_with_banner(r.caption, true, true, slots);

    let (sx, sy) = settings.centre();
    assert_eq!(
        banner::hit(r.caption, true, slots, sx, sy),
        Some(banner::Hit::Settings)
    );
    let (tx, ty) = toggle.centre();
    assert!(
        banner::hit(r.caption, true, slots, tx, ty).is_none(),
        "the banner does not claim the context toggle"
    );
    assert_eq!(
        hit_with_banner(&r, &widths(4), 0.0, true, true, false, false, slots, tx, ty),
        Hit::ToggleContext
    );
}

/// And with the column not offered the toggle is a zero-width rect against the
/// Settings slot, so the lane — not the toggle — owns that pixel.
#[test]
fn a_hidden_context_toggle_beside_settings_gives_its_room_to_the_tabs() {
    let r = open();
    let slots = banner_slots(90.0);
    let shown = caption::tab_strip_with_banner(r.caption, true, true, false, slots);
    let hidden = caption::tab_strip_with_banner(r.caption, true, false, false, slots);
    assert_eq!(hidden.w - shown.w, theme::CAPTION_H);
    assert_eq!(
        hidden.x + hidden.w,
        banner::settings_slot(r.caption, true, slots).x,
        "and the lane still stops at Settings"
    );
}

/// **The ground's lanes come off the content and the foot** and the caption
/// keeps its width: a context column keeps `CONTEXT_W` and shifts left; every
/// column stops at the foot. The lanes sit where the columns end, and the
/// columns and the lanes still add up to the window.
#[test]
fn the_edge_lane_comes_off_the_right() {
    let plain = solve(1200.0, 800.0, 300.0, 0.0);
    assert_eq!(plain.edge.w, 0.0, "nothing carved until asked");
    assert_eq!(plain.foot.h, 0.0);
    let r = plain.with_ground(8.0);
    assert_eq!(r.edge, Rect::new(1192.0, r.content.y, 8.0, r.content.h));
    assert_eq!(r.content.x + r.content.w, r.edge.x);
    assert_eq!(r.caption.x + r.caption.w, 1200.0, "the caption is not cut");
    assert_eq!(r.sidebar.w + r.content.w + r.context.w + r.edge.w, 1200.0);
    assert_eq!(r.foot, Rect::new(0.0, 792.0, 1200.0, 8.0));
    assert_eq!(r.content.y + r.content.h, r.foot.y);
    assert_eq!(
        r.sidebar.y + r.sidebar.h,
        r.foot.y,
        "the sidebar stops at the foot"
    );
    assert_eq!(r.edge.y + r.edge.h, r.foot.y, "so does the edge lane");

    let r = solve(1200.0, 800.0, 300.0, 268.0).with_ground(8.0);
    assert_eq!(
        r.content.w,
        1200.0 - 300.0 - 268.0 - 8.0,
        "the content gave it up"
    );
    assert_eq!(r.context.w, 268.0, "the context column keeps its width");
    assert_eq!(r.context.x + r.context.w, r.edge.x);
    assert_eq!(r.content.x + r.content.w, r.context.x);
    assert_eq!(r.context.y + r.context.h, r.foot.y);
}

/// **A shut pane leaves a lane on the left too** (the review's note, E71): the
/// content card then has the same air off the window on both sides, and
/// the same corners. With the pane open its card is the window's left and
/// the left lane is zero-width — the content still begins at the pane.
#[test]
fn a_shut_pane_leaves_a_lane_on_the_left() {
    let r = solve(1200.0, 800.0, 0.0, 0.0).with_ground(8.0);
    assert_eq!(r.left_edge, Rect::new(0.0, r.content.y, 8.0, r.content.h));
    assert_eq!(r.content.x, 8.0, "the content gives up the left lane");
    assert_eq!(r.content.x + r.content.w, r.edge.x);
    assert_eq!(r.content.w, 1200.0 - 16.0);
    assert_eq!(r.caption.x, 0.0, "the caption is not cut");
    assert_eq!(r.left_edge.y + r.left_edge.h, r.foot.y);
    assert_eq!(r.foot.x, 0.0, "the foot runs under the lane");

    let r = solve(1200.0, 800.0, 300.0, 0.0).with_ground(8.0);
    assert_eq!(r.left_edge.w, 0.0, "the pane is the window's left");
    assert_eq!(r.content.x, 300.0);
    let plain = solve(1200.0, 800.0, 0.0, 0.0);
    assert_eq!(plain.left_edge.w, 0.0, "nothing carved until asked");
}
