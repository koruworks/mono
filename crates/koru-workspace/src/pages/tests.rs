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
