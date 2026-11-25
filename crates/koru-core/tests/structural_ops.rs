// SPDX-License-Identifier: MPL-2.0

//! Structural editing: reorder, indent, split and merge.
//!
//! These are the operations behind Enter, Backspace, Alt+Arrow and drag-drop.
//! They live in the model rather than the editor because a document view and a
//! kanban need the same behaviour, and because splitting a line has to carry
//! marks, pills and children with it — which only the model can do.

use koru_core::node::{Attr, AttrValue, Content, Node};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{Heading, Mark, NodeId, WorkspaceId};

fn ws() -> WorkspaceDoc {
    WorkspaceDoc::create(WorkspaceId::new(), "test").unwrap()
}

/// Build an outline from `(text, depth)` pairs, the way a user would type it.
fn outline(rows: &[(&str, usize)]) -> (WorkspaceDoc, Vec<NodeId>) {
    let mut ws = ws();
    let mut stack: Vec<NodeId> = Vec::new();
    let mut ids = Vec::new();
    for (text, depth) in rows {
        let parent = match depth {
            0 => Parent::Root,
            d => Parent::Node(stack[d - 1]),
        };
        let id = ws.insert(parent, Node::task(*text).unwrap()).unwrap();
        stack.truncate(*depth);
        stack.push(id);
        ids.push(id);
    }
    (ws, ids)
}

/// The visible outline as `(text, depth)`, which is what the viewer draws.
fn shape(ws: &WorkspaceDoc) -> Vec<(String, usize)> {
    ws.walk()
        .unwrap()
        .into_iter()
        .map(|(id, depth)| {
            (
                ws.read(id)
                    .unwrap()
                    .content
                    .as_text()
                    .unwrap_or_default()
                    .to_string(),
                depth,
            )
        })
        .collect()
}

fn text_of(ws: &WorkspaceDoc, id: NodeId) -> String {
    ws.read(id)
        .unwrap()
        .content
        .as_text()
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------- reordering

#[test]
fn shift_moves_a_node_among_its_siblings() {
    let (mut ws, ids) = outline(&[("one", 0), ("two", 0), ("three", 0)]);

    assert!(ws.shift(ids[2], -1).unwrap());
    assert_eq!(
        shape(&ws),
        vec![("one".into(), 0), ("three".into(), 0), ("two".into(), 0)]
    );

    assert!(ws.shift(ids[2], 1).unwrap());
    assert_eq!(
        shape(&ws),
        vec![("one".into(), 0), ("two".into(), 0), ("three".into(), 0)]
    );
}

/// Alt+↑ on the first row is a no-op, not a failure.
#[test]
fn shift_stops_at_the_ends() {
    let (mut ws, ids) = outline(&[("one", 0), ("two", 0)]);
    assert!(!ws.shift(ids[0], -1).unwrap());
    assert!(!ws.shift(ids[1], 1).unwrap());
    assert_eq!(shape(&ws), vec![("one".into(), 0), ("two".into(), 0)]);
}

/// Reordering carries the subtree; the children were never in a separate list.
#[test]
fn shifting_a_group_takes_its_children() {
    let (mut ws, ids) = outline(&[("Phase 1", 0), ("Reward", 1), ("Phase 2", 0)]);
    assert!(ws.shift(ids[0], 1).unwrap());
    assert_eq!(
        shape(&ws),
        vec![
            ("Phase 2".into(), 0),
            ("Phase 1".into(), 0),
            ("Reward".into(), 1)
        ]
    );
}

// ------------------------------------------------------------------ indent

#[test]
fn indent_makes_a_node_the_last_child_of_the_row_above() {
    let (mut ws, ids) = outline(&[("Phase 1", 0), ("Reward", 0), ("Invite", 0)]);

    assert!(ws.indent(ids[1]).unwrap());
    assert!(ws.indent(ids[2]).unwrap());
    assert_eq!(
        shape(&ws),
        vec![
            ("Phase 1".into(), 0),
            ("Reward".into(), 1),
            ("Invite".into(), 1)
        ],
        "the second indent lands beside the first, not under it"
    );
}

/// The rule from the brief: a child of a level-1 task reaches level 2 and
/// stops. With a real tree this is not a clamp — pressing again simply finds no
/// sibling above to move under.
#[test]
fn indent_can_never_skip_a_level() {
    let (mut ws, ids) = outline(&[("Phase 1", 0), ("Reward", 0)]);
    for _ in 0..6 {
        ws.indent(ids[1]).unwrap();
    }
    assert_eq!(
        shape(&ws),
        vec![("Phase 1".into(), 0), ("Reward".into(), 1)]
    );
}

#[test]
fn the_first_row_has_nothing_to_indent_under() {
    let (mut ws, ids) = outline(&[("only", 0)]);
    assert!(!ws.indent(ids[0]).unwrap());
    assert_eq!(shape(&ws), vec![("only".into(), 0)]);
}

#[test]
fn outdent_reattaches_below_the_old_parent() {
    let (mut ws, ids) = outline(&[("Phase 1", 0), ("Reward", 1), ("Legal", 0)]);
    assert!(ws.outdent(ids[1]).unwrap());
    assert_eq!(
        shape(&ws),
        vec![
            ("Phase 1".into(), 0),
            ("Reward".into(), 0),
            ("Legal".into(), 0)
        ],
        "it lands immediately after its old parent, not at the end"
    );
    assert!(!ws.outdent(ids[1]).unwrap(), "already at the root");
}

/// Plain outdent leaves following siblings under the old parent, so they draw
/// *above* the moved row. List editing needs the opposite: the row stays above
/// those siblings and they become its children.
#[test]
fn outdent_keeping_order_takes_following_siblings_as_children() {
    let (mut ws, ids) = outline(&[("three", 0), ("empty", 1), ("four", 1)]);
    assert!(ws.outdent_keeping_order(ids[1]).unwrap());
    assert_eq!(
        shape(&ws),
        vec![("three".into(), 0), ("empty".into(), 0), ("four".into(), 1),],
        "empty stays above four; four nests under empty"
    );
    assert_eq!(
        ws.children(Parent::Node(ids[1])).unwrap(),
        vec![ids[2]],
        "four is now a child of empty"
    );
}

#[test]
fn outdent_keeping_order_without_following_siblings_matches_plain_outdent() {
    let (mut ws, ids) = outline(&[("Phase 1", 0), ("Reward", 1), ("Legal", 0)]);
    assert!(ws.outdent_keeping_order(ids[1]).unwrap());
    assert_eq!(
        shape(&ws),
        vec![
            ("Phase 1".into(), 0),
            ("Reward".into(), 0),
            ("Legal".into(), 0)
        ]
    );
}

#[test]
fn outdent_within_keeping_order_stops_at_the_boundary() {
    let (mut ws, ids) = outline(&[("only", 0)]);
    assert!(!ws.outdent_within_keeping_order(ids[0], ids[0]).unwrap());
    // Root children have parent Root, not the node itself — boundary is the
    // document root for list items. Build a child under a parent boundary.
    let (mut ws, ids) = outline(&[("parent", 0), ("child", 1)]);
    assert!(!ws.outdent_within_keeping_order(ids[1], ids[0]).unwrap());
    assert_eq!(
        shape(&ws),
        vec![("parent".into(), 0), ("child".into(), 1)],
        "refused rather than escaping the boundary"
    );
}

/// A leaf kind cannot adopt, so indenting under one is refused rather than
/// producing a tree the shape rules forbid.
#[test]
fn indenting_under_a_leaf_is_refused() {
    let mut ws = ws();
    ws.insert(
        Parent::Root,
        Node::new(koru_core::Kind::Contact, Content::text("Kim")).unwrap(),
    )
    .unwrap();
    let task = ws
        .insert(Parent::Root, Node::task("Reward").unwrap())
        .unwrap();

    assert!(!ws.indent(task).unwrap());
    assert_eq!(ws.depth(task).unwrap(), 0);
}

// ------------------------------------------------------------------- split

#[test]
fn split_moves_the_tail_into_a_new_sibling() {
    let (mut ws, ids) = outline(&[("Reward payout", 0), ("Legal", 0)]);
    let tail = ws.split(ids[0], 6).unwrap();

    assert_eq!(text_of(&ws, ids[0]), "Reward");
    assert_eq!(text_of(&ws, tail), " payout");
    assert_eq!(
        shape(&ws),
        vec![
            ("Reward".into(), 0),
            (" payout".into(), 0),
            ("Legal".into(), 0)
        ],
        "the new node sits directly below, above the next row"
    );
}

/// Enter at the end of a heading should land inside its subtree, not below it.
#[test]
fn splitting_a_group_puts_the_tail_first_among_its_children() {
    let (mut ws, ids) = outline(&[("Phase 1", 0), ("Reward", 1)]);
    let tail = ws.split(ids[0], 7).unwrap();

    assert_eq!(text_of(&ws, tail), "");
    assert_eq!(
        shape(&ws),
        vec![("Phase 1".into(), 0), ("".into(), 1), ("Reward".into(), 1)]
    );
}

/// Marks are on the text, so the tail keeps its formatting without the editor
/// re-parsing anything.
#[test]
fn split_carries_marks_across() {
    let (mut ws, ids) = outline(&[("plain bold", 0)]);
    ws.set_mark(ids[0], 6..10, Mark::Bold, true).unwrap();

    let tail = ws.split(ids[0], 6).unwrap();
    assert_eq!(text_of(&ws, tail), "bold");
    let runs = ws.runs(tail).unwrap();
    assert!(runs.iter().all(|r| r.bold), "the whole tail is still bold");

    let head = ws.runs(ids[0]).unwrap();
    assert!(!head.iter().any(|r| r.bold), "and the head is not");
}

#[test]
fn split_carries_a_pill_and_its_attribute() {
    let (mut ws, ids) = outline(&[("Due ", 0)]);
    let attr = Attr::new(
        "due",
        AttrValue::Date(koru_core::time::DateOnly::new(
            chrono::NaiveDate::from_ymd_opt(2026, 8, 2).unwrap(),
        )),
    );
    ws.insert_pill(ids[0], 4, &attr).unwrap();

    let tail = ws.split(ids[0], 4).unwrap();
    assert_eq!(
        ws.read(tail).unwrap().due(),
        Some(koru_core::time::Temporal::Date(
            koru_core::time::DateOnly::new(chrono::NaiveDate::from_ymd_opt(2026, 8, 2).unwrap())
        )),
        "the attribute moved with the anchor, so a query still sees the date"
    );
    assert!(ws.runs(tail).unwrap().iter().any(|r| r.is_pill()));
    assert!(
        ws.read(ids[0]).unwrap().content.attrs().is_empty(),
        "and the head no longer claims it"
    );
}

/// Splitting at the very end is Enter on a finished line: a fresh empty row.
#[test]
fn splitting_at_the_end_gives_an_empty_row() {
    let (mut ws, ids) = outline(&[("done typing", 0)]);
    let tail = ws.split(ids[0], 11).unwrap();
    assert_eq!(text_of(&ws, ids[0]), "done typing");
    assert_eq!(text_of(&ws, tail), "");
}

// ------------------------------------------------------------------- merge

#[test]
fn merge_joins_two_lines_at_the_seam() {
    let (mut ws, ids) = outline(&[("Reward", 0), (" payout", 0)]);
    let seam = ws.merge_into(ids[0], ids[1]).unwrap();

    assert_eq!(seam, 6);
    assert_eq!(text_of(&ws, ids[0]), "Reward payout");
    assert_eq!(shape(&ws), vec![("Reward payout".into(), 0)]);
    assert!(ws.read(ids[1]).is_err(), "the source is gone");
}

#[test]
fn merge_keeps_the_sources_formatting() {
    let (mut ws, ids) = outline(&[("plain ", 0), ("bold", 0)]);
    ws.set_mark(ids[1], 0..4, Mark::Bold, true).unwrap();

    ws.merge_into(ids[0], ids[1]).unwrap();
    let runs = ws.runs(ids[0]).unwrap();
    assert_eq!(text_of(&ws, ids[0]), "plain bold");
    let bold: String = runs
        .iter()
        .filter(|r| r.bold)
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(bold, "bold", "exactly the text that was bold, and no more");
}

/// Backspacing a heading into the row above must not strand its subtree.
#[test]
fn merge_adopts_the_sources_children() {
    let (mut ws, ids) = outline(&[("Phase 1", 0), ("Phase 2", 0), ("Reward", 1)]);
    ws.merge_into(ids[0], ids[1]).unwrap();

    assert_eq!(
        shape(&ws),
        vec![("Phase 1Phase 2".into(), 0), ("Reward".into(), 1)]
    );
}

#[test]
fn a_split_and_a_merge_round_trip() {
    let (mut ws, ids) = outline(&[("Reward payout flow", 0)]);
    ws.set_mark(ids[0], 0..6, Mark::Bold, true).unwrap();

    let tail = ws.split(ids[0], 6).unwrap();
    let seam = ws.merge_into(ids[0], tail).unwrap();

    assert_eq!(seam, 6);
    assert_eq!(text_of(&ws, ids[0]), "Reward payout flow");
    let bold: String = ws
        .runs(ids[0])
        .unwrap()
        .iter()
        .filter(|r| r.bold)
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(bold, "Reward", "the mark survived the round trip intact");
}

// ------------------------------------------------------------------ lookups

#[test]
fn parent_and_sibling_index_describe_the_position() {
    let (ws, ids) = outline(&[("Phase 1", 0), ("Reward", 1), ("Invite", 1)]);

    assert!(matches!(ws.parent(ids[0]), Ok(Parent::Root)));
    assert!(matches!(ws.parent(ids[2]), Ok(Parent::Node(p)) if p == ids[0]));
    assert_eq!(ws.sibling_index(ids[1]).unwrap(), 0);
    assert_eq!(ws.sibling_index(ids[2]).unwrap(), 1);
}

#[test]
fn text_len_is_in_bytes_so_it_matches_the_layout() {
    let (mut ws, ids) = outline(&[("보상", 0)]);
    assert_eq!(ws.text_len(ids[0]).unwrap(), 6, "two three-byte syllables");
    ws.insert_text(ids[0], 6, " ok").unwrap();
    assert_eq!(ws.text_len(ids[0]).unwrap(), 9);
}

// ----------------------------------------------------------- references

/// A node of any kind can be pointed at by another.
///
/// This used to assert something stronger and now gone: that a contact was
/// *referenceable but not a row*, because it lived in the library tree and the
/// outline could not see it. Two trees were the whole mechanism, and there is
/// one now — so a contact is a row like anything else. What survives, and is
/// the part that ever mattered to a caller, is that the reference resolves.
#[test]
fn a_node_can_be_referenced_by_another() {
    let mut ws = ws();
    let kim = ws
        .insert(
            Parent::Root,
            Node::new(koru_core::Kind::Contact, Content::text("Kim")).unwrap(),
        )
        .unwrap();
    let task = ws
        .insert(Parent::Root, Node::task("Reward").unwrap())
        .unwrap();

    assert_eq!(ws.children(Parent::Root).unwrap(), vec![kim, task]);

    // ...and a task can point at it, name and all.
    ws.set_attr(
        task,
        &Attr::new("assignee", AttrValue::Ref(koru_core::Ref::new(kim))),
    )
    .unwrap();
    let found = ws.read(task).unwrap().assignees().next().unwrap();
    assert!(matches!(
        ws.resolve(found),
        koru_core::ResolvedRef::Found { .. }
    ));
}

/// The realm half of this test went with the realms. What it was *also*
/// covering — that a reload rebuilds the id index, without which every later
/// operation looks up a node that is not there — is the half worth keeping.
#[test]
fn a_node_survives_a_snapshot_round_trip() {
    let mut ws = ws();
    let kim = ws
        .insert(
            Parent::Root,
            Node::new(koru_core::Kind::Contact, Content::text("Kim")).unwrap(),
        )
        .unwrap();

    let bytes = ws.export_snapshot().unwrap();
    let back = WorkspaceDoc::from_snapshot(&bytes).unwrap();

    assert_eq!(text_of(&back, kim), "Kim");
    assert_eq!(
        back.walk()
            .unwrap()
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        vec![kim],
        "reindex found it, and it is a row like any other now"
    );
}

/// Outdenting bottoms out at the document's root.
#[test]
fn outdent_bottoms_out_at_the_root() {
    let mut ws = ws();
    let folder = ws
        .insert(Parent::Root, Node::task("People").unwrap())
        .unwrap();
    let kim = ws
        .insert(
            Parent::Node(folder),
            Node::new(koru_core::Kind::Contact, Content::text("Kim")).unwrap(),
        )
        .unwrap();

    assert!(ws.outdent(kim).unwrap());
    assert_eq!(ws.children(Parent::Root).unwrap(), vec![folder, kim]);
    assert!(!ws.outdent(kim).unwrap(), "already at the top of its realm");
}

// --------------------------------------------------------------- alignment

/// Alignment is a block property, so it round-trips through the node's meta
/// rather than through a mark on a range.
#[test]
fn alignment_round_trips_through_a_snapshot() {
    let mut ws = ws();
    let a = ws
        .insert(Parent::Root, Node::task("centred").unwrap())
        .unwrap();
    let b = ws
        .insert(Parent::Root, Node::task("plain").unwrap())
        .unwrap();

    ws.set_align(a, Some(koru_core::Align::Center)).unwrap();

    let back = WorkspaceDoc::from_snapshot(&ws.export_snapshot().unwrap()).unwrap();
    assert_eq!(
        back.read(a).unwrap().meta.align,
        Some(koru_core::Align::Center)
    );
    assert_eq!(
        back.read(b).unwrap().meta.align,
        None,
        "a node that was never aligned stays unaligned, not defaulted"
    );
}

/// `None` restores the default rather than storing "start", so a document
/// written before alignment existed reads back identically.
#[test]
fn clearing_alignment_removes_it() {
    let mut ws = ws();
    let id = ws.insert(Parent::Root, Node::task("row").unwrap()).unwrap();

    ws.set_align(id, Some(koru_core::Align::End)).unwrap();
    assert_eq!(ws.read(id).unwrap().meta.align, Some(koru_core::Align::End));

    ws.set_align(id, None).unwrap();
    assert_eq!(ws.read(id).unwrap().meta.align, None);
}

/// Splitting a line is a text operation; alignment belongs to the block, so the
/// tail starts fresh rather than inheriting it.
#[test]
fn a_split_does_not_carry_alignment_to_the_tail() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::task("centred line").unwrap())
        .unwrap();
    ws.set_align(id, Some(koru_core::Align::Center)).unwrap();

    let tail = ws.split(id, 7).unwrap();
    assert_eq!(
        ws.read(id).unwrap().meta.align,
        Some(koru_core::Align::Center)
    );
    assert_eq!(ws.read(tail).unwrap().meta.align, None);
}

// ---------------------------------------------------------------- headings

/// A heading level is a block property like alignment, stored on the node's
/// meta as its number rather than as a mark on a range.
#[test]
fn a_heading_level_round_trips_through_a_snapshot() {
    let mut ws = ws();
    let a = ws
        .insert(Parent::Root, Node::task("a title").unwrap())
        .unwrap();
    let b = ws
        .insert(Parent::Root, Node::task("prose").unwrap())
        .unwrap();

    ws.set_heading(a, Some(Heading::H2)).unwrap();

    let back = WorkspaceDoc::from_snapshot(&ws.export_snapshot().unwrap()).unwrap();
    assert_eq!(back.read(a).unwrap().meta.heading, Some(Heading::H2));
    assert_eq!(
        back.read(b).unwrap().meta.heading,
        None,
        "a node that was never a heading stays body text, not defaulted"
    );
}

/// `None` removes the key rather than storing a level, so a document written
/// before headings existed reads back identically.
#[test]
fn setting_a_heading_to_none_returns_the_block_to_body_text() {
    let mut ws = ws();
    let id = ws.insert(Parent::Root, Node::task("row").unwrap()).unwrap();

    ws.set_heading(id, Some(Heading::H1)).unwrap();
    assert_eq!(ws.read(id).unwrap().meta.heading, Some(Heading::H1));

    ws.set_heading(id, None).unwrap();
    assert_eq!(ws.read(id).unwrap().meta.heading, None);
}

/// Enter at the end of a heading means "now write under it", so the tail is
/// body text. A second empty heading is never what was meant.
#[test]
fn a_split_at_the_end_of_a_heading_gives_a_body_paragraph() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::task("a title").unwrap())
        .unwrap();
    ws.set_heading(id, Some(Heading::H2)).unwrap();

    let tail = ws.split(id, 7).unwrap();
    assert_eq!(ws.read(id).unwrap().meta.heading, Some(Heading::H2));
    assert_eq!(ws.read(tail).unwrap().meta.heading, None);
}

/// Breaking a title in two makes two titles — the opposite of what alignment
/// does, and the reason `split` reads the heading at all.
#[test]
fn a_split_in_the_middle_of_a_heading_gives_two_headings() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::task("one two").unwrap())
        .unwrap();
    ws.set_heading(id, Some(Heading::H3)).unwrap();

    let tail = ws.split(id, 3).unwrap();
    assert_eq!(ws.read(id).unwrap().meta.heading, Some(Heading::H3));
    assert_eq!(ws.read(tail).unwrap().meta.heading, Some(Heading::H3));
}

/// The same rule at the other edge: the text keeps its level and an empty
/// heading is left above it, rather than the text being demoted to prose.
#[test]
fn a_split_at_the_start_of_a_heading_leaves_an_empty_heading_above_it() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::task("a title").unwrap())
        .unwrap();
    ws.set_heading(id, Some(Heading::H1)).unwrap();

    let tail = ws.split(id, 0).unwrap();
    assert_eq!(ws.read(id).unwrap().meta.heading, Some(Heading::H1));
    assert_eq!(ws.read(tail).unwrap().meta.heading, Some(Heading::H1));
}

/// Backspace joining a paragraph into the heading above keeps the heading —
/// `merge_into` touches no meta, and that is the right answer here.
#[test]
fn merging_a_paragraph_into_a_heading_keeps_the_heading() {
    let mut ws = ws();
    let head = ws
        .insert(Parent::Root, Node::task("Title").unwrap())
        .unwrap();
    let body = ws
        .insert(Parent::Root, Node::task(" and prose").unwrap())
        .unwrap();
    ws.set_heading(head, Some(Heading::H2)).unwrap();

    ws.merge_into(head, body).unwrap();
    assert_eq!(ws.read(head).unwrap().meta.heading, Some(Heading::H2));
}

// ------------------------------------------------------------ insert_after

/// `insert_after` lands a node in the slot directly below its anchor, pushing
/// the rest down — the operation a paste into the middle of a list needs, and
/// which `insert` (an append) does not express.
#[test]
fn insert_after_lands_directly_below_the_anchor() {
    let (mut ws, ids) = outline(&[("one", 0), ("two", 0), ("three", 0)]);

    let new = ws
        .insert_after(ids[0], Node::task("one-and-a-half").unwrap())
        .unwrap();

    assert_eq!(
        shape(&ws),
        vec![
            ("one".into(), 0),
            ("one-and-a-half".into(), 0),
            ("two".into(), 0),
            ("three".into(), 0),
        ]
    );
    assert_eq!(ws.sibling_index(new).unwrap(), 1);
}

/// The anchor at the end of a list is the common case for a paste: the new
/// node becomes the last sibling.
#[test]
fn insert_after_the_last_row_appends() {
    let (mut ws, ids) = outline(&[("one", 0), ("two", 0)]);

    let new = ws
        .insert_after(ids[1], Node::task("three").unwrap())
        .unwrap();

    assert_eq!(ws.sibling_index(new).unwrap(), 2);
    assert_eq!(
        shape(&ws),
        vec![("one".into(), 0), ("two".into(), 0), ("three".into(), 0),]
    );
}

/// `insert_after` is what a multi-row paste threads through: each landed node
/// becomes the anchor for the next, so a sequence keeps its order and stays at
/// one depth.
#[test]
fn insert_after_threads_to_keep_paste_order() {
    let (mut ws, ids) = outline(&[("head", 0)]);

    let mut anchor = ids[0];
    for text in ["a", "b", "c"] {
        anchor = ws.insert_after(anchor, Node::task(text).unwrap()).unwrap();
    }

    assert_eq!(
        shape(&ws),
        vec![
            ("head".into(), 0),
            ("a".into(), 0),
            ("b".into(), 0),
            ("c".into(), 0),
        ]
    );
}

// ----------------------------------------------------------- delete (promote)

/// Deleting a node promotes its direct children into its place rather than
/// taking the subtree down with it — the model half of Ctrl+D on a bullet.
#[test]
fn deleting_a_node_promotes_its_children_in_order() {
    let (mut ws, ids) = outline(&[("Phase", 0), ("Reward", 1), ("Detail", 2), ("Legal", 0)]);

    let promoted = ws.delete_promoting_children(ids[0]).unwrap();
    assert_eq!(promoted, vec![ids[1]]);
    assert_eq!(
        shape(&ws),
        vec![
            ("Reward".into(), 0),
            ("Detail".into(), 1),
            ("Legal".into(), 0)
        ],
        "the children outdent into the deleted row's place, keeping their shape"
    );
    assert!(ws.read(ids[0]).is_err(), "and the row itself is gone");
}

/// A node with no children is simply deleted; nothing is promoted.
#[test]
fn deleting_a_childless_node_is_a_plain_delete() {
    let (mut ws, ids) = outline(&[("one", 0), ("two", 0)]);

    let promoted = ws.delete_promoting_children(ids[1]).unwrap();
    assert!(promoted.is_empty());
    assert_eq!(shape(&ws), vec![("one".into(), 0)]);
}

// ------------------------------------------- moving a selection out, by pill

/// A document body for the extraction tests: a root, three paragraphs, one
/// nested child under the middle one — the shape a multi-row selection has.
fn document_body() -> (WorkspaceDoc, NodeId, Vec<NodeId>) {
    let mut ws = ws();
    let root = ws
        .insert(Parent::Root, Node::document("Source").unwrap())
        .unwrap();
    let ids = ["before", "first moved", "second moved", "after"]
        .iter()
        .map(|text| {
            ws.insert(Parent::Node(root), Node::paragraph(*text).unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    ws.insert(Parent::Node(ids[1]), Node::paragraph("nested").unwrap())
        .unwrap();
    (ws, root, ids)
}

fn document_attr() -> Attr {
    Attr::labelled("document", AttrValue::Text("Target".into()), "Target")
}

#[test]
fn replacing_several_subtrees_leaves_one_pill_in_the_first_slot() {
    let (mut ws, root, ids) = document_body();

    let replacement = ws
        .replace_subtrees_with_pill(&[ids[1], ids[2]], &document_attr())
        .unwrap();

    assert_eq!(
        ws.children(Parent::Node(root)).unwrap(),
        vec![ids[0], replacement, ids[3]],
        "one paragraph takes the first target's place"
    );
    assert!(ws.read(ids[1]).is_err(), "the first subtree is gone");
    assert!(ws.read(ids[2]).is_err(), "and the second");
    assert_eq!(ws.read(replacement).unwrap().content.attrs().len(), 1);
}

#[test]
fn replacing_no_subtrees_is_refused() {
    let (mut ws, _, _) = document_body();
    assert!(
        ws.replace_subtrees_with_pill(&[], &document_attr())
            .is_err()
    );
}

#[test]
fn a_document_root_among_the_targets_refuses_the_whole_replacement() {
    let (mut ws, root, ids) = document_body();

    assert!(
        ws.replace_subtrees_with_pill(&[ids[1], root], &document_attr())
            .is_err(),
        "a document root cannot be replaced with a reference"
    );
    assert!(
        ws.read(ids[1]).is_ok(),
        "and the valid target was not deleted on the way to the refusal"
    );
}

#[test]
fn insert_runs_recreates_a_pill_and_drops_an_orphan() {
    let (mut ws, root, ids) = document_body();
    let attr = document_attr();
    let source_id = attr.id;
    ws.insert_pill(ids[0], "before".len(), &attr).unwrap();
    let runs = ws.runs(ids[0]).unwrap();
    let attrs = vec![attr];

    // With the attribute beside the runs, the pill is rebuilt with a fresh
    // AttrId so pasting never duplicates the source pill's identity.
    let target = ws
        .insert(Parent::Node(root), Node::paragraph("").unwrap())
        .unwrap();
    ws.insert_runs(target, 0, &runs, &attrs);
    let pasted_attrs = ws.read(target).unwrap().content.attrs().to_vec();
    assert_eq!(pasted_attrs.len(), 1);
    assert_ne!(
        pasted_attrs[0].id, source_id,
        "pasting a pill must mint a fresh AttrId"
    );
    let pasted_runs = ws.runs(target).unwrap();
    assert!(
        pasted_runs
            .iter()
            .any(|r| r.pill == Some(pasted_attrs[0].id)),
        "the anchor carries the fresh pill AttrId"
    );

    // Without it, the anchor is dropped rather than written blind.
    let bare = ws
        .insert(Parent::Node(root), Node::paragraph("").unwrap())
        .unwrap();
    ws.insert_runs(bare, 0, &runs, &[]);
    assert_eq!(ws.read(bare).unwrap().content.attrs().len(), 0);
    assert!(ws.runs(bare).unwrap().iter().all(|r| !r.is_pill()));
    assert_eq!(text_of(&ws, bare), "before");
}

#[test]
fn duplicate_mints_fresh_pill_attr_ids() {
    let (mut ws, root, ids) = document_body();
    let attr = document_attr();
    ws.insert_pill(ids[0], "before".len(), &attr).unwrap();

    let copy = ws.duplicate(ids[0], Parent::Node(root), "").unwrap();
    let copy_attrs = ws.read(copy).unwrap().content.attrs().to_vec();
    assert_eq!(copy_attrs.len(), 1);
    assert_ne!(
        copy_attrs[0].id, attr.id,
        "duplicating a node must mint fresh AttrIds for its attributes"
    );
    assert_eq!(copy_attrs[0].role, attr.role);
    assert_eq!(copy_attrs[0].value, attr.value);
    let copy_runs = ws.runs(copy).unwrap();
    assert!(
        copy_runs.iter().any(|r| r.pill == Some(copy_attrs[0].id)),
        "the duplicated pill anchor points at the remapped AttrId"
    );
}

/// The trail pill stands where the first block stood, so it keeps that
/// block's shape: a heading stays a heading, a task keeps its checkbox.
#[test]
fn the_replacement_pill_keeps_the_first_blocks_shape() {
    let (mut ws, root, ids) = document_body();
    ws.set_heading(ids[1], Some(Heading::H2)).unwrap();

    let replacement = ws
        .replace_subtrees_with_pill(&[ids[1], ids[2]], &document_attr())
        .unwrap();
    let node = ws.read(replacement).unwrap();
    assert_eq!(node.meta.heading, Some(Heading::H2), "the heading survived");

    // And a task first-target keeps its kind through the single-subtree form.
    let task = ws
        .insert(Parent::Node(root), Node::task("tick me").unwrap())
        .unwrap();
    let replacement = ws
        .replace_subtree_with_pill(task, &document_attr())
        .unwrap();
    assert_eq!(ws.read(replacement).unwrap().kind, koru_core::Kind::Task);
}
