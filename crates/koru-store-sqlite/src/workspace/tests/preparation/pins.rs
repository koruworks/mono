// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::{Manifest, ports::UpdateOrigin, service::ListingState};
use loro::{ExportMode, LoroDoc};

#[test]
fn pin_flushes_queued_cloud_manifest_but_preserves_ordinary_document_windows() {
    for fail_prerequisite in [false, true] {
        let backend = FaultBackend::new(None, false);
        let (catalog, _control) = catalog();
        let mut store = Store::open(catalog, backend.clone()).unwrap();
        let workspace = store
            .create_profile_with_workspace(&request(), at())
            .unwrap()
            .workspace;
        let id = workspace.id;
        let manifest_id = store.manifest_id(id).unwrap();
        let document = store
            .create_document(id, "Retained", "page", None, None, at())
            .unwrap()
            .0;
        store.flush(id, manifest_id, at()).unwrap();
        let manifest = Manifest::from_snapshot(&store.snapshot(id, manifest_id).unwrap()).unwrap();
        manifest.set_peer_id(990).unwrap();
        let before = manifest.version();
        manifest
            .set_appearance(Some("fox"), Some("green"), false, false)
            .unwrap();
        let cloud = UpdateOrigin::Cloud {
            logical_replica_id: [9; 16],
        };
        store
            .apply_update(
                id,
                manifest_id,
                manifest.export_updates_since(&before).unwrap(),
                at(),
                cloud.clone(),
            )
            .unwrap();
        let page = LoroDoc::new();
        page.import(&store.snapshot(id, document).unwrap()).unwrap();
        page.set_peer_id(991).unwrap();
        let before = page.oplog_vv();
        page.get_text("retained")
            .insert(0, "queued ordinary content")
            .unwrap();
        page.commit();
        store
            .apply_update(
                id,
                document,
                page.export(ExportMode::updates(&before)).unwrap(),
                at(),
                cloud.clone(),
            )
            .unwrap();
        store.take_committed();
        let repo = backend.open_workspace(id).unwrap();
        let page_rows = repo.updates_after(document, None).unwrap();
        if fail_prerequisite {
            repo.connection.execute_batch("CREATE TRIGGER deny_commit BEFORE UPDATE OF version_vector ON loro_documents BEGIN SELECT RAISE(ABORT, 'pin prerequisite'); END;").unwrap();
            assert_eq!(
                store
                    .set_workspace_pinned(id, true, at())
                    .unwrap_err()
                    .code(),
                Code::StorageFailed
            );
            assert!(store.take_committed().is_empty());
            assert!(store.next_due().is_some());
            assert!(!store.workspace(id).unwrap().pinned);
            repo.connection
                .execute_batch("DROP TRIGGER deny_commit;")
                .unwrap();
        }
        let result = store.set_workspace_pinned(id, true, at()).unwrap();
        assert_eq!(result.listing, ListingState::Applied);
        assert!(result.workspace.pinned);
        assert_eq!(result.workspace.icon.as_deref(), Some("fox"));
        assert_eq!(result.workspace.color.as_deref(), Some("green"));
        assert!(!result.workspace.archived);
        assert_eq!(
            repo.updates_after(document, None).unwrap(),
            page_rows,
            "pin flushed an unrelated window"
        );
        assert!(store.next_due().is_some());
        let live = LoroDoc::new();
        live.import(&store.snapshot(id, document).unwrap()).unwrap();
        assert_eq!(
            live.get_text("retained").to_string(),
            "queued ordinary content"
        );
        store.flush(id, document, at()).unwrap();
        let durable = koru_store_core::recovery::open_document(&repo, manifest_id)
            .unwrap()
            .into_value();
        let durable = Manifest::from_snapshot(&durable.export_snapshot().unwrap()).unwrap();
        assert_eq!(durable.pinned(), Some(true));
        assert_eq!(durable.icon(), Some("fox".into()));
        assert!(
            repo.updates_after(manifest_id, None)
                .unwrap()
                .iter()
                .any(|update| update.origin == cloud)
        );
    }
}

#[test]
fn failed_pin_commit_restores_only_manifest_and_retains_queued_document() {
    let backend = FaultBackend::new(None, false);
    let (catalog, _control) = catalog();
    let mut store = Store::open(catalog, backend.clone()).unwrap();
    let id = store
        .create_profile_with_workspace(&request(), at())
        .unwrap()
        .workspace
        .id;
    let document = store
        .create_document(id, "Retained", "page", None, None, at())
        .unwrap()
        .0;
    let manifest_id = store.manifest_id(id).unwrap();
    store.flush(id, manifest_id, at()).unwrap();
    let before = Manifest::from_snapshot(&store.snapshot(id, manifest_id).unwrap()).unwrap();
    let source = LoroDoc::new();
    source
        .import(&store.snapshot(id, document).unwrap())
        .unwrap();
    source.set_peer_id(991).unwrap();
    let vv = source.oplog_vv();
    source
        .get_text("retained")
        .insert(0, "do not drop")
        .unwrap();
    source.commit();
    store
        .apply_update(
            id,
            document,
            source.export(ExportMode::updates(&vv)).unwrap(),
            at(),
            UpdateOrigin::Store,
        )
        .unwrap();
    let due = store.next_due();
    let repo = backend.open_workspace(id).unwrap();
    repo.connection.execute_batch("CREATE TRIGGER deny_commit BEFORE UPDATE OF version_vector ON loro_documents BEGIN SELECT RAISE(ABORT, 'pin commit'); END;").unwrap();
    store.take_committed();
    assert_eq!(
        store
            .set_workspace_pinned(id, true, at())
            .unwrap_err()
            .code(),
        Code::StorageFailed
    );
    let after = Manifest::from_snapshot(&store.snapshot(id, manifest_id).unwrap()).unwrap();
    assert_eq!(before.version(), after.version());
    assert_eq!(after.pinned(), Some(false));
    assert_eq!(store.next_due(), due);
    assert!(store.take_committed().is_empty());
    let live = LoroDoc::new();
    live.import(&store.snapshot(id, document).unwrap()).unwrap();
    assert_eq!(live.get_text("retained").to_string(), "do not drop");
    repo.connection
        .execute_batch("DROP TRIGGER deny_commit;")
        .unwrap();
    store.set_workspace_pinned(id, true, at()).unwrap();
    store.flush(id, document, at()).unwrap();
}

#[test]
fn committed_pin_survives_cold_reopen_when_both_projection_attempts_fail() {
    let backend = FaultBackend::new(None, false);
    let name = koru_sqlite::MemoryName::fresh("pin-reopen");
    let control = koru_sqlite::open_memory(&name).unwrap();
    let mut store = Store::open(
        SqliteCatalog::in_named_memory(&name).unwrap(),
        backend.clone(),
    )
    .unwrap();
    let id = store
        .create_profile_with_workspace(&request(), at())
        .unwrap()
        .workspace
        .id;
    control.execute_batch("CREATE TRIGGER deny_pin BEFORE INSERT ON workspaces BEGIN SELECT RAISE(ABORT, 'pin listing'); END;").unwrap();
    let result = store.set_workspace_pinned(id, true, at()).unwrap();
    assert!(result.workspace.pinned);
    assert!(matches!(result.listing, ListingState::Pending { .. }));
    assert!(!store.workspace(id).unwrap().pinned);
    assert!(!store.take_committed().is_empty());
    drop(store);
    let mut store = Store::open(SqliteCatalog::in_named_memory(&name).unwrap(), backend).unwrap();
    assert!(!store.workspace(id).unwrap().pinned);
    control.execute_batch("DROP TRIGGER deny_pin;").unwrap();
    assert!(store.reconcile_workspace_listing(id, at()).unwrap().pinned);
    assert!(store.workspace(id).unwrap().pinned);
}
