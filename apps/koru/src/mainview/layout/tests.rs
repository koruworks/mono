// SPDX-License-Identifier: MPL-2.0

//! Where things are, and what changes when a column or the window moves.
//!
//! The two columns live here rather than beside the routing because a column
//! *is* layout: what it takes from the content rect, what that costs to relayout,
//! and where the wheel goes are all questions about geometry.

use crate::leaf::Leaf;
use crate::mainview::MainView;
use crate::mainview::harness::*;
use crate::mainview::tab::TabView;

use super::{AI_MAX_W, AI_MIN_W};
use koru_core::node::Node;
use koru_core::schema::Parent;
use koru_draw::geometry::Rect;
use koru_draw::theme;
use koru_editor::Editing;
use koru_workspace_host::View;

use std::time::Instant;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

// -------------------------------------------------------------- performance

/// **A background tab must not go stale**, which is the property a user can
/// see: switching to it shows the document as it is now rather than as it was.
///
/// It no longer does that work *eagerly* — see
/// `a_background_tab_defers_the_work_until_it_is_shown` below — so what this
/// pins is the guarantee rather than the mechanism. `adopt` stands in for the
/// frame here: it is what the shell calls on every tick, and it is where the
/// tab about to be drawn settles up.
#[test]
fn switching_to_a_background_tab_shows_the_current_document() {
    let (mut v, mut workspace) = on_document();
    let doc = a_document(&mut workspace.ws, "Brief");
    v.open_node(&workspace.ws, doc);
    let doc_tab = v.tabs().active();

    // Switch away, then edit the document from underneath.
    let para = workspace.ws.children(Parent::Node(doc)).unwrap()[0];
    v.select(1); // back to the other tab
    workspace
        .ws
        .set_text(para, "arrived from elsewhere")
        .unwrap();
    v.adopt(&workspace.ws);

    // Now switch to it, as a user would.
    v.select(doc_tab);
    v.adopt(&workspace.ws);

    let TabView::Leaf(l) = &v.tabs().iter().nth(doc_tab).unwrap().view else {
        panic!("leaf")
    };
    assert!(
        l.body()
            .rows
            .iter()
            .any(|r| r.text == "arrived from elsewhere"),
        "switching to the tab did not bring it up to date"
    );
}

/// …and it must not do that work until then.
///
/// **This is the fix for the reconnect freeze.** A structural change set one
/// document-wide flag, and every open tab answered it by re-reading its own
/// whole document — so one peer pressing Enter cost a document walk per tab.
/// Coming back from offline delivered a burst of them and the window wedged.
///
/// The drain still happens (the Loro subscription would otherwise queue without
/// bound); what is deferred is the work.
#[test]
fn a_background_tab_defers_the_work_until_it_is_shown() {
    let (mut v, mut workspace) = on_document();
    let doc = a_document(&mut workspace.ws, "Brief");
    v.open_node(&workspace.ws, doc);
    let doc_tab = v.tabs().active();

    let para = workspace.ws.children(Parent::Node(doc)).unwrap()[0];
    v.select(1);
    workspace
        .ws
        .set_text(para, "arrived from elsewhere")
        .unwrap();
    v.adopt(&workspace.ws);

    let TabView::Leaf(l) = &v.tabs().iter().nth(doc_tab).unwrap().view else {
        panic!("leaf")
    };
    assert!(
        !l.body()
            .rows
            .iter()
            .any(|r| r.text == "arrived from elsewhere"),
        "the background tab did the work anyway — the deferral is not happening"
    );
    assert!(
        l.body().is_stale(),
        "…but it must know it owes itself a rebuild, or it never catches up"
    );
}

/// …but it must not *lay out*. `reflow` is parley over every row, and paying it
/// for tabs nobody can see is what makes a resize or a remote edit scale with
/// tab count. See the performance rules in AGENTS.md.
#[test]
fn a_background_tab_is_left_unlaid_out() {
    let (mut v, mut workspace) = on_document();
    for t in ["a", "b"] {
        let d = a_document(&mut workspace.ws, t);
        v.open_node(&workspace.ws, d);
    }
    let active = v.tabs().active();
    v.adopt(&workspace.ws);
    for (i, t) in v.tabs().iter().enumerate() {
        if i == active {
            assert!(t.sized.is_some(), "the visible tab is laid out");
        } else {
            assert_eq!(t.sized, None, "tab {i} was laid out while hidden");
        }
    }
}

/// A resize is O(1) reflows, not O(tabs): only the tab being looked at is laid
/// out, and the rest are marked stale for when they are next shown.
#[test]
fn a_resize_only_lays_out_the_visible_tab() {
    let (mut v, mut workspace) = on_document();
    let d = a_document(&mut workspace.ws, "a");
    v.open_node(&workspace.ws, d);

    v.set_viewport(800.0, 600.0);
    let active = v.tabs().active();
    let view = v.focused_view_rect();
    for (i, t) in v.tabs().iter().enumerate() {
        if i == active {
            assert_eq!(t.sized, Some((view.w, view.h)));
        } else {
            assert_eq!(t.sized, None, "tab {i}");
        }
    }
    // And it catches up on the way in, exactly once.
    v.select(1);
    assert!(v.tabs().iter().nth(1).unwrap().sized.is_some());
}

/// A column drag is chrome geometry only until release: the content keeps the
/// width it was laid out against through every mouse-move, and catches up
/// exactly once on `commit_sidebar_width`. Pinned at the container level
/// because the app-shell extraction moves `Panel` and this call path; the
/// one-reflow-per-drag contract must survive the move.
#[test]
fn a_sidebar_drag_reflows_the_content_once_on_release() {
    let (mut v, workspace) = on_document();
    v.toggle_sidebar();
    v.adopt(&workspace.ws);
    let before = v.tabs().active_tab().sized.expect("laid out after toggle");
    let before_view = v.focused_view_rect();
    assert_eq!(before, (before_view.w, before_view.h));

    // The drag: three moves, all inside the clamp (the pane's 256–600 plus
    // the rail). The column *draws* at the pointer…
    for w in [340.0, 380.0, 420.0] {
        assert!(v.set_sidebar_width(w));
    }
    assert_eq!(v.sidebar.full(), 420.0, "the drawn chrome follows the drag");
    // …while the layout holds still: the view rect keeps its committed
    // width, and the tab is not re-laid-out per mouse-move.
    assert_eq!(
        (v.focused_view_rect().w, v.focused_view_rect().h),
        before,
        "the view rect moved mid-drag"
    );
    assert_eq!(
        v.tabs().active_tab().sized,
        Some(before),
        "a mouse-move paid a reflow mid-drag"
    );

    // Release: the layout takes the new width and the view catches up, once.
    assert!(v.commit_sidebar_width());
    assert_ne!((v.focused_view_rect().w, v.focused_view_rect().h), before);
    assert_eq!(
        v.tabs().active_tab().sized,
        Some((v.focused_view_rect().w, v.focused_view_rect().h)),
        "the release did not let the view catch up"
    );
}

// ------------------------------------------------------------- pane dividers

/// A divider drag is chrome geometry only until release — the same
/// one-reflow-per-drag contract the sidebar's edge holds
/// (`a_sidebar_drag_reflows_the_content_once_on_release`).
#[test]
fn a_divider_drag_reflows_the_content_once_on_release() {
    use crate::mainview::panes::{PanePath, SplitDir};

    let (mut v, mut workspace) = on_document();
    // Move the document into a second pane on the right, then come back to the
    // left pane: the drag below is measured on its active tab.
    let notes = v
        .panes
        .focused()
        .tabs()
        .iter()
        .find(|t| !t.is_pinned() && t.label == "Notes")
        .map(|t| t.id)
        .expect("the document tab");
    let notes = v
        .panes
        .take_tab_by_id(&PanePath::root(), notes)
        .expect("took the document tab");
    v.panes.split(&PanePath::root(), SplitDir::Right, notes);
    v.panes.set_focus(PanePath::root().child(0));
    v.relayout();
    let before = v
        .tabs()
        .active_tab()
        .sized
        .expect("laid out after the split");
    let before_rect = v.focused_view_rect();
    assert_eq!(before, (before_rect.w, before_rect.h));

    // A press on the hairline starts the drag — the grip straddles it, so the
    // press lands on the divider's centre.
    let (_, _, div) = v
        .panes
        .dividers(v.content())
        .into_iter()
        .next()
        .expect("a split has a divider");
    let (x, y) = (div.x + div.w * 0.5, div.y + div.h * 0.5);
    assert!(
        v.mouse_down(&mut workspace.ws, x, y, false),
        "the grip took the press"
    );
    assert!(v.divider_drag.is_some());

    // Three moves: the hairline follows the pointer, the layout holds still.
    for dx in [40.0, 80.0, 120.0] {
        v.mouse_move(x + dx, y);
    }
    assert_eq!(
        v.tabs().active_tab().sized,
        Some(before),
        "a mouse-move paid a reflow mid-drag"
    );
    assert_ne!(
        (v.focused_view_rect().w, v.focused_view_rect().h),
        (before_rect.w, before_rect.h),
        "the divider did not follow the drag"
    );

    // Release: the views catch up, once.
    v.mouse_up(&mut workspace.ws);
    assert!(v.divider_drag.is_none());
    let view = v.focused_view_rect();
    assert_ne!((view.w, view.h), (before_rect.w, before_rect.h));
    assert_eq!(
        v.tabs().active_tab().sized,
        Some((view.w, view.h)),
        "the release did not let the view catch up"
    );

    // And the hairline announces itself to the pointer.
    let (_, _, div) = v.panes.dividers(v.content()).into_iter().next().unwrap();
    v.mouse_move(div.x + div.w * 0.5, div.y + div.h * 0.5);
    assert_eq!(v.cursor_icon(), CursorIcon::ColResize);
}

// ------------------------------------------------- the shared title bar

/// One pane, one lane: the lane is the caption's tab strip exactly — the
/// pre-split design, reached through the shared-title-bar path.
#[test]
fn a_single_pane_lane_is_the_caption_strip() {
    let (v, _workspace) = on_document();
    let lane = v
        .lane_rect_for(&crate::mainview::panes::PanePath::root())
        .expect("a lane");
    assert_eq!(lane, v.caption_lane());
    assert_eq!(lane.y, 0.0, "the lane is in the title bar");
}

/// In a split, both top-row lanes live in the title bar — and the one the
/// window chrome intersects yields that space, so tabs can never run under
/// the window buttons.
#[test]
fn top_row_lanes_share_the_title_bar_and_yield_the_window_chrome() {
    use crate::mainview::panes::{PanePath, SplitDir};

    let (mut v, _workspace) = on_document();
    let notes = v
        .panes
        .focused()
        .tabs()
        .iter()
        .find(|t| !t.is_pinned() && t.label == "Notes")
        .map(|t| t.id)
        .expect("the document tab");
    let notes = v
        .panes
        .take_tab_by_id(&PanePath::root(), notes)
        .expect("took the tab");
    v.panes.split(&PanePath::root(), SplitDir::Right, notes);

    let band = v.caption_lane();
    let left = v
        .lane_rect_for(&PanePath::root().child(0))
        .expect("left lane");
    let right = v
        .lane_rect_for(&PanePath::root().child(1))
        .expect("right lane");
    assert_eq!((left.y, left.h), (0.0, theme::CAPTION_H));
    assert_eq!((right.y, right.h), (0.0, theme::CAPTION_H));
    // Each lane is its share of the band — never under the furniture. The
    // right lane is the one the window buttons cut into.
    assert_eq!(left.x, band.x, "the left lane starts after the toggle");
    assert!(
        right.x + right.w <= band.x + band.w,
        "never under the buttons"
    );
    assert!(
        right.x + right.w < v.content().x + v.content().w,
        "the right pane's lane yields the caption-control area"
    );
}

/// Show title off is a square (`TAB_PINNED_W`), not an empty titled tab
/// clamped to `TAB_MIN_W`. Draw and hit-test used to disagree, so the chip
/// jumped every time the other cache remesured.
#[test]
fn an_icon_only_pin_keeps_tab_pinned_w() {
    let (mut v, _) = on_document();
    assert!(pin_active_document(&mut v));
    assert!(
        !v.tabs().get(0).unwrap().show_title,
        "the fixture pins without a title"
    );
    v.panes.focused_mut().ensure_widths(&mut v.engine);
    assert_eq!(
        v.panes.focused().widths[0],
        theme::TAB_PINNED_W + theme::TAB_PIN_GAP
    );
    v.ensure_tab_widths();
    assert_eq!(
        v.tab_widths()[0],
        theme::TAB_PINNED_W + theme::TAB_PIN_GAP,
        "draw and the window-level cache must agree"
    );
}

/// The wheel follows the pointer without taking focus with it. Scrolling over
/// the other pane in a split moves that document, not the one last clicked —
/// and the keyboard stays on the pane that was clicked, so a pointer resting
/// over the other half cannot take the next keystroke.
#[test]
fn the_wheel_scrolls_the_hovered_pane_without_focusing_it() {
    use crate::mainview::panes::{PanePath, SplitDir};

    let (mut v, mut workspace) = on_document();
    fill(&v, &mut workspace.ws, 80);
    v.adopt(&workspace.ws);
    let notes = v
        .panes
        .focused()
        .tabs()
        .iter()
        .find(|t| !t.is_pinned() && t.label == "Notes")
        .map(|t| t.id)
        .expect("the document tab");
    let notes = v
        .panes
        .take_tab_by_id(&PanePath::root(), notes)
        .expect("took the document tab");
    v.panes.split(&PanePath::root(), SplitDir::Right, notes);
    v.panes.set_focus(PanePath::root().child(0));
    v.relayout();

    let right = PanePath::root().child(1);
    let rect = v
        .panes
        .leaf_rects(v.content())
        .into_iter()
        .find(|(p, _)| *p == right)
        .map(|(_, r)| r)
        .expect("the right pane");
    let scroll =
        |v: &MainView, path: &PanePath| match &v.panes.group(path).expect("pane").active_tab().view
        {
            TabView::Leaf(l) => l.view().scroll,
            _ => 0.0,
        };
    assert_eq!(scroll(&v, &right), 0.0);

    let left = PanePath::root().child(0);
    v.mouse_move(rect.centre().0, rect.centre().1);
    assert_eq!(
        v.panes.focused_path(),
        &left,
        "hovering must not move focus — typing goes where you clicked"
    );
    v.scroll_by(MouseScrollDelta::LineDelta(0.0, -5.0));
    assert!(
        scroll(&v, &right) > 0.0,
        "the wheel scrolled the pane under the pointer"
    );
    assert_eq!(
        scroll(&v, &left),
        0.0,
        "and left the focused pane where it was"
    );
    assert_eq!(
        v.panes.focused_path(),
        &left,
        "scrolling the other pane must not steal the keyboard either"
    );
}

/// A press on a tab in another pane's title-bar lane focuses that pane —
/// the lane is the pane's, wherever it lives.
#[test]
fn pressing_a_tab_in_the_title_bar_focuses_its_pane() {
    use crate::mainview::panes::{PanePath, SplitDir};

    let (mut v, mut workspace) = on_document();
    let notes = v
        .panes
        .focused()
        .tabs()
        .iter()
        .find(|t| !t.is_pinned() && t.label == "Notes")
        .map(|t| t.id)
        .expect("the document tab");
    let notes = v
        .panes
        .take_tab_by_id(&PanePath::root(), notes)
        .expect("took the tab");
    v.panes.split(&PanePath::root(), SplitDir::Right, notes);
    v.panes.set_focus(PanePath::root().child(0));

    // The right pane's tab, in its share of the title bar.
    let right = PanePath::root().child(1);
    let lane = v.lane_rect_for(&right).expect("right lane");
    let widths = {
        let g = v.panes.group_mut(&right).unwrap();
        g.ensure_widths(&mut v.engine);
        g.widths.clone()
    };
    let rects = koru_shell_chrome::tabs::layout(lane, &widths, 0.0);
    let tab = rects.iter().find(|t| t.index == 0).expect("the tab");
    v.mouse_down(
        &mut workspace.ws,
        tab.body.centre().0,
        tab.body.centre().1,
        false,
    );
    assert_eq!(
        v.panes.focused_path(),
        &right,
        "the press focused the pane the tab belongs to"
    );
}

// ------------------------------------------------------------- explore column

#[test]
fn the_explore_column_starts_shut_and_ctrl_e_opens_it() {
    let (mut v, mut workspace) = container();
    assert_eq!(v.regions.sidebar.w, 0.0, "shut by default");
    assert_eq!(v.content().x, 0.0);

    assert!(v.key(&mut workspace.ws, &ch("e"), Some("e"), PRIMARY_SHIFT));
    assert!(v.sidebar.is_open());
    assert_eq!(v.regions.sidebar.w, theme::SIDEBAR_W + theme::RAIL_W);
    assert_eq!(
        v.content().x,
        theme::SIDEBAR_W + theme::RAIL_W,
        "the view made room"
    );
    assert_eq!(v.rail_area().w, theme::RAIL_W, "the rail before the pane");
    assert_eq!(v.explore_area().x, theme::RAIL_W);
    assert_eq!(v.explore_area().w, theme::SIDEBAR_W);

    assert!(v.key(&mut workspace.ws, &ch("e"), Some("e"), PRIMARY_SHIFT));
    assert!(!v.sidebar.is_open());
    assert_eq!(v.content().x, 0.0, "and took it back");
}

/// The pane runs the full height of the window and pushes the caption — logo,
/// tabs and all — to the right of it. That is the whole of the redesign, in
/// terms of geometry.
#[test]
fn opening_the_pane_pushes_the_caption_and_its_toggle_right() {
    let (mut v, mut workspace) = container();
    let shut_toggle = koru_shell_chrome::caption::sidebar_toggle(v.regions.caption);
    assert_eq!(v.regions.caption.x, 0.0, "nothing to start after yet");
    assert_eq!(shut_toggle.x, 0.0);

    assert!(v.key(&mut workspace.ws, &ch("e"), Some("e"), PRIMARY_SHIFT));

    assert_eq!(v.regions.sidebar.y, 0.0, "the pane starts at the top");
    assert_eq!(v.regions.sidebar.h, v.viewport.1, "and runs to the bottom");
    assert_eq!(v.regions.caption.x, theme::SIDEBAR_W + theme::RAIL_W);
    let open_toggle = koru_shell_chrome::caption::sidebar_toggle(v.regions.caption);
    assert_eq!(
        open_toggle.x - shut_toggle.x,
        theme::SIDEBAR_W + theme::RAIL_W,
        "the toggle moved"
    );
}

/// The buttons are anchored to the far edge, which the pane does not move, so
/// close still owns the corner however wide the pane is.
#[test]
fn the_window_buttons_stay_in_the_corner_when_the_pane_opens() {
    let (mut v, mut workspace) = container();
    let before = koru_ui::caption::controls(v.regions.caption);
    assert!(v.key(&mut workspace.ws, &ch("e"), Some("e"), PRIMARY_SHIFT));
    let after = koru_ui::caption::controls(v.regions.caption);

    for (a, b) in before.iter().zip(after.iter()) {
        assert_eq!(a.action, b.action);
        assert_eq!(a.rect.x, b.rect.x, "{:?} moved", a.action);
    }
    // Whatever is rightmost — the ✕, or the ⋮ button that replaced all four —
    // still ends at the window's edge.
    let last = after.last().expect("there is always at least one");
    assert_eq!(last.rect.x + last.rect.w, v.viewport.0);
}

/// It is still the toggle after it has moved — into the lane at the top of
/// the open pane, where the hit test has to follow it too; and the caption's
/// left end is bare while it is there.
#[test]
fn the_toggle_still_works_from_its_new_place() {
    let (mut v, mut workspace) = container();
    assert!(v.key(&mut workspace.ws, &ch("e"), Some("e"), PRIMARY_SHIFT));
    assert!(v.sidebar.is_open());

    // Settled rects: the pane is still sliding open, and the hit test reads
    // the settled lane, not the drawn one.
    let toggle = v.settled_lane_controls().toggle;
    let (x, y) = toggle.centre();
    assert!(
        x < theme::SIDEBAR_W + theme::RAIL_W,
        "it really is in the lane: {x}"
    );
    assert!(y < theme::CAPTION_H);
    let chain = koru_shell_chrome::caption::leading_chain(v.regions.caption, v.leading());
    assert_eq!(chain.toggle.w, 0.0, "and not in the caption");
    v.mouse_down(&mut workspace.ws, x, y, false);
    assert!(!v.sidebar.is_open(), "clicking it closed the pane again");
}

/// **New is always there, and it is the Add… menu's second door**
/// (QUESTIONS.md E21): the same list the tab strip's `+` opens, whichever
/// app is active, hung under the lane's square. Files, which makes nothing
/// of its own, offers it too.
#[test]
fn the_lanes_new_opens_the_add_menu_for_every_app() {
    use crate::mainview::context_menu::MenuSubject;
    use crate::panes::explore::NavigatorMode;
    let (mut v, mut workspace) = container();
    assert!(v.key(&mut workspace.ws, &ch("e"), Some("e"), PRIMARY_SHIFT));
    assert!(v.sidebar.is_open());
    assert_eq!(v.explore.navigator_mode(), NavigatorMode::Documents);
    let lane = v.settled_lane_controls();
    assert!(lane.new.w > 0.0, "Documents offers New");
    assert!(
        lane.search.x < lane.new.x && lane.new.x < lane.toggle.x,
        "Search, New, toggle, left to right"
    );

    v.show_navigator(NavigatorMode::Files);
    let lane = v.settled_lane_controls();
    assert!(
        lane.new.w > 0.0,
        "Files offers it too: the menu is not the app's"
    );

    let square = lane.new;
    let (x, y) = square.centre();
    v.mouse_down(&mut workspace.ws, x, y, false);
    let menu = v.menu.as_ref().expect("New opened the Add… menu");
    assert_eq!(
        menu.subject,
        MenuSubject::NewTab,
        "the same list as the strip's +"
    );
    let panel = menu.panel(v.viewport);
    assert!(
        panel.y >= square.y + square.h - 0.5,
        "hung under the square: {panel:?} vs {square:?}"
    );
    assert!(
        (panel.x - square.x).abs() < 0.5,
        "from its left edge: {panel:?} vs {square:?}"
    );
}

/// **The fold reflows once, at the toggle** (ANIMATION.md, "The pane
/// fold"): the layout takes the final width the moment the pane is toggled
/// and nothing re-cuts the window while it slides. The contents fade as the
/// recipe says, the chain is drawn through the fold, and the fold's clock
/// stops with the slide.
#[test]
fn the_fold_reflows_once_and_settles_with_the_slide() {
    use std::time::Duration;
    let (mut v, mut workspace) = container();
    assert!(!v.sidebar.is_open());
    assert!(v.chain_drawn(), "shut: the chain is there");
    let before = v.relayouts;
    assert!(v.key(&mut workspace.ws, &ch("e"), Some("e"), PRIMARY_SHIFT));
    assert_eq!(v.relayouts, before + 1, "one reflow at the toggle");
    assert!(v.sidebar.animating());
    assert!(v.fold.is_some_and(|(_, opening)| opening));
    assert!(v.chain_drawn(), "opening: the chain rides the edge");
    assert_eq!(
        v.fold_content_alpha(),
        0.0,
        "and the contents are not there yet"
    );

    // The slide, frame by frame: nothing re-cuts the window.
    let t0 = Instant::now();
    for ms in [20, 60, 100] {
        v.sidebar.advance(t0 + Duration::from_millis(ms));
        v.settle_fold();
        assert!(v.sidebar.animating(), "{ms} ms: still sliding");
        assert!(v.fold.is_some());
    }
    assert_eq!(v.relayouts, before + 1, "a frame paid a reflow mid-slide");

    v.sidebar.advance(t0 + Duration::from_secs(1));
    v.settle_fold();
    assert!(!v.sidebar.animating());
    assert_eq!(v.fold, None, "the fold's clock stops with the slide");
    assert!(
        !v.chain_drawn(),
        "open and at rest: the lane has the controls"
    );
    assert_eq!(v.fold_content_alpha(), 1.0);
}

/// Under reduced motion the fold lands at once: no slide, no fade, no frame
/// asked for — the OS said so, and the panel's other door answers it.
#[test]
fn reduced_motion_lands_the_fold_at_once() {
    let (mut v, mut workspace) = container();
    v.reduced_motion = true;
    assert!(v.key(&mut workspace.ws, &ch("e"), Some("e"), PRIMARY_SHIFT));
    assert!(v.sidebar.is_open());
    assert!(!v.sidebar.animating(), "no slide");
    assert_eq!(v.sidebar.progress(), 1.0, "and already flush");
    assert_eq!(v.fold, None);
    assert!(!v.chain_drawn());
    assert_eq!(v.fold_content_alpha(), 1.0);
    assert!(v.toggle_sidebar() == false || !v.sidebar.is_open());
    assert_eq!(v.sidebar.progress(), 0.0);
    assert!(!v.sidebar.animating());
}

/// ADR-26: the slide costs frames only while it is running, and stops asking
/// the moment it settles.
#[test]
fn the_pane_stops_asking_for_frames_when_it_settles() {
    let (mut v, mut workspace) = container();
    assert!(!v.animating(), "idle");

    assert!(v.key(&mut workspace.ws, &ch("e"), Some("e"), PRIMARY_SHIFT));
    assert!(v.animating(), "the pane is sliding");

    let done = Instant::now() + std::time::Duration::from_millis(1000);
    v.sidebar.advance(done);
    assert!(!v.animating(), "and nothing is left asking for frames");
    assert_eq!(v.sidebar.progress(), 1.0);
}

/// Restoring a session sets the columns without animating: a slide on startup
/// is motion nobody asked for.
#[test]
fn restoring_an_open_pane_does_not_animate() {
    let (mut v, workspace) = container();
    let mut settings = koru_workspace_host::settings::Settings::default();
    settings.sidebar_open = true;
    v.restore(&workspace, &settings);

    assert!(v.sidebar.is_open());
    assert!(!v.animating(), "no motion on restore");
    assert_eq!(v.sidebar.progress(), 1.0);
}

/// **The offset test again, with the column open.** This is the configuration
/// the translation scheme actually exists for — a content rect whose origin is
/// non-zero in *both* axes.
#[test]
fn a_click_is_still_offset_correctly_with_the_sidebar_open() {
    let (mut v, mut workspace) = on_document();
    let ws = &mut workspace.ws;
    let doc = fill(&v, ws, 6);
    v.toggle_sidebar();
    v.adopt(ws);

    let view = v.focused_view_rect();
    assert!(view.x > 0.0 && view.y > 0.0, "both axes are offset now");

    let mut bare = Leaf::document(ws, doc);
    Leaf::set_viewport(&mut bare, view.w, view.h);
    Leaf::adopt(&mut bare, ws);

    for (x, y) in [(70.0, row_y(0)), (120.0, row_y(1)), (200.0, row_y(3))] {
        v.mouse_down(ws, view.x + x, view.y + y, false);
        Leaf::mouse_down(&mut bare, ws, x, y, false);
        let TabView::Leaf(l) = &v.tabs().active_tab().view else {
            panic!("leaf")
        };
        assert_eq!(
            (l.body().focus, l.body().focused().map(|p| p.caret())),
            (bare.body().focus, bare.body().focused().map(|p| p.caret())),
            "click at ({x}, {y}) with the sidebar open"
        );
    }
}

/// The slide asks for frames, and then stops asking. A container that never
/// stopped would spin the loop at frame rate for the rest of the run.
#[test]
fn toggling_the_column_animates_and_then_settles() {
    let (mut v, _w) = container();
    assert!(!v.animating());
    v.toggle_sidebar();
    assert!(v.animating(), "the slide began");

    // `draw` is the tick, but the panel is what holds the deadline — advancing
    // it past the end is the same thing a late frame does.
    let done = Instant::now() + std::time::Duration::from_millis(theme::PANEL_SLIDE_MS as u64 * 4);
    v.sidebar.advance(done);
    assert!(!v.animating(), "and ended");
}

/// Opening the column must not lay out every tab — only the one on screen. The
/// rest reflow when they are next shown.
#[test]
fn toggling_the_column_only_lays_out_the_visible_tab() {
    let (mut v, mut workspace) = on_document();
    for t in ["a", "b"] {
        let d = a_document(&mut workspace.ws, t);
        v.open_node(&workspace.ws, d);
    }
    v.toggle_sidebar();
    let active = v.tabs().active();
    let view = v.focused_view_rect();
    for (i, t) in v.tabs().iter().enumerate() {
        if i == active {
            assert_eq!(t.sized, Some((view.w, view.h)));
        } else {
            assert_eq!(t.sized, None, "tab {i} was laid out while hidden");
        }
    }
}

/// A click in the tree shows the node — the column's whole purpose.
///
/// A bare container is showing Welcome, and **Welcome is a dedicated tab**:
/// the document opens in a new tab beside it rather than replacing it. From
/// an ordinary document tab a click still reuses the tab you are in (ADR-146;
/// `open::tests::explore_replaces_the_active_document_tab`). *Open in new
/// tab* is still there, on the right-click menu, where it is explicit.
#[test]
fn clicking_a_document_in_the_tree_from_welcome_opens_it_beside_welcome() {
    // From the bare container: the tree lists every document in the library,
    // and a fixture that opened one first would put its row above this one.
    let (mut v, mut workspace) = container();
    let doc = a_document(&mut workspace.ws, "Brief");
    v.toggle_sidebar();
    v.adopt(&workspace.ws);
    v.refresh_explore(&workspace.ws, true);

    let area = v.explore_area();
    let (_, r) = v.explore.row_rects(area).into_iter().next().expect("a row");
    // Right of the caret column, on the label. **Not the far right** — the last
    // 20 px of every row are the `…`'s reserved lane now, and a press there
    // opens the row's menu rather than the document.
    let before = v.tabs().len();
    v.mouse_down(&mut workspace.ws, r.x + r.w * 0.5, r.centre().1, false);
    // A document press seeds a latent drag; the open happens on release if the
    // pointer never armed.
    v.mouse_up(&mut workspace.ws);
    assert_eq!(
        v.tabs().len(),
        before + 1,
        "Welcome is not replaced; the document gets its own tab"
    );
    assert_eq!(v.document_root(), Some(doc), "and it shows the document");
}

#[test]
fn clicking_explore_after_opening_ai_releases_explores_latent_drag() {
    let (mut v, mut workspace) = container();
    let doc = a_document(&mut workspace.ws, "Brief");
    v.toggle_sidebar();
    v.toggle_ai();
    v.adopt(&workspace.ws);
    v.refresh_explore(&workspace.ws, true);
    assert!(v.ai_focused, "opening AI gives its composer focus");

    let area = v.explore_area();
    let (_, row) = v.explore.row_rects(area).into_iter().next().expect("a row");
    v.mouse_down(
        &mut workspace.ws,
        row.x + row.w * 0.5,
        row.centre().1,
        false,
    );
    assert!(!v.ai_focused, "the Explore press takes focus from AI");
    assert!(v.explore.pointer_active(), "the row owns the press");

    v.mouse_up(&mut workspace.ws);
    assert!(
        !v.explore.pointer_active(),
        "the matching release reaches Explore"
    );
    assert_eq!(
        v.document_root(),
        Some(doc),
        "the click still opens the row"
    );
}

/// An image no longer earns a tree row, so there is nothing here to
/// double-click. The test that drove that path — one click reveals the image,
/// a double-click opens its rename prompt — is gone with it. `Outcome::RenameImage`
/// and `MainView::rename_image` are deliberately kept: they are what comes back
/// when the hidden kinds do. See ROADMAP.

/// The wheel over the column scrolls the tree, not the document behind it.
#[test]
fn the_wheel_over_the_column_scrolls_the_tree() {
    let (mut v, mut workspace) = on_document();
    for i in 0..60 {
        a_document(&mut workspace.ws, &format!("doc {i}"));
    }
    fill(&v, &mut workspace.ws, 60);
    v.toggle_sidebar();
    v.adopt(&workspace.ws);
    v.refresh_explore(&workspace.ws, true);

    let doc_scroll = |v: &MainView| match &v.tabs().active_tab().view {
        TabView::Leaf(l) => l.view().scroll,
        _ => 0.0,
    };
    let area = v.explore_area();
    v.mouse_move(area.centre().0, area.centre().1);
    v.scroll_by(MouseScrollDelta::LineDelta(0.0, -5.0));
    assert_eq!(doc_scroll(&v), 0.0, "the document did not move");
    assert!(
        v.explore.row_rects(area).first().map(|(i, _)| *i).unwrap() > 0,
        "but the tree did"
    );
}

// ------------------------------------------------------------- context column

#[test]
fn the_context_column_starts_shut_and_ctrl_j_opens_it() {
    let (mut v, mut workspace) = container();
    assert_eq!(v.regions.context.w, 0.0);
    let full = v.content().w;

    assert!(v.key(&mut workspace.ws, &ch("j"), Some("j"), PRIMARY));
    assert!(v.context.is_open());
    assert_eq!(v.regions.context.w, theme::CONTEXT_W);
    assert_eq!(v.content().w, full - theme::CONTEXT_W, "the view made room");

    assert!(v.key(&mut workspace.ws, &ch("j"), Some("j"), PRIMARY));
    assert_eq!(v.content().w, full, "and took it back");
}

#[test]
fn the_ai_pane_uses_the_right_slot_and_clamps_its_drag_width() {
    let (mut v, _) = container();
    let full = v.content().w;
    assert!(!v.ai_panel.is_open());

    assert!(v.toggle_ai());
    assert!(v.ai_panel.is_open());
    assert_eq!(v.content().w, full - 480.0);
    assert_eq!(
        v.ai_rect().x,
        v.viewport.0,
        "the pane begins just beyond the right edge"
    );
    v.ai_panel
        .advance(Instant::now() + std::time::Duration::from_secs(1));
    assert_eq!(v.ai_rect().x, v.viewport.0 - 480.0);
    assert_eq!(v.ai_rect().w, 480.0);

    assert!(v.set_ai_width(2_000.0));
    assert_eq!(v.ai_panel.full(), AI_MAX_W);
    assert!(v.set_ai_width(20.0));
    assert_eq!(v.ai_panel.full(), AI_MIN_W);
    assert!(v.commit_ai_width());
}

#[test]
fn ai_and_context_replace_each_other_in_the_right_slot() {
    let (mut v, _) = container();
    v.toggle_context();
    assert!(v.context.is_open());

    v.toggle_ai();
    assert!(v.ai_panel.is_open());
    assert!(!v.context.is_open());

    v.toggle_context();
    assert!(v.context.is_open());
    assert!(!v.ai_panel.is_open());
}

#[test]
fn visible_ai_pane_claims_alt_question_navigation_without_focus() {
    let (mut v, mut workspace) = container();
    v.toggle_ai();
    v.ai_focused = false;

    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(NamedKey::ArrowUp),
        None,
        ModifiersState::ALT,
    ));
    assert!(v.ai_focused);
}

/// Both columns open at once: the content is squeezed from both sides, and the
/// offset test has to hold in the tightest configuration there is.
#[test]
fn both_columns_open_together_and_the_offset_still_holds() {
    let (mut v, mut workspace) = on_document();
    let ws = &mut workspace.ws;
    let doc = fill(&v, ws, 6);
    v.toggle_sidebar();
    v.toggle_context();
    v.adopt(ws);

    let c = v.content();
    assert_eq!(c.x, theme::SIDEBAR_W + theme::RAIL_W);
    assert_eq!(
        c.w,
        1200.0 - theme::SIDEBAR_W - theme::RAIL_W - theme::CONTEXT_W
    );

    let view = v.focused_view_rect();
    let mut bare = Leaf::document(ws, doc);
    Leaf::set_viewport(&mut bare, view.w, view.h);
    Leaf::adopt(&mut bare, ws);
    for (x, y) in [(70.0, row_y(0)), (120.0, row_y(1))] {
        v.mouse_down(ws, view.x + x, view.y + y, false);
        Leaf::mouse_down(&mut bare, ws, x, y, false);
        let TabView::Leaf(l) = &v.tabs().active_tab().view else {
            panic!("leaf")
        };
        assert_eq!(
            (l.body().focus, l.body().focused().map(|p| p.caret())),
            (bare.body().focus, bare.body().focused().map(|p| p.caret())),
            "click at ({x}, {y}) with both columns open"
        );
    }
}

/// Four sections are visible (Tasks joined History when its model landed,
/// Reminders when its own did, Cloud sync when the gateway learned to remember
/// its passes), so the column has a tab row; a press on it switches the
/// section and must still not fall through to the document behind the column.
#[test]
fn clicking_a_section_switches_it_without_reaching_the_document() {
    use crate::panes::context::{ContextView as CV, Section};
    let (mut v, mut workspace) = on_document();
    v.toggle_context();
    let area = v.regions.context;
    let sections = CV::section_rects(area);
    let [
        (tasks, tasks_rect),
        (reminders, reminders_rect),
        (history, history_rect),
        (cloud, cloud_rect),
    ] = sections.as_slice()
    else {
        panic!("Tasks, Reminders, History and Cloud sync should share the tab row")
    };
    assert_eq!(
        (*tasks, *reminders, *history, *cloud),
        (
            Section::Tasks,
            Section::Reminders,
            Section::History,
            Section::CloudSync
        )
    );
    assert_eq!(tasks_rect.y, history_rect.y);
    assert_eq!(tasks_rect.x + tasks_rect.w, reminders_rect.x);
    assert_eq!(reminders_rect.x + reminders_rect.w, history_rect.x);
    assert_eq!(history_rect.x + history_rect.w, cloud_rect.x);
    assert_eq!(v.context_view.section(), Section::History);
    // A press on the Tasks tab switches the column and is consumed by it.
    let (x, y) = (
        tasks_rect.x + tasks_rect.w / 2.0,
        tasks_rect.y + tasks_rect.h / 2.0,
    );
    assert!(v.mouse_down(&mut workspace.ws, x, y, false));
    assert_eq!(v.context_view.section(), Section::Tasks);
    // And the document never took a caret from any of it.
    let TabView::Leaf(l) = &v.tabs().active_tab().view else {
        panic!("leaf")
    };
    assert!(!l.view().dragging);
}

/// **Every section is reachable without a pointer, and so is the Cloud tab's
/// one action.** The column's tabs are icons in a row with no focus ring, so
/// without `Ctrl+Alt+J` a person who cannot use a pointer could not open Cloud
/// sync at all — and a *Copy details* that only a mouse can press would be the
/// same gap one level down.
#[test]
fn the_context_sections_and_the_cloud_action_are_reachable_from_the_keyboard() {
    use crate::panes::context::{CloudLine, Section};
    let (mut v, _workspace) = on_document();

    // The first press opens the column rather than skipping past a section
    // nobody has seen yet.
    assert!(!v.context.is_open());
    assert!(v.next_context_section());
    assert!(v.context.is_open());

    // From there it steps, and it comes back round.
    let start = v.context_view.section();
    let mut seen = vec![start];
    for _ in 1..Section::VISIBLE.len() {
        v.next_context_section();
        seen.push(v.context_view.section());
    }
    for section in Section::VISIBLE {
        assert!(seen.contains(section), "{section:?} is unreachable");
    }
    v.next_context_section();
    assert_eq!(v.context_view.section(), start, "the cycle closes");

    // And the action: nothing to copy while the column has no facts, and the
    // same door the button uses once it has.
    v.context_view.select(Section::CloudSync);
    assert!(!v.copy_cloud_details(), "an empty tab copies nothing");
    v.context_view.set_cloud(
        "Cloud sync".into(),
        vec![CloudLine {
            label: "This document".into(),
            value: "Synced".into(),
            detail: String::new(),
        }],
        None,
        "Copy details".into(),
    );
    assert!(v.copy_cloud_details());
}

/// The column is about whatever the active tab shows, so switching tabs
/// re-aims it. Only `document_root` can say, which is why it is what is passed.
#[test]
fn the_context_column_follows_the_active_tab() {
    let (mut v, mut workspace) = container();
    let doc = a_document(&mut workspace.ws, "Brief");
    v.toggle_context();
    assert_eq!(v.document_root(), None, "a welcome tab is not a node");

    v.open_node(&workspace.ws, doc);
    assert_eq!(v.document_root(), Some(doc), "now it has a subject");
}

/// A pointer over the column must not leave a row lit in the document behind
/// it, nor wear the I-beam.
#[test]
fn a_pointer_over_the_column_does_not_wear_the_ibeam() {
    let (mut v, mut workspace) = on_document();
    fill(&v, &mut workspace.ws, 1);
    v.toggle_sidebar();
    v.adopt(&workspace.ws);

    let c = v.content();
    v.mouse_move(c.x + 70.0, c.y + row_y(0));
    assert_eq!(v.cursor_icon(), CursorIcon::Text);

    let area = v.explore_area();
    v.mouse_move(area.x + 8.0, area.y + 200.0);
    assert_ne!(v.cursor_icon(), CursorIcon::Text, "over the column");
    let TabView::Leaf(l) = &v.tabs().active_tab().view else {
        panic!("leaf")
    };
    assert_eq!(l.view().hover, None, "the row highlight was dropped");
}

/// Not a key the container claims, so it has to reach the view underneath —
/// this is what stops a chord table swallowing ordinary typing.
#[test]
fn ordinary_typing_reaches_the_active_view() {
    let (mut v, mut workspace) = on_document();
    assert!(v.key(&mut workspace.ws, &ch("x"), Some("x"), NONE));
    let TabView::Leaf(l) = &v.tabs().active_tab().view else {
        panic!("leaf")
    };
    assert!(
        l.body().rows.iter().any(|r| r.text.contains('x')),
        "the keystroke never arrived"
    );
}

/// Escape is not a container chord either, and `NamedKey` must not resolve to
/// one by accident.
#[test]
fn a_named_key_with_ctrl_is_not_a_container_chord() {
    let (mut v, mut workspace) = container();
    let before = v.tabs().len();
    v.key(
        &mut workspace.ws,
        &Key::Named(NamedKey::Escape),
        None,
        PRIMARY,
    );
    assert_eq!(v.tabs().len(), before);
}

/// The regions the container hands out have to be the ones it drew, or a click
/// lands somewhere other than what it looks like.
#[test]
fn the_content_rect_sits_below_the_caption() {
    let (v, _w) = container();
    let r: Rect = v.content();
    assert_eq!(r.y, v.regions.caption.h);
    assert_eq!(r.x, 0.0, "both columns are collapsed for now");
    assert_eq!(r.w, 1200.0);
}

// ============================================================ drag-to-move
//
// `move_nodes` is the container half of an explore drag: chrome hands it the
// carried nodes and a (target, zone), and it translates that into `move_node`
// calls. The semantics that make a multi-move land contiguous are load-bearing,
// so each zone is pinned.

use crate::panes::{DropTarget, DropZone};

#[test]
fn move_nodes_persists_a_reorder_before() {
    let (mut v, mut workspace) = container();
    let a = a_document(&mut workspace.ws, "A");
    let b = a_document(&mut workspace.ws, "B");
    let c = a_document(&mut workspace.ws, "C");
    // Move C before A.
    assert!(v.move_nodes(
        &mut workspace.ws,
        vec![c],
        DropTarget::Row {
            node: a,
            zone: DropZone::Before,
        },
    ));
    assert_eq!(workspace.ws.children(Parent::Root).unwrap(), vec![c, a, b]);
}

#[test]
fn move_nodes_persists_a_reorder_after() {
    let (mut v, mut workspace) = container();
    let a = a_document(&mut workspace.ws, "A");
    let b = a_document(&mut workspace.ws, "B");
    let c = a_document(&mut workspace.ws, "C");
    // Move A after B.
    assert!(v.move_nodes(
        &mut workspace.ws,
        vec![a],
        DropTarget::Row {
            node: b,
            zone: DropZone::After,
        },
    ));
    assert_eq!(workspace.ws.children(Parent::Root).unwrap(), vec![b, a, c]);
}

#[test]
fn move_nodes_persists_a_nest() {
    let (mut v, mut workspace) = container();
    let a = a_document(&mut workspace.ws, "A");
    let b = a_document(&mut workspace.ws, "B");
    // Nest B under A.
    assert!(v.move_nodes(
        &mut workspace.ws,
        vec![b],
        DropTarget::Row {
            node: a,
            zone: DropZone::Nest,
        },
    ));
    let a_kids = workspace.ws.children(Parent::Node(a)).unwrap();
    assert_eq!(*a_kids.last().unwrap(), b, "B became A's last child");
    assert_eq!(
        workspace.ws.children(Parent::Root).unwrap(),
        vec![a],
        "A is still at the top level"
    );
}

#[test]
fn move_nodes_lands_a_multi_select_contiguous() {
    // Non-contiguous selection [A, C] dropped after B lands as a contiguous
    // run right after B, in order — regardless of where each came from.
    let (mut v, mut workspace) = container();
    let a = a_document(&mut workspace.ws, "A");
    let b = a_document(&mut workspace.ws, "B");
    let c = a_document(&mut workspace.ws, "C");
    let d = a_document(&mut workspace.ws, "D");
    assert!(v.move_nodes(
        &mut workspace.ws,
        vec![a, c],
        DropTarget::Row {
            node: b,
            zone: DropZone::After,
        },
    ));
    assert_eq!(
        workspace.ws.children(Parent::Root).unwrap(),
        vec![b, a, c, d],
        "A and C land together after B"
    );
}

#[test]
fn manifest_multi_move_gathers_then_places_every_selected_file() {
    use koru_core::explore::{ExploreEntry, KIND_FILE};
    use koru_workspace_host::PaneRequest;

    let (mut v, mut workspace) = container();
    let ids: Vec<_> = (0..4).map(|_| koru_core::DocumentId::new()).collect();
    v.entries = Some(
        ids.iter()
            .enumerate()
            .map(|(ordinal, id)| ExploreEntry {
                id: *id,
                parent: None,
                ordinal: ordinal as u32,
                title: format!("File {ordinal}.pdf"),
                kind: KIND_FILE.to_owned(),
                mime: Some("application/pdf".to_owned()),
                note: None,
                trashed_at: None,
                trashed_by: None,
            })
            .collect(),
    );

    assert!(v.move_nodes(
        &mut workspace.ws,
        vec![
            koru_core::NodeId::from_uuid(ids[0].as_uuid()),
            koru_core::NodeId::from_uuid(ids[2].as_uuid()),
        ],
        DropTarget::Row {
            node: koru_core::NodeId::from_uuid(ids[3].as_uuid()),
            zone: DropZone::After,
        },
    ));

    let requests: Vec<_> = std::iter::from_fn(|| View::take_pane_request(&mut v)).collect();
    assert_eq!(
        requests,
        vec![
            PaneRequest::MoveDocument {
                id: ids[0],
                parent: None,
                index: usize::MAX,
            },
            PaneRequest::MoveDocument {
                id: ids[2],
                parent: None,
                index: usize::MAX,
            },
            PaneRequest::MoveDocument {
                id: ids[0],
                parent: None,
                index: 2,
            },
            PaneRequest::MoveDocument {
                id: ids[2],
                parent: None,
                index: 3,
            },
        ]
    );
}

#[test]
fn move_nodes_skips_a_carried_descendant() {
    // Selecting both a parent and its child and dragging them must not move the
    // child twice — the parent's subtree move already carried it.
    let (mut v, mut workspace) = container();
    let a = a_document(&mut workspace.ws, "A");
    let b = a_document(&mut workspace.ws, "B");
    let child = workspace
        .ws
        .insert(Parent::Node(a), Node::task("a child").unwrap())
        .unwrap();
    // Carry both A and its child; the child should be filtered out as a
    // descendant of A, and A moved intact (with the child still under it).
    assert!(v.move_nodes(
        &mut workspace.ws,
        vec![a, child],
        DropTarget::Row {
            node: b,
            zone: DropZone::Before,
        },
    ));
    let top = workspace.ws.children(Parent::Root).unwrap();
    assert_eq!(top, vec![a, b], "A moved before B");
    let a_kids = workspace.ws.children(Parent::Node(a)).unwrap();
    assert!(
        a_kids.contains(&child),
        "A's child is still under it: {a_kids:?}"
    );
}

#[test]
fn move_nodes_can_promote_a_nested_selection_to_the_library_root() {
    let (mut v, mut workspace) = container();
    let parent = a_document(&mut workspace.ws, "Parent");
    let existing = a_document(&mut workspace.ws, "Existing");
    let child = workspace
        .ws
        .insert(Parent::Node(parent), Node::document("Child").unwrap())
        .unwrap();

    assert!(v.move_nodes(&mut workspace.ws, vec![child], DropTarget::Library));
    assert_eq!(
        workspace.ws.children(Parent::Root).unwrap(),
        vec![parent, existing, child]
    );
}
