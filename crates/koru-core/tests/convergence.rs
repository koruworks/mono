// SPDX-License-Identifier: MPL-2.0

//! Two replicas, edited independently, must converge.
//!
//! These are the tests the plan calls for before the editor is ported onto this
//! model: they prove the foundation without any UI in the way. The manual
//! two-window walkthrough in the README is the same thing with the filesystem
//! standing in for the network.

use koru_core::node::{Content, Kind, Node, NodeMeta};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{NodeId, ShapeError, WorkspaceId};

/// Two peers starting from the same snapshot.
fn forked_pair() -> (WorkspaceDoc, WorkspaceDoc, NodeId) {
    let mut a = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let root = a
        .insert(Parent::Root, Node::task("Launch referral").unwrap())
        .unwrap();
    let snapshot = a.export_snapshot().unwrap();
    let b = WorkspaceDoc::from_snapshot(&snapshot).unwrap();
    (a, b, root)
}

/// Exchange updates both ways until both sides have seen everything.
fn sync(a: &mut WorkspaceDoc, b: &mut WorkspaceDoc) {
    let from_a = a.export_all_updates().unwrap();
    let from_b = b.export_all_updates().unwrap();
    a.import(&from_b).unwrap();
    b.import(&from_a).unwrap();
}

fn texts(ws: &WorkspaceDoc) -> Vec<(String, usize)> {
    ws.walk()
        .unwrap()
        .into_iter()
        .map(|(id, depth)| {
            let n = ws.read(id).unwrap();
            (n.content.as_text().unwrap_or_default().to_string(), depth)
        })
        .collect()
}

#[test]
fn independent_inserts_both_survive() {
    let (mut a, mut b, root) = forked_pair();

    a.insert(Parent::Node(root), Node::task("Reward flow").unwrap())
        .unwrap();
    b.insert(Parent::Node(root), Node::task("Invite email").unwrap())
        .unwrap();

    sync(&mut a, &mut b);

    let ta = texts(&a);
    assert_eq!(ta, texts(&b), "replicas converged");
    let names: Vec<_> = ta.iter().map(|(t, _)| t.as_str()).collect();
    assert!(names.contains(&"Reward flow"), "{names:?}");
    assert!(names.contains(&"Invite email"), "{names:?}");
}

/// The case that would be lost by last-write-wins: two peers editing different
/// parts of the *same* line.
#[test]
fn concurrent_edits_to_one_line_merge() {
    let mut a = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let task = a
        .insert(Parent::Root, Node::task("payout flow").unwrap())
        .unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    a.set_text(task, "reward payout flow").unwrap(); // prepended
    b.set_text(task, "payout flow blocked").unwrap(); // appended

    sync(&mut a, &mut b);

    let merged = a.read(task).unwrap().content.as_text().unwrap().to_string();
    assert_eq!(merged, b.read(task).unwrap().content.as_text().unwrap());
    assert!(merged.contains("reward"), "prefix survived: {merged:?}");
    assert!(merged.contains("blocked"), "suffix survived: {merged:?}");
}

/// One peer restructures while the other edits inside the moved subtree — the
/// showcase case, and the reason `LoroTree` was chosen over a plain list.
#[test]
fn a_move_and_an_edit_inside_it_both_survive() {
    let mut a = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let one = a
        .insert(Parent::Root, Node::task("Reward flow").unwrap())
        .unwrap();
    let two = a
        .insert(Parent::Root, Node::task("Invite email").unwrap())
        .unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    // A wraps both tasks into a group.
    let group = a
        .insert(
            Parent::Root,
            Node::task("Phase 1").unwrap().with_meta(NodeMeta {
                level_name: Some("Phase".into()),
                ..Default::default()
            }),
        )
        .unwrap();
    a.move_node(one, Parent::Node(group), 0).unwrap();
    a.move_node(two, Parent::Node(group), 1).unwrap();

    // Meanwhile B edits one of the tasks that A is moving.
    b.set_text(one, "Reward payout flow").unwrap();

    sync(&mut a, &mut b);

    assert_eq!(texts(&a), texts(&b), "replicas converged");
    assert_eq!(
        a.read(one).unwrap().content.as_text(),
        Some("Reward payout flow"),
        "B's edit survived A's move"
    );
    assert_eq!(a.depth(one).unwrap(), 1, "and it ended up inside the group");
    assert_eq!(a.ancestors(one).unwrap(), vec![group]);
}

#[test]
fn syncing_is_idempotent() {
    let (mut a, mut b, root) = forked_pair();
    a.insert(Parent::Node(root), Node::task("once").unwrap())
        .unwrap();

    sync(&mut a, &mut b);
    let after_first = texts(&b);
    // Re-importing the same bytes must not duplicate anything.
    sync(&mut a, &mut b);
    sync(&mut a, &mut b);
    assert_eq!(texts(&b), after_first);
}

#[test]
fn a_snapshot_alone_reconstructs_the_document() {
    let mut a = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let group = a
        .insert(Parent::Root, Node::task("Phase 1").unwrap())
        .unwrap();
    a.insert(Parent::Node(group), Node::task("Reward flow").unwrap())
        .unwrap();
    a.set_done(group, true).unwrap();

    let restored = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    assert_eq!(texts(&restored), texts(&a));
    assert!(restored.read(group).unwrap().meta.done);
    assert_eq!(restored.name().as_deref(), Some("acme"));
}

/// Deleting a group on one peer while another adds a child to it. Loro decides
/// convergence; this pins down what the *app* ends up showing, so the behaviour
/// is a decision rather than an accident.
#[test]
fn orphan_policy_is_pinned_down() {
    let mut a = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let group = a
        .insert(Parent::Root, Node::task("Phase 1").unwrap())
        .unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    a.delete(group).unwrap();
    let child = b
        .insert(Parent::Node(group), Node::task("Reward flow").unwrap())
        .unwrap();

    sync(&mut a, &mut b);

    // Whatever the rule is, both replicas must agree on it.
    assert_eq!(texts(&a), texts(&b), "replicas converged");
    let a_sees_child = a.read(child).is_ok();
    let b_sees_child = b.read(child).is_ok();
    assert_eq!(
        a_sees_child, b_sees_child,
        "both agree on the orphan's fate"
    );
    // Document the observed rule: a child of a deleted parent goes with it.
    assert!(
        !a_sees_child,
        "deleting a group takes its concurrently-added children with it"
    );
}

#[test]
fn shape_invariants_hold_through_the_store() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let table = ws
        .insert(
            Parent::Root,
            Node::new(Kind::Table, Content::text("| a |")).unwrap(),
        )
        .unwrap();

    // A table is text, but a leaf.
    let err = ws
        .insert(Parent::Node(table), Node::task("nope").unwrap())
        .unwrap_err();
    assert!(
        matches!(
            err,
            koru_core::StoreError::Shape(ShapeError::ChildrenNotAllowed { .. })
        ),
        "{err:?}"
    );

    // And a node's line stays a line.
    let task = ws
        .insert(Parent::Root, Node::task("one line").unwrap())
        .unwrap();
    let err = ws.set_text(task, "two\nlines").unwrap_err();
    assert!(
        matches!(err, koru_core::StoreError::Shape(ShapeError::NewlineInText)),
        "{err:?}"
    );
}

/// Depth comes from the tree, so the old `indent <= parent + 1` invariant
/// cannot be violated — including after a merge.
#[test]
fn depth_is_structural_and_cannot_be_corrupted_by_a_merge() {
    let mut a = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let l0 = a.insert(Parent::Root, Node::task("L0").unwrap()).unwrap();
    let l1 = a
        .insert(Parent::Node(l0), Node::task("L1").unwrap())
        .unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    // Both peers restructure the same branch concurrently.
    let l2 = b
        .insert(Parent::Node(l1), Node::task("L2").unwrap())
        .unwrap();
    a.move_node(l1, Parent::Root, 1).unwrap();

    sync(&mut a, &mut b);

    assert_eq!(texts(&a), texts(&b));
    for (id, depth) in a.walk().unwrap() {
        assert_eq!(depth, a.depth(id).unwrap(), "walk and depth agree");
    }
    // No repair pass ran; the tree simply cannot express an illegal depth.
    let _ = l2;
}

/// Undo is local-only, and that is the only definition that works once two
/// people are editing at once: Ctrl+Z must take back *your* last change and
/// never silently revert a collaborator's.
#[test]
fn undo_takes_back_only_this_peers_own_change() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let shared = ws
        .insert(Parent::Root, Node::task("payout").unwrap())
        .unwrap();
    ws.enable_undo();

    ws.insert_text(shared, 0, "reward ").unwrap();
    ws.checkpoint();
    assert!(ws.can_undo());
    assert!(ws.undo().unwrap());

    assert_eq!(
        ws.read(shared).unwrap().content.as_text(),
        Some("payout"),
        "the peer's own edit came back out"
    );
}

/// **Undo survives a concurrent remote change**, and takes back only the run
/// this peer typed. Every text node in this schema hangs off a `LoroTree`
/// node's meta map, and until `loro-internal` 1.13.9 the `UndoManager` declined
/// that case outright — it returned false and left the document alone. That was
/// an upstream bug, not a rule: it is fixed, and this pins the behaviour we now
/// depend on, which is why `loro` is a pinned `=1.13.9` in the root manifest.
///
/// Two shapes of remote change, because they exercise different containers: a
/// tick on the same node's meta map, and text typed into a `LoroText` the local
/// run also lives in.
#[test]
fn undo_survives_a_concurrent_remote_change_and_removes_only_its_own_edit() {
    // A remote tick on the same node.
    let mut a = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let shared = a
        .insert(Parent::Root, Node::task("payout").unwrap())
        .unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    a.enable_undo();
    a.insert_text(shared, 0, "reward ").unwrap();
    a.checkpoint();
    b.set_done(shared, true).unwrap();
    a.import(&b.export_all_updates().unwrap()).unwrap();

    assert!(a.undo().unwrap(), "it undoes rather than declining");
    let node = a.read(shared).unwrap();
    assert_eq!(
        node.content.as_text(),
        Some("payout"),
        "this peer's own run came back out, and only that"
    );
    assert!(node.meta.done, "B's tick survived the undo");

    // Redo puts the run back, still without touching what B did.
    assert!(a.can_redo());
    assert!(a.redo().unwrap());
    let node = a.read(shared).unwrap();
    assert_eq!(node.content.as_text(), Some("reward payout"));
    assert!(node.meta.done);

    // The inverse is an ordinary convergent operation when B receives it.
    b.import(&a.export_all_updates().unwrap()).unwrap();
    assert_eq!(
        b.read(shared).unwrap().content.as_text(),
        a.read(shared).unwrap().content.as_text()
    );
    assert!(b.read(shared).unwrap().meta.done);

    // A remote insert into the very text the local run sits in.
    let mut a = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let shared = a
        .insert(Parent::Root, Node::task("payout").unwrap())
        .unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    a.enable_undo();
    a.insert_text(shared, 0, "reward ").unwrap();
    a.checkpoint();
    b.insert_text(shared, 6, "!").unwrap();
    a.import(&b.export_all_updates().unwrap()).unwrap();

    assert!(a.undo().unwrap());
    assert_eq!(
        a.read(shared).unwrap().content.as_text(),
        Some("payout!"),
        "the local run went, B's character stayed"
    );

    sync(&mut a, &mut b);
    assert_eq!(texts(&a), texts(&b), "replicas converged");
}

/// The limit that is still real: a node the other peer **deleted** cannot have
/// an edit undone on it. Loro declines — `undo` returns false and the document
/// is untouched — rather than resurrecting a node the remote peer removed.
/// That is the safe failure, and it is why anything user-facing that must be
/// reversible uses a forward operation instead of `undo`.
#[test]
fn undo_declines_on_a_node_the_other_peer_deleted() {
    let mut a = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let shared = a
        .insert(Parent::Root, Node::task("payout").unwrap())
        .unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    a.enable_undo();
    a.insert_text(shared, 0, "reward ").unwrap();
    a.checkpoint();
    b.delete(shared).unwrap();
    a.import(&b.export_all_updates().unwrap()).unwrap();

    assert!(!a.undo().unwrap(), "it declines");
    assert!(a.read(shared).is_err(), "the node stayed deleted");
    assert!(
        a.walk().unwrap().is_empty(),
        "and nothing was resurrected in its place"
    );
}

/// An undo that resurrects or removes a node has to leave the id map usable,
/// or every later operation on that node would fail.
#[test]
fn undo_restores_a_deleted_node_by_its_original_id() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let keep = ws
        .insert(Parent::Root, Node::task("keep").unwrap())
        .unwrap();
    ws.enable_undo();

    let gone = ws
        .insert(Parent::Root, Node::task("gone").unwrap())
        .unwrap();
    ws.checkpoint();
    ws.delete(gone).unwrap();
    assert!(ws.read(gone).is_err());

    assert!(ws.undo().unwrap());
    assert_eq!(
        ws.read(gone).unwrap().content.as_text(),
        Some("gone"),
        "the same NodeId addresses it again"
    );
    assert_eq!(ws.walk().unwrap().len(), 2);
    let _ = keep;
}

/// Does undo reverse a node inserted *under a document* (a tree-nested
/// container)? `paste_images`/`paste_rich` use `insert_after` for exactly this,
/// and image-paste undo appears not to work. This compares `insert` vs
/// `insert_after` so the fix targets the right thing: if `insert` undoes but
/// `insert_after` doesn't, the op is the culprit; if neither does, nested
/// inserts generally aren't undoable and paste needs a manual undo record.
#[test]
fn undo_of_a_nested_insert_under_a_document() {
    use koru_core::node::BlobHash;
    let img = || Node::image(BlobHash("h".into()), "image/png", Some(1), Some(1)).unwrap();

    // `insert_after` (what paste uses).
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = ws
        .insert(Parent::Root, Node::document("d").unwrap())
        .unwrap();
    let anchor = ws
        .insert(Parent::Node(doc), Node::paragraph("p").unwrap())
        .unwrap();
    ws.enable_undo();
    let via_after = ws.insert_after(anchor, img()).unwrap();
    assert!(ws.read(via_after).is_ok());
    let moved_after = ws.undo().unwrap();
    let gone_after = ws.read(via_after).is_err();
    println!("insert_after: moved={moved_after} removed={gone_after}");

    // Plain `insert` under the same document.
    let via_insert = ws.insert(Parent::Node(doc), img()).unwrap();
    assert!(ws.read(via_insert).is_ok());
    let moved_insert = ws.undo().unwrap();
    let gone_insert = ws.read(via_insert).is_err();
    println!("insert: moved={moved_insert} removed={gone_insert}");

    // Now mirror what `paste_images` actually does: bracket an insert with
    // `begin_group`/`checkpoint`. If undo stops working here, *that* is the
    // culprit, not nested inserts.
    ws.begin_group();
    let grouped = ws.insert_after(anchor, img()).unwrap();
    ws.checkpoint();
    assert!(ws.read(grouped).is_ok());
    let moved_grouped = ws.undo().unwrap();
    let gone_grouped = ws.read(grouped).is_err();
    println!("insert_after (begin_group/checkpoint): moved={moved_grouped} removed={gone_grouped}");

    assert!(gone_after, "nested insert_after must undo (solo)");
    assert!(gone_insert, "nested insert must undo (solo)");
    assert!(gone_grouped, "a grouped nested insert must undo too");
}

/// A divider inserted on one peer reaches the other as a divider, and the two
/// replicas converge. This is the CRDT check for the new kind: the storage
/// string `"divider"` round-trips through an update, not just a snapshot.
#[test]
fn a_divider_insert_converges() {
    let (mut a, mut b, root) = forked_pair();

    let on_a = a
        .insert(Parent::Node(root), Node::divider().unwrap())
        .unwrap();
    // A peer editing elsewhere at the same time.
    b.insert(Parent::Node(root), Node::paragraph("still here").unwrap())
        .unwrap();

    sync(&mut a, &mut b);

    let read_kind = |ws: &WorkspaceDoc, id| ws.read(id).map(|n| n.kind).ok();
    assert_eq!(read_kind(&a, on_a), Some(Kind::Divider));
    assert_eq!(
        read_kind(&b, on_a),
        Some(Kind::Divider),
        "the divider crossed an update, not just a snapshot"
    );
    assert_eq!(
        a.read(on_a).unwrap().content.as_text(),
        Some(""),
        "a divider carries no text after sync"
    );
    assert_eq!(texts(&a), texts(&b), "replicas converged");
}

/// Quote is stored as a kind, not inferred from a leading marker, so both its
/// semantic type and its text must cross a CRDT update intact.
#[test]
fn a_quote_insert_converges() {
    let (mut a, mut b, root) = forked_pair();
    let on_a = a
        .insert(
            Parent::Node(root),
            Node::quote("I know Koru has my back.").unwrap(),
        )
        .unwrap();
    b.insert(Parent::Node(root), Node::paragraph("still here").unwrap())
        .unwrap();

    sync(&mut a, &mut b);

    let quote = b.read(on_a).expect("the quote crossed the update");
    assert_eq!(quote.kind, Kind::Quote);
    assert_eq!(quote.content.as_text(), Some("I know Koru has my back."));
    assert_eq!(texts(&a), texts(&b), "replicas converged");
}

// --------------------------------------------------------------- the trash
//
// The payoff of trash-as-a-move rather than trash-as-a-copy. Under a third
// Loro tree, trashing would be a copy-and-delete: a new text container, so the
// first test here would lose whatever the other peer typed. It is the reason
// the design is what it is, and the thing to run if anyone proposes changing it.

/// A trashes the document while B is typing inside it. Both agree it is in the
/// bin, and **B's text survives**, so a restore hands back the real document.
#[test]
fn one_peer_trashes_while_the_other_edits_inside_it() {
    let mut a = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let doc = a
        .insert(Parent::Root, Node::document("Spec").unwrap())
        .unwrap();
    let line = a
        .insert(Parent::Node(doc), Node::paragraph("intro").unwrap())
        .unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    a.trash(doc, 1_700_000_000_000, Some("Sam")).unwrap();
    b.set_text(line, "intro, rewritten by someone else")
        .unwrap();

    sync(&mut a, &mut b);

    for (who, ws) in [("a", &a), ("b", &b)] {
        assert!(ws.in_trash(doc), "{who} should see it in the trash");
        assert!(
            ws.children(Parent::Root).unwrap().is_empty(),
            "{who}'s library should not list it"
        );
        assert_eq!(
            ws.read(line).unwrap().content.as_text(),
            Some("intro, rewritten by someone else"),
            "{who} kept the edit made while it was being binned"
        );
    }

    // And B can hand it back, which is the whole hand-off.
    b.restore(doc, Some("Ash")).unwrap();
    sync(&mut a, &mut b);
    for (who, ws) in [("a", &a), ("b", &b)] {
        assert!(!ws.in_trash(doc), "{who} should see it back");
        assert_eq!(
            ws.restored_by(doc).as_deref(),
            Some("Ash"),
            "{who} can say who brought it back"
        );
    }
}

/// The other window reads back who binned it. Not inferred from a peer id or a
/// peer file — the client that acted wrote its own label, and that is ordinary
/// CRDT data like any other.
#[test]
fn the_other_peer_reads_back_who_trashed_it() {
    let mut a = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let doc = a
        .insert(Parent::Root, Node::document("Spec").unwrap())
        .unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    a.trash(doc, 1_700_000_000_000, Some("Sam")).unwrap();
    sync(&mut a, &mut b);

    assert_eq!(b.trashed_by(doc).as_deref(), Some("Sam"));
    assert_eq!(b.trashed().unwrap()[0].trashed_by.as_deref(), Some("Sam"));
}

/// Two peers each bin something before they have met, so each mints its own
/// trash root — there is no mergeable `LoroTree::create` to stop them. After
/// they sync, both items are in the bin exactly once and neither root is a row.
/// The next write unions the duplicates away.
#[test]
fn two_peers_trashing_at_once_converge_on_one_trash() {
    let mut a = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let one = a
        .insert(Parent::Root, Node::document("One").unwrap())
        .unwrap();
    let two = a
        .insert(Parent::Root, Node::document("Two").unwrap())
        .unwrap();
    let three = a
        .insert(Parent::Root, Node::document("Three").unwrap())
        .unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    a.trash(one, 1, Some("Sam")).unwrap();
    b.trash(two, 2, Some("Ash")).unwrap();

    sync(&mut a, &mut b);

    for (who, ws) in [("a", &a), ("b", &b)] {
        assert_eq!(ws.trash_len(), 2, "{who} sees both items, once each");
        assert!(ws.in_trash(one) && ws.in_trash(two), "{who}");
        let live: Vec<NodeId> = ws.children(Parent::Root).unwrap();
        assert_eq!(live, vec![three], "{who} lists only the live document");
    }

    // The next write folds the duplicate roots together, and nothing visible
    // changes when it does.
    a.trash(three, 3, Some("Sam")).unwrap();
    assert_eq!(a.trash_len(), 3);
    assert!(a.children(Parent::Root).unwrap().is_empty());
    sync(&mut a, &mut b);
    assert_eq!(b.trash_len(), 3);
    assert!(b.children(Parent::Root).unwrap().is_empty());
}

/// The toast's Undo calls `restore`, not general history undo: it names this
/// exact item even if another local edit happened while the toast was visible.
/// The forward operation also preserves a peer edit imported after the trash.
#[test]
fn restore_is_targeted_and_preserves_a_remote_edit() {
    let mut a = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let doc = a
        .insert(Parent::Root, Node::document("Spec").unwrap())
        .unwrap();
    let line = a
        .insert(Parent::Node(doc), Node::paragraph("intro").unwrap())
        .unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    a.enable_undo();
    a.checkpoint();
    a.trash(doc, 1_700_000_000_000, Some("Sam")).unwrap();

    // Somebody else was working in it, and their change lands.
    b.set_text(line, "meanwhile").unwrap();
    a.import(&b.export_all_updates().unwrap()).unwrap();

    a.restore(doc, Some("Sam")).unwrap();
    assert!(!a.in_trash(doc));
    assert_eq!(a.children(Parent::Root).unwrap(), vec![doc]);
    assert_eq!(a.read(line).unwrap().content.as_text(), Some("meanwhile"));
}

/// One peer extracts a selection into a pill while the other types inside the
/// extracted subtree. The delete wins — the concurrent edit is lost with the
/// subtree, the same class of loss as any concurrent delete — and what matters
/// is that both replicas converge on the pill paragraph rather than one
/// keeping a half-deleted subtree.
#[test]
fn an_extraction_and_an_edit_inside_it_converge_on_the_pill() {
    use koru_core::node::{Attr, AttrValue};

    let mut a = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let root = a
        .insert(Parent::Root, Node::document("Source").unwrap())
        .unwrap();
    let moved = a
        .insert(Parent::Node(root), Node::paragraph("move me").unwrap())
        .unwrap();
    let kept = a
        .insert(Parent::Node(root), Node::paragraph("stays").unwrap())
        .unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    let attr = Attr::labelled("document", AttrValue::Text("Target".into()), "Target");
    let replacement = a.replace_subtrees_with_pill(&[moved], &attr).unwrap();
    b.set_text(moved, "move me, edited concurrently").unwrap();

    sync(&mut a, &mut b);

    assert_eq!(
        a.children(Parent::Node(root)).unwrap(),
        b.children(Parent::Node(root)).unwrap(),
        "replicas converged"
    );
    assert_eq!(
        a.children(Parent::Node(root)).unwrap(),
        vec![replacement, kept],
        "the pill paragraph stands where the subtree was"
    );
    assert!(a.read(moved).is_err(), "the extracted subtree stayed gone");
}

// ------------------------------------------------- the seed race (ADR-277-filenote)
//
// A store-registered document starts EMPTY — the store does not know what is
// inside one (ADR-80) — and gains its `Kind::Document` root from whoever
// opens it first (`seed_empty_document` on the desktop, `seed_note_document`
// on a phone). The manifest entry and the document sync separately, so two
// devices can each find the note empty and each seed it.

/// The empty document the store registers: a `LoroDoc` with nothing in it,
/// not even meta.
fn store_registered_empty() -> Vec<u8> {
    loro::LoroDoc::new()
        .export(loro::ExportMode::snapshot())
        .unwrap()
}

/// Exactly what the open-path seeds write into an empty registered document.
fn seed_like_an_opener(ws: &mut WorkspaceDoc, title: &str) {
    let root = ws
        .insert(Parent::Root, Node::document(title).unwrap())
        .unwrap();
    ws.insert(Parent::Node(root), Node::paragraph("").unwrap())
        .unwrap();
    ws.stamp_schema_version().unwrap();
}

fn document_roots(ws: &WorkspaceDoc) -> Vec<NodeId> {
    ws.children(Parent::Root)
        .unwrap()
        .into_iter()
        .filter(|&id| ws.kind_of(id) == Some(Kind::Document))
        .collect()
}

/// Two devices open the same just-registered note before either's seed has
/// replicated. Loro keeps both seeds — there is no mergeable
/// `LoroTree::create` — so the open path folds the split back to one root.
#[test]
fn two_peers_seeding_one_note_converge_on_one_root() {
    let empty = store_registered_empty();
    let mut a = WorkspaceDoc::from_snapshot(&empty).unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&empty).unwrap();

    // Each device opens the note before the other's seed could arrive.
    seed_like_an_opener(&mut a, "Report.pdf");
    seed_like_an_opener(&mut b, "Report.pdf");

    sync(&mut a, &mut b);

    // The merge keeps BOTH seeds: two document roots, and every reader that
    // answers with the first one renders the other's paragraph invisible.
    assert_eq!(texts(&a), texts(&b), "replicas converged");
    assert_eq!(document_roots(&a).len(), 2, "the race is real");
    assert_eq!(document_roots(&b).len(), 2);

    // Each replica's next open folds the split — before the other's fold has
    // arrived, which is exactly when two folds race.
    assert!(a.unify_document_roots().unwrap());
    assert!(b.unify_document_roots().unwrap());

    sync(&mut a, &mut b);

    assert_eq!(texts(&a), texts(&b), "replicas converged");
    for (who, ws) in [("a", &a), ("b", &b)] {
        let roots = document_roots(ws);
        assert_eq!(roots.len(), 1, "{who}: one root after the fold");
        assert_eq!(
            ws.read(roots[0]).unwrap().content.as_text(),
            Some("Report.pdf"),
            "{who}: the title survived"
        );
        assert_eq!(
            ws.children(Parent::Node(roots[0])).unwrap().len(),
            3,
            "{who}: both seed paragraphs and the duplicate title identity survive"
        );
    }
}

/// A share-time seed carries the shared text as the first paragraph; the
/// other device's seed has none. The fold must keep the text — healing moves
/// content, it never drops it.
#[test]
fn the_fold_keeps_a_seed_that_carried_text() {
    let empty = store_registered_empty();
    let mut a = WorkspaceDoc::from_snapshot(&empty).unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&empty).unwrap();

    // The phone's share flow seeds with the shared text; a desktop opening
    // the note icon seeds an empty paragraph.
    let root_a = a
        .insert(Parent::Root, Node::document("Report.pdf").unwrap())
        .unwrap();
    a.insert(
        Parent::Node(root_a),
        Node::paragraph("the shared text").unwrap(),
    )
    .unwrap();
    seed_like_an_opener(&mut b, "Report.pdf");

    sync(&mut a, &mut b);
    assert_eq!(document_roots(&a).len(), 2);

    assert!(a.unify_document_roots().unwrap());
    sync(&mut a, &mut b);

    assert_eq!(texts(&a), texts(&b), "replicas converged");
    let roots = document_roots(&b);
    assert_eq!(roots.len(), 1);
    let body: Vec<String> = b
        .children(Parent::Node(roots[0]))
        .unwrap()
        .into_iter()
        .map(|id| {
            b.read(id)
                .unwrap()
                .content
                .as_text()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert!(
        body.contains(&"the shared text".to_string()),
        "the shared text survived the fold: {body:?}"
    );
}

/// One root is the healthy shape, and the fold must not touch it — a repair
/// that wrote ops on every open would be churn on every tab switch.
#[test]
fn the_fold_leaves_a_healthy_document_alone() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    seed_like_an_opener(&mut ws, "Page");
    let before = ws.export_all_updates().unwrap();
    assert!(
        !ws.unify_document_roots().unwrap(),
        "one root is not a split"
    );
    assert_eq!(
        ws.export_all_updates().unwrap(),
        before,
        "and no ops were written"
    );

    // An empty registered document has nothing to fold either.
    let mut empty = WorkspaceDoc::from_snapshot(&store_registered_empty()).unwrap();
    assert!(!empty.unify_document_roots().unwrap());
}

#[test]
fn a_fold_retains_a_duplicate_root_for_late_text_marks_and_attributes() {
    use koru_core::{Attr, AttrValue, Mark};
    let empty = store_registered_empty();
    let mut a = WorkspaceDoc::from_snapshot(&empty).unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&empty).unwrap();
    seed_like_an_opener(&mut a, "");
    seed_like_an_opener(&mut b, "");
    sync(&mut a, &mut b);
    let duplicate = document_roots(&a)[1];
    assert!(a.unify_document_roots().unwrap());

    // B has not received the repair. Local equality at A did not authorize
    // deleting this identity: B is still editing the root it opened.
    b.set_text(duplicate, "Late title").unwrap();
    b.set_mark(duplicate, 0..4, Mark::Bold, true).unwrap();
    let attr = Attr::new("tag", AttrValue::Text("late".into()));
    b.set_attr(duplicate, &attr).unwrap();
    sync(&mut a, &mut b);
    for ws in [&a, &b] {
        let node = ws.read(duplicate).unwrap();
        assert_eq!(node.kind, Kind::Paragraph);
        assert_eq!(node.content.as_text(), Some("Late title"));
        assert!(node.content.attrs().contains(&attr));
        assert!(ws.runs(duplicate).unwrap()[0].has(Mark::Bold));
        assert_eq!(document_roots(ws).len(), 1);
    }
    assert_eq!(texts(&a), texts(&b));
    assert!(!a.unify_document_roots().unwrap());
}

#[test]
fn simultaneous_folds_keep_distinct_titles_and_styles_on_their_original_nodes() {
    use koru_core::{Attr, AttrValue, Mark};
    let empty = store_registered_empty();
    let mut a = WorkspaceDoc::from_snapshot(&empty).unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&empty).unwrap();
    seed_like_an_opener(&mut a, "First title");
    seed_like_an_opener(&mut b, "Second title");
    let first = document_roots(&a)[0];
    let second = document_roots(&b)[0];
    a.set_mark(first, 0..5, Mark::Bold, true).unwrap();
    b.set_mark(second, 0..6, Mark::Italic, true).unwrap();
    let attr = Attr::new("tag", AttrValue::Text("second".into()));
    b.set_attr(second, &attr).unwrap();
    sync(&mut a, &mut b);
    assert!(a.unify_document_roots().unwrap());
    assert!(b.unify_document_roots().unwrap());
    sync(&mut a, &mut b);
    for ws in [&a, &b] {
        assert_eq!(
            ws.read(first).unwrap().content.as_text(),
            Some("First title")
        );
        assert_eq!(
            ws.read(second).unwrap().content.as_text(),
            Some("Second title")
        );
        assert!(ws.runs(first).unwrap()[0].has(Mark::Bold));
        assert!(ws.runs(second).unwrap()[0].has(Mark::Italic));
        assert!(ws.read(second).unwrap().content.attrs().contains(&attr));
        assert_eq!(document_roots(ws).len(), 1);
    }
    assert_eq!(texts(&a), texts(&b));
}

// ------------------------------------------------------------------ canvas
//
// Two devices, one card: every row of the canvas plan's table (CANVAS.rev1
// §5), each asserting its stated outcome. A canvas is an ordinary document
// whose objects carry `at` and `extent`, each one whole map value in node
// meta — so these pin what Loro does with those values and with tree moves,
// not anything canvas-specific underneath.

use koru_core::canvas::{self, CanvasSnapshot, ObjectKind};
use koru_core::{CanvasExtent, CanvasPoint};

/// A canvas with two cards, `x` under `y`, forked onto two peers with fixed
/// peer ids so a Lamport tie would break the same way on every run.
fn canvas_pair() -> (WorkspaceDoc, WorkspaceDoc, NodeId, NodeId, NodeId) {
    let mut a = WorkspaceDoc::create(WorkspaceId::new(), "board").unwrap();
    let root = a
        .insert(Parent::Root, Node::document("Board").unwrap())
        .unwrap();
    let x = canvas::add_object(
        &mut a,
        root,
        ObjectKind::Card,
        CanvasPoint::new(0, 0),
        "x\nits line",
    )
    .unwrap();
    let y = canvas::add_object(
        &mut a,
        root,
        ObjectKind::Card,
        CanvasPoint::new(400, 0),
        "y",
    )
    .unwrap();
    let b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();
    a.doc().set_peer_id(1).unwrap();
    b.doc().set_peer_id(2).unwrap();
    (a, b, root, x, y)
}

fn board(ws: &WorkspaceDoc, root: NodeId) -> CanvasSnapshot {
    canvas::read(ws, root).unwrap()
}

fn at_of(ws: &WorkspaceDoc, root: NodeId, id: NodeId) -> Option<CanvasPoint> {
    board(ws, root).get(id).and_then(|o| o.at)
}

fn object_ids(canvas: &CanvasSnapshot) -> Vec<NodeId> {
    canvas.objects.iter().map(|o| o.id).collect()
}

/// Both move the same card: **one of the two positions, whole** — never A's
/// `x` with B's `y`, which is what two keys would have produced.
#[test]
fn canvas_move_against_move_is_one_whole_position() {
    let (mut a, mut b, root, x, _) = canvas_pair();
    canvas::move_object(&mut a, x, CanvasPoint::new(100, 900)).unwrap();
    canvas::move_object(&mut b, x, CanvasPoint::new(-300, 5)).unwrap();
    sync(&mut a, &mut b);

    assert_eq!(board(&a, root), board(&b, root), "replicas converged");
    let landed = at_of(&a, root, x).unwrap();
    assert!(
        landed == CanvasPoint::new(100, 900) || landed == CanvasPoint::new(-300, 5),
        "one device's position, whole: {landed:?}"
    );
}

/// One moves, the other widens: **both** survive, because the position and
/// the size are two values.
#[test]
fn canvas_move_against_width_change_keeps_both() {
    let (mut a, mut b, root, x, _) = canvas_pair();
    canvas::move_object(&mut a, x, CanvasPoint::new(50, 60)).unwrap();
    canvas::set_width(&mut b, x, 420).unwrap();
    sync(&mut a, &mut b);

    assert_eq!(board(&a, root), board(&b, root));
    let card = board(&a, root).get(x).cloned().unwrap();
    assert_eq!(card.at, Some(CanvasPoint::new(50, 60)));
    assert_eq!(card.extent, Some(CanvasExtent::new(420, None)));
}

/// One deletes, the other moves: **deleted** on both. A move is a meta write
/// on a node the tree no longer holds, and it does not bring it back.
#[test]
fn canvas_delete_against_move_is_deleted() {
    let (mut a, mut b, root, x, y) = canvas_pair();
    canvas::delete_object(&mut a, x).unwrap();
    canvas::move_object(&mut b, x, CanvasPoint::new(7, 7)).unwrap();
    sync(&mut a, &mut b);

    for ws in [&a, &b] {
        assert_eq!(object_ids(&board(ws, root)), vec![y]);
        assert!(ws.read(x).is_err());
    }
}

/// One deletes, the other types in it: **deleted** on both. The surface tells
/// the typist the card went; the model's half is that nothing resurrects it.
#[test]
fn canvas_delete_against_text_edit_is_deleted() {
    let (mut a, mut b, root, x, y) = canvas_pair();
    canvas::delete_object(&mut a, x).unwrap();
    canvas::set_text(&mut b, x, "x, rewritten\nits line\nand another").unwrap();
    sync(&mut a, &mut b);

    for ws in [&a, &b] {
        assert!(board(ws, root).get(x).is_none());
        assert!(ws.read(x).is_err());
        assert!(board(ws, root).get(y).is_some());
    }
    assert_eq!(board(&a, root), board(&b, root));
}

/// **Delete against a reorder is decided by Loro's tree order: the later of
/// the two, by Lamport timestamp, wins.** A delete is a move to Loro's
/// deleted root, so it and a concurrent bring-to-front (or send-to-back) are
/// two moves of one node, and the one ordered last is where the node ends up.
///
/// Pinned both ways, for both reorders:
///
/// - a reorder ordered **after** the delete brings the card back, whole —
///   its lines with it — on both devices. Accepted (CANVAS.rev1 §5): nothing
///   is lost, and whoever deleted it can delete it again;
/// - a reorder ordered **before** the delete leaves it deleted.
///
/// "After" is arranged by giving that peer one unrelated edit first, which
/// raises its Lamport clock past the other's.
#[test]
fn canvas_delete_against_a_reorder_is_won_by_the_later_move() {
    type Reorder = fn(&mut WorkspaceDoc, NodeId) -> koru_core::schema::Result<()>;
    let reorders: [(&str, Reorder); 2] = [
        ("bring to front", canvas::bring_to_front),
        ("send to back", canvas::send_to_back),
    ];
    for (name, reorder) in reorders {
        for reorder_is_later in [true, false] {
            let (mut a, mut b, root, x, y) = canvas_pair();
            // `send_to_back` needs something to move under; put x on top.
            if name == "send to back" {
                canvas::bring_to_front(&mut a, x).unwrap();
                b.import(&a.export_all_updates().unwrap()).unwrap();
            }
            if reorder_is_later {
                canvas::set_text(&mut b, y, "y, edited first").unwrap();
            } else {
                canvas::set_text(&mut a, y, "y, edited first").unwrap();
            }
            canvas::delete_object(&mut a, x).unwrap();
            reorder(&mut b, x).unwrap();
            sync(&mut a, &mut b);

            let case = format!("{name}, reorder later: {reorder_is_later}");
            assert_eq!(board(&a, root), board(&b, root), "{case}: converged");
            let survived = board(&a, root).get(x).cloned();
            if reorder_is_later {
                let card = survived.unwrap_or_else(|| panic!("{case}: the card came back"));
                assert_eq!(card.text, "x", "{case}");
                assert_eq!(card.lines, vec!["its line"], "{case}: with its lines");
                assert_eq!(card.at, Some(CanvasPoint::new(0, 0)), "{case}");
            } else {
                assert!(survived.is_none(), "{case}: the card stayed deleted");
                assert!(a.read(x).is_err() && b.read(x).is_err(), "{case}");
            }
        }
    }
}

/// **A moves a card, B moves it too, then A presses Ctrl+Z.** Pinned: A's
/// undo puts the card back where A found it, over B's later move — undo is
/// the inverse of A's own write, and a position is one whole value. Accepted
/// (CANVAS.rev1 §5), and the user guide says so; both devices converge.
#[test]
fn canvas_undo_of_a_move_after_a_remote_move() {
    let (mut a, mut b, root, x, _) = canvas_pair();
    a.enable_undo();
    canvas::move_object(&mut a, x, CanvasPoint::new(100, 100)).unwrap();
    sync(&mut a, &mut b);
    canvas::move_object(&mut b, x, CanvasPoint::new(200, 200)).unwrap();
    sync(&mut a, &mut b);

    assert!(a.undo().unwrap());
    sync(&mut a, &mut b);
    assert_eq!(board(&a, root), board(&b, root));
    assert_eq!(at_of(&a, root, x), Some(CanvasPoint::new(0, 0)));
}

/// **A deletes a card, B types in it, then A presses Ctrl+Z.** The card
/// returns **as A deleted it**, on both devices — and B's concurrent edit
/// does not survive.
///
/// Why, from `loro-internal` 1.13.9 (`handler.rs`, the `Create` arm of the
/// undo diff): undoing a tree delete does not move the deleted node back, it
/// creates a *new* tree node and refills its meta from the state recorded at
/// the delete. B's typing landed in the old node's text container, which
/// stays deleted. The `NodeId` is the same (it is a meta value), so every
/// reference and every arrow finds the card again. Accepted (CANVAS.rev1 §5);
/// the user guide says so.
#[test]
fn canvas_undo_of_a_delete_after_a_remote_edit() {
    let (mut a, mut b, root, x, _) = canvas_pair();
    a.enable_undo();
    canvas::delete_object(&mut a, x).unwrap();
    canvas::set_text(&mut b, x, "x, typed by B\nits line\nB's new line").unwrap();
    sync(&mut a, &mut b);
    assert!(board(&a, root).get(x).is_none());

    assert!(a.undo().unwrap());
    sync(&mut a, &mut b);
    assert_eq!(board(&a, root), board(&b, root), "converged");
    let card = board(&a, root).get(x).cloned().expect("the card came back");
    assert_eq!(card.at, Some(CanvasPoint::new(0, 0)));
    assert_eq!(card.text, "x", "as A deleted it: B's edit did not survive");
    assert_eq!(card.lines, vec!["its line"]);
}

/// **A moves a card, B deletes it, then A presses Ctrl+Z.** Nothing comes
/// back and nothing errors — and that Ctrl+Z is **not silently used up**. It
/// is better than the plan asked for: the same press goes on to undo A's
/// earlier gesture, because Loro's `UndoManager::perform` keeps popping while
/// an item turns out to be a no-op ("continue to pop the undo item as this
/// undo is a no-op", `loro-internal` 1.13.9 `undo.rs`). So a person never
/// presses Ctrl+Z and sees nothing happen.
#[test]
fn canvas_undo_of_a_move_after_a_remote_delete() {
    let (mut a, mut b, root, x, y) = canvas_pair();
    a.enable_undo();
    let earlier = canvas::add_object(
        &mut a,
        root,
        ObjectKind::Card,
        CanvasPoint::new(0, 500),
        "A's earlier gesture",
    )
    .unwrap();
    canvas::move_object(&mut a, x, CanvasPoint::new(100, 100)).unwrap();
    sync(&mut a, &mut b);
    canvas::delete_object(&mut b, x).unwrap();
    sync(&mut a, &mut b);

    let first = a.undo();
    assert!(
        matches!(first, Ok(true)),
        "no error, and it did something: {first:?}"
    );
    assert!(a.read(x).is_err(), "nothing came back");
    assert!(
        board(&a, root).get(earlier).is_none(),
        "that same press undid the earlier gesture"
    );
    assert!(!a.can_undo(), "and there is nothing left behind it");
    sync(&mut a, &mut b);
    assert_eq!(board(&a, root), board(&b, root));
    assert_eq!(object_ids(&board(&a, root)), vec![y]);
}

/// **Both devices work offline**, each moving, widening, adding and
/// deleting, and their updates arrive in either order. Every replica ends on
/// the same canvas, whichever arrived first — and it is the canvas both
/// meant: every independent edit survives.
#[test]
fn canvas_offline_edits_converge_in_either_order() {
    let mut base = WorkspaceDoc::create(WorkspaceId::new(), "board").unwrap();
    let root = base
        .insert(Parent::Root, Node::document("Board").unwrap())
        .unwrap();
    let card = |ws: &mut WorkspaceDoc, x: i32, text: &str| {
        canvas::add_object(ws, root, ObjectKind::Card, CanvasPoint::new(x, 0), text).unwrap()
    };
    let p = card(&mut base, 0, "p\nline");
    let q = card(&mut base, 300, "q");
    let r = card(&mut base, 600, "r");
    let agent = canvas::add_object(
        &mut base,
        root,
        ObjectKind::Agent,
        CanvasPoint::new(0, 300),
        "Do",
    )
    .unwrap();
    let p_to_q = canvas::add_arrow(&mut base, root, p, q).unwrap();
    let start = base.export_snapshot().unwrap();

    let mut a = WorkspaceDoc::from_snapshot(&start).unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&start).unwrap();
    let a_before = a.version();
    let b_before = b.version();

    // A, offline.
    canvas::move_object(&mut a, p, CanvasPoint::new(10, 20)).unwrap();
    canvas::set_width(&mut a, q, 500).unwrap();
    let w = card(&mut a, 900, "w from A");
    canvas::delete_object(&mut a, r).unwrap();
    canvas::add_arrow(&mut a, root, q, w).unwrap();
    canvas::set_agent(&mut a, agent, "claude", Some("opus")).unwrap();
    canvas::bring_to_front(&mut a, p).unwrap();

    // B, offline.
    canvas::move_object(&mut b, q, CanvasPoint::new(-40, 80)).unwrap();
    canvas::set_width(&mut b, p, 260).unwrap();
    let v = card(&mut b, 1200, "v from B");
    canvas::delete_arrow(&mut b, p_to_q).unwrap();
    canvas::set_text(&mut b, p, "p, renamed\nline\nsecond line").unwrap();
    canvas::set_result(&mut b, agent, &["an answer".into()]).unwrap();
    canvas::move_object(&mut b, r, CanvasPoint::new(1, 1)).unwrap();

    let from_a = a.export_updates_since(&a_before).unwrap();
    let from_b = b.export_updates_since(&b_before).unwrap();
    let mut a_then_b = WorkspaceDoc::from_snapshot(&start).unwrap();
    a_then_b.import(&from_a).unwrap();
    a_then_b.import(&from_b).unwrap();
    let mut b_then_a = WorkspaceDoc::from_snapshot(&start).unwrap();
    b_then_a.import(&from_b).unwrap();
    b_then_a.import(&from_a).unwrap();
    sync(&mut a, &mut b);

    let settled = board(&a_then_b, root);
    for (who, ws) in [("b then a", &b_then_a), ("peer a", &a), ("peer b", &b)] {
        assert_eq!(board(ws, root), settled, "{who} converged");
    }

    let get = |id| {
        settled
            .get(id)
            .cloned()
            .unwrap_or_else(|| panic!("{id:?} is there"))
    };
    assert_eq!(get(p).at, Some(CanvasPoint::new(10, 20)));
    assert_eq!(get(p).extent, Some(CanvasExtent::new(260, None)));
    assert_eq!(get(p).text, "p, renamed");
    assert_eq!(get(p).lines, vec!["line", "second line"]);
    assert_eq!(get(q).at, Some(CanvasPoint::new(-40, 80)));
    assert_eq!(get(q).extent, Some(CanvasExtent::new(500, None)));
    assert!(settled.get(r).is_none(), "A's delete beat B's move");
    assert!(settled.get(w).is_some() && settled.get(v).is_some());
    assert_eq!(get(agent).agent.as_deref(), Some("claude"));
    assert_eq!(get(agent).result, vec!["an answer"]);
    let arrows: Vec<(NodeId, NodeId)> = settled.arrows.iter().map(|a| (a.from, a.to)).collect();
    assert_eq!(
        arrows,
        vec![(q, w)],
        "B's removal and A's addition both held"
    );
}
