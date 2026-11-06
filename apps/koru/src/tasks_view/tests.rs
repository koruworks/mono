// SPDX-License-Identifier: MPL-2.0

//! What the Tasks tab promises, asserted against fabricated answers rather than
//! against a store.
//!
//! **Nothing here dials anything.** `TaskListPanel::set_client` is never called,
//! so `start` returns before it spawns — the same `no-reader` discipline the
//! search surface follows, arrived at from the other side: a test that asserted
//! "no store is listening" would pass on CI and fail on the machine of anybody
//! with Koru open.

use crate::panes::explore::rail::{Bucket, RailTarget};
use chrono::NaiveDate;
use koru_core::{DocumentId, NodeId, WorkspaceId};
use koru_draw::geometry::Rect;
use koru_store_wire::attributes::AttributeRowValue;
use koru_store_wire::tasks::{TaskGroup, TaskQueryResponse, TaskRow};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use koru_workspace_host::View as _;

use koru_store_wire::tasks::{LifecycleFilter, TaskSort};

use super::menu::{OverflowRow, overflow_items};
use super::query::{self, Filters, RowKind, TaskEntry};
use super::view::Hit;
use super::{Request, Rule, TasksView, split_date};

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 8, 31).expect("a real day")
}

fn now_ms() -> i64 {
    // Mid-morning on `today`, so "before now" is a strictly smaller set than
    // "before the end of today" and the two edges cannot pass by coincidence.
    today()
        .and_hms_opt(10, 0, 0)
        .expect("a real time")
        .and_utc()
        .timestamp_millis()
}

fn attr(role: &str, kind: &str, value: &str) -> AttributeRowValue {
    AttributeRowValue {
        id: format!("{role}-1"),
        role: role.to_owned(),
        kind: kind.to_owned(),
        value: value.to_owned(),
        zone: None,
        label: None,
    }
}

fn wire_row(title: &str, attributes: Vec<AttributeRowValue>) -> TaskRow {
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
        breadcrumb: "Test / More Documents".to_owned(),
        title: title.to_owned(),
        attributes,
    }
}

fn entry(title: &str, due: Option<&str>) -> TaskEntry {
    let attributes = due
        .map(|value| vec![attr(koru_core::attributes::KEY_DUE, "date", value)])
        .unwrap_or_default();
    TaskEntry::of(&wire_row(title, attributes), today(), now_ms()).expect("a parseable row")
}

fn view_on(target: RailTarget, rows: Vec<TaskEntry>) -> TasksView {
    let mut view = TasksView::new(target);
    view.set_viewport(1000.0, 700.0);
    view.seed_for_test(rows, today());
    view
}

// ------------------------------------------------------------- the geometry

/// **Row geometry hits back.** Every rectangle a row draws into comes from
/// `parts`, and every press is resolved through `hit_at`; this drives one
/// against the other so a control cannot become clickable where it is not drawn.
#[test]
fn row_geometry_hits_back() {
    let view = view_on(
        RailTarget::Bucket(Bucket::Today),
        vec![entry("First", Some("2026-08-30")), entry("Second", None)],
    );
    for row in 0..2 {
        let line = view
            .line_of_task_for_test(row)
            .expect("every task earns a line");
        let r = view.line_rect_for_test(line);
        let parts = view.parts_for_test(r);
        let at = |rect: Rect| (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);

        let (x, y) = at(parts.control);
        assert_eq!(
            view.hit_for_test(x, y),
            Some(Hit::Complete(row)),
            "the middle of row {row}'s complete control is that control"
        );
        let (x, y) = at(parts.dots);
        assert_eq!(
            view.hit_for_test(x, y),
            Some(Hit::Dots(row)),
            "the middle of row {row}'s trailing lane is its menu button"
        );
        let (x, y) = at(parts.text);
        assert_eq!(
            view.hit_for_test(x, y),
            Some(Hit::Row(row)),
            "the text column is the row itself, which is what opens it"
        );
    }
}

/// **The row's spacing steps are the page's numbers.** The header and the
/// capture row still read `PAD`, `CONTROL_GAP`, `CHIP_GAP` and `LANE_W`; the
/// element row names the same lengths as steps so the inspector shows a token,
/// and this is what keeps the two from drifting apart.
#[test]
fn the_rows_steps_are_the_pages_numbers() {
    use super::row::{CHIP_GAP_STEP, CONTROL_GAP_STEP, LANE_STEP, PAD_STEP, TAG_GAP};
    use super::view::{CHIP_GAP, CONTROL_GAP, LANE_W, PAD};
    let px = |space: koru_draw::theme::Space| space.px(koru_draw::theme::Profile::Desktop);
    assert_eq!(px(PAD_STEP), PAD);
    assert_eq!(px(CONTROL_GAP_STEP), CONTROL_GAP);
    assert_eq!(px(CHIP_GAP_STEP), CHIP_GAP);
    assert_eq!(px(LANE_STEP), LANE_W);
    assert_eq!(px(TAG_GAP), koru_draw::chip::CHIP_GAP);
}

/// **The element row lands where the arithmetic it replaced did**, to the
/// pixel: the control at the page margin, the text a control and a gap in,
/// the `…` lane at the far margin, and the chips laid in from the right with
/// the chip gap between them and the lane gap either side.
#[test]
fn the_element_row_reproduces_the_rows_geometry() {
    use super::view::{CHIP_GAP, CONTROL_GAP, CONTROL_W, DOTS_W, LANE_W, PAD};
    let r = Rect::new(0.0, 100.0, 900.0, super::row::row_h());
    let parts = super::row::parts(r);
    let control = Rect::new(
        r.x + PAD,
        r.y + (r.h - CONTROL_W) * 0.5,
        CONTROL_W,
        CONTROL_W,
    );
    let dots = Rect::new(
        r.x + r.w - PAD - LANE_W + (LANE_W - DOTS_W) * 0.5,
        r.y + (r.h - DOTS_W) * 0.5,
        DOTS_W,
        DOTS_W,
    );
    let text_x = control.x + CONTROL_W + CONTROL_GAP;
    assert_eq!(parts.control, control);
    assert_eq!(parts.dots, dots);
    assert_eq!(
        parts.text,
        Rect::new(text_x, r.y, dots.x - CHIP_GAP - text_x, r.h)
    );
    let side = koru_draw::theme::CHECKBOX_SIZE;
    assert_eq!(parts.checkbox.w, side, "the document's box, not the target");
    assert_eq!(
        parts.checkbox.centre(),
        parts.control.centre(),
        "centred in the target"
    );

    let widths = [40.0, 56.0];
    let chips = super::row::chip_rects(r, &widths);
    let gap = koru_draw::chip::CHIP_GAP;
    let h = koru_draw::chip::CHIP_H;
    let first_x = dots.x - CHIP_GAP - (widths[0] + gap + widths[1]);
    let y = r.y + (r.h - h) * 0.5;
    assert_eq!(
        chips,
        [
            Rect::new(first_x, y, widths[0], h),
            Rect::new(first_x + widths[0] + gap, y, widths[1], h),
        ]
    );
}

/// **What the inspector shows for a row**: the column of rows' one row, and
/// in it the control, the icon, the title, the tags and the menu — named, in
/// that order, with nothing the frame's own validation objects to.
#[test]
fn a_task_row_is_an_element_tree_the_frame_accepts() {
    use super::row::{
        CONTENT, CONTROL, ICON, LABEL, MENU, ROW, RowFace, TAG, TAGS, TITLE, layout, tree,
    };
    use super::view::RowChip;
    use koru_ui::table::{Metrics, RowState, Table};
    let r = Rect::new(0.0, 0.0, 900.0, super::row::row_h());
    let chip = |kind, label: &str| RowChip {
        kind,
        label: label.to_owned(),
        accent: None,
        list: None,
    };
    let chips = [
        chip(koru_draw::chip::ChipKind::Tag, "Bike Project"),
        chip(koru_draw::chip::ChipKind::Due, "Aug 29"),
    ];
    let face = RowFace {
        completed: false,
        residue: false,
        glyph: Some(koru_draw::icons::DOCUMENT),
        emoji: None,
        title: "Order the groupset before the sale ends",
        context: "Bike Project",
        chips: &chips,
        chip_widths: &[80.0, 48.0],
        state: RowState::Hovered,
        dots: Some(false),
    };
    let tree = tree(&face, Table::new(r, Metrics::manager(), 0.0));
    let frame = crate::input_engine::with(|engine| layout(&tree, engine, r));
    assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
    let names: Vec<&str> = frame.nodes.iter().map(|node| node.name).collect();
    assert_eq!(
        names,
        [
            ROW, CONTROL, CONTENT, LABEL, ICON, TITLE, TAGS, TAG, TAG, MENU
        ]
    );
    assert_eq!(
        frame.rect_of(ROW).map(|row| row.w),
        Some(r.w),
        "the row is the page's full width"
    );
}

/// The capture row is the list's own first row, not a button beside it.
#[test]
fn the_capture_row_is_the_top_row_of_the_list() {
    let view = view_on(RailTarget::Bucket(Bucket::Today), vec![entry("Only", None)]);
    let r = view.line_rect_for_test(0);
    assert_eq!(
        view.hit_for_test(r.x + r.w * 0.5, r.y + r.h * 0.5),
        Some(Hit::Capture)
    );
}

/// **Only the visible band is laid out or drawn.** The table's cull is what
/// makes a thousand-row bucket cost the same as a ten-row one, and a hit test
/// built on top of it cannot resolve a row nobody can see.
#[test]
fn only_the_visible_band_is_laid_out() {
    let rows: Vec<TaskEntry> = (0..400)
        .map(|i| entry(&format!("Task {i}"), Some("2026-08-30")))
        .collect();
    let view = view_on(RailTarget::Bucket(Bucket::Today), rows);
    let visible = view.visible_for_test();
    assert!(
        visible.len() < 40,
        "a 700px viewport shows a couple of dozen rows, not {} of 401",
        visible.len()
    );
    // Off the bottom of the body, and therefore not a row.
    let last = view.line_rect_for_test(400);
    assert_eq!(
        view.hit_for_test(last.x + 100.0, last.y + last.h * 0.5),
        None,
        "a row below the body is not clickable, because it is not drawn"
    );
}

/// **A row is one line on every page** — Today included, which used to carry
/// its context under the title and read as a different surface from the five
/// beside it (round nine).
#[test]
fn every_page_draws_the_same_one_line_row() {
    let rows = vec![entry("A", Some("2026-09-20"))];
    let today = view_on(RailTarget::Bucket(Bucket::Today), rows.clone());
    let upcoming = view_on(RailTarget::Bucket(Bucket::Upcoming), rows.clone());
    let list = view_on(RailTarget::List(koru_core::ids::ListId::new()), rows);
    assert_eq!(today.row_h_for_test(), upcoming.row_h_for_test());
    assert_eq!(today.row_h_for_test(), list.row_h_for_test());
    let r = today.line_rect_for_test(today.line_of_task_for_test(0).unwrap());
    let parts = today.parts_for_test(r);
    assert_eq!(
        parts.control.y + parts.control.h * 0.5,
        r.y + r.h * 0.5,
        "the control is centred on the one line"
    );
}

// --------------------------------------------------------------- the groups

/// Rule labels are Upcoming's alone, and the fixture spans **both** registers:
/// two rows in next week under one `Next week`, then two rows in *different
/// weeks* of the same month under one `Later in September`.
///
/// That second half is the whole of V-3's extension. Grouping by week and
/// labelling by month printed the month's name once per week — the same heading
/// twice, over rows nothing distinguished.
#[test]
fn upcoming_groups_by_week_then_by_month() {
    let rows = vec![
        entry("Next week", Some("2026-09-07")),
        entry("Also next week", Some("2026-09-09")),
        entry("Two weeks out", Some("2026-09-18")),
        entry("Three weeks out", Some("2026-09-22")),
        entry("October", Some("2026-10-06")),
    ];
    let grouped = view_on(RailTarget::Bucket(Bucket::Upcoming), rows.clone());
    assert_eq!(
        grouped.rules_for_test(),
        vec![
            "Next week".to_owned(),
            "Later in September".to_owned(),
            "Later in October".to_owned(),
        ],
        "one rule per week near, one per month far — Sep 18 and Sep 22 are two \
         weeks apart and share a heading"
    );

    let flat = view_on(RailTarget::Bucket(Bucket::Today), rows);
    assert!(flat.rules_for_test().is_empty(), "Today has no weeks in it");
}

#[test]
fn a_rule_reads_relatively_near_and_by_month_far() {
    let monday = NaiveDate::from_ymd_opt(2026, 8, 31).expect("a Monday");
    let label = |d: NaiveDate| Rule::of(d, today()).label(today());
    assert_eq!(label(monday), "This week");
    assert_eq!(label(monday + chrono::Duration::days(7)), "Next week");
    assert_eq!(
        label(monday + chrono::Duration::days(28)),
        "Later in September"
    );
    assert_eq!(
        Rule::of(monday, today()),
        Rule::of(monday + chrono::Duration::days(6), today()),
        "a week is one group whichever day of it a row falls on"
    );
    // The year is named only when it is not this one, which is the only way
    // "Later in January" could be read two ways.
    assert_eq!(Rule::Month(2027, 1).label(today()), "Later in January 2027");
}

/// **A group is only a group while the rows are sorted on what it groups by.**
/// Sorted by priority, the due dates come down the page in no order, so a rule
/// would be printed again every time the list wandered back into a week.
#[test]
fn rule_labels_belong_to_the_date_order() {
    let rows = vec![
        entry("Next week", Some("2026-09-07")),
        entry("Two weeks out", Some("2026-09-18")),
    ];
    let mut view = view_on(RailTarget::Bucket(Bucket::Upcoming), rows.clone());
    assert_eq!(view.rules_for_test().len(), 2);

    view.set_filters(Filters {
        sort: TaskSort::Priority,
        ..view.filters()
    });
    view.seed_for_test(rows, today());
    assert!(
        view.rules_for_test().is_empty(),
        "no date order, no date headings"
    );
}

// -------------------------------------------------------------- the capture

/// **The date word is parsed inline and previewed before it is committed.**
#[test]
fn a_trailing_date_word_becomes_a_chip_before_commit() {
    let (title, parsed) =
        split_date("Chase the sign-off tomorrow", today()).expect("tomorrow is a date word");
    assert_eq!(title, "Chase the sign-off");
    assert_eq!(
        parsed,
        koru_editor::utils::when::Parsed::Date(koru_core::time::DateOnly::new(
            NaiveDate::from_ymd_opt(2026, 9, 1).expect("a real day")
        ))
    );
}

/// The **longest** trailing phrase wins, so a three-word date does not leave
/// its first two words in the title.
#[test]
fn the_longest_trailing_phrase_wins() {
    let (title, parsed) = split_date("Ship the beta in 3 days", today()).expect("a date phrase");
    assert_eq!(title, "Ship the beta");
    assert_eq!(
        parsed,
        koru_editor::utils::when::Parsed::Date(koru_core::time::DateOnly::new(
            NaiveDate::from_ymd_opt(2026, 9, 3).expect("a real day")
        ))
    );
}

/// …and *longest that parses*, not longest that looks like one. `next tuesday`
/// is in the grammar now (the owner's 2026-11-05 ask: *next* over any weekday
/// is a rule of the parser, not a list of phrases), so the whole phrase
/// leaves the title; a word the grammar does not know stays in it.
#[test]
fn only_a_phrase_the_grammar_knows_leaves_the_title() {
    let (title, _) = split_date("Ship the beta next tuesday", today()).expect("a date phrase");
    assert_eq!(title, "Ship the beta");
    let (title, _) = split_date("Ship the beta soonish tuesday", today()).expect("a date word");
    assert_eq!(title, "Ship the beta soonish");
}

/// A phrase that would leave no title at all is refused: somebody typing only
/// `tomorrow` means a task called *tomorrow*.
#[test]
fn a_date_word_alone_is_a_title() {
    assert_eq!(split_date("tomorrow", today()), None);
}

/// Enter creates a native task, and the title it creates is the one the preview
/// chip had been promising.
#[test]
fn enter_captures_the_title_without_its_date_word() {
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), Vec::new());
    let r = view.line_rect_for_test(0);
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    koru_workspace_host::View::mouse_down(
        &mut view,
        &mut doc,
        r.x + r.w * 0.5,
        r.y + r.h * 0.5,
        false,
    );
    for ch in "Chase it tomorrow".chars() {
        let text = ch.to_string();
        koru_workspace_host::View::key(
            &mut view,
            &mut doc,
            &Key::Character(text.clone().into()),
            Some(&text),
            ModifiersState::empty(),
        );
    }
    koru_workspace_host::View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    );
    let Some(Request::Capture {
        title, due, list, ..
    }) = view.take_request()
    else {
        panic!("Enter on the capture row asks for a task");
    };
    assert_eq!(title, "Chase it");
    assert!(due.is_some(), "the parsed word became the due date");
    assert_eq!(list, None, "a task captured in a bucket belongs to no list");
}

/// **A task typed into a list carries that list** (SPEC §8) — membership is a
/// filter set on the task, never a second place it lives.
#[test]
fn a_task_typed_into_a_list_carries_it() {
    let id = koru_core::ids::ListId::new();
    let mut view = view_on(RailTarget::List(id), Vec::new());
    let r = view.line_rect_for_test(0);
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    koru_workspace_host::View::mouse_down(
        &mut view,
        &mut doc,
        r.x + r.w * 0.5,
        r.y + r.h * 0.5,
        false,
    );
    for ch in "Groupset".chars() {
        let text = ch.to_string();
        koru_workspace_host::View::key(
            &mut view,
            &mut doc,
            &Key::Character(text.clone().into()),
            Some(&text),
            ModifiersState::empty(),
        );
    }
    koru_workspace_host::View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    );
    assert_eq!(
        view.take_request(),
        Some(Request::Capture {
            title: "Groupset".to_owned(),
            due: None,
            list: Some(id),
            priority: None,
            // A list page is ordered by hand unless somebody chose a sort, and
            // a typed task goes on top of it.
            rank: Some(0),
        })
    );
}

/// A refused capture gives the typing back. Losing what somebody typed is the
/// one unrecoverable bug.
#[test]
fn a_refused_capture_puts_the_text_back() {
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), Vec::new());
    view.restore_capture("Chase it tomorrow");
    assert_eq!(
        view.capture_preview_for_test().as_deref(),
        Some("Chase it"),
        "the date word is re-parsed and previews again, exactly as before Enter"
    );
}

// ----------------------------------------------------------------- the rows

/// **A whole-row click selects the row and asks for nothing else.**
///
/// The design's one sentence about this surface was *a whole-row click opens
/// the node*, and it is why the page has no inline affordances. Using it broke
/// the sentence: a list every row of which is a trapdoor cannot be skimmed, and
/// a click aimed near a row's edge threw the page away and replaced it with a
/// document. Leaving is now asked for by name — *Go to origin*, on the row's
/// own menu, and Enter on the keyboard.
#[test]
fn a_whole_row_click_selects_and_asks_for_nothing() {
    let row = entry("Select me", None);
    let (document, node) = (row.document, row.node);
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), vec![row]);
    let line = view.line_of_task_for_test(0).expect("a line");
    let r = view.line_rect_for_test(line);
    let text = view.parts_for_test(r).text;
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    koru_workspace_host::View::mouse_down(
        &mut view,
        &mut doc,
        text.x + text.w * 0.5,
        text.y + text.h * 0.5,
        false,
    );
    assert_eq!(
        view.take_request(),
        None,
        "the click navigates nowhere: the page it was made on is still the page"
    );
    assert_eq!(
        view.selected_for_test(),
        Some(0),
        "what it does instead — which is what the keyboard and the menu act on"
    );

    // And Enter is the *act* half of select-then-act, so the gesture the click
    // gave up is still one keypress away.
    koru_workspace_host::View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    );
    assert_eq!(
        view.take_request(),
        Some(Request::Open {
            document,
            node,
            new_tab: false,
        })
    );
}

/// **A double click renames, and goes nowhere** (the owner's ask,
/// 2026-11-05): a page-made task lives in the hidden tasks document, and a
/// double click that opened it showed storage rather than the task. Opening
/// is the row menu's *Open document*, and Enter.
#[test]
fn a_double_click_renames_rather_than_opening() {
    let row = entry("Open me", None);
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), vec![row]);
    let line = view.line_of_task_for_test(0).expect("a line");
    let r = view.line_rect_for_test(line);
    let text = view.parts_for_test(r).text;
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    let (x, y) = (text.x + text.w * 0.5, text.y + text.h * 0.5);
    koru_workspace_host::View::mouse_down(&mut view, &mut doc, x, y, false);
    assert_eq!(
        view.take_request(),
        None,
        "a single click navigates nowhere"
    );
    assert!(koru_workspace_host::View::mouse_double_click(
        &mut view, &doc, x, y
    ));
    assert_eq!(view.take_request(), None, "nor does the double click");
    assert!(view.rename.is_some(), "it renames");
}

/// Where a row's text column is pressed, the row's own open target.
fn row_text_centre(view: &TasksView, row: usize) -> (f32, f32) {
    let line = view.line_of_task_for_test(row).expect("a line");
    let text = view.parts_for_test(view.line_rect_for_test(line)).text;
    (text.x + text.w * 0.5, text.y + text.h * 0.5)
}

/// **Alt alone on a row asks where the task opens** (ADR-58): the place
/// chooser's request, at the press, naming what the row's open names. The
/// row is selected as any press selects it; nothing opens here.
#[test]
fn an_alt_click_on_a_row_asks_where_it_opens() {
    let row = entry("Ask me", None);
    let (document, node) = (row.document, row.node);
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), vec![row]);
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    let (x, y) = row_text_centre(&view, 0);

    view.set_mods(ModifiersState::ALT);
    assert!(koru_workspace_host::View::mouse_down(
        &mut view, &mut doc, x, y, false
    ));

    assert_eq!(
        view.take_request(),
        Some(Request::ChoosePlace {
            document,
            node,
            at: (x, y),
        })
    );
    assert_eq!(view.selected_for_test(), Some(0), "the row it is about");
}

/// **Alt with Ctrl or Shift is that key's press** — on this page a plain
/// select — and never the chooser, as in a document.
#[test]
fn alt_with_ctrl_or_shift_on_a_row_does_not_ask() {
    for (mods, ctrl) in [
        (ModifiersState::ALT | ModifiersState::CONTROL, true),
        (ModifiersState::ALT | ModifiersState::SHIFT, false),
    ] {
        let mut view = view_on(RailTarget::Bucket(Bucket::Today), vec![entry("x", None)]);
        let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
            .expect("an in-memory document");
        let (x, y) = row_text_centre(&view, 0);
        view.set_mods(mods);
        koru_workspace_host::View::mouse_down(&mut view, &mut doc, x, y, ctrl);
        assert_eq!(view.take_request(), None, "{mods:?}: nothing asked");
        assert_eq!(view.selected_for_test(), Some(0), "{mods:?}: selected");
    }
}

/// **A row's own controls keep their meaning under Alt**: the tick still
/// ticks and the `…` still opens the row's menu. Only the row asks.
#[test]
fn alt_on_a_rows_controls_keeps_their_meaning() {
    let row = entry("Tick me", None);
    let (document, node) = (row.document, row.node);
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), vec![row]);
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    let line = view.line_of_task_for_test(0).expect("a line");
    let parts = view.parts_for_test(view.line_rect_for_test(line));
    view.set_mods(ModifiersState::ALT);

    let control = parts.control;
    koru_workspace_host::View::mouse_down(
        &mut view,
        &mut doc,
        control.x + control.w * 0.5,
        control.y + control.h * 0.5,
        false,
    );
    assert_eq!(
        view.take_request(),
        Some(Request::Complete {
            document,
            node,
            completed: true,
        }),
        "the tick"
    );

    // A fresh page: the tick's echo has already moved this one.
    let mut view = view_on(
        RailTarget::Bucket(Bucket::Today),
        vec![entry("Menu me", None)],
    );
    view.set_mods(ModifiersState::ALT);
    let dots = parts.dots;
    koru_workspace_host::View::mouse_down(
        &mut view,
        &mut doc,
        dots.x + dots.w * 0.5,
        dots.y + dots.h * 0.5,
        false,
    );
    assert!(
        matches!(view.take_request(), Some(Request::RowMenu { .. })),
        "the row's menu"
    );
}

/// A double click on the heading or empty space is not a row's gesture and
/// asks for nothing — the hit test is the gate, exactly as it is for the
/// single click.
#[test]
fn a_double_click_off_a_row_asks_for_nothing() {
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), vec![entry("x", None)]);
    let chip = view.heading_area();
    let doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    assert!(!koru_workspace_host::View::mouse_double_click(
        &mut view,
        &doc,
        chip.x + chip.w * 0.5,
        chip.y + chip.h * 0.5,
    ));
    assert_eq!(view.take_request(), None);
}

/// The complete control asks for the canonical command rather than opening
/// anything — it outranks the row it sits in, the way a tab's close box
/// outranks the tab.
#[test]
fn the_complete_control_outranks_the_row() {
    let row = entry("Tick me", None);
    let (document, node) = (row.document, row.node);
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), vec![row]);
    let line = view.line_of_task_for_test(0).expect("a line");
    let control = view.parts_for_test(view.line_rect_for_test(line)).control;
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    koru_workspace_host::View::mouse_down(
        &mut view,
        &mut doc,
        control.x + control.w * 0.5,
        control.y + control.h * 0.5,
        false,
    );
    assert_eq!(
        view.take_request(),
        Some(Request::Complete {
            document,
            node,
            completed: true,
        })
    );
}

/// **A background Tasks tab costs a bool.** `adopt` only marks; nothing is
/// re-read until `catch_up` runs for the tab about to be drawn.
#[test]
fn a_background_tab_costs_a_bool() {
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), vec![entry("A", None)]);
    let doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    // Settled by `seed_for_test`, so the first catch_up has nothing to do.
    assert!(!view.catch_up(&doc), "an unchanged tab does no work");
    koru_workspace_host::View::adopt(&mut view, &doc);
    assert!(view.catch_up(&doc), "…and a marked one does it once");
    assert!(!view.catch_up(&doc), "…and only once");
}

/// A hidden tab makes no request at all: the panel's deadline is what the event
/// loop wakes on, and there is none while nobody is looking.
#[test]
fn a_hidden_tab_has_no_deadline() {
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), Vec::new());
    let now = std::time::Instant::now();
    let workspace = Some(WorkspaceId::new());
    let target = Some(RailTarget::Bucket(Bucket::Today));
    view.panel_mut().maintain(workspace, target, false, now);
    assert_eq!(view.panel().deadline(), None);
    view.panel_mut().maintain(workspace, target, true, now);
    assert!(view.panel().deadline().is_some());
}

/// A surface built under test dials nothing: with no client, a due refresh
/// spawns no thread and answers nothing.
#[test]
fn a_surface_built_under_test_dials_nothing() {
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), Vec::new());
    let now = std::time::Instant::now();
    let target = Some(view.target());
    for _ in 0..3 {
        view.panel_mut()
            .maintain(Some(WorkspaceId::new()), target, true, now);
    }
    assert!(!view.panel().known(), "no client, no answer, no traffic");
}

// -------------------------------------------------------------- the reading

/// The wire row's fields become the row's chips, and a completed task is never
/// overdue whatever its due says.
#[test]
fn a_row_reads_its_attributes() {
    let row = TaskEntry::of(
        &{
            let mut row = wire_row(
                "\u{1F680} Ship it",
                vec![
                    attr(koru_core::attributes::KEY_DUE, "date", "2026-08-29"),
                    attr(koru_core::attributes::KEY_PRIORITY, "select", "urgent"),
                ],
            );
            row.node_kind = "image".to_owned();
            row
        },
        today(),
        now_ms(),
    )
    .expect("a parseable row");
    assert_eq!(
        row.title, "Ship it",
        "the leading emoji is peeled into the icon"
    );
    assert_eq!(row.icon.as_deref(), Some("\u{1F680}"));
    assert_eq!(row.kind, RowKind::Image);
    assert_eq!(row.priority.as_deref(), Some("urgent"));
    assert!(row.overdue, "Aug 29 is past on Aug 31");

    let mut wire = wire_row(
        "Done",
        vec![attr(koru_core::attributes::KEY_DUE, "date", "2026-08-01")],
    );
    wire.lifecycle = "completed".to_owned();
    let done = TaskEntry::of(&wire, today(), now_ms()).expect("a parseable row");
    assert!(done.completed);
    assert!(
        !done.overdue,
        "a completed task is not overdue whatever its due says"
    );
}

/// A node id the answer cannot spell is a refusal, not a row with a broken
/// target — `search::pane_request`'s rule on the same shape of input.
#[test]
fn an_unparseable_node_is_refused_rather_than_drawn() {
    let mut wire = wire_row("Broken", Vec::new());
    wire.node = "not-a-uuid".to_owned();
    assert!(TaskEntry::of(&wire, today(), now_ms()).is_none());
}

// ---------------------------------------------------------------- the query

/// **The counts come from one grouped answer**, not from folding a page.
#[test]
fn counts_come_from_one_grouped_answer() {
    let workspace = koru_store_core::ids::WorkspaceId::new();
    let group = |key: &str, count: usize, overdue: usize| TaskGroup {
        workspace,
        list: key.to_owned(),
        count,
        overdue,
        trashed: 0,
    };
    let answered = TaskQueryResponse {
        rows: Vec::new(),
        definitions: Vec::new(),
        tags: Vec::new(),
        groups: vec![
            group("today", 5, 3),
            group("next", 12, 0),
            group("upcoming", 40, 0),
            group("later", 9, 0),
            group("completed", 122, 0),
        ],
        projected: true,
        updating_workspaces: Vec::new(),
        limit: 1,
        next_after: None,
        restarted: false,
        covered: None,
    };
    let counts = crate::mainview::taskcounts::counts_of(&answered);
    assert_eq!((counts.today, counts.today_overdue), (5, 3));
    assert_eq!(counts.next, 12);
    assert_eq!(counts.upcoming, 40);
    assert_eq!(counts.later, 9);
    assert_eq!(counts.completed, 122);
    assert!(counts.known);
    assert!(
        !counts.capped,
        "a corpus-wide count is not a page, so it is never a floor"
    );
}

/// A bucket nothing matches has **no row** rather than a zero, and the rail
/// draws all five either way.
#[test]
fn a_missing_bucket_group_counts_zero() {
    let answered = TaskQueryResponse {
        rows: Vec::new(),
        definitions: Vec::new(),
        tags: Vec::new(),
        groups: Vec::new(),
        projected: true,
        updating_workspaces: Vec::new(),
        limit: 1,
        next_after: None,
        restarted: false,
        covered: None,
    };
    let counts = crate::mainview::taskcounts::counts_of(&answered);
    assert!(counts.known, "an answer with no groups is still an answer");
    assert_eq!(
        (counts.today, counts.next, counts.upcoming, counts.later),
        (0, 0, 0, 0)
    );
}

/// The page asks the **store** for its bucket, and hands it the reader's own
/// edges — never a bucket folded here.
#[test]
fn the_page_asks_the_store_for_its_bucket() {
    let workspace = koru_store_core::ids::WorkspaceId::new();
    let windows = query::windows(today(), now_ms());
    let plain = Filters::default();
    let q = query::page_query(
        workspace,
        RailTarget::Bucket(Bucket::Upcoming),
        &windows,
        plain,
        today(),
    );
    assert_eq!(q.bucket.as_deref(), Some("upcoming"));
    assert!(q.bucket_windows.is_some(), "a bucket needs its windows");
    assert_eq!(q.list, None);

    let id = koru_core::ids::ListId::new();
    let q = query::page_query(workspace, RailTarget::List(id), &windows, plain, today());
    assert_eq!(q.list.as_deref(), Some(id.to_storage()).as_deref());
    assert_eq!(q.bucket, None, "a list is a filter, not a bucket");
}

/// The six edges run in order, which is the one thing the store checks before
/// it will use them.
#[test]
fn the_windows_run_today_then_horizon_then_upcoming() {
    let w = query::windows(today(), now_ms());
    assert_eq!(w.today, "2026-08-31");
    assert_eq!(w.horizon, "2026-09-07");
    assert_eq!(w.upcoming_horizon, "2026-09-30");
    assert!(w.today_ends_ms <= w.horizon_ends_ms);
    assert!(w.horizon_ends_ms <= w.upcoming_ends_ms);
    assert_eq!(w.min_next_priority, query::MIN_NEXT_PRIORITY);
    assert!(
        w.now_ms < w.today_ends_ms,
        "now is inside today, or the overdue split would be the whole bucket"
    );
}

// ------------------------------------------------------------- the identity

/// A row typed straight into the Tasks view names no page — and says nothing
/// about it: the absent page glyph is the whole of the reading (round nine).
#[test]
fn a_native_task_names_no_document_and_says_nothing() {
    let row = entry("Order the groupset", None);
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), vec![row.clone()]);
    assert_eq!(view.context_of(&row), "Test / More Documents");
    view.set_task_documents(vec![row.document]);
    assert_eq!(view.context_of(&row), "");
}

/// A membership id the list table has forgotten draws no chip — a stale value
/// is not a missing task, which is what makes deleting a list cost one nothing.
#[test]
fn a_forgotten_list_draws_no_chip() {
    let view = view_on(RailTarget::Bucket(Bucket::Today), Vec::new());
    assert!(view.list_face(koru_core::ids::ListId::new()).is_none());
}

/// The tab and the rail hold one answer: pointing the tab elsewhere is what the
/// container mirrors into `set_selected`.
#[test]
fn the_target_round_trips() {
    let mut view = TasksView::new(RailTarget::Bucket(Bucket::Today));
    assert_eq!(view.target(), RailTarget::Bucket(Bucket::Today));
    assert!(view.set_target(RailTarget::Bucket(Bucket::Later)));
    assert_eq!(view.target(), RailTarget::Bucket(Bucket::Later));
    assert!(
        !view.set_target(RailTarget::Bucket(Bucket::Later)),
        "an unchanged push costs no frame"
    );
}

/// The row menu is the rows the design draws, with the destructive one last
/// under a hairline.
///
/// **`List…` is the one row that is not on `1c`**, and it is here for the
/// reason that mock's own note gives: *the menu is where everything else
/// lives*. Membership had no control at all when that close-up was drawn; the
/// alternative was hanging a picker off the chip, which the same note refuses
/// in as many words (*no inline edit affordances on the row itself*).
#[test]
fn the_row_menu_is_the_rows_as_drawn() {
    use crate::mainview::context_menu::MenuAction;
    let (subject, items) =
        super::row_menu(DocumentId::new(), NodeId::new(), false, false, None, false);
    assert!(matches!(
        subject,
        crate::mainview::context_menu::MenuSubject::Task { .. }
    ));
    assert_eq!(
        items,
        vec![
            MenuAction::TaskGoToOrigin,
            MenuAction::Divider,
            MenuAction::TaskComplete,
            MenuAction::TaskPinToday,
            MenuAction::TaskDue,
            MenuAction::TaskPriority,
            MenuAction::TaskList,
            MenuAction::TaskDelegate,
            MenuAction::Divider,
            MenuAction::TaskRemove,
        ]
    );
    assert!(
        MenuAction::TaskRemove.destructive(),
        "spelled red, as the document view's copy of the same row is"
    );
    let (_, done) = super::row_menu(DocumentId::new(), NodeId::new(), true, false, None, false);
    assert!(
        done.contains(&MenuAction::TaskReopen),
        "a completed row offers the other direction"
    );
    // **The pin reads the row's state, like Complete/Reopen above it**, and the
    // menu is frozen at open time so it cannot relabel itself under the pointer.
    let (_, pinned) = super::row_menu(DocumentId::new(), NodeId::new(), false, false, None, true);
    assert!(pinned.contains(&MenuAction::TaskUnpinToday));
    assert!(!pinned.contains(&MenuAction::TaskPinToday));
    // A task typed into this page lives in the hidden tasks document and has
    // no origin worth a row: the menu starts at the verbs.
    let (_, native) = super::row_menu(DocumentId::new(), NodeId::new(), false, true, None, false);
    assert!(!native.contains(&MenuAction::TaskGoToOrigin));
    assert_eq!(
        &native[..2],
        &[MenuAction::TaskCreateDocument, MenuAction::Divider],
        "and is offered a document instead"
    );
    // A row in a list leads there too — beside the origin, or alone for a
    // native row, and the rule follows the group either way.
    let list = Some(koru_core::ids::ListId::new());
    let (_, listed) = super::row_menu(DocumentId::new(), NodeId::new(), false, false, list, false);
    assert_eq!(
        &listed[..3],
        &[
            MenuAction::TaskGoToOrigin,
            MenuAction::TaskGoToList,
            MenuAction::Divider
        ]
    );
    let (_, native_listed) =
        super::row_menu(DocumentId::new(), NodeId::new(), false, true, list, false);
    assert_eq!(
        &native_listed[..3],
        &[
            MenuAction::TaskCreateDocument,
            MenuAction::TaskGoToList,
            MenuAction::Divider
        ]
    );
}

// ------------------------------------------------------------- the filters

/// The query this view would send, built from the same two functions the worker
/// thread uses — so what a chip changes is asserted on the wire body rather than
/// on the state behind it.
fn query_of(view: &TasksView) -> koru_store_wire::tasks::TaskQuery {
    let windows = query::windows(today(), now_ms());
    query::page_query(
        koru_store_core::ids::WorkspaceId::new(),
        view.target(),
        &windows,
        view.filters(),
        today(),
    )
}

fn press_at(view: &mut TasksView, at: Rect) {
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    koru_workspace_host::View::mouse_down(
        view,
        &mut doc,
        at.x + at.w * 0.5,
        at.y + at.h * 0.5,
        false,
    );
}

fn press_dots(view: &mut TasksView) {
    let r = view.header_dots_for_test();
    press_at(view, r);
}

fn press_overflow_row(view: &mut TasksView, row: usize) {
    let r = view
        .overflow_row_rect_for_test(row)
        .expect("an open menu has rectangles");
    press_at(view, r);
}

/// **A filter changes the query and only the query.** Every other field of the
/// wire body is asserted unmoved, so a filter that also changed the bucket, the
/// list or the page size fails here rather than in somebody's window. No page
/// draws the filter row now; the query still has the dimension.
#[test]
fn a_filter_changes_the_query_and_nothing_else() {
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), Vec::new());
    let plain = query_of(&view);
    assert_eq!(plain.min_priority, None);
    assert!(plain.due.is_none());

    let high = query::Filters {
        min_high: true,
        ..view.filters()
    };
    view.set_filters(high);
    let q = query_of(&view);
    assert_eq!(
        q.min_priority.as_deref(),
        Some("high"),
        "the floor is a `min_priority`, not a sift over the answer"
    );
    assert_eq!(q.bucket, plain.bucket, "the page is still Today");
    assert_eq!(q.list, plain.list);
    assert_eq!(q.limit, plain.limit);
    assert_eq!(q.lifecycle, plain.lifecycle);
    assert!(!q.include_trashed);
    assert_eq!(q.sort, plain.sort);
    assert!(q.due.is_none(), "priority is not a due window");
    assert_eq!(
        view.panel().filters(),
        view.filters(),
        "the panel asks with the page's filters — one state, pushed down, so \
         the page in flight cannot be answered under the old filter"
    );
}

/// **The heading is one element row** — the list's swatch, then the title,
/// the date and the count — where it was a title layout and a second layout of
/// `· Nov 5   5 open` placed after it by hand. The notes sit on the title's
/// baseline: their line boxes end where the title's does.
#[test]
fn the_heading_is_one_row_of_title_date_and_count() {
    use super::heading::{DATE, HEADING, HEADING_TEXT, SWATCH, SWATCH_STEP, TITLE};
    use koru_draw::theme::{Profile, roles};
    assert_eq!(super::draw::TITLE, roles::SUBTITLE.style());
    assert_eq!(SWATCH_STEP.px(Profile::Desktop), super::view::SWATCH_GAP);

    let today = view_on(RailTarget::Bucket(Bucket::Today), vec![entry("One", None)]);
    let (_, frame) = crate::input_engine::with(|engine| today.heading_frame(engine));
    assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
    // No count before the store has answered: a `0` meaning *not looked yet*
    // is a lie a reader cannot see through.
    let names: Vec<&str> = frame.nodes.iter().map(|node| node.name).collect();
    assert_eq!(names, [HEADING, HEADING_TEXT, TITLE, DATE]);
    let title = frame.rect_of(TITLE).expect("a title");
    let date = frame.rect_of(DATE).expect("Today's date");
    assert_eq!(title.x, today.title_x_for_test());
    assert!(date.x > title.x + title.w, "the date follows the title");
    // **One baseline**: where each line is drawn (its box, less the push
    // down the note was given) plus where its own shaped line puts the
    // baseline. Read from the frame, so a change to either side shows.
    let heading = today.heading();
    let date_text = format!(
        "{}{}",
        super::heading::DATE_DOT,
        heading.date.as_deref().expect("Today has a date")
    );
    assert_eq!(
        date.h,
        roles::SUBTITLE.style().metrics.line_height,
        "the date is the title's size"
    );
    let (title_base, date_base) = crate::input_engine::with(|engine| {
        (
            title.y + super::heading::baseline(engine, &heading.title, roles::SUBTITLE),
            date.y + super::heading::baseline(engine, &date_text, roles::SUBTITLE),
        )
    });
    assert!(
        (title_base - date_base).abs() < 0.01,
        "the date sits on the title's baseline: {title_base} vs {date_base}"
    );

    let (id, face) = list_face("Wedding", Some("pink"));
    let mut list = view_on(RailTarget::List(id), Vec::new());
    list.set_lists(vec![face]);
    let (_, frame) = crate::input_engine::with(|engine| list.heading_frame(engine));
    let names: Vec<&str> = frame.nodes.iter().map(|node| node.name).collect();
    assert_eq!(names, [HEADING, SWATCH, HEADING_TEXT, TITLE]);
    assert_eq!(
        frame.rect_of(TITLE).map(|r| r.x),
        Some(list.title_x_for_test()),
        "past the swatch, where the title always started"
    );
}

/// **The due windows ask the store for the rail's own edges.** No page draws
/// the due selector while lists have no chip row, but the query still has the
/// dimension; these are the edges the rail's split count uses, and the two
/// must not drift.
#[test]
fn the_due_windows_ask_for_the_rails_own_edges() {
    let (id, face) = list_face("Wedding", Some("pink"));
    let mut view = view_on(RailTarget::List(id), Vec::new());
    view.set_lists(vec![face]);
    let at = |view: &mut TasksView, window| {
        let filters = query::Filters {
            window,
            ..view.filters()
        };
        view.set_filters(filters);
    };

    at(&mut view, query::DueWindow::Overdue);
    let overdue = query_of(&view).due.expect("a window");
    assert_eq!(
        overdue.dates.and_then(|d| d.before).as_deref(),
        Some("2026-08-31"),
        "overdue is *before today*, the same edge the rail's split count uses"
    );
    assert_eq!(
        overdue.instants.and_then(|i| i.before_ms),
        Some(now_ms()),
        "…and *before now* for a zoned value, each read in its own terms"
    );

    at(&mut view, query::DueWindow::ThisWeek);
    let dates = query_of(&view).due.expect("a window").dates.expect("dates");
    assert_eq!(dates.after.as_deref(), Some("2026-08-31"), "from today");
    assert_eq!(
        dates.before.as_deref(),
        Some("2026-09-07"),
        "through Sunday inclusive, spelled as the exclusive day after it"
    );

    at(&mut view, query::DueWindow::Any);
    assert!(query_of(&view).due.is_none(), "any date narrows nothing");
}

/// **A due window is a list's alone.** A list's window carried to a queue
/// would narrow it with nothing on screen saying so, and a queue is already a
/// due window: leaving a list clears it.
#[test]
fn leaving_a_list_clears_its_due_window() {
    let (id, face) = list_face("Wedding", Some("pink"));
    let mut view = view_on(RailTarget::List(id), Vec::new());
    view.set_lists(vec![face]);
    let filters = query::Filters {
        window: query::DueWindow::Overdue,
        ..view.filters()
    };
    view.set_filters(filters);
    assert!(query_of(&view).due.is_some(), "the list asks with it");
    view.set_target(RailTarget::Bucket(Bucket::Today));
    assert!(query_of(&view).due.is_none(), "the queue asks without it");
    assert_eq!(view.filters().window, query::DueWindow::Any);
}

/// **An active chip draws from the tokens that already exist.**
///
/// Off is the neutral attribute-chip pair; on is the app's one inverted surface,
/// which the selection bubble and the offline chip already wear. A new `Palette`
/// field for *a filter is on* is what this fails on.
#[test]
fn an_active_chip_inverts_the_existing_tokens() {
    // One snapshot, because the theme is a process global other suites flip.
    let p = koru_draw::theme::p();
    use koru_ui::label_chip::Tone;
    assert_eq!(
        Tone::Neutral.colors(&p),
        (p.chip_bg, p.text_dim),
        "off is the chip family's own neutral pair"
    );
    assert_eq!(
        Tone::Inverted.colors(&p),
        (p.bubble_bg, p.bubble_fg),
        "on is the one inverted surface, dark in both themes"
    );
}

// ------------------------------------------------------------ the overflow

/// **The overflow holds one switch and the sort.** A row added anywhere
/// breaks this test by name, which is the point: everything else is a chip
/// in the open.
///
/// Three *Show* rows lived here before it and all three were the same
/// question asked badly — two said *show completed* on a page that is
/// completed, and the third asked about a window whose far side the page
/// could not show. One switch, one meaning.
#[test]
fn the_overflow_holds_exactly_the_query_changers() {
    assert_eq!(
        OverflowRow::ROWS.to_vec(),
        vec![
            OverflowRow::RecentlyCompleted,
            OverflowRow::Divider,
            OverflowRow::SortHeading,
            OverflowRow::Sort(TaskSort::Due),
            OverflowRow::Sort(TaskSort::Priority),
            OverflowRow::Sort(TaskSort::Modified),
        ]
    );
    // A queue adds *Manual* last (2026-11-05); the archive keeps the six.
    let archive = overflow_items(&Filters::default(), RailTarget::Bucket(Bucket::Completed));
    assert_eq!(archive.len(), 6, "six rows; a menu is not a settings page");
    let items = overflow_items(&Filters::default(), RailTarget::Bucket(Bucket::Today));
    assert_eq!(items.len(), 7);
    assert_eq!(
        items
            .iter()
            .filter(|i| koru_ui::MenuItem::selectable(*i))
            .count(),
        5,
        "one toggle and four sorts; the heading and the rule are not pressable"
    );
    assert_eq!(
        items[3].checked,
        Some(true),
        "the sort in force wears the check"
    );
    assert_eq!(items[0].checked, Some(false), "…and the toggle is off");
}

/// **A setting menu does not close on choice.** The check has to be seen to
/// move, and two of these are usually changed together — which is the whole
/// difference between this and a block menu.
#[test]
fn a_toggle_does_not_close_the_menu() {
    let id = koru_core::ids::ListId::new();
    let mut view = view_on(RailTarget::List(id), Vec::new());
    press_dots(&mut view);
    let rows = view.overflow_for_test().expect("the menu opened");
    let recent = rows
        .iter()
        .position(|item| item.row == OverflowRow::RecentlyCompleted)
        .expect("a Show recently completed row");

    press_overflow_row(&mut view, recent);
    assert!(
        view.overflow_for_test().is_some(),
        "the panel is still open under the pointer"
    );
    assert!(
        query_of(&view).recently_completed_since.is_some(),
        "…and the row it holds changed the query"
    );
    assert_eq!(
        view.overflow_for_test().expect("still open")[recent].checked,
        Some(true),
        "…and the check moved with it, in place"
    );

    // A sort row is the same menu's business, and leaves the panel standing for
    // the next change.
    let modified = rows
        .iter()
        .position(|item| item.row == OverflowRow::Sort(TaskSort::Modified))
        .expect("a Modified row");
    press_overflow_row(&mut view, modified);
    assert_eq!(query_of(&view).sort, TaskSort::Modified);
    assert!(view.overflow_for_test().is_some());

    // Escape is what closes it.
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    koru_workspace_host::View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty(),
    );
    assert!(view.overflow_for_test().is_none());
}

/// **The one switch keeps what was just finished where it was finished** —
/// and it is dimmed on Completed, where every row is completed already.
///
/// The queue's own number is left alone on purpose: a queue counts open
/// work, so ticking something makes it go down, which is the reward for
/// ticking it. The switch is about the page, not the count.
#[test]
fn show_recently_completed_widens_the_page_and_not_the_count() {
    let mut queue = view_on(RailTarget::Bucket(Bucket::Today), Vec::new());
    assert_eq!(
        query_of(&queue).recently_completed_since,
        None,
        "off by default: a queue is the work in front of you"
    );
    press_dots(&mut queue);
    assert_eq!(
        queue.overflow_enabled_for_test().first(),
        Some(&true),
        "offered on every open queue"
    );
    press_overflow_row(&mut queue, 0);
    assert_eq!(
        query_of(&queue).recently_completed_since,
        Some(query::recently_completed_since(today())),
        "the day the wire crate names, from the page's own today"
    );

    // A list takes it too; a list is where work sits beside work.
    let id = koru_core::ids::ListId::new();
    let mut list = view_on(RailTarget::List(id), Vec::new());
    press_dots(&mut list);
    assert_eq!(list.overflow_enabled_for_test().first(), Some(&true));
    press_overflow_row(&mut list, 0);
    assert!(query_of(&list).recently_completed_since.is_some());

    // ...and Completed does not, because it would be asking a page to show
    // what it is made of.
    let mut done = view_on(RailTarget::Bucket(Bucket::Completed), Vec::new());
    press_dots(&mut done);
    assert_eq!(
        done.overflow_enabled_for_test().first(),
        Some(&false),
        "every row here is completed"
    );
    press_overflow_row(&mut done, 0);
    assert_eq!(
        query_of(&done).recently_completed_since,
        None,
        "a dimmed row changes nothing"
    );
    assert!(
        done.overflow_for_test()
            .and_then(|items| koru_ui::MenuItem::disabled_reason(&items[0]))
            .is_some(),
        "a dimmed row that does not explain itself is a dead end"
    );
}

// ----------------------------------------------------------- the freshness

/// **The chip comes off the page it describes, and the page stays readable.**
/// `updating_workspaces` rides on the answer, so the rows and the chip are the
/// same answer — and nothing is veiled, greyed or replaced by a spinner.
#[test]
fn the_updating_chip_rides_the_answer_the_rows_came_on() {
    let answered = |updating: Vec<koru_store_core::ids::WorkspaceId>| TaskQueryResponse {
        rows: vec![wire_row("Still readable", Vec::new())],
        definitions: Vec::new(),
        tags: Vec::new(),
        groups: Vec::new(),
        projected: true,
        updating_workspaces: updating,
        limit: 100,
        next_after: None,
        restarted: false,
        covered: None,
    };
    let mut view = TasksView::new(RailTarget::Bucket(Bucket::Today));
    view.set_viewport(1000.0, 700.0);
    view.panel_mut()
        .accept_for_test(&answered(Vec::new()), today(), now_ms());
    assert!(!view.panel().updating(), "nothing behind, nothing to say");

    view.panel_mut().accept_for_test(
        &answered(vec![koru_store_core::ids::WorkspaceId::new()]),
        today(),
        now_ms(),
    );
    assert!(view.panel().updating(), "a named workspace is the chip");
    assert_eq!(
        view.panel().rows().len(),
        1,
        "…and the rows are still there — the list stays readable beneath it"
    );

    let doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    koru_workspace_host::View::adopt(&mut view, &doc);
    view.catch_up(&doc);
    assert_eq!(
        view.line_of_task_for_test(0),
        Some(1),
        "…and it lays out under the capture row like any other page"
    );
}

/// A store still indexing says the same thing: `projected: false` is *this
/// answer may be behind*, which is what the chip means.
#[test]
fn an_unprojected_answer_is_also_updating() {
    let mut view = TasksView::new(RailTarget::Bucket(Bucket::Today));
    view.panel_mut().accept_for_test(
        &TaskQueryResponse {
            rows: Vec::new(),
            definitions: Vec::new(),
            tags: Vec::new(),
            groups: Vec::new(),
            projected: false,
            updating_workspaces: Vec::new(),
            limit: 100,
            next_after: None,
            restarted: false,
            covered: None,
        },
        today(),
        now_ms(),
    );
    assert!(view.panel().updating());
}

// ------------------------------------------------------- lists as buckets (V-4)
//
// "A list is another bucket, not a new surface" — so what these assert is
// mostly *sameness*: the same header band, the same chips, the same rows, and
// one more predicate on the query. What differs is written down where it
// differs, and each difference is a sentence out of `1d`.

fn list_face(name: &str, color: Option<&str>) -> (koru_core::ids::ListId, super::ListFace) {
    let id = koru_core::ids::ListId::new();
    (
        id,
        crate::tasks_view::ListFace {
            id,
            name: name.to_owned(),
            color: color.map(str::to_owned),
        },
    )
}

/// A wire row carrying a membership, so the id travels the way the store sends
/// it — an attribute value, never a name.
fn row_in(title: &str, list: koru_core::ids::ListId) -> TaskRow {
    wire_row(
        title,
        vec![attr(
            koru_core::attributes::KEY_LIST,
            "text",
            &list.to_storage(),
        )],
    )
}

fn one_answer(rows: Vec<TaskRow>) -> TaskQueryResponse {
    TaskQueryResponse {
        rows,
        definitions: Vec::new(),
        tags: Vec::new(),
        groups: Vec::new(),
        projected: true,
        updating_workspaces: Vec::new(),
        limit: 100,
        next_after: None,
        restarted: false,
        covered: None,
    }
}

/// A view on `target` holding exactly what one answer said, taken the way the
/// worker's answer is taken — so nothing in the path is a test fixture except
/// the bytes.
fn view_answered(target: RailTarget, rows: Vec<TaskRow>) -> TasksView {
    let mut view = TasksView::new(target);
    view.set_viewport(1000.0, 700.0);
    view.panel_mut()
        .accept_for_test(&one_answer(rows), today(), now_ms());
    let doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    koru_workspace_host::View::adopt(&mut view, &doc);
    view.catch_up(&doc);
    view
}

/// **The page is `where list = X`, and nothing is sifted after the answer.**
///
/// The list arm differs from a queue's by one predicate — `list` set, `bucket`
/// cleared — and everything else on the wire body is asserted unmoved. The
/// second half is the one that matters at runtime: the rows the answer carried
/// are the rows the page draws, *including* one whose membership names another
/// list, because a page that filtered its own answer would disagree with its own
/// header the moment the list was longer than the limit.
#[test]
fn a_list_page_is_the_query_and_nothing_is_sifted_here() {
    let workspace = koru_store_core::ids::WorkspaceId::new();
    let windows = query::windows(today(), now_ms());
    let (id, _) = list_face("Wedding", Some("pink"));
    let queue = query::page_query(
        workspace,
        RailTarget::Bucket(Bucket::Later),
        &windows,
        Filters::default(),
        today(),
    );
    let list = query::page_query(
        workspace,
        RailTarget::List(id),
        &windows,
        Filters::default(),
        today(),
    );
    assert_eq!(list.list.as_deref(), Some(id.to_storage()).as_deref());
    assert_eq!(list.bucket, None, "a list is a filter, not a bucket");
    assert_eq!(list.workspaces, queue.workspaces);
    assert_eq!(list.limit, queue.limit);
    // **The one difference in order**: a list opens in the order somebody
    // dragged it into, a queue by due.
    assert_eq!(list.sort, TaskSort::Manual);
    assert_eq!(queue.sort, TaskSort::Due);
    assert_eq!(list.min_priority, queue.min_priority);
    assert_eq!(list.include_trashed, queue.include_trashed);
    assert!(
        list.bucket_windows.is_some(),
        "the reader's edges travel too"
    );

    let (other, _) = list_face("Bike Project", Some("blue"));
    let view = view_answered(
        RailTarget::List(id),
        vec![
            row_in("Book the room", id),
            row_in("Order the groupset", other),
            wire_row("Loose end", Vec::new()),
        ],
    );
    assert_eq!(
        view.panel().rows().len(),
        3,
        "the store answered the page; this surface has no filter of its own"
    );
    assert_eq!(
        view.rules_for_test(),
        Vec::<String>::new(),
        "and a list is not grouped — rule labels are Upcoming's alone"
    );
}

/// **The header band is the same band, with the list's own square and count.**
///
/// The swatch is an accent *token*, so the square in the sidebar and the square
/// on the page are the one colour in either theme; the title starts past it, and
/// a queue has no square at all.
#[test]
fn a_list_page_wears_its_colour_square_and_its_open_count() {
    let (id, face) = list_face("Wedding", Some("pink"));
    let mut view = view_answered(
        RailTarget::List(id),
        vec![row_in("Book the room", id), row_in("Send the cards", id)],
    );
    view.set_lists(vec![face]);
    assert_eq!(view.title_for_test(), "Wedding");
    assert_eq!(
        view.title_swatch_for_test(),
        Some(Some("pink".to_owned())),
        "the rail's token, not a colour of this page's own"
    );
    assert!(
        view.title_x_for_test() > super::view::PAD,
        "the name starts past the square"
    );
    assert_eq!(
        view.count_label_for_test().as_deref(),
        Some("2 open"),
        "the same count the queues print"
    );

    let queue = view_answered(RailTarget::Bucket(Bucket::Later), Vec::new());
    assert_eq!(queue.title_swatch_for_test(), None, "a queue has no square");
    assert_eq!(queue.title_x_for_test(), super::view::PAD);
}

/// A list the table has forgotten still has a page: the name falls back to the
/// neutral word and the square draws neutral, rather than either printing an id.
#[test]
fn a_forgotten_list_still_has_a_readable_page() {
    let (id, _) = list_face("Wedding", Some("pink"));
    let view = view_answered(RailTarget::List(id), Vec::new());
    assert_eq!(view.title_for_test(), "List");
    assert_eq!(
        view.title_swatch_for_test(),
        Some(None),
        "a square with no token, which draws the neutral one"
    );
}

/// **No Tasks page draws a filter row, for now** (the owner, 2026-11-05): a
/// list is every open task in it and a queue is its bucket, at any priority
/// and any date. Every page's header ends at the heading's line.
#[test]
fn no_page_draws_a_filter_row_and_a_list_asks_for_every_open_task() {
    let (id, face) = list_face("Wedding", Some("pink"));
    let mut view = view_on(RailTarget::List(id), Vec::new());
    view.set_lists(vec![face]);
    let q = query_of(&view);
    assert_eq!(q.lifecycle, LifecycleFilter::Open, "every open task");
    assert!(q.due.is_none(), "at any date");
    assert!(q.min_priority.is_none(), "at any priority");

    let queue = view_on(RailTarget::Bucket(Bucket::Today), Vec::new());
    assert!(query_of(&queue).min_priority.is_none());
    assert_eq!(
        view.header_h_for_test(),
        queue.header_h_for_test(),
        "one header, ending at the heading's line, on every page"
    );
}

/// **Membership shows on the row as one more chip in the existing family**
/// (design note 9), and the *name* is resolved locally: the wire carries a
/// `ListId`, and the workspace's own table is what turns it into a word.
#[test]
fn the_membership_chip_reads_the_name_the_wire_never_sent() {
    let (id, face) = list_face("Bike Project", Some("blue"));
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![row_in("Order the groupset", id)],
    );
    assert_eq!(
        view.panel().rows()[0].list,
        Some(id),
        "the wire carried the id"
    );
    assert!(
        view.row_chips_for_test(0)
            .iter()
            .all(|chip| chip.label != "Bike Project"),
        "…and until the table arrives there is no name to draw"
    );

    view.set_lists(vec![face]);
    let chips = view.row_chips_for_test(0);
    let membership = chips
        .iter()
        .find(|chip| chip.label == "Bike Project")
        .expect("the membership chip");
    assert_eq!(
        membership.kind,
        koru_draw::chip::ChipKind::Tag,
        "the existing chip family, not a shape of its own"
    );
    assert_eq!(membership.accent.as_deref(), Some("blue"));
}

/// **A task can be in Today *and* in Bike Project without two vocabularies.**
///
/// The same row on the Today page wears the membership chip; a row typed into
/// the list says so in its context line instead of naming a document, and the
/// two are different lines rather than two claims about the same one.
#[test]
fn a_row_is_in_a_queue_and_in_a_list_at_once() {
    let (id, face) = list_face("Bike Project", Some("blue"));
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![row_in("Order the groupset", id)],
    );
    view.set_lists(vec![face]);
    let row = view.panel().rows()[0].clone();
    assert!(
        view.row_chips_for_test(0)
            .iter()
            .any(|chip| chip.label == "Bike Project")
    );
    assert_eq!(
        view.context_of(&row),
        "Test / More Documents",
        "an ordinary task names the page it was typed on"
    );
    view.set_task_documents(vec![row.document]);
    assert_eq!(
        view.context_of(&row),
        "",
        "…and one typed straight into the list says nothing: the chip is the list"
    );
}

/// **Deleting a list leaves its rows.** The membership stops resolving, so the
/// chip goes; the task is untouched and falls back to its document's pages,
/// which is SPEC §8's promise that a list costs a task nothing.
#[test]
fn deleting_a_list_leaves_its_rows() {
    let (id, face) = list_face("Reading", Some("green"));
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![row_in("Finish the chapter", id)],
    );
    view.set_lists(vec![face]);
    assert!(
        view.row_chips_for_test(0)
            .iter()
            .any(|chip| chip.label == "Reading")
    );

    view.set_lists(Vec::new());
    assert_eq!(view.panel().rows().len(), 1, "the task is still there");
    assert!(
        view.row_chips_for_test(0)
            .iter()
            .all(|chip| chip.label != "Reading"),
        "…wearing no chip for a list that is gone"
    );
    let row = view.panel().rows()[0].clone();
    assert_eq!(
        view.context_of(&row),
        "Test / More Documents",
        "and it still says where it lives"
    );
}

/// **A dateless row is normal here, and shows the dash.** `1d` says so about
/// list pages in as many words, and the chip is the editor's own dash rather
/// than an empty lane, so the row reads as *no date* rather than as a gap.
#[test]
fn a_dateless_row_wears_the_dash_rather_than_nothing() {
    let (id, _) = list_face("Wedding", None);
    let view = view_answered(
        RailTarget::List(id),
        vec![row_in("Decide the playlist", id)],
    );
    let chips = view.row_chips_for_test(0);
    assert!(
        chips
            .iter()
            .any(|chip| chip.kind == koru_draw::chip::ChipKind::Due
                && chip.label == koru_editor::chips::DUE_NONE),
        "an open row with no date wears the dash chip"
    );
}

// -------------------------------------------------------- the membership picker

fn open_picker(view: &mut TasksView) -> (DocumentId, NodeId) {
    let row = view.panel().rows()[0].clone();
    assert!(
        view.open_list_picker(row.document, row.node),
        "the row is on this page"
    );
    (row.document, row.node)
}

fn press_picker_row(view: &mut TasksView, i: usize) {
    let r = view
        .picker_row_rect_for_test(i)
        .expect("an open picker has rectangles");
    press_at(view, r);
}

/// **The picker offers every list, then Clear.** Its rows come from the same
/// table the chip reads, in the same order, and the list the task is already in
/// is the checked one.
#[test]
fn the_picker_offers_the_table_then_clear() {
    use super::menu::ListPick;
    let (bike, bike_face) = list_face("Bike Project", Some("blue"));
    let (wedding, wedding_face) = list_face("Wedding", Some("pink"));
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![row_in("Order the groupset", bike)],
    );
    view.set_lists(vec![bike_face, wedding_face]);
    open_picker(&mut view);
    let items = view.picker_for_test().expect("an open picker");
    assert_eq!(
        items,
        vec![
            ListPick::List {
                id: bike,
                name: "Bike Project".to_owned(),
                color: Some("blue".to_owned()),
                checked: true,
            },
            ListPick::List {
                id: wedding,
                name: "Wedding".to_owned(),
                color: Some("pink".to_owned()),
                checked: false,
            },
            ListPick::Divider,
            ListPick::Clear,
        ],
        "the table's order, the current membership checked, and Clear last"
    );
}

/// **Set and clear both leave as one request**, which the container turns into
/// the one task command — so membership is written into the task's own document
/// by the same door a tick goes through.
#[test]
fn the_picker_sets_and_clears_through_the_command_channel() {
    let (bike, bike_face) = list_face("Bike Project", Some("blue"));
    let (wedding, wedding_face) = list_face("Wedding", Some("pink"));
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![row_in("Order the groupset", bike)],
    );
    view.set_lists(vec![bike_face, wedding_face]);
    let (document, node) = open_picker(&mut view);

    press_picker_row(&mut view, 1);
    assert_eq!(
        view.take_request(),
        Some(Request::SetList {
            document,
            node,
            list: Some(wedding),
        })
    );
    assert!(
        view.picker_for_test().is_none(),
        "a membership is a choice, not a setting: the panel closes"
    );

    open_picker(&mut view);
    press_picker_row(&mut view, 3);
    assert_eq!(
        view.take_request(),
        Some(Request::SetList {
            document,
            node,
            list: None,
        }),
        "Clear is the same request with no list — one dimension, both directions"
    );
}

/// Choosing the list the task is already in writes nothing: the guarded set
/// would answer the same thing, and not asking is cheaper than being told.
#[test]
fn choosing_the_list_it_is_already_in_asks_for_nothing() {
    let (bike, face) = list_face("Bike Project", Some("blue"));
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![row_in("Order the groupset", bike)],
    );
    view.set_lists(vec![face]);
    open_picker(&mut view);
    press_picker_row(&mut view, 0);
    assert_eq!(view.take_request(), None);
    assert!(view.picker_for_test().is_none());
}

/// A workspace with no lists gets a line that says where one is made, rather
/// than a panel offering only *Clear* — which is a dead end wearing a menu.
#[test]
fn a_workspace_with_no_lists_says_where_one_is_made() {
    use super::menu::ListPick;
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![wire_row("Loose end", Vec::new())],
    );
    open_picker(&mut view);
    assert_eq!(view.picker_for_test(), Some(vec![ListPick::Empty]));
    assert_eq!(
        view.picker_enabled_for_test(),
        vec![false],
        "and it cannot be chosen"
    );
}

/// The picker never opens over a row this page is not showing. The page it was
/// asked from can be half a minute old, and writing into a task the reader
/// cannot see is the one thing a menu row must not be able to do.
#[test]
fn the_picker_refuses_a_row_this_page_does_not_hold() {
    let mut view = view_answered(RailTarget::Bucket(Bucket::Today), Vec::new());
    assert!(!view.open_list_picker(DocumentId::new(), NodeId::new()));
    assert!(view.picker_for_test().is_none());
}

/// Escape closes the picker and asks for nothing — the rule every panel in this
/// window follows.
#[test]
fn escape_closes_the_picker() {
    let (bike, face) = list_face("Bike Project", Some("blue"));
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![row_in("Order the groupset", bike)],
    );
    view.set_lists(vec![face]);
    open_picker(&mut view);
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    koru_workspace_host::View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty(),
    );
    assert!(view.picker_for_test().is_none());
    assert_eq!(view.take_request(), None);
}

/// Another page is another set of rows, so a panel left standing over one of
/// them would be pointed at a task that is no longer on screen.
#[test]
fn changing_the_page_takes_the_picker_away() {
    let (bike, face) = list_face("Bike Project", Some("blue"));
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![row_in("Order the groupset", bike)],
    );
    view.set_lists(vec![face]);
    open_picker(&mut view);
    view.set_target(RailTarget::List(bike));
    assert!(view.picker_for_test().is_none());
}

// ----------------------------------------------------------- the three states (V-5)
//
// Each of the three renders under **its own** gate and not under the other two.
// The fixtures differ by exactly one fact each — a notice, a sweep — so a test
// that passes because two gates overlap is a test that fails the day the
// overlap is removed.
//
// **There were four, and the fourth's fixture was the rail's counts.** A
// *teaching* state fired whenever no bucket held anything; acceptance retired
// it, because it repeated the capture row directly above it and because its
// gate was an answer arriving on its own schedule — so it went on teaching a
// workspace that had just gained a task. Nothing here reads a count any more,
// which is the point: every gate left is a fact about the page being drawn.

use super::states::Empty;

/// An answered-empty page on `target`.
fn empty_page(target: RailTarget) -> TasksView {
    view_answered(target, Vec::new())
}

/// **An answered empty page draws nothing under its capture row** (the
/// owner's ask, 2026-11-05): no *Nothing due today.* sentence and no second
/// *Add a task* — the capture row is already the page's first row and says
/// what to do. Every queue and a list alike.
#[test]
fn an_answered_empty_page_draws_no_state() {
    let (bike, _) = list_face("Bike Project", Some("blue"));
    for target in [
        RailTarget::Bucket(Bucket::Today),
        RailTarget::Bucket(Bucket::Upcoming),
        RailTarget::Bucket(Bucket::Completed),
        RailTarget::List(bike),
    ] {
        let view = empty_page(target);
        assert_eq!(view.empty_for_test(), None, "{target:?}");
    }
}

/// **A mid-sweep zero never reads as "you have no tasks"** — it outranks the
/// bucket's own sentence, which would otherwise claim the store had answered.
#[test]
fn a_sweep_is_not_an_empty_workspace() {
    let mut view = TasksView::new(RailTarget::Bucket(Bucket::Today));
    view.set_viewport(1000.0, 700.0);
    view.panel_mut().accept_for_test(
        &TaskQueryResponse {
            rows: Vec::new(),
            definitions: Vec::new(),
            tags: Vec::new(),
            groups: Vec::new(),
            projected: true,
            updating_workspaces: vec![koru_store_core::ids::WorkspaceId::new()],
            limit: 100,
            next_after: None,
            restarted: false,
            covered: None,
        },
        today(),
        now_ms(),
    );
    let state = view.empty_for_test().expect("a state");
    assert_eq!(state, Empty::Cold);
    assert_eq!(state.line(), "Checking your workspaces\u{2026}");
    assert!(state.dotted(), "the one state with something in progress");
}

/// **A store that would not answer says so**, and outranks both â€” an empty
/// list and a failed query look identical, and the one somebody can act on is
/// the one that names itself.
#[test]
fn a_refused_query_names_itself() {
    let mut view = TasksView::new(RailTarget::Bucket(Bucket::Today));
    view.set_viewport(1000.0, 700.0);
    view.panel_mut()
        .fail_for_test("Could not list tasks: no store");
    assert_eq!(
        view.empty_for_test(),
        Some(Empty::Notice("Could not list tasks: no store".to_owned())),
        "a notice is drawn even though nothing has been answered"
    );
}

/// Before the first answer an empty list means *we have not looked yet*, and
/// half a second of blank is honest where any of the three sentences would not
/// be.
#[test]
fn nothing_is_said_before_the_first_answer() {
    let mut view = TasksView::new(RailTarget::Bucket(Bucket::Today));
    view.set_viewport(1000.0, 700.0);
    assert_eq!(view.empty_for_test(), None);
}

// ------------------------------------------------------------ the rollover (V-5)

/// **Midnight is a comparison, not a timer.** The watch is a pure function of
/// the day it is handed, which is what lets a rollover be driven in a test.
#[test]
fn the_day_watch_answers_only_when_the_date_moves() {
    let mut watch = query::DayWatch::default();
    assert!(
        !watch.rolled(today()),
        "the first tick has nothing to be stale yet"
    );
    assert!(!watch.rolled(today()), "and the same day is not a rollover");
    let tomorrow = today().succ_opt().expect("a real day");
    assert!(watch.rolled(tomorrow));
    assert_eq!(watch.day(), Some(tomorrow));
    assert!(!watch.rolled(tomorrow), "once, not on every tick after");
    // Backwards too: a clock corrected, or a laptop woken in another zone.
    assert!(watch.rolled(today()));
}

/// What the app does with that answer: the page is asked for again **now**
/// rather than at the end of its half minute, which is what makes every bucket
/// edge recomputed at the rollover rather than whenever the next refresh
/// happened to fall.
#[test]
fn a_rollover_re_arms_the_page() {
    let mut panel = query::TaskListPanel::default();
    let now = std::time::Instant::now();
    panel.maintain(
        Some(WorkspaceId::new()),
        Some(RailTarget::Bucket(Bucket::Today)),
        true,
        now,
    );
    panel.accept_for_test(&one_answer(Vec::new()), today(), now_ms());
    // The one call the rollover makes, which is the same one a mutation makes.
    panel.invalidate();
    assert!(
        panel
            .deadline()
            .is_some_and(|due| due <= std::time::Instant::now()),
        "after a rollover the page is owed immediately"
    );
}

// ------------------------------------------------- keyboard and selection (V-5)

/// **The Trash tab's conventions**, so the two lists in this window answer an
/// arrow the same way: down from nothing selects the first, up from nothing the
/// last, and neither runs off its end.
///
/// The one place they differ is the one place the pages differ: this list has a
/// row *above* its first task — the capture line — and Trash does not, so up
/// from the first row lands in it rather than stopping. See the arm in
/// `TasksView::key`; before Plan 2.1 that line was reachable with a pointer and
/// with nothing else, which made the only way to create a task here unreachable
/// without a mouse.
#[test]
fn arrows_step_the_selection_the_way_the_trash_tab_does() {
    let mut view = view_on(
        RailTarget::Bucket(Bucket::Today),
        vec![
            entry("First", Some("2026-08-30")),
            entry("Second", None),
            entry("Third", None),
        ],
    );
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    let mut press = |view: &mut TasksView, key: NamedKey| {
        koru_workspace_host::View::key(
            view,
            &mut doc,
            &Key::Named(key),
            None,
            ModifiersState::empty(),
        )
    };
    assert!(press(&mut view, NamedKey::ArrowDown));
    assert_eq!(view.selected_for_test(), Some(0));
    assert!(press(&mut view, NamedKey::ArrowDown));
    assert_eq!(view.selected_for_test(), Some(1));
    assert!(press(&mut view, NamedKey::ArrowUp));
    assert_eq!(view.selected_for_test(), Some(0));
    assert!(press(&mut view, NamedKey::ArrowUp));
    assert_eq!(
        view.selected_for_test(),
        None,
        "up from the first row leaves the list for the capture line above it"
    );
    assert!(view.capturing_for_test(), "and that line takes the caret");
    assert!(press(&mut view, NamedKey::Escape));
    assert_eq!(view.selected_for_test(), None);
    assert!(!view.capturing_for_test(), "Escape leaves it again");
    assert!(press(&mut view, NamedKey::ArrowUp));
    assert_eq!(
        view.selected_for_test(),
        Some(2),
        "up from nothing is the last row"
    );
}

/// **Enter opens, because a whole-row click opens** â€” the keyboard and the
/// pointer cannot ask for two different things.
#[test]
fn enter_opens_the_selected_row() {
    let rows = vec![entry("First", None), entry("Second", None)];
    let (document, node) = (rows[1].document, rows[1].node);
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), rows);
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    for key in [NamedKey::ArrowDown, NamedKey::ArrowDown, NamedKey::Enter] {
        koru_workspace_host::View::key(
            &mut view,
            &mut doc,
            &Key::Named(key),
            None,
            ModifiersState::empty(),
        );
    }
    assert_eq!(
        view.take_request(),
        Some(Request::Open {
            document,
            node,
            new_tab: false,
        }),
        "the same request a click on the row makes"
    );
}

/// The menu key is the row's `â€¦` by another name, and it is anchored where that
/// button is rather than at a pointer that was never there.
#[test]
fn the_menu_key_asks_for_the_selected_rows_menu() {
    let rows = vec![entry("Only", None)];
    let (document, node) = (rows[0].document, rows[0].node);
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), rows);
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    for key in [NamedKey::ArrowDown, NamedKey::ContextMenu] {
        koru_workspace_host::View::key(
            &mut view,
            &mut doc,
            &Key::Named(key),
            None,
            ModifiersState::empty(),
        );
    }
    let line = view.line_of_task_for_test(0).expect("a line");
    let dots = view.parts_for_test(view.line_rect_for_test(line)).dots;
    assert_eq!(
        view.take_request(),
        Some(Request::RowMenu {
            document,
            node,
            completed: false,
            pinned_today: false,
            native: false,
            list: None,
            at: (dots.x + dots.w, dots.y + dots.h),
        })
    );
}

/// A press moves the keyboard with it, so Enter afterwards acts on the row that
/// was touched rather than on one left behind.
#[test]
fn a_press_moves_the_selection() {
    let mut view = view_on(
        RailTarget::Bucket(Bucket::Today),
        vec![entry("First", None), entry("Second", None)],
    );
    let line = view.line_of_task_for_test(1).expect("a line");
    let r = view.line_rect_for_test(line);
    let text = view.parts_for_test(r).text;
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    koru_workspace_host::View::mouse_down(
        &mut view,
        &mut doc,
        text.x + 4.0,
        r.y + r.h * 0.5,
        false,
    );
    assert_eq!(view.selected_for_test(), Some(1));
}

/// A page that has moved on drops the selection rather than re-pointing it: the
/// keyboard must not end up on a task nobody put it on.
#[test]
fn a_selection_whose_row_has_gone_is_dropped() {
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![wire_row("Only", Vec::new())],
    );
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    koru_workspace_host::View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::ArrowDown),
        None,
        ModifiersState::empty(),
    );
    assert_eq!(view.selected_for_test(), Some(0));
    view.panel_mut().accept_for_test(
        &one_answer(vec![wire_row("Something else", Vec::new())]),
        today(),
        now_ms(),
    );
    koru_workspace_host::View::adopt(&mut view, &doc);
    view.catch_up(&doc);
    assert_eq!(view.selected_for_test(), None);
}

/// **A task lands in the queue it was typed into.** Three tasks typed into
/// Today used to file themselves in *Later* — a dateless task's queue — and
/// read as tasks that were never created (finding 28).
///
/// The dates are not asserted here. Each one is fed back to
/// `koru_core::tasks::bucket`, which is the function that decides queues, so
/// this says what it means — *the capture lands where it was typed* — rather
/// than restating a table of offsets that already lives in the domain.
#[test]
fn a_capture_lands_in_the_queue_it_was_typed_into() {
    for bucket in [Bucket::Today, Bucket::Next, Bucket::Upcoming] {
        let (request, today) = captured(bucket, "Ship it");
        let Some(Request::Capture { due, .. }) = request else {
            panic!("Enter on the capture row asks for a task");
        };
        let due = due.expect("a datable queue seeds a date");
        assert_eq!(
            koru_core::tasks::bucket(
                Some(&due),
                koru_core::attributes::Lifecycle::Open,
                None,
                koru_core::time::DateOnly::new(today),
                &koru_editor::utils::zone::SystemZone::detect(),
            ),
            bucket,
            "typed into {bucket:?}"
        );
    }
}

/// *Later* seeds nothing, and it does not have to: a dateless task with no
/// priority is already Later. The capture writes what the person said and
/// nothing more, which is the rule everywhere it can be kept.
#[test]
fn a_capture_into_later_invents_no_date() {
    assert_eq!(
        captured(Bucket::Later, "Someday").0,
        Some(Request::Capture {
            title: "Someday".to_owned(),
            due: None,
            list: None,
            priority: None,
            rank: None,
        })
    );
}

/// **A typed date word still wins.** The seed is a default, never an override:
/// `Chase it tomorrow` on the Today page is due tomorrow, which puts it in
/// *Next* — the person said so.
#[test]
fn a_typed_date_word_beats_the_queue_it_was_typed_into() {
    let (request, today) = captured(Bucket::Today, "Chase it tomorrow");
    let Some(Request::Capture { title, due, .. }) = request else {
        panic!("Enter on the capture row asks for a task");
    };
    assert_eq!(title, "Chase it");
    let due = due.expect("the word became the date");
    assert_ne!(
        due,
        koru_core::time::Temporal::Date(koru_core::time::DateOnly::new(today)),
        "not today's seed"
    );
}

/// Type `title` into `bucket`'s capture row and press Enter, and answer the
/// day the view was drawn for beside the request — a seeded date is relative to
/// *its* today, never to this machine's.
fn captured(bucket: Bucket, title: &str) -> (Option<Request>, chrono::NaiveDate) {
    let mut view = view_on(RailTarget::Bucket(bucket), Vec::new());
    let r = view.line_rect_for_test(0);
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    koru_workspace_host::View::mouse_down(
        &mut view,
        &mut doc,
        r.x + r.w * 0.5,
        r.y + r.h * 0.5,
        false,
    );
    for ch in title.chars() {
        let text = ch.to_string();
        koru_workspace_host::View::key(
            &mut view,
            &mut doc,
            &Key::Character(text.clone().into()),
            Some(&text),
            ModifiersState::empty(),
        );
    }
    koru_workspace_host::View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    );
    (view.take_request(), view.today())
}

/// **Completed offers no capture row**, because it is the one queue the seed
/// cannot reach: `first_day_in` answers `None` for it, and `bucket` decides it
/// from the lifecycle before it reads a due at all. The row used to be drawn
/// there, take the words, and file the task in *Later* — finding 28's shape,
/// surviving in the one place the fix could not follow.
///
/// The empty state had always said so; this is the list agreeing with it.
#[test]
fn completed_offers_no_capture_row() {
    let view = view_on(RailTarget::Bucket(Bucket::Completed), Vec::new());
    assert!(
        !view.has_capture_line_for_test(),
        "Completed is a log, not a place work is added"
    );
    for bucket in [Bucket::Today, Bucket::Next, Bucket::Upcoming, Bucket::Later] {
        let view = view_on(RailTarget::Bucket(bucket), Vec::new());
        assert!(
            view.has_capture_line_for_test(),
            "{bucket:?} takes a new task"
        );
    }
    let view = view_on(RailTarget::List(koru_core::ids::ListId::new()), Vec::new());
    assert!(
        view.has_capture_line_for_test(),
        "and so does a list — SPEC §8's membership is set by the capture"
    );
}

// ------------------------------------------------ the two in-place pickers (INPLACE V)

use super::pickers::{Choice, PriorityRow, Target, resolve_pick};
use koru_core::attributes::Priority;
use koru_core::time::{DateOnly, Temporal};
use koru_ui::date_grid::{self, DateGrid, Pick};

fn dated_row(title: &str, due: &str) -> TaskRow {
    wire_row(
        title,
        vec![attr(koru_core::attributes::KEY_DUE, "date", due)],
    )
}

fn prioritised_row(title: &str, level: &str) -> TaskRow {
    wire_row(
        title,
        vec![attr(koru_core::attributes::KEY_PRIORITY, "text", level)],
    )
}

fn first_row(view: &TasksView) -> (DocumentId, NodeId) {
    let row = view.panel().rows()[0].clone();
    (row.document, row.node)
}

fn escape(view: &mut TasksView) {
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test").unwrap();
    koru_workspace_host::View::key(
        view,
        &mut doc,
        &Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty(),
    );
}

fn type_phrase(view: &mut TasksView, phrase: &str) {
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test").unwrap();
    for ch in phrase.chars() {
        let text = ch.to_string();
        koru_workspace_host::View::key(
            view,
            &mut doc,
            &Key::Character(text.clone().into()),
            Some(&text),
            ModifiersState::empty(),
        );
    }
    koru_workspace_host::View::key(
        view,
        &mut doc,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    );
}

/// **The whole adapter, four arms.** A day is a date, a phrase is parsed
/// against this grid's reference and converted by the one function, Clear is
/// the same request with no due, and nothing is nothing with the panel kept.
#[test]
fn the_four_picks_resolve_to_the_four_answers() {
    let (document, node) = (DocumentId::new(), NodeId::new());
    let grid = DateGrid::bare(today());
    let day = NaiveDate::from_ymd_opt(2026, 9, 4).unwrap();
    assert_eq!(
        resolve_pick(&Target::row(document, node), &grid, Pick::Day(day)),
        Choice::Request(Request::SetDue {
            document,
            node,
            due: Some(Temporal::Date(DateOnly::new(day))),
        }),
        "a day stores a calendar date, as the document's picker stores one"
    );
    assert_eq!(
        resolve_pick(&Target::row(document, node), &grid, Pick::Clear),
        Choice::Request(Request::SetDue {
            document,
            node,
            due: None,
        })
    );
    assert_eq!(
        resolve_pick(&Target::row(document, node), &grid, Pick::None),
        Choice::Stay
    );
    match resolve_pick(
        &Target::row(document, node),
        &grid,
        Pick::Timed("tomorrow 9am".into()),
    ) {
        Choice::Request(Request::SetDue {
            due: Some(Temporal::Zoned(_)),
            ..
        }) => {}
        other => panic!("a timed phrase stores a zoned instant: {other:?}"),
    }
}

/// The priority rows are the editor's tables, in the scale's order, the
/// current one checked — and Clear only while one is set.
#[test]
fn the_priority_picker_offers_the_scale_and_clear_only_while_one_is_set() {
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![prioritised_row("Order the groupset", "high")],
    );
    let (document, node) = first_row(&view);
    assert!(view.open_priority_picker(document, node));
    let rows = view.priority_picker_for_test().expect("a priority picker");
    assert_eq!(
        rows,
        vec![
            PriorityRow::Level {
                level: Priority::Urgent,
                checked: false
            },
            PriorityRow::Level {
                level: Priority::High,
                checked: true
            },
            PriorityRow::Level {
                level: Priority::Medium,
                checked: false
            },
            PriorityRow::Level {
                level: Priority::Low,
                checked: false
            },
            PriorityRow::Divider,
            PriorityRow::Clear,
        ]
    );

    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![wire_row("Loose end", Vec::new())],
    );
    let (document, node) = first_row(&view);
    assert!(view.open_priority_picker(document, node));
    let rows = view.priority_picker_for_test().expect("a priority picker");
    assert_eq!(rows.len(), 4, "nothing to clear, no Clear row");
    assert!(
        rows.iter()
            .all(|r| koru_ui::MenuItem::checked(r) == Some(false))
    );
}

/// A level leaves as a request and the panel closes; the level it already
/// has asks for nothing; Clear is the same request with no level.
#[test]
fn a_priority_row_leaves_as_a_request_and_closes() {
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![prioritised_row("Order the groupset", "high")],
    );
    let (document, node) = first_row(&view);
    view.open_priority_picker(document, node);
    press_picker_row(&mut view, 0);
    assert_eq!(
        view.take_request(),
        Some(Request::SetPriority {
            document,
            node,
            priority: Some(Priority::Urgent),
        })
    );
    assert!(!view.has_picker_for_test(), "a choice closes the panel");

    // The row already wears Urgent — the echo put it there before the store
    // answered — so Urgent is the no-op now.
    view.open_priority_picker(document, node);
    press_picker_row(&mut view, 0);
    assert_eq!(
        view.take_request(),
        None,
        "the level it already has is a no-op"
    );
    assert!(!view.has_picker_for_test());

    view.open_priority_picker(document, node);
    press_picker_row(&mut view, 5);
    assert_eq!(
        view.take_request(),
        Some(Request::SetPriority {
            document,
            node,
            priority: None,
        })
    );
}

/// **What the calendar opens on.** A row with a due opens on that day with
/// Clear offered; a dateless row opens on today without it — the editor's
/// own rule, read off the row.
#[test]
fn the_calendar_opens_on_the_rows_day_with_clear_and_on_today_without() {
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![dated_row("Ship it", "2026-11-02")],
    );
    let (document, node) = first_row(&view);
    assert!(view.open_due_picker(document, node));
    let (selected, offers_clear, panel) = view.due_picker_for_test().expect("a calendar");
    assert_eq!(selected, NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
    assert!(offers_clear);
    assert!(panel.x >= 0.0 && panel.y >= 0.0, "inside the viewport");
    assert!(panel.x + panel.w <= 1000.0 && panel.y + panel.h <= 700.0);

    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![wire_row("Loose end", Vec::new())],
    );
    let (document, node) = first_row(&view);
    assert!(view.open_due_picker(document, node));
    let (selected, offers_clear, _) = view.due_picker_for_test().expect("a calendar");
    assert_eq!(selected, today());
    assert!(!offers_clear, "nothing to clear, no Clear row");
}

/// A day pressed on the grid leaves as a date and closes the panel; the
/// press lands exactly where the widget draws the cell, offset by the phrase
/// field's band and the panel's place on the page.
#[test]
fn a_day_pressed_on_the_calendar_leaves_as_a_request() {
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![wire_row("Loose end", Vec::new())],
    );
    let (document, node) = first_row(&view);
    view.open_due_picker(document, node);
    let (selected, _, panel) = view.due_picker_for_test().unwrap();
    let cell = date_grid::cell_rect(selected, 15);
    press_at(
        &mut view,
        Rect::new(
            panel.x + cell.x,
            panel.y + cell.y + date_grid::field_band(),
            cell.w,
            cell.h,
        ),
    );
    assert_eq!(
        view.take_request(),
        Some(Request::SetDue {
            document,
            node,
            due: Some(Temporal::Date(DateOnly::new(
                NaiveDate::from_ymd_opt(2026, 8, 15).unwrap()
            ))),
        })
    );
    assert!(!view.has_picker_for_test());
}

#[test]
fn clear_on_the_calendar_leaves_as_a_clear_request() {
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![dated_row("Ship it", "2026-11-02")],
    );
    let (document, node) = first_row(&view);
    view.open_due_picker(document, node);
    let (selected, offers_clear, panel) = view.due_picker_for_test().unwrap();
    assert!(offers_clear);
    let grid = DateGrid::with_phrase_field(
        &mut koru_draw::text::TextEngine::new(),
        selected,
        today().and_hms_opt(12, 0, 0).unwrap(),
        true,
    );
    let top = grid.clear_row_top().expect("a Clear row");
    press_at(
        &mut view,
        Rect::new(panel.x, panel.y + top, panel.w, koru_draw::theme::ROW_H),
    );
    assert_eq!(
        view.take_request(),
        Some(Request::SetDue {
            document,
            node,
            due: None,
        })
    );
}

/// A phrase typed into the calendar's box is parsed against this grid and
/// converted by the one function — a timed one stores an instant.
#[test]
fn a_timed_phrase_typed_on_the_calendar_leaves_as_a_zoned_due() {
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![wire_row("Loose end", Vec::new())],
    );
    let (document, node) = first_row(&view);
    view.open_due_picker(document, node);
    type_phrase(&mut view, "tomorrow 9am");
    match view.take_request() {
        Some(Request::SetDue {
            document: d,
            node: n,
            due: Some(Temporal::Zoned(_)),
        }) => assert_eq!((d, n), (document, node)),
        other => panic!("a timed phrase stores a zoned instant: {other:?}"),
    }
    assert!(!view.has_picker_for_test());
}

/// Escape closes either panel and writes nothing.
#[test]
fn escape_closes_a_picker_and_writes_nothing() {
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![dated_row("Ship it", "2026-11-02")],
    );
    let (document, node) = first_row(&view);
    view.open_due_picker(document, node);
    escape(&mut view);
    assert!(!view.has_picker_for_test());
    assert_eq!(view.take_request(), None);
    view.open_priority_picker(document, node);
    escape(&mut view);
    assert!(!view.has_picker_for_test());
    assert_eq!(view.take_request(), None);
}

/// A press outside the calendar dismisses it and does nothing more.
#[test]
fn a_press_outside_the_calendar_dismisses_it() {
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![wire_row("Loose end", Vec::new())],
    );
    let (document, node) = first_row(&view);
    view.open_due_picker(document, node);
    let (_, _, panel) = view.due_picker_for_test().unwrap();
    let outside = if panel.y > 60.0 {
        10.0
    } else {
        panel.y + panel.h + 20.0
    };
    press_at(&mut view, Rect::new(900.0, outside, 1.0, 1.0));
    assert!(!view.has_picker_for_test());
    assert_eq!(view.take_request(), None);
}

/// A picker over a row this page is not showing answers `false` — the page
/// it was asked from can be half a minute old.
#[test]
fn a_picker_over_a_row_the_page_does_not_hold_answers_false() {
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![wire_row("Loose end", Vec::new())],
    );
    assert!(!view.open_due_picker(DocumentId::new(), NodeId::new()));
    assert!(!view.open_priority_picker(DocumentId::new(), NodeId::new()));
    assert!(!view.has_picker_for_test());
}

/// A panel opened over the last row near the bottom of the viewport is
/// placed above the row rather than off the page.
#[test]
fn a_calendar_near_the_bottom_opens_above_its_row() {
    let dots = Rect::new(900.0, 660.0, 16.0, 16.0);
    let panel = super::pickers::DuePick::place(dots, (200.0, 300.0), (1000.0, 700.0));
    assert!(panel.y + panel.h <= dots.y, "above the row");
    assert!(panel.y >= 0.0);
    assert!(panel.x + panel.w <= 1000.0);
    let panel = super::pickers::DuePick::place(
        Rect::new(900.0, 100.0, 16.0, 16.0),
        (200.0, 300.0),
        (1000.0, 700.0),
    );
    assert!(panel.y > 116.0, "below the row when there is room");
}

// ------------------------------------------------ the echo (round nine, 31)

/// Press the capture row, type, press Enter — the whole gesture, so the row
/// arrives the way a person makes one.
fn capture(view: &mut TasksView, text: &str) {
    let r = view.line_rect_for_test(0);
    press_at(view, r);
    type_phrase(view, text);
}

fn tick_row(view: &mut TasksView, row: usize) {
    let line = view.line_of_task_for_test(row).expect("a line");
    let parts = view.parts_for_test(view.line_rect_for_test(line));
    press_at(view, parts.control);
}

fn a_receipt_for(document: DocumentId, node: NodeId) -> koru_workspace_host::tasks::Landed {
    koru_workspace_host::tasks::Landed {
        workspace: None,
        document,
        node: Some(node),
        caret: None,
        receipt: Some(koru_workspace::tasks::WriteReceipt {
            peer: 1,
            counter: 1,
        }),
        published: true,
    }
}

/// **A captured task is on the page the moment Enter is pressed**, above the
/// store's answer, and it is still there when the store answers without it.
#[test]
fn a_capture_shows_its_row_before_the_store_answers() {
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![dated_row("Existing", "2026-08-31")],
    );
    capture(&mut view, "Chase it");
    assert!(matches!(view.take_request(), Some(Request::Capture { .. })));
    assert_eq!(view.rows.len(), 2, "the new row is drawn now");
    assert_eq!(view.rows[1].title, "Chase it");
    assert_eq!(view.echoes_for_test(), 1);

    // The store answers, behind the write: the row stays.
    let doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test").unwrap();
    view.panel_mut().accept_for_test(
        &one_answer(vec![dated_row("Existing", "2026-08-31")]),
        today(),
        now_ms(),
    );
    view.mark_stale();
    view.catch_up(&doc);
    assert_eq!(view.rows.len(), 2, "still drawn over the store's rows");
}

/// A tick takes the row off an open queue at once; the store confirms later.
#[test]
fn a_tick_leaves_the_queue_at_once() {
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![dated_row("Ship it", "2026-08-31")],
    );
    tick_row(&mut view, 0);
    assert!(matches!(
        view.take_request(),
        Some(Request::Complete {
            completed: true,
            ..
        })
    ));
    assert!(view.rows.is_empty(), "the row left the queue with the tick");
}

/// A date past the queue moves the row off the page at once; one inside it
/// changes the chip and keeps the row.
#[test]
fn a_new_date_moves_the_row_where_the_date_says() {
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![dated_row("Ship it", "2026-08-31")],
    );
    let (document, node) = first_row(&view);
    view.open_due_picker(document, node);
    type_phrase(&mut view, "in 20d");
    // A day phrase refines the grid; the second Enter picks the day.
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test").unwrap();
    koru_workspace_host::View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    );
    assert!(matches!(view.take_request(), Some(Request::SetDue { .. })));
    assert!(
        view.rows.is_empty(),
        "twenty days out is Upcoming, not Today"
    );

    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Later),
        vec![wire_row("Someday", Vec::new())],
    );
    let (document, node) = first_row(&view);
    view.open_priority_picker(document, node);
    press_picker_row(&mut view, 1);
    assert!(matches!(
        view.take_request(),
        Some(Request::SetPriority { .. })
    ));
    assert!(
        view.rows.is_empty(),
        "a dateless task at High is Next, and leaves Later"
    );
}

/// **The echo goes when the wait ends**, and only then: an answer that
/// arrives while the wait stands does not take it away, and the answer that
/// ends the wait replaces it.
#[test]
fn the_echo_goes_when_the_wait_ends() {
    // One row, reused: a second `dated_row` would be a second task.
    let shipped = dated_row("Ship it", "2026-08-31");
    let mut view = view_answered(RailTarget::Bucket(Bucket::Today), vec![shipped.clone()]);
    let (document, node) = first_row(&view);
    tick_row(&mut view, 0);
    view.take_request();
    assert!(view.rows.is_empty());
    let doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test").unwrap();

    // The receipt lands and the wait begins.
    view.echo_landed(&a_receipt_for(document, node));
    let t0 = std::time::Instant::now();
    let requirement = crate::tasks_view::query::WriteRequirement {
        workspace: koru_store_core::ids::WorkspaceId::new(),
        document: koru_store_core::ids::DocumentId::new(),
        peer: 1,
        counter: 1,
    };
    view.panel_mut().require_for_test(requirement, t0);
    let carried = view.panel().wanted_for_test().to_vec();

    // An answer behind the write: the row is still gone from the page.
    let mut stale = one_answer(vec![shipped.clone()]);
    stale.covered = Some(false);
    view.panel_mut()
        .accept_carrying_for_test(&stale, &carried, today(), now_ms(), t0 + ms(600));
    view.mark_stale();
    view.catch_up(&doc);
    assert!(view.rows.is_empty(), "the store is behind; the echo stands");
    assert_eq!(view.echoes_for_test(), 1);

    // The answer that has the write: the echo is spent, the rows are the
    // store's.
    let fresh = one_answer(Vec::new());
    let mut fresh = fresh;
    fresh.covered = Some(true);
    view.panel_mut()
        .accept_carrying_for_test(&fresh, &carried, today(), now_ms(), t0 + ms(900));
    view.mark_stale();
    view.catch_up(&doc);
    assert_eq!(view.echoes_for_test(), 0);
    assert!(view.rows.is_empty());
}

/// **An echo the store was never told to wait for still stands.**
///
/// `Landed::published` is `false` for an ordinary state — a stopped session, an
/// outbox that will carry the write later — and a receipt that names no
/// addressable workspace is the same: both mean *invalidate and do not wait*,
/// so no requirement is armed. The echo is armed all the same, and the next
/// answer to arrive is necessarily one taken before the write. Dropping the
/// echo there puts the row back the way it was, which is the tick that
/// un-ticks itself.
#[test]
fn an_echo_with_nothing_to_wait_on_is_not_dropped_by_the_next_answer() {
    let shipped = dated_row("Ship it", "2026-08-31");
    let mut view = view_answered(RailTarget::Bucket(Bucket::Today), vec![shipped.clone()]);
    let (document, node) = first_row(&view);
    tick_row(&mut view, 0);
    view.take_request();
    assert!(view.rows.is_empty(), "the tick shows at once");
    let doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test").unwrap();

    // The receipt lands, but the write was not published — so the container
    // invalidates without a requirement and no wait is armed.
    let mut receipt = a_receipt_for(document, node);
    receipt.published = false;
    view.echo_landed(&receipt);
    assert_eq!(view.echoes_for_test(), 1);

    // An ordinary answer, taken before the write. Nothing about it can contain
    // a write the store has not been told about.
    let t0 = std::time::Instant::now();
    let stale = one_answer(vec![shipped.clone()]);
    view.panel_mut()
        .accept_carrying_for_test(&stale, &[], today(), now_ms(), t0 + ms(600));
    view.mark_stale();
    view.catch_up(&doc);

    assert_eq!(
        view.echoes_for_test(),
        1,
        "the echo was dropped by an answer that could not have had the write"
    );
    assert!(
        view.rows.is_empty(),
        "so the row came back and the tick undid itself"
    );
}

/// ...and it is still bounded. An echo with nothing to wait on is not dropped
/// by an answer, so the ceiling is the only thing that ends it — which is what
/// the module note means by *no echo outlives the bounded wait by more than a
/// turn*, and what stops the fix above turning a flicker into a row that never
/// agrees with the store again.
#[test]
fn an_echo_with_nothing_to_wait_on_still_expires_at_the_ceiling() {
    let shipped = dated_row("Ship it", "2026-08-31");
    let mut view = view_answered(RailTarget::Bucket(Bucket::Today), vec![shipped.clone()]);
    let (document, node) = first_row(&view);
    tick_row(&mut view, 0);
    view.take_request();
    let doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test").unwrap();

    let mut receipt = a_receipt_for(document, node);
    receipt.published = false;
    view.echo_landed(&receipt);
    assert_eq!(view.echoes_for_test(), 1);

    // Past the ceiling, with the store still answering the old row.
    let over = crate::tasks_view::query::wait_ceiling() + ms(2000);
    let t0 = std::time::Instant::now();
    let stale = one_answer(vec![shipped]);
    view.panel_mut()
        .accept_carrying_for_test(&stale, &[], today(), now_ms(), t0 + over);
    view.age_echoes_for_test(over);
    view.mark_stale();
    view.catch_up(&doc);

    assert_eq!(view.echoes_for_test(), 0, "the ceiling ends it");
}

/// A refused capture takes its row back with the refusal; a landed one takes
/// the receipt's ids, so it can be acted on from that moment.
#[test]
fn a_refused_capture_takes_its_row_back_and_a_landed_one_takes_its_ids() {
    let mut view = view_answered(RailTarget::Bucket(Bucket::Today), Vec::new());
    capture(&mut view, "Chase it");
    view.take_request();
    assert_eq!(view.rows.len(), 1);
    view.echo_failed();
    assert!(view.rows.is_empty(), "the promise is taken back");
    assert_eq!(view.echoes_for_test(), 0);

    capture(&mut view, "Try again");
    view.take_request();
    let (document, node) = (DocumentId::new(), NodeId::new());
    view.echo_landed(&a_receipt_for(document, node));
    assert_eq!((view.rows[0].document, view.rows[0].node), (document, node));
}

fn ms(n: u64) -> std::time::Duration {
    std::time::Duration::from_millis(n)
}

// ------------------------------------------------ the origin glyph (round nine)

/// **A row that lives in a document leads with the page glyph; one added on
/// this page leads with nothing.** The kinds keep their own, and a task whose
/// title begins with an emoji wears that instead.
#[test]
fn a_row_with_an_origin_wears_the_page_glyph_and_a_native_one_wears_none() {
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Today),
        vec![
            wire_row("From a page", Vec::new()),
            wire_row("Added here", Vec::new()),
        ],
    );
    let rows = view.panel().rows().to_vec();
    view.set_task_documents(vec![rows[1].document]);
    assert_eq!(view.row_glyph(&rows[0]), Some(koru_draw::icons::DOCUMENT));
    assert_eq!(
        view.row_glyph(&rows[1]),
        None,
        "typed into this page: no document to point at"
    );

    let mut emoji = rows[0].clone();
    emoji.icon = Some("\u{1F680}".to_owned());
    assert_eq!(
        view.row_glyph(&emoji),
        None,
        "its own picture, drawn from `icon`"
    );
    let mut image = rows[1].clone();
    image.kind = RowKind::Image;
    assert_eq!(view.row_glyph(&image), Some(koru_draw::icons::IMAGE));
}

// ------------------------------------------------- the list chip (round nine)

/// **The list chip is the one chip that is a control.** On a queue page a press
/// on it asks for the list's page and the row's menu offers *Go to list*; on
/// the list's own page the same chip is a label again and the menu offers
/// nothing, because a door onto the room you are in is not a door. The other
/// chips stay labels wherever they are, and the rectangles the press is
/// resolved against are the ones the row is drawn into.
#[test]
fn the_list_chip_opens_the_list_and_only_where_it_leads_somewhere_else() {
    use crate::mainview::context_menu::MenuAction;
    use koru_draw::chip::ChipKind;
    let (birthday, face) = list_face("Birthday Tasks", Some("pink"));
    let row = TaskEntry::of(&row_in("Balloons", birthday), today(), now_ms()).expect("a row");
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), vec![row.clone()]);
    view.set_lists(vec![face.clone()]);

    let chips = view.row_chips_for_test(0);
    let rects = view.row_chip_rects_for_test(0);
    assert_eq!(chips.len(), rects.len(), "one rectangle per chip");
    let list_chip = chips
        .iter()
        .position(|chip| chip.list == Some(birthday))
        .expect("the list chip leads to the list");
    assert_eq!(
        chips.iter().filter(|chip| chip.list.is_some()).count(),
        1,
        "and no other chip does"
    );
    let (x, y) = rects[list_chip].centre();
    assert_eq!(view.hit_for_test(x, y), Some(Hit::ListChip(0)));
    let due = chips
        .iter()
        .position(|chip| chip.kind == ChipKind::Due)
        .expect("a dateless open row wears the dash");
    let (dx, dy) = rects[due].centre();
    // A value chip opens its own picker (the owner's ask: *click due,
    // priority or color to change it*), so the dash is a target too.
    assert_eq!(
        view.hit_for_test(dx, dy),
        Some(Hit::ValueChip(0, ChipKind::Due)),
        "its picker"
    );

    // **A press opens the list picker**, to change it — never the list's
    // page (the owner's ask, 2026-11-05). Going there is the menu's row.
    press_at(&mut view, rects[list_chip]);
    assert_eq!(view.take_request(), None, "nothing opened");
    assert!(view.list_picker_for_test().is_some(), "the list picker");
    assert_eq!(
        view.selected_for_test(),
        Some(0),
        "and the press selected the row"
    );
    view.picker = None;

    // The `\u{2026}` says so as well.
    let line = view.line_of_task_for_test(0).expect("a line");
    let dots = view.parts_for_test(view.line_rect_for_test(line)).dots;
    press_at(&mut view, dots);
    let Some(Request::RowMenu { list, .. }) = view.take_request() else {
        panic!("the row's menu")
    };
    assert_eq!(list, Some(birthday));
    let (_, items) = super::row_menu(row.document, row.node, false, false, list, false);
    assert!(items.contains(&MenuAction::TaskGoToList));

    // On the list's own page the chip still changes the list, and the
    // menu has no *Go to list* row.
    let mut own = view_on(RailTarget::List(birthday), vec![row]);
    own.set_lists(vec![face]);
    let rects = own.row_chip_rects_for_test(0);
    let (x, y) = rects[list_chip].centre();
    assert_eq!(own.hit_for_test(x, y), Some(Hit::ListChip(0)));
    // A list page draws one-line rows, so its dots are not where Today's were.
    let line = own.line_of_task_for_test(0).expect("a line");
    let dots = own.parts_for_test(own.line_rect_for_test(line)).dots;
    press_at(&mut own, dots);
    let Some(Request::RowMenu { list, .. }) = own.take_request() else {
        panic!("the row's menu")
    };
    assert_eq!(list, None);
}

// ------------------------------------------------ a home for a task (round nine)

/// **A page-made task given a document is the same row with a home.** The
/// echo names the new page under the title at once; the receipt gives the row
/// the page's ids, so it wears the page glyph and offers *View in origin*
/// from that moment; and the store's answer, which still carries the
/// page-made row until it catches up, does not draw it twice.
#[test]
fn a_task_given_a_home_keeps_its_row_and_takes_the_pages_ids() {
    use koru_workspace_host::tasks::TaskCommand;
    let mut view = view_answered(
        RailTarget::Bucket(Bucket::Later),
        vec![wire_row("Buy balloons", Vec::new())],
    );
    let row = view.panel().rows()[0].clone();
    view.set_task_documents(vec![row.document]);
    assert_eq!(view.row_glyph(&row), None, "page-made: no origin");

    view.echo_command(&TaskCommand::CreateHome {
        document: row.document,
        node: row.node,
    });
    assert_eq!(view.rows.len(), 1);
    assert_eq!(
        view.rows[0].title, "Notes Buy balloons",
        "the page is the task"
    );
    assert_eq!(view.rows[0].kind, RowKind::Document);
    assert_eq!(view.rows[0].breadcrumb, "");

    let (home, first) = (DocumentId::new(), NodeId::new());
    view.echo_landed(&a_receipt_for(home, first));
    let t0 = std::time::Instant::now();
    let requirement = crate::tasks_view::query::WriteRequirement {
        workspace: koru_store_core::ids::WorkspaceId::new(),
        document: koru_store_core::ids::DocumentId::new(),
        peer: 1,
        counter: 1,
    };
    view.panel_mut().require_for_test(requirement, t0);
    let carried = view.panel().wanted_for_test().to_vec();
    assert_eq!(
        view.rows.len(),
        1,
        "one row, not the old one and the new one"
    );
    assert_eq!((view.rows[0].document, view.rows[0].node), (home, first));
    assert_eq!(
        view.row_glyph(&view.rows[0].clone()),
        Some(koru_draw::icons::DOCUMENT),
        "a page's own glyph"
    );

    // The store answers again, still with the page-made row: hidden.
    let mut stale = one_answer(vec![wire_row("Buy balloons", Vec::new())]);
    stale.rows[0].document = koru_workspace::explore::store_document(row.document);
    stale.rows[0].node = row.node.to_storage();
    stale.rows[0].task = row.task.clone();
    stale.covered = Some(false);
    view.panel_mut()
        .accept_carrying_for_test(&stale, &carried, today(), now_ms(), t0 + ms(600));
    view.mark_stale();
    let doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    view.catch_up(&doc);
    assert_eq!(view.rows.len(), 1, "the store is behind; the echo stands");
    assert_eq!((view.rows[0].document, view.rows[0].node), (home, first));
}

// ------------------------------------------ the input's slash options (round nine)

/// The capture row and its `/` menu, typed into: the words, then a `/word`,
/// then Enter on the menu — the picker the page opens is left up.
fn capture_slash(view: &mut TasksView, words: &str, slash: &str) {
    let r = view.line_rect_for_test(0);
    press_at(view, r);
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    for ch in words.chars().chain(slash.chars()) {
        let text = ch.to_string();
        koru_workspace_host::View::key(
            view,
            &mut doc,
            &Key::Character(text.clone().into()),
            Some(&text),
            ModifiersState::empty(),
        );
    }
    assert!(view.capture_menu_open_for_test(), "the `/` menu is up");
    koru_workspace_host::View::key(
        view,
        &mut doc,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    );
}

fn enter(view: &mut TasksView) {
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    koru_workspace_host::View::key(
        view,
        &mut doc,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    );
}

/// **`/priority` on the input opens the page's own priority picker over the
/// row**, the pick rides on the row as a chip, and Enter carries it out with
/// the words — the `/word` itself never reaches the title.
#[test]
fn slash_priority_on_the_input_rides_out_with_the_capture() {
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), Vec::new());
    capture_slash(&mut view, "Chase it ", "/pri");
    assert!(
        view.priority_picker_for_test().is_some(),
        "the priority picker opened over the input"
    );
    assert_eq!(view.capture_preview_for_test(), None);
    press_picker_row(&mut view, 0);
    assert_eq!(
        view.capture_picks_for_test(),
        Some((None, Some(Priority::Urgent), None)),
        "Urgent, worn by the row"
    );
    assert!(!view.has_picker_for_test());
    enter(&mut view);
    let Some(Request::Capture {
        title,
        priority,
        due,
        ..
    }) = view.take_request()
    else {
        panic!("a capture")
    };
    assert_eq!(title, "Chase it");
    assert_eq!(priority, Some(Priority::Urgent));
    assert!(
        due.is_some(),
        "still seeded into the queue it was typed into"
    );
}

/// `/task_list` on the input offers the workspace's lists; the pick outranks
/// the page's own list, and a capture into a bucket carries it too.
#[test]
fn slash_task_list_on_the_input_files_the_capture_in_a_list() {
    let (bike, bike_face) = list_face("Bike Project", Some("blue"));
    let (wedding, wedding_face) = list_face("Wedding", Some("pink"));
    let mut view = view_on(RailTarget::List(bike), Vec::new());
    view.set_lists(vec![bike_face, wedding_face]);
    capture_slash(&mut view, "Order the cake ", "/task");
    let rows = view.picker_for_test().expect("the list picker opened");
    assert!(
        matches!(&rows[0], super::menu::ListPick::List { checked: true, .. }),
        "the page's own list is the one checked: {rows:?}"
    );
    press_picker_row(&mut view, 1);
    assert_eq!(
        view.capture_picks_for_test().map(|(_, _, list)| list),
        Some(Some(wedding))
    );
    enter(&mut view);
    let Some(Request::Capture { title, list, .. }) = view.take_request() else {
        panic!("a capture")
    };
    assert_eq!(title, "Order the cake");
    assert_eq!(list, Some(wedding), "the pick outranks the page's list");
}

/// `/due` opens the calendar over the input; a picked day outranks a typed
/// date word, which then stays in the title.
#[test]
fn slash_due_on_the_input_opens_the_calendar_and_outranks_the_word() {
    use super::pickers::{Choice, Picked};
    let mut view = view_on(RailTarget::Bucket(Bucket::Later), Vec::new());
    capture_slash(&mut view, "Chase it tomorrow ", "/due");
    assert!(view.due_picker_for_test().is_some(), "the calendar opened");
    let day = koru_core::time::DateOnly::parse("2026-09-20").unwrap();
    view.settle_for_test(Choice::Picked(Picked::Due(Some(
        koru_core::time::Temporal::Date(day),
    ))));
    assert!(!view.has_picker_for_test());
    enter(&mut view);
    let Some(Request::Capture { title, due, .. }) = view.take_request() else {
        panic!("a capture")
    };
    assert_eq!(title, "Chase it tomorrow", "the word is title under a pick");
    assert_eq!(due, Some(koru_core::time::Temporal::Date(day)));
}

/// **`/due` opens the calendar under the `/` that was typed** — its left edge
/// at the command, below the field — rather than hanging from the row's `…`
/// at the far end of the page, where the eye is not.
#[test]
fn slash_due_opens_the_calendar_under_the_command() {
    let mut view = view_on(RailTarget::Bucket(Bucket::Later), Vec::new());
    let r = view.line_rect_for_test(0);
    press_at(&mut view, r);
    // One frame, so the row knows where its field is drawn.
    view.paint(
        &mut vello::Scene::new(),
        vello::kurbo::Affine::IDENTITY,
        true,
    );
    capture_slash(&mut view, "Chase it ", "/due");
    let (_, _, panel) = view.due_picker_for_test().expect("the calendar opened");
    let field = view.capture_rect();
    let words = crate::input_engine::with(|engine| {
        engine
            .simple_layout("Chase it ", koru_draw::theme::role::row())
            .width()
    });
    assert!(
        (panel.x - (field.x + words)).abs() < 0.5,
        "the panel starts at the `/`: {} vs {}",
        panel.x,
        field.x + words
    );
    assert!(panel.y >= field.y + field.h, "below the field");
}

/// **A coloured line wears its colour on the page**: the swatch the document
/// draws after the same task's chips, last, in the line's own token.
#[test]
fn a_coloured_task_wears_its_swatch_last() {
    let row = TaskEntry::of(
        &wire_row(
            "Notes Improve LLMs",
            vec![attr(koru_core::attributes::KEY_COLOR, "text", "red")],
        ),
        today(),
        now_ms(),
    )
    .expect("a parseable row");
    assert_eq!(row.color.as_deref(), Some("red"));
    let view = view_on(RailTarget::Bucket(Bucket::Today), vec![row]);
    let chips = view.row_chips_for_test(0);
    let last = chips.last().expect("chips");
    assert_eq!(last.kind, koru_draw::chip::ChipKind::Swatch);
    assert_eq!(last.accent.as_deref(), Some("red"));

    let plain = view_on(
        RailTarget::Bucket(Bucket::Today),
        vec![entry("Plain", None)],
    );
    assert!(
        plain
            .row_chips_for_test(0)
            .iter()
            .all(|chip| chip.kind != koru_draw::chip::ChipKind::Swatch),
        "no colour, no swatch"
    );
}

/// **A value chip opens its picker at the pointer**: the due chip the
/// calendar, the colour dot the colour picker — and a colour chosen there is
/// one *set colour* for the row.
#[test]
fn a_value_chip_opens_its_picker_where_it_was_pressed() {
    let row = TaskEntry::of(
        &wire_row(
            "Ship it",
            vec![
                attr(koru_core::attributes::KEY_DUE, "date", "2026-09-02"),
                attr(koru_core::attributes::KEY_COLOR, "text", "red"),
            ],
        ),
        today(),
        now_ms(),
    )
    .expect("a parseable row");
    let (document, node) = (row.document, row.node);
    let mut view = view_on(RailTarget::Bucket(Bucket::Upcoming), vec![row]);
    let chips = view.row_chips_for_test(0);
    let rects = view.row_chip_rects_for_test(0);
    let rect_of = |kind| {
        chips
            .iter()
            .zip(&rects)
            .find(|(chip, _)| chip.kind == kind)
            .map(|(_, r)| *r)
            .expect("the chip")
    };

    let due = rect_of(koru_draw::chip::ChipKind::Due);
    press_at(&mut view, due);
    let (_, _, panel) = view.due_picker_for_test().expect("the calendar opened");
    let at = (due.x + due.w * 0.5, due.y + due.h * 0.5);
    // Hung from the pointer: just under it, and spanning it — the chips sit
    // at the row's right end, so the panel is pushed back inside the window
    // rather than starting exactly at the press.
    assert!(
        panel.x <= at.0 && at.0 <= panel.x + panel.w && panel.y >= at.1 && panel.y - at.1 < 24.0,
        "under the pointer, not the row's `…`: {panel:?} vs {at:?}"
    );
    koru_workspace_host::View::key(
        &mut view,
        &mut koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test").unwrap(),
        &Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty(),
    );

    press_at(&mut view, rect_of(koru_draw::chip::ChipKind::Swatch));
    let rows = view
        .color_picker_for_test()
        .expect("the colour picker opened");
    let blue = rows
        .iter()
        .position(|r| matches!(r, super::menu::ColorPick::Color { token: "blue", .. }))
        .expect("blue is offered");
    assert!(
        rows.iter().any(|r| matches!(
            r,
            super::menu::ColorPick::Color {
                token: "red",
                checked: true
            }
        )),
        "the line's own colour is checked"
    );
    assert!(view.choose_color(blue));
    assert_eq!(
        view.take_request(),
        Some(Request::SetColor {
            document,
            node,
            color: Some("blue".to_owned()),
        })
    );
}

// ------------------------------------------------- several rows at once

/// Press the middle of task `row`'s line with `mods` held.
fn press_row(view: &mut TasksView, row: usize, mods: ModifiersState) {
    let line = view.line_of_task_for_test(row).expect("a line");
    let r = view.line_rect_for_test(line);
    let p = view.parts_for_test(r).text;
    koru_workspace_host::View::set_mods(view, mods);
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    koru_workspace_host::View::mouse_down(
        view,
        &mut doc,
        p.x + 4.0,
        r.y + r.h * 0.5,
        mods.control_key(),
    );
    koru_workspace_host::View::set_mods(view, ModifiersState::empty());
}

fn five_rows() -> TasksView {
    view_on(
        RailTarget::Bucket(Bucket::Today),
        ["a", "b", "c", "d", "e"]
            .into_iter()
            .map(|t| entry(t, None))
            .collect(),
    )
}

/// **Ctrl+click adds a row or takes it out; Shift+click takes a range from
/// the focused row; a plain click is one row again.**
#[test]
fn ctrl_and_shift_click_build_a_selection() {
    let mut view = five_rows();
    press_row(&mut view, 1, ModifiersState::empty());
    press_row(&mut view, 3, ModifiersState::CONTROL);
    assert_eq!(view.selection(), [1, 3], "Ctrl adds");
    press_row(&mut view, 1, ModifiersState::CONTROL);
    assert_eq!(view.selection(), [3], "and takes out");

    press_row(&mut view, 0, ModifiersState::SHIFT);
    assert_eq!(
        view.selection(),
        [0, 1, 2, 3],
        "Shift ranges from the focused row"
    );
    press_row(&mut view, 4, ModifiersState::SHIFT);
    assert_eq!(
        view.selection(),
        [3, 4],
        "…from it again, not from the last press"
    );

    press_row(&mut view, 2, ModifiersState::empty());
    assert_eq!(view.selection(), [2], "a plain click is one row");
}

/// **Delete asks to take the selection off Tasks**, every selected task and
/// only those.
#[test]
fn delete_asks_to_remove_the_selection() {
    let mut view = five_rows();
    press_row(&mut view, 0, ModifiersState::empty());
    press_row(&mut view, 2, ModifiersState::CONTROL);
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    assert!(koru_workspace_host::View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::Delete),
        None,
        ModifiersState::empty(),
    ));
    let Some(Request::RemoveTasks { tasks }) = view.take_request() else {
        panic!("Delete asks to remove")
    };
    assert_eq!(tasks, view.selected_tasks());
    assert_eq!(tasks.len(), 2);
}

/// **A right-click inside the selection is the selection's menu**; outside
/// it, the row becomes the selection and gets its own.
#[test]
fn a_right_click_inside_a_selection_opens_the_selections_menu() {
    let mut view = five_rows();
    press_row(&mut view, 0, ModifiersState::empty());
    press_row(&mut view, 1, ModifiersState::CONTROL);
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    let at = |view: &TasksView, row: usize| {
        let r = view.line_rect_for_test(view.line_of_task_for_test(row).expect("a line"));
        (r.x + 120.0, r.y + r.h * 0.5)
    };
    let (x, y) = at(&view, 1);
    assert!(koru_workspace_host::View::context_menu(
        &mut view, &mut doc, x, y
    ));
    let Some(Request::RowsMenu {
        tasks, completed, ..
    }) = view.take_request()
    else {
        panic!("the selection's menu")
    };
    assert_eq!(tasks.len(), 2);
    assert!(!completed);

    let (x, y) = at(&view, 3);
    assert!(koru_workspace_host::View::context_menu(
        &mut view, &mut doc, x, y
    ));
    assert!(matches!(view.take_request(), Some(Request::RowMenu { .. })));
    assert_eq!(
        view.selection(),
        [3],
        "the row outside became the selection"
    );
}

/// **One pick, every selected row**: the selection's *Due* is the page's own
/// calendar, and a day picked in it is one edit per row, left as one batch.
#[test]
fn a_pick_for_a_selection_is_one_edit_per_row() {
    let mut view = five_rows();
    let tasks: Vec<_> = [0usize, 2, 4]
        .iter()
        .map(|&i| (view.rows[i].document, view.rows[i].node))
        .collect();
    assert!(view.open_due_picker_for(&tasks));
    let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 20).unwrap();
    assert!(view.pick_day_for_test(day));
    let Some(Request::Many(edits)) = view.take_request() else {
        panic!("one batch")
    };
    let want = Some(koru_core::time::Temporal::Date(
        koru_core::time::DateOnly::new(day),
    ));
    let rows: Vec<_> = edits
        .iter()
        .map(|edit| match edit {
            Request::SetDue {
                document,
                node,
                due,
            } => {
                assert_eq!(*due, want);
                (*document, *node)
            }
            other => panic!("a due edit, not {other:?}"),
        })
        .collect();
    assert_eq!(rows, tasks, "one per selected row, in the page's order");
}

/// **Adding a task leaves the caret in the capture row**, emptied, so a run
/// of tasks is one Enter after another.
#[test]
fn adding_a_task_leaves_the_caret_in_an_empty_capture_row() {
    let mut view = view_on(RailTarget::Bucket(Bucket::Today), Vec::new());
    type_into_capture(&mut view, "Test");
    enter(&mut view);
    assert!(
        matches!(view.take_request(), Some(Request::Capture { .. })),
        "Enter asked for the task"
    );
    assert!(view.capturing_for_test(), "the caret is still in the row");
    assert_eq!(
        view.capture_value_for_test().as_deref(),
        Some(""),
        "and the row is empty for the next one"
    );
    enter(&mut view);
    assert_eq!(view.take_request(), None, "an empty row asks for nothing");
}

/// The capture row, pressed and typed into.
fn type_into_capture(view: &mut TasksView, words: &str) {
    let r = view.line_rect_for_test(0);
    press_at(view, r);
    let mut doc = koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test")
        .expect("an in-memory document");
    for ch in words.chars() {
        let text = ch.to_string();
        koru_workspace_host::View::key(
            view,
            &mut doc,
            &Key::Character(text.clone().into()),
            Some(&text),
            ModifiersState::empty(),
        );
    }
}

// --------------------------------------- the Completed window (round nine)

/// **The page asks for a window.**
///
/// Completed is a log of recent work rather than an archive, so the query
/// carries the day the window starts on — computed from the page's own today
/// and the wire crate's one constant, so the page and the rail's number are
/// asking the same question. Older work is reached through search, with
/// `/task_state completed`.
#[test]
fn the_page_asks_for_a_completed_window() {
    let view = view_on(RailTarget::Bucket(Bucket::Completed), Vec::new());
    let since = query_of(&view)
        .completed_on_or_after
        .expect("the page names where the window starts");
    assert_eq!(
        since,
        query::completed_since(today()),
        "one boundary, from the page's own day"
    );
    let start = chrono::NaiveDate::parse_from_str(&since, "%Y-%m-%d").expect("a canonical day");
    assert_eq!(
        (today() - start).num_days(),
        koru_store_wire::tasks::COMPLETED_WINDOW_DAYS,
        "and it is the window the wire crate names"
    );

    // The rail's counts ask the same thing, or its number would not be the
    // number the page draws.
    let counts = crate::mainview::taskcounts::bucket_query(
        koru_store_core::ids::WorkspaceId::new(),
        &query::windows(today(), now_ms()),
        today(),
    );
    assert_eq!(
        counts.completed_on_or_after.as_deref(),
        Some(since.as_str())
    );
}

/// **Add to Today reaches the seventh canonical command**, through the one
/// queue every other Tasks write uses. The row's own state decides which
/// direction the menu offered, and the command carries it.
#[test]
fn the_pin_row_dispatches_the_canonical_command_in_both_directions() {
    use crate::mainview::context_menu::MenuAction;
    for (pinned_now, action, sent) in [
        (false, MenuAction::TaskPinToday, true),
        (true, MenuAction::TaskUnpinToday, false),
    ] {
        let (_, items) = super::row_menu(
            DocumentId::new(),
            NodeId::new(),
            false,
            false,
            None,
            pinned_now,
        );
        assert!(items.contains(&action), "pinned_now={pinned_now}");
        // The direction the command would carry is the row's state inverted,
        // which is what makes one variant serve both ends.
        assert_eq!(matches!(action, MenuAction::TaskPinToday), sent);
    }
}

// ------------------------------------------------- the manual order (drag)

fn ranked(title: &str, rank: Option<i64>) -> TaskEntry {
    let attributes = rank
        .map(|r| {
            vec![attr(
                koru_core::attributes::KEY_RANK,
                "number",
                &r.to_string(),
            )]
        })
        .unwrap_or_default();
    TaskEntry::of(&wire_row(title, attributes), today(), now_ms()).expect("a parseable row")
}

fn list_page(rows: Vec<TaskEntry>) -> TasksView {
    view_on(RailTarget::List(koru_core::ids::ListId::new()), rows)
}

fn row_rect(view: &TasksView, row: usize) -> Rect {
    let line = view
        .line_of_task_for_test(row)
        .expect("every task earns a line");
    view.line_rect_for_test(line)
}

/// Where a press on `row` lands: in its words, clear of every control.
fn press_point(view: &TasksView, row: usize) -> (f32, f32) {
    let t = view.parts_for_test(row_rect(view, row)).text;
    (t.x + 4.0, t.y + t.h * 0.5)
}

fn titles(view: &TasksView) -> Vec<String> {
    view.rows.iter().map(|row| row.title.clone()).collect()
}

fn a_doc() -> koru_core::schema::WorkspaceDoc {
    koru_core::schema::WorkspaceDoc::create(WorkspaceId::new(), "Test").expect("a document")
}

/// Press `row`, move past the threshold, and release at `to_y`.
fn drag_row(view: &mut TasksView, row: usize, to_y: f32) {
    let mut doc = a_doc();
    let (x, y) = press_point(view, row);
    assert!(koru_workspace_host::View::mouse_down(
        view, &mut doc, x, y, false
    ));
    koru_workspace_host::View::mouse_move(view, x, y + 1.0);
    koru_workspace_host::View::mouse_move(view, x, to_y);
    koru_workspace_host::View::mouse_up(view, &mut doc);
}

/// **A press on a row arms a drag on a list ordered by hand, and only
/// there** — no grip: the row is what is held. A queue has no order of its
/// **Every page's rows can be carried; only a page ordered by hand takes
/// them back in a new place.** A press arms a drag anywhere (a queue's rows
/// can be dropped on a list in the sidebar), but a queue by due date and a
/// list by due offer no gap and ask for no rank.
#[test]
fn a_press_arms_a_drag_everywhere_but_only_a_manual_page_reorders() {
    let mut doc = a_doc();
    let mut list = list_page(vec![ranked("A", None), ranked("B", None)]);
    let at = press_point(&list, 0);
    koru_workspace_host::View::mouse_down(&mut list, &mut doc, at.0, at.1, false);
    assert!(list.drag.is_some(), "armed");
    koru_workspace_host::View::mouse_up(&mut list, &mut doc);
    assert!(list.drag.is_none());
    assert_eq!(list.selection(), [0], "a press that never moved is a click");

    let mut queue = view_on(
        RailTarget::Bucket(Bucket::Today),
        vec![entry("A", None), entry("B", None)],
    );
    let top = row_rect(&queue, 0);
    drag_row(&mut queue, 1, top.y + 2.0);
    assert_eq!(queue.take_request(), None, "by due date: no new place");
    assert_eq!(titles(&queue), ["A", "B"]);
}

/// **A press on the page's empty space clears the selection** — a whole
/// Ctrl+click selection, not only the last row.
#[test]
fn a_press_on_empty_space_unselects_every_task() {
    let mut doc = a_doc();
    let mut view = view_on(
        RailTarget::Bucket(Bucket::Today),
        vec![entry("A", None), entry("B", None)],
    );
    let a = press_point(&view, 0);
    koru_workspace_host::View::mouse_down(&mut view, &mut doc, a.0, a.1, false);
    koru_workspace_host::View::mouse_up(&mut view, &mut doc);
    let b = press_point(&view, 1);
    koru_workspace_host::View::mouse_down(&mut view, &mut doc, b.0, b.1, true);
    assert_eq!(view.selection(), [0, 1]);
    let below = row_rect(&view, 1);
    let (x, y) = (below.x + below.w * 0.5, below.y + below.h + 120.0);
    assert_eq!(view.hit_for_test(x, y), None, "empty space");
    assert!(koru_workspace_host::View::mouse_down(
        &mut view, &mut doc, x, y, false
    ));
    assert!(view.selection().is_empty());
}

/// **Carried rows dropped on a list join it** — the selection, each task
/// once — and while they are over the sidebar the page offers no gap.
#[test]
fn carried_rows_dropped_on_a_list_join_it() {
    let mut doc = a_doc();
    let mut view = view_on(
        RailTarget::Bucket(Bucket::Today),
        vec![entry("A", None), entry("B", None)],
    );
    let (x, y) = press_point(&view, 0);
    koru_workspace_host::View::mouse_down(&mut view, &mut doc, x, y, false);
    koru_workspace_host::View::mouse_move(&mut view, x, y + 10.0);
    assert!(view.carrying_rows());
    assert!(view.set_rows_outside(true));
    koru_workspace_host::View::mouse_move(&mut view, x, y + 12.0);
    assert_eq!(
        view.drag.as_ref().and_then(|d| d.gap),
        None,
        "no gap over the sidebar"
    );
    let bike = koru_core::ids::ListId::new();
    assert!(view.drop_rows_into_list(bike));
    assert!(view.drag.is_none());
    match view.take_request() {
        Some(Request::SetList { list, .. }) => assert_eq!(list, Some(bike)),
        other => panic!("one list change: {other:?}"),
    }
}

/// **A click inside a selection of several narrows it to that row** — but
/// only on release, so a press there can still drag the whole selection.
#[test]
fn a_click_in_a_selection_narrows_it_on_release() {
    let mut doc = a_doc();
    let mut view = list_page(vec![
        ranked("A", None),
        ranked("B", None),
        ranked("C", None),
    ]);
    let a = press_point(&view, 0);
    koru_workspace_host::View::mouse_down(&mut view, &mut doc, a.0, a.1, false);
    koru_workspace_host::View::mouse_up(&mut view, &mut doc);
    let c = press_point(&view, 2);
    koru_workspace_host::View::mouse_down(&mut view, &mut doc, c.0, c.1, true);
    assert_eq!(view.selection(), [0, 2]);
    koru_workspace_host::View::mouse_down(&mut view, &mut doc, a.0, a.1, false);
    assert_eq!(view.selection(), [0, 2], "kept whole while held");
    koru_workspace_host::View::mouse_up(&mut view, &mut doc);
    assert_eq!(view.selection(), [0]);
}

/// **A double click renames the task in place** — it never opens the
/// hidden tasks document. Enter keeps the new words and asks for them once;
/// the row shows them at once.
#[test]
fn a_double_click_renames_the_task_in_place() {
    let mut doc = a_doc();
    let mut view = list_page(vec![ranked("Old name", None)]);
    let at = press_point(&view, 0);
    assert!(koru_workspace_host::View::mouse_double_click(
        &mut view, &doc, at.0, at.1
    ));
    assert!(view.rename.is_some());
    assert_eq!(view.take_request(), None, "nothing opened");
    for _ in 0.."name".len() {
        koru_workspace_host::View::key(
            &mut view,
            &mut doc,
            &Key::Named(NamedKey::Backspace),
            None,
            ModifiersState::empty(),
        );
    }
    koru_workspace_host::View::key(
        &mut view,
        &mut doc,
        &Key::Character("task".into()),
        Some("task"),
        ModifiersState::empty(),
    );
    koru_workspace_host::View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    );
    assert!(view.rename.is_none());
    let Some(Request::Retitle { title, .. }) = view.take_request() else {
        panic!("a rename");
    };
    assert_eq!(title, "Old task");
    assert_eq!(titles(&view), ["Old task"], "drawn at once");
}

/// Escape puts the old words back, and an unchanged Enter asks for nothing.
#[test]
fn escape_or_no_change_renames_nothing() {
    let mut doc = a_doc();
    let mut view = list_page(vec![ranked("Same", None)]);
    assert!(view.begin_rename(0));
    koru_workspace_host::View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    );
    assert_eq!(view.take_request(), None);
    assert!(view.begin_rename(0));
    koru_workspace_host::View::key(
        &mut view,
        &mut doc,
        &Key::Character("x".into()),
        Some("x"),
        ModifiersState::empty(),
    );
    koru_workspace_host::View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty(),
    );
    assert!(view.rename.is_none());
    assert_eq!(view.take_request(), None);
    assert_eq!(titles(&view), ["Same"]);
}

/// **A drag up puts the row where it was dropped**, at once, and asks for the
/// one rank that says so — the neighbours keep theirs.
#[test]
fn dragging_a_row_up_moves_it_and_writes_one_rank() {
    use super::rank::GAP;
    let mut view = list_page(vec![
        ranked("A", Some(GAP)),
        ranked("B", Some(2 * GAP)),
        ranked("C", Some(3 * GAP)),
    ]);
    let top = row_rect(&view, 0);
    drag_row(&mut view, 2, top.y + 2.0);
    let request = view.take_request().expect("the drop asked for a rank");
    let Request::SetRank {
        rank: Some(rank), ..
    } = request
    else {
        panic!("one rank, for the moved row: {request:?}");
    };
    assert!(rank < GAP);
    assert_eq!(titles(&view), ["C", "A", "B"], "drawn where it was dropped");
}

/// **A selection moves together**, keeping its own order — Explore's
/// multi-drag, on a list page. The first drag on a list nobody ordered ranks
/// only what it has to; the rest follow by due.
#[test]
fn a_selection_is_dragged_together_in_its_order() {
    let mut view = list_page(vec![
        ranked("A", None),
        ranked("B", None),
        ranked("C", None),
        ranked("D", None),
    ]);
    let mut doc = a_doc();
    // Select B, then Ctrl+click D.
    let b = view.parts_for_test(row_rect(&view, 1)).text;
    koru_workspace_host::View::mouse_down(&mut view, &mut doc, b.x + 4.0, b.y + b.h * 0.5, false);
    koru_workspace_host::View::mouse_up(&mut view, &mut doc);
    let d = view.parts_for_test(row_rect(&view, 3)).text;
    koru_workspace_host::View::mouse_down(&mut view, &mut doc, d.x + 4.0, d.y + d.h * 0.5, true);
    assert_eq!(view.selection(), [1, 3]);

    let top = row_rect(&view, 0);
    drag_row(&mut view, 1, top.y + 2.0);
    let Some(Request::Many(requests)) = view.take_request() else {
        panic!("one rank per moved row");
    };
    assert_eq!(requests.len(), 2);
    assert_eq!(titles(&view), ["B", "D", "A", "C"]);
}

/// **Escape takes a drag back**: nothing is asked for and nothing moves.
#[test]
fn escape_cancels_a_drag() {
    let mut view = list_page(vec![ranked("A", None), ranked("B", None)]);
    let mut doc = a_doc();
    let (x, y) = press_point(&view, 1);
    koru_workspace_host::View::mouse_down(&mut view, &mut doc, x, y, false);
    let up = row_rect(&view, 0).y + 2.0;
    koru_workspace_host::View::mouse_move(&mut view, x, up);
    assert!(koru_workspace_host::View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty(),
    ));
    koru_workspace_host::View::mouse_up(&mut view, &mut doc);
    assert_eq!(view.take_request(), None);
    assert_eq!(titles(&view), ["A", "B"]);
}

/// **A list's menu offers its manual order first, and it is the one
/// checked**; a queue's menu never offers it.
#[test]
fn a_list_menu_leads_with_the_manual_order() {
    let list = RailTarget::List(koru_core::ids::ListId::new());
    let items = overflow_items(&Filters::default(), list);
    let sorts: Vec<_> = items
        .iter()
        .filter_map(|item| match item.row {
            OverflowRow::Sort(sort) => Some((sort, item.checked)),
            _ => None,
        })
        .collect();
    assert_eq!(sorts[0], (TaskSort::Manual, Some(true)));
    assert!(
        sorts[1..]
            .iter()
            .all(|(_, checked)| *checked == Some(false))
    );
    // A queue offers it last, unchecked — it opens by due date — and the
    // archive never offers it.
    let queue = overflow_items(&Filters::default(), RailTarget::Bucket(Bucket::Today));
    let last = queue.last().expect("rows");
    assert_eq!(last.row, OverflowRow::Sort(TaskSort::Manual));
    assert_eq!(last.checked, Some(false));
    let archive = overflow_items(&Filters::default(), RailTarget::Bucket(Bucket::Completed));
    assert!(
        !archive
            .iter()
            .any(|item| item.row == OverflowRow::Sort(TaskSort::Manual))
    );
}

/// **A queue set to Manual is dragged like a list** (the owner's ask,
/// 2026-11-05): a press arms a drag on Later once Manual is chosen, never on
/// Later by due date, and never on Completed.
#[test]
fn a_queue_set_to_manual_can_be_dragged() {
    let mut doc = a_doc();
    let rows = || vec![entry("A", None), entry("B", None)];
    let later = view_on(RailTarget::Bucket(Bucket::Later), rows());
    assert!(!later.manual_order(), "by due date: no order of its own");

    let mut manual = view_on(RailTarget::Bucket(Bucket::Later), rows());
    let mut filters = Filters::default();
    filters.set_sort_for(RailTarget::Bucket(Bucket::Later), TaskSort::Manual);
    manual.filters = filters.clone();
    assert!(manual.manual_order());
    let at = press_point(&manual, 0);
    koru_workspace_host::View::mouse_down(&mut manual, &mut doc, at.0, at.1, false);
    assert!(manual.drag.is_some(), "Manual: armed");

    let mut archive = view_on(RailTarget::Bucket(Bucket::Completed), Vec::new());
    archive.filters = filters;
    assert!(!archive.manual_order(), "the archive has no order to drag");
}

/// **A task typed into a list ordered by hand goes on top of it.**
#[test]
fn a_task_typed_into_a_ranked_list_goes_on_top() {
    use super::rank::GAP;
    let mut view = list_page(vec![ranked("A", Some(5 * GAP)), ranked("B", Some(6 * GAP))]);
    let mut doc = a_doc();
    let r = view.line_rect_for_test(0);
    koru_workspace_host::View::mouse_down(
        &mut view,
        &mut doc,
        r.x + r.w * 0.5,
        r.y + r.h * 0.5,
        false,
    );
    for ch in "New".chars() {
        let text = ch.to_string();
        koru_workspace_host::View::key(
            &mut view,
            &mut doc,
            &Key::Character(text.clone().into()),
            Some(&text),
            ModifiersState::empty(),
        );
    }
    koru_workspace_host::View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    );
    let Some(Request::Capture { rank, .. }) = view.take_request() else {
        panic!("a capture");
    };
    assert_eq!(rank, Some(4 * GAP));
    assert_eq!(titles(&view)[0], "New", "drawn on top at once");
}

// ------------------------------------------------------------- the note

mod note {
    use super::*;
    use koru_core::ids::{ListId, PageKey, page_document_id};
    use koru_core::schema::{WorkspaceDoc, genesis_seed_id};
    use koru_workspace::pages::{PagePhase, PageState};
    use vello::kurbo::Affine;
    use winit::window::CursorIcon;

    fn page() -> (RailTarget, PageKey, DocumentId, WorkspaceDoc) {
        let list = ListId::new();
        let workspace = WorkspaceId::new();
        let page = PageKey::List(list);
        let document = page_document_id(workspace, page, 0);
        let mut doc = WorkspaceDoc::page_genesis(workspace, document).unwrap();
        doc.stamp_document_id(document).unwrap();
        doc.enable_undo();
        (RailTarget::List(list), page, document, doc)
    }

    fn state(page: PageKey, document: DocumentId, phase: PagePhase) -> PageState {
        PageState {
            page,
            document,
            generation: 0,
            phase,
            conflict: false,
        }
    }

    fn conflict_state(page: PageKey, document: DocumentId) -> PageState {
        PageState {
            page,
            document,
            generation: 0,
            phase: PagePhase::Empty,
            conflict: true,
        }
    }

    /// §6.2: a note in a deletion conflict shows the band with *Keep
    /// deleted* and *Recover*, and a press on each routes the person's
    /// answer out.
    #[test]
    fn a_conflicted_note_offers_keep_deleted_and_recover() {
        use koru_workspace::pages::PageDecision;
        let (target, page, document, mut doc) = page();
        let mut view = TasksView::new(target);
        view.set_viewport(900.0, 600.0);
        assert!(view.set_page_state(&conflict_state(page, document)));
        view.catch_up(&doc);
        assert!(view.rows_top() > view.header_h, "the band takes a row");
        let mut scene = vello::Scene::new();
        view.layout_band();
        view.paint_notice(&mut scene, Affine::IDENTITY);
        let (keep, recover) = view.conflict_actions_for_test().expect("drawn");
        assert!(view.take_wants_decision().is_none());
        assert!(view.mouse_down(&mut doc, recover.x + 1.0, recover.y + 1.0, false));
        assert_eq!(view.take_wants_decision(), Some(PageDecision::Recover));
        assert!(view.mouse_down(&mut doc, keep.x + 1.0, keep.y + 1.0, false));
        assert_eq!(view.take_wants_decision(), Some(PageDecision::Keep));
        assert!(view.take_wants_decision().is_none(), "drained");
    }

    /// A page's state names the document the tab edits (D7), and an open
    /// still in flight names none to activate.
    #[test]
    fn the_page_state_names_what_the_tab_edits() {
        let (target, page, document, _) = page();
        let mut view = TasksView::new(target);
        assert_eq!(view.page_document(), None);
        assert!(view.set_page_state(&state(page, document, PagePhase::Loading)));
        assert_eq!(view.page_document(), Some(document));
        assert!(!view.page_is_local(), "still opening: nothing to activate");
        assert!(view.set_page_state(&state(page, document, PagePhase::Empty)));
        assert!(view.page_is_local());
        assert!(
            !view.set_page_state(&state(page, document, PagePhase::Empty)),
            "no change"
        );
        let other = PageKey::List(ListId::new());
        assert!(
            !view.set_page_state(&state(other, document, PagePhase::Empty)),
            "not this page"
        );
    }

    /// The note is built only over its own document, and the rows move
    /// down by its band (D8).
    #[test]
    fn the_note_is_built_over_its_own_document_and_moves_the_rows_down() {
        let (target, page, document, doc) = page();
        let mut view = TasksView::new(target);
        view.set_viewport(900.0, 600.0);
        view.set_page_state(&state(page, document, PagePhase::Empty));

        let stranger = WorkspaceDoc::create(WorkspaceId::new(), "other").unwrap();
        view.catch_up(&stranger);
        assert!(view.body.is_none(), "a stranger document builds nothing");

        assert!(view.catch_up(&doc), "built over its own document");
        assert!(view.body.is_some());
        let top_before = view.rows_top();
        view.layout_band();
        assert!(view.rows_top() > view.header_h, "the rows moved down");
        assert!(view.rows_top() >= top_before);
        assert!(
            view.rows_top() - view.header_h <= 600.0 * 0.4 + 1.0,
            "never past its share"
        );
    }

    /// A press in the note takes the keyboard, typing lands in the document,
    /// and Escape hands the keyboard back to the table.
    #[test]
    fn a_press_in_the_note_takes_the_keyboard_and_escape_hands_it_back() {
        let (target, page, document, mut doc) = page();
        let mut view = TasksView::new(target);
        view.set_viewport(900.0, 600.0);
        view.set_page_state(&state(page, document, PagePhase::Empty));
        view.catch_up(&doc);
        view.layout_band();
        let band = view.body.as_ref().unwrap().band;
        let mods = ModifiersState::empty();

        assert!(view.mouse_down(
            &mut doc,
            band.x + band.w * 0.5,
            band.y + band.h * 0.5,
            false
        ));
        assert!(view.body_is_focused_in(&doc));
        view.key(&mut doc, &Key::Character("a".into()), Some("a"), mods);
        let seed = genesis_seed_id(document);
        assert_eq!(doc.read(seed).unwrap().content.as_text(), Some("a"));

        assert!(view.key(&mut doc, &Key::Named(NamedKey::Escape), None, mods));
        assert!(
            !view.body_is_focused_in(&doc),
            "Escape hands the keyboard back"
        );
        view.key(&mut doc, &Key::Character("b".into()), Some("b"), mods);
        assert_eq!(
            doc.read(seed).unwrap().content.as_text(),
            Some("a"),
            "the table has the keyboard now"
        );
    }

    /// Rung 3 (P2): a bucket page — Completed, which has no capture row —
    /// takes the same note, over its own document.
    #[test]
    fn the_note_is_built_on_a_bucket_page_too() {
        let workspace = WorkspaceId::new();
        let page = PageKey::Bucket(Bucket::Completed);
        let document = page_document_id(workspace, page, 0);
        let mut doc = WorkspaceDoc::page_genesis(workspace, document).unwrap();
        doc.stamp_document_id(document).unwrap();
        doc.enable_undo();
        let mut view = TasksView::new(RailTarget::Bucket(Bucket::Completed));
        view.set_viewport(900.0, 600.0);
        assert_eq!(view.page_key(), Some(page));
        view.set_page_state(&state(page, document, PagePhase::Empty));
        assert!(view.catch_up(&doc));
        view.layout_band();
        assert!(view.rows_top() > view.header_h);
        let band = view.body.as_ref().unwrap().band;
        assert!(view.mouse_down(&mut doc, band.x + 5.0, band.y + 5.0, false));
        assert!(view.body_is_focused_in(&doc), "the press focused the note");
        let caret = view.body.as_ref().unwrap().leaf().caret_spot();
        assert!(
            caret.is_some(),
            "the caret arrived: {caret:?} band {band:?}"
        );
        let took = view.key(
            &mut doc,
            &Key::Character("c".into()),
            Some("c"),
            ModifiersState::empty(),
        );
        assert!(took, "the note took the key");
        assert_eq!(
            doc.read(genesis_seed_id(document))
                .unwrap()
                .content
                .as_text(),
            Some("c")
        );
    }

    /// Full document parity (P3): a picker opened in the note floats over
    /// the rows, and a press on the page while it is open is the note's —
    /// here, dismissing it — rather than the table's.
    #[test]
    fn a_picker_opened_in_the_note_takes_the_next_press_wherever_it_lands() {
        let (target, page, document, mut doc) = page();
        let mut view = TasksView::new(target);
        view.set_viewport(900.0, 600.0);
        view.set_page_state(&state(page, document, PagePhase::Empty));
        view.catch_up(&doc);
        view.layout_band();
        let band = view.body.as_ref().unwrap().band;
        let mods = ModifiersState::empty();
        view.mouse_down(
            &mut doc,
            band.x + band.w * 0.5,
            band.y + band.h * 0.5,
            false,
        );
        view.key(&mut doc, &Key::Character("/".into()), Some("/"), mods);
        assert!(
            view.body.as_ref().unwrap().overlay_open(),
            "the slash menu opened in the note"
        );
        // Far below the band, where a row would be.
        let took = view.mouse_down(&mut doc, 300.0, 560.0, false);
        assert!(took);
        assert!(
            !view.body.as_ref().unwrap().overlay_open(),
            "the press was the note's: the menu closed"
        );
        assert!(view.selected_row().is_none(), "and no row was selected");
    }

    /// Rung 6 (P4): a task made in a list's note joins the list, in the
    /// same step as the command that made it.
    #[test]
    fn a_task_made_in_a_lists_note_joins_the_list() {
        let (target, page, document, mut doc) = page();
        let RailTarget::List(list) = target else {
            unreachable!()
        };
        let mut view = TasksView::new(target);
        view.set_viewport(900.0, 600.0);
        view.set_page_state(&state(page, document, PagePhase::Empty));
        view.catch_up(&doc);
        view.layout_band();
        let band = view.body.as_ref().unwrap().band;
        let mods = ModifiersState::empty();
        view.mouse_down(
            &mut doc,
            band.x + band.w * 0.5,
            band.y + band.h * 0.5,
            false,
        );
        for ch in ["/", "t", "a", "s", "k"] {
            view.key(&mut doc, &Key::Character(ch.into()), Some(ch), mods);
        }
        view.key(&mut doc, &Key::Named(NamedKey::Enter), None, mods);
        let seed = genesis_seed_id(document);
        let node = doc.read(seed).unwrap();
        assert!(
            koru_core::tasks::is_task(&koru_core::tasks::TaskFields::of(&node)),
            "the line became a task"
        );
        let in_list = node.content.attrs().iter().any(|a| {
            a.role == koru_core::attributes::KEY_LIST
                && a.value == koru_core::node::AttrValue::Text(list.to_storage())
        });
        assert!(in_list, "and joined the list");
        doc.undo().unwrap();
        let node = doc.read(seed).unwrap();
        assert!(
            !node
                .content
                .attrs()
                .iter()
                .any(|a| a.role == koru_core::attributes::KEY_LIST),
            "one undo takes the membership out with the task"
        );
    }

    /// Rung 6 (P4): typing on a task's line asks the rows again once it
    /// settles; typing prose asks nothing and remeasures nothing.
    #[test]
    fn typing_on_a_task_asks_the_rows_again_and_prose_does_not() {
        let (target, page, document, mut doc) = page();
        let mut view = TasksView::new(target);
        view.set_viewport(900.0, 600.0);
        view.set_page_state(&state(page, document, PagePhase::Empty));
        view.catch_up(&doc);
        view.layout_band();
        let band = view.body.as_ref().unwrap().band;
        let mods = ModifiersState::empty();
        view.mouse_down(
            &mut doc,
            band.x + band.w * 0.5,
            band.y + band.h * 0.5,
            false,
        );
        view.key(&mut doc, &Key::Character("p".into()), Some("p"), mods);
        assert_eq!(view.task_touch_deadline(), None, "prose asks nothing");
        assert!(!view.is_stale());

        let seed = genesis_seed_id(document);
        koru_core::tasks::ensure_task(&mut doc, seed, koru_core::attributes::TaskReason::Explicit)
            .unwrap();
        view.key(&mut doc, &Key::Character("q".into()), Some("q"), mods);
        let due = view
            .task_touch_deadline()
            .expect("a task's line was typed on");
        assert!(
            !view.flush_task_touch(std::time::Instant::now()),
            "not yet settled"
        );
        assert!(view.flush_task_touch(due + std::time::Duration::from_millis(1)));
        assert!(view.is_stale(), "the rows are asked again");
        assert_eq!(view.task_touch_deadline(), None, "once");
    }

    /// A note that failed to register shows its notice with *Try again*,
    /// and a press on it asks for the retry (plan Q8).
    #[test]
    fn a_failed_note_offers_try_again() {
        let (target, page, document, mut doc) = page();
        let mut view = TasksView::new(target);
        view.set_viewport(900.0, 600.0);
        view.set_page_state(&state(
            page,
            document,
            PagePhase::Failed {
                why: "no store".into(),
                opening: false,
            },
        ));
        view.catch_up(&doc);
        assert!(view.rows_top() > view.header_h, "the notice takes a row");
        let mut scene = vello::Scene::new();
        view.paint_notice(&mut scene, Affine::IDENTITY);
        let (_, retry) = view.notice.expect("drawn");
        assert!(!view.take_wants_retry());
        assert!(view.mouse_down(&mut doc, retry.x + 1.0, retry.y + 1.0, false));
        assert!(view.take_wants_retry());
        assert!(!view.take_wants_retry(), "drained");
    }

    fn note_view() -> (
        RailTarget,
        koru_core::ids::ListId,
        DocumentId,
        WorkspaceDoc,
        TasksView,
    ) {
        let (target, page, document, doc) = page();
        let RailTarget::List(list) = target else {
            unreachable!()
        };
        let mut view = TasksView::new(target);
        view.set_viewport(900.0, 600.0);
        view.set_page_state(&state(page, document, PagePhase::Empty));
        view.catch_up(&doc);
        view.layout_band();
        (target, list, document, doc, view)
    }

    /// Review note: the band's top-right corner opens the note's document
    /// in a tab of its own, and lights under the pointer as a control.
    #[test]
    fn the_note_opens_in_a_new_tab_from_its_corner() {
        let (_, _, document, mut doc, mut view) = note_view();
        let open = view.body.as_ref().unwrap().open_rect();
        assert!(open.w > 0.0 && open.h > 0.0, "the control has a place");
        let (x, y) = (open.x + open.w * 0.5, open.y + open.h * 0.5);
        assert!(view.mouse_move(x, y));
        assert_eq!(view.cursor_icon(), koru_ui::cursor::PRESS);
        assert!(view.mouse_down(&mut doc, x, y, false));
        assert_eq!(view.take_request(), Some(Request::OpenNote { document }));
        assert!(
            !view.body.as_ref().unwrap().focused,
            "the press was the control's, not the note's"
        );
    }

    /// Review note: the band's grip sizes it by hand — taller than its
    /// content, scrolling when shorter — and a double click on the grip
    /// gives it back to its content.
    #[test]
    fn the_band_is_sized_by_its_grip_and_given_back_by_a_double_click() {
        let (_, _, _, mut doc, mut view) = note_view();
        let before = view.rows_top() - view.header_h;
        assert!(
            before > 0.0 && before < 100.0,
            "one line's band to begin with"
        );
        let grip = view.body.as_ref().unwrap().grip_rect();
        let (x, y) = (grip.x + grip.w * 0.5, grip.y + grip.h * 0.5);
        assert!(view.mouse_move(x, y));
        assert_eq!(view.cursor_icon(), CursorIcon::RowResize);
        assert!(view.mouse_down(&mut doc, x, y, false));
        assert!(view.mouse_move(x, y + 150.0));
        assert_eq!(view.cursor_icon(), CursorIcon::RowResize, "while dragging");
        assert!(view.mouse_up(&mut doc));
        view.layout_band();
        let after = view.rows_top() - view.header_h;
        assert!(
            (after - (before + 150.0)).abs() < 2.0,
            "the band followed the pointer: {before} -> {after}"
        );
        assert!(view.band_fixed_h_for_test().is_some());

        let grip = view.body.as_ref().unwrap().grip_rect();
        assert!(view.mouse_double_click(&doc, grip.x + 10.0, grip.y + grip.h * 0.5));
        view.layout_band();
        assert!(view.band_fixed_h_for_test().is_none());
        assert!(
            ((view.rows_top() - view.header_h) - before).abs() < 0.5,
            "back to its content"
        );
    }

    /// Acceptance round 2: a drag can give the note more room, never less
    /// than the band has on its own — squeezed to a sliver it showed a
    /// line's top edge and nothing to read.
    #[test]
    fn the_band_is_never_dragged_shorter_than_its_content() {
        let (_, _, _, mut doc, mut view) = note_view();
        let natural = view.rows_top() - view.header_h;
        let grip = view.body.as_ref().unwrap().grip_rect();
        let (x, y) = (grip.x + grip.w * 0.5, grip.y + grip.h * 0.5);
        assert!(view.mouse_down(&mut doc, x, y, false));
        assert!(view.mouse_move(x, y - natural * 2.0));
        assert!(view.mouse_up(&mut doc));
        view.layout_band();
        assert!(view.band_fixed_h_for_test().is_some(), "a height was given");
        assert!(
            ((view.rows_top() - view.header_h) - natural).abs() < 0.5,
            "and it is floored at the band's own: {natural}"
        );
        // Typing a line while the band is held at its floor: the floor
        // follows the content, as it does with no height given.
        let band = view.body.as_ref().unwrap().band;
        view.mouse_down(
            &mut doc,
            band.x + band.w * 0.5,
            band.y + band.h * 0.5,
            false,
        );
        view.key(
            &mut doc,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        view.layout_band();
        assert!(
            view.rows_top() - view.header_h > natural + 1.0,
            "a second line makes the band taller"
        );
    }

    /// Acceptance round 2: a right-click in the note opens the editor's
    /// own menu over it, as in a document tab, and takes the keyboard.
    #[test]
    fn a_right_click_in_the_note_opens_the_editors_menu() {
        let (_, _, _, mut doc, mut view) = note_view();
        let band = view.body.as_ref().unwrap().band;
        assert!(view.context_menu(&mut doc, band.x + band.w * 0.5, band.y + band.h * 0.5));
        let body = view.body.as_ref().unwrap();
        assert!(body.focused, "the keyboard went with it");
        assert!(body.menu_open(), "the editor's menu is up");
    }

    /// Review note: a note taller than its band shows the thumb a document
    /// tab shows, and the band is what it scrolls in.
    #[test]
    fn a_tall_note_scrolls_in_its_band_and_wears_a_thumb() {
        let (_, _, _, mut doc, mut view) = note_view();
        let band = view.body.as_ref().unwrap().band;
        let mods = ModifiersState::empty();
        view.mouse_down(
            &mut doc,
            band.x + band.w * 0.5,
            band.y + band.h * 0.5,
            false,
        );
        for _ in 0..30 {
            view.key(&mut doc, &Key::Named(NamedKey::Enter), None, mods);
        }
        view.layout_band();
        let cap = 600.0 * 0.4;
        let h = view.rows_top() - view.header_h;
        assert!((h - cap).abs() < 1.0, "capped at the page's share: {h}");
        let leaf = view.body_leaf_mut().expect("the note");
        let content = koru_editor::rowlayout::content_height(koru_editor::Editing::body(leaf));
        assert!(content > cap, "taller than the band");
        assert!(
            koru_editor::Editing::view(leaf)
                .scrollbar_geom(content)
                .is_some(),
            "so the thumb is drawn"
        );
    }

    /// Review note: the heading's colour square opens the list's colour
    /// menu at the press — the rail row's *Color*, one press nearer.
    #[test]
    fn the_colour_square_opens_the_lists_colour_menu() {
        let (_, list, _, mut doc, mut view) = note_view();
        assert!(view.swatch_rect().is_some(), "a list page wears the square");
        view.set_lists(vec![crate::tasks_view::ListFace {
            id: list,
            name: "Top".to_owned(),
            color: Some("teal".to_owned()),
        }]);
        let swatch = view.swatch_rect().unwrap();
        let (x, y) = (swatch.x + swatch.w * 0.5, swatch.y + swatch.h * 0.5);
        assert!(view.mouse_move(x, y));
        assert_eq!(view.cursor_icon(), koru_ui::cursor::PRESS);
        assert!(view.mouse_down(&mut doc, x, y, false));
        assert_eq!(
            view.take_request(),
            Some(Request::ListColor { list, at: (x, y) })
        );
    }

    /// Review note: a double click on a list page's title renames the list
    /// in place — Enter keeps the words, Escape puts the old ones back, and
    /// a sibling's name is refused.
    #[test]
    fn a_double_click_on_the_title_renames_the_list_in_place() {
        let (_, list, _, mut doc, mut view) = note_view();
        let other = koru_core::ids::ListId::new();
        view.set_lists(vec![
            crate::tasks_view::ListFace {
                id: list,
                name: "Top".to_owned(),
                color: Some("teal".to_owned()),
            },
            crate::tasks_view::ListFace {
                id: other,
                name: "Later".to_owned(),
                color: None,
            },
        ]);
        let mods = ModifiersState::empty();
        let title = view
            .title_rect()
            .expect("a list page has a title to rename");
        assert!(view.mouse_double_click(&doc, title.x + 2.0, title.y + 2.0));
        assert!(view.heading_rename.is_some(), "the field stands");
        for ch in ["N", "e", "w"] {
            view.key(&mut doc, &Key::Character(ch.into()), Some(ch), mods);
        }
        view.key(&mut doc, &Key::Named(NamedKey::Enter), None, mods);
        assert!(view.heading_rename.is_none(), "and goes");
        assert_eq!(
            view.take_request(),
            Some(Request::RenameList {
                list,
                name: "New".to_owned()
            })
        );

        assert!(view.mouse_double_click(&doc, title.x + 2.0, title.y + 2.0));
        view.key(&mut doc, &Key::Named(NamedKey::Escape), None, mods);
        assert!(view.heading_rename.is_none());
        assert_eq!(view.take_request(), None, "Escape asks nothing");

        assert!(view.mouse_double_click(&doc, title.x + 2.0, title.y + 2.0));
        for ch in ["L", "a", "t", "e", "r"] {
            view.key(&mut doc, &Key::Character(ch.into()), Some(ch), mods);
        }
        view.key(&mut doc, &Key::Named(NamedKey::Enter), None, mods);
        assert_eq!(view.take_request(), None, "a sibling's name is refused");

        let mut today = TasksView::new(RailTarget::Bucket(Bucket::Today));
        today.set_viewport(900.0, 600.0);
        assert!(
            today.title_rect().is_none(),
            "a queue's name is nobody's to change"
        );
    }
}
