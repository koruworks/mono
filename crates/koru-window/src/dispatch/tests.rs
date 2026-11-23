// SPDX-License-Identifier: MPL-2.0

//! What can be asserted about the discipline without a GPU.
//!
//! The arithmetic the loop does on its own account, plus the window effects it
//! performs — which are assertable at all only because [`WindowHandle`] exists.
//! The sequences that must behave the same through both runners are next door
//! in [`super::parity`].

use std::cell::{Cell, RefCell};

use super::*;

/// A window that records instead of existing.
pub(crate) struct FakeWindow {
    pub size: Cell<PhysicalSize<u32>>,
    pub scale: Cell<f64>,
    pub maximized: Cell<bool>,
    pub position: Cell<Option<PhysicalPosition<i32>>>,
    pub log: RefCell<Vec<String>>,
}

impl Default for FakeWindow {
    fn default() -> Self {
        Self {
            size: Cell::new(PhysicalSize::new(800, 600)),
            scale: Cell::new(1.0),
            maximized: Cell::new(false),
            position: Cell::new(Some(PhysicalPosition::new(10, 20))),
            log: RefCell::new(Vec::new()),
        }
    }
}

impl FakeWindow {
    fn say(&self, line: impl Into<String>) {
        self.log.borrow_mut().push(line.into());
    }

    pub fn taken(&self) -> Vec<String> {
        std::mem::take(&mut self.log.borrow_mut())
    }
}

impl WindowHandle for FakeWindow {
    fn inner_size(&self) -> PhysicalSize<u32> {
        self.size.get()
    }

    fn outer_position(&self) -> Option<PhysicalPosition<i32>> {
        self.position.get()
    }

    fn scale_factor(&self) -> f64 {
        self.scale.get()
    }

    fn is_maximized(&self) -> bool {
        self.maximized.get()
    }

    fn request_redraw(&self) {
        self.say("request_redraw");
    }

    fn set_cursor(&self, icon: CursorIcon) {
        self.say(format!("set_cursor {icon:?}"));
    }

    fn drag_window(&self) {
        self.say("drag_window");
    }

    fn drag_resize_window(&self, direction: ResizeDirection) {
        self.say(format!("drag_resize_window {direction:?}"));
    }

    fn set_maximized(&self, maximized: bool) {
        self.maximized.set(maximized);
        self.say(format!("set_maximized {maximized}"));
    }

    fn set_minimized(&self, minimized: bool) {
        self.say(format!("set_minimized {minimized}"));
    }

    fn focus_window(&self) {
        self.say("focus_window");
    }

    fn set_always_on_top(&self, on: bool) {
        self.say(format!("set_always_on_top {on}"));
    }

    fn apply_frame(&self, square: bool) {
        self.say(format!("apply_frame {square}"));
    }

    fn place(&self, what: Placement, default_size: (u32, u32)) {
        self.say(format!("place {what:?} {default_size:?}"));
    }

    fn activate_app(&self) {
        self.say("activate_app");
    }

    fn set_ime_cursor_area(&self, area: Rect) {
        self.say(format!(
            "set_ime_cursor_area {} {} {} {}",
            area.x, area.y, area.w, area.h
        ));
    }
}

fn moments_ago(millis: u64) -> Instant {
    Instant::now() - Duration::from_millis(millis)
}

#[test]
fn a_second_press_soon_and_near_is_a_double_click() {
    assert!(continues_press(
        Some((moments_ago(50), 100.0, 40.0, 1)),
        101.0,
        41.0
    ));
}

#[test]
fn a_third_press_soon_and_near_is_a_triple_click() {
    assert_eq!(
        next_click_count(Some((moments_ago(50), 100.0, 40.0, 2)), 101.0, 41.0),
        3
    );
}

#[test]
fn a_second_press_that_is_late_is_a_click_of_its_own() {
    assert!(!continues_press(
        Some((moments_ago(1_000), 100.0, 40.0, 1)),
        100.0,
        40.0
    ));
}

#[test]
fn a_second_press_somewhere_else_is_a_click_of_its_own() {
    // Without the distance test, clicking one row and then quickly clicking a
    // different one selects a word in the second — the gesture is fast, and the
    // interval alone cannot tell the two apart.
    assert!(!continues_press(
        Some((moments_ago(50), 100.0, 40.0, 1)),
        400.0,
        40.0
    ));
    assert!(!continues_press(
        Some((moments_ago(50), 100.0, 40.0, 1)),
        100.0,
        300.0
    ));
}

#[test]
fn the_first_press_of_all_is_never_a_double_click() {
    assert!(!continues_press(None, 10.0, 10.0));
}

#[test]
fn resetting_a_window_returns_to_the_size_the_app_asks_for() {
    // `Placement::Reset` has to name a size, and the honest one is whatever a
    // fresh window of this app would have been — not a constant the loop keeps.
    let attributes =
        Window::default_attributes().with_inner_size(winit::dpi::LogicalSize::new(640.0, 480.0));
    assert_eq!(default_size(&attributes), (640, 480));
}

#[test]
fn an_app_that_names_no_size_still_gets_one() {
    // A fallback rather than a panic: `Reset` on a window that never asked for
    // a size is an odd thing to do, and doing something reasonable beats
    // bringing the process down over it.
    assert_eq!(default_size(&Window::default_attributes()), (1120, 800));
}

#[test]
fn a_wait_until_deadline_runs_maintenance_without_an_input_event() {
    let now = Instant::now();
    assert!(starts_maintenance(&StartCause::ResumeTimeReached {
        start: now,
        requested_resume: now,
    }));
    assert!(!starts_maintenance(&StartCause::WaitCancelled {
        start: now,
        requested_resume: Some(now),
    }));
}

#[test]
fn an_unidentified_ctrl_alt_p_reaches_global_search_as_p() {
    use winit::keyboard::NativeKeyCode;

    let unidentified = Key::Unidentified(NativeKeyCode::Unidentified.into());
    let mods = ModifiersState::CONTROL | ModifiersState::ALT;
    assert_eq!(
        profile_switcher_key(&unidentified, PhysicalKey::Code(KeyCode::KeyP), mods),
        Some(Key::Character("p".into()))
    );
    assert_eq!(
        profile_switcher_key(
            &Key::Character("þ".into()),
            PhysicalKey::Code(KeyCode::KeyP),
            mods,
        ),
        None,
        "a real AltGr character belongs to the keyboard layout"
    );
}

#[test]
fn an_animation_polls_and_a_deadline_waits_until_it() {
    let deadline = Instant::now() + Duration::from_millis(20);
    assert_eq!(next_flow(true, None), ControlFlow::Poll);
    assert_eq!(
        next_flow(true, Some(deadline)),
        ControlFlow::Poll,
        "an animation outranks a deadline: the next frame is sooner than any of them"
    );
    assert_eq!(
        next_flow(false, Some(deadline)),
        ControlFlow::WaitUntil(deadline)
    );
    assert_eq!(
        next_flow(false, None),
        ControlFlow::Wait,
        "the resting state, and what keeps an idle windowless process at zero CPU"
    );
}

#[test]
fn seating_a_window_forgets_the_last_one_s_border_and_cursor() {
    // An overlay is dropped and reopened; a resize edge and a cursor shape are
    // facts about the surface that went away. The pointer position and the
    // modifiers are not, and deliberately survive.
    let mut input = WindowInput {
        pointer: (12.0, 34.0),
        resize_edge: Some(ResizeDirection::East),
        cursor_icon: CursorIcon::Text,
        mods: ModifiersState::SHIFT,
        ..WindowInput::default()
    };
    input.seat(true);
    assert_eq!(input.resize_edge, None);
    assert_eq!(input.cursor_icon, CursorIcon::Default);
    assert!(input.pinned);
    assert_eq!(input.pointer, (12.0, 34.0));
    assert_eq!(input.mods, ModifiersState::SHIFT);
}

#[test]
fn the_composition_area_reaches_the_os_once_per_change() {
    let window = FakeWindow::default();
    let mut input = WindowInput::default();
    let caret = Rect::new(40.0, 12.0, 1.0, 18.0);

    // An app with no opinion leaves the area alone.
    sync_ime_area(&mut input, Some(&window), None);
    assert!(window.taken().is_empty());

    sync_ime_area(&mut input, Some(&window), Some(caret));
    assert_eq!(window.taken(), ["set_ime_cursor_area 40 12 1 18"]);

    // The pointer moving over a field whose caret has not moved asks again
    // with the same answer, and that must not be a native call each time.
    sync_ime_area(&mut input, Some(&window), Some(caret));
    sync_ime_area(&mut input, Some(&window), None);
    assert!(window.taken().is_empty());

    let typed = Rect::new(48.0, 12.0, 1.0, 18.0);
    sync_ime_area(&mut input, Some(&window), Some(typed));
    assert_eq!(window.taken(), ["set_ime_cursor_area 48 12 1 18"]);

    // A new window has been told nothing, whatever the last one was told.
    input.seat(false);
    sync_ime_area(&mut input, Some(&window), Some(typed));
    assert_eq!(window.taken(), ["set_ime_cursor_area 48 12 1 18"]);
}

#[test]
fn a_composition_area_with_no_window_is_not_remembered_as_applied() {
    let mut input = WindowInput::default();
    let caret = Rect::new(40.0, 12.0, 1.0, 18.0);
    sync_ime_area::<FakeWindow>(&mut input, None, Some(caret));
    assert_eq!(input.ime_area, None);
}
