// SPDX-License-Identifier: MPL-2.0

//! The rail's geometry, its vocabulary, and what its trailing lane says.
//!
//! Driven over a synthetic list table, so nothing here needs a document, a
//! font or a store — the same contract the explore column's geometry half
//! keeps.

use super::*;

mod tree;

fn area() -> Rect {
    Rect::new(0.0, theme::CAPTION_H, theme::SIDEBAR_W, 600.0)
}

/// Where the rows begin, as `ExploreView` computes it. A number rather than a
/// call, because these tests drive the rail on its own.
fn top() -> f32 {
    theme::CAPTION_H + theme::CAPTION_H + theme::NAV_PAD + 24.0
}

fn list(name: &str, color: Option<&str>, count: usize, overdue: usize) -> RailList {
    RailList::flat(ListId::new(), name, color, count, overdue)
}

/// The rail's lists in the order it holds them, depth first.
fn order(rail: &TasksRail) -> Vec<ListId> {
    rail.lists().iter().map(|l| l.id).collect()
}

fn rail(lists: usize) -> TasksRail {
    let mut rail = TasksRail::default();
    rail.set_lists(
        (0..lists)
            .map(|i| list(&format!("list {i}"), None, i, 0))
            .collect(),
    );
    rail.set_counts(TaskCounts {
        known: true,
        today: 5,
        today_overdue: 3,
        next: 5,
        upcoming: 12,
        later: 28,
        completed: 122,
        ..TaskCounts::default()
    });
    rail
}

/// The round trip that stops the rail being off by one — the same shape as the
/// library rows' `every_visible_row_hit_tests_back_to_its_own_index`.
#[test]
fn every_rail_row_hit_tests_back_to_itself() {
    let rail = rail(3);
    let (a, t) = (area(), top());
    let rows = rail.rows(a, t);
    assert!(!rows.is_empty());
    for (row, r) in rows {
        let (x, y) = r.centre();
        assert_eq!(rail.at(a, t, x, y), Some(row), "{row:?}");
        // ...and every part of it is inside it, so a press on the count lane
        // or the `+` cannot land on the row above.
        let parts = rail.row_parts(row, r);
        for part in [parts.icon, parts.label, parts.trailing] {
            let (px, py) = part.centre();
            assert_eq!(rail.at(a, t, px, py), Some(row), "{row:?} part");
        }
    }
}

/// The order the design draws: the four queues with Reminders under Upcoming,
/// the Lists heading, the lists, and Completed last under its rule. There is
/// no "+ New list" row at the foot of the section any more: the heading's `+`
/// is the way in.
#[test]
fn the_rail_reads_top_to_bottom_with_completed_last() {
    let rail = rail(2);
    let (a, t) = (area(), top());
    let rows: Vec<RailRow> = rail.rows(a, t).into_iter().map(|(row, _)| row).collect();
    assert_eq!(
        rows,
        vec![
            RailRow::Bucket(Bucket::Today),
            RailRow::Bucket(Bucket::Next),
            RailRow::Bucket(Bucket::Upcoming),
            RailRow::Reminders,
            RailRow::Bucket(Bucket::Later),
            RailRow::ListsHeading,
            RailRow::List(0),
            RailRow::List(1),
            RailRow::Bucket(Bucket::Completed),
        ]
    );
    let rects = rail.rows(a, t);
    for pair in rects.windows(2) {
        assert!(
            pair[0].1.y + pair[0].1.h <= pair[1].1.y + 0.01,
            "{:?} overlaps {:?}",
            pair[0].0,
            pair[1].0
        );
    }
}

/// The hairline over Completed is chrome too: captured as a named part, in
/// the band of air that carries it — a 1px line is what is drawn, but the
/// band is what a pointer can reach in the inspector.
#[test]
fn the_completed_rule_is_a_named_part_in_the_band_that_carries_it() {
    let mut rail = rail(1);
    let (a, t) = (area(), top());
    let mut scene = vello::Scene::new();
    let mut engine = TextEngine::new();
    let root = vello::kurbo::Affine::IDENTITY;
    let capture = koru_draw::inspect::Frame::begin(1, root, scene::rect(a.x, a.y, a.w, a.h));
    rail.draw(&mut scene, root, &mut engine, a, t);
    let records = capture.finish().records;

    let divider = records
        .iter()
        .find(|r| r.name == "ExploreDivider")
        .expect("the rule over Completed is captured");
    let band = divider.rect();
    assert_eq!(band.height(), RULE_GAP as f64);
    let completed = rail
        .rows(a, t)
        .into_iter()
        .find(|(row, _)| *row == RailRow::Bucket(Bucket::Completed))
        .map(|(_, r)| r)
        .expect("Completed is on screen");
    assert!(
        (band.y1 - completed.y as f64).abs() < 1e-6,
        "the band ends where Completed begins"
    );
    assert!(
        divider
            .properties
            .iter()
            .any(|p| p.name == "stroke" && p.value == "1 px"),
        "the drawn hairline is named"
    );

    // And the hierarchy reads in the order the column reads: the divider sits
    // between the last list row and the Completed row, not at the head of the
    // pass.
    let row_index = |want: RailRow| {
        records
            .iter()
            .position(|r| {
                r.name == "ExploreRow"
                    && r.properties
                        .iter()
                        .any(|p| p.name == "row" && p.value == format!("{want:?}"))
            })
            .unwrap_or_else(|| panic!("{want:?} is captured"))
    };
    let last_list = row_index(RailRow::List(0));
    let completed_row = row_index(RailRow::Bucket(Bucket::Completed));
    let divider_at = records
        .iter()
        .position(|r| r.name == "ExploreDivider")
        .unwrap();
    assert!(
        last_list < divider_at && divider_at < completed_row,
        "divider at {divider_at}, rows at {last_list} and {completed_row}"
    );
}

/// A press on a bucket asks for it **and** shows it as the selected one, so the
/// card appears on the frame the row was clicked rather than a round trip
/// later.
#[test]
fn pressing_a_bucket_opens_it_and_selects_it() {
    let mut rail = rail(1);
    let (a, t) = (area(), top());
    let (_, r) = rail
        .rows(a, t)
        .into_iter()
        .find(|(row, _)| *row == RailRow::Bucket(Bucket::Upcoming))
        .unwrap();
    let (x, y) = r.centre();
    assert_eq!(
        rail.mouse_down(a, t, x, y, false, false),
        Some(TasksRailRequest::OpenBucket {
            bucket: Bucket::Upcoming,
            new_tab: false
        })
    );
    assert_eq!(rail.selected(), Some(RailTarget::Bucket(Bucket::Upcoming)));
}

/// **The Reminders row is a page, like a queue.** A plain press asks for the
/// All Reminders page *and* moves the card — the page is a tab, so there is
/// a page for the selection to mean.
#[test]
fn pressing_reminders_opens_the_page_and_moves_the_card() {
    let mut rail = rail(1);
    let (a, t) = (area(), top());
    let (_, r) = rail
        .rows(a, t)
        .into_iter()
        .find(|(row, _)| *row == RailRow::Reminders)
        .expect("Reminders");
    let (x, y) = r.centre();
    assert_eq!(
        rail.mouse_down(a, t, x, y, false, false),
        Some(TasksRailRequest::OpenReminders { new_tab: false })
    );
    assert_eq!(rail.selected(), Some(RailTarget::Reminders));
}

/// Ctrl on the Reminders row carries the queue rows' disposition too: a new
/// tab, and the card stays on the page still in front.
#[test]
fn ctrl_clicking_reminders_asks_for_a_new_tab_and_leaves_the_card() {
    let mut rail = rail(1);
    let (a, t) = (area(), top());
    let (_, today) = rail
        .rows(a, t)
        .into_iter()
        .find(|(row, _)| *row == RailRow::Bucket(Bucket::Today))
        .expect("Today");
    let (x, y) = today.centre();
    rail.mouse_down(a, t, x, y, false, false);
    assert_eq!(rail.selected(), Some(RailTarget::Bucket(Bucket::Today)));

    let (_, r) = rail
        .rows(a, t)
        .into_iter()
        .find(|(row, _)| *row == RailRow::Reminders)
        .expect("Reminders");
    let (x, y) = r.centre();
    assert_eq!(
        rail.mouse_down(a, t, x, y, true, false),
        Some(TasksRailRequest::OpenReminders { new_tab: true })
    );
    assert_eq!(
        rail.selected(),
        Some(RailTarget::Bucket(Bucket::Today)),
        "the card stays on the queue still in front"
    );
}

/// The Reminders row's count is the bell badge's number, pushed like every
/// other answer the rail is drawn from.
#[test]
fn the_reminder_attention_count_is_pushed_and_compared() {
    let mut rail = TasksRail::default();
    assert!(rail.set_reminder_attention(3));
    assert!(
        !rail.set_reminder_attention(3),
        "the same number is not a change"
    );
    assert!(rail.set_reminder_attention(0));
}

/// A list row names its id, never its index: the table is a cache that
/// reorders when a peer renames one.
#[test]
fn pressing_a_list_names_its_id() {
    let mut rail = rail(3);
    let id = rail.lists()[1].id;
    let (a, t) = (area(), top());
    let (_, r) = rail
        .rows(a, t)
        .into_iter()
        .find(|(row, _)| *row == RailRow::List(1))
        .unwrap();
    let (x, y) = r.centre();
    assert_eq!(
        rail.mouse_down(a, t, x, y, false, false),
        Some(TasksRailRequest::OpenList {
            id: id,
            new_tab: false
        })
    );
    assert_eq!(rail.selected(), Some(RailTarget::List(id)));
}

/// The `+` beside the heading opens the inline field for a top-level list; the
/// word "Lists" itself is furniture. There is no second "+ New list" row at the
/// foot of the section: the field opens **first under the heading**, where the
/// new list will be.
#[test]
fn the_heading_plus_asks_for_the_field_and_it_opens_under_the_heading() {
    let mut rail = rail(1);
    let (a, t) = (area(), top());
    let rows = rail.rows(a, t);
    let (_, heading) = rows
        .iter()
        .find(|(row, _)| *row == RailRow::ListsHeading)
        .unwrap();
    let plus = rail.row_parts(RailRow::ListsHeading, *heading).trailing;
    let (px, py) = plus.centre();
    assert_eq!(
        rail.mouse_down(a, t, px, py, false, false),
        Some(TasksRailRequest::BeginNewList { parent: None })
    );
    let label = rail.row_parts(RailRow::ListsHeading, *heading).label;
    let (lx, ly) = label.centre();
    assert_eq!(
        rail.mouse_down(a, t, lx, ly, false, false),
        None,
        "the word is furniture"
    );

    // The bottom row is gone: the last list is followed directly by Completed.
    let order: Vec<RailRow> = rows.iter().map(|(row, _)| *row).collect();
    let last_list = order
        .iter()
        .position(|row| *row == RailRow::List(0))
        .unwrap();
    assert_eq!(
        order.get(last_list + 1),
        Some(&RailRow::Bucket(Bucket::Completed))
    );
    assert!(!order.contains(&RailRow::Compose), "no field, no row");

    // ...and the field, once open, is the first row under the heading.
    rail.begin_new_list(None);
    let order: Vec<RailRow> = rail.rows(a, t).into_iter().map(|(row, _)| row).collect();
    let heading_at = order
        .iter()
        .position(|row| *row == RailRow::ListsHeading)
        .unwrap();
    assert_eq!(order.get(heading_at + 1), Some(&RailRow::Compose));
    assert_eq!(order.get(heading_at + 2), Some(&RailRow::List(0)));
}

/// The field the container draws sits after the swatch, inside the row the
/// rail laid out — so the colour the list is about to wear stays visible while
/// its name is typed.
#[test]
fn the_field_covers_the_new_list_row_after_its_swatch() {
    let mut rail = rail(1);
    let (a, t) = (area(), top());
    rail.begin_new_list(None);
    assert_eq!(rail.composing(), Some(None));
    let row = rail.new_list_rect(a, t).unwrap();
    let field = rail.new_list_field_rect(a, t).unwrap();
    assert!(field.x > row.x, "the swatch keeps the head of the row");
    assert!(field.x + field.w <= row.x + row.w);
    assert_eq!(field.y, row.y);
    rail.end_new_list();
    assert_eq!(rail.composing(), None);
}

/// "5 · 3 overdue": one quiet number, one warm one, the separator between them,
/// and no separator when there is nothing to separate.
#[test]
fn the_trailing_lane_splits_the_count_from_the_overdue_share() {
    assert_eq!(
        count_parts(Some(5), Some(3), false),
        (Some("5 · ".into()), Some("3 overdue".into()))
    );
    assert_eq!(
        count_parts(Some(14), None, false),
        (Some("14".into()), None)
    );
    // A list row shows the overdue share **instead of** its count.
    assert_eq!(
        count_parts(None, Some(1), false),
        (None, Some("1 overdue".into()))
    );
    // A page the store capped is a floor, and says so rather than being
    // quietly wrong.
    assert_eq!(
        count_parts(Some(500), None, true),
        (Some("500+".into()), None)
    );
    assert_eq!(count_parts(None, None, false), (None, None));
}

/// Nothing is drawn until an answer arrives: a `0` that means *we have not
/// looked yet* is a lie a reader cannot see through.
#[test]
fn counts_are_absent_until_the_first_answer() {
    let rail = TasksRail::default();
    assert!(!rail.counts().known);
    let mut rail = rail;
    assert!(rail.set_counts(TaskCounts {
        known: true,
        today: 2,
        ..TaskCounts::default()
    }));
    assert!(!rail.set_counts(TaskCounts {
        known: true,
        today: 2,
        ..TaskCounts::default()
    }));
    assert!(rail.counts().known);
}

/// One colour from the existing swatch set, and never the same one twice while
/// there is a spare.
#[test]
fn a_new_list_takes_the_first_unused_swatch() {
    let mut rail = TasksRail::default();
    assert_eq!(rail.suggested_color(), theme::ACCENT_TOKENS[0]);
    rail.set_lists(vec![
        list("a", Some(theme::ACCENT_TOKENS[0]), 0, 0),
        list("b", Some(theme::ACCENT_TOKENS[1]), 0, 0),
    ]);
    assert_eq!(rail.suggested_color(), theme::ACCENT_TOKENS[2]);
    rail.set_lists(
        theme::ACCENT_TOKENS
            .iter()
            .map(|token| list(token, Some(token), 0, 0))
            .collect(),
    );
    assert_eq!(rail.suggested_color(), theme::ACCENT_TOKENS[0], "wraps");
}

/// A rail longer than the column scrolls, and stops at both ends.
#[test]
fn a_long_rail_scrolls_and_stops_at_both_ends() {
    let mut rail = rail(60);
    let (a, t) = (area(), top());
    let first = || -> RailRow { RailRow::Bucket(Bucket::Today) };
    assert_eq!(rail.rows(a, t)[0].0, first());
    rail.scroll_by(a, t, -400.0);
    assert_ne!(rail.rows(a, t)[0].0, first(), "scrolled off the top");
    rail.scroll_by(a, t, -100_000.0);
    let bottom = rail.rows(a, t).last().unwrap().1;
    assert!(
        bottom.y + bottom.h <= a.y + a.h,
        "the last row stops inside the column"
    );
    rail.scroll_by(a, t, 100_000.0);
    assert_eq!(rail.rows(a, t)[0].0, first(), "back to the top");
}

/// A collapsed column offers nothing at all — `Rect::contains` is inclusive on
/// every edge, so a zero-width row at `x = 0` would answer for a press meant
/// for the view beside it.
#[test]
fn nothing_is_hit_in_a_collapsed_column() {
    let mut rail = rail(3);
    let shut = Rect::new(0.0, theme::CAPTION_H, 0.0, 600.0);
    assert!(rail.rows(shut, top()).is_empty());
    assert_eq!(rail.at(shut, top(), 0.0, top() + 4.0), None);
    assert_eq!(
        rail.mouse_down(shut, top(), 0.0, top() + 4.0, false, false),
        None
    );
    assert_eq!(rail.new_list_rect(shut, top()), None);
}

/// The hover is the rail's own and answers the cursor question, which is what
/// keeps the record of which surfaces are pressable (ADR-215).
#[test]
fn the_hover_follows_the_pointer_and_says_the_row_is_pressable() {
    let mut rail = rail(2);
    let (a, t) = (area(), top());
    assert!(!rail.pressable());
    let (_, r) = rail.rows(a, t)[0];
    let (x, y) = r.centre();
    assert!(rail.mouse_move(a, t, x, y));
    assert!(!rail.mouse_move(a, t, x, y), "the same row is not a move");
    assert!(rail.pressable());
    assert!(rail.clear_hover());
    assert!(!rail.pressable());
}

// ------------------------------------------------------------ managing a list
//
// V-6. The rail's half of Rename / Colour / Delete is two questions and no
// verbs: *which list was right-clicked*, and *where does a field over its row
// go*. Everything else — the menu, the box, the confirmation — is the
// container's, which is why there is nothing here to test about any of them.

/// **Every row that names a page has a menu**, and the target it reports is the
/// one that was under the pointer rather than the one that is selected.
#[test]
fn right_clicking_a_row_reports_the_page_it_names() {
    let rail = rail(3);
    let (a, t) = (area(), top());
    let wanted = rail.lists()[1].id;
    let (_, r) = rail
        .rows(a, t)
        .into_iter()
        .find(|(row, _)| *row == RailRow::List(1))
        .expect("the second list");
    let (x, y) = r.centre();
    assert_eq!(
        rail.context_menu(a, t, x, y),
        Some(TasksRailRequest::RowMenu {
            target: RailTarget::List(wanted),
            at: (x, y)
        })
    );

    // A queue has one too. It has nothing to rename, recolour or delete, which
    // is why it used to have no menu at all — and that withheld *Open in new
    // tab*, which is a thing you can do to a computed queue.
    for bucket in [Bucket::Today, Bucket::Later, Bucket::Completed] {
        let (_, r) = rail
            .rows(a, t)
            .into_iter()
            .find(|(row, _)| *row == RailRow::Bucket(bucket))
            .unwrap_or_else(|| panic!("no {bucket:?}"));
        let (x, y) = r.centre();
        assert_eq!(
            rail.context_menu(a, t, x, y),
            Some(TasksRailRequest::RowMenu {
                target: RailTarget::Bucket(bucket),
                at: (x, y)
            })
        );
    }
}

/// The Reminders row names a page, so it wears the queue's one-row menu:
/// *Open in new tab* is a thing you can do to a page even though there is
/// nothing about it to rename, recolour or delete.
#[test]
fn the_reminders_row_has_the_queues_one_row_menu() {
    let rail = rail(2);
    let (a, t) = (area(), top());
    let (_, r) = rail
        .rows(a, t)
        .into_iter()
        .find(|(row, _)| *row == RailRow::Reminders)
        .expect("Reminders");
    let (x, y) = r.centre();
    assert_eq!(
        rail.context_menu(a, t, x, y),
        Some(TasksRailRequest::RowMenu {
            target: RailTarget::Reminders,
            at: (x, y)
        })
    );
}

/// The furniture answers *nothing to open*: the heading and the row a new
/// list's name is typed on name no page. The caller swallows the press rather
/// than passing it to the view behind the column.
#[test]
fn the_rails_furniture_has_no_menu() {
    let mut rail = rail(2);
    rail.begin_new_list(None);
    let (a, t) = (area(), top());
    for row in [RailRow::ListsHeading, RailRow::Compose] {
        let (_, r) = rail
            .rows(a, t)
            .into_iter()
            .find(|(have, _)| *have == row)
            .unwrap_or_else(|| panic!("no {row:?}"));
        let (x, y) = r.centre();
        assert_eq!(rail.context_menu(a, t, x, y), None, "{row:?}");
    }
    // ...and so does the background below the last row.
    assert_eq!(rail.context_menu(a, t, a.x + 4.0, a.y + a.h - 2.0), None);
}

/// **Ctrl opens a second page without leaving the one you are reading**, which
/// is the whole meaning of the modifier — so the card does not move. A plain
/// press moves it, because that press *is* the rail's answer to *which queue*.
#[test]
fn ctrl_clicking_a_row_asks_for_a_new_tab_and_leaves_the_card() {
    let mut rail = rail(2);
    let (a, t) = (area(), top());
    let (_, today) = rail
        .rows(a, t)
        .into_iter()
        .find(|(row, _)| *row == RailRow::Bucket(Bucket::Today))
        .expect("Today");
    let (x, y) = today.centre();
    rail.mouse_down(a, t, x, y, false, false);
    assert_eq!(rail.selected(), Some(RailTarget::Bucket(Bucket::Today)));

    let (_, later) = rail
        .rows(a, t)
        .into_iter()
        .find(|(row, _)| *row == RailRow::Bucket(Bucket::Later))
        .expect("Later");
    let (x, y) = later.centre();
    assert_eq!(
        rail.mouse_down(a, t, x, y, true, false),
        Some(TasksRailRequest::OpenBucket {
            bucket: Bucket::Later,
            new_tab: true
        })
    );
    assert_eq!(
        rail.selected(),
        Some(RailTarget::Bucket(Bucket::Today)),
        "the card stays on the page still in front"
    );
}

/// **The rename box sits exactly on the row's own rectangle**, through the same
/// arithmetic the `+` row's field uses — so a name cannot be typed anywhere
/// other than where the list is drawn.
#[test]
fn the_rename_field_covers_the_row_it_renames() {
    let rail = rail(3);
    let (a, t) = (area(), top());
    let id = rail.lists()[2].id;
    assert_eq!(rail.row_of_list(id), Some(RailRow::List(2)));
    let row = rail
        .rows(a, t)
        .into_iter()
        .find(|(have, _)| *have == RailRow::List(2))
        .map(|(_, r)| r)
        .expect("the third list");
    let field = rail.list_field_rect(a, t, id).expect("a field rect");
    assert_eq!((field.y, field.h), (row.y, row.h));
    let parts = rail.row_parts(RailRow::List(2), row);
    assert_eq!(
        field.x, parts.icon.x,
        "the swatch lane and everything after"
    );
    assert!(
        field.x + field.w <= row.x + row.w,
        "and it stops inside the column"
    );
}

/// A list the table no longer holds has no row, so it has no field either — the
/// answer a peer's delete arriving under an open rename has to give.
#[test]
fn a_list_that_has_gone_has_no_rename_field() {
    let rail = rail(2);
    let (a, t) = (area(), top());
    assert_eq!(rail.row_of_list(ListId::new()), None);
    assert_eq!(rail.list_field_rect(a, t, ListId::new()), None);
}

/// One keyboard, one box: opening the rename puts the `+` row's field away, and
/// each is ended by its own call so a container cannot lose track of which is up.
#[test]
fn a_rename_and_a_new_list_are_never_both_open() {
    let mut rail = rail(1);
    let id = rail.lists()[0].id;
    rail.begin_new_list(None);
    assert_eq!(rail.composing(), Some(None));
    rail.begin_rename(id);
    assert_eq!(rail.composing(), None, "the + row's field went");
    assert_eq!(rail.renaming(), Some(id));
    rail.end_rename();
    assert_eq!(rail.renaming(), None);
}

// ------------------------------------------- dragging a list to a new place

/// The row's own rect on the rail.
fn list_rect(rail: &TasksRail, i: usize) -> Rect {
    rail.rows(area(), top())
        .into_iter()
        .find(|(row, _)| *row == RailRow::List(i))
        .map(|(_, r)| r)
        .unwrap_or_else(|| panic!("no rect for list {i}"))
}

/// **A press opens; a press that travels reorders.** The library's gesture on
/// a flat section: under the threshold the release asks for nothing, because
/// the press already opened the list; past it the rows reorder here and the
/// release asks the container to write the move down. The drop is on the
/// **bottom quarter** of the last row — the tree drag's *after* zone — since
/// the middle of a row now means *into* it.
#[test]
fn dragging_a_list_row_reorders_the_section_and_asks_for_the_write() {
    let mut rail = rail(3);
    let (a, t) = (area(), top());
    let was: Vec<ListId> = order(&rail);

    // A press that never travels: the open it already made is the whole of it.
    let r = list_rect(&rail, 0);
    let (x, y) = r.centre();
    assert!(matches!(
        rail.mouse_down(a, t, x, y, false, false),
        Some(TasksRailRequest::OpenList { .. })
    ));
    assert!(rail.dragging(), "and a drag is armed behind it");
    assert!(!rail.drag_armed(), "latent until it travels");
    rail.mouse_move(a, t, x, y + 1.0);
    assert!(!rail.drag_armed(), "one pixel is not a drag");
    assert_eq!(rail.mouse_up(), None, "so the release asks for nothing");
    assert!(!rail.dragging());
    assert_eq!(order(&rail), was, "and nothing moved");

    // ...and one that travels to the last row's bottom edge carries it there.
    let r = list_rect(&rail, 0);
    let (x, y) = r.centre();
    rail.mouse_down(a, t, x, y, false, false);
    let third = list_rect(&rail, 2);
    rail.mouse_move(a, t, x, after_edge(third));
    assert!(rail.drag_armed());
    assert_eq!(
        order(&rail),
        vec![was[1], was[2], was[0]],
        "the row is carried to the end as the pointer passes"
    );
    assert_eq!(
        rail.mouse_up(),
        Some(TasksRailRequest::MoveList {
            ids: vec![was[0]],
            parent: None,
            before: None
        })
    );
    assert!(!rail.dragging());
}

/// The bottom quarter of a row: the tree drag's *after* zone.
fn after_edge(r: Rect) -> f32 {
    r.y + r.h * 0.9
}

/// The top quarter of a row: the tree drag's *before* zone.
fn before_edge(r: Rect) -> f32 {
    r.y + r.h * 0.1
}

/// Dropping on a row's **top edge** puts the carried row immediately before
/// it, and the release names that sibling.
#[test]
fn dropping_on_a_rows_top_edge_puts_the_list_before_it() {
    let mut rail = rail(3);
    let (a, t) = (area(), top());
    let was = order(&rail);
    let r = list_rect(&rail, 2);
    let (x, y) = r.centre();
    rail.mouse_down(a, t, x, y, false, false);
    let second = list_rect(&rail, 1);
    rail.mouse_move(a, t, x, before_edge(second));
    assert!(rail.drag_armed());
    assert_eq!(order(&rail), vec![was[0], was[2], was[1]]);
    assert_eq!(
        rail.mouse_up(),
        Some(TasksRailRequest::MoveList {
            ids: vec![was[2]],
            parent: None,
            before: Some(was[1])
        })
    );
}

/// **The pushed table is refused while a row is being carried**, or the
/// container's every-turn push would snap it back under the pointer — and it
/// is taken again the moment the gesture ends.
#[test]
fn the_table_is_refused_while_a_row_is_carried_and_taken_again_after() {
    let mut rail = rail(3);
    let (a, t) = (area(), top());
    let was = order(&rail);
    let r = list_rect(&rail, 0);
    let (x, y) = r.centre();
    rail.mouse_down(a, t, x, y, false, false);
    let third = list_rect(&rail, 2);
    rail.mouse_move(a, t, x, after_edge(third));
    assert!(rail.drag_armed());

    let pushed: Vec<RailList> = (0..3)
        .map(|i| list(&format!("pushed {i}"), None, 0, 0))
        .collect();
    assert!(!rail.set_lists(pushed.clone()), "refused mid-carry");
    assert_eq!(order(&rail), vec![was[1], was[2], was[0]]);

    rail.mouse_up();
    assert!(rail.set_lists(pushed.clone()), "and taken once it ends");
    assert_eq!(
        order(&rail),
        pushed.iter().map(|l| l.id).collect::<Vec<_>>()
    );
}

/// A cancelled gesture commits nothing, and the frames it asked for stop. The
/// rows go back to where they were when the carry began, and the container's
/// next push is the truth again, which `set_lists` now accepts.
#[test]
fn cancelling_a_carry_writes_nothing() {
    let mut rail = rail(3);
    let (a, t) = (area(), top());
    let was = order(&rail);
    let r = list_rect(&rail, 0);
    let (x, y) = r.centre();
    rail.mouse_down(a, t, x, y, false, false);
    let third = list_rect(&rail, 2);
    rail.mouse_move(a, t, x, after_edge(third));
    assert_eq!(order(&rail), vec![was[1], was[2], was[0]], "carried");
    assert!(rail.animating(), "a carried row keeps the frames coming");
    assert!(rail.cancel_drag());
    assert!(!rail.dragging());
    assert_eq!(rail.mouse_up(), None);
    assert_eq!(order(&rail), was, "the carry is put back");
    let pushed: Vec<RailList> = (0..3)
        .map(|i| list(&format!("pushed {i}"), None, 0, 0))
        .collect();
    assert!(rail.set_lists(pushed.clone()));
    assert_eq!(
        order(&rail),
        pushed.iter().map(|l| l.id).collect::<Vec<_>>()
    );
}

/// The middle of a row means **into** it: a flat row dropped there becomes
/// that list's child, and the release says so.
#[test]
fn dropping_on_the_middle_of_a_row_nests_the_list_inside_it() {
    let mut rail = rail(3);
    let (a, t) = (area(), top());
    let was = order(&rail);
    let r = list_rect(&rail, 0);
    let (x, y) = r.centre();
    rail.mouse_down(a, t, x, y, false, false);
    let third = list_rect(&rail, 2);
    rail.mouse_move(a, t, x, third.y + third.h * 0.5);
    assert!(rail.drag_armed());
    assert_eq!(
        rail.mouse_up(),
        Some(TasksRailRequest::MoveList {
            ids: vec![was[0]],
            parent: Some(was[2]),
            before: None
        })
    );
    let moved = rail.lists().iter().find(|l| l.id == was[0]).unwrap();
    assert_eq!((moved.parent, moved.depth), (Some(was[2]), 1));
}
