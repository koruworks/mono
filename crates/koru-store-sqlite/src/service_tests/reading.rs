// SPDX-License-Identifier: MPL-2.0

//! The linked reading document (OCR TECHNICAL D9, D14, §7 "Identity and
//! registration"), against real storage held in memory: `ensure_reading`, the
//! two-device registration that converges on one document, the lifecycle a
//! reading document inherits from its parent, the transfer that carries it —
//! and the transfer that now carries a file's note link too — and the
//! checkpoint that records no history for an unversioned kind.

use super::*;
use koru_store_core::errors::Code;
use koru_store_core::ids::reading_document_id;
use koru_store_core::manifest::Manifest;
use koru_store_core::ports::UpdateOrigin;
use koru_store_core::{CheckpointPolicy, DocumentId};

/// The token `koru_core::explore::KIND_READING` spells. A literal on purpose,
/// as this crate's other tests write `"page"`: the store links no node model.
const READING: &str = "koru-reading";

type MemoryStore = Store<SqliteCatalog, SqliteBackend>;

fn memory_store() -> MemoryStore {
    let catalog =
        SqliteCatalog::in_named_memory(&koru_sqlite::MemoryName::fresh("catalog.db")).unwrap();
    Store::open(catalog, SqliteBackend::in_memory()).unwrap()
}

fn at_ms(ms: i64) -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000 + ms)
}

/// A file, as the import path registers one.
fn a_file(store: &mut MemoryStore, workspace: WorkspaceId, title: &str) -> DocumentId {
    let doc = loro::LoroDoc::new();
    doc.commit();
    store
        .import_document_at_with_mime(
            workspace,
            DocumentId::new(),
            title,
            "file",
            Some("application/pdf"),
            None,
            None,
            doc.export(loro::ExportMode::Snapshot).unwrap(),
            at(),
        )
        .unwrap()
        .0
}

fn a_page(
    store: &mut MemoryStore,
    workspace: WorkspaceId,
    parent: Option<DocumentId>,
) -> DocumentId {
    store
        .create_document(workspace, "Page", "page", parent, None, at())
        .unwrap()
        .0
}

/// Write one key into a document as a client would, and make it durable.
fn write(store: &mut MemoryStore, workspace: WorkspaceId, document: DocumentId, key: &str) {
    let doc = loro::LoroDoc::new();
    doc.import(&store.snapshot(workspace, document).unwrap())
        .unwrap();
    let before = doc.oplog_vv();
    doc.get_map("readings").insert(key, key).unwrap();
    doc.commit();
    store
        .apply_update(
            workspace,
            document,
            doc.export(loro::ExportMode::updates(&before)).unwrap(),
            at(),
            UpdateOrigin::Store,
        )
        .unwrap();
    store.flush(workspace, document, at()).unwrap();
}

/// The keys a document's `readings` map holds, read back from the store.
fn keys(store: &mut MemoryStore, workspace: WorkspaceId, document: DocumentId) -> Vec<String> {
    let doc = loro::LoroDoc::new();
    doc.import(&store.snapshot(workspace, document).unwrap())
        .unwrap();
    let mut keys: Vec<String> = doc
        .get_map("readings")
        .keys()
        .map(|key| key.to_string())
        .collect();
    keys.sort();
    keys
}

/// Everything `target` lacks of `document` that `source` has, applied and
/// flushed — one direction of a sync, as the replication path delivers it.
fn sync_one(
    source: &mut MemoryStore,
    target: &mut MemoryStore,
    workspace: WorkspaceId,
    document: DocumentId,
) {
    let have = target.version(workspace, document).unwrap();
    let update = source.updates_since(workspace, document, &have).unwrap();
    target
        .apply_update(workspace, document, update, at(), UpdateOrigin::Store)
        .unwrap();
    target.flush(workspace, document, at()).unwrap();
}

fn entry(
    store: &mut MemoryStore,
    workspace: WorkspaceId,
    id: DocumentId,
) -> Option<koru_store_core::DocumentEntry> {
    store
        .documents(workspace)
        .unwrap()
        .into_iter()
        .find(|entry| entry.id == id)
}

fn live_manifest(store: &mut MemoryStore, workspace: WorkspaceId) -> Manifest {
    let id = store.manifest_id(workspace).unwrap();
    Manifest::from_snapshot(&store.snapshot(workspace, id).unwrap()).unwrap()
}

// ------------------------------------------------------------ registration

/// **One reading document per entry, however many times it is asked for**,
/// and a file and a page each take one. It is registered empty, untitled, as
/// the parent's last child, under the derived id; the second call registers
/// nothing.
#[test]
fn ensure_reading_twice_returns_the_same_document_for_a_file_and_a_page() {
    let mut store = memory_store();
    let workspace = workspace(&mut store);
    let file = a_file(&mut store, workspace, "Scan.pdf");
    let note = store.ensure_file_note(workspace, file, at()).unwrap().id;
    let page = a_page(&mut store, workspace, None);
    let sub_page = a_page(&mut store, workspace, Some(page));

    for (parent, earlier_child) in [(file, note), (page, sub_page)] {
        let first = store.ensure_reading(workspace, parent, at()).unwrap();
        assert!(first.created);
        assert!(
            !first.delta.is_empty(),
            "a registration changed the manifest"
        );
        assert_eq!(first.id, reading_document_id(parent));

        let again = store.ensure_reading(workspace, parent, at_ms(5)).unwrap();
        assert_eq!(again.id, first.id, "a second ensure made a second document");
        assert!(!again.created);
        assert!(
            again.delta.is_empty(),
            "the idempotent path changed nothing"
        );

        // `documents` reads the SQL mirror, so this is also the proof the
        // registration was reindexed.
        let reading = entry(&mut store, workspace, first.id).expect("listed by the store");
        assert_eq!(reading.kind, READING);
        assert_eq!(reading.parent, Some(parent));
        assert_eq!(reading.title, "", "its title is shown nowhere");
        let sibling = entry(&mut store, workspace, earlier_child).unwrap();
        assert!(
            reading.ordinal > sibling.ordinal,
            "registered at the end of its parent's children"
        );
        assert_eq!(
            keys(&mut store, workspace, first.id),
            Vec::<String>::new(),
            "registered empty"
        );
    }
    let readings = store
        .documents(workspace)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.kind == READING)
        .count();
    assert_eq!(readings, 2, "one per parent, and no third");
}

/// Only a file or a page takes one: the parent's kind is the one token the
/// store interprets here. An unknown parent is not found.
#[test]
fn ensure_reading_refuses_anything_but_a_file_or_a_page() {
    let mut store = memory_store();
    let workspace = workspace(&mut store);
    let file = a_file(&mut store, workspace, "Scan.pdf");
    let note = store.ensure_file_note(workspace, file, at()).unwrap().id;
    let reading = store.ensure_reading(workspace, file, at()).unwrap().id;
    let (canvas, _) = store
        .create_document(workspace, "Board", "canvas", None, None, at())
        .unwrap();

    for refused in [note, reading, canvas] {
        let error = store.ensure_reading(workspace, refused, at()).unwrap_err();
        assert_eq!(error.code(), Code::InvalidRequest);
    }
    let error = store
        .ensure_reading(workspace, DocumentId::new(), at())
        .unwrap_err();
    assert_eq!(error.code(), Code::DocumentNotFound);
}

/// **A backlog registers many reading documents in one manifest commit**
/// (TECHNICAL §9 "Registering during a backlog"). Three files and a page take
/// one each through one journal entry of the manifest; a note and a parent
/// nobody holds are refused without holding the others up; a parent asked for
/// twice counts once; and asking again registers nothing.
#[test]
fn ensure_readings_registers_a_batch_in_one_manifest_commit() {
    let mut store = memory_store();
    let workspace = workspace(&mut store);
    let files: Vec<DocumentId> = (0..3)
        .map(|i| a_file(&mut store, workspace, &format!("Scan {i}.pdf")))
        .collect();
    let page = a_page(&mut store, workspace, None);
    let note = store
        .ensure_file_note(workspace, files[0], at())
        .unwrap()
        .id;
    let already = store.ensure_reading(workspace, files[2], at()).unwrap().id;
    let manifest = store.manifest_id(workspace).unwrap();
    let before = store.document_report(workspace, manifest).unwrap();

    let mut asked = files.clone();
    asked.extend([page, note, DocumentId::new(), files[0]]);
    let batch = store.ensure_readings(workspace, &asked, at_ms(1)).unwrap();

    let after = store.document_report(workspace, manifest).unwrap();
    assert_eq!(
        after.pending_updates + after.checkpoints as u64,
        before.pending_updates + before.checkpoints as u64 + 1,
        "three registrations were one manifest commit"
    );
    assert!(!batch.delta.is_empty());
    assert_eq!(batch.created.len(), 3, "files[2] already had one");
    assert!(!batch.created.contains(&already));
    assert_eq!(batch.refused.len(), 2, "the note and the unknown parent");
    assert!(batch.refused.contains(&note));
    let expected: Vec<(DocumentId, DocumentId)> = files
        .iter()
        .chain([&page])
        .map(|parent| (*parent, reading_document_id(*parent)))
        .collect();
    assert_eq!(batch.readings, expected, "each once, in the order asked");
    for (parent, reading) in &batch.readings {
        let listed = entry(&mut store, workspace, *reading).expect("listed");
        assert_eq!(listed.kind, READING);
        assert_eq!(listed.parent, Some(*parent));
        assert_eq!(listed.title, "");
        assert_eq!(keys(&mut store, workspace, *reading), Vec::<String>::new());
    }

    let again = store.ensure_readings(workspace, &asked, at_ms(2)).unwrap();
    assert!(again.created.is_empty() && again.delta.is_empty());
    assert_eq!(again.readings, expected);
}

/// **Bytes landing are heard, once, and a kind is read back without opening
/// anything** — the two seams the daemon's coordinator and readings mirror
/// are told through (OCR ROADMAP §3, TECHNICAL §7).
#[test]
fn a_blob_that_lands_is_heard_once_and_a_listed_kind_reads_back() {
    let mut store = memory_store();
    let workspace = workspace(&mut store);
    let file = a_file(&mut store, workspace, "Scan.pdf");
    let reading = store.ensure_reading(workspace, file, at()).unwrap().id;
    let hash = koru_store_core::model::BlobHash::parse(&format!("b3:{}", "ab".repeat(32)))
        .expect("a well-formed hash");
    assert!(store.take_blobs_stored().is_empty());

    for _ in 0..2 {
        store
            .finish_blob_upload(
                workspace,
                koru_store_core::ports::CompletedBlobUpload {
                    hash: hash.clone(),
                    size: 7,
                },
                "application/pdf",
                None,
                at(),
                None,
            )
            .unwrap();
    }
    assert_eq!(
        store.take_blobs_stored(),
        vec![koru_store_core::BlobStored {
            workspace,
            hash: hash.clone(),
        }],
        "the same bytes landing twice is one wake"
    );
    assert!(store.take_blobs_stored().is_empty(), "drained");

    assert_eq!(
        store.document_kind(workspace, reading).as_deref(),
        Some(READING)
    );
    assert_eq!(
        store.document_kind(workspace, file).as_deref(),
        Some("file")
    );
    assert_eq!(store.document_kind(workspace, DocumentId::new()), None);
    assert_eq!(store.document_kind(WorkspaceId::new(), file), None);
}

/// **Two devices that each registered one while apart hold one after they
/// sync** (TECHNICAL §7). Both derived the same id, so both wrote into the
/// same document; the manifest held two tree nodes for it, and
/// `Manifest::reindex` keeps one on both sides. What each wrote before the
/// sync is in the one document afterwards.
#[test]
fn two_stores_that_each_ensured_one_while_apart_hold_one_document_after_syncing() {
    let mut a = memory_store();
    let workspace = workspace(&mut a);
    let file = a_file(&mut a, workspace, "Scan.pdf");
    let record = a.workspace(workspace).unwrap();
    let profile = a.profile(record.owner).unwrap();
    let manifest = a.manifest_id(workspace).unwrap();

    // Device B receives the workspace as it stands: the file, no reading yet.
    let mut b = memory_store();
    b.provision_replica(&profile, &record, manifest, at())
        .unwrap();
    sync_one(&mut a, &mut b, workspace, manifest);
    assert!(entry(&mut b, workspace, file).is_some());

    // Apart: each registers the file's reading document and writes into it.
    let on_a = a.ensure_reading(workspace, file, at()).unwrap();
    let on_b = b.ensure_reading(workspace, file, at()).unwrap();
    assert!(on_a.created && on_b.created, "each device registered one");
    assert_eq!(on_a.id, on_b.id, "and they are the same document");
    let reading = on_a.id;
    write(&mut a, workspace, reading, "page 1 from a");
    write(&mut b, workspace, reading, "page 2 from b");

    // They meet: the manifest both ways, then the document both ways.
    sync_one(&mut a, &mut b, workspace, manifest);
    sync_one(&mut b, &mut a, workspace, manifest);
    sync_one(&mut a, &mut b, workspace, reading);
    sync_one(&mut b, &mut a, workspace, reading);

    for store in [&mut a, &mut b] {
        let rows: Vec<_> = store
            .documents(workspace)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.id == reading)
            .collect();
        assert_eq!(rows.len(), 1, "one document, one row");
        assert_eq!(rows[0].parent, Some(file));
        assert_eq!(rows[0].kind, READING);
        assert_eq!(
            keys(store, workspace, reading),
            vec!["page 1 from a".to_owned(), "page 2 from b".to_owned()],
            "nothing either device wrote was orphaned"
        );
        // And a third ensure on either side is the idempotent path.
        let again = store.ensure_reading(workspace, file, at()).unwrap();
        assert!(!again.created && again.delta.is_empty());
    }
    assert_eq!(
        a.version(workspace, manifest).unwrap(),
        b.version(workspace, manifest).unwrap(),
        "the manifests converged"
    );
}

// ---------------------------------------------------------------- lifecycle

/// **Trash, restore and purge take the reading document with its parent**,
/// by inheritance and by the subtree, with no lifecycle code of its own. The
/// trash is the Store's own operation; restore and purge are manifest edits a
/// client makes and sends, as the desktop does.
#[test]
fn trashing_restoring_and_purging_the_parent_take_the_reading_document_with_it() {
    let mut store = memory_store();
    let workspace = workspace(&mut store);
    let file = a_file(&mut store, workspace, "Scan.pdf");
    let reading = store.ensure_reading(workspace, file, at()).unwrap().id;
    let manifest_id = store.manifest_id(workspace).unwrap();

    store
        .trash_document_entry(workspace, file, at(), None)
        .unwrap();
    let manifest = live_manifest(&mut store, workspace);
    assert!(manifest.is_trashed(reading), "binned with its parent");
    let (live, trashed) = manifest.partitioned_entries();
    assert!(live.iter().all(|entry| entry.id != reading));
    assert!(trashed.iter().any(|entry| entry.id == reading));

    // Restore, as a client does it: a manifest edit sent to the Store.
    let mut client = live_manifest(&mut store, workspace);
    let before = client.version();
    client.restore_document(file).unwrap();
    store
        .apply_update(
            workspace,
            manifest_id,
            client.export_updates_since(&before).unwrap(),
            at(),
            UpdateOrigin::Store,
        )
        .unwrap();
    store.flush(workspace, manifest_id, at()).unwrap();
    let manifest = live_manifest(&mut store, workspace);
    assert!(!manifest.is_trashed(reading), "restored with its parent");
    assert_eq!(
        entry(&mut store, workspace, reading).map(|entry| entry.parent),
        Some(Some(file))
    );

    // Purge — *Delete permanently* — removes the subtree.
    let mut client = live_manifest(&mut store, workspace);
    let before = client.version();
    let removed = client.remove_document(file).unwrap();
    assert!(removed.contains(&reading), "the purge named it");
    store
        .apply_update(
            workspace,
            manifest_id,
            client.export_updates_since(&before).unwrap(),
            at(),
            UpdateOrigin::Store,
        )
        .unwrap();
    store.flush(workspace, manifest_id, at()).unwrap();
    assert!(entry(&mut store, workspace, reading).is_none());
    assert!(entry(&mut store, workspace, file).is_none());
}

/// **A move to another workspace carries the reading document**, and its id
/// is still the one derived from its parent — the transfer keeps ids, so no
/// link needed carrying. Asking again in the target is the idempotent path.
#[test]
fn a_move_to_another_workspace_carries_the_reading_document_under_its_derived_id() {
    let mut store = memory_store();
    let source = workspace(&mut store);
    let owner = store.workspace(source).unwrap().owner;
    let target = store.create_workspace(owner, "target", at()).unwrap().id;
    let file = a_file(&mut store, source, "Scan.pdf");
    let reading = store.ensure_reading(source, file, at()).unwrap().id;
    write(&mut store, source, reading, "page 1");

    let (moved, _, _) = store
        .transfer_documents(source, target, file, None, 0, at())
        .unwrap();
    assert!(moved.contains(&reading));

    let carried = entry(&mut store, target, reading).expect("carried with its parent");
    assert_eq!(carried.parent, Some(file));
    assert_eq!(carried.kind, READING);
    assert_eq!(carried.id, reading_document_id(file));
    assert_eq!(keys(&mut store, target, reading), vec!["page 1".to_owned()]);
    assert!(entry(&mut store, source, reading).is_none());
    assert_eq!(store.locate(reading).unwrap(), Some(target));

    let again = store.ensure_reading(target, file, at()).unwrap();
    assert_eq!(again.id, reading);
    assert!(!again.created && again.delta.is_empty());
}

/// **A file with a note, moved to another workspace, keeps its note** — a fix
/// to shipped behaviour (the maintainer, 2026-11-11: "It should go with, we
/// need to make sure it does"). The note document always travelled, as the
/// file's child; the transfer re-created the file's entry from its id, title,
/// kind and type and dropped `note_doc`, so the moved file no longer pointed
/// at its note and was offered *Add note* again. It now carries the link, and
/// `ensure_file_note` in the target hands back the same note.
#[test]
fn a_file_moved_to_another_workspace_keeps_its_note() {
    let mut store = memory_store();
    let source = workspace(&mut store);
    let owner = store.workspace(source).unwrap().owner;
    let target = store.create_workspace(owner, "target", at()).unwrap().id;
    let file = a_file(&mut store, source, "Scan.pdf");
    let note = store.ensure_file_note(source, file, at()).unwrap().id;

    store
        .transfer_documents(source, target, file, None, 0, at())
        .unwrap();

    let moved = entry(&mut store, target, file).expect("the file moved");
    assert_eq!(
        moved.note,
        Some(note),
        "the moved file still points at its note"
    );
    assert_eq!(
        live_manifest(&mut store, target).note_of(file).unwrap(),
        Some(note),
        "in the manifest that syncs, not only in the mirror"
    );
    let opened = store.ensure_file_note(target, file, at()).unwrap();
    assert_eq!(opened.id, note, "it opens the same note");
    assert!(!opened.created, "and is not offered a second one");
    assert_eq!(
        store
            .documents(target)
            .unwrap()
            .iter()
            .filter(|entry| entry.kind == "note")
            .count(),
        1
    );
}

// ------------------------------------------------------------------ history

/// **Fifty updates and a checkpoint leave `document_versions` unchanged** for
/// a kind the host named unversioned (TECHNICAL D14). The checkpoint itself is
/// still taken — the journal is bounded as for any document — and a page
/// beside it, edited the same way, still earns its history row, so the
/// assertion is not vacuous.
#[test]
fn fifty_updates_and_a_checkpoint_record_no_history_for_a_reading_document() {
    let mut store = memory_store().with_unversioned_kinds([READING]);
    store.checkpoints = CheckpointPolicy {
        every_updates: 10,
        every_bytes: u64::MAX,
    };
    let workspace = workspace(&mut store);
    let file = a_file(&mut store, workspace, "Scan.pdf");
    let reading = store.ensure_reading(workspace, file, at()).unwrap().id;
    let page = a_page(&mut store, workspace, None);

    for i in 0..50 {
        write(&mut store, workspace, reading, &format!("page {i}"));
        write(&mut store, workspace, page, &format!("line {i}"));
    }

    let report = store.document_report(workspace, reading).unwrap();
    assert!(
        report.checkpoints > 0,
        "the reading document was checkpointed"
    );
    assert_eq!(
        store
            .history_entries(workspace, reading, None, 100)
            .unwrap()
            .len(),
        0,
        "and its checkpoints recorded no version"
    );
    assert!(
        !store
            .history_entries(workspace, page, None, 100)
            .unwrap()
            .is_empty(),
        "the page beside it was versioned as before"
    );
}

/// A Store told nothing versions every document, as it always did: the set is
/// the host's to name, never the store's to assume.
#[test]
fn a_store_told_no_unversioned_kinds_versions_a_reading_document() {
    let mut store = memory_store();
    store.checkpoints = CheckpointPolicy {
        every_updates: 10,
        every_bytes: u64::MAX,
    };
    let workspace = workspace(&mut store);
    let file = a_file(&mut store, workspace, "Scan.pdf");
    let reading = store.ensure_reading(workspace, file, at()).unwrap().id;
    for i in 0..20 {
        write(&mut store, workspace, reading, &format!("page {i}"));
    }
    assert!(
        !store
            .history_entries(workspace, reading, None, 100)
            .unwrap()
            .is_empty()
    );
}

// ------------------------------------------------------------------- counts

/// **Cloud sync's progress counts reading documents, and must** (TECHNICAL
/// §12). Its required figure is `documents(workspace)` plus the manifest, and
/// its synced figure walks the same ids against `document_positions`; a kind
/// missing from either would hold a workspace at *Syncing* for ever. Both
/// carry a reading document, kind-blind, as they carry a note.
#[test]
fn a_reading_document_is_in_both_halves_of_the_cloud_sync_count() {
    let mut store = memory_store();
    let workspace = workspace(&mut store);
    let file = a_file(&mut store, workspace, "Scan.pdf");
    let reading = store.ensure_reading(workspace, file, at()).unwrap().id;

    assert!(
        store
            .documents(workspace)
            .unwrap()
            .iter()
            .any(|entry| entry.id == reading),
        "in the membership the required figure counts"
    );
    assert!(
        store
            .document_positions(workspace)
            .unwrap()
            .iter()
            .any(|(id, _)| *id == reading),
        "and has the durable position the synced figure compares"
    );
}
