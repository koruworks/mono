// SPDX-License-Identifier: MPL-2.0

//! Memory-only: the repair's operation identity and the journal record it
//! writes, over in-memory Loro documents. No catalog, no disk, no Store — the
//! Store-level behaviour (which rows are candidates, the atomicity, the
//! restart) is in `koru-store-sqlite`'s `Root` fixture against a real catalog.

use super::*;
use crate::ids::ProfileId;

/// The Store's writer. Every durable unit is authored under it.
const STORE: u64 = 4_242;

fn placement(profile: ProfileId) -> Placement {
    Placement {
        profile: Some(profile),
        move_id: Uuid::new_v4(),
    }
}

/// The repair's write, for a Workspace with neither a durable unit nor an
/// overlay — the only state a candidate is ever in.
fn repair_write(workspace: WorkspaceId, profile: ProfileId, admitted: bool) -> RootUnitWrite {
    plan_placement_write(workspace, None, None, admitted, STORE, placement(profile)).unwrap()
}

#[test]
fn the_repair_operation_is_the_single_placement_write_under_the_store_writer() {
    let workspace = WorkspaceId::new();
    let write = repair_write(workspace, ProfileId::new(), true);

    let (peer, counter) = repair_operation(&write, workspace, STORE).unwrap();

    assert_eq!(peer, STORE, "a durable write is authored by the Store");
    let RootUnitWrite::Durable { commit, .. } = &write else {
        panic!("an admitted destination writes the durable unit");
    };
    let unit = WorkspaceCatalogUnit::from_snapshot(&commit.snapshot, workspace, STORE).unwrap();
    // A version-vector counter is an exclusive end, so the last operation's
    // counter is one less; and the placement commit is that last operation.
    assert_eq!(
        counter,
        unit.version().get_last(STORE).unwrap(),
        "the identity is version[writer] - 1, not the exclusive end"
    );
    assert_eq!(
        loro::Frontiers::from_id(loro::ID::new(peer, counter)),
        frontiers_of(&commit.snapshot),
        "equivalently, the oplog frontiers right after the commit is that one id"
    );
}

/// The planned document's own frontiers, to check the two readings agree.
fn frontiers_of(snapshot: &[u8]) -> loro::Frontiers {
    let doc = loro::LoroDoc::new();
    doc.import(snapshot).unwrap();
    doc.oplog_frontiers()
}

#[test]
fn a_local_only_hint_profile_identifies_the_operation_in_the_private_overlay() {
    let workspace = WorkspaceId::new();
    let write = repair_write(workspace, ProfileId::new(), false);

    let (peer, counter) = repair_operation(&write, workspace, STORE).unwrap();

    let RootUnitWrite::Overlay(overlay) = &write else {
        panic!("a Local-only or fenced destination writes the overlay");
    };
    assert_eq!(
        peer, overlay.writer_peer_id,
        "the private fork has its own writer, and the record names that one"
    );
    assert_ne!(peer, STORE);
    assert!(counter >= 0);
    assert_eq!(
        loro::Frontiers::from_id(loro::ID::new(peer, counter)),
        frontiers_of(&overlay.private_snapshot),
    );
}

#[test]
fn the_record_round_trips_through_the_journal_and_rebuilds_the_operation() {
    let workspace = WorkspaceId::new();
    let profile = ProfileId::new();
    let write = repair_write(workspace, profile, true);
    let (writer_peer, counter) = repair_operation(&write, workspace, STORE).unwrap();
    let repair = BootstrapRepair {
        profile,
        move_id: Uuid::new_v4(),
        writer_peer,
        counter,
        at_ms: 1_700_000_000_000,
    };

    let json = serde_json::to_string(&repair).unwrap();
    let back: BootstrapRepair = serde_json::from_str(&json).unwrap();

    assert_eq!(back, repair);
    assert_eq!(back.operation(), loro::ID::new(writer_peer, counter));
    assert_eq!(back.frontiers(), loro::Frontiers::from_id(back.operation()));
}

#[test]
fn the_journal_key_is_one_row_per_workspace_under_the_root_catalog_prefix() {
    let (a, b) = (WorkspaceId::new(), WorkspaceId::new());

    assert!(bootstrap_repair_key(a).starts_with(K_BOOTSTRAP_REPAIR_PREFIX));
    assert!(K_BOOTSTRAP_REPAIR_PREFIX.starts_with("root_catalog."));
    assert_ne!(bootstrap_repair_key(a), bootstrap_repair_key(b));
    assert_eq!(bootstrap_repair_key(a), bootstrap_repair_key(a));
}

#[test]
fn every_repair_mints_its_own_move_id_so_two_devices_never_agree_by_accident() {
    let workspace = WorkspaceId::new();
    let profile = ProfileId::new();

    let one = repair_write(workspace, profile, true);
    let two = repair_write(workspace, profile, true);

    let read = |write: &RootUnitWrite| {
        let RootUnitWrite::Durable { commit, .. } = write else {
            unreachable!("admitted")
        };
        WorkspaceCatalogUnit::from_snapshot(&commit.snapshot, workspace, STORE)
            .unwrap()
            .placement()
            .unwrap()
    };
    let (first, second) = (read(&one), read(&two));
    assert_eq!(first.profile, Some(profile));
    assert_eq!(second.profile, Some(profile));
    assert_ne!(
        first.move_id, second.move_id,
        "the repair never uses the deterministic seed the migration and peer provisioning use"
    );
}
