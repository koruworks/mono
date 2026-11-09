// SPDX-License-Identifier: MPL-2.0

//! Memory-only: the job record, the retention arithmetic and the lifecycle
//! closures handed to the shared planner, over in-memory Loro documents. The
//! Store-level behavior (one transaction, projections, restart) is held in
//! `koru-store-sqlite`'s named-memory tests beside the real catalog.

use super::*;
use crate::ports::{RootCatalogUnitRow, RootUnitWrite};

const STORE: u64 = 4_242;

fn placed(workspace: WorkspaceId, profile: ProfileId) -> RootCatalogUnitRow {
    let unit = WorkspaceCatalogUnit::create(workspace, STORE).unwrap();
    unit.set_placement(Some(profile), uuid::Uuid::new_v4())
        .unwrap();
    RootCatalogUnitRow {
        snapshot: unit.snapshot().unwrap(),
        revision: 1,
    }
}

fn durable_snapshot(write: RootUnitWrite) -> Vec<u8> {
    match write {
        RootUnitWrite::Durable { commit, .. } => commit.snapshot,
        other => panic!("expected a durable write, got {other:?}"),
    }
}

#[test]
fn the_job_key_is_the_prefix_and_the_batch_id_and_the_record_round_trips() {
    let batch = BatchId::new();
    let key = batch_job_key(batch);
    assert!(key.starts_with(K_ROOT_CATALOG_BATCH_PREFIX));
    assert!(key.ends_with(&batch.to_storage()));

    let job = ProfileDeletionJob {
        batch_id: batch,
        profile: ProfileId::new(),
        profile_deletion: DeletionId::new(),
        at_ms: 7,
        retention_ms: TRASH_RETENTION_MS,
        targets: vec![(WorkspaceId::new(), DeletionId::new())],
        phase: BatchPhase::Deleted,
    };
    let json = serde_json::to_string(&job).unwrap();
    assert!(json.contains("\"phase\":\"deleted\""));
    let back: ProfileDeletionJob = serde_json::from_str(&json).unwrap();
    assert_eq!(back, job);

    let restored = ProfileDeletionJob {
        phase: BatchPhase::Restored {
            restore: RestoreId::new(),
            at_ms: 9,
        },
        ..job
    };
    let back: ProfileDeletionJob =
        serde_json::from_str(&serde_json::to_string(&restored).unwrap()).unwrap();
    assert_eq!(back, restored);
}

#[test]
fn expiry_is_the_facts_own_retention_not_a_global_clock() {
    let thirty_days = DeletionRecord::new(1_000, None);
    assert_eq!(expires_at(&thirty_days), 1_000 + TRASH_RETENTION_MS);
    let short = DeletionRecord {
        at_ms: 1_000,
        retention_ms: 5,
        batch_id: None,
    };
    assert_eq!(expires_at(&short), 1_005);
    let saturating = DeletionRecord {
        at_ms: i64::MAX,
        retention_ms: 1,
        batch_id: None,
    };
    assert_eq!(expires_at(&saturating), i64::MAX);
}

#[test]
fn a_deletion_fact_through_the_shared_planner_lands_on_the_durable_unit_with_its_batch() {
    let workspace = WorkspaceId::new();
    let held = placed(workspace, ProfileId::new());
    let batch = BatchId::new();
    let deletion = DeletionId::new();
    let record = DeletionRecord::new(50, Some(batch.as_uuid()));

    let write = plan_unit_write::<WorkspaceCatalogUnit>(
        CatalogUnitRef::Workspace { workspace },
        Some(&held),
        None,
        true,
        STORE,
        |unit| Ok(unit.record_deletion(deletion, &record)?),
    )
    .unwrap();
    let unit = WorkspaceCatalogUnit::from_snapshot(&durable_snapshot(write), workspace, 1).unwrap();
    assert!(unit.is_deleted());
    assert_eq!(
        unit.active_deletions().into_iter().collect::<Vec<_>>(),
        [deletion]
    );
    assert_eq!(unit.deletions()[&deletion], record);
    assert_eq!(
        earliest_active_deletion(&unit),
        Some(Timestamp::from_millis(50))
    );
}

#[test]
fn a_restore_observing_only_the_batch_deletion_leaves_an_earlier_one_active() {
    let workspace = WorkspaceId::new();
    let unit = WorkspaceCatalogUnit::create(workspace, STORE).unwrap();
    unit.set_placement(Some(ProfileId::new()), uuid::Uuid::new_v4())
        .unwrap();
    let earlier = DeletionId::new();
    unit.record_deletion(earlier, &DeletionRecord::new(10, None))
        .unwrap();
    let batch = BatchId::new();
    let from_batch = DeletionId::new();
    unit.record_deletion(from_batch, &DeletionRecord::new(20, Some(batch.as_uuid())))
        .unwrap();
    let held = RootCatalogUnitRow {
        snapshot: unit.snapshot().unwrap(),
        revision: 3,
    };

    let record = RestoreRecord {
        at_ms: 30,
        observed_deletions: vec![from_batch],
        batch_id: Some(batch.as_uuid()),
    };
    let write = plan_unit_write::<WorkspaceCatalogUnit>(
        CatalogUnitRef::Workspace { workspace },
        Some(&held),
        None,
        true,
        STORE,
        |unit| Ok(unit.record_restore(RestoreId::new(), &record)?),
    )
    .unwrap();
    let after =
        WorkspaceCatalogUnit::from_snapshot(&durable_snapshot(write), workspace, 1).unwrap();
    assert_eq!(
        after.active_deletions().into_iter().collect::<Vec<_>>(),
        [earlier],
        "the earlier independent deletion stays in the Trash"
    );
    assert_eq!(
        earliest_active_deletion(&after),
        Some(Timestamp::from_millis(10))
    );
}

#[test]
fn a_profile_unit_takes_lifecycle_facts_through_the_same_planner_and_may_wait_in_an_overlay() {
    let profile = ProfileId::new();
    let held = RootCatalogUnitRow {
        snapshot: ProfileCatalogUnit::create(profile, STORE)
            .unwrap()
            .snapshot()
            .unwrap(),
        revision: 1,
    };
    let deletion = DeletionId::new();
    let record = DeletionRecord::new(5, Some(uuid::Uuid::new_v4()));

    // Not admitted: the fact goes to a private fork and the durable bytes
    // are what they were.
    let write = plan_unit_write::<ProfileCatalogUnit>(
        CatalogUnitRef::Profile { profile },
        Some(&held),
        None,
        false,
        STORE,
        |unit| Ok(unit.record_deletion(deletion, &record)?),
    )
    .unwrap();
    let RootUnitWrite::Overlay(overlay) = write else {
        panic!("a Local-only Profile writes its overlay");
    };
    assert_ne!(overlay.writer_peer_id, STORE);
    let private = ProfileCatalogUnit::from_snapshot(&overlay.private_snapshot, profile, 1).unwrap();
    assert!(private.is_deleted());
    let durable = ProfileCatalogUnit::from_snapshot(&held.snapshot, profile, 1).unwrap();
    assert!(durable.is_live());

    // Admitted later: the overlay folds into the durable unit and the
    // deletion is still exactly one fact under its id.
    let write = plan_unit_write::<ProfileCatalogUnit>(
        CatalogUnitRef::Profile { profile },
        Some(&held),
        Some(&overlay),
        true,
        STORE,
        |_| Ok(()),
    )
    .unwrap();
    let RootUnitWrite::Durable {
        commit,
        clear_overlay,
    } = write
    else {
        panic!("admitted writes are durable");
    };
    assert!(clear_overlay);
    let folded = ProfileCatalogUnit::from_snapshot(&commit.snapshot, profile, 1).unwrap();
    assert_eq!(folded.deletions().len(), 1);
    assert!(folded.is_deleted());
}

#[test]
fn the_wrong_family_is_refused_before_any_byte_is_authored() {
    let error = plan_unit_write::<ProfileCatalogUnit>(
        CatalogUnitRef::Workspace {
            workspace: WorkspaceId::new(),
        },
        None,
        None,
        true,
        STORE,
        |_| Ok(()),
    )
    .unwrap_err();
    assert_eq!(error.code(), Code::InvalidRequest);
}
