// SPDX-License-Identifier: MPL-2.0

//! The registry without a store: what exists before anything is written,
//! what the first write leaves on disk, what a failed registration keeps,
//! and what a restart finds. The store's half — the seed, the join, the
//! release on acknowledgement — needs a store and is acceptance-tested.

use std::time::{Duration, Instant};

use koru_core::ids::{ListId, PageKey, WorkspaceId};
use koru_core::schema::{WorkspaceDoc, genesis_root_id, genesis_seed_id};

use super::*;
use crate::store::revisions::{DiskFs, discover};

/// A workspace that believes it is open on a store at `root`, with no store
/// there: a registration fails at once, which is the branch these tests are
/// about.
fn workspace_at(root: &std::path::Path, id: WorkspaceId) -> Workspace {
    let mut ws = Workspace::new(WorkspaceDoc::create(id, "test").unwrap());
    ws.current = Some(id);
    ws.store = Some(("test".to_owned(), Some(root.to_path_buf())));
    ws
}

fn settle(ws: &mut Workspace, document: DocumentId) -> PagePhase {
    let started = Instant::now();
    loop {
        ws.tick_pages();
        ws.buffer_pages_now();
        ws.tick_pages();
        let phase = ws.pages.phase(document).expect("the entry stays");
        if phase != PagePhase::Registering || started.elapsed() > Duration::from_secs(20) {
            return phase;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Rung 2: nothing is made until something is written. An empty page gets
/// a genesis document parked under its derived id — no store call, no file
/// on disk, no generation recorded.
#[test]
fn an_empty_page_is_a_genesis_document_parked_under_its_derived_id_and_nothing_else() {
    let root = tempfile::tempdir().unwrap();
    let id = WorkspaceId::new();
    let mut ws = workspace_at(root.path(), id);
    let page = PageKey::List(ListId::new());

    let state = ws.ensure_page(page).expect("a page in an open workspace");
    assert_eq!(state.phase, PagePhase::Empty);
    assert_eq!(state.generation, 0);
    assert_eq!(state.document, page_document_id(id, page, 0));
    let doc = ws.document(state.document).expect("parked under its id");
    assert!(doc.read(genesis_root_id(state.document)).is_ok());
    assert_eq!(doc.own_document_id(), Some(state.document), "stamped");

    // Asked again: the same answer, no second document.
    assert_eq!(ws.ensure_page(page), Some(state.clone()));
    ws.tick_pages();
    ws.buffer_pages_now();
    ws.tick_pages();
    assert_eq!(ws.pages.phase(state.document), Some(PagePhase::Empty));
    assert!(!root.path().join("outbox").exists(), "nothing on disk");
    assert_eq!(ws.unconfirmed().documents, 0);
}

/// Rung 4 and §6.5: the first write puts a revision on disk before anything
/// is sent, and registration starts. With no store it fails, the writing is
/// kept, the guard counts it as saved on this computer, and *Retry* runs it
/// again.
#[test]
fn the_first_write_leaves_a_revision_on_disk_and_a_failed_registration_keeps_it() {
    let root = tempfile::tempdir().unwrap();
    let id = WorkspaceId::new();
    let mut ws = workspace_at(root.path(), id);
    let page = PageKey::List(ListId::new());
    let state = ws.ensure_page(page).unwrap();
    let document = state.document;

    ws.open_document_in(id, document).unwrap();
    assert_eq!(ws.active(), Some(document), "activated like any document");
    ws.ws
        .set_text(genesis_seed_id(document), "Bike project notes")
        .unwrap();

    ws.tick_pages();
    ws.buffer_pages_now();
    ws.tick_pages();
    let phase = ws.pages.phase(document).unwrap();
    assert!(
        matches!(phase, PagePhase::Registering | PagePhase::Failed { .. }),
        "{phase:?}"
    );
    let dir = document_dir(Some(root.path()), id, document).unwrap();
    let found = discover(&DiskFs, &dir).unwrap();
    assert_eq!(found.complete.len(), 1, "one revision, written first");
    let record = &found.complete[0].record;
    assert_eq!(record.page, page.to_storage());
    assert_eq!(record.generation, 0);
    assert_eq!(record.document, document.to_storage());
    assert!(
        record.blobs.is_empty(),
        "images reach the store before their node"
    );

    let phase = settle(&mut ws, document);
    assert!(
        matches!(phase, PagePhase::Failed { opening: false, .. }),
        "no store to register with: {phase:?}"
    );
    let owed = ws.unconfirmed();
    assert_eq!(owed.documents, 1);
    assert_eq!(owed.not_buffered, None, "the revision is on this disk");

    ws.retry_page(page);
    assert_eq!(ws.pages.phase(document), Some(PagePhase::Registering));
    // The revision is still there, and the writing with it.
    assert_eq!(discover(&DiskFs, &dir).unwrap().complete.len(), 1);
    let _ = settle(&mut ws, document);
}

/// A tab letting go of a pending page hands the document back to the
/// registry rather than dropping it; a fresh process finds the revision and
/// takes the writing up again (§6.5 discovery).
#[test]
fn a_closed_pending_page_is_held_and_a_restart_finds_its_writing() {
    let root = tempfile::tempdir().unwrap();
    let id = WorkspaceId::new();
    let page = PageKey::List(ListId::new());
    let document = {
        let mut ws = workspace_at(root.path(), id);
        let state = ws.ensure_page(page).unwrap();
        let document = state.document;
        // Written while parked, as a second tab's edit would be.
        ws.others
            .get_mut(&document)
            .unwrap()
            .set_text(genesis_seed_id(document), "kept")
            .unwrap();
        ws.tick_pages();
        ws.buffer_pages_now();
        ws.tick_pages();
        let _ = settle(&mut ws, document);

        assert!(ws.close_document(document), "handed back, not dropped");
        assert!(ws.document(document).is_none());
        assert!(ws.pages.is_pending(document));
        assert_eq!(ws.unconfirmed().documents, 1);
        document
    };

    let mut fresh = workspace_at(root.path(), id);
    fresh.discover_pages();
    assert_eq!(fresh.pages.phase(document), Some(PagePhase::Registering));
    let held = fresh
        .pages
        .get_held(document)
        .expect("the writing came back off the disk");
    assert_eq!(
        held.read(genesis_seed_id(document))
            .unwrap()
            .content
            .as_text(),
        Some("kept")
    );
    assert_eq!(fresh.unconfirmed().documents, 1);

    // The page asks for it: the held document is parked for the tab.
    let state = fresh.ensure_page(page).unwrap();
    assert_eq!(state.document, document);
    assert!(fresh.document(document).is_some());
}

/// A directory without a record is kept and counted, never consumed.
#[test]
fn discovery_keeps_and_counts_what_it_cannot_consume() {
    let root = tempfile::tempdir().unwrap();
    let id = WorkspaceId::new();
    let document = DocumentId::new();
    let dir = document_dir(Some(root.path()), id, document).unwrap();
    std::fs::create_dir_all(dir.join("00000000000000aa.1")).unwrap();
    std::fs::write(dir.join("00000000000000aa.1").join("updates.loro"), b"x").unwrap();

    let mut ws = workspace_at(root.path(), id);
    ws.discover_pages();
    assert_eq!(ws.unconfirmed().documents, 1, "owed, and said so");
    assert!(ws.pages.get_held(document).is_none(), "nothing consumable");
    assert!(dir.join("00000000000000aa.1").exists(), "kept");
}

/// Rung 3 (P2): a bucket's page is per workspace — Today in workspace A and
/// Today in workspace B are two documents — Completed has its own, and the
/// first input records generation 0 in the system document, creating it
/// when the workspace has none.
#[test]
fn a_buckets_page_is_per_workspace_and_the_first_input_records_its_generation() {
    use crate::system::SystemStore;
    use crate::system::tests::FakeStore;
    use koru_core::tasks::Bucket;
    use std::sync::Arc;

    let root = tempfile::tempdir().unwrap();
    let (a_id, b_id) = (WorkspaceId::new(), WorkspaceId::new());
    let mut a = workspace_at(root.path(), a_id);
    let mut b = workspace_at(root.path(), b_id);
    a.system_store_for_tests = Some(Arc::new(FakeStore::new([])) as Arc<dyn SystemStore>);
    b.system_store_for_tests = Some(Arc::new(FakeStore::new([])) as Arc<dyn SystemStore>);
    let today = PageKey::Bucket(Bucket::Today);
    let completed = PageKey::Bucket(Bucket::Completed);

    let a_today = a.ensure_page(today).unwrap();
    let b_today = b.ensure_page(today).unwrap();
    let a_completed = a.ensure_page(completed).unwrap();
    assert_ne!(a_today.document, b_today.document, "per workspace");
    assert_ne!(
        a_today.document, a_completed.document,
        "Completed has its own"
    );
    assert_eq!(a_today.document, page_document_id(a_id, today, 0));
    assert_eq!(b_today.document, page_document_id(b_id, today, 0));
    assert!(a.document(a_today.document).is_some() && b.document(b_today.document).is_some());
    assert!(
        a.document(b_today.document).is_none(),
        "B's note is not in A"
    );
    assert!(
        a.system.is_none(),
        "nothing written: no system document made"
    );

    a.others
        .get_mut(&a_today.document)
        .unwrap()
        .set_text(genesis_seed_id(a_today.document), "today's note")
        .unwrap();
    a.tick_pages();
    a.buffer_pages_now();
    a.tick_pages();
    let gens = a
        .system
        .as_ref()
        .expect("the first input makes the system document")
        .page_generations(today);
    assert_eq!(gens, Some(vec![0]), "generation 0 recorded");
    assert_eq!(a.ensure_page(today).unwrap().document, a_today.document);
    assert!(b.system.is_none(), "B wrote nothing and records nothing");
    let _ = settle(&mut a, a_today.document);
}

/// Rung 9 (P5): a note's title is the list's name, resolved — never its
/// root text; renaming the note directly gives it its own name; renaming
/// the list takes the title back.
#[test]
fn a_notes_title_is_resolved_from_the_list_and_a_direct_rename_overrides_it() {
    use crate::system::SystemStore;
    use crate::system::tests::FakeStore;
    use koru_core::system::PageTitle;
    use std::sync::Arc;

    let root = tempfile::tempdir().unwrap();
    let id = WorkspaceId::new();
    let mut ws = workspace_at(root.path(), id);
    ws.system_store_for_tests = Some(Arc::new(FakeStore::new([])) as Arc<dyn SystemStore>);
    let list = ws.create_list("Bike", None).expect("a list");
    let page = PageKey::List(list);
    let state = ws.ensure_page(page).unwrap();
    let document = state.document;

    assert_eq!(
        ws.page_title_of(document),
        Some(PageTitle::Text("Bike".into())),
        "named for its list from the start"
    );
    ws.others
        .get_mut(&document)
        .unwrap()
        .set_text(genesis_seed_id(document), "notes")
        .unwrap();
    ws.tick_pages();
    ws.buffer_pages_now();
    ws.tick_pages();
    assert_eq!(
        ws.page_title_of(document),
        Some(PageTitle::Text("Bike".into()))
    );
    assert_eq!(ws.page_of_document(document), Some(page));

    assert!(ws.rename_page_document(document, "Old research"));
    assert_eq!(
        ws.page_title_of(document),
        Some(PageTitle::Text("Old research".into()))
    );
    assert!(ws.rename_list(list, "Bike Build"));
    assert_eq!(
        ws.page_title_of(document),
        Some(PageTitle::Text("Bike Build".into())),
        "the list rename takes the title back"
    );
    assert!(
        !ws.rename_page_document(DocumentId::new(), "x"),
        "nobody's note"
    );
    let _ = settle(&mut ws, document);
}

/// Rung 14 (P5): permanently deleting a page document mints a tombstone
/// (its body stays retained), a device that wrote concurrently resolves to
/// a conflict, *Keep deleted* closes it, and *Recover* ends it — all at the
/// workspace layer, over a fake system store.
#[test]
fn a_permanent_delete_is_a_recoverable_tombstone_and_a_concurrent_write_is_a_conflict() {
    use crate::system::SystemStore;
    use crate::system::tests::FakeStore;
    use koru_core::pages::deletion::Resolution;
    use std::sync::Arc;

    let root = tempfile::tempdir().unwrap();
    let id = WorkspaceId::new();
    let mut ws = workspace_at(root.path(), id);
    ws.system_store_for_tests = Some(Arc::new(FakeStore::new([])) as Arc<dyn SystemStore>);
    let list = ws.create_list("Bike", None).expect("a list");
    let page = PageKey::List(list);

    // Write something, so the page has a document generation 0.
    let state = ws.ensure_page(page).unwrap();
    let document = state.document;
    ws.others
        .get_mut(&document)
        .unwrap()
        .set_text(genesis_seed_id(document), "notes")
        .unwrap();
    ws.tick_pages();
    ws.buffer_pages_now();
    ws.tick_pages();
    assert_eq!(ws.page_resolution(page), Resolution::Live);

    // The device holds a copy, so the deletion's coverage includes its
    // version: deleting is a plain tombstone, no conflict.
    let covers = ws.document(document).unwrap().version();
    assert!(ws.system.as_mut().unwrap().write_deletion(
        page,
        0,
        koru_core::pages::deletion::EventId::new(),
        &covers,
        1_000,
    ));
    assert_eq!(ws.page_resolution(page), Resolution::Deleted);

    // A peer wrote past that coverage: now a conflict, shown on this device.
    let generation = 0;
    let mut peer_vv = ws.document(document).unwrap().version();
    peer_vv.insert(0xBEEF, 7); // a peer counter this device has never seen
    let event = ws
        .system
        .as_ref()
        .unwrap()
        .deletion_record(page, generation)
        .unwrap()
        .events()
        .next()
        .unwrap();
    ws.system
        .as_mut()
        .unwrap()
        .report_uncovered(page, generation, 0xBEEF, event, &peer_vv);
    assert!(matches!(ws.page_resolution(page), Resolution::Conflict(_)));

    // Keep deleted closes it.
    assert!(ws.keep_page_deleted(page));
    assert_eq!(ws.page_resolution(page), Resolution::Deleted);

    // Recover ends it, and the body was never destroyed, so the page can be
    // shown again.
    assert!(ws.recover_page(page));
    assert_eq!(ws.page_resolution(page), Resolution::Live);
    let _ = settle(&mut ws, document);
}

/// The `+` menu's two items, through the workspace: a page shows no note
/// until it has a document or is asked to, and its attachments are
/// references the page state carries in attached order.
#[test]
fn a_pages_note_and_attachments_ride_on_its_state() {
    use crate::system::SystemStore;
    use crate::system::tests::FakeStore;
    use std::sync::Arc;

    let root = tempfile::tempdir().unwrap();
    let id = WorkspaceId::new();
    let mut ws = workspace_at(root.path(), id);
    ws.system_store_for_tests = Some(Arc::new(FakeStore::new([])) as Arc<dyn SystemStore>);
    let list = ws.create_list("Bike", None).expect("a list");
    let page = PageKey::List(list);

    let state = ws.ensure_page(page).unwrap();
    assert!(!state.note_shown, "an empty page shows no band");
    assert!(state.attachments.is_empty());

    assert!(ws.set_page_note_shown(page, true));
    assert!(ws.ensure_page(page).unwrap().note_shown, "asked for");

    let (a, b) = (DocumentId::new(), DocumentId::new());
    assert!(ws.attach_files_to_page(page, &[a, b]));
    assert_eq!(ws.ensure_page(page).unwrap().attachments, vec![a, b]);
    assert!(ws.detach_file_from_page(page, a));
    assert_eq!(ws.ensure_page(page).unwrap().attachments, vec![b]);
}

/// No disk or real store: a parent purge must wait for its page child's
/// deletion event to be durably acknowledged by the system room.
#[test]
fn parent_purge_waits_for_page_tombstone_acknowledgment() {
    let (mut ws, root, page_doc, system_id, store) = deletion_fixture();
    assert!(!ws.purge_document(root).unwrap());
    assert!(
        ws.manifest
            .as_ref()
            .unwrap()
            .contains(explore::store_document(page_doc))
    );
    let link = store.link(system_id);
    let batch = loro_protocol::BatchId((link.sent().len() as u64).to_le_bytes());
    link.acks(batch, loro_protocol::UpdateStatusCode::Ok);
    ws.system.as_mut().unwrap().tick();
    assert!(ws.purge_document(root).unwrap());
    assert!(
        !ws.manifest
            .as_ref()
            .unwrap()
            .contains(explore::store_document(page_doc))
    );
}

#[test]
fn restoring_a_parent_cancels_its_childs_queued_deletion() {
    let (mut ws, root, page_doc, system_id, store) = deletion_fixture();
    assert!(!ws.purge_document(root).unwrap());
    ws.manifest
        .as_mut()
        .unwrap()
        .restore_document(explore::store_document(root))
        .unwrap();
    ws.recover_on_restore(root);
    let link = store.link(system_id);
    link.acks(
        loro_protocol::BatchId((link.sent().len() as u64).to_le_bytes()),
        loro_protocol::UpdateStatusCode::Ok,
    );
    ws.system.as_mut().unwrap().tick();
    ws.tick_pages();
    assert!(
        ws.manifest
            .as_ref()
            .unwrap()
            .contains(explore::store_document(page_doc))
    );
    assert!(ws.pages.pending_purges.is_empty());
}

fn deletion_fixture() -> (
    Workspace,
    DocumentId,
    DocumentId,
    DocumentId,
    std::sync::Arc<crate::system::tests::FakeStore>,
) {
    use crate::system::tests::FakeStore;
    let workspace = WorkspaceId::new();
    let mut ws = Workspace::new(WorkspaceDoc::create(workspace, "fixture").unwrap());
    ws.current = Some(workspace);
    let system_id = DocumentId::new();
    let parent = DocumentId::new();
    let page = PageKey::Bucket(koru_core::tasks::Bucket::Today);
    let page_doc = page_document_id(workspace, page, 0);
    let mut system = koru_core::system::SystemDoc::create().unwrap();
    system.add_page_generation(page, 0).unwrap();
    let store = std::sync::Arc::new(FakeStore::new([(system_id, system)]));
    ws.system_store_for_tests = Some(store.clone());
    let mut manifest = koru_store_core::manifest::Manifest::create(
        explore::store_workspace(workspace),
        "fixture",
        koru_store_core::model::Timestamp::from_millis(1),
    )
    .unwrap();
    manifest
        .add_document(
            explore::store_document(system_id),
            "",
            koru_core::explore::KIND_SYSTEM,
            None,
        )
        .unwrap();
    manifest
        .add_document(
            explore::store_document(parent),
            "parent",
            koru_core::explore::KIND_DOCUMENT,
            None,
        )
        .unwrap();
    manifest
        .add_document(
            explore::store_document(page_doc),
            "note",
            koru_core::explore::KIND_DOCUMENT,
            Some(explore::store_document(parent)),
        )
        .unwrap();
    manifest
        .trash_document(
            explore::store_document(parent),
            koru_store_core::model::Timestamp::from_millis(2),
            None,
        )
        .unwrap();
    ws.manifest = Some(manifest);
    ws.ensure_system(false);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !ws.system.as_ref().unwrap().pages_loaded() {
        assert!(Instant::now() < deadline);
        ws.system.as_mut().unwrap().tick();
        std::thread::yield_now();
    }
    (ws, parent, page_doc, system_id, store)
}

#[test]
fn a_revision_reply_is_required_before_registration_can_start() {
    let workspace = WorkspaceId::new();
    let mut ws = Workspace::new(WorkspaceDoc::create(workspace, "fixture").unwrap());
    ws.current = Some(workspace);
    let page = PageKey::Bucket(koru_core::tasks::Bucket::Today);
    let state = ws.ensure_page(page).unwrap();
    let id = state.document;
    ws.others
        .get_mut(&id)
        .unwrap()
        .set_text(genesis_seed_id(id), "unsaved")
        .unwrap();
    let (_sender, receiver) = mpsc::channel();
    ws.pages.entries.get_mut(&id).unwrap().revision_write = Some(receiver);
    ws.tick_pages();
    assert_eq!(ws.pages.phase(id), Some(PagePhase::Empty));
    assert!(ws.pages.entries[&id].request.is_none());
    assert!(ws.pages.entries[&id].written.is_none());
}
