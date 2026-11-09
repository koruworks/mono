// SPDX-License-Identifier: MPL-2.0

//! Plan 1.3 of the root catalog against a real catalog, entirely in named
//! memory: the root is the lifecycle writer, a Profile deletion is a
//! captured batch, a restore observes exactly its batch, the purge retires
//! personal rows per Workspace and never around the guard, an empty Profile
//! stays empty, and a Workspace under a deleted Profile is found again.
//! Reuses `root_catalog.rs`'s fixture (`Root`) and the `FaultBackend`.

use super::root_catalog::Root;
use super::*;
use koru_store_core::ids::{DeletionId, DocumentId, RestoreId, UserScopeId};
use koru_store_core::model::{Profile, Workspace};
use koru_store_core::ports::{PurgeHold, RootCatalogUnitCommit};
use koru_store_core::root_catalog::{
    CatalogUnitRef, DeletionRecord, ProfileCatalogUnit, RestoreRecord, WorkspaceCatalogUnit,
};
use koru_store_core::service::{
    EnsuredWorkspace, ProfileDeletionJob, UnassignedReason, batch_job_key,
};
use koru_store_core::transport::{TransportMode, TransportSubject};
use koru_store_core::{Manifest, TRASH_RETENTION_MS};

fn after_retention(from: Timestamp) -> Timestamp {
    Timestamp::from_millis(from.millis() + TRASH_RETENTION_MS)
}

impl Root {
    /// The Workspace unit as the Store reads it: the private overlay when
    /// one is held (a Local-only Profile's lifecycle facts land there), else
    /// the durable row.
    fn workspace_unit(&self, workspace: WorkspaceId) -> WorkspaceCatalogUnit {
        match self.overlay(workspace) {
            Some(overlay) => {
                WorkspaceCatalogUnit::from_snapshot(&overlay.private_snapshot, workspace, 1)
                    .unwrap()
            }
            None => WorkspaceCatalogUnit::from_snapshot(
                &self.unit(workspace).unwrap().snapshot,
                workspace,
                1,
            )
            .unwrap(),
        }
    }

    fn profile_unit(&self, profile: ProfileId) -> Option<ProfileCatalogUnit> {
        let unit = CatalogUnitRef::Profile { profile };
        let catalog = self.catalog();
        if let Some(overlay) = catalog.root_catalog_overlay(self.scope(), unit).unwrap() {
            return Some(
                ProfileCatalogUnit::from_snapshot(&overlay.private_snapshot, profile, 1).unwrap(),
            );
        }
        catalog
            .root_catalog_unit(self.scope(), unit)
            .unwrap()
            .map(|row| ProfileCatalogUnit::from_snapshot(&row.snapshot, profile, 1).unwrap())
    }

    /// What another device's converged write would leave in the durable
    /// Workspace unit: edit a copy under a foreign writer and commit it
    /// under the row's revision. Only for exportable (Cloud) subjects, whose
    /// facts are durable.
    fn remote_unit_edit(&self, workspace: WorkspaceId, edit: impl Fn(&WorkspaceCatalogUnit)) {
        assert!(
            self.overlay(workspace).is_none(),
            "a remote edit reaches the durable unit; this subject holds an overlay"
        );
        let row = self.unit(workspace).unwrap();
        let unit = WorkspaceCatalogUnit::from_snapshot(&row.snapshot, workspace, 77).unwrap();
        edit(&unit);
        self.catalog()
            .commit_root_catalog_units(
                self.scope(),
                &[RootCatalogUnitCommit {
                    unit: CatalogUnitRef::Workspace { workspace },
                    snapshot: unit.snapshot().unwrap(),
                    expected_revision: Some(row.revision),
                }],
            )
            .unwrap();
    }

    fn job(&self, batch: koru_store_core::ids::BatchId) -> ProfileDeletionJob {
        serde_json::from_str(
            &self
                .catalog()
                .state(&batch_job_key(batch))
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    }

    fn row(&self, workspace: WorkspaceId) -> Workspace {
        self.store.workspace_including_deleted(workspace).unwrap()
    }
}

// ------------------------------------------------------------ creation

#[test]
fn creation_publishes_the_root_units_with_the_rows() {
    let mut f = Root::new();
    let a = f.profile("A");
    assert!(
        f.profile_unit(a).is_some_and(|unit| unit.is_live()),
        "a Profile is created with its unit"
    );
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let unit = f.workspace_unit(w);
    assert_eq!(unit.placement().unwrap().profile, Some(a));
    assert!(unit.is_live());
    assert_eq!(f.unit(w).unwrap().revision, 1);
    assert_eq!(f.store.root_placement(w).unwrap().unwrap().profile, Some(a));

    let created = f
        .store
        .create_profile_with_workspace(&request(), at())
        .unwrap();
    assert!(f.profile_unit(created.profile.id).is_some());
    assert_eq!(
        f.workspace_unit(created.workspace.id)
            .placement()
            .unwrap()
            .profile,
        Some(created.profile.id)
    );
    // Both root units are in the one publication: a fault on the second
    // leaves no Profile, no Workspace, no unit.
    let mut g = Root::new();
    g.fail_on("BEFORE INSERT ON root_catalog_units WHEN NEW.unit_kind = 'workspace_catalog'");
    assert!(
        g.store
            .create_profile_with_workspace(&request(), at())
            .is_err()
    );
    g.recover();
    assert!(g.store.profiles().unwrap().is_empty());
    assert!(g.store.workspaces_including_deleted().unwrap().is_empty());
    assert_eq!(g.count("SELECT COUNT(*) FROM root_catalog_units"), 0);
}

// ------------------------------------------------------- empty Profile

#[test]
fn an_empty_profile_survives_open_restart_and_ensure_live_without_a_workspace_or_unit() {
    let mut f = Root::new();
    let a = f.profile("A");
    for _ in 0..2 {
        assert_eq!(
            f.store.ensure_live_workspace(a, "Default", at()).unwrap(),
            EnsuredWorkspace::Empty
        );
        assert!(f.store.workspaces_including_deleted().unwrap().is_empty());
        assert_eq!(
            f.count(
                "SELECT COUNT(*) FROM root_catalog_units WHERE unit_kind = 'workspace_catalog'"
            ),
            0
        );
        f.restart();
    }
    // Trashing the only Workspace leaves the Profile empty, not replaced.
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    f.store.delete_workspace(w, at()).unwrap();
    assert_eq!(
        f.store.ensure_live_workspace(a, "Default", at()).unwrap(),
        EnsuredWorkspace::Empty
    );
    f.restart();
    assert_eq!(
        f.store.ensure_live_workspace(a, "Default", at()).unwrap(),
        EnsuredWorkspace::Empty
    );
    assert_eq!(f.store.workspaces_including_deleted().unwrap().len(), 1);
    assert_eq!(
        f.count("SELECT COUNT(*) FROM root_catalog_units WHERE unit_kind = 'workspace_catalog'"),
        1,
        "no replacement unit either"
    );
}

// ------------------------------------------------ trash and restore

#[test]
fn trash_and_restore_write_facts_on_the_unit_and_project_one_way() {
    let mut f = Root::new();
    let a = f.cloud_profile("A");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let (document, _) = f
        .store
        .create_document(w, "One", "page", None, None, at())
        .unwrap();
    let revision_before = f.unit(w).unwrap().revision;

    let deleted = f.store.delete_workspace(w, at()).unwrap();
    assert_eq!(deleted.deleted_at, Some(at()));
    let unit = f.workspace_unit(w);
    assert_eq!(unit.active_deletions().len(), 1);
    let (id, record) = unit.deletions().into_iter().next().unwrap();
    assert_eq!(record.at_ms, at().millis());
    assert_eq!(record.retention_ms, TRASH_RETENTION_MS);
    assert_eq!(record.batch_id, None);
    assert_eq!(f.unit(w).unwrap().revision, revision_before + 1);
    assert_eq!(f.row(w).deleted_at, Some(at()), "projected one-way");
    assert_eq!(f.store.locate(document).unwrap(), None, "routing removed");
    assert_eq!(
        f.store.workspace(w).unwrap_err().code(),
        Code::WorkspaceNotFound
    );
    assert_eq!(
        f.store.workspace_manifest_deletion(w).unwrap(),
        Some(Some(at())),
        "the manifest hint is written after the commit"
    );
    // Idempotent: a second trash writes no second fact.
    f.store.delete_workspace(w, at()).unwrap();
    assert_eq!(f.workspace_unit(w).deletions().len(), 1);
    f.restart();
    assert_eq!(f.row(w).deleted_at, Some(at()));

    let restored = f.store.restore_workspace(w).unwrap();
    assert_eq!(restored.deleted_at, None);
    let unit = f.workspace_unit(w);
    assert!(unit.is_live());
    let restores = unit.restores();
    assert_eq!(restores.len(), 1);
    assert_eq!(
        restores.values().next().unwrap().observed_deletions,
        vec![id],
        "the restore names the deletion it observed"
    );
    assert_eq!(f.row(w).deleted_at, None);
    assert_eq!(f.store.locate(document).unwrap(), Some(w), "routing back");
    assert_eq!(f.store.workspace_manifest_deletion(w).unwrap(), Some(None));
    f.restart();
    assert_eq!(f.row(w).deleted_at, None);
    assert!(f.store.workspace(w).is_ok());
}

#[test]
fn a_local_only_profiles_lifecycle_facts_wait_in_the_overlay_and_still_project() {
    let mut f = Root::new();
    // Migration 12's seeded default: nobody chose it, nothing may leave.
    let a = f.profile("A");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let durable_before = f.unit(w).unwrap();
    assert!(
        f.overlay(w).is_none(),
        "creation publishes the durable unit"
    );

    f.store.delete_workspace(w, at()).unwrap();
    assert_eq!(
        f.unit(w).unwrap(),
        durable_before,
        "the durable bytes are untouched: nothing about the Trash may leave"
    );
    let overlay = f.overlay(w).expect("the fact waits in the overlay");
    assert_ne!(overlay.writer_peer_id, f.store.peer_id());
    assert!(
        f.workspace_unit(w).is_deleted(),
        "the overlay is what the device reads"
    );
    assert_eq!(
        f.row(w).deleted_at,
        Some(at()),
        "and the projection follows it"
    );
    assert_eq!(
        f.store.workspace(w).unwrap_err().code(),
        Code::WorkspaceNotFound
    );
    f.restart();
    assert_eq!(f.row(w).deleted_at, Some(at()));

    f.store.restore_workspace(w).unwrap();
    assert!(f.workspace_unit(w).is_live());
    assert_eq!(f.row(w).deleted_at, None);
    assert_eq!(f.unit(w).unwrap(), durable_before);
    // Enabling export publishes the overlay's facts into the durable unit.
    f.set(TransportSubject::Profile(a), 1, TransportMode::Cloud);
    f.set(TransportSubject::Workspace(w), 1, TransportMode::Cloud);
    assert!(f.overlay(w).is_none(), "cleared with the publication");
    let published = f.workspace_unit(w);
    assert_eq!(published.deletions().len(), 1);
    assert_eq!(published.restores().len(), 1);
    assert!(published.is_live());
}

#[test]
fn a_failed_lifecycle_publication_changes_nothing_and_a_restore_needs_its_owner() {
    let mut f = Root::new();
    let a = f.profile("A");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let unit_before = f.unit(w).unwrap();
    f.fail_on("BEFORE UPDATE ON workspaces");
    assert_eq!(
        f.store.delete_workspace(w, at()).unwrap_err().code(),
        Code::StorageFailed
    );
    f.recover();
    assert_eq!(f.unit(w).unwrap(), unit_before, "the unit rolled back too");
    assert!(f.overlay(w).is_none(), "and so did the overlay row");
    assert_eq!(f.row(w).deleted_at, None);
    assert_eq!(
        f.store.workspace_manifest_deletion(w).unwrap(),
        None,
        "no hint before the commit"
    );
    assert!(f.store.workspace(w).is_ok(), "still open and live");

    // A Workspace whose Profile is gone from this device is not restored by
    // the Workspace route; the Profile's own operation is.
    f.store.delete_workspace(w, at()).unwrap();
    let mark = f
        .store
        .begin_profile_removal(
            a,
            koru_store_core::profile_removal::RemovalKind::Local,
            at(),
        )
        .unwrap();
    f.store
        .remove_profile_local(a, Some(mark.attempt), at())
        .unwrap();
    assert_eq!(
        f.store.restore_workspace(w).unwrap_err().code(),
        Code::ProfileRemoved
    );
    assert_eq!(f.workspace_unit(w).restores().len(), 0);
}

#[test]
fn an_incoming_manifest_deletion_is_a_hint_that_cannot_trash_a_unit_holding_workspace() {
    let mut f = Root::new();
    let a = f.profile("A");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let manifest = f.store.manifest_id(w).unwrap();

    // A pre-13 peer's manifest says the Workspace is deleted.
    let snapshot = f.store.snapshot(w, manifest).unwrap();
    let remote = Manifest::from_snapshot(&snapshot).unwrap();
    remote.set_peer_id(99).unwrap();
    let before = remote.version();
    remote
        .set_workspace_deleted_at(Some(Timestamp::from_millis(at().millis() + 1)))
        .unwrap();
    let update = remote.export_updates_since(&before).unwrap();
    f.store
        .apply_update(
            w,
            manifest,
            update,
            at(),
            koru_store_core::ports::UpdateOrigin::Store,
        )
        .unwrap();
    f.store.flush(w, manifest, at()).unwrap();

    assert_eq!(
        f.store.workspace_manifest_deletion(w).unwrap(),
        Some(Some(Timestamp::from_millis(at().millis() + 1))),
        "the hint arrived"
    );
    assert_eq!(f.row(w).deleted_at, None, "and changed no projection");
    assert!(f.workspace_unit(w).is_live(), "and produced no root fact");
    assert!(f.store.workspace(w).is_ok());
    // Nor does the explicit listing repair or the authority read believe it.
    let repaired = f.store.reconcile_workspace_listing(w, at()).unwrap();
    assert_eq!(repaired.deleted_at, None);
    let authority = f.store.workspace_authority(a).unwrap();
    assert_eq!(authority.len(), 1);
    assert!(matches!(
        authority[0].authority,
        koru_store_core::service::WorkspaceAuthorityState::Read {
            deleted_at: None,
            ..
        }
    ));
    f.restart();
    assert_eq!(f.row(w).deleted_at, None);
    assert!(f.store.workspace(w).is_ok());
}

// ------------------------------------------------------------- batch

#[test]
fn a_profile_deletion_is_a_captured_batch_and_its_restore_observes_exactly_that_batch() {
    let mut f = Root::new();
    let p = f.cloud_profile("P");
    let q = f.cloud_profile("Q");
    let w1 = f.store.create_workspace(p, "W1", at()).unwrap().id;
    let w2 = f.store.create_workspace(p, "W2", at()).unwrap().id;
    let w3 = f.store.create_workspace(p, "W3", at()).unwrap().id;
    let w4 = f.store.create_workspace(p, "W4", at()).unwrap().id;
    let elsewhere = f.store.create_workspace(q, "Q1", at()).unwrap().id;
    // W4 is in the Trash already: its deletion is its own, not the batch's.
    f.store.delete_workspace(w4, at()).unwrap();

    let preview = f.store.profile_deletion_preview(p).unwrap();
    assert_eq!(preview.targets, vec![w1, w2, w3]);
    // The list changed under the confirmation: refused, nothing written.
    let w5 = f.store.create_workspace(p, "W5", at()).unwrap().id;
    assert_eq!(
        f.store
            .delete_profile(p, &preview, at())
            .unwrap_err()
            .code(),
        Code::IdempotencyConflict
    );
    assert!(f.store.profile(p).is_ok());
    assert!(f.profile_unit(p).unwrap().is_live());
    let fresh = f.store.profile_deletion_preview(p).unwrap();
    assert_eq!(fresh.targets, vec![w1, w2, w3, w5]);
    assert_ne!(fresh.placement_revision, preview.placement_revision);

    // A fault on the last projection rolls the whole batch back.
    f.fail_on("BEFORE UPDATE ON profiles");
    assert!(f.store.delete_profile(p, &fresh, at()).is_err());
    f.recover();
    assert!(f.store.profile(p).is_ok());
    assert!(f.workspace_unit(w1).is_live());
    assert_eq!(f.row(w1).deleted_at, None);
    assert_eq!(
        f.count("SELECT COUNT(*) FROM store_state WHERE key LIKE 'root_catalog.batch.%'"),
        0
    );

    let batch = f.store.delete_profile(p, &fresh, at()).unwrap();
    assert_eq!(
        batch.targets.iter().map(|(w, _)| *w).collect::<Vec<_>>(),
        vec![w1, w2, w3, w5]
    );
    // The Profile unit: one deletion with the batch id, no target list.
    let unit = f.profile_unit(p).unwrap();
    assert_eq!(unit.active_deletions(), [batch.profile_deletion].into());
    assert_eq!(
        unit.deletions()[&batch.profile_deletion].batch_id,
        Some(batch.batch_id.as_uuid())
    );
    // Each target: its own fact with the same batch id, projected one-way.
    for (workspace, deletion) in &batch.targets {
        let unit = f.workspace_unit(*workspace);
        assert_eq!(unit.active_deletions(), [*deletion].into());
        assert_eq!(
            unit.deletions()[deletion].batch_id,
            Some(batch.batch_id.as_uuid())
        );
        assert_eq!(f.row(*workspace).deleted_at, Some(at()));
    }
    assert_eq!(
        f.row(w4).deleted_at,
        Some(at()),
        "W4 keeps its own Trash entry"
    );
    assert_eq!(f.row(elsewhere).deleted_at, None);
    // Visibility: hidden like a removed Profile; the row is kept.
    assert!(f.store.profiles().unwrap().iter().all(|row| row.id != p));
    assert!(f.store.profile(p).is_err());
    assert_eq!(
        f.store.reminders(p).unwrap_err().code(),
        Code::ProfileRemoved,
        "hidden the way a removed Profile is"
    );
    assert!(f.catalog().profile_including_removed(p).unwrap().is_some());
    // The device-local inverse cannot clear a shared deletion.
    assert_eq!(
        f.store.restore_profile_local(p).unwrap_err().code(),
        Code::ProfileRemoved
    );
    // The job holds the exact targets.
    let job = f.job(batch.batch_id);
    assert_eq!(job.targets, batch.targets);
    assert_eq!(job.profile_deletion, batch.profile_deletion);
    assert_eq!(job.phase, koru_store_core::service::BatchPhase::Deleted);
    // Listed as restorable.
    let listed = f.store.profile_deletions(at()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].profile, p);
    assert_eq!(listed[0].deletion, batch.profile_deletion);
    assert_eq!(listed[0].targets, vec![w1, w2, w3, w5]);
    assert!(listed[0].expired_or_purged.is_empty());
    f.restart();
    assert_eq!(f.store.profile_deletions(at()).unwrap().len(), 1);

    // What other devices (M2) do meanwhile, written as they would land:
    // W2 gets an independent deletion; W3 is restored from the batch and
    // moved to Q; W5 is purged after an expired second deletion.
    f.remote_unit_edit(w2, |unit| {
        unit.record_deletion(
            DeletionId::new(),
            &DeletionRecord::new(at().millis() + 5, None),
        )
        .unwrap();
    });
    let w3_deletion = batch.targets[2].1;
    f.remote_unit_edit(w3, |unit| {
        unit.record_restore(
            RestoreId::new(),
            &RestoreRecord {
                at_ms: at().millis() + 5,
                observed_deletions: vec![w3_deletion],
                batch_id: Some(batch.batch_id.as_uuid()),
            },
        )
        .unwrap();
        unit.set_placement(Some(q), DocumentId::new().as_uuid())
            .unwrap();
    });
    f.remote_unit_edit(w5, |unit| {
        unit.record_deletion(
            DeletionId::new(),
            &DeletionRecord {
                at_ms: at().millis(),
                retention_ms: 0,
                batch_id: None,
            },
        )
        .unwrap();
    });
    let purge = f.store.purge_deleted(at()).unwrap();
    assert_eq!(purge.purged, vec![w5], "only the expired deletion purges");
    assert!(f.store.workspace_including_deleted(w5).is_err());
    assert!(!f.workspace_unit(w5).purged().is_empty(), "evidence kept");
    assert_eq!(
        f.store.profile_deletions(at()).unwrap()[0].expired_or_purged,
        vec![w5]
    );

    let outcome = f.store.restore_profile(p, batch.profile_deletion).unwrap();
    assert_eq!(outcome.restored, vec![w1]);
    assert_eq!(outcome.left_in_trash, vec![w2]);
    assert_eq!(outcome.moved_elsewhere, vec![w3]);
    assert_eq!(outcome.purged, vec![w5]);
    assert!(f.store.profile(p).is_ok(), "visible again");
    assert!(f.profile_unit(p).unwrap().is_live());
    assert_eq!(f.row(w1).deleted_at, None);
    assert!(f.workspace_unit(w1).is_live());
    assert_eq!(
        f.row(w2).deleted_at,
        Some(at()),
        "the independent deletion holds W2"
    );
    assert_eq!(f.workspace_unit(w2).active_deletions().len(), 1);
    assert_eq!(
        f.store.root_placement(w3).unwrap().unwrap().profile,
        Some(q),
        "W3 stays where it was moved"
    );
    assert_eq!(
        f.workspace_unit(w3).restores().len(),
        1,
        "nothing was written on W3"
    );
    assert_eq!(
        f.row(w4).deleted_at,
        Some(at()),
        "earlier Trash stays trashed"
    );
    assert!(matches!(
        f.job(batch.batch_id).phase,
        koru_store_core::service::BatchPhase::Restored { .. }
    ));
    assert!(f.store.profile_deletions(at()).unwrap().is_empty());
    // The same deletion cannot be restored twice.
    assert_eq!(
        f.store
            .restore_profile(p, batch.profile_deletion)
            .unwrap_err()
            .code(),
        Code::InvalidRequest
    );
    f.restart();
    assert!(f.store.profile(p).is_ok());
    assert_eq!(f.row(w1).deleted_at, None);
    assert_eq!(f.row(w2).deleted_at, Some(at()));
}

#[test]
fn a_workspace_placed_into_a_deleted_profile_after_capture_stays_live_and_is_unassigned() {
    let mut f = Root::new();
    let p = f.cloud_profile("P");
    let q = f.cloud_profile("Q");
    let target = f.store.create_workspace(p, "T", at()).unwrap().id;
    let late = f.store.create_workspace(q, "Late", at()).unwrap().id;
    let preview = f.store.profile_deletion_preview(p).unwrap();
    let batch = f.store.delete_profile(p, &preview, at()).unwrap();
    assert_eq!(batch.targets.len(), 1);

    // A concurrent move into P that the capture did not see, as it lands.
    f.remote_unit_edit(late, |unit| {
        unit.set_placement(Some(p), DocumentId::new().as_uuid())
            .unwrap();
    });
    assert!(f.store.workspace(late).is_ok(), "preserved, not deleted");
    assert!(f.workspace_unit(late).is_live());
    let unassigned = f.store.unassigned_workspaces().unwrap();
    assert_eq!(unassigned.len(), 1);
    assert_eq!(unassigned[0].workspace.id, late);
    assert_eq!(unassigned[0].former_profile, Some(p));
    assert_eq!(unassigned[0].reason, UnassignedReason::ProfileDeleted);
    assert!(
        !unassigned.iter().any(|u| u.workspace.id == target),
        "a captured target is in the Trash, not unassigned"
    );
    // A normal move recovers it; restoring P would too.
    f.store.move_workspace(late, q).unwrap();
    assert!(f.store.unassigned_workspaces().unwrap().is_empty());

    // The other reasons.
    let orphan = f.store.create_workspace(q, "Orphan", at()).unwrap().id;
    f.remote_unit_edit(orphan, |unit| {
        unit.set_placement(None, DocumentId::new().as_uuid())
            .unwrap();
    });
    let absent = f.store.create_workspace(q, "Absent", at()).unwrap().id;
    f.remote_unit_edit(absent, |unit| {
        unit.set_placement(Some(ProfileId::new()), DocumentId::new().as_uuid())
            .unwrap();
    });
    let holder = Profile::holder(Locale::default(), at()).unwrap();
    f.catalog().put_profile(&holder).unwrap();
    let held = f.store.create_workspace(q, "Held", at()).unwrap().id;
    f.remote_unit_edit(held, |unit| {
        unit.set_placement(Some(holder.id), DocumentId::new().as_uuid())
            .unwrap();
    });
    let reasons: std::collections::BTreeMap<_, _> = f
        .store
        .unassigned_workspaces()
        .unwrap()
        .into_iter()
        .map(|u| (u.workspace.id, u.reason))
        .collect();
    assert_eq!(reasons[&orphan], UnassignedReason::ExplicitlyUnassigned);
    assert_eq!(reasons[&absent], UnassignedReason::ProfileAbsent);
    assert_eq!(reasons[&held], UnassignedReason::HolderPlaced);
    assert_eq!(reasons.len(), 3);
}

#[test]
fn holders_are_refused_everywhere_a_profile_lifecycle_is_written() {
    let mut f = Root::new();
    let holder = Profile::holder(Locale::default(), at()).unwrap();
    f.catalog().put_profile(&holder).unwrap();
    assert_eq!(
        f.store
            .profile_deletion_preview(holder.id)
            .unwrap_err()
            .code(),
        Code::ProfileReserved
    );
    let fake = koru_store_core::service::ProfileDeletionPreview {
        targets: Vec::new(),
        placement_revision: 0,
    };
    assert_eq!(
        f.store
            .delete_profile(holder.id, &fake, at())
            .unwrap_err()
            .code(),
        Code::ProfileReserved
    );
    assert_eq!(
        f.store
            .restore_profile(holder.id, DeletionId::new())
            .unwrap_err()
            .code(),
        Code::ProfileReserved
    );
    assert!(f.profile_unit(holder.id).is_none(), "and gains no unit");
}

// ------------------------------------------------------------- purge

#[test]
fn purge_retires_this_scopes_personal_row_first_and_keeps_the_units_evidence() {
    let mut f = Root::new();
    let a = f.profile("A");
    let w = f.store.create_workspace(a, "W", at()).unwrap().id;
    let (_, rule) = super::root_catalog::reminder_in(w);
    f.store.create_reminder(a, &rule).unwrap();
    let scope = f.scope();
    assert!(
        f.catalog()
            .workspace_personal_unit(scope, w)
            .unwrap()
            .is_some()
    );
    f.store.delete_workspace(w, at()).unwrap();
    // Not expired yet: nothing happens, the personal row is untouched.
    assert_eq!(
        f.store.purge_deleted(at()).unwrap(),
        koru_store_core::service::PurgeOutcome::default()
    );
    assert!(
        f.catalog()
            .workspace_personal_unit(scope, w)
            .unwrap()
            .is_some()
    );

    let outcome = f.store.purge_deleted(after_retention(at())).unwrap();
    assert_eq!(outcome.purged, vec![w]);
    assert!(
        f.catalog()
            .workspace_personal_unit(scope, w)
            .unwrap()
            .is_none(),
        "retired before the row went"
    );
    assert_eq!(f.count("SELECT COUNT(*) FROM workspace_reminder_due"), 0);
    assert!(f.store.workspace_including_deleted(w).is_err());
    assert_eq!(f.backend.0.deleted.borrow().as_slice(), &[w]);
    let unit = f.workspace_unit(w);
    assert_eq!(unit.purged().len(), 1, "the evidence stays on the unit");
    assert!(!unit.is_live());
    // A second pass finds nothing left to do.
    assert_eq!(
        f.store.purge_deleted(after_retention(at())).unwrap(),
        koru_store_core::service::PurgeOutcome::default()
    );
}

#[test]
fn a_foreign_scope_personal_unit_holds_only_its_own_workspace() {
    let mut f = Root::new();
    let a = f.profile("A");
    let mine = f.store.create_workspace(a, "Mine", at()).unwrap().id;
    let theirs = f.store.create_workspace(a, "Theirs", at()).unwrap().id;
    // Another user's personal unit on `theirs`, as a shared installation
    // would hold it. Nothing of ours is authorized to retire it.
    let other = UserScopeId::new();
    f.catalog()
        .commit_workspace_personal_unit(
            other,
            theirs,
            &koru_store_core::profile_units::PersonalManifest::create_v2(theirs, 5)
                .unwrap()
                .export_snapshot()
                .unwrap(),
            5,
        )
        .unwrap();
    f.store.delete_workspace(mine, at()).unwrap();
    f.store.delete_workspace(theirs, at()).unwrap();

    let outcome = f.store.purge_deleted(after_retention(at())).unwrap();
    assert_eq!(outcome.purged, vec![mine]);
    assert_eq!(outcome.held, vec![(theirs, PurgeHold::ForeignScope)]);
    assert!(outcome.pending.is_empty());
    assert!(
        f.store.workspace_including_deleted(theirs).is_ok(),
        "content kept"
    );
    assert!(
        f.catalog()
            .workspace_personal_unit(other, theirs)
            .unwrap()
            .is_some(),
        "never deleted around the guard"
    );
    assert_eq!(
        f.workspace_unit(theirs).purged().len(),
        1,
        "the intent is recorded; the content waits"
    );
    assert_eq!(f.backend.0.deleted.borrow().as_slice(), &[mine]);
    // The hold is reported again, not escalated, on the next pass.
    let again = f.store.purge_deleted(after_retention(at())).unwrap();
    assert!(again.purged.is_empty());
    assert_eq!(again.held, vec![(theirs, PurgeHold::ForeignScope)]);
}

#[test]
fn a_failing_step_leaves_that_workspace_pending_while_the_others_purge_and_a_retry_completes_it() {
    let mut f = Root::new();
    let a = f.profile("A");
    let fine = f.store.create_workspace(a, "Fine", at()).unwrap().id;
    let stuck = f.store.create_workspace(a, "Stuck", at()).unwrap().id;
    f.store.delete_workspace(fine, at()).unwrap();
    f.store.delete_workspace(stuck, at()).unwrap();
    // Step 5 fails for one Workspace only.
    f.control
        .execute_batch(&format!(
            "CREATE TRIGGER fail BEFORE DELETE ON workspaces WHEN OLD.id = '{}' \
             BEGIN SELECT RAISE(ABORT, 'injected fault'); END;",
            stuck.to_storage()
        ))
        .unwrap();

    let outcome = f.store.purge_deleted(after_retention(at())).unwrap();
    assert_eq!(outcome.purged, vec![fine]);
    assert_eq!(outcome.pending, vec![(stuck, Code::StorageFailed)]);
    assert!(f.store.workspace_including_deleted(stuck).is_ok());
    assert_eq!(
        f.workspace_unit(stuck).purged().len(),
        1,
        "the intent was committed before the failing step"
    );
    assert_eq!(f.backend.0.deleted.borrow().len(), 2, "bytes went for both");

    f.recover();
    let retry = f.store.purge_deleted(after_retention(at())).unwrap();
    assert_eq!(retry.purged, vec![stuck]);
    assert!(retry.pending.is_empty());
    assert!(f.store.workspace_including_deleted(stuck).is_err());
    assert_eq!(
        f.workspace_unit(stuck).purged().len(),
        1,
        "the retry resumed from step 2 and wrote no second fact"
    );
}

#[test]
fn an_expired_profile_deletion_gains_purge_evidence_once_every_target_is_purged() {
    let mut f = Root::new();
    let p = f.cloud_profile("P");
    let w = f.store.create_workspace(p, "W", at()).unwrap().id;
    let preview = f.store.profile_deletion_preview(p).unwrap();
    let batch = f.store.delete_profile(p, &preview, at()).unwrap();

    let outcome = f.store.purge_deleted(after_retention(at())).unwrap();
    assert_eq!(outcome.purged, vec![w]);
    assert_eq!(outcome.profiles_purged, vec![p]);
    let unit = f.profile_unit(p).unwrap();
    assert_eq!(unit.purged().len(), 1);
    assert!(f.store.profile_deletions(at()).unwrap().is_empty());
    assert!(matches!(
        f.job(batch.batch_id).phase,
        koru_store_core::service::BatchPhase::Purged { .. }
    ));
    // The rows stay: the Profile is retained, hidden, and named for the
    // keepers of data beside it.
    assert!(f.catalog().profile_including_removed(p).unwrap().is_some());
    assert_eq!(
        f.store
            .profiles_removed_before(after_retention(at()))
            .unwrap(),
        vec![p]
    );
    assert_eq!(
        f.store
            .restore_profile(p, batch.profile_deletion)
            .unwrap_err()
            .code(),
        Code::VersionUnavailable
    );
}
