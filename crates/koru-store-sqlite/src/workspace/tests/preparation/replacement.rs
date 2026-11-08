// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::{
    Manifest, Workspace,
    service::{ListingState, RemovedWorkspace, Replacement, WorkspaceManifestInspection},
};

#[test]
fn profile_cascade_has_no_replacement_and_stale_steps_change_nothing() {
    use koru_store_core::profile_removal::RemovalKind;
    let mut f = Fixture::new();
    let owner = f.source.owner;
    let other = f.store.create_workspace(owner, "Survivor", at()).unwrap();
    let previous_trash = f
        .store
        .create_workspace(owner, "Earlier Trash", at())
        .unwrap();
    f.store.delete_workspace(previous_trash.id, at()).unwrap();
    let mark = f
        .store
        .begin_profile_removal(owner, RemovalKind::Local, at())
        .unwrap();
    f.reopen();
    assert_eq!(f.store.profile_removal(owner).unwrap(), Some(mark.clone()));
    assert!(matches!(
        f.store
            .ensure_live_workspace(owner, "Default", at())
            .unwrap(),
        koru_store_core::service::EnsuredWorkspace::Removing { .. }
    ));
    for result in [
        f.remove(true).map(|_| ()),
        f.remove(false).map(|_| ()),
        f.store.delete_workspace(f.source.id, at()).map(|_| ()),
    ] {
        assert_eq!(result.unwrap_err().code(), Code::ProfileBeingRemoved);
    }
    f.assert_live();
    let result = f
        .store
        .delete_workspace_for_removal(f.source.id, mark.attempt, at())
        .unwrap();
    assert_eq!(result.replacement, Replacement::NotNeeded);
    assert!(matches!(result.listing, ListingState::Applied));
    assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 3);
    f.store.stop_profile_removal(owner, mark.attempt).unwrap();
    assert_eq!(
        f.store
            .delete_workspace_for_removal(other.id, mark.attempt, at())
            .unwrap_err()
            .code(),
        Code::RemovalAttemptStale
    );
    let next = f
        .store
        .begin_profile_removal(owner, RemovalKind::Local, at())
        .unwrap();
    assert_eq!(
        f.store
            .remove_profile_local(owner, Some(mark.attempt), at())
            .unwrap_err()
            .code(),
        Code::RemovalAttemptStale
    );
    f.store.keep_profile(owner, next.attempt).unwrap();
    assert_eq!(
        f.store
            .delete_workspace_for_removal(other.id, next.attempt, at())
            .unwrap_err()
            .code(),
        Code::RemovalAttemptStale
    );
    assert!(f.store.workspace(other.id).is_ok());
    assert_eq!(
        f.store
            .workspace_including_deleted(previous_trash.id)
            .unwrap()
            .deleted_at,
        Some(at())
    );
    assert_eq!(
        f.store
            .workspace_including_deleted(f.source.id)
            .unwrap()
            .deleted_at,
        Some(at())
    );
    assert!(
        matches!(f.store.ensure_live_workspace(owner, "Default", at()).unwrap(), koru_store_core::service::EnsuredWorkspace::Active { workspace, .. } if workspace.id == other.id)
    );
    assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 3);
}

struct Fixture {
    store: Store<SqliteCatalog, FaultBackend>,
    backend: FaultBackend,
    catalog: koru_sqlite::MemoryName,
    control: Connection,
    source: Workspace,
}

impl Fixture {
    fn new() -> Self {
        let backend = FaultBackend::new(None, false);
        let catalog = koru_sqlite::MemoryName::fresh("replacement");
        let control = koru_sqlite::open_memory(&catalog).unwrap();
        let mut store = Store::open(
            SqliteCatalog::in_named_memory(&catalog).unwrap(),
            backend.clone(),
        )
        .unwrap();
        let source = store
            .create_profile_with_workspace(&request(), at())
            .unwrap()
            .workspace;
        store.take_committed();
        Self {
            store,
            backend,
            catalog,
            control,
            source,
        }
    }
    fn remove(&mut self, delete: bool) -> Result<RemovedWorkspace> {
        if delete {
            self.store
                .delete_workspace_replacing(self.source.id, "Replacement", at())
        } else {
            self.store
                .archive_workspace_replacing(self.source.id, "Replacement", at())
        }
    }
    fn reopen(&mut self) {
        self.store = Store::open(
            SqliteCatalog::in_named_memory(&self.catalog).unwrap(),
            self.backend.clone(),
        )
        .unwrap();
    }
    fn verified(&self) -> koru_store_core::service::VerifiedWorkspaceManifest {
        match self.backend.inspect_workspace_manifest(self.source.id) {
            WorkspaceManifestInspection::Verified(value) => value,
            _ => panic!("expected intact authority"),
        }
    }
    fn assert_live(&self) {
        let authority = self.verified();
        assert!(!authority.archived);
        assert!(authority.deleted_at.is_none());
        assert_eq!(self.store.workspace(self.source.id).unwrap(), self.source);
    }
}

#[test]
fn removing_last_prepares_one_durable_replacement_and_cold_retry_reuses_it() {
    for delete in [false, true] {
        let mut f = Fixture::new();
        let result = f.remove(delete).unwrap();
        assert!(result.authority_committed);
        assert_eq!(result.listing, ListingState::Applied);
        let Replacement::Created(replacement) = result.replacement else {
            panic!("last workspace")
        };
        assert_eq!(replacement.owner, f.source.owner);
        assert_ne!(replacement.id, f.source.id);
        assert!(matches!(
            f.backend.inspect_workspace_manifest(replacement.id),
            WorkspaceManifestInspection::Verified(_)
        ));
        let authority = f.verified();
        assert_eq!(authority.archived, !delete);
        assert_eq!(authority.deleted_at.is_some(), delete);
        assert_eq!(authority.icon, Some(f.source.icon.clone()));
        assert_eq!(authority.color, Some(f.source.color.clone()));
        f.reopen();
        let retry = f.remove(delete).unwrap();
        assert_eq!(retry.replacement, Replacement::Existing(replacement));
        assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 2);
        assert_eq!(f.backend.0.created.borrow().len(), 2);
    }
}

#[test]
fn preparation_failures_leave_source_authority_listing_and_notifications_unchanged() {
    for delete in [false, true] {
        for fault in ["open", "document", "journal", "reindex"] {
            let mut f = Fixture::new();
            f.backend.0.fault.set(Some(fault));
            assert!(f.remove(delete).is_err(), "{fault}");
            f.assert_live();
            assert!(f.store.take_committed().is_empty());
            assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 1);
            assert!(!f.backend.0.deleted.borrow().contains(&f.source.id));
        }
    }
}

#[test]
fn failed_manifest_commit_rolls_back_only_manifest_and_cleans_prepared_replacement() {
    for delete in [false, true] {
        let mut f = Fixture::new();
        let manifest = f.store.manifest_id(f.source.id).unwrap();
        let before = f.store.snapshot(f.source.id, manifest).unwrap();
        let version = Manifest::from_snapshot(&before).unwrap().version();
        let repo = f.backend.open_workspace(f.source.id).unwrap();
        repo.connection.execute_batch("CREATE TRIGGER fail BEFORE UPDATE OF version_vector ON loro_documents BEGIN SELECT RAISE(ABORT, 'source commit fault'); END;").unwrap();
        assert_eq!(f.remove(delete).unwrap_err().code(), Code::StorageFailed);
        f.assert_live();
        let after = f.store.snapshot(f.source.id, manifest).unwrap();
        assert_eq!(Manifest::from_snapshot(&after).unwrap().version(), version);
        assert!(f.store.take_committed().is_empty());
        assert_eq!(f.backend.0.deleted.borrow().len(), 1);
        assert_ne!(f.backend.0.deleted.borrow()[0], f.source.id);
        repo.connection.execute_batch("DROP TRIGGER fail;").unwrap();
        assert!(matches!(
            f.remove(delete).unwrap().replacement,
            Replacement::Created(_)
        ));
    }
}

#[test]
fn publication_failure_retains_committed_removal_and_unlisted_bytes_then_cold_retry_repairs() {
    for delete in [false, true] {
        for table in ["workspaces", "local_transport_modes", "document_locations"] {
            let mut f = Fixture::new();
            f.control.execute_batch(&format!("CREATE TRIGGER fail BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT, 'publication fault'); END;")).unwrap();
            let result = f.remove(delete).unwrap();
            assert!(result.authority_committed);
            assert_eq!(
                result.listing,
                ListingState::Pending {
                    cause: "storage_failed".into()
                }
            );
            assert_eq!(result.replacement, Replacement::Unknown);
            assert_eq!(
                f.store.workspace(f.source.id).unwrap(),
                f.source,
                "catalog transaction rolled back"
            );
            assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 1);
            assert_eq!(
                f.store.take_committed().len(),
                1,
                "only source commit is published"
            );
            assert!(
                f.backend.0.deleted.borrow().is_empty(),
                "committed removal cannot clean prepared storage"
            );
            let unlisted = f.backend.0.created.borrow()[1];
            assert!(matches!(
                f.backend.inspect_workspace_manifest(unlisted),
                WorkspaceManifestInspection::Verified(_)
            ));
            assert_eq!(f.verified().deleted_at.is_some(), delete);
            assert_eq!(f.verified().archived, !delete);
            f.control.execute_batch("DROP TRIGGER fail;").unwrap();
            f.reopen();
            let overlay = f.store.workspace_authority(f.source.owner).unwrap();
            assert!(overlay[0].listing_differs);
            let result = f.remove(delete).unwrap();
            assert_eq!(result.listing, ListingState::Applied);
            assert!(matches!(result.replacement, Replacement::Created(_)));
            assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 2);
            assert!(matches!(
                f.backend.inspect_workspace_manifest(unlisted),
                WorkspaceManifestInspection::Verified(_)
            ));
        }
    }
}

#[test]
fn healthy_remaining_workspace_wins_but_unknown_remaining_bytes_refuse_a_default() {
    let mut f = Fixture::new();
    let uncertain = f
        .store
        .create_workspace(f.source.owner, "Uncertain", at())
        .unwrap();
    f.backend.delete_workspace(uncertain.id).unwrap();
    assert!(f.remove(false).is_err());
    f.assert_live();
    assert_eq!(f.backend.0.created.borrow().len(), 2);
    let healthy = f
        .store
        .create_workspace(f.source.owner, "Healthy", at())
        .unwrap();
    assert_eq!(
        f.remove(false).unwrap().replacement,
        Replacement::Existing(healthy)
    );
    assert_eq!(f.backend.0.created.borrow().len(), 3);
}

#[test]
fn repaired_source_projection_does_not_erase_the_replacement_obligation() {
    for availability in [false, true] {
        let mut f = Fixture::new();
        f.control.execute_batch("CREATE TRIGGER fail BEFORE INSERT ON document_locations BEGIN SELECT RAISE(ABORT, 'publication fault'); END;").unwrap();
        assert!(matches!(
            f.remove(false).unwrap().listing,
            ListingState::Pending { .. }
        ));
        f.control.execute_batch("DROP TRIGGER fail;").unwrap();
        f.reopen();
        // Ordinary reopen may already have repaired the source's catalog row.
        f.store.manifest_id(f.source.id).unwrap();
        assert!(f.store.workspace(f.source.id).unwrap().archived);
        assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 1);
        if availability {
            assert!(matches!(
                f.store
                    .ensure_live_workspace(f.source.owner, "Replacement", at())
                    .unwrap(),
                koru_store_core::service::EnsuredWorkspace::Created { .. }
            ));
        } else {
            assert!(matches!(
                f.remove(false).unwrap().replacement,
                Replacement::Created(_)
            ));
        }
        assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 2);
    }
}

#[test]
fn move_and_source_replacement_commit_together_and_retry_does_not_invent_source() {
    for fail in [false, true] {
        let mut f = Fixture::new();
        let target = f
            .store
            .create_profile("Target", None, None, Locale::default(), at())
            .unwrap();
        if fail {
            // Fail on the replacement's routing, after the owner UPDATE.
            f.control.execute_batch("CREATE TRIGGER fail BEFORE INSERT ON document_locations BEGIN SELECT RAISE(ABORT, 'move publication fault'); END;").unwrap();
        }
        let result = f
            .store
            .move_workspace_replacing(f.source.id, target.id, "Replacement", at());
        if fail {
            assert!(result.is_err());
            f.assert_live();
            assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 1);
            assert_eq!(f.backend.0.deleted.borrow().len(), 1);
            assert!(f.store.take_committed().is_empty());
            continue;
        }
        let result = result.unwrap();
        assert_eq!(result.from, f.source.owner);
        assert_eq!(result.workspace.owner, target.id);
        let Replacement::Created(replacement) = result.replacement else {
            panic!("source replacement")
        };
        assert_eq!(replacement.owner, f.source.owner);
        f.reopen();
        let retry = f
            .store
            .move_workspace_replacing(f.source.id, target.id, "Replacement", at())
            .unwrap();
        assert_eq!(retry.from, target.id);
        assert_eq!(retry.replacement, Replacement::Unknown);
        assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 2);
    }
}

#[test]
fn prerequisite_flush_refuses_before_preparation_and_delete_eviction_is_safe_on_retry() {
    for deletion in [false, true] {
        let mut f = Fixture::new();
        let manifest = f.store.manifest_id(f.source.id).unwrap();
        let snapshot = f.store.snapshot(f.source.id, manifest).unwrap();
        let source = Manifest::from_snapshot(&snapshot).unwrap();
        source.set_peer_id(9876).unwrap();
        let before = source.version();
        if deletion {
            source.set_workspace_deleted_at(Some(at())).unwrap();
        } else {
            source.set_name("Accepted rename").unwrap();
        }
        let delta = source.export_updates_since(&before).unwrap();
        f.store
            .apply_update(
                f.source.id,
                manifest,
                delta,
                at(),
                koru_store_core::ports::UpdateOrigin::Store,
            )
            .unwrap();
        let repo = f.backend.open_workspace(f.source.id).unwrap();
        repo.connection.execute_batch("CREATE TRIGGER fail BEFORE UPDATE OF version_vector ON loro_documents BEGIN SELECT RAISE(ABORT, 'predecessor fault'); END;").unwrap();
        assert_eq!(f.remove(deletion).unwrap_err().code(), Code::StorageFailed);
        assert_eq!(
            f.backend.0.created.borrow().len(),
            1,
            "flush failure precedes preparation"
        );
        assert!(f.store.take_committed().is_empty());
        assert!(!f.verified().archived);
        assert!(f.verified().deleted_at.is_none());
        repo.connection.execute_batch("DROP TRIGGER fail;").unwrap();
        let result = f.remove(deletion).unwrap();
        assert_eq!(result.listing, ListingState::Applied);
        assert!(matches!(result.replacement, Replacement::Created(_)));
        assert_eq!(f.backend.0.created.borrow().len(), 2);
        if !deletion {
            assert_eq!(result.workspace.name, "Accepted rename");
        }
    }
}
