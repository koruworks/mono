// SPDX-License-Identifier: MPL-2.0

//! What the Tasks page promises a screen reader, asserted against fabricated
//! rows rather than against a store.
//!
//! Nothing here touches disk, dials a store, opens a browser or drives a
//! desktop: the page is seeded the way the rest of this directory's suite seeds
//! it, and the projection is a pure function of it. The one thing these cannot
//! establish — that Windows, Narrator and NVDA agree with any of it — is the
//! interactive lane's, and is written down in
//! `docs/features/accessibility/README.md`.

use accesskit::{Action, Role, Toggled};
use chrono::NaiveDate;
use koru_core::NodeId;
use koru_draw::geometry::Rect;
use koru_i18n::Lang;
use koru_store_wire::attributes::AttributeRowValue;
use koru_store_wire::tasks::TaskRow;
use koru_workspace_host::View as _;
use koru_workspace_host::accessibility::{
    Occurrence, Revision, SemanticNode, ViewSlot, WindowSlot,
};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};

use super::*;
use crate::panes::explore::rail::Bucket;
use crate::tasks_view::query::TaskEntry;
use crate::tasks_view::view::Request;

const WINDOW: WindowSlot = WindowSlot::from_raw(1);
const VIEW: ViewSlot = ViewSlot::from_raw(2);

/// The content region, deliberately **not** at the window's origin: every
/// rectangle this module publishes has to be shifted by it, and a fixture at
/// `0,0` would let a missing translation pass.
const AREA: Rect = Rect {
    x: 260.0,
    y: 44.0,
    w: 1000.0,
    h: 700.0,
};

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 8, 31).expect("a real day")
}

fn cx() -> Context {
    Context {
        window: WINDOW,
        view: VIEW,
        area: AREA,
        lang: Lang::En,
    }
}

fn wire_row(title: &str) -> TaskRow {
    let workspace = koru_store_core::ids::WorkspaceId::new();
    let document = koru_store_core::ids::DocumentId::new();
    let node = NodeId::new();
    TaskRow {
        workspace,
        document,
        node: node.to_storage(),
        task: format!(
            "task:{}:{}:{}",
            workspace.to_storage(),
            document.to_storage(),
            node.to_storage()
        ),
        node_kind: "paragraph".to_owned(),
        lifecycle: "open".to_owned(),
        document_trashed: false,
        in_trash: false,
        breadcrumb: String::new(),
        title: title.to_owned(),
        attributes: Vec::<AttributeRowValue>::new(),
    }
}

fn entry(title: &str) -> TaskEntry {
    TaskEntry::of(&wire_row(title), today(), 0).expect("a parseable row")
}

/// A page of `titles`, sized to [`AREA`].
fn page(titles: &[&str]) -> TasksView {
    let mut view = TasksView::new(RailTarget::Bucket(Bucket::Today));
    view.set_viewport(AREA.w, AREA.h);
    view.seed_for_test(titles.iter().map(|title| entry(title)).collect(), today());
    view
}

/// The whole tree, flattened for searching.
fn nodes(view: &TasksView) -> Vec<SemanticNode> {
    let mut out = Vec::new();
    flatten(&view.semantics(&cx()), &mut out);
    out
}

fn flatten(node: &SemanticNode, out: &mut Vec<SemanticNode>) {
    out.push(node.clone());
    for child in &node.children {
        flatten(child, out);
    }
}

fn find(view: &TasksView, locator: &str) -> Option<SemanticNode> {
    nodes(view)
        .into_iter()
        .find(|node| node.occurrence.locator() == locator)
}

fn by_role(view: &TasksView, role: Role) -> Vec<SemanticNode> {
    nodes(view)
        .into_iter()
        .filter(|node| node.role == role)
        .collect()
}

fn row_occurrence(view: &TasksView, i: usize, part: ControlPart) -> Occurrence {
    Occurrence::model(WINDOW, VIEW, view.rows[i].node.as_uuid(), part)
}

fn revision(view: &TasksView) -> u64 {
    let mut rev = Revision::new();
    view.revise(&mut rev);
    rev.finish()
}

// ---------------------------------------------- the middle of three duplicates

/// **The requirement the whole plan turns on.** Three rows, two of them
/// identically titled, and a client acting on the second must hit the second.
#[test]
fn three_rows_with_a_duplicate_title_are_three_distinct_controls() {
    let view = page(&["Draft", "Draft", "Draft"]);
    let rows = by_role(&view, Role::ListBoxOption);
    assert_eq!(rows.len(), 3, "one control per row");
    assert!(
        rows.iter().all(|row| row.label.as_deref() == Some("Draft")),
        "the fixture is three rows a person cannot tell apart by name"
    );
    let locators: std::collections::BTreeSet<String> =
        rows.iter().map(|row| row.occurrence.locator()).collect();
    assert_eq!(
        locators.len(),
        3,
        "identity is the task's node, never its label"
    );
}

/// **What a reader hears that tells them apart.** `position_in_set` is stored
/// zero-based because `accesskit_windows` adds the one a person hears, and the
/// total is on the container because that is the only place the consumer reads
/// it from.
#[test]
fn the_middle_of_three_is_two_of_three_by_ear() {
    let view = page(&["Draft", "Draft", "Draft"]);
    let rows = by_role(&view, Role::ListBoxOption);
    assert_eq!(
        rows.iter().map(|row| row.position).collect::<Vec<_>>(),
        vec![Some(0), Some(1), Some(2)],
        "stored from zero; the adapter adds the one a reader hears, so the \
         middle row is announced as 2"
    );
    assert!(
        rows.iter().all(|row| row.size_of_set.is_none()),
        "an item's own total is never consulted on Windows"
    );
    let list = by_role(&view, Role::ListBox);
    assert_eq!(list.len(), 1, "one container");
    assert_eq!(
        list[0].size_of_set,
        Some(3),
        "the total lives on the container the consumer walks up to"
    );
}

/// The container's role is load-bearing rather than decorative:
/// `accesskit_consumer::is_container_with_selectable_children` does not include
/// `Role::List`, so a list published as one would answer *3* with no *of 3*.
#[test]
fn the_container_is_a_role_accesskit_counts_as_one() {
    let view = page(&["a", "b"]);
    let list = find(&view, "koru:w1/v2/s:tasks#list").expect("the list");
    assert_eq!(list.role, Role::ListBox);
    assert!(
        matches!(
            list.role,
            Role::ComboBox
                | Role::EditableComboBox
                | Role::Grid
                | Role::ListBox
                | Role::ListGrid
                | Role::Menu
                | Role::MenuBar
                | Role::MenuListPopup
                | Role::RadioGroup
                | Role::TabList
                | Role::Toolbar
                | Role::Tree
        ),
        "the total is read off a container of selectable children and nowhere else"
    );
}

/// **A row that carried `selected` could not be invoked.** Which row the
/// keyboard is on is focus, and it is published as focus.
#[test]
fn no_row_carries_selected_so_every_row_keeps_invoke() {
    let mut view = page(&["a", "b", "c"]);
    view.select_row(1);
    assert!(
        by_role(&view, Role::ListBoxOption)
            .iter()
            .all(|row| row.selected.is_none()),
        "`is_invocable` excludes a node whose `is_selected()` is Some"
    );
    assert!(
        by_role(&view, Role::ListBoxOption)
            .iter()
            .all(|row| row.actions.contains(&Action::Click)),
        "every row can still be opened"
    );
}

/// Identity survives what a label cannot: a rename moves the words and nothing
/// else.
#[test]
fn a_rows_locator_survives_a_rename_and_a_change_of_language() {
    let mut view = page(&["Draft", "Draft"]);
    let before: Vec<String> = by_role(&view, Role::ListBoxOption)
        .iter()
        .map(|row| row.occurrence.locator())
        .collect();
    view.rows[1].title = "Something else entirely".to_owned();
    let after: Vec<String> = by_role(&view, Role::ListBoxOption)
        .iter()
        .map(|row| row.occurrence.locator())
        .collect();
    assert_eq!(before, after, "a rename moves no identity");

    for lang in Lang::ALL {
        let mut out = Vec::new();
        flatten(&view.semantics(&Context { lang, ..cx() }), &mut out);
        let locators: Vec<String> = out.iter().map(|n| n.occurrence.locator()).collect();
        let mut english = Vec::new();
        flatten(&view.semantics(&cx()), &mut english);
        assert_eq!(
            locators,
            english
                .iter()
                .map(|n| n.occurrence.locator())
                .collect::<Vec<_>>(),
            "{} moved a locator",
            lang.tag()
        );
    }
}

// ------------------------------------------------- the box and the lifecycle

/// The tick on this page *is* the canonical lifecycle, and it says so with the
/// word a task uses rather than the one a checklist item uses.
#[test]
fn the_tick_publishes_the_canonical_lifecycle() {
    let mut view = page(&["open", "done"]);
    view.rows[1].completed = true;
    let ticks = by_role(&view, Role::CheckBox);
    assert_eq!(ticks.len(), 2);
    assert_eq!(ticks[0].toggled, Some(Toggled::False));
    assert_eq!(ticks[1].toggled, Some(Toggled::True));
    assert_eq!(
        ticks[0].label.as_deref(),
        Some(koru_i18n::t(Lang::En, koru_i18n::Key::A11yTaskDone)),
        "the Tasks page's box is the lifecycle and is named for it"
    );
    assert_ne!(
        koru_i18n::t(Lang::En, koru_i18n::Key::A11yTaskDone),
        koru_i18n::t(Lang::En, koru_i18n::Key::A11yChecklistDone),
        "a checklist item's box and a task's lifecycle are never the same word"
    );
}

/// SPEC §5's concurrent residue — completed and still holding unfinished work —
/// is the one thing the tick cannot say, so it gets a control exactly where the
/// warning triangle is drawn and nowhere else.
#[test]
fn residue_is_published_only_where_the_triangle_is_drawn() {
    let mut view = page(&["quiet", "loud"]);
    view.rows[0].completed = true;
    view.rows[1].completed = true;
    view.rows[1].residue = true;
    let states: Vec<String> = nodes(&view)
        .into_iter()
        .filter(|node| node.occurrence.part == parts::STATE)
        .map(|node| node.occurrence.locator())
        .collect();
    assert_eq!(
        states,
        vec![row_occurrence(&view, 1, parts::STATE).locator()],
        "only the row with unfinished work inside it says so"
    );
}

// --------------------------------------------------------------- the actions

/// Acting on the middle of three identically titled rows reaches the middle
/// one — the plan's own verification, driven through the request the container
/// drains.
#[test]
fn ticking_the_middle_of_three_duplicates_completes_the_middle_one() {
    let mut view = page(&["Draft", "Draft", "Draft"]);
    let middle = view.rows[1].node;
    assert!(view.accessibility_action(row_occurrence(&view, 1, parts::CHECK), Action::Click));
    match view.take_request() {
        Some(Request::Complete {
            node, completed, ..
        }) => {
            assert_eq!(node, middle, "the second row, not a neighbour");
            assert!(completed);
        }
        other => panic!("expected the canonical complete command, got {other:?}"),
    }
}

/// An invoke on a row opens it, in this window — `Enter`'s own disposition, so
/// a client and the keyboard cannot open two different things.
#[test]
fn invoking_a_row_asks_for_the_same_open_enter_asks_for() {
    let mut view = page(&["a", "b"]);
    assert!(view.accessibility_action(row_occurrence(&view, 1, parts::ROW), Action::Click));
    let from_client = view.take_request();

    let mut keyboard = page(&["a", "b"]);
    keyboard.select_row(1);
    assert!(keyboard.open_selected(false));
    let from_enter = keyboard.take_request();

    match (from_client, from_enter) {
        (Some(Request::Open { new_tab: a, .. }), Some(Request::Open { new_tab: b, .. })) => {
            assert_eq!(a, b, "one gesture, one disposition");
            assert!(!a, "here, not in a new tab");
        }
        other => panic!("both routes must ask to open: {other:?}"),
    }
}

/// The `…` on a row asks for the same menu the pointer's `…` asks for, over the
/// row it was aimed at.
#[test]
fn the_rows_menu_button_opens_the_menu_for_that_row() {
    let mut view = page(&["a", "b", "c"]);
    let wanted = view.rows[2].node;
    assert!(view.accessibility_action(row_occurrence(&view, 2, parts::ACTIONS), Action::Click));
    match view.take_request() {
        Some(Request::RowMenu { node, .. }) => assert_eq!(node, wanted),
        other => panic!("expected the row menu, got {other:?}"),
    }
}

/// **A stale action cannot affect a neighbour.** The list refreshes under the
/// reader every half minute; a request naming a row that has left it performs
/// nothing rather than landing on whatever slid into that position.
#[test]
fn a_request_for_a_row_that_has_left_the_page_performs_nothing() {
    let mut view = page(&["gone", "stayed"]);
    let stale = row_occurrence(&view, 0, parts::CHECK);
    let survivor = view.rows[1].node;
    view.seed_for_test(vec![view.rows[1].clone()], today());
    assert_eq!(view.rows.len(), 1);
    assert!(
        !view.accessibility_action(stale, Action::Click),
        "the departed row's id resolves to nothing"
    );
    assert!(view.take_request().is_none(), "and nothing was queued");
    // …and the neighbour that took its place is untouched and still actable.
    assert!(view.accessibility_action(
        Occurrence::model(WINDOW, VIEW, survivor.as_uuid(), parts::CHECK),
        Action::Click
    ));
    assert!(matches!(
        view.take_request(),
        Some(Request::Complete { node, .. }) if node == survivor
    ));
}

/// A control this page does not perform on that target answers `false`, which
/// is the honest reply to the `Focus` Windows sends ahead of every invoke.
#[test]
fn an_action_this_page_does_not_perform_is_refused() {
    let mut view = page(&["a"]);
    assert!(!view.accessibility_action(row_occurrence(&view, 0, parts::CHECK), Action::Focus));
    assert!(!view.accessibility_action(
        row_occurrence(&view, 0, parts::ROW),
        Action::SetTextSelection
    ));
    assert!(view.take_request().is_none());
}

// --------------------------------------------------- the capture row and focus

/// **The one gesture on this page a pointer could reach and a keyboard could
/// not.** Up from the first row lands in the capture line, which is the list's
/// own first row.
#[test]
fn up_from_the_first_row_puts_the_caret_in_the_capture_line() {
    let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("create");
    let mut view = page(&["a", "b"]);
    view.key(
        &mut ws,
        &WKey::Named(NamedKey::ArrowDown),
        None,
        ModifiersState::empty(),
    );
    assert_eq!(view.selected_row(), Some(0));
    assert!(view.capture.is_none());
    view.key(
        &mut ws,
        &WKey::Named(NamedKey::ArrowUp),
        None,
        ModifiersState::empty(),
    );
    assert!(view.capture.is_some(), "the caret is in the capture row");
    assert_eq!(
        view.accessible_focus(&cx()),
        cx().surface(parts::CAPTURE),
        "and that is where a screen reader is told the keyboard is"
    );
}

/// A client puts the caret there the way a press does.
#[test]
fn a_client_can_put_the_caret_in_the_capture_row() {
    let mut view = page(&["a"]);
    let capture = cx().surface(parts::CAPTURE);
    assert!(view.accessibility_action(capture, Action::Focus));
    assert!(view.capture.is_some());
}

/// Accessible focus is a function over the state the page is drawn from, never
/// a second field kept in step.
#[test]
fn focus_is_the_row_the_page_has_selected() {
    let mut view = page(&["a", "b", "c"]);
    assert_eq!(view.accessible_focus(&cx()), cx().surface(parts::PAGE));
    view.select_row(2);
    assert_eq!(
        view.accessible_focus(&cx()),
        row_occurrence(&view, 2, parts::ROW)
    );
}

// -------------------------------------------------------------- open panels

/// A panel owns everything beneath it — the same answer the keyboard gives,
/// where `key` never reaches the list while one is up.
#[test]
fn an_open_panel_takes_the_page_and_is_named() {
    let mut view = page(&["a", "b"]);
    let (document, node) = (view.rows[0].document, view.rows[0].node);
    assert!(view.open_priority_picker(document, node));
    let panel = nodes(&view)
        .into_iter()
        .find(|n| n.role == Role::Group && n.modal)
        .expect("the open panel is published");
    assert_eq!(
        panel.label.as_deref(),
        Some(koru_i18n::t(
            Lang::En,
            koru_i18n::Key::A11yTaskPriorityPicker
        ))
    );
    assert_eq!(view.accessible_focus(&cx()), panel.occurrence);
    assert!(
        !view.accessibility_action(row_occurrence(&view, 1, parts::CHECK), Action::Click),
        "a request at the list underneath is refused while a panel stands"
    );
}

/// **A panel that closed and reopened is a new instance.** `ObjectKey::Instance`
/// with a number its owner never reuses, so a request queued against the one
/// that went cannot land in its successor.
#[test]
fn a_reopened_panel_does_not_inherit_the_closed_ones_identity() {
    let mut view = page(&["a"]);
    let (document, node) = (view.rows[0].document, view.rows[0].node);
    assert!(view.open_priority_picker(document, node));
    let first = view.accessible_focus(&cx());
    view.picker = None;
    assert!(view.open_priority_picker(document, node));
    let second = view.accessible_focus(&cx());
    assert_ne!(
        first, second,
        "one opening, one number, and it is never handed out twice"
    );
}

// -------------------------------------------------------------- the geometry

/// Every rectangle is published where the control answers a press: the page's
/// own arithmetic, shifted into the window's space exactly once.
#[test]
fn every_rectangle_is_the_pages_own_shifted_into_the_window() {
    let view = page(&["a", "b"]);
    let line = view
        .lines
        .iter()
        .position(|l| l == &Line::Task(1))
        .expect("row 1 has a line");
    let rect = view.table().row_rect(&view.bands(), line);
    let published = find(&view, &row_occurrence(&view, 1, parts::ROW).locator())
        .expect("the row")
        .bounds
        .expect("with bounds");
    assert_eq!(published.x, rect.x + AREA.x);
    assert_eq!(published.y, rect.y + AREA.y);
    assert_eq!(published.w, rect.w);
    assert_eq!(published.h, rect.h);

    let tick = find(&view, &row_occurrence(&view, 1, parts::CHECK).locator())
        .expect("the tick")
        .bounds
        .expect("with bounds");
    assert_eq!(
        tick,
        Rect::new(
            view.parts(rect).control.x + AREA.x,
            view.parts(rect).control.y + AREA.y,
            view.parts(rect).control.w,
            view.parts(rect).control.h,
        ),
        "the tick is where the press on it is resolved"
    );
}

/// A row below the fold is published anyway, at the rectangle it would occupy —
/// culling is for drawing.
#[test]
fn a_row_below_the_fold_is_still_a_control() {
    let titles: Vec<String> = (0..80).map(|i| format!("row {i}")).collect();
    let refs: Vec<&str> = titles.iter().map(String::as_str).collect();
    let view = page(&refs);
    assert!(
        view.table().visible(&view.bands()).len() < 80,
        "the fixture has to be longer than the band for this to mean anything"
    );
    assert_eq!(
        by_role(&view, Role::ListBoxOption).len(),
        80,
        "every row in the answer is a control, wherever it is"
    );
}

// ------------------------------------------------------------- the revision

/// The fingerprint changes exactly when the tree would, and not when something
/// moves that the tree does not carry.
#[test]
fn scrolling_does_not_rebuild_and_ticking_does() {
    let titles: Vec<String> = (0..80).map(|i| format!("row {i}")).collect();
    let refs: Vec<&str> = titles.iter().map(String::as_str).collect();
    let mut view = page(&refs);
    let before = revision(&view);
    view.scroll_by(winit::event::MouseScrollDelta::LineDelta(0.0, -4.0));
    assert_eq!(
        revision(&view),
        before,
        "a scrolled row keeps its identity, its label and its place in the set"
    );
    view.rows[3].completed = true;
    assert_ne!(revision(&view), before, "a tick is news");
}

/// **The count above the list is a sentence, and it is silent until the page
/// has actually been answered** — a `0` meaning *we have not looked yet* is a
/// lie a reader cannot see through, which is `count_label`'s own rule.
#[test]
fn the_count_is_a_localized_sentence_and_silent_before_the_first_answer() {
    let mut view = page(&["a", "b"]);
    let locator = cx().surface(parts::COUNT).locator();
    assert!(
        find(&view, &locator).is_none(),
        "a page nobody has answered yet published a count"
    );

    view.panel_mut().accept_for_test(
        &koru_store_wire::tasks::TaskQueryResponse {
            rows: vec![wire_row("a"), wire_row("b")],
            definitions: Vec::new(),
            tags: Vec::new(),
            groups: Vec::new(),
            projected: true,
            updating_workspaces: Vec::new(),
            limit: 100,
            next_after: None,
            restarted: false,
            covered: None,
        },
        today(),
        0,
    );

    let open = view.panel().open_count() as u64;
    for lang in [Lang::En, Lang::De, Lang::Ja] {
        let mut out = Vec::new();
        flatten(&view.semantics(&Context { lang, ..cx() }), &mut out);
        let count = out
            .into_iter()
            .find(|node| node.occurrence.locator() == locator)
            .expect("the count");
        assert_eq!(count.role, Role::Label);
        assert_eq!(
            count.label.as_deref(),
            Some(
                koru_i18n::fill(
                    koru_i18n::t(lang, koru_i18n::Key::A11yTaskOpenCount),
                    &[&koru_i18n::number::decimal(lang, open)]
                )
                .as_str()
            ),
            "{} did not say how much work the page is showing",
            lang.tag()
        );
    }
}

/// The page and its list are named in the reader's language, and the name is
/// never part of an identity.
#[test]
fn the_queues_name_is_localized_and_the_drawn_heading_is_not() {
    let view = page(&["a"]);
    for (lang, expected) in [(Lang::En, "Today"), (Lang::De, "Heute"), (Lang::Ja, "今日")] {
        let node = view.semantics(&Context { lang, ..cx() });
        assert_eq!(node.label.as_deref(), Some(expected));
    }
    assert_eq!(
        view.title(),
        "Today",
        "the drawn heading is English in every language — SPEC §11's debt, \
         recorded in ACCEPTANCE.md rather than respelled here"
    );
}

/// **The list chip is published as a control** — a button that opens the
/// list picker to change the list, never the list's page (the owner's ask,
/// 2026-11-05). Going to the list is the row menu's *Go to list*.
#[test]
fn the_list_chip_is_a_control_and_opens_the_list_picker() {
    use koru_core::ids::ListId;
    let list = ListId::new();
    let mut view = page(&["Buy milk"]);
    view.rows[0].list = Some(list);
    view.set_lists(vec![crate::tasks_view::ListFace {
        id: list,
        name: "Bike Project".to_owned(),
        color: None,
    }]);

    let chip = find(&view, &row_occurrence(&view, 0, parts::LIST_CHIP).locator())
        .expect("the list chip is published");
    assert_eq!(chip.role, Role::Button);
    assert_eq!(
        chip.label.as_deref(),
        Some("Bike Project"),
        "the list's own name, which is the person's content"
    );

    assert!(view.accessibility_action(row_occurrence(&view, 0, parts::LIST_CHIP), Action::Click));
    assert_eq!(view.take_request(), None, "nothing opened");
    assert!(view.list_picker_for_test().is_some(), "the list picker");
}

/// A row below the fold is revealed through the page's own scrolling, which is
/// the only kind there is.
#[test]
fn scroll_into_view_reveals_a_row_below_the_fold() {
    let titles: Vec<String> = (0..80).map(|i| format!("row {i}")).collect();
    let refs: Vec<&str> = titles.iter().map(String::as_str).collect();
    let mut view = page(&refs);
    let last = view.rows.len() - 1;
    assert!(
        !view.visible_for_test().contains(&last),
        "the fixture has to start with the last row out of the band"
    );
    assert!(view.accessibility_action(
        row_occurrence(&view, last, parts::ROW),
        Action::ScrollIntoView
    ));
    let line = view
        .lines
        .iter()
        .position(|l| l == &Line::Task(last))
        .expect("the last row has a line");
    assert!(
        view.visible_for_test().contains(&line),
        "the page did not scroll to the row a client asked for"
    );
    assert!(
        view.take_request().is_none(),
        "revealing is not opening, and asks the container for nothing"
    );
}
