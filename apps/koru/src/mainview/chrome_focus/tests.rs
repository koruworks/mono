// SPDX-License-Identifier: MPL-2.0

use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::*;
use crate::mainview::harness::container;
use koru_keys::PRIMARY_SHIFT;

fn press(v: &mut MainView, ws: &mut WorkspaceDoc, key: NamedKey) -> bool {
    v.key(ws, &Key::Named(key), None, ModifiersState::empty())
}

/// With the pane open, `F6` walks lane → apps → floor and wraps; with it
/// shut, only the lane is standing, so a second `F6` stays there.
#[test]
fn f6_rotates_over_the_standing_stops_and_wraps() {
    koru_features::testing::with(koru_features::Features::ALL_OFF, || {
        let (mut v, mut workspace) = container();
        assert!(press(&mut v, &mut workspace.ws, NamedKey::F6));
        assert_eq!(v.chrome_focus, Some(ChromeStop::Lane(LaneControl::Toggle)));
        assert!(press(&mut v, &mut workspace.ws, NamedKey::F6));
        assert_eq!(
            v.chrome_focus,
            Some(ChromeStop::Lane(LaneControl::Toggle)),
            "the pane is shut: the lane is the only stop"
        );

        assert!(v.key(
            &mut workspace.ws,
            &Key::Character("e".into()),
            Some("e"),
            PRIMARY_SHIFT
        ));
        assert!(v.sidebar.is_open());
        assert!(press(&mut v, &mut workspace.ws, NamedKey::F6));
        assert!(
            matches!(v.chrome_focus, Some(ChromeStop::Apps(_))),
            "{:?}",
            v.chrome_focus
        );
        assert!(press(&mut v, &mut workspace.ws, NamedKey::F6));
        assert_eq!(v.chrome_focus, Some(ChromeStop::Floor(FloorControl::Chip)));
        assert!(press(&mut v, &mut workspace.ws, NamedKey::F6));
        assert_eq!(
            v.chrome_focus,
            Some(ChromeStop::Lane(LaneControl::Toggle)),
            "wraps"
        );
    });
}

/// The arrows move within a stop; Enter activates; Escape leaves.
#[test]
fn arrows_move_within_a_stop_enter_activates_and_escape_leaves() {
    koru_features::testing::with(koru_features::Features::ALL_OFF, || {
        let (mut v, mut workspace) = container();
        assert!(v.key(
            &mut workspace.ws,
            &Key::Character("e".into()),
            Some("e"),
            PRIMARY_SHIFT
        ));
        // Lane → apps.
        press(&mut v, &mut workspace.ws, NamedKey::F6);
        press(&mut v, &mut workspace.ws, NamedKey::F6);
        let shown = NavigatorMode::visible();
        // Home leads the row; Documents, the active app, is second.
        let documents = shown
            .iter()
            .position(|m| *m == NavigatorMode::Documents)
            .expect("Documents is shown");
        assert_eq!(
            v.chrome_focus,
            Some(ChromeStop::Apps(documents)),
            "lands on the active app"
        );
        assert!(press(&mut v, &mut workspace.ws, NamedKey::ArrowRight));
        assert_eq!(v.chrome_focus, Some(ChromeStop::Apps(documents + 1)));
        assert!(press(&mut v, &mut workspace.ws, NamedKey::Enter));
        assert_eq!(
            v.explore.navigator_mode(),
            shown[documents + 1],
            "Enter shows the app"
        );
        // Past the last app is Manage apps, then it wraps.
        for _ in 0..shown.len() {
            press(&mut v, &mut workspace.ws, NamedKey::ArrowRight);
        }
        assert_eq!(
            v.chrome_focus,
            Some(ChromeStop::Apps(documents)),
            "wrapped past Manage apps"
        );
        // Floor: the chip is its one control (the ? lives in the popover's
        // toolbar now); Enter on it opens the switcher.
        press(&mut v, &mut workspace.ws, NamedKey::F6);
        assert_eq!(v.chrome_focus, Some(ChromeStop::Floor(FloorControl::Chip)));
        press(&mut v, &mut workspace.ws, NamedKey::ArrowRight);
        assert_eq!(v.chrome_focus, Some(ChromeStop::Floor(FloorControl::Chip)));
        assert!(press(&mut v, &mut workspace.ws, NamedKey::Enter));
        assert!(
            v.explore.popover_open(),
            "Enter on the chip opens the switcher"
        );
        assert!(press(&mut v, &mut workspace.ws, NamedKey::Escape));
        assert!(!v.explore.popover_open(), "Escape shuts the popover first");
        assert!(press(&mut v, &mut workspace.ws, NamedKey::Escape));
        assert_eq!(v.chrome_focus, None, "and then leaves the chrome");
    });
}

/// Enter on the lane's toggle shuts the pane; the lane keeps the keyboard
/// and the stops that went with the pane are gone from the rotation.
#[test]
fn enter_on_the_toggle_shuts_the_pane_and_the_lane_keeps_the_keyboard() {
    koru_features::testing::with(koru_features::Features::ALL_OFF, || {
        let (mut v, mut workspace) = container();
        assert!(v.key(
            &mut workspace.ws,
            &Key::Character("e".into()),
            Some("e"),
            PRIMARY_SHIFT
        ));
        press(&mut v, &mut workspace.ws, NamedKey::F6);
        assert_eq!(v.chrome_focus, Some(ChromeStop::Lane(LaneControl::Toggle)));
        assert!(press(&mut v, &mut workspace.ws, NamedKey::Enter));
        assert!(!v.sidebar.is_open());
        assert_eq!(v.chrome_focus, Some(ChromeStop::Lane(LaneControl::Toggle)));
        press(&mut v, &mut workspace.ws, NamedKey::F6);
        assert_eq!(
            v.chrome_focus,
            Some(ChromeStop::Lane(LaneControl::Toggle)),
            "no apps or floor stop while the pane is shut"
        );
        // A press anywhere moves the keyboard.
        v.mouse_down(
            &mut workspace.ws,
            v.viewport.0 * 0.5,
            v.viewport.1 * 0.5,
            false,
        );
        assert_eq!(v.chrome_focus, None);
    });
}
