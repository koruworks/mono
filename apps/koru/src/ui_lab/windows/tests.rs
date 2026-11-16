// SPDX-License-Identifier: MPL-2.0

//! The two windows driven the way the runner drives them: an untranslated
//! event, then the ordinary method it becomes, then the frame it asked for.
//! A real text engine and a scene; no window, GPU, event loop or provider.
//!
//! What cannot be reached from here is said where it is skipped: the F12 key
//! itself (a key event cannot be constructed outside winit, so the inspector
//! is opened through the facade's `toggle`, which is what the key calls), and
//! everything `raw_event` reads from a live `Window`.

use std::cell::RefCell;
use std::rc::Rc;

use accesskit::{Action, ActionRequest, TreeId, TreeUpdate};
use koru_window::{AccessibilityPending, AccessibilityProvider};
use koru_workspace_host::accessibility::{ControlPart, SemanticNode, WindowSemantics};
use winit::dpi::{LogicalSize, PhysicalPosition, Size};
use winit::event::DeviceId;
use winit::window::WindowLevel;

use super::super::placement::Placement;
use super::super::registry;
use super::*;

const SIZE: (f32, f32) = (1120.0, 720.0);
const PANEL_SIZE: (f32, f32) = (520.0, 720.0);

fn english<T>(body: impl FnOnce() -> T) -> T {
    koru_i18n::with_current(koru_i18n::Lang::En, body)
}

/// The two windows, the Lab's drawn once.
fn opened() -> (LabWindows, TextEngine) {
    let mut engine = TextEngine::new();
    let entry = registry::story(registry::BUTTON_DIALOG_STORY)
        .expect("the trial story is registered")
        .0;
    let lab = Lab::new(&mut engine, Placement::in_memory(Some(SIZE)), entry);
    let mut windows = LabWindows::new(lab);
    frame(&mut windows, &mut engine);
    (windows, engine)
}

fn frame(windows: &mut LabWindows, engine: &mut TextEngine) {
    windows.draw(LAB, &mut Scene::new(), engine, SIZE.0, SIZE.1);
}

fn panel_frame(windows: &mut LabWindows, engine: &mut TextEngine) {
    windows.draw(PANEL, &mut Scene::new(), engine, PANEL_SIZE.0, PANEL_SIZE.1);
}

/// What F12 does in the Lab's window, without the key.
fn f12(windows: &mut LabWindows) {
    windows.inspector.toggle(SIZE);
    windows.follow_inspector();
}

fn moved(at: (f32, f32)) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: PhysicalPosition::new(f64::from(at.0), f64::from(at.1)),
    }
}

fn mouse(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state,
        button: MouseButton::Left,
    }
}

fn tree(windows: &LabWindows) -> WindowSemantics {
    let mut slots = Slots::default();
    windows
        .lab
        .window_semantics(slots.window(), slots.view(), 1.0)
        .expect("a drawn Lab has a tree")
}

fn node(windows: &LabWindows, part: &str) -> SemanticNode {
    fn find(nodes: &[SemanticNode], part: &str) -> Option<SemanticNode> {
        nodes.iter().find_map(|node| {
            if node.occurrence.part.name() == part {
                Some(node.clone())
            } else {
                find(&node.children, part)
            }
        })
    }
    find(&tree(windows).children, part).unwrap_or_else(|| panic!("no `{part}` in the tree"))
}

fn centre(windows: &LabWindows, part: &str) -> (f32, f32) {
    node(windows, part)
        .bounds
        .expect("the control is placed")
        .centre()
}

fn result(windows: &LabWindows) -> String {
    node(windows, "result").label.expect("the result is said")
}

/// The pointer arrives at `at` in the Lab's window, as the runner delivers
/// it: the untranslated event, then `pointer_moved`.
fn point(windows: &mut LabWindows, at: (f32, f32)) -> bool {
    windows.source_event(&moved(at));
    windows.pointer_moved(LAB, at.0, at.1)
}

/// A full press and release at `at` in the Lab's window.
fn press(windows: &mut LabWindows, engine: &mut TextEngine, at: (f32, f32)) {
    windows.source_event(&mouse(ElementState::Pressed));
    windows.click(LAB, engine, at.0, at.1, false);
    windows.source_event(&mouse(ElementState::Released));
    windows.mouse_up(LAB);
}

// ------------------------------------------------------------- lifetimes

#[test]
fn the_lab_is_one_window_until_f12_and_one_again_after() {
    let (mut windows, _) = opened();
    assert_eq!(windows.windows(), [LAB]);
    assert!(!windows.take_activation_request(LAB));

    f12(&mut windows);
    assert_eq!(windows.windows(), [LAB, PANEL]);
    assert!(!windows.should_exit());

    // Live, so the next F12 closes it, and the keyboard is the Lab's again.
    f12(&mut windows);
    assert_eq!(windows.windows(), [LAB]);
    assert!(windows.take_activation_request(LAB));
    assert!(!windows.take_activation_request(LAB), "asked for once");
    assert!(!windows.take_activation_request(PANEL));
    assert!(!windows.should_exit());
}

#[test]
fn closing_the_panel_keeps_the_lab_and_closing_the_lab_ends_the_process() {
    let (mut windows, mut engine) = opened();
    f12(&mut windows);
    panel_frame(&mut windows, &mut engine);

    windows.close_requested(PANEL);
    assert_eq!(windows.windows(), [LAB]);
    assert!(!windows.should_exit(), "the Lab outlives its inspector");
    assert!(windows.take_activation_request(LAB));
    assert!(
        windows.maintain(LAB, Instant::now()),
        "the Lab repaints without the highlight"
    );

    // Reopened, then the Lab itself is closed: the process ends, and the
    // runner drops the panel with it.
    f12(&mut windows);
    windows.close_requested(LAB);
    assert!(windows.should_exit());
}

#[test]
fn both_windows_are_painted_before_they_are_shown() {
    let (windows, _) = opened();
    assert!(windows.paint_before_showing(LAB));
    assert!(windows.paint_before_showing(PANEL));
}

// ------------------------------------------------------------- inspection

#[test]
fn a_closed_inspector_adds_nothing_to_a_frame_or_an_event() {
    english(|| {
        let (mut windows, mut engine) = opened();
        assert!(!koru_draw::inspect::enabled());
        assert_eq!(windows.inspector.capture_micros(), 0);
        assert!(!windows.panel_seat.stale && !windows.lab_seat.stale);

        // The Lab is driven exactly as it was when it was the only window.
        let subject = centre(&windows, "subject");
        assert!(point(&mut windows, subject));
        assert!(!windows.withheld);
        press(&mut windows, &mut engine, subject);
        frame(&mut windows, &mut engine);
        assert_eq!(result(&windows), "Presses: 1");
        assert_eq!(windows.windows(), [LAB]);
        assert!(windows.inspector.selected_ancestry().is_empty());
    });
}

/// F12's tree shows the production button inside the Lab's own elements:
/// real ancestry, from the frame that was painted.
#[test]
fn a_pick_names_the_production_button_inside_the_labs_own_elements() {
    english(|| {
        let (mut windows, mut engine) = opened();
        f12(&mut windows);
        let subject = centre(&windows, "subject");
        point(&mut windows, subject);
        frame(&mut windows, &mut engine);

        let ancestry = windows.inspector.selected_ancestry();
        let expected = [
            OWNER,
            "UI Lab",
            "Workbench",
            "Preview",
            "Preview stage",
            "Component under test",
            "KoruButton",
        ];
        assert_eq!(
            &ancestry[..expected.len().min(ancestry.len())],
            expected,
            "{ancestry:?}"
        );
        assert!(
            windows.inspector.capture_micros() > 0,
            "the capture is timed"
        );
        assert!(windows.panel_seat.stale, "the panel shows the new pick");

        // The toolbar's Reset is the shared compact button, in its own row.
        let reset = centre(&windows, "reset");
        point(&mut windows, reset);
        frame(&mut windows, &mut engine);
        let ancestry = windows.inspector.selected_ancestry();
        let expected = [OWNER, "UI Lab", "Toolbar", "Reset", "KoruButton"];
        assert_eq!(
            &ancestry[..expected.len().min(ancestry.len())],
            expected,
            "{ancestry:?}"
        );
    });
}

#[test]
fn while_a_target_is_picked_the_press_is_the_inspectors_and_then_the_labs_again() {
    english(|| {
        let (mut windows, mut engine) = opened();
        f12(&mut windows);
        let subject = centre(&windows, "subject");
        point(&mut windows, subject);
        frame(&mut windows, &mut engine);
        assert_eq!(
            windows.cursor_icon(LAB, subject.0, subject.1),
            CursorIcon::Default
        );

        // The press that pins the pick does not press the button.
        windows.source_event(&mouse(ElementState::Pressed));
        assert!(windows.withheld);
        assert!(!windows.wants_drag(LAB));
        windows.click(LAB, &mut engine, subject.0, subject.1, false);
        windows.source_event(&mouse(ElementState::Released));
        assert!(windows.withheld, "nor does its release reach the Lab");
        windows.mouse_up(LAB);
        frame(&mut windows, &mut engine);
        assert_eq!(result(&windows), "Not pressed yet");

        // Pinned: the Lab has its window back, with the panel still open.
        assert_eq!(windows.windows(), [LAB, PANEL]);
        point(&mut windows, subject);
        assert!(!windows.withheld);
        press(&mut windows, &mut engine, subject);
        frame(&mut windows, &mut engine);
        assert_eq!(result(&windows), "Presses: 1");
    });
}

#[test]
fn a_pick_on_the_caption_does_not_drag_or_close_the_window() {
    english(|| {
        let (mut windows, mut engine) = opened();
        let close = centre(&windows, "close");
        let title = (40.0, close.1);
        assert!(windows.is_caption(LAB, title.0, title.1));

        f12(&mut windows);
        point(&mut windows, title);
        frame(&mut windows, &mut engine);
        windows.source_event(&mouse(ElementState::Pressed));
        assert!(!windows.is_caption(LAB, title.0, title.1));
        assert_eq!(
            windows.resize_direction(LAB, SIZE.0, SIZE.1, 1.0, 300.0, false),
            None
        );

        // Picking again, over the close control: the click is not a close.
        f12(&mut windows);
        point(&mut windows, close);
        frame(&mut windows, &mut engine);
        windows.source_event(&mouse(ElementState::Pressed));
        windows.click(LAB, &mut engine, close.0, close.1, false);
        assert!(!windows.should_exit());
    });
}

#[test]
fn a_change_made_in_one_window_is_a_frame_owed_by_the_other() {
    english(|| {
        let (mut windows, mut engine) = opened();
        f12(&mut windows);
        panel_frame(&mut windows, &mut engine);
        let subject = centre(&windows, "subject");
        point(&mut windows, subject);
        frame(&mut windows, &mut engine);
        windows.take_stale(PANEL);
        windows.take_stale(LAB);

        // A tree row chosen in the panel moves the highlight in the Lab. The
        // tree has scrolled to the pick, so the row at its top is one of the
        // pick's ancestors or neighbours, never the pick itself.
        let picked = windows.inspector.selected_ancestry();
        windows.panel_event(&moved((60.0, 111.0)));
        assert!(windows.pointer_moved(PANEL, 60.0, 111.0));
        windows.panel_event(&mouse(ElementState::Pressed));
        assert!(windows.click(PANEL, &mut engine, 60.0, 111.0, false));
        let chosen = windows.inspector.selected_ancestry();
        assert_ne!(chosen, picked);
        assert_eq!(chosen.first(), Some(&OWNER));
        assert!(
            windows.maintain(LAB, Instant::now()),
            "the Lab owes a frame"
        );
        assert!(!windows.maintain(LAB, Instant::now()), "and owes it once");
    });
}

// ----------------------------------------------------- the panel's window

#[test]
fn the_panel_opens_as_the_inspectors_window_and_reopens_as_it_was_left() {
    english(|| {
        let (mut windows, _) = opened();
        let first = windows.window(PANEL);
        assert_eq!(
            first.inner_size,
            Some(Size::Logical(LogicalSize::new(
                f64::from(PANEL_SIZE.0),
                f64::from(PANEL_SIZE.1)
            )))
        );
        assert_eq!(first.position, None, "the OS places a first panel");
        assert_eq!(first.window_level, WindowLevel::Normal);
        assert!(!first.decorations);
        assert_ne!(first.title, windows.window(LAB).title);
        assert!(!windows.pinned(PANEL) && !windows.pinned(LAB));

        f12(&mut windows);
        windows.monitors = vec![(0, 0, 1920, 1080)];
        windows.close_requested(PANEL);
        windows.window_closing(
            PANEL,
            ClosingWindowState {
                position: Some(PhysicalPosition::new(300, 120)),
                inner_size: PhysicalSize::new(900, 1200),
                scale_factor: 1.5,
                maximised: false,
                pinned: true,
            },
        );
        let again = windows.window(PANEL);
        assert_eq!(
            again.inner_size,
            Some(Size::Logical(LogicalSize::new(600.0, 800.0)))
        );
        assert_eq!(again.position, Some(PhysicalPosition::new(300, 120).into()));
        assert_eq!(again.window_level, WindowLevel::AlwaysOnTop);
        assert!(windows.pinned(PANEL));
        // The Lab's own window is untouched by any of it.
        assert_eq!(
            windows.window(LAB).inner_size,
            Some(Size::Logical(LogicalSize::new(
                f64::from(SIZE.0),
                f64::from(SIZE.1)
            )))
        );
    });
}

#[test]
fn a_press_on_the_panels_title_is_a_drag_asked_inside_that_press() {
    let (mut windows, mut engine) = opened();
    f12(&mut windows);
    panel_frame(&mut windows, &mut engine);
    windows.panel_event(&moved((18.0, 15.0)));
    windows.panel_event(&mouse(ElementState::Pressed));
    assert_eq!(
        windows.take_window_request(PANEL),
        Some(WindowRequest::Drag)
    );
    assert_eq!(windows.take_window_request(PANEL), None);
    assert_eq!(windows.take_window_request(LAB), None);
    assert!(!windows.is_caption(PANEL, 18.0, 15.0));
}

#[test]
fn the_pin_is_asked_of_the_runner_once_and_follows_its_answer() {
    let (mut windows, mut engine) = opened();
    f12(&mut windows);
    panel_frame(&mut windows, &mut engine);
    let out = windows
        .inspector
        .panel_action(ControlPart::new("inspect.pin"), Action::Click);
    windows.after_panel(out);
    assert_eq!(
        windows.take_window_request(PANEL),
        Some(WindowRequest::TogglePin)
    );
    // The runner raised the window and says so.
    assert!(windows.set_pinned(PANEL, true));
    windows.after_panel(PanelEvent::default());
    assert_eq!(windows.take_window_request(PANEL), None);
    assert!(!windows.set_pinned(LAB, true), "the Lab has no pin");
}

// ------------------------------------------------------------ composition

#[test]
fn only_the_labs_window_composes_and_only_at_the_label_fields_caret() {
    english(|| {
        let (mut windows, mut engine) = opened();
        assert!(windows.wants_ime(LAB));
        assert!(!windows.wants_ime(PANEL));
        assert_eq!(windows.ime_cursor_area(LAB), None);

        let field = node(&windows, "label").bounds.unwrap();
        press(&mut windows, &mut engine, field.centre());
        frame(&mut windows, &mut engine);
        let caret = windows
            .ime_cursor_area(LAB)
            .expect("the field is being typed in");
        assert!(caret.x >= field.x && caret.x <= field.x + field.w);
        assert!(caret.y >= field.y && caret.y + caret.h <= field.y + field.h);
        assert_eq!(windows.ime_cursor_area(PANEL), None);
    });
}

// ---------------------------------------------------------- accessibility

/// A provider that records instead of talking to a screen reader.
#[derive(Clone, Default)]
struct Reader {
    pending: Rc<RefCell<AccessibilityPending>>,
    published: Rc<RefCell<Vec<TreeUpdate>>>,
}

impl Reader {
    fn attach(&self) {
        self.pending.borrow_mut().initial_tree = true;
    }

    fn ask(&self, node: &SemanticNode, action: Action) {
        self.pending.borrow_mut().actions.push(ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: node.occurrence.node_id(),
            data: None,
        });
    }

    fn trees(&self) -> usize {
        self.published.borrow().len()
    }
}

impl AccessibilityProvider for Reader {
    fn process_event(&mut self, _window: &Window, _event: &WindowEvent) {}

    fn take_pending(&mut self) -> AccessibilityPending {
        std::mem::take(&mut self.pending.borrow_mut())
    }

    fn publish(&mut self, build: &mut dyn FnMut() -> TreeUpdate) -> bool {
        self.published.borrow_mut().push(build());
        true
    }
}

/// Give `key`'s window a recording provider, as the runner would a real one.
fn seat_reader(windows: &mut LabWindows, key: WindowKey) -> Reader {
    let reader = Reader::default();
    let (window, view) = (windows.slots.window(), windows.slots.view());
    windows.seat(key).access = Some(PaneAccessibility::new(
        window,
        view,
        Some(Box::new(reader.clone())),
    ));
    windows.seated(key, WindowId::dummy());
    reader
}

/// The node the Lab's seated window publishes for `part`: its identity is
/// the seated window's, which a request has to name.
fn seated_node(windows: &LabWindows, part: &str) -> SemanticNode {
    let access = windows.lab_seat.access.as_ref().expect("the Lab is seated");
    let semantics = windows
        .lab
        .window_semantics(access.slot(), access.view_slot(), 1.0)
        .unwrap();
    fn find(nodes: &[SemanticNode], part: &str) -> Option<SemanticNode> {
        nodes.iter().find_map(|node| {
            if node.occurrence.part.name() == part {
                Some(node.clone())
            } else {
                find(&node.children, part)
            }
        })
    }
    find(&semantics.children, part).unwrap_or_else(|| panic!("no `{part}` in the tree"))
}

#[test]
fn nothing_is_published_until_a_reader_attaches_and_then_only_on_change() {
    english(|| {
        let (mut windows, mut engine) = opened();
        let reader = seat_reader(&mut windows, LAB);
        for _ in 0..3 {
            assert!(!windows.maintain(LAB, Instant::now()));
        }
        assert_eq!(reader.trees(), 0, "no client, no tree");

        reader.attach();
        windows.maintain(LAB, Instant::now());
        assert_eq!(reader.trees(), 1);
        // Settling again, and hovering, publish nothing: nothing changed.
        let subject = centre(&windows, "subject");
        point(&mut windows, subject);
        frame(&mut windows, &mut engine);
        windows.maintain(LAB, Instant::now());
        assert_eq!(reader.trees(), 1);

        press(&mut windows, &mut engine, subject);
        windows.maintain(LAB, Instant::now());
        assert_eq!(reader.trees(), 2, "the press changed what is said");
        let counters = windows.lab_seat.access.as_ref().unwrap().counters();
        assert_eq!(counters.duplicate_ids, 0);
        assert_eq!(counters.unresolved_focus, 0);
    });
}

#[test]
fn a_readers_click_presses_the_button_on_the_next_frame() {
    english(|| {
        let (mut windows, mut engine) = opened();
        let reader = seat_reader(&mut windows, LAB);
        reader.attach();
        windows.maintain(LAB, Instant::now());

        reader.ask(&seated_node(&windows, "subject"), Action::Click);
        assert!(
            windows.maintain(LAB, Instant::now()),
            "the request asks for the frame that will do it"
        );
        assert_eq!(result(&windows), "Not pressed yet");
        frame(&mut windows, &mut engine);
        assert_eq!(result(&windows), "Presses: 1");

        windows.maintain(LAB, Instant::now());
        assert_eq!(reader.trees(), 2, "and the new count is published");
        let counters = windows.lab_seat.access.as_ref().unwrap().counters();
        assert_eq!(counters.actions_routed, 1);
        assert_eq!(counters.actions_refused, 0);
    });
}

#[test]
fn the_panel_publishes_its_own_tree_and_its_close_closes_it() {
    english(|| {
        let (mut windows, mut engine) = opened();
        f12(&mut windows);
        panel_frame(&mut windows, &mut engine);
        let reader = seat_reader(&mut windows, PANEL);
        reader.attach();
        windows.maintain(PANEL, Instant::now());
        assert_eq!(reader.trees(), 1);

        let access = windows.panel_seat.access.as_ref().unwrap();
        let semantics = windows
            .inspector
            .panel_semantics(access.slot(), 1.0, PANEL_SIZE)
            .unwrap();
        let close = semantics
            .children
            .iter()
            .find(|node| node.occurrence.part.name() == "inspect.close")
            .expect("the panel has a close control");
        reader.ask(close, Action::Click);
        windows.maintain(PANEL, Instant::now());
        assert_eq!(windows.windows(), [LAB]);
        assert!(windows.take_activation_request(LAB));

        // The runner drops the window: its provider is retired with it.
        windows.window_closing(
            PANEL,
            ClosingWindowState {
                position: None,
                inner_size: PhysicalSize::new(520, 720),
                scale_factor: 1.0,
                maximised: false,
                pinned: false,
            },
        );
        assert!(windows.panel_seat.access.is_none());
    });
}

#[test]
fn without_a_loop_there_is_no_provider_to_ask_for() {
    let (windows, _) = opened();
    assert!(windows.accessibility_wake(LAB).is_none());
    assert!(windows.accessibility_wake(PANEL).is_none());
}

// ------------------------------------------------------------ appearance

/// The theme is process-wide, so the inspector's panel is drawn in whatever
/// the Lab's toolbar chose. Nothing of the panel's own says so: the Lab's
/// window tells it to draw again. With the panel closed there is nobody to
/// tell.
#[test]
fn a_theme_chosen_in_the_lab_redraws_the_inspectors_panel() {
    use koru_draw::theme::themes;
    // The theme lock for as long as this runs, and the theme put back.
    struct Restore(&'static themes::Theme);
    impl Drop for Restore {
        fn drop(&mut self) {
            themes::set_active(self.0);
        }
    }
    let _lock = crate::tests::lock_theme();
    let _restore = Restore(themes::active());
    themes::set_active(&themes::LIGHT_THEME);
    english(|| {
        let (mut windows, mut engine) = opened();
        let choose = |windows: &mut LabWindows, engine: &mut TextEngine, theme: &str| {
            let field = centre(windows, "theme");
            point(windows, field);
            press(windows, engine, field);
            frame(windows, engine);
            let option = centre(windows, theme);
            point(windows, option);
            press(windows, engine, option);
            frame(windows, engine);
        };
        // Closed: the choice is made and nothing is owed to a panel.
        choose(&mut windows, &mut engine, "dark");
        assert!(std::ptr::eq(themes::active(), &themes::DARK_THEME));
        assert!(!windows.panel_seat.stale);

        // Open, and past the pick that opening starts with.
        f12(&mut windows);
        let subject = centre(&windows, "subject");
        point(&mut windows, subject);
        frame(&mut windows, &mut engine);
        press(&mut windows, &mut engine, subject);
        frame(&mut windows, &mut engine);
        panel_frame(&mut windows, &mut engine);
        windows.panel_seat.stale = false;

        choose(&mut windows, &mut engine, "light");
        assert!(std::ptr::eq(themes::active(), &themes::LIGHT_THEME));
        assert!(
            windows.panel_seat.stale,
            "the panel is asked to draw in the new theme"
        );
    });
}
