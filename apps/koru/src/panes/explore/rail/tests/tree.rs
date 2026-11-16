// SPDX-License-Identifier: MPL-2.0

//! The Lists section as a tree (nested-lists §6, §12 *B — rail*): rows from
//! the pushed tree and this device's open set, the caret, the hover `+`, the
//! row a new list is typed on, rolled-up counts, and the nest drag.

use winit::keyboard::{Key as WKey, NamedKey};

use super::super::*;
use super::{area, top};
use crate::panes::Outcome;

fn leaf(name: &str, parent: Option<&RailList>, count: usize, overdue: usize) -> RailList {
    RailList {
        parent: parent.map(|p| p.id),
        depth: parent.map_or(0, |p| p.depth + 1),
        ..RailList::flat(ListId::new(), name, None, count, overdue)
    }
}

/// *Top 3* holding *Backlog* and *Core Data*, then *Other* and *Solo*.
fn tree() -> (TasksRail, [ListId; 5]) {
    let mut top3 = leaf("Top 3", None, 1, 0);
    top3.has_children = true;
    let backlog = leaf("Backlog", Some(&top3), 2, 1);
    let core = leaf("Core Data", Some(&top3), 3, 0);
    let other = leaf("Other", None, 4, 0);
    let solo = leaf("Solo", None, 5, 0);
    let ids = [top3.id, backlog.id, core.id, other.id, solo.id];
    let mut rail = TasksRail::default();
    rail.set_lists(vec![top3, backlog, core, other, solo]);
    rail.set_counts(TaskCounts {
        known: true,
        ..TaskCounts::default()
    });
    (rail, ids)
}

fn rect_of(rail: &TasksRail, id: ListId) -> Rect {
    let want = rail.row_of_list(id).expect("the list is in the table");
    rail.rows(area(), top())
        .into_iter()
        .find(|(row, _)| *row == want)
        .map(|(_, r)| r)
        .expect("the list's row is showing")
}

fn showing(rail: &TasksRail) -> Vec<ListId> {
    rail.rows(area(), top())
        .into_iter()
        .filter_map(|(row, _)| match row {
            RailRow::List(i) => Some(rail.lists()[i].id),
            _ => None,
        })
        .collect()
}

/// Press `id`'s row on its name and carry it until its centre is at `y`.
fn drag_to(rail: &mut TasksRail, id: ListId, y: f32) -> Option<TasksRailRequest> {
    let r = rect_of(rail, id);
    let x = rail.row_parts(rail.row_of_list(id).unwrap(), r).label.x + 2.0;
    let (_, cy) = r.centre();
    rail.mouse_down(area(), top(), x, cy, false);
    // Two moves: the first arms the drag, the second is the carry proper.
    rail.mouse_move(area(), top(), x, cy + (y - cy).signum() * (DRAG_ARM_PX + 1.0));
    rail.mouse_move(area(), top(), x, y);
    rail.mouse_up()
}

// ------------------------------------------------------------- rows and parts

#[test]
fn a_list_inside_another_is_drawn_under_it_and_indented() {
    let (rail, [top3, backlog, core, other, solo]) = tree();
    assert_eq!(showing(&rail), vec![top3, backlog, core, other, solo]);
    let x = |id| {
        let r = rect_of(&rail, id);
        rail.row_parts(rail.row_of_list(id).unwrap(), r).icon.x
    };
    assert_eq!(x(backlog) - x(top3), theme::NAV_INDENT);
    assert_eq!(x(other), x(top3), "siblings line up, caret or none");
    let parts = rail.row_parts(rail.row_of_list(top3).unwrap(), rect_of(&rail, top3));
    assert!(parts.caret.w > 0.0, "a parent has a caret");
    let parts = rail.row_parts(rail.row_of_list(other).unwrap(), rect_of(&rail, other));
    assert_eq!(parts.caret.w, 0.0, "a leaf has none to press");
}

/// A table nobody has nested in pays for no caret lane: the rail of every
/// workspace written before this looks as it did.
#[test]
fn a_flat_table_reserves_no_caret_lane() {
    let mut flat = TasksRail::default();
    let a = RailList::flat(ListId::new(), "A", None, 0, 0);
    let id = a.id;
    flat.set_lists(vec![a]);
    let r = rect_of(&flat, id);
    let parts = flat.row_parts(RailRow::List(0), r);
    assert_eq!(parts.icon.x, r.x + theme::NAV_PAD);
}

/// D7: no depth limit in the model, and a name is never squeezed out of a
/// narrow rail — the indent stops growing after `MAX_INDENT` levels.
#[test]
fn indent_stops_growing_after_four_levels() {
    let mut lists: Vec<RailList> = Vec::new();
    for depth in 0..10 {
        let mut list = leaf(&format!("level {depth}"), lists.last(), 0, 0);
        list.has_children = depth < 9;
        lists.push(list);
    }
    let ids: Vec<ListId> = lists.iter().map(|l| l.id).collect();
    let mut rail = TasksRail::default();
    rail.set_lists(lists);
    let x = |i: usize| {
        let r = rect_of(&rail, ids[i]);
        rail.row_parts(RailRow::List(i), r).icon.x
    };
    assert!(x(MAX_INDENT) > x(MAX_INDENT - 1));
    assert_eq!(x(9), x(MAX_INDENT), "deeper rows keep the fourth level's indent");
    let r = rect_of(&rail, ids[9]);
    assert!(
        rail.row_parts(RailRow::List(9), r).label.w > 40.0,
        "and the name still has room"
    );
}

#[test]
fn the_caret_shuts_and_opens_and_opens_no_page() {
    let (mut rail, [top3, backlog, core, other, solo]) = tree();
    let r = rect_of(&rail, top3);
    let (cx, cy) = rail.row_parts(RailRow::List(0), r).caret.centre();

    assert_eq!(rail.mouse_down(area(), top(), cx, cy, false), None);
    assert_eq!(showing(&rail), vec![top3, other, solo], "shut hides what is inside");
    assert_eq!(rail.selected(), None, "no page was opened");
    assert!(!rail.dragging(), "and no drag was armed");
    assert_eq!(rail.collapsed(), vec![top3]);

    assert_eq!(rail.mouse_down(area(), top(), cx, cy, false), None);
    assert_eq!(showing(&rail), vec![top3, backlog, core, other, solo]);
    assert!(rail.collapsed().is_empty());
}

/// Which rows are open is this device's, and survives a restart: the set is
/// read out, kept by the container, and put back before the table arrives.
#[test]
fn the_open_set_is_put_back_before_the_table_arrives() {
    let (rail, [top3, _, _, other, solo]) = tree();
    let lists = rail.lists().to_vec();
    let mut restored = TasksRail::default();
    restored.set_collapsed([top3]);
    restored.set_lists(lists);
    assert_eq!(showing(&restored), vec![top3, other, solo]);
}

/// D5: a shut parent shows its own tasks plus everything inside it; an open
/// one shows no number, because each row under it shows its own.
#[test]
fn a_shut_parent_shows_the_total_and_an_open_one_shows_no_number() {
    let (mut rail, [top3, ..]) = tree();
    assert_eq!(rail.shown_counts(0), None, "open: no number on the parent");
    assert_eq!(rail.shown_counts(1), Some((2, 1)), "each child shows its own");
    assert_eq!(rail.shown_counts(3), Some((4, 0)), "a leaf shows its own");

    rail.set_open(top3, false);
    assert_eq!(rail.shown_counts(0), Some((6, 1)), "shut: 1 + 2 + 3, 1 overdue");
}

// ------------------------------------------------------------ making a list

/// **The bug this work started from.** With more lists than fit, `+` opened a
/// field over a row at the foot of the section, scrolled out of sight: the
/// field held the keyboard and drew nothing. The row is now first under the
/// heading and is brought into view.
#[test]
fn the_plus_shows_the_field_at_the_top_however_long_the_rail_is() {
    let mut rail = super::rail(80);
    let (a, t) = (area(), top());
    // Read the last list, then press `+` in the heading from down there.
    rail.scroll_by(a, t, -100_000.0);
    assert!(
        rail.rows(a, t)
            .iter()
            .all(|(row, _)| *row != RailRow::ListsHeading),
        "the heading is scrolled out"
    );

    rail.begin_new_list(None);
    rail.reveal_new_list(a, t);
    let field = rail
        .new_list_field_rect(a, t)
        .expect("the field has somewhere to be drawn");
    assert!(field.y >= t && field.y + field.h <= a.y + a.h);

    let rows = rail.laid_out(a, t);
    let heading = rows
        .iter()
        .position(|(row, _)| *row == RailRow::ListsHeading)
        .unwrap();
    assert_eq!(rows[heading + 1].0, RailRow::Compose, "first under the heading");
    assert_eq!(rows[heading + 2].0, RailRow::List(0));
}

#[test]
fn no_field_no_compose_row() {
    let (rail, _) = tree();
    assert!(
        rail.laid_out(area(), top())
            .iter()
            .all(|(row, _)| *row != RailRow::Compose)
    );
    assert_eq!(rail.new_list_field_rect(area(), top()), None);
}

/// Rung three: the field for a child opens as the first row under its parent,
/// indented, and a shut parent is opened to show it.
#[test]
fn a_child_is_typed_first_under_its_parent_which_opens() {
    let (mut rail, [top3, backlog, ..]) = tree();
    rail.set_open(top3, false);
    rail.begin_new_list(Some(top3));
    assert_eq!(rail.composing(), Some(Some(top3)));

    let rows = rail.laid_out(area(), top());
    let parent = rows
        .iter()
        .position(|(row, _)| *row == RailRow::List(0))
        .unwrap();
    assert_eq!(rows[parent + 1].0, RailRow::Compose);
    assert_eq!(rows[parent + 2].0, rail.row_of_list(backlog).unwrap());

    let compose = rail.row_parts(RailRow::Compose, rows[parent + 1].1);
    let child = rail.row_parts(rows[parent + 2].0, rows[parent + 2].1);
    assert_eq!(compose.icon.x, child.icon.x, "indented as a child is");
}

/// A parent deleted while its child's name was being typed leaves the field
/// at the top level rather than nowhere.
#[test]
fn composing_under_a_list_that_is_gone_falls_to_the_top_level() {
    let (mut rail, _) = tree();
    rail.begin_new_list(Some(ListId::new()));
    assert_eq!(rail.composing(), Some(None));
}

/// The row's `+` sits immediately left of the count lane, so the number
/// never shifts; it is there for the row under the pointer and no other.
#[test]
fn the_hover_plus_makes_a_child_and_never_moves_the_count() {
    let (mut rail, [_, _, _, other, _]) = tree();
    let row = rail.row_of_list(other).unwrap();
    let r = rect_of(&rail, other);
    let quiet = rail.row_parts(row, r);
    let (ax, ay) = quiet.add.centre();

    // Not under the pointer: a press where the `+` would be opens the list.
    assert_eq!(
        rail.mouse_down(area(), top(), ax, ay, false),
        Some(TasksRailRequest::OpenList {
            id: other,
            new_tab: false
        })
    );
    rail.mouse_up();

    assert!(rail.mouse_move(area(), top(), ax, ay));
    let lit = rail.row_parts(row, r);
    assert_eq!(lit.trailing, quiet.trailing, "the count lane did not move");
    assert_eq!(lit.add, quiet.add);
    assert!(lit.add.x + lit.add.w <= lit.trailing.x);
    assert!(lit.label.x + lit.label.w <= lit.add.x, "the name gives way");
    assert_eq!(
        rail.mouse_down(area(), top(), ax, ay, false),
        Some(TasksRailRequest::BeginNewList {
            parent: Some(other)
        })
    );
    assert!(!rail.dragging(), "the `+` arms no drag");
}

// ------------------------------------------------------------------ the drag

#[test]
fn dragging_onto_the_middle_of_a_row_nests() {
    let (mut rail, [_, _, _, other, solo]) = tree();
    let (_, into) = rect_of(&rail, solo).centre();
    assert_eq!(
        drag_to(&mut rail, other, into),
        Some(TasksRailRequest::MoveList {
            id: other,
            parent: Some(solo),
            before: None
        })
    );
    let moved = rail.lists().iter().find(|l| l.id == other).unwrap();
    assert_eq!((moved.parent, moved.depth), (Some(solo), 1));
    assert!(showing(&rail).contains(&other), "and its new parent is open");
}

#[test]
fn dragging_out_past_the_last_row_un_nests() {
    let (mut rail, [top3, backlog, _, _, solo]) = tree();
    let below = rect_of(&rail, solo);
    assert_eq!(
        drag_to(&mut rail, backlog, below.y + below.h * 3.0),
        Some(TasksRailRequest::MoveList {
            id: backlog,
            parent: None,
            before: None
        })
    );
    let moved = rail.lists().iter().find(|l| l.id == backlog).unwrap();
    assert_eq!((moved.parent, moved.depth), (None, 0));
    assert!(
        rail.lists().iter().find(|l| l.id == top3).unwrap().has_children,
        "Core Data is still inside Top 3"
    );
}

#[test]
fn dragging_to_a_rows_top_edge_reorders_among_siblings() {
    let (mut rail, [_, _, _, other, solo]) = tree();
    let over = rect_of(&rail, other);
    assert_eq!(
        drag_to(&mut rail, solo, over.y + over.h * 0.1),
        Some(TasksRailRequest::MoveList {
            id: solo,
            parent: None,
            before: Some(other)
        })
    );
}

/// A list cannot go into itself or anything inside it. Those rows are not on
/// screen to be dropped on: the carried row is drawn shut.
#[test]
fn a_list_is_never_dropped_into_itself_or_its_own_children() {
    // Sweep the whole section, a sixth of a row at a time.
    for step in 0..40 {
        let (mut rail, [top3, backlog, core, _, _]) = tree();
        let first = rect_of(&rail, top3);
        let y = first.y + step as f32 * first.h / 6.0 + 1.0;
        let inside = [Some(top3), Some(backlog), Some(core)];
        if let Some(TasksRailRequest::MoveList { parent, .. }) = drag_to(&mut rail, top3, y) {
            assert!(
                !inside.contains(&parent),
                "step {step}: dropped into itself or a child"
            );
        }
        let now = rail.lists().iter().find(|l| l.id == top3).unwrap();
        assert!(!inside.contains(&now.parent), "step {step}");
        // What was inside it travelled with it.
        for child in [backlog, core] {
            let child = rail.lists().iter().find(|l| l.id == child).unwrap();
            assert_eq!(child.parent, Some(top3), "step {step}");
        }
    }
    // The arithmetic behind it refuses outright.
    let (rail, [top3, backlog, ..]) = tree();
    let mut lists = rail.lists().to_vec();
    let before = lists.clone();
    assert!(!apply_move(&mut lists, top3, Some(backlog), None));
    assert_eq!(lists, before, "and changes nothing");
}

#[test]
fn a_release_where_the_row_began_asks_for_nothing() {
    // *Other* sits right under an open list's last child: the slot where a
    // careless reading would put it down inside *Top 3*.
    let (mut rail, [_, _, _, other, _]) = tree();
    let r = rect_of(&rail, other);
    let (_, cy) = r.centre();
    let before = rail.lists().to_vec();
    // Out and back: armed, carried away, and returned to its own slot.
    assert_eq!(drag_to(&mut rail, other, cy), None);
    assert_eq!(rail.lists(), before);
    // A slip of the hand: lifted a few pixels and let go.
    let x = rail.row_parts(rail.row_of_list(other).unwrap(), r).label.x + 2.0;
    rail.mouse_down(area(), top(), x, cy, false);
    rail.mouse_move(area(), top(), x, cy - DRAG_ARM_PX - 2.0);
    assert_eq!(rail.mouse_up(), None);
    assert_eq!(rail.lists(), before);
}

#[test]
fn cancelling_a_drag_puts_the_table_back() {
    let (mut rail, [_, _, _, other, solo]) = tree();
    let before = rail.lists().to_vec();
    let r = rect_of(&rail, other);
    let x = rail.row_parts(rail.row_of_list(other).unwrap(), r).label.x + 2.0;
    let (_, cy) = r.centre();
    let (_, into) = rect_of(&rail, solo).centre();
    rail.mouse_down(area(), top(), x, cy, false);
    rail.mouse_move(area(), top(), x, cy + DRAG_ARM_PX + 1.0);
    rail.mouse_move(area(), top(), x, into);
    assert_ne!(rail.lists(), before, "the preview moved the row");
    assert!(rail.cancel_drag());
    assert_eq!(rail.lists(), before);
}

/// **A work count, not a timing.** A drag reads the cached rows: moving the
/// pointer inside one landing rebuilds nothing at all.
#[test]
fn a_carry_inside_one_landing_rebuilds_nothing() {
    let (mut rail, [_, _, _, other, solo]) = tree();
    let r = rect_of(&rail, other);
    let x = rail.row_parts(rail.row_of_list(other).unwrap(), r).label.x + 2.0;
    let (_, cy) = r.centre();
    let (_, into) = rect_of(&rail, solo).centre();
    rail.mouse_down(area(), top(), x, cy, false);
    rail.mouse_move(area(), top(), x, cy + DRAG_ARM_PX + 1.0);
    rail.mouse_move(area(), top(), x, into);

    let settled = rail.rebuilds();
    for wobble in 0..50 {
        rail.mouse_move(area(), top(), x + wobble as f32, into + (wobble % 3) as f32 - 1.0);
    }
    assert_eq!(rail.rebuilds(), settled);
    // Hit-testing and laying out read the cache too.
    for _ in 0..50 {
        rail.rows(area(), top());
        rail.at(area(), top(), x, into);
    }
    assert_eq!(rail.rebuilds(), settled);
}

/// The same, on the shapes that would hurt: a deep chain and a wide table
/// are each one rebuild per change of table, however many rows there are.
#[test]
fn a_deep_and_a_wide_table_are_each_built_once() {
    let mut deep: Vec<RailList> = Vec::new();
    for depth in 0..2_000 {
        let mut list = leaf("deep", deep.last(), 1, 0);
        list.has_children = depth < 1_999;
        deep.push(list);
    }
    let mut rail = TasksRail::default();
    rail.set_lists(deep);
    assert_eq!(rail.rebuilds(), 1);
    assert_eq!(rail.shown_counts(0), None);
    let root = rail.lists()[0].id;
    rail.set_open(root, false);
    assert_eq!(rail.shown_counts(0), Some((2_000, 0)));
    assert_eq!(rail.rebuilds(), 2);

    let wide: Vec<RailList> = (0..5_000).map(|_| leaf("wide", None, 1, 0)).collect();
    let mut rail = TasksRail::default();
    rail.set_lists(wide.clone());
    assert_eq!(rail.rebuilds(), 1);
    assert!(!rail.set_lists(wide), "the same table is not a change");
    assert_eq!(rail.rebuilds(), 1);
}

// -------------------------------------------------------------- the keyboard

fn key(rail: &mut TasksRail, named: NamedKey) -> Option<Outcome> {
    rail.queue_key(area(), top(), &WKey::Named(named))
}

/// One ring, in the order the rows are drawn: the queues, the `+`, the lists
/// that are showing, then Completed.
#[test]
fn the_keyboard_walks_queues_the_plus_the_lists_then_completed() {
    let (mut rail, [top3, backlog, ..]) = tree();
    assert!(rail.focus_queues(area(), top()));
    assert_eq!(rail.focus(), Some(RailFocus::Bucket(Bucket::Today)));
    for _ in 0..4 {
        key(&mut rail, NamedKey::ArrowDown);
    }
    assert_eq!(rail.focus(), Some(RailFocus::NewList));
    assert_eq!(
        key(&mut rail, NamedKey::Enter),
        Some(Outcome::Tasks(TasksRailRequest::BeginNewList {
            parent: None
        }))
    );
    key(&mut rail, NamedKey::ArrowDown);
    assert_eq!(rail.focus(), Some(RailFocus::List(top3)));
    key(&mut rail, NamedKey::ArrowDown);
    assert_eq!(rail.focus(), Some(RailFocus::List(backlog)));
    key(&mut rail, NamedKey::End);
    assert_eq!(rail.focus(), Some(RailFocus::Bucket(Bucket::Completed)));
}

/// Left and Right shut and open a row, as in the document tree, and then
/// step out and in.
#[test]
fn left_and_right_shut_open_and_step_through_the_tree() {
    let (mut rail, [top3, backlog, ..]) = tree();
    rail.set_focus(area(), top(), RailFocus::List(top3));

    key(&mut rail, NamedKey::ArrowLeft);
    assert!(!rail.is_open(top3), "Left shuts an open parent");
    assert_eq!(rail.focus(), Some(RailFocus::List(top3)));
    key(&mut rail, NamedKey::ArrowRight);
    assert!(rail.is_open(top3), "Right opens it");
    key(&mut rail, NamedKey::ArrowRight);
    assert_eq!(rail.focus(), Some(RailFocus::List(backlog)), "then steps in");
    key(&mut rail, NamedKey::ArrowLeft);
    assert_eq!(rail.focus(), Some(RailFocus::List(top3)), "and Left steps out");
}

/// The menu key opens the focused row's menu — which is where *New list
/// inside*, *Move to top level* and *Delete* are, without a pointer.
#[test]
fn the_menu_key_opens_the_focused_lists_menu() {
    let (mut rail, [_, backlog, ..]) = tree();
    rail.set_focus(area(), top(), RailFocus::List(backlog));
    let Some(Outcome::Tasks(TasksRailRequest::RowMenu { target, at })) =
        key(&mut rail, NamedKey::ContextMenu)
    else {
        panic!("the row's menu");
    };
    assert_eq!(target, RailTarget::List(backlog));
    let r = rect_of(&rail, backlog);
    assert!(at.0 >= r.x && at.0 <= r.x + r.w, "hung from the row itself");
    assert_eq!(rail.focus(), Some(RailFocus::List(backlog)), "focus stays");
}

/// Where the keyboard goes when a list is deleted: the row above it.
#[test]
fn the_row_above_takes_the_keyboard_after_a_delete() {
    let (rail, [top3, backlog, _, other, _]) = tree();
    assert_eq!(rail.focus_above(backlog), RailFocus::List(top3));
    assert_eq!(rail.focus_above(other), RailFocus::List(rail.lists()[2].id));
    assert_eq!(rail.focus_above(top3), RailFocus::NewList);
}

/// Focusing a list under a shut parent opens the way to it.
#[test]
fn revealing_a_list_opens_every_parent_above_it() {
    let (mut rail, [top3, backlog, ..]) = tree();
    rail.set_open(top3, false);
    assert!(!showing(&rail).contains(&backlog));
    assert!(rail.reveal_list(area(), top(), backlog));
    assert!(showing(&rail).contains(&backlog));
}
