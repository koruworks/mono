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

/// The story on show.
fn story(lab: &Lab) -> &ButtonStory {
    lab.story.as_ref().expect("the record on show has a story")
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
const LIST_AT: &str = "s:ui-lab#navigator";
const PAGE_AT: &str = "s:ui-lab#page";
const PANEL_AT: &str = "s:ui-lab#properties";
const CONTROLS_AT: &str = "s:ui-lab.group#controls";
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

        // The window's own level: its three controls, Reset, and the three
        // regions, in reading order.
        let top: Vec<(String, NativeRole, Option<&str>)> = semantics
            .children
            .iter()
            .map(|node| (tail(&node.occurrence), node.role, node.label.as_deref()))
            .collect();
        assert_eq!(
            top,
            [
                (
                    "s:window#minimize".to_owned(),
                    NativeRole::Button,
                    Some("Minimize")
                ),
                (
                    "s:window#maximize".to_owned(),
                    NativeRole::Button,
                    Some("Maximize")
                ),
                (
                    CLOSE_AT.to_owned(),
                    NativeRole::Button,
                    Some("Close window")
                ),
                (RESET_AT.to_owned(), NativeRole::Button, Some("Reset")),
                (LIST_AT.to_owned(), NativeRole::ListBox, Some("Components")),
                (PAGE_AT.to_owned(), NativeRole::Group, Some("Details")),
                (PANEL_AT.to_owned(), NativeRole::Group, Some("Properties")),
            ]
        );
        let click_focus = vec![Action::Click, Action::Focus];
        for locator in ["s:window#minimize", "s:window#maximize"] {
            assert!(find(&semantics, locator).actions.is_empty());
        }
        assert_eq!(find(&semantics, CLOSE_AT).actions, [Action::Click]);
        assert_eq!(find(&semantics, RESET_AT).actions, click_focus);

        // The page: the component, what pressing it did, then its symbols.
        let page = find(&semantics, PAGE_AT);
        let described: Vec<(String, NativeRole, Option<&str>, Vec<Action>)> = page
            .children
            .iter()
            .take(2)
            .map(|node| {
                (
                    tail(&node.occurrence),
                    node.role,
                    node.label.as_deref(),
                    node.actions.clone(),
                )
            })
            .collect();
        assert_eq!(
            described,
            [
                (
                    SUBJECT_AT.to_owned(),
                    NativeRole::Button,
                    Some("Save changes"),
                    click_focus.clone()
                ),
                (
                    RESULT_AT.to_owned(),
                    NativeRole::Label,
                    Some("Not pressed yet"),
                    vec![]
                ),
            ]
        );
        assert_eq!(find(&semantics, RESULT_AT).live, Some(Live::Polite));
        let first = find(&semantics, "s:ui-lab.symbol#koru_ui::button::draw_button");
        assert_eq!(first.role, NativeRole::Label);
        assert_eq!(
            first.label.as_deref(),
            Some("koru_ui::button::draw_button"),
            "a symbol is named as it is written"
        );
        assert_eq!(
            first.description.as_deref(),
            Some("Story available · Not supported by A2UI")
        );

        // The properties column: what the record is, the story's inputs,
        // then what is recorded about it.
        let panel = find(&semantics, PANEL_AT);
        let stated: Vec<(String, NativeRole, Option<&str>)> = panel
            .children
            .iter()
            .map(|node| (tail(&node.occurrence), node.role, node.label.as_deref()))
            .collect();
        let purpose = koru_i18n::t(koru_i18n::Lang::En, koru_i18n::Key::UiLabButtonPurpose);
        let availability = "Availability: Windows: Runs in this Lab, macOS: Same source builds; \
                            not verified, Linux: Same source builds; not verified";
        let a2ui = "A2UI: Component “Button”; the host picks its style, Used by the host only; \
                    a declaration cannot name it, Not supported by A2UI";
        assert_eq!(
            stated,
            [
                (
                    "s:ui-lab.fact#name".to_owned(),
                    NativeRole::Heading,
                    Some("Button")
                ),
                (
                    "s:ui-lab.fact#purpose".to_owned(),
                    NativeRole::Label,
                    Some(purpose)
                ),
                (FIELD_AT.to_owned(), NativeRole::TextInput, Some("Label")),
                (STYLE_AT.to_owned(), NativeRole::Group, Some("Style")),
                (
                    "s:ui-lab.fact#kind".to_owned(),
                    NativeRole::Label,
                    Some("Kind: Shared component")
                ),
                (
                    "s:ui-lab.fact#source".to_owned(),
                    NativeRole::Label,
                    Some("Source: koru-ui, koru_ui::button")
                ),
                (
                    "s:ui-lab.fact#availability".to_owned(),
                    NativeRole::Label,
                    Some(availability)
                ),
                (
                    "s:ui-lab.fact#a2ui".to_owned(),
                    NativeRole::Label,
                    Some(a2ui)
                ),
                (
                    "s:ui-lab.fact#story".to_owned(),
                    NativeRole::Label,
                    Some("Story: Story available")
                ),
            ]
        );
        assert_eq!(
            find(&semantics, FIELD_AT).actions,
            [Action::Focus, Action::SetValue]
        );
        assert_eq!(
            find(&semantics, FIELD_AT).value.as_deref(),
            Some("Save changes")
        );
        let style = find(&semantics, STYLE_AT);
        assert_eq!(style.actions, [Action::Focus]);
        let options: Vec<(String, NativeRole, Option<&str>, Vec<Action>)> = style
            .children
            .iter()
            .map(|node| {
                (
                    tail(&node.occurrence),
                    node.role,
                    node.label.as_deref(),
                    node.actions.clone(),
                )
            })
            .collect();
        assert_eq!(
            options,
            [
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
            ]
        );
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

/// The navigator is a list one of whose rows is the selected one. It says
/// how many rows it has, each row where it sits among them, and each group
/// whether it is folded; only the rows in view are nodes.
#[test]
fn the_navigator_is_a_list_of_groups_and_selectable_records() {
    english(|| {
        let (mut lab, mut engine) = drawn();
        let semantics = tree(&lab);
        let list = find(&semantics, LIST_AT);
        let all = lab.nav.rows().len();
        assert_eq!(list.size_of_set, Some(all));
        assert!(list.children.len() < all, "only the rows in view");
        assert_eq!(
            list.children.len(),
            lab.laid
                .as_ref()
                .unwrap()
                .frame
                .nodes
                .iter()
                .filter(|node| matches!(node.name, "Navigator entry" | "Navigator group"))
                .count()
        );
        // The list opened part of the way down: it can be scrolled both ways.
        assert!(list.actions.contains(&Action::ScrollUp));
        assert!(list.actions.contains(&Action::ScrollDown));

        let entry = find(&semantics, ENTRY_AT);
        assert_eq!(entry.role, NativeRole::ListBoxOption);
        assert_eq!(entry.label.as_deref(), Some("Button"));
        assert_eq!(entry.selected, Some(true));
        assert_eq!(entry.actions, [Action::Click, Action::Focus]);
        let at = lab
            .nav
            .rows()
            .iter()
            .position(|row| row.key() == Key::surface("ui-lab.navigator", "C-BUTTON"))
            .unwrap();
        assert_eq!(entry.position, Some(at), "its place among every row");
        let selected = list
            .children
            .iter()
            .filter(|node| node.selected == Some(true))
            .count();
        assert_eq!(selected, 1);
        for node in &list.children {
            match node.role {
                NativeRole::ListBoxOption => assert!(node.selected.is_some()),
                NativeRole::Button => assert!(node.description.is_some()),
                other => panic!("a {other:?} is in the navigator"),
            }
        }

        // Scrolled to the top, the first row is the first group, expanded.
        while lab.nav.scroll > 0.0 {
            assert!(ask(&mut lab, &mut engine, LIST_AT, Action::ScrollUp));
        }
        let semantics = tree(&lab);
        assert!(
            !find(&semantics, LIST_AT)
                .actions
                .contains(&Action::ScrollUp),
            "nothing above the top"
        );
        let group = find(&semantics, "s:ui-lab.group#foundations");
        assert_eq!(group.role, NativeRole::Button);
        assert_eq!(group.label.as_deref(), Some("Foundations"));
        assert_eq!(group.description.as_deref(), Some("Expanded"));
        assert_eq!(group.actions, [Action::Click, Action::Focus]);

        // A click on it folds it, as a press does, and says so.
        assert!(ask(
            &mut lab,
            &mut engine,
            "s:ui-lab.group#foundations",
            Action::Click
        ));
        assert!(lab.nav.is_folded(Family::Foundations));
        let semantics = tree(&lab);
        assert_eq!(
            find(&semantics, "s:ui-lab.group#foundations")
                .description
                .as_deref(),
            Some("Collapsed")
        );
        assert_eq!(
            find(&semantics, LIST_AT).size_of_set,
            Some(lab.nav.rows().len())
        );
        assert_eq!(
            find(&semantics, CONTROLS_AT).description.as_deref(),
            Some("Expanded")
        );
        assert_eq!(
            semantics.focus,
            Some(find(&semantics, "s:ui-lab.group#foundations").occurrence)
        );
    });
}

/// A click on a record's row shows it. A record with no story publishes its
/// status and the reason in words, and no component.
#[test]
fn a_record_without_a_story_is_announced_as_one() {
    english(|| {
        let (mut lab, mut engine) = drawn();
        let row = lab
            .nav
            .rows()
            .iter()
            .find_map(|row| match row {
                Row::Entry(entry) if entry.status() == registry::Story::NoStoryYet => Some(*entry),
                _ => None,
            })
            .unwrap();
        // Its row is brought into view by the keyboard's own path, then
        // asked for.
        lab.set_focus(&mut engine, Some(Row::Entry(row).key()));
        lab.reveal(Row::Entry(row).key());
        draw(&mut lab, &mut engine);
        let locator = format!("s:ui-lab.navigator#{}", row.catalog_id);
        assert!(ask(&mut lab, &mut engine, &locator, Action::Click));
        assert_eq!(lab.entry.catalog_id, row.catalog_id);

        let semantics = tree(&lab);
        assert_eq!(find(&semantics, &locator).selected, Some(true));
        let page = find(&semantics, PAGE_AT);
        let status = &page.children[0];
        assert_eq!(tail(&status.occurrence), "s:ui-lab#status");
        assert_eq!(status.role, NativeRole::Heading);
        assert_eq!(status.label.as_deref(), Some("No story yet"));
        let reason = &page.children[1];
        assert_eq!(tail(&reason.occurrence), "s:ui-lab#status-reason");
        assert_eq!(
            reason.label.as_deref(),
            Some("The Lab has nothing to run for this yet. No stand-in is shown in its place.")
        );
        for node in flat(&semantics.children) {
            let locator = tail(&node.occurrence);
            assert!(
                locator != SUBJECT_AT && locator != RESULT_AT && locator != FIELD_AT,
                "a record with no story publishes `{locator}`"
            );
        }
        // Reset has nothing to put back: it says so and does nothing.
        let reset = find(&semantics, RESET_AT);
        assert!(reset.disabled);
        assert_eq!(reset.actions, [Action::Focus]);
        assert!(!ask(&mut lab, &mut engine, RESET_AT, Action::Click));
        assert!(!ask(&mut lab, &mut engine, RESET_AT, Action::Focus));
        assert_eq!(
            find(&tree(&lab), "s:ui-lab.fact#story").label.as_deref(),
            Some("Story: No story yet")
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
            (ENTRY_AT, Key::surface("ui-lab.navigator", "C-BUTTON")),
            (LIST_AT, LIST),
            (PAGE_AT, PAGE),
            (PANEL_AT, PANEL),
            (FIELD_AT, control_key(Control::Label)),
            (STYLE_AT, control_key(Control::Style)),
        ] {
            assert_eq!(find(&semantics, locator).bounds, lab.rect(key), "{locator}");
        }
        let segments = lab.laid.as_ref().unwrap().segments.clone().unwrap();
        for (style, rect) in story(&lab).styles().iter().zip(&segments.segments) {
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
        assert_eq!(story(&lab).presses(), 1);
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
        assert_eq!(story(&lab).style(), ButtonRole::Destructive);
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
        assert!(story(&lab).is_pristine());
    });
}

#[test]
fn a_focus_request_moves_the_keyboard_and_presses_nothing() {
    english(|| {
        let (mut lab, mut engine) = drawn();
        assert!(ask(&mut lab, &mut engine, SUBJECT_AT, Action::Focus));
        assert_eq!(lab.focus, Some(SUBJECT));
        assert_eq!(story(&lab).presses(), 0);
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
        assert_eq!(story(&lab).label(), "Delete");
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
        assert_eq!(story(&lab).label(), "Delete");
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
        let out_of_view = lab
            .nav
            .rows()
            .iter()
            .rev()
            .find_map(|row| match row {
                Row::Entry(entry) => Some(entry.catalog_id),
                Row::Group(_) => None,
            })
            .unwrap();
        let unlisted = registry::catalog()
            .find(|entry| !entry.on_desktop())
            .unwrap()
            .catalog_id;
        let shown = lab.entry.catalog_id;
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
            // A row the list holds and the frame does not show.
            absent(ObjectKey::Surface("ui-lab.navigator"), out_of_view),
            // A record of the registry that the gallery does not list.
            absent(ObjectKey::Surface("ui-lab.navigator"), unlisted),
            absent(ObjectKey::Surface(STYLE_OPTIONS), "ghost"),
            absent(ObjectKey::Model(uuid::Uuid::from_u128(7)), "subject"),
            absent(ObjectKey::Instance { kind: "tab", id: 1 }, "subject"),
        ] {
            assert!(!lab.accessibility_action(&mut engine, &request));
        }
        assert!(story(&lab).is_pristine());
        assert_eq!(lab.focus, None);
        assert_eq!(lab.entry.catalog_id, shown);
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
