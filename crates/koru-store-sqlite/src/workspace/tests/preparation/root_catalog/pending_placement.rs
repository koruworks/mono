// SPDX-License-Identifier: MPL-2.0

//! A move that arrives before the Profile it names. The unit takes the
//! placement and the `workspaces` row waits; these pin that the row lands
//! when the Profile does, and that a device already left behind catches up
//! the next time that unit is delivered, whatever the delivery carries.

use super::*;
use koru_store_core::ids::{DeletionId, RestoreId};
use koru_store_core::root_catalog::{DeletionRecord, RestoreRecord};

/// The Workspace's unit as another device holds it after moving it to `to`:
/// this device's durable history plus one placement written there.
fn moved_elsewhere(f: &Root, workspace: WorkspaceId, to: ProfileId) -> WorkspaceCatalogUnit {
    let held = f.unit(workspace).expect("the workspace has a unit");
    let unit = WorkspaceCatalogUnit::from_snapshot(&held.snapshot, workspace, 9_001).unwrap();
    unit.set_placement(Some(to), DocumentId::new().as_uuid())
        .unwrap();
    unit
}

fn owner(f: &Root, workspace: WorkspaceId) -> ProfileId {
    f.store
        .workspace_including_deleted(workspace)
        .unwrap()
        .owner
}

/// A Profile another device created, not yet on this one.
fn not_here_yet(name: &str) -> Profile {
    Profile::new(name, Locale::default(), at()).unwrap()
}

/// A Workspace under `home` whose unit says `later`, with `later` absent:
/// the state an arriving move leaves behind.
fn pending(f: &mut Root) -> (ProfileId, WorkspaceId, Profile, Uuid, WorkspaceCatalogUnit) {
    let (home, workspace) = f.cloud_pair();
    let account = bound_account(f);
    let later = not_here_yet("B");
    let moved = moved_elsewhere(f, workspace, later.id);
    arrives(f, account, workspace, &[moved.snapshot().unwrap()]);
    assert_eq!(
        f.store.root_placement(workspace).unwrap().unwrap().profile,
        Some(later.id),
        "the unit took the move"
    );
    assert_eq!(
        owner(f, workspace),
        home,
        "the row waits for a Profile that is not here"
    );
    (home, workspace, later, account, moved)
}

#[test]
fn a_move_that_arrived_before_its_profile_lands_when_the_profile_arrives() {
    let mut f = Root::new();
    let (_, workspace, later, _, _) = pending(&mut f);

    f.store.provision_replica_profile(&later).unwrap();

    assert_eq!(owner(&f, workspace), later.id);
    assert_eq!(
        f.store.current_placement_profile(workspace).unwrap(),
        Some(later.id)
    );
}

#[test]
fn a_placement_stays_pending_while_its_profile_is_still_absent() {
    let mut f = Root::new();
    let (home, workspace, _, account, moved) = pending(&mut f);

    // The same unit again: nothing new, and still nowhere to list it.
    arrives(&mut f, account, workspace, &[moved.snapshot().unwrap()]);

    assert_eq!(owner(&f, workspace), home);
}

/// The device this was found on: the Profile was already here and the row
/// had never been looked at again. Written behind the Store's back, because
/// every way the Store itself takes a Profile now applies what was pending.
fn left_behind(f: &mut Root) -> (WorkspaceId, Profile, Uuid, WorkspaceCatalogUnit) {
    let (home, workspace, later, account, moved) = pending(f);
    f.catalog().put_profile(&later).unwrap();
    assert_eq!(owner(f, workspace), home, "nothing has caught it up yet");
    (workspace, later, account, moved)
}

#[test]
fn a_row_left_behind_catches_up_when_the_same_unit_is_delivered_again() {
    let mut f = Root::new();
    let (workspace, later, account, moved) = left_behind(&mut f);

    arrives(&mut f, account, workspace, &[moved.snapshot().unwrap()]);

    assert_eq!(owner(&f, workspace), later.id);
}

#[test]
fn a_row_left_behind_catches_up_on_an_update_that_leaves_the_placement_alone() {
    let mut f = Root::new();
    let (workspace, later, account, moved) = left_behind(&mut f);
    // Trashed and restored on the other device: new operations, the same
    // placement and the same lifecycle. The import has no difference to
    // project, which is exactly when the row used to be skipped.
    let deletion = DeletionId::new();
    moved
        .record_deletion(deletion, &DeletionRecord::new(at().millis(), None))
        .unwrap();
    moved
        .record_restore(
            RestoreId::new(),
            &RestoreRecord {
                at_ms: at().millis(),
                observed_deletions: vec![deletion],
                batch_id: None,
            },
        )
        .unwrap();

    arrives(&mut f, account, workspace, &[moved.snapshot().unwrap()]);

    assert_eq!(owner(&f, workspace), later.id);
    assert!(
        f.store
            .workspace_including_deleted(workspace)
            .unwrap()
            .deleted_at
            .is_none()
    );
}
