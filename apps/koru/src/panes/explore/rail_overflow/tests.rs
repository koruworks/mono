// SPDX-License-Identifier: MPL-2.0

use super::*;
fn app_pin(n: u128) -> koru_settings::pins::Pin {
    koru_settings::pins::Pin {
        id: koru_settings::pins::PinId(n),
        family: "33333333-3333-4333-8333-333333333333".into(),
        version: format!("44444444-4444-4444-8444-{n:012x}"),
        title: format!("App {n}"),
    }
}
#[test]
fn pin_group_preserves_footer_builtin_slots_and_cancels_obsolete_drag() {
    testing::with(Features::ALL_ON, || {
        let mut view = ExploreView::new();
        view.replace_mini_app_pins((1..=20).map(app_pin).collect());
        let modes = view.rail_modes();
        let slots = modes
            .iter()
            .map(|m| view.rail_shortcut(*m))
            .collect::<Vec<_>>();
        for height in [350.0, 400.0, 554.0, 700.0, 1000.0] {
            let band = Rect::new(0.0, 46.0, theme::RAIL_W, height);
            let rects = view.apps_rail_rects(band);
            assert!(rects.profile.y + rects.profile.h <= band.y + band.h);
            assert!(rects.pin_overflow.y + rects.pin_overflow.h <= rects.profile.y);
            assert!(rects.pins.iter().all(|(_, r)| r.y + r.h <= rects.profile.y));
        }
        assert_eq!(view.rail_modes(), modes);
        assert_eq!(view.rail_order_storage(), "");
        assert_eq!(
            modes
                .iter()
                .map(|m| view.rail_shortcut(*m))
                .collect::<Vec<_>>(),
            slots
        );
        let band = Rect::new(0.0, 46.0, theme::RAIL_W, 1000.0);
        let (_, r) = view.apps_rail_rects(band).pins[0];
        let (x, y) = r.centre();
        view.apps_rail_mouse_down(band, x, y);
        assert!(view.rail_dragging());
        view.rail_drag_move(band, -1000.0);
        assert_eq!(view.mini_app_pins[0].id, koru_settings::pins::PinId(1));
        let refreshed = vec![app_pin(99)];
        view.replace_mini_app_pins(refreshed.clone());
        assert!(!view.cancel_rail_drag(band));
        assert_eq!(view.mini_app_pins, refreshed);
        assert!(!view.nudge_pin(band, koru_settings::pins::PinId(99), true));
        assert!(!view.nudge_pin(band, koru_settings::pins::PinId(99), false));
    });
}
use koru_features::{Features, testing};

#[test]
fn zero_through_eight_views_keep_every_identity_and_promote_only_the_active_view() {
    let all = &NavigatorMode::ALL[..8];
    for count in 0..=8 {
        for capacity in 2..=10 {
            for active in all {
                let modes = &all[..count];
                let result = project(modes, *active, capacity);
                let mut combined = result.visible.clone();
                combined.extend(&result.folded);
                combined.sort_by_key(|mode| mode.token());
                let mut expected = modes.to_vec();
                expected.sort_by_key(|mode| mode.token());
                assert_eq!(combined, expected);
                assert!(result.visible.len() + usize::from(!result.folded.is_empty()) <= capacity);
                if modes.contains(active) {
                    assert!(result.visible.contains(active));
                }
                if result.promoted.is_some() {
                    assert_eq!(result.visible.last(), Some(active));
                }
                let folded_in_order: Vec<_> = modes
                    .iter()
                    .copied()
                    .filter(|mode| result.folded.contains(mode))
                    .collect();
                assert_eq!(result.folded, folded_in_order);
            }
        }
    }
}

#[test]
fn short_rail_keeps_fixed_controls_and_promotion_does_not_save_or_drag() {
    testing::with(Features::ALL_ON, || {
        let mut view = ExploreView::new();
        view.workspaces = (0..4)
            .map(|i| {
                let mut entry = WorkspaceEntry::new(
                    koru_core::WorkspaceId::new(),
                    &format!("Workspace {i}"),
                    i,
                );
                entry.pinned = i != 0;
                entry
            })
            .collect();
        view.current = Some(view.workspaces[0].id);
        let original = view.rail_modes();
        let active = *original.last().unwrap();
        view.set_navigator_mode(active);
        for height in [554.0, 600.0, 900.0] {
            let rail = Rect::new(0.0, 46.0, theme::RAIL_W, height);
            let controls = view.rail_controls(rail);
            // The pins in their saved order, the unpinned open workspace
            // after them, then the `…`.
            let squares: Vec<_> = [1, 2, 3, 0]
                .into_iter()
                .map(|i| apps_rail::Hit::Workspace(view.workspaces[i].id))
                .chain([apps_rail::Hit::WorkspaceMenu])
                .collect();
            assert_eq!(controls[..5], squares[..]);
            assert_eq!(controls.last(), Some(&apps_rail::Hit::Profile));
            assert!(!controls.contains(&apps_rail::Hit::Help));
            let rects = view.apps_rail_rects(rail);
            assert!(rects.profile.y + rects.profile.h <= rail.y + rail.h);
            assert!(
                rects
                    .apps
                    .iter()
                    .all(|(_, rect)| rect.y + rect.h < rects.profile.y)
            );
            assert_eq!(view.rail_modes(), original);
            assert_eq!(view.rail_order_storage(), "");
            if height == 554.0 {
                assert!(controls.contains(&apps_rail::Hit::Overflow));
                let square = rects
                    .apps
                    .iter()
                    .find(|(mode, _)| *mode == active)
                    .unwrap()
                    .1;
                let (x, y) = square.centre();
                view.apps_rail_mouse_down(rail, x, y);
                assert!(
                    !view.rail_dragging(),
                    "a promoted square is not a stored slot"
                );
                assert_eq!(
                    view.rail_shortcut(active),
                    koru_keys::rail_slot(original.len() - 1)
                );
            }
        }
    });
}

#[test]
fn moving_a_visible_view_preserves_folded_slots_and_escape_restores_order() {
    testing::with(Features::ALL_ON, || {
        let mut view = ExploreView::new();
        // Tall enough for three views under the waiting square and the
        // `…`, short enough to fold the rest.
        let rail = Rect::new(0.0, 0.0, theme::RAIL_W, 400.0);
        let original = view.rail_modes();
        view.set_navigator_mode(*original.last().unwrap());
        let before = view.view_projection(rail.h);
        assert!(before.visible.len() >= 3);
        assert!(before.visible.len() < original.len(), "some are folded");
        let rects = view.apps_rail_rects(rail);
        let (x, y) = rects.apps[0].1.centre();
        view.apps_rail_mouse_down(rail, x, y);
        view.rail_drag_move(rail, y + 40.0);
        let reordered = view.rail_modes();
        assert_eq!(reordered[0], original[1]);
        assert_eq!(reordered[1], original[0]);
        assert_eq!(&reordered[2..], &original[2..]);
        view.cancel_rail_drag(rail);
        assert_eq!(view.rail_modes(), original);
    });
}
