// SPDX-License-Identifier: MPL-2.0

//! A repaired update is one the real `accesskit_consumer` accepts.
//!
//! Every case is handed to the consumer itself, because the rule being kept is
//! the consumer's and a copy of it here could share whatever this module got
//! wrong. The field crash was *"TreeUpdate includes duplicate child"*, and the
//! first test reproduces it before it shows the repair.

use std::panic::{AssertUnwindSafe, catch_unwind};

use accesskit::{Node, NodeId, Role, Tree, TreeId, TreeUpdate};
use accesskit_consumer::{Tree as Consumer, TreeChangeHandler as ChangeHandler};
use koru_draw::geometry::Rect;

use super::{Repair, repair};
use crate::accessibility::identity::{ControlPart, Occurrence, ViewSlot, WindowSlot};
use crate::accessibility::projection::{
    self, Runs, Scope, SemanticNode, TextBody, WindowSemantics,
};

const ROOT: NodeId = NodeId(1);

struct Ignore;

impl ChangeHandler for Ignore {
    fn node_added(&mut self, _: &accesskit_consumer::Node) {}
    fn node_updated(&mut self, _: &accesskit_consumer::Node, _: &accesskit_consumer::Node) {}
    fn focus_moved(
        &mut self,
        _: Option<&accesskit_consumer::Node>,
        _: Option<&accesskit_consumer::Node>,
    ) {
    }
    fn node_removed(&mut self, _: &accesskit_consumer::Node) {}
}

/// Whether the consumer accepts `update` as a first tree.
fn accepted_whole(update: &TreeUpdate) -> bool {
    rejection(update).is_none()
}

/// The consumer's panic message for `update` as a first tree, if it panics.
fn rejection(update: &TreeUpdate) -> Option<String> {
    let update = update.clone();
    let payload = catch_unwind(AssertUnwindSafe(|| {
        Consumer::new(update, true);
    }))
    .err()?;
    Some(
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default(),
    )
}

/// Whether the consumer accepts `delta` on top of `whole`.
fn accepted_delta(whole: &TreeUpdate, delta: &TreeUpdate) -> bool {
    let (whole, delta) = (whole.clone(), delta.clone());
    catch_unwind(AssertUnwindSafe(|| {
        let mut tree = Consumer::new(whole, true);
        tree.update_and_process_changes(delta, &mut Ignore);
    }))
    .is_ok()
}

fn parent(role: Role, locator: &str, children: Vec<NodeId>) -> Node {
    let mut node = Node::new(role);
    node.set_author_id(locator.to_string());
    node.set_children(children);
    node
}

fn leaf(role: Role) -> Node {
    Node::new(role)
}

fn whole(nodes: Vec<(NodeId, Node)>, focus: NodeId) -> TreeUpdate {
    TreeUpdate {
        nodes,
        tree: Some(Tree::new(ROOT)),
        tree_id: TreeId::ROOT,
        focus,
    }
}

fn ids(update: &TreeUpdate) -> Vec<NodeId> {
    update.nodes.iter().map(|(id, _)| *id).collect()
}

/// **The field crash.** A paragraph published twice: its runs and the
/// paragraph appear twice, and the body names it twice. The consumer panics on
/// the first repeated child; repaired, it takes one paragraph.
#[test]
fn a_control_published_twice_no_longer_takes_the_window_down() {
    let (body, paragraph, run) = (NodeId(10), NodeId(20), NodeId(30));
    let nodes = vec![
        (run, leaf(Role::TextRun)),
        (
            paragraph,
            parent(Role::MultilineTextInput, "koru:w1/v2/n:aa#text", vec![run]),
        ),
        (run, leaf(Role::TextRun)),
        (
            paragraph,
            parent(Role::MultilineTextInput, "koru:w1/v2/n:aa#text", vec![run]),
        ),
        (
            body,
            parent(
                Role::Group,
                "koru:w1/v2/s:body#root",
                vec![paragraph, paragraph],
            ),
        ),
        (ROOT, parent(Role::Window, "koru:w1/root", vec![body])),
    ];
    let mut update = whole(nodes, paragraph);
    let crash = rejection(&update).expect("the unrepaired update is rejected");
    assert!(
        crash.contains("TreeUpdate includes duplicate child"),
        "not the field crash: {crash}"
    );

    let repairs = repair(&mut update, ROOT);

    assert!(accepted_whole(&update));
    assert_eq!(ids(&update), vec![run, paragraph, body, ROOT]);
    let (_, kept_body) = update
        .nodes
        .iter()
        .find(|(id, _)| *id == body)
        .expect("body");
    assert_eq!(kept_body.children(), &[paragraph]);
    assert_eq!(
        update.focus, paragraph,
        "a focus on a node that survived stays"
    );
    // The log line has to name the control, or the view that published it
    // twice cannot be found.
    assert!(repairs.iter().any(|repair| matches!(
        repair,
        Repair::NodeTwice(named) if named.locator.as_deref() == Some("koru:w1/v2/n:aa#text")
    )));
    assert!(repairs.iter().any(|repair| matches!(
        repair,
        Repair::ChildTwice { child, kept_under, .. }
            if child.id == paragraph && kept_under.id == body
    )));
}

#[test]
fn a_healthy_update_is_left_exactly_as_it_was() {
    let (body, paragraph, run) = (NodeId(10), NodeId(20), NodeId(30));
    let mut update = whole(
        vec![
            (run, leaf(Role::TextRun)),
            (
                paragraph,
                parent(Role::MultilineTextInput, "koru:p", vec![run]),
            ),
            (body, parent(Role::Group, "koru:b", vec![paragraph])),
            (ROOT, parent(Role::Window, "koru:root", vec![body])),
        ],
        paragraph,
    );
    let before = update.clone();
    assert!(repair(&mut update, ROOT).is_empty());
    assert_eq!(update, before);
}

/// A child one parent lists twice is listed once.
#[test]
fn a_child_named_twice_by_one_parent_is_named_once() {
    let item = NodeId(10);
    let mut update = whole(
        vec![
            (item, leaf(Role::Button)),
            (ROOT, parent(Role::Window, "koru:root", vec![item, item])),
        ],
        ROOT,
    );
    assert!(!accepted_whole(&update));
    let repairs = repair(&mut update, ROOT);
    assert!(accepted_whole(&update));
    assert_eq!(repairs.len(), 1);
}

/// **A delta's ordinary shape survives the repair.** A paragraph republished
/// on its own has no parent in the update — the client holds the body — and
/// dropping it as an orphan would lose the edit, so only what the repair
/// itself orphaned may go.
#[test]
fn a_delta_keeps_the_nodes_whose_parent_the_client_already_holds() {
    let (body, first, second, run) = (NodeId(10), NodeId(20), NodeId(21), NodeId(30));
    let before = whole(
        vec![
            (run, leaf(Role::TextRun)),
            (
                first,
                parent(Role::MultilineTextInput, "koru:p1", vec![run]),
            ),
            (second, leaf(Role::MultilineTextInput)),
            (body, parent(Role::Group, "koru:b", vec![first, second])),
            (ROOT, parent(Role::Window, "koru:root", vec![body])),
        ],
        first,
    );
    let mut delta = TreeUpdate {
        nodes: vec![
            (run, leaf(Role::TextRun)),
            (
                first,
                parent(Role::MultilineTextInput, "koru:p1", vec![run]),
            ),
            (
                first,
                parent(Role::MultilineTextInput, "koru:p1", vec![run]),
            ),
            (second, leaf(Role::MultilineTextInput)),
        ],
        tree: None,
        tree_id: TreeId::ROOT,
        focus: first,
    };
    assert!(accepted_whole(&before));
    assert!(!accepted_delta(&before, &delta));

    repair(&mut delta, ROOT);

    assert!(accepted_delta(&before, &delta));
    assert_eq!(ids(&delta), vec![run, first, second]);
}

/// Two *different* nodes under one id — a collision rather than a repeat. The
/// later one's child is left with no parent and has to go with it, and a
/// focus on that child goes back to the root rather than naming nothing.
#[test]
fn what_a_dropped_copy_alone_named_is_dropped_and_the_focus_moves_off_it() {
    let (shared, kept_child, orphan) = (NodeId(10), NodeId(20), NodeId(30));
    let mut update = whole(
        vec![
            (kept_child, leaf(Role::Button)),
            (shared, parent(Role::Group, "koru:first", vec![kept_child])),
            (orphan, leaf(Role::Button)),
            (shared, parent(Role::Group, "koru:second", vec![orphan])),
            (ROOT, parent(Role::Window, "koru:root", vec![shared])),
        ],
        orphan,
    );

    let repairs = repair(&mut update, ROOT);

    assert!(accepted_whole(&update));
    assert_eq!(ids(&update), vec![kept_child, shared, ROOT]);
    assert_eq!(update.focus, ROOT);
    assert!(repairs.contains(&Repair::Orphaned(super::Named {
        id: orphan,
        role: Some(Role::Button),
        locator: None,
    })));
    assert!(
        repairs
            .iter()
            .any(|repair| matches!(repair, Repair::FocusReset(_)))
    );
}

// ------------------------------------------------------------ through build

fn doc() -> uuid::Uuid {
    uuid::Uuid::parse_str("6f9619ff-8b86-d011-b42d-00cf4fc964ff").expect("a fixed fixture uuid")
}

fn run(id: u64) -> (NodeId, Node) {
    let mut node = Node::new(Role::TextRun);
    node.set_value("abcd".to_string());
    node.set_character_lengths(vec![1u8; 4]);
    (NodeId(id), node)
}

/// **The shape the document body produces when its row list holds one block
/// twice**: the same paragraph, runs and all, twice under the window. `build`
/// used to count this in `duplicate_ids` and hand the update to the adapter
/// anyway, which is where the window died.
#[test]
fn build_hands_the_adapter_an_update_it_accepts_when_a_paragraph_repeats() {
    let window = WindowSlot::from_raw(1);
    let occurrence = Occurrence::model(window, ViewSlot::from_raw(1), doc(), ControlPart::ROOT);
    let paragraph = SemanticNode::new(occurrence, Role::MultilineTextInput).with_text(TextBody {
        runs: Runs::Published(vec![run(7001)]),
        selection: None,
        read_only: false,
    });
    let semantics = WindowSemantics {
        window,
        label: "Notes".to_string(),
        scale: 1.0,
        client: Rect::new(0.0, 0.0, 1200.0, 800.0),
        focus: Some(occurrence),
        children: vec![paragraph.clone(), paragraph],
        scope: Scope::Whole,
        retired: Vec::new(),
    };

    let built = projection::build(&semantics, &std::collections::HashMap::new());

    assert!(accepted_whole(&built.update));
    assert!(!built.repairs.is_empty());
    assert_eq!(
        built.duplicate_ids, 1,
        "still counted: it is still a bug in the view"
    );
    assert_eq!(built.nodes, built.update.nodes.len());
    assert_eq!(built.update.focus, occurrence.node_id());
    assert_eq!(
        built.occurrences.get(&occurrence.node_id()),
        Some(&occurrence)
    );
}
