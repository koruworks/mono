// SPDX-License-Identifier: MPL-2.0

//! The Lab's semantic tree read back from a drawn frame, and a screen
//! reader's requests made against it. In memory: a real text engine and a
//! scene, no window and no provider.
//!
//! The display language is set per thread, as in the shell's own tests.

use std::collections::{BTreeSet, HashMap};

use accesskit::Toggled;
use koru_workspace_host::accessibility::projection::build;

use super::*;

const SIZE: (f32, f32) = (1120.0, 720.0);
const WINDOW: WindowSlot = WindowSlot::from_raw(3);
const VIEW: ViewSlot = ViewSlot::from_raw(5);

fn english<T>(body: impl FnOnce() -> T) -> T {
    koru_i18n::with_current(koru_i18n::Lang::En, body)
}

fn draw(lab: &mut Lab, engine: &mut TextEngine) {
    let mut scene = vello::Scene::new();
    WindowApp::draw(lab, &mut scene, engine, SIZE.0, SIZE.1);
}

fn drawn() -> (Lab, TextEngine) {
    let mut engine = TextEngine::new();
    let entry = registry::story(registry::BUTTON_DIALOG_STORY)
        .expect("the trial story is registered")
        .0;
    let mut lab = Lab::new(&mut engine, Placement::in_memory(Some(SIZE)), entry);
    draw(&mut lab, &mut engine);
    (lab, engine)
}

fn tree(lab: &Lab) -> WindowSemantics {
    lab.window_semantics(WINDOW, VIEW, 1.0)
        .expect("a drawn Lab has a tree")
}

/// Every node, parents before children.
fn flat(nodes: &[SemanticNode]) -> Vec<&SemanticNode> {
    let mut all = Vec::new();
    for node in nodes {
        all.push(node);
        all.extend(flat(&node.children));
    }
    all
}

/// The locator of an occurrence without its window and view numbers.
fn tail(occurrence: &Occurrence) -> String {
    format!("{}#{}", occurrence.object, occurrence.part)
}

fn find<'a>(semantics: &'a WindowSemantics, locator: &str) -> &'a SemanticNode {
    flat(&semantics.children)
        .into_iter()
        .find(|node| tail(&node.occurrence) == locator)
        .unwrap_or_else(|| panic!("the tree has no `{locator}`"))
}

/// A request as the lifecycle resolves one.
fn request(lab: &Lab, locator: &str, action: Action, data: Option<ActionData>) -> Resolved {
    Resolved {
        occurrence: find(&tree(lab), locator).occurrence,
        action,
        data,
    }
}

fn ask(lab: &mut Lab, engine: &mut TextEngine, locator: &str, action: Action) -> bool {
    let request = request(lab, locator, action, None);
    let changed = lab.accessibility_action(engine, &request);
    draw(lab, engine);
    changed
}

const SUBJECT_AT: &str = "s:ui-lab#subject";
const RESET_AT: &str = "s:ui-lab#reset";
const RESULT_AT: &str = "s:ui-lab#result";
const FIELD_AT: &str = "s:ui-lab.control#label";
const STYLE_AT: &str = "s:ui-lab.control#style";
const ENTRY_AT: &str = "s:ui-lab.navigator#C-BUTTON";
const CLOSE_AT: &str = "s:window#close";

#[test]
fn nothing_is_published_before_the_first_frame() {
    let mut engine = TextEngine::new();
    let entry = registry::story(registry::BUTTON_DIALOG_STORY).unwrap().0;
    let lab = Lab::new(&mut engine, Placement::in_memory(None), entry);
    assert!(lab.window_semantics(WINDOW, VIEW, 1.0).is_none());
    assert_eq!(lab.frame_size(), None);
}

#[test]
fn the_tree_names_every_control_by_role_name_and_what_it_can_be_asked() {
    english(|| {
        let (lab, _) = drawn();
        let semantics = tree(&lab);
        assert_eq!(semantics.label, "Koru UI Lab");
        assert_eq!(semantics.window, WINDOW);
        assert_eq!(semantics.client, Rect::new(0.0, 0.0, SIZE.0, SIZE.1));
        assert_eq!(semantics.focus, None, "nothing has the keyboard yet");

        let described: Vec<(String, NativeRole, Option<&str>, Vec<Action>)> =
            flat(&semantics.children)
                .into_iter()
                .map(|node| {
                    (
                        tail(&node.occurrence),
                        node.role,
                        node.label.as_deref(),
                        node.actions.clone(),
                    )
                })
                .collect();
        let click_focus = vec![Action::Click, Action::Focus];
        assert_eq!(
            described,
            [
                (
                    "s:window#minimize".to_owned(),
                    NativeRole::Button,
                    Some("Minimize"),
                    vec![]
                ),
                (
                    "s:window#maximize".to_owned(),
                    NativeRole::Button,
                    Some("Maximize"),
                    vec![]
                ),
                (
                    CLOSE_AT.to_owned(),
                    NativeRole::Button,
                    Some("Close window"),
                    vec![Action::Click]
                ),
                (
                    RESET_AT.to_owned(),
                    NativeRole::Button,
                    Some("Reset"),
                    click_focus.clone()
                ),
                (
                    ENTRY_AT.to_owned(),
                    NativeRole::ListItem,
                    Some("Button"),
                    click_focus.clone()
                ),
                (
                    SUBJECT_AT.to_owned(),
                    NativeRole::Button,
                    Some("Save changes"),
                    click_focus
                ),
                (
                    FIELD_AT.to_owned(),
                    NativeRole::TextInput,
                    Some("Label"),
                    vec![Action::Focus, Action::SetValue]
                ),
                (
                    STYLE_AT.to_owned(),
                    NativeRole::Group,
                    Some("Style"),
                    vec![Action::Focus]
                ),
                (
                    "s:ui-lab.style#secondary".to_owned(),
                    NativeRole::RadioButton,
                    Some("Secondary"),
                    vec![Action::Click]
                ),
                (
                    "s:ui-lab.style#primary".to_owned(),
                    NativeRole::RadioButton,
                    Some("Primary"),
                    vec![Action::Click]
                ),
                (
                    "s:ui-lab.style#destructive".to_owned(),
                    NativeRole::RadioButton,
                    Some("Destructive"),
                    vec![Action::Click]
                ),
                (
                    RESULT_AT.to_owned(),
                    NativeRole::Label,
                    Some("Not pressed yet"),
                    vec![]
                ),
            ]
        );
        assert_eq!(
            find(&semantics, FIELD_AT).value.as_deref(),
            Some("Save changes")
        );
        assert_eq!(find(&semantics, RESULT_AT).live, Some(Live::Polite));
        assert_eq!(
            find(&semantics, "s:ui-lab.style#secondary").toggled,
            Some(Toggled::True),
            "the style the button opens in is the checked one"
        );
        assert_eq!(
            find(&semantics, "s:ui-lab.style#primary").toggled,
            Some(Toggled::False)
        );
    });
}

/// Identity is a code keyword: the same locators in every language, and no
/// two controls sharing a node id.
#[test]
fn identity_is_the_same_in_every_language_and_never_shared() {
    let mut baseline: Option<Vec<String>> = None;
    for lang in koru_i18n::Lang::ALL {
        koru_i18n::with_current(lang, || {
            let (lab, _) = drawn();
            let semantics = tree(&lab);
            let locators: Vec<String> = flat(&semantics.children)
                .into_iter()
                .map(|node| node.occurrence.locator())
                .collect();
            let distinct: BTreeSet<&String> = locators.iter().collect();
            assert_eq!(distinct.len(), locators.len(), "{lang:?}");
            for node in flat(&semantics.children) {
                assert!(
                    node.label.as_ref().is_some_and(|label| !label.is_empty()),
                    "{lang:?}: `{}` has no name",
                    node.occurrence.locator()
                );
            }
            let projection = build(&semantics, &HashMap::new());
            assert_eq!(projection.duplicate_ids, 0, "{lang:?}");
            assert!(!projection.unresolved_focus, "{lang:?}");
            match &baseline {
                Some(first) => assert_eq!(first, &locators, "{lang:?}"),
                None => baseline = Some(locators),
            }
        });
    }
}

/// Where a control is announced is where it is drawn and pressed: every
/// rectangle is the frame's own.
#[test]
fn every_node_stands_where_its_control_was_drawn() {
    english(|| {
        let (lab, _) = drawn();
        let semantics = tree(&lab);
        for (locator, key) in [
            (SUBJECT_AT, SUBJECT),
            (RESET_AT, RESET),
            (FIELD_AT, control_key(Control::Label)),
            (STYLE_AT, control_key(Control::Style)),
        ] {
            assert_eq!(find(&semantics, locator).bounds, lab.rect(key), "{locator}");
        }
        let segments = lab.laid.as_ref().unwrap().segments.clone().unwrap();
        for (style, rect) in lab.story.styles().iter().zip(&segments.segments) {
            let locator = format!("s:{STYLE_OPTIONS}#{}", style_part(*style));
            assert_eq!(find(&semantics, &locator).bounds, Some(*rect));
        }
        let caption = lab.caption_rect().unwrap();
        for control in caption::direct_controls(caption) {
            let locator = match control.action {
                caption::Action::Minimise => "s:window#minimize",
                caption::Action::ToggleMaximise => "s:window#maximize",
                _ => CLOSE_AT,
            };
            assert_eq!(find(&semantics, locator).bounds, Some(control.rect));
        }
        // Inside the window, every one of them.
        for node in flat(&semantics.children) {
            let bounds = node.bounds.expect("every node is placed");
            assert!(bounds.x >= 0.0 && bounds.y >= 0.0);
            assert!(bounds.x + bounds.w <= SIZE.0 && bounds.y + bounds.h <= SIZE.1);
        }
    });
}

#[test]
fn a_click_request_is_the_press_the_pointer_makes() {
    english(|| {
        let (mut lab, mut engine) = drawn();
        assert!(ask(&mut lab, &mut engine, SUBJECT_AT, Action::Click));
        assert_eq!(lab.story.presses(), 1);
        let semantics = tree(&lab);
        assert_eq!(
            find(&semantics, RESULT_AT).label.as_deref(),
            Some("Presses: 1")
        );
        assert_eq!(
            semantics.focus,
            Some(find(&semantics, SUBJECT_AT).occurrence),
            "the keyboard follows the request, as it follows a press"
        );

        assert!(ask(
            &mut lab,
            &mut engine,
            "s:ui-lab.style#destructive",
            Action::Click
        ));
        assert_eq!(lab.story.style(), ButtonRole::Destructive);
        assert_eq!(
            find(&tree(&lab), "s:ui-lab.style#destructive").toggled,
            Some(Toggled::True)
        );
        // Asking for the style it already wears changes nothing.
        assert!(!ask(
            &mut lab,
            &mut engine,
            "s:ui-lab.style#destructive",
            Action::Click
        ));

        assert!(ask(&mut lab, &mut engine, RESET_AT, Action::Click));
        assert!(lab.story.is_pristine());
    });
}

#[test]
fn a_focus_request_moves_the_keyboard_and_presses_nothing() {
    english(|| {
        let (mut lab, mut engine) = drawn();
        assert!(ask(&mut lab, &mut engine, SUBJECT_AT, Action::Focus));
        assert_eq!(lab.focus, Some(SUBJECT));
        assert_eq!(lab.story.presses(), 0);
        assert!(!ask(&mut lab, &mut engine, SUBJECT_AT, Action::Focus));

        assert!(ask(&mut lab, &mut engine, FIELD_AT, Action::Focus));
        assert_eq!(lab.focus, Some(control_key(Control::Label)));
        // The window lost the keyboard: no control is the focused one.
        WindowApp::focus_changed(&mut lab, false);
        assert_eq!(tree(&lab).focus, None);
    });
}

#[test]
fn setting_the_fields_value_relabels_the_button() {
    english(|| {
        let (mut lab, mut engine) = drawn();
        let set = request(
            &lab,
            FIELD_AT,
            Action::SetValue,
            Some(ActionData::Value("Delete".into())),
        );
        assert!(lab.accessibility_action(&mut engine, &set));
        draw(&mut lab, &mut engine);
        assert_eq!(lab.story.label(), "Delete");
        let semantics = tree(&lab);
        assert_eq!(find(&semantics, FIELD_AT).value.as_deref(), Some("Delete"));
        assert_eq!(
            find(&semantics, SUBJECT_AT).label.as_deref(),
            Some("Delete")
        );
        // A value request with no value, or on a control that holds none.
        let empty = request(&lab, FIELD_AT, Action::SetValue, None);
        assert!(!lab.accessibility_action(&mut engine, &empty));
        let wrong = request(
            &lab,
            SUBJECT_AT,
            Action::SetValue,
            Some(ActionData::Value("x".into())),
        );
        assert!(!lab.accessibility_action(&mut engine, &wrong));
        assert_eq!(lab.story.label(), "Delete");
    });
}

#[test]
fn only_close_can_be_asked_of_the_caption_and_it_ends_the_process() {
    english(|| {
        let (mut lab, mut engine) = drawn();
        for locator in ["s:window#minimize", "s:window#maximize"] {
            assert!(!ask(&mut lab, &mut engine, locator, Action::Click));
        }
        assert!(!WindowApp::should_exit(&lab));
        assert!(WindowApp::take_window_request(&mut lab).is_none());
        assert!(!ask(&mut lab, &mut engine, CLOSE_AT, Action::Focus));
        assert!(ask(&mut lab, &mut engine, CLOSE_AT, Action::Click));
        assert!(WindowApp::should_exit(&lab));
    });
}

#[test]
fn a_request_for_something_the_frame_does_not_show_does_nothing() {
    english(|| {
        let (mut lab, mut engine) = drawn();
        let absent = |object: ObjectKey, part: &'static str| Resolved {
            occurrence: Occurrence {
                window: WINDOW,
                view: VIEW,
                object,
                part: ControlPart::new(part),
            },
            action: Action::Click,
            data: None,
        };
        for request in [
            absent(ObjectKey::Surface("ui-lab"), "search"),
            absent(ObjectKey::Surface(STYLE_OPTIONS), "ghost"),
            absent(ObjectKey::Model(uuid::Uuid::from_u128(7)), "subject"),
            absent(ObjectKey::Instance { kind: "tab", id: 1 }, "subject"),
        ] {
            assert!(!lab.accessibility_action(&mut engine, &request));
        }
        assert!(lab.story.is_pristine());
        assert_eq!(lab.focus, None);
    });
}

#[test]
fn the_revision_follows_what_is_published_and_not_the_pointer() {
    english(|| {
        let (mut lab, mut engine) = drawn();
        let opened = lab.semantics_revision();
        // Hover changes what is painted, not what is published.
        let (x, y) = lab.rect(SUBJECT).unwrap().centre();
        assert!(WindowApp::pointer_moved(&mut lab, x, y));
        draw(&mut lab, &mut engine);
        assert_eq!(lab.semantics_revision(), opened);

        WindowApp::click(&mut lab, &mut engine, x, y, false);
        let pressed = lab.semantics_revision();
        assert_ne!(pressed, opened);
        WindowApp::set_maximised(&mut lab, true);
        assert_ne!(lab.semantics_revision(), pressed);
    });
}

#[test]
fn the_candidate_area_is_the_fields_caret_and_only_while_it_is_typed_in() {
    english(|| {
        let (mut lab, mut engine) = drawn();
        assert_eq!(lab.ime_cursor_area(), None);
        let field = lab.rect(control_key(Control::Label)).unwrap();
        let (x, y) = field.centre();
        WindowApp::click(&mut lab, &mut engine, x, y, false);
        WindowApp::mouse_up(&mut lab);
        draw(&mut lab, &mut engine);
        let caret = lab.ime_cursor_area().expect("the field is being typed in");
        assert!(caret.x >= field.x && caret.x <= field.x + field.w);
        assert!(caret.y >= field.y && caret.y + caret.h <= field.y + field.h);

        // The caret moves with what is typed, and the area with it.
        WindowApp::key(
            &mut lab,
            &mut engine,
            &KeyboardKey::Named(NamedKey::End),
            None,
            ModifiersState::empty(),
        );
        let at_end = lab.ime_cursor_area().unwrap();
        WindowApp::key(
            &mut lab,
            &mut engine,
            &KeyboardKey::Character("!".into()),
            Some("!"),
            ModifiersState::empty(),
        );
        assert!(lab.ime_cursor_area().unwrap().x > at_end.x);

        // Another window has the keyboard: no composition is ours to place.
        WindowApp::focus_changed(&mut lab, false);
        assert_eq!(lab.ime_cursor_area(), None);
    });
}
