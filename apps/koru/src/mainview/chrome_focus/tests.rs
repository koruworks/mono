// SPDX-License-Identifier: MPL-2.0

use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::*;
use crate::mainview::harness::container;
use crate::panes::explore::NavigatorMode;
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
        // Past the last app are Help, Settings and the profile square, then
        // it wraps. ↓ walks the rail as → does.
        for _ in 0..shown.len() + 2 {
            press(&mut v, &mut workspace.ws, NamedKey::ArrowDown);
        }
        assert_eq!(
            v.chrome_focus,
            Some(ChromeStop::Apps(documents)),
            "wrapped past the foot"
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

/// **`Ctrl+Shift+↓`/`↑` on a rail view the keyboard is on moves it**, and the
/// keyboard goes with it; at the top there is nowhere to go and the chord is
/// still the view's. The order is what the session saves, and the view's own
/// shortcut goes with it — `Alt+4` is Chats wherever Chats sits.
#[test]
fn the_move_chords_move_the_focused_rail_view_and_its_shortcut_goes_with_it() {
    koru_features::testing::with(koru_features::Features::ALL_ON, || {
        let (mut v, mut workspace) = container();
        assert!(v.key(
            &mut workspace.ws,
            &Key::Character("e".into()),
            Some("e"),
            PRIMARY_SHIFT
        ));
        // `F6` round to the rail: the bell and the lane come first while
        // reminders are shown.
        for _ in 0..4 {
            if matches!(v.chrome_focus, Some(ChromeStop::Apps(_))) {
                break;
            }
            press(&mut v, &mut workspace.ws, NamedKey::F6);
        }
        let default = NavigatorMode::visible();
        let chats = default.iter().position(|m| *m == NavigatorMode::Chats);
        let chats = chats.expect("Chats is shown");
        // Onto Chats, and up the rail to the top: one place per chord.
        for _ in 0..chats {
            press(&mut v, &mut workspace.ws, NamedKey::ArrowDown);
        }
        assert_eq!(v.chrome_focus, Some(ChromeStop::Apps(chats)));
        let up = Key::Named(NamedKey::ArrowUp);
        for at in (0..chats).rev() {
            assert!(v.key(&mut workspace.ws, &up, None, PRIMARY_SHIFT));
            assert_eq!(v.chrome_focus, Some(ChromeStop::Apps(at)), "follows it");
        }
        assert_eq!(v.explore.rail_modes()[0], NavigatorMode::Chats);
        assert!(
            v.key(&mut workspace.ws, &up, None, PRIMARY_SHIFT),
            "at the top the chord is still the view's"
        );
        assert_eq!(v.chrome_focus, Some(ChromeStop::Apps(0)));
        assert_eq!(v.explore.rail_modes()[0], NavigatorMode::Chats);

        // Down once: Chats under Documents.
        let down = Key::Named(NamedKey::ArrowDown);
        assert!(v.key(&mut workspace.ws, &down, None, PRIMARY_SHIFT));
        assert_eq!(
            &v.explore.rail_modes()[..2],
            &[NavigatorMode::Documents, NavigatorMode::Chats]
        );
        assert_eq!(v.chrome_focus, Some(ChromeStop::Apps(1)));
        // Enter shows the view the ring is on, which is the one that moved.
        assert!(press(&mut v, &mut workspace.ws, NamedKey::Enter));
        assert_eq!(v.explore.navigator_mode(), NavigatorMode::Chats);

        let mut settings = koru_workspace_host::Settings::default();
        koru_workspace_host::View::capture(&v, &mut settings);
        assert!(
            settings.rail_order.contains("documents,chats,files"),
            "{}",
            settings.rail_order
        );

        // The view's chord did not move with its place.
        assert!(press(&mut v, &mut workspace.ws, NamedKey::Escape));
        assert!(v.key(
            &mut workspace.ws,
            &Key::Character("1".into()),
            Some("1"),
            koru_keys::VIEW
        ));
        assert_eq!(v.explore.navigator_mode(), NavigatorMode::Documents);
        assert!(v.key(
            &mut workspace.ws,
            &Key::Character("4".into()),
            Some("4"),
            koru_keys::VIEW
        ));
        assert_eq!(v.explore.navigator_mode(), NavigatorMode::Chats);
    });
}

/// Without the keyboard on the rail the chord is not the rail's: the order
/// stays as it is.
#[test]
fn the_move_chords_leave_the_rail_alone_without_the_keyboard_on_it() {
    koru_features::testing::with(koru_features::Features::ALL_ON, || {
        let (mut v, mut workspace) = container();
        let before = v.explore.rail_modes();
        v.key(
            &mut workspace.ws,
            &Key::Named(NamedKey::ArrowDown),
            None,
            PRIMARY_SHIFT,
        );
        assert_eq!(v.chrome_focus, None);
        assert_eq!(v.explore.rail_modes(), before);
    });
}
