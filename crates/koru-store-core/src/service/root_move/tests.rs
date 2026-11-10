// SPDX-License-Identifier: MPL-2.0

//! Memory-only: the placement-write planner and the overlay fold over
//! in-memory Loro documents. No catalog, no disk, no Store.

use super::*;
use crate::ids::ProfileId;

/// The Store's writer. Every durable unit is authored under it.
const STORE: u64 = 4_242;

fn durable(workspace: WorkspaceId, profile: ProfileId, revision: i64) -> RootCatalogUnitRow {
    let unit = WorkspaceCatalogUnit::create(workspace, STORE).unwrap();
    unit.set_placement(Some(profile), uuid::Uuid::new_v4())
        .unwrap();
    RootCatalogUnitRow {
        snapshot: unit.snapshot().unwrap(),
        revision,
    }
}

fn to(profile: ProfileId) -> Placement {
    Placement {
        profile: Some(profile),
        move_id: uuid::Uuid::new_v4(),
    }
}

fn read(snapshot: &[u8], workspace: WorkspaceId) -> WorkspaceCatalogUnit {
    WorkspaceCatalogUnit::from_snapshot(snapshot, workspace, 1).unwrap()
}

fn overlay_of(write: RootUnitWrite) -> RootCatalogOverlay {
    match write {
        RootUnitWrite::Overlay(overlay) => overlay,
        other => panic!("expected an overlay write, got {other:?}"),
    }
}

fn durable_of(write: RootUnitWrite) -> (RootCatalogUnitCommit, bool) {
    match write {
        RootUnitWrite::Durable {
            commit,
            clear_overlay,
        } => (commit, clear_overlay),
        other => panic!("expected a durable write, got {other:?}"),
    }
}

#[test]
fn an_admitted_move_writes_the_durable_unit_under_the_store_writer() {
    let workspace = WorkspaceId::new();
    let (a, b) = (ProfileId::new(), ProfileId::new());
    let held = durable(workspace, a, 3);
    let placement = to(b);

    let (commit, clear) = durable_of(
        plan_placement_write(workspace, Some(&held), None, true, STORE, placement).unwrap(),
    );

    assert!(!clear);
    assert_eq!(commit.unit, CatalogUnitRef::Workspace { workspace });
    assert_eq!(
        commit.expected_revision,
        Some(3),
        "compare-and-set on the read revision"
    );
    assert_ne!(commit.snapshot, held.snapshot);
    let unit = read(&commit.snapshot, workspace);
    assert_eq!(unit.placement(), Some(placement));
    assert!(
        unit.version().get(&STORE).is_some(),
        "written by the Store's stream"
    );
    assert!(unit.is_live());
}

#[test]
fn an_admitted_move_of_a_unit_less_workspace_creates_its_unit() {
    let workspace = WorkspaceId::new();
    let placement = to(ProfileId::new());

    let (commit, clear) =
        durable_of(plan_placement_write(workspace, None, None, true, STORE, placement).unwrap());

    assert!(!clear);
    assert_eq!(
        commit.expected_revision, None,
        "a row that must not exist yet"
    );
    assert_eq!(
        read(&commit.snapshot, workspace).placement(),
        Some(placement)
    );
}

#[test]
fn a_private_destination_forks_an_overlay_and_leaves_the_durable_bytes_alone() {
    let workspace = WorkspaceId::new();
    let (a, b) = (ProfileId::new(), ProfileId::new());
    let held = durable(workspace, a, 1);
    let before = read(&held.snapshot, workspace).placement().unwrap();
    let placement = to(b);

    let overlay = overlay_of(
        plan_placement_write(workspace, Some(&held), None, false, STORE, placement).unwrap(),
    );

    // Decoded: two independently built `VersionVector` maps encode their peer
    // entries in their own iteration order, so only the value is the claim.
    assert_eq!(
        VersionVector::decode(&overlay.base_vv).unwrap(),
        read(&held.snapshot, workspace).version(),
        "forked at the durable unit's version"
    );
    assert_ne!(overlay.writer_peer_id, STORE, "its own persisted writer");
    let fork = read(&overlay.private_snapshot, workspace);
    assert_eq!(fork.placement(), Some(placement));
    assert!(fork.version().get(&overlay.writer_peer_id).is_some());
    // The durable row is not part of the write at all; what it holds still
    // reads as the old placement.
    assert_eq!(read(&held.snapshot, workspace).placement(), Some(before));
}

#[test]
fn a_private_move_keeps_editing_the_overlay_it_already_holds() {
    let workspace = WorkspaceId::new();
    let (a, b, c) = (ProfileId::new(), ProfileId::new(), ProfileId::new());
    let held = durable(workspace, a, 1);
    let first = overlay_of(
        plan_placement_write(workspace, Some(&held), None, false, STORE, to(b)).unwrap(),
    );
    let placement = to(c);

    let second = overlay_of(
        plan_placement_write(
            workspace,
            Some(&held),
            Some(&first),
            false,
            STORE,
            placement,
        )
        .unwrap(),
    );

    assert_eq!(second.writer_peer_id, first.writer_peer_id);
    assert_eq!(second.base_vv, first.base_vv);
    assert_eq!(
        read(&second.private_snapshot, workspace).placement(),
        Some(placement)
    );
}

#[test]
fn an_entirely_private_subject_lives_only_in_its_overlay_until_enabled() {
    let workspace = WorkspaceId::new();
    let placement = to(ProfileId::new());

    let overlay =
        overlay_of(plan_placement_write(workspace, None, None, false, STORE, placement).unwrap());
    assert_eq!(overlay.base_vv, VersionVector::new().encode());
    assert_eq!(
        read(&overlay.private_snapshot, workspace).placement(),
        Some(placement)
    );

    // Enabled later: its whole history becomes the durable unit, written by
    // the Store from here on.
    let candidate = fold_overlay::<WorkspaceCatalogUnit>(
        CatalogUnitRef::Workspace { workspace },
        None,
        &overlay,
        STORE,
    )
    .unwrap();
    assert_eq!(candidate.placement(), Some(placement));
    assert_eq!(candidate.writer_peer(), STORE);
    assert!(candidate.version().get(&overlay.writer_peer_id).is_some());
}

#[test]
fn an_admitted_move_folds_a_pending_overlay_into_the_durable_unit_and_clears_it() {
    let workspace = WorkspaceId::new();
    let (a, b, c) = (ProfileId::new(), ProfileId::new(), ProfileId::new());
    let held = durable(workspace, a, 1);
    let pending = overlay_of(
        plan_placement_write(workspace, Some(&held), None, false, STORE, to(b)).unwrap(),
    );
    let placement = to(c);

    let (commit, clear) = durable_of(
        plan_placement_write(
            workspace,
            Some(&held),
            Some(&pending),
            true,
            STORE,
            placement,
        )
        .unwrap(),
    );

    assert!(clear, "the overlay goes with the commit");
    assert_eq!(commit.expected_revision, Some(1));
    let unit = read(&commit.snapshot, workspace);
    assert_eq!(unit.placement(), Some(placement));
    // Both streams are in the history: the private move to `b` kept its
    // causal place and the new move to `c` was written on top of it.
    assert!(unit.version().get(&pending.writer_peer_id).is_some());
    assert!(unit.version().get(&STORE).is_some());
}

#[test]
fn publishing_an_overlay_without_a_new_move_keeps_its_placement() {
    let workspace = WorkspaceId::new();
    let (a, b) = (ProfileId::new(), ProfileId::new());
    let held = durable(workspace, a, 7);
    let pending = overlay_of(
        plan_placement_write(workspace, Some(&held), None, false, STORE, to(b)).unwrap(),
    );
    let private = read(&pending.private_snapshot, workspace).placement();

    let candidate = fold_overlay::<WorkspaceCatalogUnit>(
        CatalogUnitRef::Workspace { workspace },
        Some(&held),
        &pending,
        STORE,
    )
    .unwrap();

    assert_eq!(candidate.placement(), private);
    assert_eq!(candidate.writer_peer(), STORE);
}

#[test]
fn an_undecodable_overlay_base_is_refused_rather_than_guessed() {
    let workspace = WorkspaceId::new();
    let held = durable(workspace, ProfileId::new(), 1);
    let mut pending = overlay_of(
        plan_placement_write(
            workspace,
            Some(&held),
            None,
            false,
            STORE,
            to(ProfileId::new()),
        )
        .unwrap(),
    );
    pending.base_vv = b"not a version vector".to_vec();

    assert_eq!(
        fold_overlay::<WorkspaceCatalogUnit>(
            CatalogUnitRef::Workspace { workspace },
            Some(&held),
            &pending,
            STORE
        )
        .unwrap_err()
        .code(),
        Code::CorruptState
    );
}
