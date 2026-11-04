// SPDX-License-Identifier: MPL-2.0

//! The column's sections, and the geometry rule they follow.

use super::*;

fn area() -> Rect {
    Rect::new(900.0, theme::CAPTION_H, theme::CONTEXT_W, 600.0)
}

fn rows(count: usize) -> Vec<HistoryRow> {
    (0..count)
        .map(|i| HistoryRow {
            key: format!("k{i}"),
            label: format!("Row {i}"),
            detail: String::new(),
        })
        .collect()
}

/// Four sections are offered — Tasks joined History when its model landed
/// (V-5), Reminders when its own did, and Cloud sync when the gateway learned
/// to remember what its passes had done; Comments and Assets stay parked
/// (owner decision, 2026-08-31). Tasks is hidden for now
/// (`koru_features::CONTEXT_TASKS_SHOWN`), so with every flag on the row holds
/// three. The lone-tab rule ("a single visible section draws no tab row")
/// lives in `section_rects`' guard and `tabs_extra`; a build without
/// Reminders and Cloud sync is the one it applies to.
#[test]
fn the_visible_sections_are_tasks_reminders_history_and_cloud() {
    assert_eq!(
        Section::VISIBLE,
        &[
            Section::Tasks,
            Section::Reminders,
            Section::History,
            Section::CloudSync
        ]
    );
    assert_eq!(
        Section::visible(),
        [Section::Reminders, Section::History, Section::CloudSync],
        "Tasks is hidden for now"
    );
    let a = area();
    let sections = ContextView::section_rects(a);
    assert_eq!(sections.len(), 3);
    assert!(sections.iter().all(|(s, _)| *s != Section::Tasks));
    // They share one row, left to right, with no gap between them.
    for pair in sections.windows(2) {
        let (_, left) = pair[0];
        let (_, right) = pair[1];
        assert_eq!(left.y, right.y);
        assert_eq!(left.x + left.w, right.x);
    }
    for (section, rect) in sections {
        assert_eq!(
            ContextView::section_at(a, rect.centre().0, rect.centre().1),
            Some(section)
        );
        let mut view = ContextView::new();
        assert_eq!(
            view.mouse_down(a, rect.centre().0, rect.centre().1),
            Outcome::Consumed
        );
    }
}

/// The default section must be one the column actually offers.
#[test]
fn the_default_section_is_visible() {
    assert!(Section::VISIBLE.contains(&Section::default()));
    assert!(Section::visible().contains(&Section::default()));
}

/// **Tasks is hidden for now** (`koru_features::CONTEXT_TASKS_SHOWN`), and
/// every way into it says no: it is not shown, `select` — the keyboard's door —
/// refuses it, and no press anywhere in the column lands on it. The column
/// opens on History.
#[test]
fn the_tasks_section_is_hidden_for_now() {
    assert!(!Section::Tasks.shown_in_product());
    let mut v = ContextView::new();
    assert_eq!(v.section(), Section::History, "it opens on History");
    assert!(!v.select(Section::Tasks));
    assert_eq!(v.section(), Section::History);
    let a = area();
    let mut y = a.y;
    while y < a.y + 60.0 {
        let mut x = a.x;
        while x < a.x + a.w {
            assert_ne!(ContextView::section_at(a, x, y), Some(Section::Tasks));
            x += 4.0;
        }
        y += 2.0;
    }
}

/// A press on the column's background is still the column's — it must not fall
/// through to the view beside it.
#[test]
fn a_press_on_the_background_is_consumed() {
    let a = area();
    let mut v = ContextView::new();
    let before = v.section();
    assert_eq!(
        v.mouse_down(a, a.x + 10.0, a.y + a.h - 10.0),
        Outcome::Consumed
    );
    assert_eq!(v.section(), before, "and changed nothing");
}

/// A collapsed column is a zero-width rect, and `Rect::contains` is inclusive
/// on every edge — so nothing in one may answer for a click.
#[test]
fn nothing_is_hit_in_a_collapsed_column() {
    let shut = Rect::new(1200.0, theme::CAPTION_H, 0.0, 600.0);
    assert_eq!(ContextView::section_at(shut, 1200.0, 300.0), None);
}

/// `Section::ALL` keeps every section — parked ones included — carrying its
/// own label and empty line, so a returning section is a `VISIBLE` edit and
/// not a hunt for lost strings.
#[test]
fn every_section_has_its_own_label_and_empty_line() {
    let mut labels: Vec<&str> = Section::ALL.iter().map(|s| s.label()).collect();
    let n = labels.len();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), n, "two sections share a label");

    let mut empties: Vec<&str> = Section::ALL.iter().map(|s| s.empty_line()).collect();
    empties.sort_unstable();
    empties.dedup();
    assert_eq!(empties.len(), n, "two sections share an empty line");
}

#[test]
fn the_compare_button_answers_by_selection_count() {
    let a = area();
    let mut view = ContextView::new();
    view.set_history(rows(2), None);
    let layout = view.history_layout(a);
    let compare = layout.compare;

    // Nothing selected: the press lands on chrome and asks for nothing.
    assert_eq!(
        view.mouse_down(a, compare.centre().0, compare.centre().1),
        Outcome::Consumed
    );

    // One selected: compare against the document as it is now.
    view.mouse_down(a, layout.rows[0].centre().0, layout.rows[0].centre().1);
    assert_eq!(
        view.mouse_down(a, compare.centre().0, compare.centre().1),
        Outcome::CompareWithCurrent("k0".into())
    );

    // Two selected: compare each other.
    let layout = view.history_layout(a);
    view.mouse_down(a, layout.rows[1].centre().0, layout.rows[1].centre().1);
    assert_eq!(
        view.mouse_down(a, compare.centre().0, compare.centre().1),
        Outcome::CompareVersions("k0".into(), "k1".into())
    );
}

#[test]
fn history_selection_survives_refresh_while_its_id_remains() {
    let a = area();
    let mut view = ContextView::new();
    view.set_history(
        vec![
            HistoryRow {
                key: "one".into(),
                label: "One".into(),
                detail: String::new(),
            },
            HistoryRow {
                key: "two".into(),
                label: "Two".into(),
                detail: String::new(),
            },
        ],
        None,
    );
    let first = view.history_layout(a).rows[0];
    view.mouse_down(a, first.centre().0, first.centre().1);
    assert_eq!(view.selected_history, vec!["one"]);

    view.set_history(
        vec![
            HistoryRow {
                key: "new".into(),
                label: "New".into(),
                detail: String::new(),
            },
            HistoryRow {
                key: "one".into(),
                label: "One refreshed".into(),
                detail: String::new(),
            },
        ],
        None,
    );
    assert_eq!(view.selected_history, vec!["one"]);

    view.set_history(
        vec![HistoryRow {
            key: "new".into(),
            label: "New".into(),
            detail: String::new(),
        }],
        None,
    );
    assert!(view.selected_history.is_empty());
}

/// A short list needs no scrollbar and does not move; a long one scrolls,
/// clamps at both ends, and grows a thumb.
#[test]
fn the_version_list_scrolls_only_when_it_overflows() {
    let a = area();
    let mut view = ContextView::new();

    view.set_history(rows(3), None);
    assert!(view.history_layout(a).scrollbar(0.0).is_none());
    assert!(!view.scroll_by(a, -100.0), "nothing to scroll");
    assert_eq!(view.history_scroll, 0.0);

    view.set_history(rows(60), None);
    let layout = view.history_layout(a);
    assert!(layout.scrollbar(0.0).is_some());
    assert!(layout.max_scroll() > 0.0);

    assert!(view.scroll_by(a, -100.0), "wheel down moves the list");
    assert_eq!(view.history_scroll, 100.0);
    view.scroll_by(a, -1.0e6);
    assert_eq!(view.history_scroll, view.history_layout(a).max_scroll());
    view.scroll_by(a, 1.0e6);
    assert_eq!(view.history_scroll, 0.0);
}

/// A row scrolled out of the viewport is not clickable at its old position —
/// the press lands on whatever is really there now.
#[test]
fn a_scrolled_out_row_is_not_clickable_where_it_was() {
    let a = area();
    let mut view = ContextView::new();
    view.set_history(rows(60), None);
    let first = view.history_layout(a).rows[0];
    view.mouse_down(a, first.centre().0, first.centre().1);
    assert_eq!(view.selected_history, vec!["k0"]);

    // Scroll the top row far out of the viewport; its old centre now sits on
    // a different row, and the first row's own rect is above the viewport.
    view.scroll_by(a, -10.0 * ROW_PITCH);
    let layout = view.history_layout(a);
    assert!(layout.rows[0].y + layout.rows[0].h < layout.viewport.y);
    view.mouse_down(a, first.centre().0, first.centre().1);
    assert_ne!(view.selected_history, vec!["k0", "k0"]);
    assert!(view.selected_history.len() <= 2);

    // And a hovered row that scrolled away anchors no tooltip.
    assert!(view.history_tooltip(a).is_none());
}

/// Grabbing the thumb and moving to the track's foot lands at max scroll;
/// release ends the drag.
#[test]
fn the_thumb_drags_the_list() {
    let a = area();
    let mut view = ContextView::new();
    view.set_history(rows(60), None);
    let layout = view.history_layout(a);
    let geom = layout.scrollbar(0.0).expect("overflowing list");
    assert_eq!(
        view.mouse_down(a, geom.thumb_x + geom.thumb_w / 2.0, geom.thumb_top + 2.0),
        Outcome::Consumed
    );
    assert!(view.history_drag.is_some());
    assert!(view.mouse_move(a, geom.thumb_x, geom.track_bottom + 50.0));
    assert_eq!(view.history_scroll, view.history_layout(a).max_scroll());
    assert!(view.mouse_up());
    assert!(view.history_drag.is_none());
}

// ------------------------------------------------------------- the Tasks section
//
// Hidden for now (`koru_features::CONTEXT_TASKS_SHOWN`), so these show it with
// `show_hidden_section` rather than `select`, which refuses it: the section's
// rows keep their tests while its tab is away.

fn a_task(title: &str, due: Option<&str>) -> TaskRow {
    TaskRow {
        node: NodeId::new(),
        title: title.to_owned(),
        due: due.map(str::to_owned),
        completed: false,
        overdue: false,
    }
}

/// **Every task row hits back to itself** — the first rule for chrome, and the
/// reason `task_rects` is the only arithmetic in the section.
#[test]
fn every_task_row_hit_tests_back_to_itself() {
    let a = area();
    let mut v = ContextView::new();
    v.show_hidden_section(Section::Tasks);
    let rows = vec![a_task("One", Some("Sep 2")), a_task("Two", None)];
    assert!(v.set_tasks(rows.clone()));
    for (row, r) in rows.iter().zip(task_rects(a, rows.len())) {
        let (x, y) = r.centre();
        assert_eq!(
            v.mouse_down(a, x, y),
            Outcome::RevealTask(row.node),
            "a press on a row names that row"
        );
    }
}

/// The rows are only the Tasks section's. A press at the same place while
/// another section is showing belongs to that section — History's own chrome
/// sits there now — never to a task nobody can see.
#[test]
fn a_task_row_is_only_pressable_while_its_section_is_showing() {
    let a = area();
    let mut v = ContextView::new();
    v.set_tasks(vec![a_task("One", None)]);
    let (x, y) = task_rects(a, 1)[0].centre();
    assert!(
        !matches!(v.mouse_down(a, x, y), Outcome::RevealTask(_)),
        "History is showing, so the press belongs to History's chrome"
    );
    v.show_hidden_section(Section::Tasks);
    assert!(matches!(v.mouse_down(a, x, y), Outcome::RevealTask(_)));
}

/// A hover keyed by index over a list that has just changed is a hover on
/// whatever slid into that place.
#[test]
fn new_rows_drop_the_hover() {
    let a = area();
    let mut v = ContextView::new();
    v.show_hidden_section(Section::Tasks);
    v.set_tasks(vec![a_task("One", None), a_task("Two", None)]);
    let (x, y) = task_rects(a, 2)[1].centre();
    assert!(v.mouse_move(a, x, y));
    assert_eq!(v.hovered_task, Some(1));
    v.set_tasks(vec![a_task("Only", None)]);
    assert_eq!(v.hovered_task, None);
}

/// The empty line is a fact about **this document** now, not about a feature
/// that does not exist yet.
#[test]
fn an_empty_tasks_section_says_so_about_the_document() {
    assert_eq!(Section::Tasks.empty_line(), "No tasks in this document.");
}

// ---------------------------------------------------------- the Cloud section

fn a_line(label: &str, value: &str) -> CloudLine {
    CloudLine {
        label: label.to_owned(),
        value: value.to_owned(),
        detail: String::new(),
    }
}

fn a_cloud_view(lines: Vec<CloudLine>, note: Option<&str>) -> ContextView {
    let mut view = ContextView::new();
    view.select(Section::CloudSync);
    view.set_cloud(
        "Cloud sync".into(),
        lines,
        note.map(str::to_owned),
        "Copy details".into(),
    );
    view
}

/// **Draw and hit test agree.** The one action this section has is pressable
/// exactly where `cloud_layout` puts it, whatever the line count above it —
/// the rule `every_task_row_hit_tests_back_to_itself` states for Tasks.
#[test]
fn the_copy_action_is_pressable_where_it_is_drawn() {
    let a = area();
    for count in 1..=5 {
        let lines = (0..count)
            .map(|i| a_line(&format!("Label {i}"), &format!("Value {i}")))
            .collect();
        let mut view = a_cloud_view(lines, None);
        let copy = view.cloud_layout(a).copy;
        let (x, y) = copy.centre();
        assert_eq!(
            view.mouse_down(a, x, y),
            Outcome::CopyCloudDetails,
            "{count} lines"
        );
        // And the button sits below every line it follows, rather than over
        // one of them.
        let last = *view.cloud_layout(a).lines.last().expect("a line");
        assert!(copy.y > last.y + last.h, "{count} lines");
    }
}

/// Nothing to copy, nothing pressable. An action that puts an empty string on
/// the clipboard looks like it worked.
#[test]
fn an_empty_cloud_section_offers_no_action() {
    let a = area();
    let mut view = a_cloud_view(Vec::new(), Some("Reading Cloud sync…"));
    assert!(!view.has_cloud_details());
    let copy = view.cloud_layout(a).copy;
    let (x, y) = copy.centre();
    assert_eq!(view.mouse_down(a, x, y), Outcome::Consumed);
}

/// The press belongs to whichever section is showing — the same rule the task
/// rows keep.
#[test]
fn the_copy_action_is_only_pressable_while_its_section_is_showing() {
    let a = area();
    let mut view = a_cloud_view(vec![a_line("Sync", "On for Steven")], None);
    let (x, y) = view.cloud_layout(a).copy.centre();
    view.select(Section::History);
    assert_ne!(view.mouse_down(a, x, y), Outcome::CopyCloudDetails);
    view.select(Section::CloudSync);
    assert_eq!(view.mouse_down(a, x, y), Outcome::CopyCloudDetails);
}

/// **The clipboard carries what the column is showing**, one line each — and
/// the exact-moment variant wherever a line has one.
#[test]
fn the_copied_text_is_what_the_column_says() {
    let view = a_cloud_view(
        vec![
            a_line("Sync", "On for Steven"),
            CloudLine {
                label: "Last pass".into(),
                value: "2 min · settled 9, uploaded 1, downloaded 0".into(),
                detail: "12:36:30 · settled 9, uploaded 1, downloaded 0".into(),
            },
        ],
        Some("Images do not travel through Koru Cloud in this release."),
    );
    assert_eq!(
        view.cloud_details(),
        "Sync: On for Steven\n\
         Last pass: 12:36:30 · settled 9, uploaded 1, downloaded 0\n\
         Images do not travel through Koru Cloud in this release.\n"
    );
}

/// Every state the tab can be in draws something, and each draws its own
/// thing — a state that rendered as another is a state nobody can diagnose.
#[test]
fn every_cloud_state_has_its_own_reading() {
    let states = [
        (Vec::new(), Some("Reading Cloud sync…")),
        (
            vec![a_line("Sync", "Off, or not signed in on this device")],
            None,
        ),
        (vec![a_line("This document", "Never uploaded")], None),
        (vec![a_line("This document", "Uploaded 1h")], None),
        (vec![a_line("This document", "Downloaded 3 min")], None),
        (
            vec![a_line("This document", "Waiting: sync_transfer_timeout")],
            None,
        ),
        (
            vec![a_line(
                "Images",
                "2 not on this device · waiting for WIN10-MAIN",
            )],
            Some("Images do not travel through Koru Cloud in this release."),
        ),
    ];
    let mut seen: Vec<String> = Vec::new();
    for (lines, note) in states {
        let empty = lines.is_empty();
        let view = a_cloud_view(lines, note);
        assert_eq!(view.has_cloud_details(), !empty);
        let details = view.cloud_details();
        assert!(
            !seen.contains(&details),
            "two states read the same: {details}"
        );
        seen.push(details);
    }
}

/// The tab's localized label reaches the column's heading, so the Cloud tab is
/// titled like the others — "Cloud sync · <document>".
#[test]
fn the_injected_label_replaces_the_hardcoded_one() {
    let mut view = ContextView::new();
    view.select(Section::CloudSync);
    assert_eq!(view.section_label(Section::CloudSync), "Cloud sync");
    view.set_cloud(
        "Synchronisation cloud".into(),
        Vec::new(),
        None,
        "Copier".into(),
    );
    assert_eq!(
        view.section_label(Section::CloudSync),
        "Synchronisation cloud"
    );
}

/// A push that changes nothing is not a redraw — the contract every setter in
/// this column keeps.
#[test]
fn an_unchanged_cloud_push_reports_nothing_moved() {
    let mut view = ContextView::new();
    let lines = vec![a_line("Sync", "On")];
    assert!(view.set_cloud("Cloud sync".into(), lines.clone(), None, "Copy".into(),));
    assert!(!view.set_cloud("Cloud sync".into(), lines, None, "Copy".into()));
}
