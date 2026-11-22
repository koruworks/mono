// SPDX-License-Identifier: MPL-2.0

//! `Store::restore_workspace_to` (trash-recovery PLAN.final P1): a Workspace
//! trashed with a Profile that is now gone comes back under another Profile
//! in one operation — restored, placed, and syncing the way that Profile
//! does — and the reason it is one operation is pinned at the reducers.

use super::*;
use koru_store_core::ids::RestoreId;
use koru_store_core::root_catalog::{RestoreRecord, Staged};
use koru_store_core::transport::EffectiveTransportMode;

fn unit(f: &Root, workspace: WorkspaceId) -> WorkspaceCatalogUnit {
    let held = f.unit(workspace).expect("the workspace has a durable unit");
    WorkspaceCatalogUnit::from_snapshot(&held.snapshot, workspace, 1).unwrap()
}

fn mode(f: &Root, workspace: WorkspaceId) -> EffectiveTransportMode {
    f.store
        .transport_admission(TransportSubject::Workspace(workspace))
        .unwrap()
}

fn remove_from_device(f: &mut Root, profile: ProfileId) {
    let mark = f
        .store
        .begin_profile_removal(profile, RemovalKind::Local, at())
        .unwrap();
    f.store
        .remove_profile_local(profile, Some(mark.attempt), at())
        .unwrap();
}

/// A Workspace filed under a peer-to-peer Profile, so its own mode is that
/// Profile's by inheritance; then trashed, and the Profile removed from this
/// device. Beside it, a Cloud Profile to bring it back into.
fn trashed_with_its_profile(f: &mut Root) -> (WorkspaceId, ProfileId, ProfileId) {
    let gone = f.profile("Gone");
    f.set(TransportSubject::Profile(gone), 1, TransportMode::P2p);
    let workspace = f.store.create_workspace(gone, "W", at()).unwrap().id;
    let kept = f.cloud_profile("Kept");
    f.store.delete_workspace(workspace, at()).unwrap();
    remove_from_device(f, gone);
    (workspace, gone, kept)
}

fn account_live_key(workspace: WorkspaceId) -> String {
    format!("root_catalog.account_live.{}", workspace.to_storage())
}

#[test]
fn a_workspace_trashed_with_a_profile_since_removed_comes_back_under_another() {
    let mut f = Root::new();
    let (workspace, _, kept) = trashed_with_its_profile(&mut f);
    let before = mode(&f, workspace);
    assert_eq!(
        (before.stored_mode, before.explicit_choice),
        (TransportMode::P2p, false),
        "the mode it has is the gone Profile's, and nobody chose it"
    );
    assert_eq!(
        f.store.restore_workspace(workspace).unwrap_err().code(),
        Code::ProfileRemoved,
        "the ordinary restore has no Profile to restore it under"
    );
    f.catalog()
        .set_state(&account_live_key(workspace), "{}")
        .unwrap();

    let restored = f.store.restore_workspace_to(workspace, kept).unwrap();

    assert_eq!((restored.owner, restored.deleted_at), (kept, None));
    let row = f.store.workspace(workspace).expect("live again");
    assert_eq!(row.owner, kept);
    let held = unit(&f, workspace);
    assert!(
        held.active_deletions().is_empty(),
        "the deletion is observed"
    );
    assert_eq!(held.placement().unwrap().profile, Some(kept));
    assert!(
        f.overlay(workspace).is_none(),
        "written to the durable unit: it is what syncs"
    );
    let after = mode(&f, workspace);
    assert_eq!(
        (after.stored_mode, after.explicit_choice),
        (TransportMode::Cloud, false),
        "a mode nobody chose follows the Profile it came back into"
    );
    assert!(after.allows_cloud());
    assert!(
        after.revision > before.revision,
        "a new revision for the change"
    );
    assert_eq!(
        f.catalog().state(&account_live_key(workspace)).unwrap(),
        None,
        "nothing is left to say once it is back"
    );
}

#[test]
fn a_mode_somebody_chose_is_kept_and_decides_where_the_restore_is_written() {
    let mut f = Root::new();
    let gone = f.profile("Gone");
    f.set(TransportSubject::Profile(gone), 1, TransportMode::P2p);
    let peer_only = f.store.create_workspace(gone, "Peer", at()).unwrap().id;
    let closed = f.store.create_workspace(gone, "Closed", at()).unwrap().id;
    f.set(
        TransportSubject::Workspace(peer_only),
        1,
        TransportMode::P2p,
    );
    f.set(
        TransportSubject::Workspace(closed),
        1,
        TransportMode::LocalOnly,
    );
    let kept = f.cloud_profile("Kept");
    for workspace in [peer_only, closed] {
        f.store.delete_workspace(workspace, at()).unwrap();
    }
    remove_from_device(&mut f, gone);

    f.store.restore_workspace_to(peer_only, kept).unwrap();
    f.store.restore_workspace_to(closed, kept).unwrap();

    // Peer-to-peer by choice: back, in the durable unit, and still not Cloud.
    let chosen = mode(&f, peer_only);
    assert_eq!(
        (chosen.stored_mode, chosen.explicit_choice),
        (TransportMode::P2p, true)
    );
    assert!(!chosen.allows_cloud());
    assert!(f.overlay(peer_only).is_none());
    assert!(unit(&f, peer_only).active_deletions().is_empty());

    // Sync off by choice: back and filed where it was put, but privately —
    // nothing about the restore or the placement leaves this device.
    let off = mode(&f, closed);
    assert_eq!(
        (off.stored_mode, off.explicit_choice),
        (TransportMode::LocalOnly, true)
    );
    let row = f.store.workspace(closed).expect("live here");
    assert_eq!(row.owner, kept);
    assert_eq!(
        f.store.root_placement(closed).unwrap().unwrap().profile,
        Some(kept)
    );
    assert!(
        f.overlay(closed).is_some(),
        "kept in the private overlay, as any write to a closed subject is"
    );
}

#[test]
fn a_private_overlay_is_folded_into_the_durable_unit_not_left_behind() {
    let mut f = Root::new();
    // Under a Profile nobody opened, the Workspace's facts are private: its
    // deletion is written to the overlay.
    let quiet = f.profile("Quiet");
    let workspace = f.store.create_workspace(quiet, "W", at()).unwrap().id;
    f.store.delete_workspace(workspace, at()).unwrap();
    assert!(
        f.overlay(workspace).is_some(),
        "the fixture: a deletion held in the private overlay"
    );
    let kept = f.cloud_profile("Kept");
    remove_from_device(&mut f, quiet);

    f.store.restore_workspace_to(workspace, kept).unwrap();

    assert!(f.overlay(workspace).is_none(), "the overlay is spent");
    let held = unit(&f, workspace);
    assert!(held.active_deletions().is_empty());
    assert_eq!(held.placement().unwrap().profile, Some(kept));
    assert!(mode(&f, workspace).allows_cloud());
}

#[test]
fn the_destination_is_refused_as_a_move_refuses_it_and_a_live_workspace_is_just_moved() {
    let mut f = Root::new();
    let (workspace, gone, kept) = trashed_with_its_profile(&mut f);
    assert_eq!(
        f.store
            .restore_workspace_to(workspace, ProfileId::new())
            .unwrap_err()
            .code(),
        Code::ProfileNotFound,
        "a Profile that is not here"
    );
    assert_eq!(
        f.store
            .restore_workspace_to(workspace, gone)
            .unwrap_err()
            .code(),
        Code::ProfileNotFound,
        "a Profile removed from this device is no destination"
    );
    assert!(
        f.store.workspace(workspace).is_err(),
        "a refusal wrote nothing: still in the Trash"
    );

    // Not in the Trash: the same call is a move and writes no restore.
    let live = f.store.create_workspace(kept, "Live", at()).unwrap().id;
    let elsewhere = f.cloud_profile("Elsewhere");
    let moved = f.store.restore_workspace_to(live, elsewhere).unwrap();
    assert_eq!(moved.owner, elsewhere);
    assert!(unit(&f, live).restores().is_empty());
}

#[test]
fn a_workspace_whose_content_was_purged_is_not_brought_back_empty() {
    let mut f = Root::new();
    let gone = f.profile("Gone");
    f.set(TransportSubject::Profile(gone), 1, TransportMode::P2p);
    let workspace = f.store.create_workspace(gone, "W", at()).unwrap().id;
    let kept = f.cloud_profile("Kept");
    // Trashed long enough ago that its retention has run out.
    f.store
        .delete_workspace(workspace, Timestamp::from_millis(1))
        .unwrap();
    f.store.purge_deleted(at()).unwrap();

    assert_eq!(
        f.store
            .restore_workspace_to(workspace, kept)
            .unwrap_err()
            .code(),
        Code::WorkspaceNotFound
    );
}

/// **Why it is one operation.** The device's unit says *under a Profile now
/// gone, deleted*; the account's says *under another Profile, live* — two
/// seeds of one Workspace, as the root catalog's migration made them on two
/// devices that disagreed (2026-11-17). Joined as they are, the deletion
/// wins: an active deletion outranks a move (root-catalog TECHNICAL.final
/// §6.2), so opening the path between them, or moving the Workspace first,
/// trashes it in the account. Joined after the restore and the placement,
/// it is live under the account's Profile on both sides.
#[test]
fn joined_without_the_restore_the_account_loses_it_and_with_it_both_agree() {
    let workspace = WorkspaceId::new();
    let (gone, kept) = (ProfileId::new(), ProfileId::new());
    let seeded = |owner, deleted_at, writer| {
        let snapshot = seed_workspace_unit(workspace, owner, deleted_at, writer).unwrap();
        WorkspaceCatalogUnit::from_snapshot(&snapshot, workspace, writer).unwrap()
    };
    let joined = |into: &WorkspaceCatalogUnit, from: &WorkspaceCatalogUnit| match into
        .staged_import(&[from.snapshot().unwrap()])
        .unwrap()
    {
        Staged::Applied(unit) => unit,
        Staged::Pending => panic!("two whole units have every ancestor"),
    };
    let device = seeded(gone, Some(1_000), 11);
    let account = seeded(kept, None, 22);

    assert!(
        !joined(&account, &device).active_deletions().is_empty(),
        "the hazard: the account takes the device's deletion"
    );

    device
        .record_restore(
            RestoreId::new(),
            &RestoreRecord {
                at_ms: 2_000,
                observed_deletions: device.active_deletions().into_iter().collect(),
                batch_id: None,
            },
        )
        .unwrap();
    device
        .set_placement(Some(kept), DocumentId::new().as_uuid())
        .unwrap();

    for side in [joined(&account, &device), joined(&device, &account)] {
        assert!(side.active_deletions().is_empty());
        assert_eq!(side.placement().unwrap().profile, Some(kept));
    }
}

#[test]
fn the_accounts_own_unit_arriving_after_the_restore_leaves_it_live() {
    let mut f = Root::new();
    let (workspace, _, kept) = trashed_with_its_profile(&mut f);
    f.store.restore_workspace_to(workspace, kept).unwrap();
    let account = bound_account(&mut f);

    let theirs = seed_workspace_unit(workspace, kept, None, 9_002).unwrap();
    arrives(&mut f, account, workspace, &[theirs]);

    let row = f.store.workspace(workspace).expect("still live");
    assert_eq!(row.owner, kept);
    assert!(unit(&f, workspace).active_deletions().is_empty());
    assert_eq!(
        f.store.root_placement(workspace).unwrap().unwrap().profile,
        Some(kept)
    );
}
