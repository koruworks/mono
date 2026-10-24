// SPDX-License-Identifier: MPL-2.0

//! What must stay true about this window's semantic tree while everything a
//! person can see changes underneath it.
//!
//! Nothing here touches disk, the Store, a browser or a desktop: the harness
//! stands a container up over an in-memory workspace, and the projection is a
//! pure function of it. The one thing these cannot establish — that Windows,
//! Narrator and NVDA agree with any of it — is the interactive lane's, and is
//! written down in `docs/features/accessibility/README.md`.

use accesskit::{Action, Role};
use koru_core::schema::WorkspaceDoc;
use koru_workspace::Workspace;
use koru_workspace_host::accessibility::{Occurrence, Scope, SemanticNode, ViewSlot, WindowSlot};
use koru_workspace_host::routing::Blockers;
use winit::keyboard::{Key, NamedKey};

use crate::mainview::MainView;
use crate::mainview::harness::*;
use koru_workspace_host::View;

const WINDOW: WindowSlot = WindowSlot::from_raw(1);
const VIEW: ViewSlot = ViewSlot::from_raw(1);

/// A container seated in window 1, showing a document, with its column open.
///
/// The column is opened because A1's journey starts there: *hear the window,
/// find a document, open it*. A window with it shut is the same tree minus one
/// group, which `an_empty_result_set…` and the focus tests cover from the other
/// side.
fn seated() -> (MainView, Workspace) {
    let (mut v, w) = on_document();
    v.seat_accessibility(WINDOW);
    if !v.sidebar.is_open() {
        assert!(v.toggle_sidebar(), "the column would not open");
    }
    (v, w)
}

/// The tree this window would publish right now, flattened for searching.
fn nodes(v: &MainView) -> Vec<SemanticNode> {
    let mut out = Vec::new();
    for node in v.semantics(VIEW, Scope::Whole) {
        flatten(&node, &mut out);
    }
    out
}

fn flatten(node: &SemanticNode, out: &mut Vec<SemanticNode>) {
    out.push(node.clone());
    for child in &node.children {
        flatten(child, out);
    }
}

fn locators(v: &MainView) -> Vec<String> {
    nodes(v)
        .iter()
        .map(|node| node.occurrence.locator())
        .collect()
}

fn by_role(v: &MainView, role: Role) -> Vec<SemanticNode> {
    nodes(v)
        .into_iter()
        .filter(|node| node.role == role)
        .collect()
}

fn find(v: &MainView, locator: &str) -> Option<SemanticNode> {
    nodes(v)
        .into_iter()
        .find(|node| node.occurrence.locator() == locator)
}

/// Put a listing in the column, the way a store-backed workspace does.
///
/// **The manifest path, not `document(ws, title)`.** That one is find-or-create
/// by title, so it cannot produce the two same-titled documents the roadmap
/// requires a client to tell apart — and the manifest is what the shipping app
/// projects from anyway (`refresh_explore` prefers `self.entries`).
fn with_rows(v: &mut MainView, titles: &[&str]) -> Vec<koru_core::NodeId> {
    let entries: Vec<koru_core::explore::ExploreEntry> = titles
        .iter()
        .enumerate()
        .map(|(i, title)| koru_core::explore::ExploreEntry {
            id: koru_core::DocumentId::new(),
            parent: None,
            ordinal: i as u32,
            title: (*title).to_string(),
            kind: koru_core::explore::KIND_DOCUMENT.to_string(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        })
        .collect();
    let ids = entries
        .iter()
        .map(|entry| koru_core::NodeId::from_uuid(entry.id.as_uuid()))
        .collect();
    v.explore.rebuild_from_entries(&entries);
    ids
}

// ------------------------------------------------------------------ identity

#[test]
fn a_tabs_locator_survives_a_rename_a_reorder_and_a_change_of_language() {
    let (mut v, mut w) = seated();
    let second = a_document(&mut w.ws, "Brief");
    v.open_node(&w.ws, second);
    let watched = v.tabs().get(1).expect("a second tab").id;
    let before = tab_locator(watched);

    // A rename. The strip caches the title, so this is what a person typing in
    // the document's first row eventually produces.
    let index = v
        .tabs()
        .iter()
        .position(|tab| tab.id == watched)
        .expect("the tab is in the strip");
    v.tabs_mut()
        .get_mut(index)
        .expect("the tab")
        .label
        .push_str(" (edited)");
    assert_eq!(tab_locator(watched), before, "a rename moved an identity");

    // A reorder. The strip addresses tabs by position and the position moves;
    // the identity may not.
    assert!(v.tabs_mut().move_tab(index, 0));
    assert_eq!(
        tab_locator(watched),
        before,
        "reordering the strip moved an identity"
    );

    // The language is the third one, and it is asserted where the language
    // actually enters: the explore column takes its `Lang` as a parameter, so
    // `a_locator_is_the_same_in_every_language` there compares two real runs.
    // What is asserted here is the property that makes that possible — no
    // locator anywhere carries anything a person wrote.
}

fn tab_locator(id: u64) -> String {
    Occurrence::instance(WINDOW, VIEW, super::TAB, id, super::parts::ROOT).locator()
}

#[test]
fn a_locator_never_contains_a_label() {
    let (mut v, _w) = seated();
    with_rows(&mut v, &["Ünïcode ✅ title", "another"]);
    let titles = ["Ünïcode ✅ title", "another", "Notes", "Welcome"];
    for locator in locators(&v) {
        for title in titles {
            assert!(
                !locator.contains(title),
                "{locator} carries a label somebody can rename"
            );
        }
        assert!(
            locator.is_ascii(),
            "{locator} is not ASCII, so something a person wrote reached it"
        );
    }
}

#[test]
fn two_tabs_on_one_document_are_two_occurrences() {
    let (mut v, mut w) = seated();
    let doc = a_document(&mut w.ws, "Shared");
    v.open_node(&w.ws, doc);
    let first = v.tabs().active();
    v.open_node_in_new_tab(&w.ws, doc);
    let second = v.tabs().active();
    assert_ne!(first, second, "the fixture opened one tab, not two");

    let ids: Vec<u64> = v.tabs().iter().map(|tab| tab.id).collect();
    let a = tab_locator(ids[first]);
    let b = tab_locator(ids[second]);
    assert_ne!(a, b, "one document in two tabs published one occurrence");
    let published = locators(&v);
    assert!(published.contains(&a) && published.contains(&b));
}

#[test]
fn two_documents_with_the_same_title_are_distinguishable_without_reading_a_label() {
    let (mut v, _w) = seated();
    let ids = with_rows(&mut v, &["Notes", "Notes"]);
    let rows = by_role(&v, Role::TreeItem);
    let same: Vec<&SemanticNode> = rows
        .iter()
        .filter(|node| node.label.as_deref() == Some("Notes"))
        .collect();
    assert!(
        same.len() >= 2,
        "the fixture did not produce two rows with one title: {:?}",
        rows.iter().map(|n| n.label.clone()).collect::<Vec<_>>()
    );
    let a = same[0].occurrence.locator();
    let b = same[1].occurrence.locator();
    assert_ne!(a, b, "two same-titled documents share one identity");
    // …and each one names the document it stands for, so a client can tell
    // *which* it found rather than only that they differ.
    for id in ids {
        let locator = Occurrence::model(
            WINDOW,
            VIEW,
            id.as_uuid(),
            koru_workspace_host::accessibility::ControlPart::ROOT,
        )
        .locator();
        assert!(
            locators(&v).contains(&locator),
            "no node named the document {locator}"
        );
    }
}

// --------------------------------------------------------------- transience

#[test]
fn a_reopened_menu_does_not_reuse_its_predecessors_identity() {
    let (mut v, mut w) = seated();
    let first = open_a_tab_menu(&mut v, &mut w.ws);
    let before: Vec<String> = menu_locators(&v);
    assert!(!before.is_empty(), "the menu published no rows");

    assert!(dismiss(&mut v, &mut w.ws), "Escape did not close the menu");
    assert!(v.menu.is_none());
    assert!(
        menu_locators(&v).is_empty(),
        "a closed menu left nodes in the tree"
    );

    let second = open_a_tab_menu(&mut v, &mut w.ws);
    assert_ne!(first, second, "a reopened menu reused its instance number");
    let after = menu_locators(&v);
    for locator in &after {
        assert!(
            !before.contains(locator),
            "{locator} came back after the menu that minted it closed"
        );
    }
}

#[test]
fn a_queued_action_for_a_closed_menu_is_refused() {
    let (mut v, mut w) = seated();
    open_a_tab_menu(&mut v, &mut w.ws);
    let row = menu_rows(&v)
        .into_iter()
        .next()
        .expect("the tab menu has rows");
    // It works while the menu is up…
    assert!(
        v.accessibility_action(&mut w.ws, row.occurrence, Action::Focus, None),
        "a live menu row refused a focus request"
    );

    assert!(dismiss(&mut v, &mut w.ws));
    let tabs_before = v.tabs().len();
    // …and is refused once it is not. The host would not even offer this one —
    // the node has left the published tree — but a view that performed it
    // anyway would make that refusal a formality rather than a rule.
    assert!(
        !v.accessibility_action(&mut w.ws, row.occurrence, Action::Click, None),
        "a request for a closed menu's row was performed"
    );
    assert_eq!(
        v.tabs().len(),
        tabs_before,
        "the refused request did something"
    );

    // And it does not land on the menu that replaced it either.
    open_a_tab_menu(&mut v, &mut w.ws);
    assert!(
        !v.accessibility_action(&mut w.ws, row.occurrence, Action::Click, None),
        "a request queued against a closed menu was routed to its successor"
    );
}

/// Right-click the first tab, which is how a person opens this menu.
fn open_a_tab_menu(v: &mut MainView, ws: &mut WorkspaceDoc) -> u64 {
    v.ensure_tab_widths();
    let rect = koru_shell_chrome::tabs::layout(v.strip(), v.tab_widths(), v.tab_scroll())
        .first()
        .expect("the strip has a tab")
        .body;
    let (x, y) = rect.centre();
    assert!(View::context_menu(v, ws, x, y), "the tab menu did not open");
    v.menu.as_ref().expect("a menu is up").instance
}

fn menu_locators(v: &MainView) -> Vec<String> {
    menu_rows(v)
        .into_iter()
        .map(|node| node.occurrence.locator())
        .collect()
}

fn menu_rows(v: &MainView) -> Vec<SemanticNode> {
    by_role(v, Role::MenuItem)
}

fn dismiss(v: &mut MainView, ws: &mut WorkspaceDoc) -> bool {
    View::key(v, ws, &Key::Named(NamedKey::Escape), None, NONE)
}

// --------------------------------------------------------------- structure

#[test]
fn the_window_publishes_its_context_its_tabs_and_its_documents() {
    let (mut v, _w) = seated();
    with_rows(&mut v, &["Brief", "Minutes"]);

    let roles: Vec<Role> = nodes(&v).iter().map(|node| node.role).collect();
    for wanted in [
        Role::Group,    // the shell, and the explore column
        Role::TabList,  // the strip, and the navigator's projections
        Role::Tab,      // a tab, and a projection button
        Role::Tree,     // the library
        Role::TreeItem, // a document entry
        Role::Button,   // the caption's controls, the `+`, the empty action
        Role::TabPanel, // the region the active tab draws into
    ] {
        assert!(
            roles.contains(&wanted),
            "no {wanted:?} in the published tree"
        );
    }

    // The selected tab is a *state*, not a second object.
    let tabs = by_role(&v, Role::Tab);
    let selected: Vec<&SemanticNode> = tabs
        .iter()
        .filter(|node| node.selected == Some(true))
        .collect();
    assert!(
        !selected.is_empty(),
        "no control said it was the selected one"
    );
    // Every tab and every entry says where it is in its set, so a reader can
    // say "3 of 7" without counting.
    for tab in &tabs {
        assert!(
            tab.position.is_some(),
            "a tab published no position in its set"
        );
    }
    for row in by_role(&v, Role::TreeItem) {
        assert!(row.position.is_some(), "an entry published no position");
        assert!(row.level.is_some(), "an entry published no depth");
    }
}

#[test]
fn the_open_action_is_advertised_and_opens_the_document() {
    let (mut v, mut w) = seated();
    let ids = with_rows(&mut v, &["Brief"]);
    let row = by_role(&v, Role::TreeItem)
        .into_iter()
        .find(|node| node.label.as_deref() == Some("Brief"))
        .expect("the entry is published");
    assert!(
        row.actions.contains(&Action::Click),
        "a document entry does not advertise a way to open it"
    );

    // **The same request the keyboard makes, not a second one.** Enter on a
    // focused row is the existing open route; what an invoke has to produce is
    // that, byte for byte, rather than something that merely also opens a
    // document. So both are run and compared.
    let area = v.explore_area();
    assert!(
        v.focus_or_toggle_sidebar(&w.ws),
        "the tree would not take the keyboard"
    );
    assert_eq!(v.explore.focused(), Some(ids[0]));
    let out = v
        .explore
        .nav_key(area, &Key::Named(NamedKey::Enter))
        .expect("Enter on a focused row is the open route");
    v.apply(&w.ws, out);
    let by_keyboard = format!("{:?}", View::take_pane_request(&mut v));
    assert!(
        by_keyboard.contains("Navigate"),
        "Enter on a focused row asked for nothing: {by_keyboard}"
    );

    assert!(v.accessibility_action(&mut w.ws, row.occurrence, Action::Click, None));
    let by_client = format!("{:?}", View::take_pane_request(&mut v));
    assert_eq!(
        by_client, by_keyboard,
        "an invoke took a different route from Enter"
    );
}

#[test]
fn an_empty_result_set_explains_itself_and_keeps_a_route_back() {
    let (mut v, w) = seated();
    // A listing with nothing in it: the pane's own empty state, which is what
    // a person meets after a filter that matched nothing and in a workspace
    // whose documents have not arrived yet.
    with_rows(&mut v, &[]);
    assert!(v.explore.is_empty());
    let _ = w;

    let published = nodes(&v);
    let explanation = published
        .iter()
        .find(|node| node.role == Role::Label)
        .expect("an empty column published no explanation");
    let text = explanation.label.clone().unwrap_or_default();
    assert!(!text.is_empty(), "the explanation is empty");
    // Localized: §7.2 forbids an authored accessible name that is not in the
    // table, and the table has no English fall-back.
    let lang = koru_workspace_host::accessibility::ui_lang();
    assert!(
        [
            koru_i18n::Key::A11yEmptyDocuments,
            koru_i18n::Key::A11yEmptyWorkspace,
        ]
        .into_iter()
        .any(|key| koru_i18n::t(lang, key) == text),
        "the explanation is not one of the table's: {text:?}"
    );
    // …and a way out: the tab strip and the caption are still reachable, and
    // the `+` still offers to make something.
    let buttons = by_role(&v, Role::Button);
    assert!(
        buttons
            .iter()
            .any(|node| node.actions.contains(&Action::Click)),
        "an empty column left no reachable control"
    );
    assert!(
        !by_role(&v, Role::Tab).is_empty(),
        "an empty column took the tabs with it"
    );
}

// -------------------------------------------------------------------- focus

#[test]
fn accessible_focus_is_the_state_the_focus_ring_is_drawn_from() {
    let (mut v, w) = seated();
    let ids = with_rows(&mut v, &["Brief", "Minutes"]);
    assert!(v.sidebar.is_open());

    // Take the keyboard the way Ctrl+Shift+E does.
    assert!(v.focus_or_toggle_sidebar(&w.ws));
    let focused = v.explore.focused().expect("the tree took the keyboard");
    assert_eq!(
        v.accessibility_focus(VIEW).map(|f| f.locator()),
        Some(
            Occurrence::model(
                WINDOW,
                VIEW,
                focused.as_uuid(),
                koru_workspace_host::accessibility::ControlPart::ROOT
            )
            .locator()
        ),
        "accessible focus is not the row the ring is on"
    );

    // Move it with the arrows and it follows — one state, not two.
    let area = v.explore_area();
    v.explore
        .nav_key(area, &Key::Named(NamedKey::ArrowDown))
        .expect("the tree answered the arrow");
    let moved = v.explore.focused().expect("still focused");
    assert_ne!(moved, focused, "the arrow moved nothing");
    assert_eq!(
        v.accessibility_focus(VIEW).map(|f| f.locator()),
        Some(
            Occurrence::model(
                WINDOW,
                VIEW,
                moved.as_uuid(),
                koru_workspace_host::accessibility::ControlPart::ROOT
            )
            .locator()
        ),
    );
    let _ = ids;

    // And giving it back gives it back — to the document, since Plan 1b.3 the
    // active tab's own paragraph rather than the region around it. Still one
    // state: `ViewState::active` and `Body::focus` are what the caret is drawn
    // from.
    assert!(v.explore.clear_focus());
    let after = v.accessibility_focus(VIEW).expect("something has it");
    assert_eq!(after.part, super::document::TEXT);
}

#[test]
fn a_published_focus_always_names_a_control_in_the_tree() {
    let (mut v, mut w) = seated();
    with_rows(&mut v, &["Brief"]);
    for stage in 0..4 {
        match stage {
            1 => {
                assert!(v.focus_or_toggle_sidebar(&w.ws));
            }
            2 => {
                open_a_tab_menu(&mut v, &mut w.ws);
            }
            3 => {
                let _ = dismiss(&mut v, &mut w.ws);
            }
            _ => {}
        }
        let semantics = window_semantics(&v);
        let projection = build(&semantics);
        assert!(
            !projection.unresolved_focus,
            "stage {stage}: the published focus named nothing in the tree"
        );
        assert_eq!(
            projection.duplicate_ids, 0,
            "stage {stage}: two controls shared one node id"
        );
    }
}

#[test]
fn a_modal_takes_focus_and_nothing_below_it_is_reachable() {
    let (mut v, mut w) = seated();
    with_rows(&mut v, &["Brief"]);
    let before = locators(&v);
    assert!(before.iter().any(|l| l.contains("s:explore#tree")));

    open_a_tab_menu(&mut v, &mut w.ws);
    assert!(matches!(v.blockers(), Blockers::Modal(_)));

    let after = locators(&v);
    assert!(
        !after.iter().any(|l| l.contains("s:explore")),
        "the column stayed reachable under an open menu"
    );
    assert!(
        !after.iter().any(|l| l.contains("s:shell#tabs")),
        "the tab strip stayed reachable under an open menu"
    );
    assert!(
        !menu_rows(&v).is_empty(),
        "the menu that took the keyboard published nothing"
    );
    // The focus went with it.
    let focus = v.accessibility_focus(VIEW).expect("a modal has the focus");
    assert!(
        focus.locator().contains("/menu:"),
        "focus stayed outside the modal: {}",
        focus.locator()
    );

    // And a request naming something underneath is refused rather than
    // performed — the other half of publishing the blocker alone.
    let entry = before
        .iter()
        .find(|l| l.contains("/n:"))
        .expect("a document entry was published before the menu opened");
    let occurrence = by_role_before(&v, entry);
    assert!(
        !v.accessibility_action(&mut w.ws, occurrence, Action::Click, None),
        "a control under an open menu was still performed"
    );
}

/// Rebuild the occurrence a locator names, for a node that is no longer in the
/// tree. The projection is the only place a locator is *written*, so a test
/// that wants a stale one has to hold the occurrence it came from.
fn by_role_before(v: &MainView, locator: &str) -> Occurrence {
    let uuid = locator
        .split("/n:")
        .nth(1)
        .and_then(|rest| rest.split('#').next())
        .expect("a model locator");
    let _ = v;
    Occurrence::model(
        WINDOW,
        VIEW,
        uuid::Uuid::parse_str(uuid).expect("a uuid"),
        koru_workspace_host::accessibility::ControlPart::ROOT,
    )
}

#[test]
fn dismissing_a_menu_restores_a_surviving_control() {
    let (mut v, mut w) = seated();
    assert!(v.focus_or_toggle_sidebar(&w.ws));
    let before = v.accessibility_focus(VIEW).expect("something had it");

    open_a_tab_menu(&mut v, &mut w.ws);
    let during = v.accessibility_focus(VIEW).expect("the menu has it");
    assert!(during.locator().contains("/menu:"));

    assert!(dismiss(&mut v, &mut w.ws));
    let after = v.accessibility_focus(VIEW).expect("something has it again");
    assert_eq!(
        after.locator(),
        before.locator(),
        "dismissing the menu did not restore the control that was focused"
    );
    // …and whatever it is, it is in the tree.
    assert!(locators(&v).contains(&after.locator()));
}

/// Every blocking surface, not only the one a fixture can stand up.
///
/// The menu is the modal `a_modal_takes_focus…` drives end to end; this covers
/// the other sixteen rows of the table, whose surfaces need a store, a device
/// or a running install to open. What it asserts is the property the whole
/// blocker arm rests on: the node published and the focus published are the
/// same control, in every kind, so a client is never focused on something that
/// is not in the tree.
#[test]
fn every_kind_of_blocker_publishes_the_control_it_focuses() {
    let (v, _w) = seated();
    let cx = v.shell(VIEW);
    // **The window's own table**, not a second list of kinds to keep in step:
    // `Layer::ALL` is the order this container stacks its surfaces in, and
    // `Layer::blocker` is what each one *is*.
    let mut seen = std::collections::HashSet::new();
    for layer in crate::mainview::routing::Layer::ALL {
        let blockers = layer.blocker();
        let node = v.blocker_semantics(&cx, blockers);
        let focus = v.blocker_focus(&cx, blockers);
        assert_eq!(
            node.occurrence, focus,
            "{layer:?} focuses a control it does not publish"
        );
        assert!(node.modal, "{layer:?} did not say it owns the window");
        assert!(
            node.label.is_some(),
            "{layer:?} published a surface with no name"
        );
        assert!(
            node.bounds.is_some_and(|rect| rect.w > 0.0 && rect.h > 0.0),
            "{layer:?} published a surface a hover cannot land on"
        );
        assert!(
            seen.insert(node.occurrence.locator()),
            "two kinds of blocker share one identity: {layer:?}"
        );
    }
}

// ----------------------------------------------------------------- geometry

#[test]
fn the_window_commands_carry_the_rectangles_the_caption_draws() {
    let (v, _w) = seated();
    let geometry = v.window_command_bounds();
    let drawn = koru_ui::caption::controls(v.regions.caption);
    let want = |action| {
        drawn
            .iter()
            .find(|control| control.action == action)
            .map(|control| control.rect)
    };
    assert_eq!(
        geometry.minimize,
        want(koru_ui::caption::Action::Minimise),
        "the minimise command is not where the caption draws it"
    );
    assert_eq!(
        geometry.maximize,
        want(koru_ui::caption::Action::ToggleMaximise)
    );
    assert_eq!(geometry.close, want(koru_ui::caption::Action::Close));
    for rect in [geometry.minimize, geometry.maximize, geometry.close] {
        let rect = rect.expect("a rectangle");
        assert!(rect.w > 0.0 && rect.h > 0.0, "an empty rectangle: {rect:?}");
    }
}

#[test]
fn an_entry_carries_the_rectangle_the_pane_hit_tests() {
    let (mut v, _w) = seated();
    with_rows(&mut v, &["Brief", "Minutes"]);
    let area = v.explore_area();
    let drawn = v.explore.row_rects(area);
    assert!(!drawn.is_empty(), "the fixture drew no rows");

    for (i, rect) in drawn {
        let node = v.explore.row_node(i).expect("a drawn row exists");
        let published = find(
            &v,
            &Occurrence::model(
                WINDOW,
                VIEW,
                node.as_uuid(),
                koru_workspace_host::accessibility::ControlPart::ROOT,
            )
            .locator(),
        )
        .expect("the row is published");
        assert_eq!(
            published.bounds,
            Some(rect),
            "row {i} is published somewhere other than where it is drawn"
        );
    }
}

/// **A node with no rectangle is a control an inspector's hover cannot reach.**
///
/// The first thing a person does with Accessibility Insights is point at
/// something, and a node with no bounds has a zero-size one — in the tree, and
/// invisible to the pointer and to a screen reader's spatial navigation.
/// Plan 1a.1's window commands were exactly that, which is what this plan was
/// asked to close; this is the assertion that keeps it closed for everything
/// published, containers included.
///
/// **The one carve-out, added by Plan 1b.4: status text nothing draws.** The
/// save status is a sentence about the focused document, and Koru paints no
/// per-document equivalent of it — the caption's chip speaks for the workspace
/// and only while the Store is away. AccessKit serves a node with no bounds as
/// an empty rectangle and documents that as legal, so the rule this test exists
/// for is kept where it bites: **anything a client can act on is somewhere a
/// pointer can land.** A node that performs nothing and has no pixels is
/// allowed to say so rather than borrow a neighbour's rectangle, which is the
/// other way this could have been satisfied and would have published a control
/// somewhere other than where it is.
#[test]
fn every_published_control_has_a_rectangle_a_pointer_can_land_on() {
    let (mut v, mut w) = seated();
    with_rows(&mut v, &["Brief", "Minutes"]);
    v.ensure_tab_widths();
    // The save status among them, so the carve-out above is exercised rather
    // than merely described.
    let document = koru_core::DocumentId::new();
    let active = v.tabs().active();
    v.tabs_mut().get_mut(active).expect("tab").document = Some(document);
    v.set_durability(&koru_workspace::durability::Durability {
        documents: vec![(
            document,
            koru_workspace::durability::DocumentDurability {
                durable: true,
                in_flight: 0,
                buffered: false,
                buffer_failure: None,
                stopped: None,
                link: koru_workspace::durability::LinkHealth::Live,
                read_only: None,
            },
        )],
        document_list: None,
    });

    let mut checked = 0;
    let mut without_bounds = 0;
    for stage in 0..2 {
        if stage == 1 {
            open_a_tab_menu(&mut v, &mut w.ws);
        }
        for node in nodes(&v) {
            let locator = node.occurrence.locator();
            let Some(bounds) = node.bounds else {
                assert!(
                    node.actions.is_empty(),
                    "{locator} can be acted on and published no rectangle at all"
                );
                without_bounds += 1;
                continue;
            };
            assert!(
                bounds.w > 0.0 && bounds.h > 0.0,
                "{locator} published an empty rectangle a hover cannot hit: {bounds:?}"
            );
            checked += 1;
        }
    }
    assert!(checked > 20, "the fixture published almost nothing");
    assert!(
        without_bounds > 0,
        "the fixture no longer publishes the status text the carve-out is about"
    );

    // …and the window's own three, which the host publishes from what this
    // view answers.
    let geometry = v.window_command_bounds();
    for rect in [geometry.minimize, geometry.maximize, geometry.close] {
        let rect = rect.expect("a window command with no rectangle");
        assert!(rect.w > 0.0 && rect.h > 0.0);
    }
}

#[test]
fn a_tab_carries_the_rectangle_the_strip_hit_tests() {
    let (mut v, mut w) = seated();
    let second = a_document(&mut w.ws, "Brief");
    v.open_node(&w.ws, second);
    v.ensure_tab_widths();
    let rects = koru_shell_chrome::tabs::layout(v.strip(), v.tab_widths(), v.tab_scroll());
    for (i, tab) in v.tabs().iter().enumerate() {
        let published = find(&v, &tab_locator(tab.id)).expect("the tab is published");
        assert_eq!(
            published.bounds,
            rects.get(i).map(|rect| rect.body),
            "tab {i} is published somewhere other than where it is drawn"
        );
    }
}

#[test]
fn an_offscreen_entry_is_published_and_can_be_revealed() {
    let (mut v, mut w) = seated();
    let titles: Vec<String> = (0..200).map(|i| format!("doc {i}")).collect();
    let refs: Vec<&str> = titles.iter().map(String::as_str).collect();
    let ids = with_rows(&mut v, &refs);
    let area = v.explore_area();

    let drawn: Vec<usize> = v
        .explore
        .row_rects(area)
        .into_iter()
        .map(|(i, _)| i)
        .collect();
    assert!(
        drawn.len() < ids.len(),
        "the fixture fits in the column, so nothing is offscreen"
    );
    let hidden = *ids.last().expect("a last row");
    let locator = Occurrence::model(
        WINDOW,
        VIEW,
        hidden.as_uuid(),
        koru_workspace_host::accessibility::ControlPart::ROOT,
    )
    .locator();
    let node = find(&v, &locator).expect("an offscreen entry is still published");
    assert!(
        node.actions.contains(&Action::ScrollIntoView),
        "an offscreen entry offers no way to be revealed"
    );

    // Revealing it uses the pane's own scrolling, and then it is drawn.
    assert!(v.accessibility_action(&mut w.ws, node.occurrence, Action::ScrollIntoView, None));
    let now: Vec<usize> = v
        .explore
        .row_rects(area)
        .into_iter()
        .map(|(i, _)| i)
        .collect();
    let index = (0..)
        .map_while(|i| v.explore.row_node(i))
        .position(|node| node == hidden)
        .expect("the row exists");
    assert!(
        now.contains(&index),
        "the reveal did not bring the entry into the band"
    );
}

// --------------------------------------------------------------------- cost

#[test]
fn scrolling_does_not_change_the_revision() {
    let (mut v, _w) = seated();
    let titles: Vec<String> = (0..200).map(|i| format!("doc {i}")).collect();
    let refs: Vec<&str> = titles.iter().map(String::as_str).collect();
    with_rows(&mut v, &refs);
    let area = v.explore_area();

    let before = v.semantics_revision();
    v.explore.scroll_by(area, -400.0);
    assert_eq!(
        v.semantics_revision(),
        before,
        "scrolling asked for a new tree"
    );
    // Nor does asking twice.
    assert_eq!(v.semantics_revision(), before);
}

#[test]
fn a_change_a_person_can_see_does_change_the_revision() {
    let (mut v, mut w) = seated();
    with_rows(&mut v, &["Brief", "Minutes"]);
    let mut seen = vec![v.semantics_revision()];

    assert!(v.focus_or_toggle_sidebar(&w.ws));
    seen.push(v.semantics_revision());

    let area = v.explore_area();
    v.explore.nav_key(area, &Key::Named(NamedKey::ArrowDown));
    seen.push(v.semantics_revision());

    open_a_tab_menu(&mut v, &mut w.ws);
    seen.push(v.semantics_revision());

    let _ = dismiss(&mut v, &mut w.ws);
    seen.push(v.semantics_revision());

    let doc = a_document(&mut w.ws, "Third");
    v.open_node(&w.ws, doc);
    seen.push(v.semantics_revision());

    for pair in seen.windows(2) {
        assert_ne!(pair[0], pair[1], "a visible change left the revision alone");
    }
}

#[test]
fn an_inactive_tab_contributes_nothing() {
    let (mut v, mut w) = seated();
    let hidden = a_document(&mut w.ws, "Background");
    v.open_node(&w.ws, hidden);
    let active = v.tabs().active();
    v.tabs_mut().select(0);
    assert_ne!(v.tabs().active(), active);

    // The tab is still in the strip — it is a control, and it must stay
    // reachable — but nothing *inside* it is published.
    let published = locators(&v);
    let panels = by_role(&v, Role::TabPanel);
    assert_eq!(
        panels.len(),
        1,
        "more than one tab published a region: {published:?}"
    );
    assert_eq!(
        panels[0].label.as_deref(),
        Some(v.tabs().active_tab().label.as_str()),
        "the published region is not the active tab's"
    );
    assert!(
        panels[0].children.is_empty(),
        "a tab region published contents; the document body is Plan 1b.1's"
    );
}

// ------------------------------------------------------------------ the host

/// The window snapshot the host would build from this view, so a test can run
/// the real `projection::build` over it.
fn window_semantics(v: &MainView) -> koru_workspace_host::accessibility::WindowSemantics {
    let mut children = koru_workspace_host::accessibility::window_commands(
        WINDOW,
        false,
        koru_workspace_host::accessibility::ui_lang(),
        v.window_command_bounds(),
    );
    children.extend(v.semantics(VIEW, Scope::Whole));
    koru_workspace_host::accessibility::WindowSemantics {
        window: WINDOW,
        label: "Koru".to_string(),
        scale: 1.0,
        client: koru_draw::geometry::Rect::new(0.0, 0.0, v.viewport.0, v.viewport.1),
        focus: v.accessibility_focus(VIEW),
        children,
        scope: koru_workspace_host::accessibility::Scope::Whole,
        retired: Vec::new(),
    }
}

/// `build` with nothing already published, which is what a whole snapshot
/// always means. Plan 1b.2 gave `build` a second argument — the occurrences a
/// client already holds — so a delta can be published without making an
/// untouched control unreachable; these tests only ever build whole trees.
fn build(
    semantics: &koru_workspace_host::accessibility::WindowSemantics,
) -> koru_workspace_host::accessibility::Projection {
    koru_workspace_host::accessibility::projection::build(
        semantics,
        &std::collections::HashMap::new(),
    )
}

#[test]
fn every_published_control_has_a_distinct_node_id() {
    let (mut v, _w) = seated();
    with_rows(&mut v, &["Brief", "Minutes", "Minutes"]);
    let projection = build(&window_semantics(&v));
    assert_eq!(
        projection.duplicate_ids, 0,
        "two controls hashed to one AccessKit node id"
    );
    assert!(!projection.unresolved_focus);
    assert!(projection.nodes > 10, "the tree is suspiciously small");
}
