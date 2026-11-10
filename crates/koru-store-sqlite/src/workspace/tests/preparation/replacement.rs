// SPDX-License-Identifier: MPL-2.0

//! The replacing archive, delete and move routes against a real catalog.
//!
//! Since Plan 1.2 of the root catalog none of them *creates* a replacement:
//! an emptied Profile stays empty (`TECHNICAL.final.md` §6.3, D17). What is
//! held here is that selection of an existing live Workspace still works,
//! that the lifecycle authority and its listing keep their order, and that
//! a move is one root transaction.

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
    /// How many repositories the backend has ever been asked to create. One
    /// is the source itself; a second would be a manufactured replacement.
    fn repositories(&self) -> usize {
        self.backend.0.created.borrow().len()
    }
}

#[test]
fn removing_the_last_workspace_leaves_the_profile_empty_and_creates_nothing() {
    for delete in [false, true] {
        let mut f = Fixture::new();
        let result = f.remove(delete).unwrap();
        assert!(result.authority_committed);
        assert_eq!(result.listing, ListingState::Applied);
        assert_eq!(result.replacement, Replacement::NotNeeded);
        assert_eq!(f.repositories(), 1, "no replacement was manufactured");
        let authority = f.verified();
        assert_eq!(authority.archived, !delete);
        assert_eq!(authority.deleted_at.is_some(), delete);
        assert_eq!(authority.icon, Some(f.source.icon.clone()));
        assert_eq!(authority.color, Some(f.source.color.clone()));
        assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 1);
        assert_eq!(f.store.workspaces().unwrap().len(), usize::from(!delete));
        f.reopen();
        let retry = f.remove(delete).unwrap();
        assert_eq!(retry.replacement, Replacement::NotNeeded);
        assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 1);
        assert_eq!(f.repositories(), 1);
    }
}

#[test]
fn a_creation_fault_cannot_touch_a_removal_because_nothing_is_created() {
    for delete in [false, true] {
        for fault in ["open", "document", "journal", "reindex"] {
            let mut f = Fixture::new();
            f.backend.0.fault.set(Some(fault));
            let result = f.remove(delete).unwrap();
            assert_eq!(result.replacement, Replacement::NotNeeded, "{fault}");
            assert_eq!(result.listing, ListingState::Applied, "{fault}");
            assert_eq!(f.repositories(), 1, "{fault}");
            assert!(!f.backend.0.deleted.borrow().contains(&f.source.id));
        }
    }
}

/// The archive is still manifest-authoritative, so a failed manifest commit
/// refuses it and rolls back nothing else.
#[test]
fn failed_manifest_commit_rolls_back_only_the_manifest() {
    let mut f = Fixture::new();
    let manifest = f.store.manifest_id(f.source.id).unwrap();
    let before = f.store.snapshot(f.source.id, manifest).unwrap();
    let version = Manifest::from_snapshot(&before).unwrap().version();
    let repo = f.backend.open_workspace(f.source.id).unwrap();
    repo.connection.execute_batch("CREATE TRIGGER fail BEFORE UPDATE OF version_vector ON loro_documents BEGIN SELECT RAISE(ABORT, 'source commit fault'); END;").unwrap();
    assert_eq!(f.remove(false).unwrap_err().code(), Code::StorageFailed);
    f.assert_live();
    let after = f.store.snapshot(f.source.id, manifest).unwrap();
    assert_eq!(Manifest::from_snapshot(&after).unwrap().version(), version);
    assert!(f.store.take_committed().is_empty());
    assert!(
        f.backend.0.deleted.borrow().is_empty(),
        "nothing was prepared, so nothing is cleaned up"
    );
    repo.connection.execute_batch("DROP TRIGGER fail;").unwrap();
    assert_eq!(f.remove(false).unwrap().replacement, Replacement::NotNeeded);
}

/// **The delete is a root fact since Plan 1.3** (root-catalog
/// TECHNICAL.final §6): the fact and its projections commit first, and the
/// manifest's `deleted_at` is a compatibility hint written *after* that
/// commit, best-effort. A repository that cannot take the hint therefore
/// changes nothing — the deletion stands, the manifest keeps saying live,
/// and no replacement is manufactured.
#[test]
fn a_deletion_whose_manifest_hint_cannot_be_written_stands_on_its_root_fact() {
    let mut f = Fixture::new();
    let manifest = f.store.manifest_id(f.source.id).unwrap();
    let before = f.store.snapshot(f.source.id, manifest).unwrap();
    let version = Manifest::from_snapshot(&before).unwrap().version();
    let repo = f.backend.open_workspace(f.source.id).unwrap();
    repo.connection.execute_batch("CREATE TRIGGER fail BEFORE UPDATE OF version_vector ON loro_documents BEGIN SELECT RAISE(ABORT, 'source commit fault'); END;").unwrap();

    let result = f.remove(true).unwrap();
    assert!(result.authority_committed);
    assert_eq!(result.listing, ListingState::Applied);
    assert_eq!(result.replacement, Replacement::NotNeeded);
    assert_eq!(result.workspace.deleted_at, Some(at()));
    assert_eq!(
        f.store
            .workspace_including_deleted(f.source.id)
            .unwrap()
            .deleted_at,
        Some(at()),
        "the root fact was projected"
    );
    let after = f.store.snapshot(f.source.id, manifest).unwrap();
    assert_eq!(
        Manifest::from_snapshot(&after).unwrap().version(),
        version,
        "the hint was written anyway"
    );
    assert!(f.verified().deleted_at.is_none(), "the hint is only a hint");
    assert_eq!(f.repositories(), 1);
    assert!(f.backend.0.deleted.borrow().is_empty());
    repo.connection.execute_batch("DROP TRIGGER fail;").unwrap();
}

#[test]
fn archive_publication_failure_retains_the_committed_removal_and_a_cold_retry_repairs_the_listing()
{
    for table in ["workspaces", "local_transport_modes"] {
        let mut f = Fixture::new();
        f.control.execute_batch(&format!("CREATE TRIGGER fail BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT, 'publication fault'); END;")).unwrap();
        let result = f.remove(false).unwrap();
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
        assert_eq!(
            f.repositories(),
            1,
            "{table}: no unlisted repository exists"
        );
        assert!(f.backend.0.deleted.borrow().is_empty());
        assert!(f.verified().deleted_at.is_none());
        assert!(f.verified().archived);
        f.control.execute_batch("DROP TRIGGER fail;").unwrap();
        f.reopen();
        let overlay = f.store.workspace_authority(f.source.owner).unwrap();
        assert!(overlay[0].listing_differs);
        let result = f.remove(false).unwrap();
        assert_eq!(result.listing, ListingState::Applied);
        assert_eq!(result.replacement, Replacement::NotNeeded);
        assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 1);
        assert_eq!(f.repositories(), 1);
    }
}

/// The delete's own publication is the root transaction, so a failure in it
/// is a **refusal** — nothing was deleted — rather than a committed removal
/// with a pending listing (`service::replacement`'s `delete_workspace_root`).
#[test]
fn a_delete_publication_failure_refuses_and_deletes_nothing() {
    let mut f = Fixture::new();
    f.control.execute_batch("CREATE TRIGGER fail BEFORE INSERT ON workspaces BEGIN SELECT RAISE(ABORT, 'publication fault'); END;").unwrap();
    assert_eq!(f.remove(true).unwrap_err().code(), Code::StorageFailed);
    f.assert_live();
    assert!(f.verified().deleted_at.is_none());
    assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 1);
    assert_eq!(f.repositories(), 1);
    assert!(f.backend.0.deleted.borrow().is_empty());

    f.control.execute_batch("DROP TRIGGER fail;").unwrap();
    let result = f.remove(true).unwrap();
    assert_eq!(result.listing, ListingState::Applied);
    assert_eq!(result.replacement, Replacement::NotNeeded);
    assert_eq!(
        f.store
            .workspace_including_deleted(f.source.id)
            .unwrap()
            .deleted_at,
        Some(at())
    );
    assert_eq!(f.repositories(), 1);
}

#[test]
fn healthy_remaining_workspace_is_selected_but_unknown_remaining_bytes_refuse_the_removal() {
    let mut f = Fixture::new();
    let uncertain = f
        .store
        .create_workspace(f.source.owner, "Uncertain", at())
        .unwrap();
    f.backend.delete_workspace(uncertain.id).unwrap();
    assert!(f.remove(false).is_err());
    f.assert_live();
    assert_eq!(f.repositories(), 2);
    let healthy = f
        .store
        .create_workspace(f.source.owner, "Healthy", at())
        .unwrap();
    assert_eq!(
        f.remove(false).unwrap().replacement,
        Replacement::Existing(healthy)
    );
    assert_eq!(f.repositories(), 3);
}

#[test]
fn a_repaired_source_projection_still_lists_the_removal_and_creates_nothing() {
    let mut f = Fixture::new();
    f.control.execute_batch("CREATE TRIGGER fail BEFORE INSERT ON workspaces BEGIN SELECT RAISE(ABORT, 'publication fault'); END;").unwrap();
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
    assert_eq!(f.remove(false).unwrap().replacement, Replacement::NotNeeded);
    assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 1);
    assert_eq!(f.repositories(), 1);
    // `EnsureLive` creates nothing either since Plan 1.3: a Profile whose
    // only Workspace is archived is empty, and stays that way (the full arm
    // is in `workspace::tests::availability`).
    assert!(matches!(
        f.store
            .ensure_live_workspace(f.source.owner, "Replacement", at())
            .unwrap(),
        koru_store_core::service::EnsuredWorkspace::Empty
    ));
    assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 1);
}

/// The destination here is a Profile nobody has spoken about, so it is
/// Local-only and the new placement waits in the private overlay (the
/// exportable destination is `preparation::root_catalog`'s move tests). What
/// is held here is that the placement write and the projection are **one**
/// transaction: a failure in the projection leaves the unit creation
/// published with the row (Plan 1.3) exactly as it was, with no overlay.
#[test]
fn a_move_commits_its_placement_and_projection_together_and_a_retry_changes_nothing() {
    for fail in [false, true] {
        let mut f = Fixture::new();
        let target = f
            .store
            .create_profile("Target", None, None, Locale::default(), at())
            .unwrap();
        if fail {
            // Fail on the projection, after the placement write in the same
            // transaction.
            f.control.execute_batch("CREATE TRIGGER fail BEFORE UPDATE ON workspaces BEGIN SELECT RAISE(ABORT, 'move publication fault'); END;").unwrap();
        }
        let result = f
            .store
            .move_workspace_replacing(f.source.id, target.id, "Replacement", at());
        let scope = f.store_catalog().local_user_scope().unwrap();
        let unit = koru_store_core::root_catalog::CatalogUnitRef::Workspace {
            workspace: f.source.id,
        };
        if fail {
            assert!(result.is_err());
            f.assert_live();
            assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 1);
            let row = f
                .store_catalog()
                .root_catalog_unit(scope, unit)
                .unwrap()
                .expect("creation published the Workspace unit with its row");
            assert_eq!(row.revision, 1, "the placement write was not rolled back");
            assert_eq!(
                placed_under(&row, f.source.id),
                Some(f.source.owner),
                "the rolled-back move moved the placement anyway"
            );
            assert!(
                f.store_catalog()
                    .root_catalog_overlay(scope, unit)
                    .unwrap()
                    .is_none(),
                "a private fork survived the rollback"
            );
            assert!(f.backend.0.deleted.borrow().is_empty());
            assert!(f.store.take_committed().is_empty());
            continue;
        }
        let result = result.unwrap();
        assert_eq!(result.from, f.source.owner);
        assert_eq!(result.workspace.owner, target.id);
        assert_eq!(
            result.replacement,
            Replacement::NotNeeded,
            "the source Profile is left empty, not refilled"
        );
        assert_eq!(f.repositories(), 1);
        let row = f
            .store_catalog()
            .root_catalog_unit(scope, unit)
            .unwrap()
            .unwrap();
        assert_eq!(
            row.revision, 1,
            "a Local-only destination writes the placement to the overlay"
        );
        assert_eq!(placed_under(&row, f.source.id), Some(f.source.owner));
        assert!(
            f.store_catalog()
                .root_catalog_overlay(scope, unit)
                .unwrap()
                .is_some(),
            "the new placement has to be somewhere"
        );
        f.reopen();
        let retry = f
            .store
            .move_workspace_replacing(f.source.id, target.id, "Replacement", at())
            .unwrap();
        assert_eq!(retry.from, target.id);
        assert_eq!(retry.replacement, Replacement::Unknown);
        assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 1);
        assert_eq!(
            f.store_catalog()
                .root_catalog_unit(scope, unit)
                .unwrap()
                .unwrap()
                .revision,
            1,
            "a same-owner retry is not a second move"
        );
    }
}

impl Fixture {
    /// A second handle on the catalog, for the root rows the Store keeps
    /// to itself.
    fn store_catalog(&self) -> SqliteCatalog {
        SqliteCatalog::in_named_memory(&self.catalog).unwrap()
    }
}

/// The Profile a durable unit row places a Workspace under.
fn placed_under(
    row: &koru_store_core::ports::RootCatalogUnitRow,
    workspace: WorkspaceId,
) -> Option<ProfileId> {
    koru_store_core::root_catalog::WorkspaceCatalogUnit::from_snapshot(
        &row.snapshot,
        workspace,
        row.revision.try_into().unwrap(),
    )
    .unwrap()
    .placement()
    .and_then(|placement| placement.profile)
}

#[test]
fn prerequisite_flush_refuses_before_the_removal_and_delete_eviction_is_safe_on_retry() {
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
        assert_eq!(f.repositories(), 1);
        assert!(f.store.take_committed().is_empty());
        assert!(!f.verified().archived);
        assert!(f.verified().deleted_at.is_none());
        repo.connection.execute_batch("DROP TRIGGER fail;").unwrap();
        let result = f.remove(deletion).unwrap();
        assert_eq!(result.listing, ListingState::Applied);
        assert_eq!(result.replacement, Replacement::NotNeeded);
        assert_eq!(f.repositories(), 1);
        if !deletion {
            assert_eq!(result.workspace.name, "Accepted rename");
        }
    }
}
