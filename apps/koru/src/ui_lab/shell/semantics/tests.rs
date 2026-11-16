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
use crate::ui_lab::story::{ButtonStory, dialog_part};
use koru_ui::button::ButtonRole;

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

/// The button's story, which is the one on show.
fn story(lab: &Lab) -> &ButtonStory {
    match lab.story.as_ref() {
        Some(Story::Button(story)) => story,
        other => panic!("the story on show is not the button's: {other:?}"),
    }
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
const SEARCH_AT: &str = "s:ui-lab#search";
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
                (
                    "s:ui-lab#toggle-navigator".to_owned(),
                    NativeRole::Button,
                    Some("Components")
                ),
                (
                    "s:ui-lab#toggle-properties".to_owned(),
                    NativeRole::Button,
                    Some("Properties")
                ),
                (
                    SEARCH_AT.to_owned(),
                    NativeRole::TextInput,
                    Some("Search components")
                ),
                (LIST_AT.to_owned(), NativeRole::ListBox, Some("Components")),
                (RESET_AT.to_owned(), NativeRole::Button, Some("Reset")),
                (
                    "s:ui-lab.setting#theme".to_owned(),
                    NativeRole::ComboBox,
                    Some("Theme")
                ),
                (
                    "s:ui-lab.setting#width".to_owned(),
                    NativeRole::ComboBox,
                    Some("Preview width")
                ),
                (
                    "s:ui-lab.setting#language".to_owned(),
                    NativeRole::ComboBox,
                    Some("Language")
                ),
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
            let locator = format!("s:{STYLE_OPTIONS}#{}", dialog_part(*style));
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
            absent(ObjectKey::Surface("ui-lab"), "story-picker"),
            // The button that empties a search, while the search finds things.
            absent(ObjectKey::Surface("ui-lab"), "reset-search"),
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

/// The search field is a text field with its value, and setting the value
/// searches. Finding nothing is said aloud, and the button that empties the
/// search is offered beside it.
#[test]
fn the_search_field_is_announced_and_can_be_set() {
    english(|| {
        let (mut lab, mut engine) = drawn();
        let semantics = tree(&lab);
        let search = find(&semantics, SEARCH_AT);
        assert_eq!(search.role, NativeRole::TextInput);
        assert_eq!(search.label.as_deref(), Some("Search components"));
        assert_eq!(search.value.as_deref(), Some(""));
        assert_eq!(search.actions, [Action::Focus, Action::SetValue]);
        assert_eq!(search.bounds, lab.rect(SEARCH));
        // It reads before the list it narrows.
        let order: Vec<String> = semantics
            .children
            .iter()
            .map(|node| tail(&node.occurrence))
            .collect();
        let at = |locator: &str| order.iter().position(|l| l == locator).unwrap();
        assert!(at("s:ui-lab#toggle-properties") < at(SEARCH_AT));
        assert!(at(SEARCH_AT) < at(LIST_AT) && at(LIST_AT) < at(RESET_AT));

        let all = find(&semantics, LIST_AT).size_of_set.unwrap();
        let set = request(
            &lab,
            SEARCH_AT,
            Action::SetValue,
            Some(ActionData::Value("split button".into())),
        );
        assert!(lab.accessibility_action(&mut engine, &set));
        draw(&mut lab, &mut engine);
        let semantics = tree(&lab);
        assert_eq!(
            find(&semantics, SEARCH_AT).value.as_deref(),
            Some("split button")
        );
        let list = find(&semantics, LIST_AT);
        assert_eq!(list.size_of_set, Some(2), "a group and the record found");
        assert!(list.size_of_set.unwrap() < all);
        assert_eq!(
            semantics.focus,
            Some(find(&semantics, SEARCH_AT).occurrence)
        );

        let before = lab.semantics_revision();
        let nothing = request(
            &lab,
            SEARCH_AT,
            Action::SetValue,
            Some(ActionData::Value("zzzz".into())),
        );
        assert!(lab.accessibility_action(&mut engine, &nothing));
        draw(&mut lab, &mut engine);
        assert_ne!(lab.semantics_revision(), before, "what is said changed");
        let semantics = tree(&lab);
        let said = find(&semantics, "s:ui-lab#no-results");
        assert_eq!(said.role, NativeRole::Label);
        assert_eq!(said.label.as_deref(), Some("Nothing matches “zzzz”."));
        assert_eq!(said.live, Some(Live::Polite));
        let reset = find(&semantics, "s:ui-lab#reset-search");
        assert_eq!(reset.role, NativeRole::Button);
        assert_eq!(reset.label.as_deref(), Some("Reset search"));
        assert_eq!(reset.actions, [Action::Click, Action::Focus]);
        assert!(
            flat(&semantics.children)
                .iter()
                .all(|node| tail(&node.occurrence) != LIST_AT),
            "no list while nothing is found"
        );
        let order: Vec<String> = semantics
            .children
            .iter()
            .map(|node| tail(&node.occurrence))
            .collect();
        let at = |locator: &str| order.iter().position(|l| l == locator).unwrap();
        assert!(at(SEARCH_AT) < at("s:ui-lab#no-results"));
        assert!(at("s:ui-lab#no-results") < at("s:ui-lab#reset-search"));

        assert!(ask(
            &mut lab,
            &mut engine,
            "s:ui-lab#reset-search",
            Action::Click
        ));
        let semantics = tree(&lab);
        assert_eq!(find(&semantics, SEARCH_AT).value.as_deref(), Some(""));
        assert_eq!(find(&semantics, LIST_AT).size_of_set, Some(all));
        assert_eq!(
            semantics.focus,
            Some(find(&semantics, SEARCH_AT).occurrence)
        );
    });
}

/// A setting is a combo box that says its value; asking for it opens its
/// list, which is a menu of radio items with the one in force checked, and
/// asking for an item chooses it and closes the list.
#[test]
fn a_setting_is_a_combo_box_and_its_list_a_menu() {
    english(|| {
        let (mut lab, mut engine) = drawn();
        const WIDTH_AT: &str = "s:ui-lab.setting#width";
        const MENU_AT: &str = "s:ui-lab#menu";
        let semantics = tree(&lab);
        let width = find(&semantics, WIDTH_AT);
        assert_eq!(width.value.as_deref(), Some("Full width"));
        assert_eq!(width.actions, [Action::Click, Action::Focus]);
        assert_eq!(
            find(&semantics, "s:ui-lab.setting#language")
                .value
                .as_deref(),
            Some("English")
        );
        assert!(
            flat(&semantics.children)
                .iter()
                .all(|node| tail(&node.occurrence) != MENU_AT),
            "no list is open"
        );

        let closed = lab.semantics_revision();
        assert!(ask(&mut lab, &mut engine, WIDTH_AT, Action::Click));
        assert_ne!(lab.semantics_revision(), closed);
        let semantics = tree(&lab);
        assert_eq!(semantics.focus, Some(find(&semantics, WIDTH_AT).occurrence));
        let menu = find(&semantics, MENU_AT);
        assert_eq!(menu.role, NativeRole::Menu);
        assert_eq!(menu.label.as_deref(), Some("Preview width"));
        let options: Vec<(String, NativeRole, Option<&str>, Option<Toggled>)> = menu
            .children
            .iter()
            .map(|node| {
                (
                    tail(&node.occurrence),
                    node.role,
                    node.label.as_deref(),
                    node.toggled,
                )
            })
            .collect();
        let radio = NativeRole::MenuItemRadio;
        assert_eq!(
            options,
            [
                (
                    "s:ui-lab.option#fit".to_owned(),
                    radio,
                    Some("Full width"),
                    Some(Toggled::True)
                ),
                (
                    "s:ui-lab.option#320".to_owned(),
                    radio,
                    Some("320 px"),
                    Some(Toggled::False)
                ),
                (
                    "s:ui-lab.option#480".to_owned(),
                    radio,
                    Some("480 px"),
                    Some(Toggled::False)
                ),
                (
                    "s:ui-lab.option#640".to_owned(),
                    radio,
                    Some("640 px"),
                    Some(Toggled::False)
                ),
                (
                    "s:ui-lab.option#800".to_owned(),
                    radio,
                    Some("800 px"),
                    Some(Toggled::False)
                ),
            ]
        );
        // Each option is where the shared menu draws and presses it.
        let panel = menu.bounds.unwrap();
        for option in &menu.children {
            let bounds = option.bounds.unwrap();
            assert!(bounds.y >= panel.y && bounds.y + bounds.h <= panel.y + panel.h);
            assert_eq!(option.actions, [Action::Click]);
        }

        assert!(ask(
            &mut lab,
            &mut engine,
            "s:ui-lab.option#480",
            Action::Click
        ));
        let semantics = tree(&lab);
        assert_eq!(find(&semantics, WIDTH_AT).value.as_deref(), Some("480 px"));
        assert!(
            flat(&semantics.children)
                .iter()
                .all(|node| tail(&node.occurrence) != MENU_AT),
            "choosing closed the list"
        );
        // An option of a list that is not open cannot be asked for.
        let stale = Resolved {
            occurrence: Occurrence::surface(WINDOW, VIEW, "ui-lab.option", ControlPart::new("640")),
            action: Action::Click,
            data: None,
        };
        assert!(!lab.accessibility_action(&mut engine, &stale));
        // A request for the window under an open list closes the list first.
        assert!(ask(&mut lab, &mut engine, WIDTH_AT, Action::Click));
        assert!(ask(&mut lab, &mut engine, SUBJECT_AT, Action::Click));
        assert_eq!(story(&lab).presses(), 1);
        assert!(
            flat(&tree(&lab).children)
                .iter()
                .all(|node| tail(&node.occurrence) != MENU_AT)
        );
    });
}

/// The tree is published in the language the Lab is shown in.
#[test]
fn the_tree_is_published_in_the_labs_language() {
    english(|| {
        let (mut lab, mut engine) = drawn();
        assert!(ask(
            &mut lab,
            &mut engine,
            "s:ui-lab.setting#language",
            Action::Click
        ));
        assert!(ask(
            &mut lab,
            &mut engine,
            "s:ui-lab.option#ja",
            Action::Click
        ));
        let semantics = tree(&lab);
        let japanese = |key| koru_i18n::t(koru_i18n::Lang::Ja, key);
        assert_eq!(semantics.label, japanese(koru_i18n::Key::UiLabTitle));
        assert_eq!(
            find(&semantics, RESET_AT).label.as_deref(),
            Some(japanese(koru_i18n::Key::UiLabReset))
        );
        assert_eq!(
            find(&semantics, "s:ui-lab.setting#language")
                .value
                .as_deref(),
            Some("日本語")
        );
        assert_eq!(
            find(&semantics, "s:window#close").label.as_deref(),
            {
                let lang = koru_i18n::Lang::Ja;
                let commands = window_commands(
                    WINDOW,
                    false,
                    lang,
                    CaptionGeometry {
                        minimize: None,
                        maximize: None,
                        close: None,
                    },
                );
                commands
                    .iter()
                    .find(|command| command.occurrence.part == part::CLOSE)
                    .and_then(|command| command.label.clone())
            }
            .as_deref()
        );
        // The identities are what they were in English.
        assert!(find(&semantics, SUBJECT_AT).label.is_some());
    });
}

/// A column's toggle says whether its column shows, and a request toggles
/// it: the column's nodes leave the tree with it and come back with it.
#[test]
fn a_column_toggle_says_whether_its_column_shows() {
    english(|| {
        let (mut lab, mut engine) = drawn();
        const TOGGLE_AT: &str = "s:ui-lab#toggle-properties";
        let semantics = tree(&lab);
        let toggle = find(&semantics, TOGGLE_AT);
        assert_eq!(toggle.toggled, Some(Toggled::True));
        assert_eq!(toggle.actions, [Action::Click, Action::Focus]);
        // Each toggle says its chord, written as this platform writes it:
        // the same value the keyboard is matched against.
        let primary = koru_keys::label_for(koru_keys::PRIMARY, koru_keys::On::Letter('j'));
        assert_eq!(toggle.description.as_deref(), Some(primary.as_str()));
        let navigator = find(&semantics, "s:ui-lab#toggle-navigator");
        assert_eq!(
            navigator.description,
            Some(koru_keys::label_for(
                koru_keys::PRIMARY,
                koru_keys::On::Letter('b')
            ))
        );
        if cfg!(windows) {
            assert_eq!(toggle.description.as_deref(), Some("Ctrl+J"));
            assert_eq!(navigator.description.as_deref(), Some("Ctrl+B"));
        }
        // Both stand in the caption, where they are drawn and pressed.
        let caption = lab.caption_rect().unwrap();
        for node in [toggle, navigator] {
            let bounds = node.bounds.unwrap();
            assert!(bounds.y >= caption.y && bounds.y + bounds.h <= caption.y + caption.h);
        }
        assert_eq!(
            find(&semantics, "s:ui-lab#toggle-navigator").toggled,
            Some(Toggled::True)
        );

        // The keyboard is in the column when it is asked to close.
        assert!(ask(&mut lab, &mut engine, FIELD_AT, Action::Focus));
        let shown = lab.semantics_revision();
        assert!(ask(&mut lab, &mut engine, TOGGLE_AT, Action::Click));
        assert_ne!(lab.semantics_revision(), shown);
        let semantics = tree(&lab);
        assert_eq!(find(&semantics, TOGGLE_AT).toggled, Some(Toggled::False));
        for node in flat(&semantics.children) {
            let locator = tail(&node.occurrence);
            assert!(
                locator != PANEL_AT && locator != FIELD_AT && locator != STYLE_AT,
                "`{locator}` is published with its column hidden"
            );
        }
        assert_eq!(
            semantics.focus,
            Some(find(&semantics, TOGGLE_AT).occurrence),
            "the request moved the keyboard to the toggle"
        );
        // The preview is still there, and still works.
        assert!(ask(&mut lab, &mut engine, SUBJECT_AT, Action::Click));
        assert_eq!(story(&lab).presses(), 1);

        assert!(ask(&mut lab, &mut engine, TOGGLE_AT, Action::Click));
        let semantics = tree(&lab);
        assert_eq!(find(&semantics, TOGGLE_AT).toggled, Some(Toggled::True));
        assert_eq!(
            find(&semantics, FIELD_AT).value.as_deref(),
            Some("Save changes")
        );
    });
}
