// SPDX-License-Identifier: MPL-2.0

//! What the multi-window runner does that the single-window one has no need
//! for. The behaviour they share is asserted once, next door in
//! [`crate::dispatch::parity`].

use std::time::Duration;

use super::*;

#[test]
fn the_earliest_deadline_across_windows_wins_and_nothing_pending_never_does() {
    let now = Instant::now();
    let soon = now + Duration::from_millis(10);
    let later = now + Duration::from_millis(500);
    assert_eq!(earliest(Some(later), Some(soon)), Some(soon));
    assert_eq!(earliest(Some(soon), Some(later)), Some(soon));
    assert_eq!(
        earliest(None, Some(soon)),
        Some(soon),
        "a window with nothing pending must not postpone one that has work"
    );
    assert_eq!(earliest(Some(soon), None), Some(soon));
    assert_eq!(earliest(None, None), None);
}

/// The default `windows()` authority is a slice the app owns, so a runner
/// settle allocates nothing to read it. The key is opaque: nothing compares it
/// to anything but another key.
#[test]
fn a_window_key_is_only_ever_compared() {
    let mut keys = vec![WindowKey(9), WindowKey(1), WindowKey(4)];
    keys.sort();
    assert_eq!(keys, [WindowKey(1), WindowKey(4), WindowKey(9)]);
    assert_ne!(WindowKey(1), WindowKey(2));
}

#[derive(Default)]
struct Minimal {
    windows: Vec<WindowKey>,
    seen: Vec<String>,
}

impl MultiWindowApp for Minimal {
    type Event = ();

    fn windows(&self) -> &[WindowKey] {
        &self.windows
    }

    fn should_exit(&self) -> bool {
        false
    }

    fn close_requested(&mut self, key: WindowKey) {
        self.seen.push(format!("close {}", key.0));
    }

    fn pointer_moved(&mut self, key: WindowKey, x: f32, y: f32) -> bool {
        self.seen.push(format!("moved {} {x} {y}", key.0));
        true
    }

    fn click(
        &mut self,
        key: WindowKey,
        _engine: &mut TextEngine,
        x: f32,
        y: f32,
        _ctrl: bool,
    ) -> bool {
        self.seen.push(format!("click {} {x} {y}", key.0));
        true
    }

    fn right_click(&mut self, key: WindowKey, _engine: &mut TextEngine, x: f32, y: f32) -> bool {
        self.seen.push(format!("right_click {} {x} {y}", key.0));
        true
    }

    fn key(
        &mut self,
        key: WindowKey,
        _engine: &mut TextEngine,
        logical: &Key,
        _text: Option<&str>,
        _mods: ModifiersState,
    ) -> bool {
        self.seen.push(format!("key {} {logical:?}", key.0));
        true
    }

    fn draw(
        &mut self,
        _key: WindowKey,
        _scene: &mut Scene,
        _engine: &mut TextEngine,
        _w: f32,
        _h: f32,
    ) {
    }
}

/// The two adapters `WindowApp` has — `context_menu` falling through to
/// `right_click`, and `rich_key` falling through to `key` for presses only —
/// are written the same way here, so an app ported between the runners keeps
/// its behaviour.
#[test]
fn the_defaulted_adapters_route_per_window_exactly_as_the_single_window_ones_do() {
    let mut app = Minimal::default();
    let mut engine = TextEngine::new();
    let key = WindowKey(3);
    assert!(app.context_menu(key, &mut engine, 4.0, 5.0));

    let logical = Key::Character("k".into());
    let press = RichKeyEvent {
        logical_key: &logical,
        physical_key: PhysicalKey::Code(winit::keyboard::KeyCode::KeyK),
        location: winit::keyboard::KeyLocation::Standard,
        text: Some("k"),
        state: crate::KeyState::Pressed,
        repeat: false,
        modifiers: ModifiersState::empty(),
    };
    assert!(app.rich_key(key, &mut engine, press));
    assert!(!app.rich_key(
        key,
        &mut engine,
        RichKeyEvent {
            state: crate::KeyState::Released,
            ..press
        }
    ));

    assert_eq!(
        app.seen,
        ["right_click 3 4 5", "key 3 Character(\"k\")",],
        "a release is visible only to an app that overrides `rich_key`"
    );
}

/// The three opt-in hooks are off for an app that does not mention them: no
/// accessibility provider is asked for, no composition area is named, and the
/// untranslated event has nowhere to go. That is what keeps the app shell,
/// which implements none of them, on exactly the path it had.
#[test]
fn an_app_that_names_no_opt_in_hook_asks_for_nothing() {
    let app = Minimal::default();
    let key = WindowKey(1);
    assert!(app.accessibility_wake(key).is_none());
    assert_eq!(app.ime_cursor_area(key), None);
    assert!(!app.wants_ime(key));
    assert!(app.seen.is_empty());
}

/// Where the hooks sit in the runner cannot be driven without a real window,
/// so their order is pinned by reading the source: the untranslated event
/// comes before the discipline, the composition area after it and before the
/// settle that may drop the window, and the provider is installed before
/// anything that can show the window.
#[test]
fn the_hooks_are_called_in_the_order_their_contracts_state() {
    let runner = include_str!("../multi.rs");
    let start = runner
        .find("    fn window_event(&mut self, events: &ActiveEventLoop")
        .expect("the runner still has `window_event`");
    let body = &runner[start..];
    let body = &body[..body.find("\n    }\n").expect("`window_event` closes")];
    let at = |needle: &str| {
        body.find(needle)
            .unwrap_or_else(|| panic!("`window_event` no longer contains `{needle}`"))
    };
    assert!(at("self.app.raw_event(") < at("dispatch::dispatch("));
    assert!(at("dispatch::dispatch(") < at("dispatch::sync_ime_area("));
    assert!(at("dispatch::sync_ime_area(") < at("self.settle(events)"));
    assert!(at("self.app.wants_ime(key)") < at("dispatch::sync_ime_area("));

    let gpu = include_str!("../gpu.rs");
    let start = gpu.find("    pub fn open<T: DispatchApp>(").unwrap();
    let open = &gpu[start..];
    let created = open.find("app.created(events, &window);").unwrap();
    assert!(open.find("events.create_window(").unwrap() < created);
    for shows in ["window.set_visible(true)", "reveal::when_painted("] {
        assert!(created < open.find(shows).unwrap(), "{shows}");
    }
}
