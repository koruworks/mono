// SPDX-License-Identifier: MPL-2.0

//! The facade driven the way a host drives it: events in, a frame drawn
//! between `begin_capture` and `end_capture`, and what it asks of the host
//! read back. No window, GPU or event loop.
//!
//! F12 itself is asked through [`HostedInspector::toggle`]: a key event
//! cannot be constructed outside winit, so the press filter in front of it
//! is asserted where the workspace's own inspector asserts it,
//! `inspector::tests::f12_focus_transfer_does_not_close_or_reopen_the_inspector`.

use winit::dpi::{LogicalSize, PhysicalPosition, Size};
use winit::event::DeviceId;

use super::*;

const SOURCE: (f32, f32) = (900.0, 700.0);
const OWNER: &str = "Test window";
/// One monitor, 1920 by 1080, at the desktop's origin.
const MONITOR: [(i32, i32, i32, i32); 1] = [(0, 0, 1920, 1080)];

fn moved(x: f64, y: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: PhysicalPosition::new(x, y),
    }
}

fn mouse(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state,
        button: MouseButton::Left,
    }
}

/// Draw one frame as a host does: a registered component with a part inside
/// it, through the shared draw seams. Returns what `end_capture` answered and
/// whether the frame was being measured while it was drawn.
fn frame(hosted: &mut HostedInspector) -> (bool, bool) {
    let mut canvas = Scene::new();
    let capture = hosted.begin_capture(OWNER, SOURCE);
    let measured = inspect::enabled();
    {
        let _component = inspect::scope(
            "KoruButton",
            "component",
            Affine::IDENTITY,
            scene::rect(20.0, 20.0, 100.0, 32.0),
            |_| {},
        );
        inspect::leaf(
            "label",
            Affine::IDENTITY,
            scene::rect(30.0, 24.0, 60.0, 20.0),
            |_| {},
        );
    }
    (hosted.end_capture(capture, &mut canvas), measured)
}

/// An open inspector with the pointer over the label and one frame taken.
fn picking_the_label() -> HostedInspector {
    let mut hosted = HostedInspector::in_memory();
    assert!(hosted.toggle(SOURCE));
    hosted.source_event(&moved(40.0, 30.0), 1.0, SOURCE);
    frame(&mut hosted);
    hosted
}

#[test]
fn a_closed_inspector_measures_nothing_and_takes_no_event() {
    let mut hosted = HostedInspector::in_memory();
    assert!(!hosted.is_open());
    assert!(hosted.begin_capture(OWNER, SOURCE).is_none());
    assert_eq!(frame(&mut hosted), (false, false));
    for event in [
        moved(40.0, 30.0),
        mouse(ElementState::Pressed),
        mouse(ElementState::Released),
    ] {
        assert_eq!(
            hosted.source_event(&event, 1.0, SOURCE),
            SourceEvent::default()
        );
        assert_eq!(hosted.panel_event(&event, 1.0), PanelEvent::default());
    }
    assert!(!hosted.wants_arrow_cursor());
    assert!(!hosted.take_panel_semantics_dirty());
    assert!(
        hosted
            .panel_semantics(WindowSlot::from_raw(2), 1.0, (520.0, 720.0))
            .is_none()
    );
    assert!(hosted.selected_ancestry().is_empty());
    assert_eq!(hosted.capture_micros(), 0);
}

#[test]
fn a_pick_names_the_part_its_component_and_the_window_that_drew_them() {
    let mut hosted = HostedInspector::in_memory();
    assert!(hosted.toggle(SOURCE));
    assert!(hosted.wants_arrow_cursor());

    // The pointer arrives over the label before any frame has been measured.
    let arrived = hosted.source_event(&moved(40.0, 30.0), 1.0, SOURCE);
    assert!(arrived.consumed && arrived.redraw_source && arrived.redraw_panel);
    assert!(hosted.selected_ancestry().is_empty());

    // The next frame is measured, and the pick is answered from it.
    let (panel_changed, measured) = frame(&mut hosted);
    assert!(measured && panel_changed);
    assert_eq!(hosted.selected_ancestry(), [OWNER, "KoruButton", "label"]);

    // Off the label and still on the button.
    hosted.source_event(&moved(110.0, 48.0), 1.0, SOURCE);
    assert_eq!(hosted.selected_ancestry(), [OWNER, "KoruButton"]);

    // Outside the button the window itself is what is there.
    hosted.source_event(&moved(400.0, 400.0), 1.0, SOURCE);
    assert_eq!(hosted.selected_ancestry(), [OWNER]);
}

#[test]
fn a_pick_is_in_the_hosts_own_logical_pixels_at_any_scale() {
    let mut hosted = HostedInspector::in_memory();
    hosted.toggle(SOURCE);
    // 150%: the pointer is reported in physical pixels, the frame is logical.
    hosted.source_event(&moved(60.0, 45.0), 1.5, SOURCE);
    frame(&mut hosted);
    assert_eq!(hosted.selected_ancestry(), [OWNER, "KoruButton", "label"]);
}

#[test]
fn picking_withholds_both_halves_of_the_press_and_freezes_what_was_measured() {
    let mut hosted = picking_the_label();
    let press = hosted.source_event(&mouse(ElementState::Pressed), 1.0, SOURCE);
    assert!(press.consumed && press.redraw_source && !press.closed);
    assert!(
        hosted
            .source_event(&mouse(ElementState::Released), 1.0, SOURCE)
            .consumed,
        "the release of a press the inspector took is not the host's either"
    );
    assert!(!hosted.wants_arrow_cursor());

    // Pinned: later frames are not measured, and the selection stays.
    assert!(hosted.begin_capture(OWNER, SOURCE).is_none());
    assert_eq!(frame(&mut hosted).1, false);
    assert_eq!(hosted.selected_ancestry(), [OWNER, "KoruButton", "label"]);

    // The host has its window back: the next press is its own.
    assert!(
        !hosted
            .source_event(&mouse(ElementState::Pressed), 1.0, SOURCE)
            .consumed
    );
}

#[test]
fn f12_resumes_a_pinned_pick_before_it_closes() {
    let mut hosted = picking_the_label();
    hosted.source_event(&mouse(ElementState::Pressed), 1.0, SOURCE);
    hosted.source_event(&mouse(ElementState::Released), 1.0, SOURCE);
    assert!(hosted.begin_capture(OWNER, SOURCE).is_none());

    assert!(hosted.toggle(SOURCE), "the first F12 resumes picking");
    assert!(hosted.wants_arrow_cursor());
    assert_eq!(frame(&mut hosted).1, true);

    assert!(!hosted.toggle(SOURCE), "the second closes");
    assert!(!hosted.is_open());
    assert_eq!(frame(&mut hosted), (false, false));
}

#[test]
fn a_frame_is_measured_only_while_the_inspection_is_live() {
    let mut hosted = picking_the_label();
    assert!(hosted.capture_micros() < 5_000_000, "a capture was timed");
    assert!(!inspect::enabled(), "nothing stays armed between frames");
    hosted.close();
    assert_eq!(frame(&mut hosted), (false, false));
    assert!(!inspect::enabled());
}

#[test]
fn a_press_on_the_panels_title_asks_the_host_for_a_native_drag() {
    let mut hosted = picking_the_label();
    hosted.panel_event(&moved(18.0, 15.0), 1.0);
    let press = hosted.panel_event(&mouse(ElementState::Pressed), 1.0);
    assert!(press.drag_window && !press.closed);
    assert!(hosted.is_open());
}

#[test]
fn a_tree_row_chosen_in_the_panel_repaints_the_inspected_window() {
    let mut hosted = picking_the_label();
    let before = hosted.selected_ancestry();
    // The tree's first row is the window that owns the frame.
    let tree_top = (PAD + 4.0, TREE_TOP + ROW / 2.0);
    hosted.panel_event(
        &moved(f64::from(tree_top.0 + 40.0), f64::from(tree_top.1)),
        1.0,
    );
    let press = hosted.panel_event(&mouse(ElementState::Pressed), 1.0);
    assert!(press.redraw_panel);
    assert_ne!(hosted.selected_ancestry(), before);
    assert_eq!(hosted.selected_ancestry(), [OWNER]);
    assert!(
        press.redraw_source,
        "the highlight is drawn in the inspected window"
    );
    // The panel's pointer never picks from the inspected window.
    frame(&mut hosted);
    assert_eq!(hosted.selected_ancestry(), [OWNER]);
}

#[test]
fn the_panels_close_control_closes_and_says_so() {
    let mut hosted = picking_the_label();
    let slot = WindowSlot::from_raw(7);
    let semantics = hosted
        .panel_semantics(slot, 1.25, (520.0, 720.0))
        .expect("an open panel has a tree");
    assert_eq!(semantics.window, slot);
    assert_eq!(semantics.scale, 1.25);
    let close = ControlPart::new("inspect.close");
    let node = semantics
        .children
        .iter()
        .find(|node| node.occurrence.part == close)
        .expect("the panel publishes its close control");
    assert_eq!(node.role, Role::Button);
    assert!(node.actions.contains(&Action::Click));
    assert!(node.label.as_ref().is_some_and(|label| !label.is_empty()));

    // Focus alone moves the keyboard and leaves the panel open.
    let focused = hosted.panel_action(close, Action::Focus);
    assert!(focused.redraw_panel && !focused.closed);
    assert!(hosted.take_panel_semantics_dirty());
    assert!(!hosted.take_panel_semantics_dirty());

    let clicked = hosted.panel_action(close, Action::Click);
    assert!(clicked.closed && clicked.redraw_source);
    assert!(!hosted.is_open());
    // A control the panel does not have asks for nothing.
    assert_eq!(
        hosted.panel_action(ControlPart::new("inspect.absent"), Action::Click),
        PanelEvent::default()
    );
}

#[test]
fn the_pin_follows_the_panel_and_is_what_the_next_panel_opens_with() {
    let mut hosted = picking_the_label();
    assert!(!hosted.panel_pinned());
    hosted.panel_action(ControlPart::new("inspect.pin"), Action::Click);
    assert!(hosted.panel_pinned());

    // The host reports the window's last state as it drops it.
    hosted.close();
    hosted.panel_closing(Some((100, 80)), (640.0, 800.0), false, true);
    assert!(hosted.panel_pinned(), "the pin outlives the closed panel");

    let window = hosted.panel_window(1.0, &MONITOR);
    assert_eq!(
        window.inner_size,
        Some(Size::Logical(LogicalSize::new(640.0, 800.0)))
    );
    assert_eq!(window.window_level, WindowLevel::AlwaysOnTop);
    assert_eq!(
        window.position,
        Some(PhysicalPosition::new(100, 80).into()),
        "reopened where it was"
    );
    assert!(!window.decorations);

    hosted.toggle(SOURCE);
    assert!(hosted.panel_pinned());
}

#[test]
fn a_remembered_position_no_monitor_holds_is_left_to_the_os() {
    let mut hosted = HostedInspector::in_memory();
    hosted.panel_closing(Some((9000, 9000)), (640.0, 800.0), false, false);
    assert_eq!(hosted.panel_window(1.0, &MONITOR).position, None);
    // The size is still the one it had.
    assert_eq!(
        hosted.panel_window(1.0, &MONITOR).inner_size,
        Some(Size::Logical(LogicalSize::new(640.0, 800.0)))
    );
}

#[test]
fn a_new_process_opens_the_panel_at_its_default_and_a_maximised_one_keeps_its_size() {
    let mut hosted = HostedInspector::in_memory();
    let default = Geometry::default();
    let window = hosted.panel_window(1.0, &MONITOR);
    assert_eq!(
        window.inner_size,
        Some(Size::Logical(LogicalSize::new(
            f64::from(default.size.0),
            f64::from(default.size.1)
        )))
    );
    assert_eq!(window.position, None);
    assert_eq!(window.window_level, WindowLevel::Normal);

    hosted.panel_closing(Some((100, 80)), (640.0, 800.0), false, false);
    // Closed while maximised: the restored size is the one worth keeping.
    hosted.panel_closing(Some((0, 0)), (1920.0, 1080.0), true, true);
    let window = hosted.panel_window(1.0, &MONITOR);
    assert_eq!(
        window.inner_size,
        Some(Size::Logical(LogicalSize::new(640.0, 800.0)))
    );
    assert_eq!(window.window_level, WindowLevel::AlwaysOnTop);
}

/// The code lines of a source file: a comment may name what the code avoids.
fn code_lines(source: &'static str) -> impl Iterator<Item = &'static str> {
    source
        .lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with("//"))
}

/// **No geometry file is read or written**, pinned by structure: a test
/// cannot assert a file was not touched without looking at a real machine's
/// state directory. The facade's only placement is the in-memory one, and
/// that constructor names no path, no row reader and no writer thread.
#[test]
fn the_facade_reaches_no_geometry_file() {
    let facade = include_str!("../hosted.rs");
    for forbidden in [
        "Placement::open",
        "window_geometry_path",
        "read_window_row",
        "WindowRowWriter",
        ".remember(",
        "std::fs",
    ] {
        assert!(
            code_lines(facade).all(|line| !line.contains(forbidden)),
            "the hosted inspector names `{forbidden}`"
        );
    }
    assert_eq!(
        code_lines(facade)
            .filter(|line| line.contains("Placement::in_memory("))
            .count(),
        1,
        "the facade makes its placement once, in memory"
    );

    let placement = include_str!("../placement.rs");
    let start = placement
        .find("pub fn in_memory(")
        .expect("placement.rs still has `in_memory`");
    let body = &placement[start..];
    let body = &body[..body.find("\n    }\n").expect("`in_memory` closes")];
    for forbidden in [
        "window_geometry_path",
        "read_window_row",
        "WindowRowWriter::open",
    ] {
        assert!(
            code_lines(body).all(|line| !line.contains(forbidden)),
            "`Placement::in_memory` names `{forbidden}`"
        );
    }
    assert!(body.contains("WindowRowWriter::default()"));
}
