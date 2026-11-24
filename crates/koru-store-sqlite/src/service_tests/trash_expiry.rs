// SPDX-License-Identifier: MPL-2.0

//! The 30-day trash sweep (`koru_replication::cloud::client_store::expiry`)
//! against real storage held in memory: a due document goes with its whole
//! subtree and one purge event each, a younger one stays, bin rows go only
//! when their own stamp is due, and only the trusted time decides.

use super::*;
use koru_core::{Node, WorkspaceDoc, schema::Parent, trash_expiry::DAY_MS};
use koru_replication::cloud::client_store::{expiry, removal};
use koru_store_core::DocumentId;

type MemoryStore = Store<SqliteCatalog, SqliteBackend>;

const T: i64 = 1_770_000_000_000;

fn workspace() -> (MemoryStore, WorkspaceId) {
    let catalog =
        SqliteCatalog::in_named_memory(&koru_sqlite::MemoryName::fresh("catalog.db")).unwrap();
    let mut store = Store::open(catalog, SqliteBackend::in_memory()).unwrap();
    let owner = store
        .create_profile("owner", None, None, Locale::default(), at())
        .unwrap()
        .id;
    let workspace = store.create_workspace(owner, "sweep", at()).unwrap().id;
    (store, workspace)
}

fn page(store: &mut MemoryStore, workspace: WorkspaceId, parent: Option<DocumentId>) -> DocumentId {
    store
        .create_document(workspace, "Page", "page", parent, None, at())
        .unwrap()
        .0
}

fn trash(store: &mut MemoryStore, workspace: WorkspaceId, document: DocumentId, ms: i64) {
    store
        .trash_document_entry(workspace, document, Timestamp::from_millis(ms), None)
        .unwrap();
}

fn present(store: &mut MemoryStore, workspace: WorkspaceId, document: DocumentId) -> bool {
    store
        .documents(workspace)
        .unwrap()
        .iter()
        .any(|entry| entry.id == document)
}

fn sweep(store: &mut MemoryStore, workspace: WorkspaceId, trusted_now: i64) -> expiry::Swept {
    expiry::sweep(
        store,
        workspace,
        trusted_now,
        0x5eed,
        Timestamp::from_millis(trusted_now),
    )
    .unwrap()
}

#[test]
fn a_document_due_goes_with_its_subtree_and_a_younger_one_stays() {
    let (mut store, workspace) = workspace();
    let old = page(&mut store, workspace, None);
    let child = page(&mut store, workspace, Some(old));
    let grandchild = page(&mut store, workspace, Some(child));
    let young = page(&mut store, workspace, None);
    let kept = page(&mut store, workspace, None);
    trash(&mut store, workspace, old, T);
    trash(&mut store, workspace, young, T + 2 * DAY_MS);

    // Thirty days is the promise; the purge waits one more.
    assert_eq!(sweep(&mut store, workspace, T + 30 * DAY_MS).documents, 0);
    assert!(present(&mut store, workspace, old));

    let swept = sweep(&mut store, workspace, T + 31 * DAY_MS);
    assert_eq!(
        swept.documents, 3,
        "the due document and both levels under it"
    );
    for gone in [old, child, grandchild] {
        assert!(!present(&mut store, workspace, gone));
        assert_eq!(store.locate(gone).unwrap(), None);
    }
    assert!(present(&mut store, workspace, young), "29 days in: stays");
    assert!(present(&mut store, workspace, kept), "never trashed: stays");

    // One durable purge event per document, each naming the root removed.
    let system = removal::load(&mut store, workspace).unwrap().unwrap();
    let mut purged: Vec<_> = system
        .content
        .purged_documents()
        .into_iter()
        .map(|id| id.as_uuid())
        .collect();
    purged.sort();
    let mut expected = vec![old.as_uuid(), child.as_uuid(), grandchild.as_uuid()];
    expected.sort();
    assert_eq!(purged, expected);

    assert_eq!(
        sweep(&mut store, workspace, T + 31 * DAY_MS),
        expiry::Swept::default(),
        "a second sweep has nothing left to do"
    );
    assert_eq!(sweep(&mut store, workspace, T + 33 * DAY_MS).documents, 1);
    assert!(!present(&mut store, workspace, young));
}

#[test]
fn only_the_trusted_time_decides() {
    let (mut store, workspace) = workspace();
    let binned = page(&mut store, workspace, None);
    // Stamped by a device whose clock ran a year ahead.
    let ahead = page(&mut store, workspace, None);
    trash(&mut store, workspace, binned, T);
    trash(&mut store, workspace, ahead, T + 365 * DAY_MS);

    // The Cloud vouched for ten days after the first stamp: nothing is due,
    // whatever this device's own clock says.
    assert_eq!(sweep(&mut store, workspace, T + 10 * DAY_MS).documents, 0);
    // Forty days on, only the honest stamp is due; one later than the trusted
    // time never is.
    assert_eq!(sweep(&mut store, workspace, T + 40 * DAY_MS).documents, 1);
    assert!(!present(&mut store, workspace, binned));
    assert!(present(&mut store, workspace, ahead));
}

#[test]
fn bin_rows_go_when_their_own_stamp_is_due_and_nothing_else_is_rewritten() {
    let (mut store, workspace) = workspace();
    let mut doc = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "binned").unwrap();
    let old = doc
        .insert(Parent::Root, Node::paragraph("old").unwrap())
        .unwrap();
    let young = doc
        .insert(Parent::Root, Node::paragraph("young").unwrap())
        .unwrap();
    let live = doc
        .insert(Parent::Root, Node::paragraph("live").unwrap())
        .unwrap();
    doc.trash(old, T, None).unwrap();
    doc.trash(young, T + 5 * DAY_MS, None).unwrap();
    let (binned, _) = store
        .import_document(
            workspace,
            "binned",
            "page",
            None,
            doc.export_snapshot().unwrap(),
            at(),
        )
        .unwrap();
    let untouched = page(&mut store, workspace, None);
    let before = store.version(workspace, untouched).unwrap();

    let swept = sweep(&mut store, workspace, T + 31 * DAY_MS);
    assert_eq!((swept.rows, swept.rewritten), (1, 1));

    let after = WorkspaceDoc::adopt(store.fork_document(workspace, binned).unwrap()).unwrap();
    let left: Vec<_> = after
        .trashed()
        .unwrap()
        .into_iter()
        .map(|row| row.node)
        .collect();
    assert_eq!(left, vec![young], "5 days younger: still in the bin");
    assert!(after.read(old).is_err(), "the due row is gone for good");
    assert!(after.read(live).is_ok(), "a row never binned stays");
    assert_eq!(
        store.version(workspace, untouched).unwrap(),
        before,
        "a document with nothing due is not rewritten"
    );
}
