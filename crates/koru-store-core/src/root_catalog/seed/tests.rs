// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::service::TRASH_RETENTION_MS;

#[test]
fn seeded_ids_are_deterministic_and_distinct_per_subject_and_time() {
    let a = WorkspaceId::new();
    let b = WorkspaceId::new();
    assert_eq!(seeded_move_id(a), seeded_move_id(a));
    assert_ne!(seeded_move_id(a), seeded_move_id(b));
    assert_eq!(seeded_deletion_id(a, 5), seeded_deletion_id(a, 5));
    assert_ne!(seeded_deletion_id(a, 5), seeded_deletion_id(a, 6));
    assert_ne!(seeded_deletion_id(a, 5), seeded_deletion_id(b, 5));
    // A move id and a deletion id of one Workspace never coincide.
    assert_ne!(seeded_move_id(a), seeded_deletion_id(a, 0).as_uuid());
    assert_eq!(seeded_move_id(a).get_version(), Some(uuid::Version::Sha1));
    assert_eq!(
        ROOT_SEED_NAMESPACE.to_string(),
        "5e1d9c42-7a3b-4f8e-9d2c-6b1f0a8d3e57"
    );
}

#[test]
fn two_devices_seeding_the_same_legacy_rows_agree_on_every_fact() {
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    let deleted_at = 1_770_000_000_000;
    for (writer_a, writer_b) in [(11, 11), (11, 22)] {
        let a = WorkspaceCatalogUnit::from_snapshot(
            &seed_workspace_unit(workspace, profile, Some(deleted_at), writer_a).unwrap(),
            workspace,
            writer_a,
        )
        .unwrap();
        let b = WorkspaceCatalogUnit::from_snapshot(
            &seed_workspace_unit(workspace, profile, Some(deleted_at), writer_b).unwrap(),
            workspace,
            writer_b,
        )
        .unwrap();
        assert_eq!(a.placement(), b.placement());
        assert_eq!(a.deletions(), b.deletions());
        let placement = a.placement().unwrap();
        assert_eq!(placement.profile, Some(profile));
        assert_eq!(placement.move_id, seeded_move_id(workspace));
        let deletions = a.deletions();
        assert_eq!(deletions.len(), 1);
        let (id, record) = deletions.iter().next().unwrap();
        assert_eq!(*id, seeded_deletion_id(workspace, deleted_at));
        assert_eq!(record.at_ms, deleted_at);
        assert_eq!(record.retention_ms, TRASH_RETENTION_MS);
        assert_eq!(record.batch_id, None);
        assert!(a.is_deleted());
        // The two seeds merge into one fact, not two.
        let mut merged = a;
        merged
            .import_frames(&[b.snapshot().unwrap()])
            .unwrap()
            .applied()
            .unwrap();
        assert_eq!(merged.deletions().len(), 1);
        assert_eq!(merged.placement(), b.placement());
    }
}

#[test]
fn a_live_workspace_seed_has_a_placement_and_no_deletion() {
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    let unit = WorkspaceCatalogUnit::from_snapshot(
        &seed_workspace_unit(workspace, profile, None, 7).unwrap(),
        workspace,
        7,
    )
    .unwrap();
    assert!(unit.is_live());
    assert!(unit.deletions().is_empty());
    assert_eq!(unit.placement().unwrap().profile, Some(profile));
    assert_eq!(unit.writer_peer(), 7);
}

#[test]
fn a_profile_seed_is_live_whatever_its_local_removal_says() {
    // `removed_at` is device-local suppression (§2.1); the seed takes no
    // removal argument at all, so there is nothing to turn into a deletion.
    let profile = ProfileId::new();
    let unit =
        ProfileCatalogUnit::from_snapshot(&seed_profile_unit(profile, 3).unwrap(), profile, 3)
            .unwrap();
    assert!(unit.is_live());
    assert!(unit.deletions().is_empty());
    assert_eq!(unit.profile(), profile);
}

#[test]
fn receipts_round_trip_as_json_and_name_the_holder_exclusion() {
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    for receipt in [
        RootSeedReceipt::Profile {
            profile,
            writer_peer_id: u64::MAX >> 1,
            legacy_removed_at_ms: Some(9),
        },
        RootSeedReceipt::HolderExcluded { profile },
        RootSeedReceipt::Workspace {
            workspace,
            writer_peer_id: 5,
            placement_profile: profile,
            move_id: seeded_move_id(workspace),
            deleted_at_ms: Some(4),
            deletion_id: Some(seeded_deletion_id(workspace, 4)),
            owner_is_holder: false,
        },
    ] {
        let text = serde_json::to_string(&receipt).unwrap();
        assert_eq!(
            serde_json::from_str::<RootSeedReceipt>(&text).unwrap(),
            receipt
        );
    }
    let excluded = serde_json::to_value(RootSeedReceipt::HolderExcluded { profile }).unwrap();
    assert_eq!(excluded["seed"], "holder_excluded");
}
