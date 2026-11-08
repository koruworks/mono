// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::fixtures::{BIDI_LINE, COMPOSED, DECOMPOSED, SAMPLES, UNICODE_FILENAME};

fn at() -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000)
}

fn fresh() -> Manifest {
    Manifest::create(WorkspaceId::new(), "Workspace", at()).expect("create")
}

/// Two replicas of one manifest, as two peers would have.
fn pair() -> (Manifest, Manifest) {
    let a = fresh();
    let b = Manifest::from_snapshot(&a.export_snapshot().unwrap()).unwrap();
    (a, b)
}

/// Exchange everything both ways until neither has anything new.
fn converge(a: &mut Manifest, b: &mut Manifest) {
    let from_a = a.export_updates_since(&b.version()).unwrap();
    let from_b = b.export_updates_since(&a.version()).unwrap();
    b.import(&from_a).unwrap();
    a.import(&from_b).unwrap();
}

fn titles(m: &Manifest) -> Vec<String> {
    m.entries().into_iter().map(|e| e.title).collect()
}

// ------------------------------------------------------------------ meta

#[test]
fn a_new_manifest_knows_what_it_is() {
    let ws = WorkspaceId::new();
    let m = Manifest::create(ws, "Quarterly plan", at()).unwrap();
    assert_eq!(m.workspace_id(), Some(ws));
    assert_eq!(m.name().as_deref(), Some("Quarterly plan"));
    assert_eq!(m.created_at(), Some(at()));
    assert_eq!(m.schema_version(), Some(CRDT_SCHEMA_VERSION));
    assert!(m.compatibility().writable());
    assert!(m.entries().is_empty());
}

#[test]
fn a_manifest_round_trips_through_a_snapshot() {
    let mut a = fresh();
    let d = DocumentId::new();
    a.add_document(d, "季度计划", "page", None).unwrap();

    let b = Manifest::from_snapshot(&a.export_snapshot().unwrap()).unwrap();
    assert_eq!(b.workspace_id(), a.workspace_id());
    assert_eq!(b.entries(), a.entries());
    assert!(b.contains(d), "the index was not rebuilt on load");
}

#[test]
fn a_name_in_every_supported_language_survives_a_snapshot() {
    for s in SAMPLES {
        let mut a = Manifest::create(WorkspaceId::new(), s.title, at()).unwrap();
        a.add_document(DocumentId::new(), s.body, "page", None)
            .unwrap();
        let b = Manifest::from_snapshot(&a.export_snapshot().unwrap()).unwrap();
        assert_eq!(b.name().as_deref(), Some(s.title), "{}", s.tag);
        assert_eq!(titles(&b), vec![s.body.to_string()], "{}", s.tag);
    }
}

#[test]
fn bidi_and_both_unicode_spellings_survive_byte_for_byte() {
    let mut a = fresh();
    for (i, text) in [BIDI_LINE, COMPOSED, DECOMPOSED, UNICODE_FILENAME]
        .into_iter()
        .enumerate()
    {
        a.add_document(DocumentId::new(), text, "page", None)
            .unwrap_or_else(|e| panic!("{i}: {e}"));
    }
    let b = Manifest::from_snapshot(&a.export_snapshot().unwrap()).unwrap();
    assert_eq!(
        titles(&b),
        vec![BIDI_LINE, COMPOSED, DECOMPOSED, UNICODE_FILENAME]
    );
    // The two spellings stay two spellings: content is never normalised.
    assert_ne!(titles(&b)[1], titles(&b)[2]);
}

#[test]
fn shared_workspace_appearance_converges_and_can_clear_optional_fields() {
    let (mut a, mut b) = pair();
    a.set_appearance(Some("ðŸŒ´"), Some("purple"), true, true)
        .unwrap();
    converge(&mut a, &mut b);
    assert_eq!(b.icon().as_deref(), Some("ðŸŒ´"));
    assert_eq!(b.color().as_deref(), Some("purple"));
    assert_eq!(b.pinned(), Some(true));
    assert_eq!(b.archived(), Some(true));

    b.set_appearance(None, None, false, false).unwrap();
    converge(&mut a, &mut b);
    assert_eq!(a.icon_state(), Some(None));
    assert_eq!(a.color_state(), Some(None));
    assert_eq!(a.pinned(), Some(false));
    assert_eq!(a.archived(), Some(false));
    assert_eq!(a.icon_state(), b.icon_state());
    assert_eq!(a.color_state(), b.color_state());
}

// ------------------------------------------------------------------ structure

#[test]
fn documents_come_back_in_tree_order_with_their_parents() {
    let mut m = fresh();
    let parent = DocumentId::new();
    let child = DocumentId::new();
    let sibling = DocumentId::new();
    m.add_document(parent, "Parent", "page", None).unwrap();
    m.add_document(child, "Child", "page", Some(parent))
        .unwrap();
    m.add_document(sibling, "Sibling", "page", None).unwrap();

    let entries = m.entries();
    assert_eq!(
        entries.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(),
        ["Parent", "Child", "Sibling"]
    );
    assert_eq!(entries[0].parent, None);
    assert_eq!(entries[1].parent, Some(parent));
    assert_eq!(entries[2].parent, None);
    // Ordinal is a position among siblings, not a position in the walk.
    assert_eq!(entries[0].ordinal, 0);
    assert_eq!(entries[1].ordinal, 0);
    assert_eq!(entries[2].ordinal, 1);
}

/// **A document can be registered at a position, not only at the end.**
///
/// What *New document* needs: the page it was made from is the anchor, and
/// `ordinal + 1` is below it. Done inside the registration rather than as a move
/// afterwards, so no client ever sees it at the end first.
#[test]
fn a_document_can_be_added_among_its_siblings() {
    let mut m = fresh();
    let (a, b, mid) = (DocumentId::new(), DocumentId::new(), DocumentId::new());
    m.add_document(a, "A", "page", None).unwrap();
    m.add_document(b, "B", "page", None).unwrap();
    m.add_document_at(mid, "Mid", "page", None, Some(1))
        .unwrap();
    assert_eq!(
        m.entries()
            .iter()
            .map(|e| e.title.as_str())
            .collect::<Vec<_>>(),
        ["A", "Mid", "B"]
    );

    // Under a parent, the index counts that parent's children only.
    let (kid, first) = (DocumentId::new(), DocumentId::new());
    m.add_document(first, "First child", "page", Some(a))
        .unwrap();
    m.add_document_at(kid, "New child", "page", Some(a), Some(0))
        .unwrap();
    assert_eq!(
        m.entries()
            .iter()
            .map(|e| e.title.as_str())
            .collect::<Vec<_>>(),
        ["A", "New child", "First child", "Mid", "B"]
    );
}

/// **Past the end is the end**, not an error. A new page below the last one asks
/// for an index one past every sibling, and `mov_to` refuses that outright — so
/// the clamp lives in `add_document_at` rather than in each caller.
#[test]
fn adding_past_the_end_lands_last() {
    let mut m = fresh();
    let (a, b) = (DocumentId::new(), DocumentId::new());
    m.add_document(a, "A", "page", None).unwrap();
    m.add_document_at(b, "B", "page", None, Some(99)).unwrap();
    assert_eq!(
        m.entries()
            .iter()
            .map(|e| e.title.as_str())
            .collect::<Vec<_>>(),
        ["A", "B"]
    );
}

#[test]
fn adding_the_same_document_twice_is_refused() {
    let mut m = fresh();
    let d = DocumentId::new();
    m.add_document(d, "One", "page", None).unwrap();
    assert_eq!(
        m.add_document(d, "Again", "page", None).unwrap_err().code(),
        Code::DocumentExists
    );
}

#[test]
fn an_unknown_document_is_not_found_rather_than_a_panic() {
    let mut m = fresh();
    let ghost = DocumentId::new();
    assert_eq!(
        m.rename_document(ghost, "x").unwrap_err().code(),
        Code::DocumentNotFound
    );
    assert_eq!(
        m.remove_document(ghost).unwrap_err().code(),
        Code::DocumentNotFound
    );
}

#[test]
fn a_move_reorders_among_siblings() {
    let mut m = fresh();
    let (a, b, c) = (DocumentId::new(), DocumentId::new(), DocumentId::new());
    for (id, t) in [(a, "A"), (b, "B"), (c, "C")] {
        m.add_document(id, t, "page", None).unwrap();
    }
    m.move_document(c, None, 0).unwrap();
    assert_eq!(titles(&m), ["C", "A", "B"]);
}

#[test]
fn a_document_cannot_be_moved_inside_itself() {
    let mut m = fresh();
    let parent = DocumentId::new();
    let child = DocumentId::new();
    m.add_document(parent, "Parent", "page", None).unwrap();
    m.add_document(child, "Child", "page", Some(parent))
        .unwrap();

    assert_eq!(
        m.move_document(parent, Some(parent), 0).unwrap_err().code(),
        Code::InvalidRequest
    );
    // The subtree case: a cycle that a naive parent check misses.
    assert_eq!(
        m.move_document(parent, Some(child), 0).unwrap_err().code(),
        Code::InvalidRequest
    );
    assert_eq!(titles(&m), ["Parent", "Child"]);
}

#[test]
fn removing_a_document_reports_its_whole_subtree() {
    let mut m = fresh();
    let parent = DocumentId::new();
    let child = DocumentId::new();
    let grandchild = DocumentId::new();
    let keep = DocumentId::new();
    m.add_document(parent, "Parent", "page", None).unwrap();
    m.add_document(child, "Child", "page", Some(parent))
        .unwrap();
    m.add_document(grandchild, "Grandchild", "page", Some(child))
        .unwrap();
    m.add_document(keep, "Keep", "page", None).unwrap();

    let mut removed = m.remove_document(parent).unwrap();
    removed.sort();
    let mut expected = vec![parent, child, grandchild];
    expected.sort();
    assert_eq!(removed, expected, "the caller must learn every orphaned id");

    assert_eq!(titles(&m), ["Keep"]);
    assert!(!m.contains(parent) && !m.contains(grandchild));
}

// ------------------------------------------------------------------ versioning

#[test]
fn a_manifest_from_a_newer_build_is_read_only() {
    let mut m = fresh();
    // Stand in for a replica written by a future release.
    m.doc
        .get_map(C_META)
        .insert(K_SCHEMA, CRDT_SCHEMA_VERSION + 1)
        .unwrap();
    m.doc.commit();

    assert!(!m.compatibility().writable());
    // Every mutation refuses, and with the code a client branches on.
    assert_eq!(
        m.add_document(DocumentId::new(), "x", "page", None)
            .unwrap_err()
            .code(),
        Code::SchemaTooNew
    );
    assert_eq!(
        m.rename_document(DocumentId::new(), "x")
            .unwrap_err()
            .code(),
        Code::SchemaTooNew
    );
    // Reading still works — that is what "read-only" means.
    assert!(m.name().is_some());
    assert!(m.export_snapshot().is_ok());
}

// ------------------------------------------------------------------ transport

#[test]
fn a_frame_already_applied_is_recognised_from_its_header() {
    let (mut a, mut b) = pair();
    a.add_document(DocumentId::new(), "One", "page", None)
        .unwrap();
    let frame = a.export_updates_since(&b.version()).unwrap();

    assert!(!b.frame_is_known(&frame), "b has not seen it yet");
    b.import(&frame).unwrap();
    assert!(b.frame_is_known(&frame), "b must not replay what it has");
}

#[test]
fn garbage_is_rejected_rather_than_accepted_as_empty() {
    let mut m = fresh();
    assert_eq!(
        m.import(b"not a loro update").unwrap_err().code(),
        Code::CrdtRejected
    );
    assert!(!m.frame_is_known(b"not a loro update"));
}

#[test]
fn importing_frames_names_the_one_that_failed() {
    let mut a = fresh();
    let mut b = Manifest::from_snapshot(&a.export_snapshot().unwrap()).unwrap();
    a.add_document(DocumentId::new(), "Good", "page", None)
        .unwrap();
    let good = a.export_updates_since(&b.version()).unwrap();

    let err = b
        .import_frames(&[good, b"rubbish".to_vec()])
        .expect_err("the second frame is not a frame");
    // Which frame was bad is the whole of a recovery report — this is why
    // frames are imported one at a time rather than as a batch.
    assert!(err.message().contains("frame 1"), "{err}");
}

#[test]
fn an_empty_batch_is_a_no_op() {
    let mut m = fresh();
    let before = m.version();
    m.import_frames(&[]).unwrap();
    assert!(m.version().includes_vv(&before) && before.includes_vv(&m.version()));
}

#[test]
fn a_peer_id_can_be_pinned_and_survives_edits() {
    let m = fresh();
    m.set_peer_id(0x00C0_FFEE).unwrap();
    assert_eq!(m.peer_id(), 0x00C0_FFEE);
}

// ------------------------------------------------------------------ concurrency
//
// The manifest cases matter most: "converged" is not the same as "the document
// is still where the user put it".

#[test]
fn concurrent_creates_both_survive() {
    let (mut a, mut b) = pair();
    a.add_document(DocumentId::new(), "From A", "page", None)
        .unwrap();
    b.add_document(DocumentId::new(), "From B", "page", None)
        .unwrap();
    converge(&mut a, &mut b);

    assert_eq!(a.entries().len(), 2, "a create was lost");
    assert_eq!(a.entries(), b.entries(), "replicas disagree");
    let mut got = titles(&a);
    got.sort();
    assert_eq!(got, ["From A", "From B"]);
}

#[test]
fn rename_versus_rename_converges_on_one_whole_title() {
    let (mut a, mut b) = pair();
    let d = DocumentId::new();
    a.add_document(d, "Original", "page", None).unwrap();
    converge(&mut a, &mut b);

    a.rename_document(d, "Planejamento trimestral").unwrap();
    b.rename_document(d, "四半期計画").unwrap();
    converge(&mut a, &mut b);

    assert_eq!(a.entries(), b.entries(), "replicas disagree");
    let won = &a.entries()[0].title;
    // The semantic that matters: one edit wins outright. A character-merged
    // title would converge too, and be gibberish in both languages.
    assert!(
        won == "Planejamento trimestral" || won == "四半期計画",
        "titles were merged rather than one winning: {won:?}"
    );
}

#[test]
fn move_versus_move_converges_on_one_position() {
    let (mut a, mut b) = pair();
    let (x, y, z) = (DocumentId::new(), DocumentId::new(), DocumentId::new());
    for (id, t) in [(x, "X"), (y, "Y"), (z, "Z")] {
        a.add_document(id, t, "page", None).unwrap();
    }
    converge(&mut a, &mut b);

    a.move_document(z, None, 0).unwrap();
    b.move_document(z, None, 1).unwrap();
    converge(&mut a, &mut b);

    assert_eq!(a.entries(), b.entries(), "replicas disagree on order");
    let mut names = titles(&a);
    names.sort();
    assert_eq!(
        names,
        ["X", "Y", "Z"],
        "a move lost or duplicated a document"
    );
}

#[test]
fn edit_versus_delete_does_not_resurrect_a_deleted_document() {
    let (mut a, mut b) = pair();
    let d = DocumentId::new();
    a.add_document(d, "Doomed", "page", None).unwrap();
    converge(&mut a, &mut b);

    a.remove_document(d).unwrap();
    b.rename_document(d, "Renamed anyway").unwrap();
    converge(&mut a, &mut b);

    assert_eq!(a.entries(), b.entries(), "replicas disagree");
    // Loro converges either way; the Koru semantic is that a delete wins, so a
    // rename cannot bring a document back from nowhere.
    assert!(
        a.entries().is_empty(),
        "a deleted document came back: {:?}",
        a.entries()
    );
    assert!(!a.contains(d) && !b.contains(d));
}

#[test]
fn a_replica_that_was_offline_a_long_time_catches_up_in_one_exchange() {
    let (mut a, mut b) = pair();
    for i in 0..50 {
        a.add_document(DocumentId::new(), &format!("Doc {i}"), "page", None)
            .unwrap();
    }
    // One delta for fifty changes, not fifty.
    let catch_up = a.export_updates_since(&b.version()).unwrap();
    b.import(&catch_up).unwrap();

    assert_eq!(b.entries().len(), 50);
    assert_eq!(a.entries(), b.entries());
}

#[test]
fn a_duplicate_delivery_changes_nothing() {
    let (mut a, mut b) = pair();
    a.add_document(DocumentId::new(), "Once", "page", None)
        .unwrap();
    let frame = a.export_updates_since(&b.version()).unwrap();

    b.import(&frame).unwrap();
    let after_first = b.entries();
    // Retries happen: a transport that redelivers must cost bytes, not
    // correctness.
    b.import(&frame).unwrap();
    b.import_frames(&[frame]).unwrap();
    assert_eq!(b.entries(), after_first);
}

#[test]
fn a_stray_duplicate_id_resolves_the_same_way_on_every_replica() {
    // There is no mergeable `LoroTree::create`, so two peers can each create a
    // node claiming one DocumentId. Both replicas must pick the same winner
    // without exchanging anything about the choice.
    let (mut a, mut b) = pair();
    let shared = DocumentId::new();
    a.add_document(shared, "From A", "page", None).unwrap();
    b.add_document(shared, "From B", "page", None).unwrap();
    converge(&mut a, &mut b);

    assert_eq!(
        a.entries(),
        b.entries(),
        "replicas picked different winners"
    );
    assert_eq!(a.entries().len(), 1, "one id must yield one row");
    assert!(a.contains(shared));
}

// ---------------------------------------------------------------- the trash

/// **Move to trash keeps the document**, which is the whole of ADR-69: losing
/// content is the one unrecoverable bug, so the button labelled *Move to trash*
/// must not be able to cause it.
#[test]
fn trashing_a_document_stamps_it_rather_than_removing_it() {
    let mut m = Manifest::create(WorkspaceId::new(), "W", at()).unwrap();
    let doc = DocumentId::new();
    m.add_document(doc, "Roadmap", "page", None).unwrap();

    m.trash_document(doc, at(), Some("Tester")).unwrap();
    let entries = m.entries();
    assert_eq!(entries.len(), 1, "the entry was removed, not trashed");
    assert_eq!(entries[0].trashed_at, Some(at()));
    assert!(m.is_trashed(doc));

    m.restore_document(doc).unwrap();
    assert_eq!(m.entries()[0].trashed_at, None);
    assert!(!m.is_trashed(doc));
}

/// **A trashed parent takes its subtree with it, by inheritance.**
///
/// The child is never stamped, so restoring the parent restores the lot —
/// and the alternative, stamping every descendant, would have to be undone
/// exactly right on the way back out.
#[test]
fn a_child_of_a_trashed_document_is_trashed_too() {
    let mut m = Manifest::create(WorkspaceId::new(), "W", at()).unwrap();
    let parent = DocumentId::new();
    let child = DocumentId::new();
    m.add_document(parent, "Section", "page", None).unwrap();
    m.add_document(child, "Page", "page", Some(parent)).unwrap();

    m.trash_document(parent, at(), Some("Tester")).unwrap();
    assert!(m.is_trashed(child), "the child stayed visible");
    assert_eq!(
        m.entries()
            .iter()
            .find(|e| e.id == child)
            .and_then(|e| e.trashed_at),
        None,
        "the child was stamped rather than inheriting"
    );

    m.restore_document(parent).unwrap();
    assert!(!m.is_trashed(child), "the child did not come back with it");
}

#[test]
fn explicitly_restoring_an_inherited_child_detaches_it_from_the_trashed_parent() {
    let mut m = Manifest::create(WorkspaceId::new(), "W", at()).unwrap();
    let parent = DocumentId::new();
    let child = DocumentId::new();
    m.add_document(parent, "Section", "page", None).unwrap();
    m.add_document(child, "Page", "page", Some(parent)).unwrap();
    m.trash_document(parent, at(), Some("Tester")).unwrap();

    m.restore_document(child).unwrap();

    let child_entry = m
        .entries()
        .into_iter()
        .find(|entry| entry.id == child)
        .unwrap();
    assert_eq!(child_entry.parent, None, "the restored child stayed nested");
    assert!(!m.is_trashed(child), "the restore succeeded invisibly");
    assert!(
        m.is_trashed(parent),
        "restoring the child restored its parent"
    );
}

/// Listing both sides classifies inherited trash during the tree walk, while
/// keeping the original order within each side and the child's raw stamp clear.
#[test]
fn partitioned_entries_classifies_inherited_trash_in_one_listing() {
    let mut m = Manifest::create(WorkspaceId::new(), "W", at()).unwrap();
    let live_a = DocumentId::new();
    let trashed_parent = DocumentId::new();
    let inherited_child = DocumentId::new();
    let live_b = DocumentId::new();
    m.add_document(live_a, "A", "page", None).unwrap();
    m.add_document(trashed_parent, "Bin", "page", None).unwrap();
    m.add_document(inherited_child, "Child", "page", Some(trashed_parent))
        .unwrap();
    m.add_document(live_b, "B", "page", None).unwrap();
    m.trash_document(trashed_parent, at(), Some("Tester"))
        .unwrap();

    let (live, trashed) = m.partitioned_entries();
    assert_eq!(
        live.iter().map(|e| e.id).collect::<Vec<_>>(),
        [live_a, live_b]
    );
    assert_eq!(
        trashed.iter().map(|e| e.id).collect::<Vec<_>>(),
        [trashed_parent, inherited_child]
    );
    assert_eq!(
        trashed[1].trashed_at, None,
        "classification must not rewrite the inherited child's raw stamp"
    );
}

/// A child trashed **on its own** keeps its own stamp, and stays in the bin
/// when its parent comes back out. That distinction is the reason inheritance
/// is a query rather than a rewrite of the subtree.
#[test]
fn a_child_trashed_on_its_own_stays_trashed_when_its_parent_returns() {
    let mut m = Manifest::create(WorkspaceId::new(), "W", at()).unwrap();
    let parent = DocumentId::new();
    let child = DocumentId::new();
    m.add_document(parent, "Section", "page", None).unwrap();
    m.add_document(child, "Page", "page", Some(parent)).unwrap();

    m.trash_document(child, at(), Some("Tester")).unwrap();
    m.trash_document(parent, at(), Some("Tester")).unwrap();
    m.restore_document(parent).unwrap();

    assert!(!m.is_trashed(parent));
    assert!(
        m.is_trashed(child),
        "the child was dragged back out with it"
    );
}

#[test]
fn a_restore_clears_observed_deletions_but_not_a_concurrent_unseen_one() {
    let (mut a, mut b) = pair();
    let doc = DocumentId::new();
    a.add_document(doc, "Roadmap", "page", None).unwrap();
    a.trash_document(doc, at(), Some("A")).unwrap();
    converge(&mut a, &mut b);

    a.restore_document(doc).unwrap();
    b.trash_document(doc, Timestamp::from_millis(at().millis() + 1), Some("B"))
        .unwrap();
    converge(&mut a, &mut b);

    assert!(a.is_trashed(doc));
    assert!(b.is_trashed(doc));
    assert_eq!(a.entries(), b.entries());
    assert_eq!(a.entries()[0].trashed_by.as_deref(), Some("B"));

    // A later explicit restore observes B's deletion and therefore clears it.
    a.restore_document(doc).unwrap();
    converge(&mut a, &mut b);
    assert!(!a.is_trashed(doc));
    assert!(!b.is_trashed(doc));
}

#[test]
fn a_concurrent_modify_and_trash_keeps_the_modified_document_recoverable() {
    let (mut a, mut b) = pair();
    let doc = DocumentId::new();
    a.add_document(doc, "Original", "page", None).unwrap();
    converge(&mut a, &mut b);

    a.rename_document(doc, "Edited while offline").unwrap();
    b.trash_document(doc, at(), Some("B")).unwrap();
    converge(&mut a, &mut b);

    assert_eq!(a.entries(), b.entries());
    assert_eq!(a.entries()[0].title, "Edited while offline");
    assert!(a.is_trashed(doc));

    b.restore_document(doc).unwrap();
    converge(&mut a, &mut b);
    assert_eq!(a.entries()[0].title, "Edited while offline");
    assert!(!a.is_trashed(doc));
    assert!(!b.is_trashed(doc));
}

/// **Emptying the trash is the one thing that really deletes**, and it is
/// `remove_document` — reached from nowhere else. A test that let *Move to
/// trash* call this would be the bug ADR-69 exists to prevent.
#[test]
fn removing_a_document_is_a_separate_and_final_operation() {
    let mut m = Manifest::create(WorkspaceId::new(), "W", at()).unwrap();
    let doc = DocumentId::new();
    m.add_document(doc, "Roadmap", "page", None).unwrap();
    m.trash_document(doc, at(), Some("Tester")).unwrap();

    m.remove_document(doc).unwrap();
    assert!(m.entries().is_empty());
    assert!(!m.contains(doc));
    // Nothing to restore: this is the irreversible one.
    assert!(m.restore_document(doc).is_err());
}

// ------------------------------------------------------------- file notes

/// The link round-trips the same transport every other meta key does: a peer
/// that imports the snapshot sees the same attachment.
#[test]
fn a_note_attachment_round_trips_through_a_snapshot() {
    let mut a = fresh();
    let file = DocumentId::new();
    let note = DocumentId::new();
    a.add_document(file, "Report.pdf", "file", None).unwrap();
    a.add_document(note, "Report.pdf", "note", Some(file))
        .unwrap();
    a.set_note(file, Some(note)).unwrap();

    let b = Manifest::from_snapshot(&a.export_snapshot().unwrap()).unwrap();
    assert_eq!(b.note_of(file).unwrap(), Some(note));
    assert_eq!(b.entries(), a.entries());
    let file_entry = b.entries().into_iter().find(|e| e.id == file).unwrap();
    assert_eq!(file_entry.note, Some(note));

    // Clearing deletes the key: a manifest that lost its note reads exactly
    // like one that never had it.
    let mut c = b;
    c.set_note(file, None).unwrap();
    assert_eq!(c.note_of(file).unwrap(), None);
    assert!(
        c.entries()
            .into_iter()
            .find(|e| e.id == file)
            .is_some_and(|e| e.note.is_none())
    );
}

/// A manifest written before the key existed simply has no note: the absent
/// key is `None`, not an error — the additive-key rule that kept this off
/// `CRDT_SCHEMA_VERSION`.
#[test]
fn a_document_written_before_notes_reads_as_having_none() {
    let mut m = fresh();
    let file = DocumentId::new();
    m.add_document(file, "Report.pdf", "file", None).unwrap();

    assert_eq!(m.note_of(file).unwrap(), None);
    let entry = m.entries().into_iter().find(|e| e.id == file).unwrap();
    assert_eq!(entry.note, None);
    // And the partitioned listing answers the same way.
    let (live, _) = m.partitioned_entries();
    assert_eq!(live[0].note, None);
}

/// The note is the file's *child* in the manifest tree, so binning the file
/// bins the note by inheritance and restoring the file brings it back — no
/// lifecycle code of the note's own.
#[test]
fn trashing_a_file_trashes_its_note_and_restoring_returns_it() {
    let mut m = fresh();
    let file = DocumentId::new();
    let note = DocumentId::new();
    m.add_document(file, "Report.pdf", "file", None).unwrap();
    m.add_document(note, "Report.pdf", "note", Some(file))
        .unwrap();
    m.set_note(file, Some(note)).unwrap();

    assert!(!m.is_trashed(note));
    m.trash_document(file, at(), Some("Tester")).unwrap();
    assert!(
        m.is_trashed(note),
        "the note did not inherit the file's trash"
    );

    m.restore_document(file).unwrap();
    assert!(!m.is_trashed(note));
}

/// Purging the file takes the note with it: `remove_document` answers the
/// whole subtree.
#[test]
fn removing_a_file_returns_its_note_in_the_removed_subtree() {
    let mut m = fresh();
    let file = DocumentId::new();
    let note = DocumentId::new();
    m.add_document(file, "Report.pdf", "file", None).unwrap();
    m.add_document(note, "Report.pdf", "note", Some(file))
        .unwrap();
    m.set_note(file, Some(note)).unwrap();

    let removed = m.remove_document(file).unwrap();
    assert!(removed.contains(&file));
    assert!(
        removed.contains(&note),
        "the note survived its file's purge"
    );
    assert!(m.entries().is_empty());
}

/// Every change's commit message in `m`, oldest first, `None` for a change
/// that has none.
fn commit_messages(m: &Manifest) -> Vec<Option<String>> {
    let mut messages = Vec::new();
    let frontiers = m.doc.oplog_frontiers();
    let ids: Vec<loro::ID> = frontiers.iter().collect();
    m.doc
        .travel_change_ancestors(&ids, &mut |change| {
            messages.push((change.lamport, change.message.as_deref().map(str::to_owned)));
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    messages.sort_by_key(|(lamport, _)| *lamport);
    messages.into_iter().map(|(_, message)| message).collect()
}

/// **The Store's commits on behalf of an app connection carry its message,
/// and only those** (Cloud MCP Plan 2.1; `contracts/connector-attribution-v1.md`).
/// A manifest makes no message of its own; one labelled while the
/// subscription lives keeps it in an exported update, and a write refused
/// before it made an operation leaves nothing behind to label the next
/// commit somebody else makes.
#[test]
fn an_attributed_commit_carries_its_message_and_nothing_else_does() {
    let mut m = fresh();
    m.add_document(DocumentId::new(), "Before", "page", None)
        .unwrap();
    assert!(
        commit_messages(&m).iter().all(Option::is_none),
        "a manifest writes no commit message of its own"
    );

    let existing = DocumentId::new();
    m.add_document(existing, "Existing", "page", None).unwrap();
    {
        let _attributed = m.attribute_commits("refused");
        m.add_document(existing, "Again", "page", None)
            .expect_err("already in this manifest");
    }
    let before = m.version();
    let mut follower = Manifest::from_snapshot(&m.export_snapshot().unwrap()).unwrap();
    {
        let _attributed = m.attribute_commits(r#"{"koru":"actor/1"}"#);
        m.add_document(DocumentId::new(), "Made for an assistant", "page", None)
            .unwrap();
    }
    m.add_document(DocumentId::new(), "After", "page", None)
        .unwrap();

    let messages = commit_messages(&m);
    assert_eq!(
        messages.iter().flatten().collect::<Vec<_>>(),
        [r#"{"koru":"actor/1"}"#],
        "exactly the attributed commit is labelled: {messages:?}"
    );

    // The message travels with the operations, in a snapshot and in the
    // update a subscriber is sent.
    let replica = Manifest::from_snapshot(&m.export_snapshot().unwrap()).unwrap();
    assert_eq!(commit_messages(&replica), messages);
    follower
        .import(&m.export_updates_since(&before).unwrap())
        .unwrap();
    assert_eq!(commit_messages(&follower), messages);
}
