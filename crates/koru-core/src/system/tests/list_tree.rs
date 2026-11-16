// SPDX-License-Identifier: MPL-2.0

//! Nested lists: the `parent` field, the tree repaired on read, and what a
//! build that has never heard of `parent` does to it (nested-lists §4, §5).

use super::super::*;

fn list(sys: &mut SystemDoc, parent: Option<ListId>, name: &str) -> ListId {
    sys.create_list_under(parent, name, None, 1).unwrap().id()
}

/// `(name, depth)` for every row, in the order the rail draws them.
fn shape(sys: &SystemDoc) -> Vec<(String, usize)> {
    sys.list_tree()
        .nodes()
        .iter()
        .map(|node| (node.entry.name.clone(), node.depth))
        .collect()
}

fn sync(a: &mut SystemDoc, b: &mut SystemDoc) {
    let from_a = a.export_all_updates().unwrap();
    let from_b = b.export_all_updates().unwrap();
    a.import(&from_b).unwrap();
    b.import(&from_a).unwrap();
}

/// A bare record, for the tree's own arithmetic: no document, so ten
/// thousand of them cost nothing to make.
fn record(id: ListId, parent: Option<ListId>, deleted: bool) -> ListEntry {
    ListEntry {
        id,
        name: String::new(),
        parent,
        color: None,
        order: None,
        created_ms: None,
        deleted_ms: deleted.then_some(1),
    }
}

/// What every workspace written before this change holds: no `parent`
/// anywhere, which reads as today's flat rail.
#[test]
fn a_table_with_no_parent_reads_as_a_flat_rail() {
    let mut sys = SystemDoc::create().unwrap();
    let a = list(&mut sys, None, "A");
    let b = list(&mut sys, None, "B");
    let tree = sys.list_tree();
    assert_eq!(tree.nodes().len(), 2);
    assert!(
        tree.nodes()
            .iter()
            .all(|node| node.depth == 0 && node.parent.is_none() && !node.has_children)
    );
    let flat: Vec<ListId> = sys.lists().into_iter().map(|l| l.id).collect();
    let drawn: Vec<ListId> = tree.nodes().iter().map(|n| n.entry.id).collect();
    assert_eq!(drawn, flat, "the tree of a flat table is the flat table");
    assert!(flat.contains(&a) && flat.contains(&b));
    assert_eq!(tree.repairs, TreeRepairs::default());
}

/// Rung one: the new list is first, and stays first, by one write to its own
/// record. No other list's rank moves.
#[test]
fn a_new_list_is_first_among_its_siblings_and_moves_no_other_rank() {
    let mut sys = SystemDoc::create().unwrap();
    let old = list(&mut sys, None, "Old");
    sys.set_list_order(old, Some(7)).unwrap();
    let unranked = list(&mut sys, None, "Peer made");
    sys.set_list_order(unranked, None).unwrap();

    let new = list(&mut sys, None, "New");
    let order: Vec<ListId> = sys.lists().into_iter().map(|l| l.id).collect();
    assert_eq!(order, vec![new, old, unranked]);
    assert_eq!(sys.list_entry(old).unwrap().order, Some(7));
    assert_eq!(sys.list_entry(unranked).unwrap().order, None);

    let newer = list(&mut sys, None, "Newer");
    assert_eq!(sys.lists()[0].id, newer, "and the next one goes above it");
}

/// D8: a name is unique among live siblings, not across the workspace.
#[test]
fn the_same_name_is_one_list_under_one_parent_and_two_under_two() {
    let mut sys = SystemDoc::create().unwrap();
    let top3 = list(&mut sys, None, "Top 3");
    let backlog = sys
        .create_list_under(Some(top3), "Backlog", None, 1)
        .unwrap();
    assert!(backlog.created());

    assert_eq!(
        sys.create_list_under(Some(top3), "BACKLOG", None, 2).unwrap(),
        ListCreation::Existing(backlog.id()),
        "the same name twice under one parent answers the sibling"
    );
    let second = sys.create_list_under(None, "Backlog", None, 3).unwrap();
    assert!(second.created(), "at the top it is a second list");
    assert_ne!(second.id(), backlog.id());
    assert_eq!(sys.lists().len(), 3);
}

/// An `Existing` answer is a selection, not a move: neither the parent nor
/// the rank of the list it names is written.
#[test]
fn answering_an_existing_list_writes_nothing() {
    let mut sys = SystemDoc::create().unwrap();
    let first = list(&mut sys, None, "Wedding");
    let _above = list(&mut sys, None, "Bike");
    let before = sys.list_entry(first).unwrap();
    let version = sys.export_snapshot().unwrap();

    assert_eq!(
        sys.create_list_under(None, "wedding", Some("blue"), 9)
            .unwrap(),
        ListCreation::Existing(first)
    );
    assert_eq!(sys.list_entry(first).unwrap(), before);
    assert_eq!(
        sys.export_snapshot().unwrap(),
        version,
        "nothing was committed"
    );
}

/// Renaming onto a sibling's name is allowed, as it is today: two rows with
/// one name, and nothing folds them.
#[test]
fn renaming_onto_a_siblings_name_keeps_both_lists() {
    let mut sys = SystemDoc::create().unwrap();
    let a = list(&mut sys, None, "A");
    let b = list(&mut sys, None, "B");
    assert!(sys.rename_list(b, "A").unwrap());
    let tree = sys.list_tree();
    assert_eq!(tree.nodes().len(), 2);
    assert!(tree.name_is_shared(a) && tree.name_is_shared(b));
}

/// Two peers each creating the same name offline keep both, under one parent
/// as at the top.
#[test]
fn concurrent_same_name_creation_keeps_both() {
    let mut a = SystemDoc::create().unwrap();
    let top3 = list(&mut a, None, "Top 3");
    let mut b = SystemDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();
    let from_a = list(&mut a, Some(top3), "Backlog");
    let from_b = list(&mut b, Some(top3), "Backlog");
    sync(&mut a, &mut b);
    for peer in [&a, &b] {
        let inside = peer.list_tree().descendants(top3);
        assert!(inside.contains(&from_a) && inside.contains(&from_b));
    }
}

#[test]
fn set_list_parent_refuses_self_a_descendant_and_a_dead_or_unknown_parent() {
    let mut sys = SystemDoc::create().unwrap();
    let top = list(&mut sys, None, "Top");
    let mid = list(&mut sys, Some(top), "Mid");
    let leaf = list(&mut sys, Some(mid), "Leaf");
    let gone = list(&mut sys, None, "Gone");
    sys.delete_list(gone, 2).unwrap();

    assert!(!sys.set_list_parent(top, Some(top)).unwrap(), "itself");
    assert!(!sys.set_list_parent(top, Some(mid)).unwrap(), "its child");
    assert!(
        !sys.set_list_parent(top, Some(leaf)).unwrap(),
        "its grandchild"
    );
    assert!(!sys.set_list_parent(top, Some(gone)).unwrap(), "deleted");
    assert!(
        !sys.set_list_parent(top, Some(ListId::new())).unwrap(),
        "unknown"
    );
    assert!(
        !sys.set_list_parent(gone, Some(top)).unwrap(),
        "a deleted list"
    );
    assert_eq!(
        shape(&sys),
        vec![("Top".into(), 0), ("Mid".into(), 1), ("Leaf".into(), 2)]
    );

    assert!(sys.set_list_parent(leaf, None).unwrap(), "un-nest");
    assert_eq!(sys.list_tree().node(leaf).unwrap().depth, 0);
}

/// Nesting arranges lists among their new siblings and writes no other
/// list's parent.
#[test]
fn move_list_places_a_list_before_a_sibling_under_its_new_parent() {
    let mut sys = SystemDoc::create().unwrap();
    let top3 = list(&mut sys, None, "Top 3");
    let one = list(&mut sys, Some(top3), "One");
    let two = list(&mut sys, Some(top3), "Two");
    let backlog = list(&mut sys, None, "Backlog");

    assert!(sys.move_list(backlog, Some(top3), Some(one)).unwrap());
    let inside = sys.list_tree().descendants(top3);
    assert_eq!(inside, vec![two, backlog, one]);

    assert!(sys.move_list(backlog, Some(top3), None).unwrap(), "to last");
    assert_eq!(sys.list_tree().descendants(top3), vec![two, one, backlog]);

    assert!(!sys.move_list(top3, Some(backlog), None).unwrap());
    assert_eq!(sys.list_tree().node(top3).unwrap().parent, None);
}

/// D3.2, the merge nobody can prevent: each replica nests the other's list.
/// Both must draw one identical tree, broken at the lowest id.
#[test]
fn two_replicas_nesting_each_others_list_merge_into_one_identical_tree() {
    let mut a = SystemDoc::create().unwrap();
    let x = list(&mut a, None, "X");
    let y = list(&mut a, None, "Y");
    let mut b = SystemDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    assert!(a.set_list_parent(x, Some(y)).unwrap());
    assert!(b.set_list_parent(y, Some(x)).unwrap());
    sync(&mut a, &mut b);

    let tree = a.list_tree();
    assert_eq!(shape(&a), shape(&b), "one tree on both replicas");
    assert_eq!(tree.repairs.cycles, 1);
    let (root, child) = if x < y { (x, y) } else { (y, x) };
    assert_eq!(tree.node(root).unwrap().parent, None, "the lowest id");
    assert_eq!(tree.node(child).unwrap().parent, Some(root));
    // Rule 3: the repair is in the reading, never in the record.
    assert_eq!(a.list_entry(root).unwrap().parent, Some(child));
}

/// D3.1: a parent that is deleted or unknown is ignored, and the list is
/// drawn under its nearest live ancestor.
#[test]
fn a_dead_parent_surfaces_its_children_under_the_nearest_live_ancestor() {
    let mut sys = SystemDoc::create().unwrap();
    let top = list(&mut sys, None, "Top");
    let mid = list(&mut sys, Some(top), "Mid");
    let leaf = list(&mut sys, Some(mid), "Leaf");
    sys.delete_list(mid, 2).unwrap();

    let tree = sys.list_tree();
    assert_eq!(tree.node(leaf).unwrap().parent, Some(top));
    assert_eq!(tree.node(leaf).unwrap().depth, 1);
    assert_eq!(tree.repairs.dead_parents, 1);
    assert_eq!(
        sys.list_entry(leaf).unwrap().parent,
        Some(mid),
        "nothing was rewritten"
    );

    sys.delete_list(top, 3).unwrap();
    assert_eq!(sys.list_tree().node(leaf).unwrap().parent, None);
}

/// D4: exactly the confirmed ids go, in one commit; a list a peer nested in
/// meanwhile survives and surfaces one level up.
#[test]
fn deleting_a_parent_tombstones_exactly_the_confirmed_ids() {
    let mut a = SystemDoc::create().unwrap();
    let top3 = list(&mut a, None, "Top 3");
    let backlog = list(&mut a, Some(top3), "Backlog");
    let core = list(&mut a, Some(top3), "Core Data");
    let other = list(&mut a, None, "Other");
    let mut b = SystemDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    // The person on `a` was shown two lists inside Top 3...
    let shown = a.list_tree().descendants(top3);
    assert_eq!(shown.len(), 2);
    // ...while `b` nested a third one in.
    assert!(b.set_list_parent(other, Some(top3)).unwrap());

    let gone = a.delete_list_tree(top3, &shown, 5).unwrap();
    assert_eq!(gone.len(), 3);
    sync(&mut a, &mut b);

    for peer in [&a, &b] {
        let live: Vec<ListId> = peer.lists().into_iter().map(|l| l.id).collect();
        assert_eq!(live, vec![other], "the list nested in meanwhile survives");
        assert_eq!(peer.list_tree().node(other).unwrap().parent, None);
        for id in [top3, backlog, core] {
            assert_eq!(peer.list_entry(id).unwrap().deleted_ms, Some(5));
        }
    }
}

/// A 0.1.x build reorders by writing dense ranks to the flat order it sees
/// and has never heard of `parent`.
#[test]
fn an_old_builds_reorder_changes_no_parent() {
    let mut sys = SystemDoc::create().unwrap();
    let top3 = list(&mut sys, None, "Top 3");
    let backlog = list(&mut sys, Some(top3), "Backlog");
    let other = list(&mut sys, None, "Other");

    for (rank, id) in [backlog, other, top3].into_iter().enumerate() {
        sys.set_list_order(id, Some(rank as i64)).unwrap();
    }
    let tree = sys.list_tree();
    assert_eq!(tree.node(backlog).unwrap().parent, Some(top3));
    assert_eq!(
        shape(&sys),
        vec![
            ("Other".into(), 0),
            ("Top 3".into(), 0),
            ("Backlog".into(), 1)
        ]
    );
}

/// ...and deletes one list at a time, which surfaces a parent's children.
#[test]
fn an_old_builds_delete_of_a_parent_surfaces_its_children() {
    let mut sys = SystemDoc::create().unwrap();
    let top3 = list(&mut sys, None, "Top 3");
    let backlog = list(&mut sys, Some(top3), "Backlog");
    sys.delete_list(top3, 2).unwrap();
    assert_eq!(shape(&sys), vec![("Backlog".into(), 0)]);
    assert_eq!(sys.list_entry(backlog).unwrap().deleted_ms, None);
}

/// A path is the ancestors' names, outermost first, read from the repaired
/// tree — so a parent's rename is the only write a path change takes.
#[test]
fn a_path_follows_a_parents_rename_and_is_never_stored() {
    let mut sys = SystemDoc::create().unwrap();
    let top3 = list(&mut sys, None, "Top 3");
    let backlog = list(&mut sys, Some(top3), "Backlog");
    assert_eq!(sys.list_tree().ancestors(backlog), vec!["Top 3"]);
    sys.rename_list(top3, "Top 5").unwrap();
    assert_eq!(sys.list_tree().ancestors(backlog), vec!["Top 5"]);
    assert_eq!(sys.list_entry(backlog).unwrap().name, "Backlog");
}

/// **A work count, not a timing.** A chain ten thousand deep is one visit
/// per record in each stage and never a walk per node; were it recursive it
/// would not return at all.
#[test]
fn a_ten_thousand_deep_chain_builds_in_one_pass() {
    const N: usize = 10_000;
    let ids: Vec<ListId> = (0..N).map(|_| ListId::new()).collect();
    let records: Vec<ListEntry> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| record(*id, i.checked_sub(1).map(|p| ids[p]), false))
        .collect();
    let tree = ListTree::build(records);
    assert_eq!(tree.nodes().len(), N);
    assert_eq!(tree.node(ids[N - 1]).unwrap().depth, N - 1);
    assert!(tree.steps() <= 6 * N, "{} steps for {N} lists", tree.steps());
}

/// The worst table a merge can leave: every list in one ring.
#[test]
fn a_cycle_of_every_list_builds_in_one_pass_and_breaks_at_the_lowest_id() {
    const N: usize = 10_000;
    let ids: Vec<ListId> = (0..N).map(|_| ListId::new()).collect();
    let records: Vec<ListEntry> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| record(*id, Some(ids[(i + 1) % N]), false))
        .collect();
    let tree = ListTree::build(records);
    assert_eq!(tree.nodes().len(), N, "no list is lost to a cycle");
    assert_eq!(tree.repairs.cycles, 1);
    let lowest = *ids.iter().min().unwrap();
    assert_eq!(tree.node(lowest).unwrap().parent, None);
    assert_eq!(
        tree.nodes().iter().filter(|n| n.parent.is_none()).count(),
        1
    );
    assert!(tree.steps() <= 6 * N, "{} steps for {N} lists", tree.steps());
}

/// A long run of tombstones above many live lists is walked once, through
/// the memo, rather than once per list under it.
#[test]
fn a_dead_chain_above_many_lists_is_resolved_once() {
    const DEAD: usize = 2_000;
    const LIVE: usize = 2_000;
    let root = ListId::new();
    let dead: Vec<ListId> = (0..DEAD).map(|_| ListId::new()).collect();
    let mut records = vec![record(root, None, false)];
    for (i, id) in dead.iter().enumerate() {
        let parent = if i == 0 { root } else { dead[i - 1] };
        records.push(record(*id, Some(parent), true));
    }
    let live: Vec<ListId> = (0..LIVE).map(|_| ListId::new()).collect();
    for id in &live {
        records.push(record(*id, Some(dead[DEAD - 1]), false));
    }
    let tree = ListTree::build(records);
    assert!(
        live.iter()
            .all(|id| tree.node(*id).unwrap().parent == Some(root))
    );
    assert_eq!(tree.repairs.dead_parents, LIVE);
    let n = DEAD + LIVE + 1;
    assert!(tree.steps() <= 6 * n, "{} steps for {n} records", tree.steps());
}

/// A ring made only of tombstones leads nowhere live: the list under it is
/// drawn at the top, and the build still returns.
#[test]
fn a_ring_of_tombstones_puts_its_children_at_the_top() {
    let (a, b, child) = (ListId::new(), ListId::new(), ListId::new());
    let tree = ListTree::build(vec![
        record(a, Some(b), true),
        record(b, Some(a), true),
        record(child, Some(a), false),
    ]);
    assert_eq!(tree.node(child).unwrap().parent, None);
}

/// D8's consequence: a name alone no longer identifies a list, so a list
/// whose name is shared says where it is. Only the lists that need to.
#[test]
fn a_shared_name_gets_a_path_and_nothing_else_does() {
    let mut sys = SystemDoc::create().unwrap();
    let top3 = list(&mut sys, None, "Top 3");
    let inner = list(&mut sys, Some(top3), "Inner");
    let nested = list(&mut sys, Some(inner), "Backlog");
    let top = list(&mut sys, None, "backlog");
    let alone = list(&mut sys, Some(top3), "Core Data");

    let tree = sys.list_tree();
    let paths = shared_name_paths(tree.nodes(), " / ");
    assert_eq!(
        paths.get(&nested).map(String::as_str),
        Some("Top 3 / Inner / ")
    );
    assert_eq!(
        paths.get(&top),
        None,
        "the top-level namesake has nothing above it to show"
    );
    assert_eq!(paths.get(&alone), None, "a name nobody shares needs no path");
    assert_eq!(paths.len(), 1);

    // A parent's rename changes the path shown and nothing else.
    sys.rename_list(top3, "Top 5").unwrap();
    let paths = shared_name_paths(sys.list_tree().nodes(), " / ");
    assert_eq!(
        paths.get(&nested).map(String::as_str),
        Some("Top 5 / Inner / ")
    );
    assert_eq!(sys.list_entry(nested).unwrap().name, "Backlog");
}
