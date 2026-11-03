// SPDX-License-Identifier: MPL-2.0

//! Moving the rail's views, driven through the press, the pointer and the
//! release the container hands the pane — no window, no font.

use std::time::Duration;

use koru_draw::theme;
use koru_features::{Features, testing};

use super::*;
use NavigatorMode::*;

/// The rail beside a 600-tall column: every shown view fits above the foot.
fn rail() -> Rect {
    Rect::new(0.0, theme::CAPTION_H, theme::RAIL_W, 600.0)
}

/// One slot down the rail: a square and the gap under it.
const STEP: f32 = apps_rail::SQUARE + apps_rail::GAP;

/// Where `mode`'s square is now.
fn square(v: &ExploreView, mode: NavigatorMode) -> Rect {
    v.apps_rail_rects(rail())
        .apps
        .into_iter()
        .find(|(m, _)| *m == mode)
        .map(|(_, r)| r)
        .unwrap_or_else(|| panic!("{mode:?} is not on the rail"))
}

/// Press `mode`'s square at its centre; the centre's `y`.
fn press(v: &mut ExploreView, mode: NavigatorMode) -> f32 {
    let (x, y) = square(v, mode).centre();
    v.apps_rail_mouse_down(rail(), x, y);
    y
}

/// The shown views in their default order, for this suite's features.
fn default_order() -> Vec<NavigatorMode> {
    NavigatorMode::visible()
}

/// **A click is still a click.** A press and a release with no travel shows
/// the view, leaves the order alone and saves nothing.
#[test]
fn a_click_without_moving_shows_the_view_and_keeps_the_order() {
    testing::with(Features::ALL_ON, || {
        let mut v = ExploreView::new();
        let y = press(&mut v, Files);
        assert!(v.rail_dragging(), "a press takes hold, as a tab's does");
        assert!(v.rail_drag_move(rail(), y));
        assert!(v.rail_drag_release(rail()));
        assert!(!v.rail_dragging());
        assert_eq!(v.navigator_mode(), Files, "the press showed the view");
        assert_eq!(v.rail_modes(), default_order());
        assert_eq!(v.rail_order_storage(), "", "nothing to save");
        assert!(!v.rail_slides.animating(), "and nothing to settle");
    });
}

/// **The half-way rule.** A square swaps with the one below once half of it is
/// over that one — its centre in that square — and not a pixel before; the
/// same going back up. The one it passed starts where it was and slides.
#[test]
fn a_view_moves_once_half_of_it_is_over_its_neighbour() {
    testing::with(Features::ALL_ON, || {
        let mut v = ExploreView::new();
        let y = press(&mut v, Documents);
        // Half a square and the gap under it, less a pixel: still in place.
        let half = apps_rail::SQUARE * 0.5 + apps_rail::GAP;
        v.rail_drag_move(rail(), y + half - 1.0);
        assert_eq!(v.rail_modes(), default_order(), "not half way");
        v.rail_drag_move(rail(), y + half);
        assert_eq!(&v.rail_modes()[..2], &[Files, Documents], "half below");
        // Files was drawn a slot down and eases up into the slot it now has.
        assert_eq!(v.rail_slides.offset(0), STEP, "starts where it was");
        assert_eq!(v.rail_slides.offset(1), 0.0, "the held one does not slide");
        assert!(v.animating(), "a neighbour sliding asks for frames");

        // Back up past half of Files again, and it swaps back.
        v.rail_drag_move(rail(), y + STEP - half + 1.0);
        assert_eq!(&v.rail_modes()[..2], &[Files, Documents], "not half");
        v.rail_drag_move(rail(), y + STEP - half);
        assert_eq!(v.rail_modes(), default_order(), "half above");
    });
}

/// **A drop keeps the order and saves it.** Released two slots down, the view
/// stays there, settles into its slot, stops asking for frames, and the line
/// it saves reads back as the same rail.
#[test]
fn a_drop_reorders_the_rail_and_the_order_is_saved() {
    testing::with(Features::ALL_ON, || {
        let mut v = ExploreView::new();
        let y = press(&mut v, Documents);
        v.rail_drag_move(rail(), y + STEP * 2.0 + 7.0);
        assert!(v.rail_drag_release(rail()));
        let want: Vec<NavigatorMode> = [Files, Tasks, Documents]
            .into_iter()
            .chain(default_order().into_iter().skip(3))
            .collect();
        assert_eq!(v.rail_modes(), want);
        assert_eq!(
            v.rail_slides.offset(2),
            7.0,
            "it drops in from where it was let go"
        );
        v.rail_slides
            .advance(Instant::now() + Duration::from_millis(theme::TAB_SLIDE_MS as u64 + 50));
        assert!(!v.animating(), "settled, and no more frames");

        let line = v.rail_order_storage();
        assert!(line.contains("files,tasks,documents"), "{line}");
        let mut next_launch = ExploreView::new();
        next_launch.restore_rail_order(&line);
        assert_eq!(next_launch.rail_modes(), want);
    });
}

/// **Esc puts it back.** Wherever it was carried to, the view returns to the
/// slot the press found it in, the views it passed slide back, and there is
/// nothing to save.
#[test]
fn escape_cancels_the_drag_and_everything_slides_back() {
    testing::with(Features::ALL_ON, || {
        let mut v = ExploreView::new();
        let y = press(&mut v, Chats);
        v.rail_drag_move(rail(), y - STEP * 3.0);
        assert_eq!(v.rail_modes()[0], Chats, "carried to the top");
        // Let the views it passed finish sliding out of its way.
        v.rail_slides
            .advance(Instant::now() + Duration::from_millis(theme::TAB_SLIDE_MS as u64 + 50));
        assert!(v.cancel_rail_drag(rail()));
        assert!(!v.rail_dragging());
        assert_eq!(v.rail_modes(), default_order(), "back where it was");
        assert_eq!(v.rail_order_storage(), "");
        assert!(v.rail_slides.animating(), "and everything slides home");
        assert_eq!(
            v.rail_slides.offset(0),
            STEP,
            "Documents starts a slot down, where it had moved to"
        );
        assert_eq!(
            v.rail_slides.offset(3),
            -STEP * 3.0,
            "Chats starts at the top, where it was let go"
        );
    });
}

/// The held square follows the pointer only along the run of views: never
/// above the first slot, never past the last, never over the foot.
#[test]
fn a_held_view_stays_on_the_run_of_views() {
    testing::with(Features::ALL_ON, || {
        let mut v = ExploreView::new();
        press(&mut v, Tasks);
        let rects = v.apps_rail_rects(rail());
        let (first, last) = (rects.apps[0].1, rects.apps.last().unwrap().1);

        v.rail_drag_move(rail(), -5000.0);
        let (_, held) = v.rail_carried(&v.apps_rail_rects(rail())).unwrap();
        assert_eq!(held.y, first.y);

        v.rail_drag_move(rail(), 5000.0);
        let (_, held) = v.rail_carried(&v.apps_rail_rects(rail())).unwrap();
        assert_eq!(held.y + held.h, last.y + last.h);
        assert!(held.y + held.h <= rects.help.y, "never over the foot");
        assert_eq!(*v.rail_modes().last().unwrap(), Tasks, "thrown to the end");
    });
}

/// Only a view is held: Help, Settings and the profile square are pressed,
/// never carried.
#[test]
fn the_foot_is_never_carried() {
    testing::with(Features::ALL_ON, || {
        let mut v = ExploreView::new();
        let rects = v.apps_rail_rects(rail());
        for foot in [rects.help, rects.settings, rects.profile] {
            let (x, y) = foot.centre();
            v.apps_rail_mouse_down(rail(), x, y);
            assert!(!v.rail_dragging(), "{foot:?}");
            v.close_popover();
        }
    });
}

/// `Ctrl+Shift+↑`/`↓`: one place at a time, the view easing into its new slot;
/// at either end there is nowhere to go.
#[test]
fn a_nudge_moves_a_view_one_place_and_stops_at_the_ends() {
    testing::with(Features::ALL_ON, || {
        let mut v = ExploreView::new();
        let n = v.rail_modes().len();
        assert_eq!(v.nudge_rail_view(rail(), 0, true), None, "top");
        assert_eq!(v.nudge_rail_view(rail(), n - 1, false), None, "bottom");
        assert_eq!(v.rail_modes(), default_order());

        assert_eq!(v.nudge_rail_view(rail(), 0, false), Some(1));
        assert_eq!(&v.rail_modes()[..2], &[Files, Documents]);
        assert_eq!(
            v.rail_slides.offset(1),
            -STEP,
            "eases down from its old slot"
        );
        assert_eq!(v.rail_slides.offset(0), STEP, "the one it passed eases up");
        assert_eq!(v.nudge_rail_view(rail(), 1, true), Some(0));
        assert_eq!(v.rail_modes(), default_order());
    });
}
