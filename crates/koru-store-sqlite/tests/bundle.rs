// SPDX-License-Identifier: MPL-2.0

//! **Export and import, against real storage.**
//!
//! The case worth the most care is the difference between the two import
//! modes, because getting either wrong is bad in a way that takes a while to
//! notice:
//!
//! - a *restore* that minted new ids gives two workspaces that look identical
//!   and can never converge again;
//! - a *duplicate* that kept them gives two replicas of one workspace that will
//!   merge the moment they meet — which is exactly what somebody asking for a
//!   copy did not want.

use koru_store_core::bundle::ImportMode;
use koru_store_core::errors::Code;
use koru_store_core::fixtures::{BIDI_LINE, SAMPLES, UNICODE_FILENAME};
use koru_store_core::ids::{ProfileId, WorkspaceId};
use koru_store_core::model::Timestamp;
use koru_store_core::ports::HistoryReason;
use koru_store_core::service::Store;
use koru_store_core::text::Locale;
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};

type TestStore = Store<SqliteCatalog, SqliteBackend>;

fn at(ms: i64) -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000 + ms)
}

struct Fixture {
    dir: tempfile::TempDir,
    store: TestStore,
    owner: ProfileId,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_in(dir.path());
        let owner = store
            .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at(0))
            .unwrap()
            .id;
        Self { dir, store, owner }
    }

    /// A second, empty store — where a bundle would be carried to.
    fn elsewhere() -> (tempfile::TempDir, TestStore, ProfileId) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_in(dir.path());
        let owner = store
            .create_profile("Elsewhere", None, None, Locale::parse("ja").unwrap(), at(0))
            .unwrap()
            .id;
        (dir, store, owner)
    }

    fn a_workspace(&mut self, titles: &[&str]) -> WorkspaceId {
        let id = self
            .store
            .create_workspace(self.owner, "Planejamento", at(0))
            .unwrap()
            .id;
        for title in titles {
            self.store
                .create_document(id, title, "page", None, None, at(0))
                .unwrap();
        }
        id
    }
}

fn open_in(root: &std::path::Path) -> TestStore {
    let catalog = SqliteCatalog::open(&root.join("catalog.db")).unwrap();
    Store::open(catalog, SqliteBackend::new(root)).unwrap()
}

fn titles(store: &mut TestStore, ws: WorkspaceId) -> Vec<String> {
    store
        .documents(ws)
        .unwrap()
        .into_iter()
        .map(|e| e.title)
        .collect()
}

// ------------------------------------------------------------------ export

#[test]
fn an_export_carries_every_document_and_its_checksum() {
    let mut f = Fixture::new();
    let ws = f.a_workspace(&["One", "Two"]);
    let bundle = f.store.export_workspace(ws, at(100)).unwrap();

    bundle.validate().unwrap();
    assert_eq!(bundle.manifest.workspace, ws);
    assert_eq!(bundle.manifest.name, "Planejamento");
    assert_eq!(bundle.manifest.exported_at, at(100));
    // One manifest document — the workspace's own — and nothing else, because
    // documents registered in the manifest have no separate storage until
    // something writes to them.
    assert!(bundle.manifest_document().is_some());
    assert!(!bundle.manifest.documents.is_empty());
}

#[test]
fn an_export_carries_blobs_with_their_metadata() {
    let mut f = Fixture::new();
    let ws = f.a_workspace(&["One"]);
    let blob = f
        .store
        .put_blob(
            ws,
            "四半期計画".as_bytes(),
            "text/plain",
            Some(UNICODE_FILENAME),
            at(0),
            None,
        )
        .unwrap();

    let bundle = f.store.export_workspace(ws, at(100)).unwrap();
    assert_eq!(bundle.manifest.blobs.len(), 1);
    assert_eq!(bundle.manifest.blobs[0].hash, blob.hash);
    assert_eq!(
        bundle.manifest.blobs[0].filename.as_deref(),
        Some(UNICODE_FILENAME)
    );
    assert_eq!(bundle.blobs[0].1, "四半期計画".as_bytes());
}

#[test]
fn history_survives_export_and_restore() {
    let mut f = Fixture::new();
    let ws = f.a_workspace(&["One"]);
    let document = f.store.documents(ws).unwrap()[0].id;
    f.store
        .capture_history(
            ws,
            document,
            None,
            HistoryReason::Manual,
            at(50),
            Some("Milestone"),
            Some("Before review"),
            None,
        )
        .unwrap();

    let bundle = f.store.export_workspace(ws, at(100)).unwrap();
    assert_eq!(bundle.manifest.history_versions.len(), 1);
    assert_eq!(bundle.manifest.history_entries.len(), 1);
    assert_eq!(bundle.history_snapshots.len(), 1);

    let (_dir, mut there, owner) = Fixture::elsewhere();
    let restored = there
        .import_workspace(&bundle, ImportMode::Restore, owner, at(200))
        .unwrap();
    let entries = there.history_entries(restored, document, None, 50).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].label.as_deref(), Some("Milestone"));
    assert_eq!(entries[0].message.as_deref(), Some("Before review"));
    assert_eq!(
        there
            .history_version(restored, entries[0].id)
            .unwrap()
            .snapshot,
        bundle.history_snapshots[0].1
    );
}

#[test]
fn exporting_an_unknown_workspace_is_not_found() {
    let mut f = Fixture::new();
    assert_eq!(
        f.store
            .export_workspace(WorkspaceId::new(), at(0))
            .unwrap_err()
            .code(),
        Code::WorkspaceNotFound
    );
}

// ------------------------------------------------------------------ restore

#[test]
fn a_restore_preserves_the_workspace_and_document_ids() {
    // What makes the result *another replica* rather than a lookalike: it must
    // stay able to converge with its origin.
    let mut f = Fixture::new();
    let ws = f.a_workspace(&["One", "Two"]);
    let bundle = f.store.export_workspace(ws, at(100)).unwrap();
    let manifest_before = f.store.manifest_id(ws).unwrap();

    let (_dir, mut there, owner) = Fixture::elsewhere();
    let restored = there
        .import_workspace(&bundle, ImportMode::Restore, owner, at(200))
        .unwrap();

    assert_eq!(restored, ws, "a restore minted a new workspace id");
    assert_eq!(
        there.manifest_id(restored).unwrap(),
        manifest_before,
        "a restore minted a new document id"
    );
    assert_eq!(titles(&mut there, restored), ["One", "Two"]);
}

#[test]
fn a_restore_over_a_workspace_already_here_is_refused() {
    // Merging two histories under one id, with no record of it, is the one
    // outcome that cannot be undone.
    let mut f = Fixture::new();
    let ws = f.a_workspace(&["One"]);
    let bundle = f.store.export_workspace(ws, at(100)).unwrap();

    let err = f
        .store
        .import_workspace(&bundle, ImportMode::Restore, f.owner, at(200))
        .unwrap_err();
    assert_eq!(err.code(), Code::WorkspaceExists);
    assert!(err.message().contains("duplicate"), "{}", err.message());
}

#[test]
fn a_restore_records_its_import_receipt_and_nothing_it_was_derived_from() {
    // It is not derived from anything — it *is* the thing — so the one row
    // names itself: the receipt of the publication that brought this replica
    // here, in the lineage journal rather than a table of its own
    // (root-catalog TECHNICAL.final §8.5).
    let mut f = Fixture::new();
    let ws = f.a_workspace(&["One"]);
    let bundle = f.store.export_workspace(ws, at(100)).unwrap();

    let (_dir, mut there, owner) = Fixture::elsewhere();
    let restored = there
        .import_workspace(&bundle, ImportMode::Restore, owner, at(200))
        .unwrap();
    let lineage = there.lineage(restored).unwrap();
    assert_eq!(lineage.len(), 1);
    assert_eq!(lineage[0].workspace, restored);
    assert_eq!(lineage[0].derived_from, restored);
    assert_eq!(lineage[0].kind.as_str(), "restore");
    assert_eq!(lineage[0].at, at(200));
}

// ------------------------------------------------------------------ duplicate

#[test]
fn a_duplicate_mints_new_ids_and_records_where_it_came_from() {
    let mut f = Fixture::new();
    let ws = f.a_workspace(&["One", "Two"]);
    let bundle = f.store.export_workspace(ws, at(100)).unwrap();
    let manifest_before = f.store.manifest_id(ws).unwrap();

    let copy = f
        .store
        .import_workspace(&bundle, ImportMode::Duplicate, f.owner, at(200))
        .unwrap();

    assert_ne!(copy, ws, "a duplicate kept its origin's workspace id");
    assert_ne!(
        f.store.manifest_id(copy).unwrap(),
        manifest_before,
        "a duplicate kept its origin's document id"
    );
    // The content is the same; only the identities differ.
    assert_eq!(titles(&mut f.store, copy), ["One", "Two"]);
    assert_eq!(titles(&mut f.store, ws), ["One", "Two"]);
    for entry in f.store.documents(copy).unwrap() {
        assert_eq!(
            f.store.locate(entry.id).unwrap(),
            Some(copy),
            "every reminted manifest id resolves inside the duplicate",
        );
        assert!(f.store.snapshot(copy, entry.id).is_ok());
    }

    let lineage = f.store.lineage(copy).unwrap();
    assert_eq!(lineage.len(), 1);
    assert_eq!(lineage[0].derived_from, ws);
    assert_eq!(lineage[0].kind.as_str(), "duplicate");
}

#[test]
fn a_duplicate_can_live_beside_its_origin_in_one_store() {
    // Which is the point of re-minting document ids: two documents with one id
    // in one store would collide in the location index.
    let mut f = Fixture::new();
    let ws = f.a_workspace(&["One"]);
    let bundle = f.store.export_workspace(ws, at(100)).unwrap();

    let a = f
        .store
        .import_workspace(&bundle, ImportMode::Duplicate, f.owner, at(200))
        .unwrap();
    let b = f
        .store
        .import_workspace(&bundle, ImportMode::Duplicate, f.owner, at(300))
        .unwrap();

    assert_ne!(a, b);
    assert_ne!(
        f.store.manifest_id(a).unwrap(),
        f.store.manifest_id(b).unwrap()
    );
    // And each resolves to its own workspace, not to the other's.
    let manifest_a = f.store.manifest_id(a).unwrap();
    assert_eq!(f.store.locate(manifest_a).unwrap(), Some(a));
}

#[test]
fn a_duplicate_still_resolves_its_reminted_documents_after_a_restart() {
    // The manifest is a Loro document too. Its entries must name the reminted
    // storage ids, not the source ids carried by its imported snapshot. The
    // live Store can conceal that disagreement until its open-document cache
    // is dropped, which is why this case deliberately crosses a restart.
    let mut f = Fixture::new();
    let ws = f.a_workspace(&["One", "Two"]);
    let bundle = f.store.export_workspace(ws, at(100)).unwrap();
    let copy = f
        .store
        .import_workspace(&bundle, ImportMode::Duplicate, f.owner, at(200))
        .unwrap();
    let document_ids: Vec<_> = f
        .store
        .documents(copy)
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect();

    let Fixture { dir, store, .. } = f;
    drop(store);
    let mut reopened = open_in(dir.path());

    assert_eq!(titles(&mut reopened, copy), ["One", "Two"]);
    for id in document_ids {
        assert_eq!(reopened.locate(id).unwrap(), Some(copy));
        reopened
            .snapshot(copy, id)
            .expect("a reminted manifest id must resolve after reopening the store");
    }
}

#[test]
fn edits_after_a_duplicate_do_not_reach_the_original() {
    let mut f = Fixture::new();
    let ws = f.a_workspace(&["Shared"]);
    let bundle = f.store.export_workspace(ws, at(100)).unwrap();
    let copy = f
        .store
        .import_workspace(&bundle, ImportMode::Duplicate, f.owner, at(200))
        .unwrap();

    f.store
        .create_document(copy, "Only in the copy", "page", None, None, at(300))
        .unwrap();

    assert_eq!(titles(&mut f.store, copy), ["Shared", "Only in the copy"]);
    assert_eq!(titles(&mut f.store, ws), ["Shared"]);
}

// ------------------------------------------------------------------ carrying

#[test]
fn a_workspace_survives_a_round_trip_through_another_store() {
    let mut f = Fixture::new();
    let titles_in: Vec<&str> = SAMPLES.iter().map(|s| s.title).collect();
    let ws = f.a_workspace(&titles_in);
    f.store
        .create_document(ws, BIDI_LINE, "page", None, None, at(0))
        .unwrap();
    let blob = f
        .store
        .put_blob(
            ws,
            b"bytes",
            "application/pdf",
            Some(UNICODE_FILENAME),
            at(0),
            None,
        )
        .unwrap();

    let bundle = f.store.export_workspace(ws, at(100)).unwrap();

    let (_dir, mut there, owner) = Fixture::elsewhere();
    let restored = there
        .import_workspace(&bundle, ImportMode::Restore, owner, at(200))
        .unwrap();

    let mut expected: Vec<String> = titles_in.iter().map(|t| t.to_string()).collect();
    expected.push(BIDI_LINE.to_string());
    assert_eq!(titles(&mut there, restored), expected);

    // The blob came with it, bytes and metadata both.
    assert_eq!(there.get_blob(restored, &blob.hash).unwrap(), b"bytes");
    assert_eq!(
        there
            .blob(restored, &blob.hash)
            .unwrap()
            .filename
            .as_deref(),
        Some(UNICODE_FILENAME)
    );
}

#[test]
fn an_imported_workspace_survives_a_restart() {
    // The import wrote to disk rather than only to memory.
    let f = Fixture::new();
    let mut here = open_in(f.dir.path());
    let owner = here
        .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at(0))
        .unwrap()
        .id;
    let ws = here.create_workspace(owner, "W", at(0)).unwrap().id;
    here.create_document(ws, "四半期計画", "page", None, None, at(0))
        .unwrap();
    let bundle = here.export_workspace(ws, at(100)).unwrap();

    let (dir, mut there, other) = Fixture::elsewhere();
    let restored = there
        .import_workspace(&bundle, ImportMode::Restore, other, at(200))
        .unwrap();
    drop(there);

    let mut reopened = open_in(dir.path());
    assert_eq!(titles(&mut reopened, restored), ["四半期計画"]);
}

// ------------------------------------------------------------------ refusals

#[test]
fn a_bundle_for_a_missing_profile_is_refused_and_leaves_nothing_behind() {
    let mut f = Fixture::new();
    let ws = f.a_workspace(&["One"]);
    let bundle = f.store.export_workspace(ws, at(100)).unwrap();

    let (_dir, mut there, _owner) = Fixture::elsewhere();
    assert_eq!(
        there
            .import_workspace(&bundle, ImportMode::Restore, ProfileId::new(), at(200))
            .unwrap_err()
            .code(),
        Code::ProfileNotFound
    );
    assert!(there.workspaces().unwrap().is_empty());
}

#[test]
fn a_damaged_bundle_is_refused_before_anything_is_written() {
    let mut f = Fixture::new();
    let ws = f.a_workspace(&["One"]);
    let mut bundle = f.store.export_workspace(ws, at(100)).unwrap();
    bundle.snapshots[0].1 = b"rotted".to_vec();

    let (_dir, mut there, owner) = Fixture::elsewhere();
    assert_eq!(
        there
            .import_workspace(&bundle, ImportMode::Duplicate, owner, at(200))
            .unwrap_err()
            .code(),
        Code::CorruptState
    );
    assert!(
        there.workspaces().unwrap().is_empty(),
        "a half-imported workspace was left behind"
    );
}
