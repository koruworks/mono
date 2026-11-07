// SPDX-License-Identifier: MPL-2.0

//! Real Store/SQLite rollback and retry without filesystem or network I/O.
use super::*;
use koru_store_core::{
    Locale, WorkspaceId,
    ports::UpdateOrigin,
    service::{Backend, Store},
};
use loro::{ExportMode, LoroDoc};
use rusqlite::OpenFlags;

pub(super) struct MemoryWorkspace(pub(super) String);
pub(super) fn connect(uri: &str) -> Connection {
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap()
}
impl Backend for MemoryWorkspace {
    type Workspace = SqliteWorkspace;
    type Blobs = crate::blobs::SqliteBlobs;
    fn open_workspace(&self, _: WorkspaceId) -> Result<SqliteWorkspace> {
        SqliteWorkspace::from_connection(connect(&self.0))
    }
    fn create_workspace(&self, id: WorkspaceId) -> Result<SqliteWorkspace> {
        self.open_workspace(id)
    }
    fn delete_workspace(&self, _: WorkspaceId) -> Result<()> {
        panic!("this test never deletes workspaces");
    }
    fn objects(&self) -> Result<Self::Blobs> {
        panic!("this test never opens blobs");
    }
}

#[test]
fn failed_store_flush_retains_exact_cloud_updates_until_database_commit() {
    exercise(false, false);
    exercise(true, false);
}

#[test]
fn failed_store_flush_preserves_older_updates_when_new_edits_arrive() {
    exercise(false, true);
    exercise(true, true);
}

fn exercise(manifest: bool, append_after_failure: bool) {
    let uri = format!(
        "file:flush-retry-{}?mode=memory&cache=shared",
        DocumentId::new()
    );
    let control = connect(&uri);
    let mut store = Store::open(
        crate::SqliteCatalog::in_memory().unwrap(),
        MemoryWorkspace(uri.clone()),
    )
    .unwrap();
    let profile = store
        .create_profile("Owner", None, None, Locale::default(), at())
        .unwrap();
    let workspace = store
        .create_workspace(profile.id, "Workspace", at())
        .unwrap()
        .id;
    let document = if manifest {
        store.manifest_id(workspace).unwrap()
    } else {
        store
            .create_document(workspace, "Document", "page", None, None, at())
            .unwrap()
            .0
    };
    store.flush(workspace, document, at()).unwrap();
    store.take_committed();
    let base = store.snapshot(workspace, document).unwrap();
    let source = LoroDoc::new();
    source.import(&base).unwrap();
    source.set_peer_id(995).unwrap();
    let prior = SqliteWorkspace::from_connection(connect(&uri)).unwrap();
    let previous = prior.document(document).unwrap().unwrap();
    let old_rows = prior.updates_after(document, None).unwrap();
    let old_head = old_rows.last().map(|row| row.seq);
    drop(prior);

    let version = source.oplog_vv();
    source
        .get_text("cloud-retry")
        .insert(0, "retained update")
        .unwrap();
    source.commit();
    let bytes = source.export(ExportMode::updates(&version)).unwrap();
    let origin = UpdateOrigin::Cloud {
        logical_replica_id: [9; 16],
    };
    store
        .apply_update(workspace, document, bytes.clone(), at(), origin.clone())
        .unwrap();
    let due = store.next_due();
    assert!(due.is_some());
    // Reject the position update AFTER the journal insert. SQLite must roll
    // back both, and the live Store must retain the original retry work.
    control
        .execute_batch(
            "CREATE TRIGGER deny_commit BEFORE UPDATE OF version_vector ON loro_documents
         BEGIN SELECT RAISE(ABORT, 'injected commit failure'); END;",
        )
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            store.flush(workspace, document, at()).unwrap_err().code(),
            Code::StorageFailed
        );
        assert!(store.take_committed().is_empty());
        let repository = SqliteWorkspace::from_connection(connect(&uri)).unwrap();
        assert_eq!(repository.updates_after(document, None).unwrap(), old_rows);
        assert_eq!(
            repository
                .document(document)
                .unwrap()
                .unwrap()
                .version_vector,
            previous.version_vector
        );
        // A known-frame retry must not erase the obligation to flush.
        store
            .apply_update(workspace, document, bytes.clone(), at(), origin.clone())
            .unwrap();
        assert_eq!(store.next_due(), due, "failed flush lost its retry window");
    }

    let mut expected = vec![(bytes.clone(), origin.clone())];
    if append_after_failure {
        let version = source.oplog_vv();
        source
            .get_text("cloud-retry")
            .insert(0, "later edit: ")
            .unwrap();
        source.commit();
        let later = source.export(ExportMode::updates(&version)).unwrap();
        store
            .apply_update(
                workspace,
                document,
                later.clone(),
                at(),
                UpdateOrigin::Store,
            )
            .unwrap();
        expected.push((later, UpdateOrigin::Store));
        assert_eq!(
            store.next_due(),
            due,
            "new edit reset the original deadline"
        );
    }
    control.execute_batch("DROP TRIGGER deny_commit").unwrap();
    assert!(store.flush(workspace, document, at()).unwrap().is_some());
    let committed = store.take_committed();
    assert!(
        committed
            .iter()
            .any(|event| event.workspace == workspace && event.document == document)
    );

    // A new repository has only committed SQL, not the Store's live Loro cache.
    let repository = SqliteWorkspace::from_connection(connect(&uri)).unwrap();
    let rows = repository.updates_after(document, old_head).unwrap();
    assert_eq!(rows.len(), expected.len());
    let recovered = LoroDoc::new();
    recovered.import(&base).unwrap();
    for (row, (bytes, origin)) in rows.iter().zip(&expected) {
        assert_eq!(&row.bytes, bytes);
        assert_eq!(&row.origin, origin);
        assert!(recovered.import(&row.bytes).unwrap().pending.is_none());
    }
    assert_eq!(recovered.get_deep_value(), source.get_deep_value());
    assert_eq!(recovered.oplog_vv(), source.oplog_vv());
    assert_eq!(
        loro::VersionVector::decode(
            &repository
                .document(document)
                .unwrap()
                .unwrap()
                .version_vector
        )
        .unwrap(),
        source.oplog_vv()
    );
    let count = repository.update_count(document).unwrap();
    store
        .apply_update(workspace, document, bytes, at(), origin)
        .unwrap();
    assert_eq!(store.flush(workspace, document, at()).unwrap(), None);
    assert_eq!(repository.update_count(document).unwrap(), count);
    assert!(store.take_committed().is_empty());
}
