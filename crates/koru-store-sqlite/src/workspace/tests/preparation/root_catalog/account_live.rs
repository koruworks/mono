// SPDX-License-Identifier: MPL-2.0

//! The fact *live in the account, in the Trash here* (trash-recovery
//! PLAN.final P2): what one observation records, that it is replaced as a
//! set, and that it counts only while its Profile is here and is that
//! account's.

use super::*;
use koru_store_core::ports::PurgeHold;
use koru_store_core::profile_account::ProfileAccountBinding;

fn account() -> Uuid {
    UserScopeId::new().as_uuid()
}

/// A Cloud Profile on this device that is `account`'s: made here, then
/// attached to the account, so it has its identity as well as its binding.
fn account_profile(f: &mut Root, account: Uuid, name: &str) -> ProfileId {
    let id = f.cloud_profile(name);
    let binding = ProfileAccountBinding::parse_identity_id(&account.to_string()).unwrap();
    f.store.bind_profile_account(id, None, binding).unwrap();
    id
}

/// A Workspace trashed under a peer-to-peer Profile: its deletion cannot
/// leave over the Cloud path.
fn stuck_in_the_trash(f: &mut Root) -> WorkspaceId {
    let local = f.profile("Local");
    f.set(TransportSubject::Profile(local), 1, TransportMode::P2p);
    let workspace = f.store.create_workspace(local, "Stuck", at()).unwrap().id;
    f.store.delete_workspace(workspace, at()).unwrap();
    workspace
}

fn listed(f: &Root) -> Vec<(WorkspaceId, ProfileId)> {
    f.store
        .trashed_in_account()
        .unwrap()
        .into_iter()
        .map(|row| (row.workspace.id, row.profile))
        .collect()
}

#[test]
fn only_a_deletion_that_cannot_leave_is_recorded_and_every_trashed_one_is_answered() {
    let mut f = Root::new();
    let account = account();
    let kept = account_profile(&mut f, account, "Kept");
    let stuck = stuck_in_the_trash(&mut f);
    // Trashed under the syncing Profile itself: its deletion is on its way
    // to the account, which lists it live only until the next pass.
    let leaving = f.store.create_workspace(kept, "Leaving", at()).unwrap().id;
    f.store.delete_workspace(leaving, at()).unwrap();
    let live = f.store.create_workspace(kept, "Live", at()).unwrap().id;

    let trashed = f
        .store
        .reconcile_account_live(
            account,
            &[(stuck, kept), (leaving, kept), (live, kept)],
            at(),
        )
        .unwrap();

    assert!(trashed.changed, "a fact was written");
    assert_eq!(
        trashed.trashed,
        [stuck, leaving].into_iter().collect(),
        "neither is admitted for content"
    );
    assert_eq!(
        listed(&f),
        vec![(stuck, kept)],
        "but only the one that cannot leave is this case"
    );
}

#[test]
fn the_set_is_replaced_and_a_fact_seen_again_keeps_what_it_knew() {
    let mut f = Root::new();
    let account = account();
    let kept = account_profile(&mut f, account, "Kept");
    let stuck = stuck_in_the_trash(&mut f);
    let first = Timestamp::from_millis(at().millis() + 1);
    let later = Timestamp::from_millis(at().millis() + 9_000);
    f.store
        .reconcile_account_live(account, &[(stuck, kept)], first)
        .unwrap();
    assert!(
        f.store
            .acknowledge_trashed_in_account(stuck, first)
            .unwrap()
    );

    let again = f
        .store
        .reconcile_account_live(account, &[(stuck, kept)], later)
        .unwrap();
    assert!(!again.changed, "nothing for a reader to read again");
    let row = f.store.trashed_in_account().unwrap().remove(0);
    assert_eq!(
        (row.first_seen_ms, row.acknowledged_ms),
        (first.millis(), Some(first.millis())),
        "the same observation again changes nothing"
    );

    // Another account's observation says nothing about this account's facts.
    f.store
        .reconcile_account_live(self::account(), &[], later)
        .unwrap();
    assert_eq!(listed(&f), vec![(stuck, kept)]);

    // This account no longer lists it live: the fact goes with the set.
    f.store.reconcile_account_live(account, &[], later).unwrap();
    assert!(listed(&f).is_empty());
    assert!(
        !f.store
            .acknowledge_trashed_in_account(stuck, later)
            .unwrap()
    );
}

#[test]
fn the_fact_counts_only_while_its_profile_is_here_and_is_that_accounts() {
    let mut f = Root::new();
    let account = account();
    let kept = account_profile(&mut f, account, "Kept");
    let stuck = stuck_in_the_trash(&mut f);
    f.store
        .reconcile_account_live(account, &[(stuck, kept)], at())
        .unwrap();
    assert_eq!(listed(&f), vec![(stuck, kept)]);

    // Continue offline: the Profile is no longer the account's.
    let binding = ProfileAccountBinding::parse_identity_id(&account.to_string()).unwrap();
    f.store.clear_profile_account(kept, binding).unwrap();
    assert!(listed(&f).is_empty(), "inert without anything clearing it");
}

#[test]
fn restoring_it_into_the_profile_ends_the_fact() {
    let mut f = Root::new();
    let account = account();
    let kept = account_profile(&mut f, account, "Kept");
    let stuck = stuck_in_the_trash(&mut f);
    f.store
        .reconcile_account_live(account, &[(stuck, kept)], at())
        .unwrap();

    f.store.restore_workspace_to(stuck, kept).unwrap();

    assert!(listed(&f).is_empty());
    assert_eq!(f.store.workspace(stuck).unwrap().owner, kept);
    assert!(
        f.store
            .reconcile_account_live(account, &[(stuck, kept)], at())
            .unwrap()
            .trashed
            .is_empty(),
        "live again: admitted like any other"
    );
}

/// **The purge waits** (trash-recovery PLAN.final P3). A deletion past its
/// retention is held while the fact counts, with nothing written — no purge
/// evidence, so it is still restorable — and purged on the next call once
/// the fact is gone or its Profile is no longer the account's.
#[test]
fn an_expired_deletion_the_account_still_holds_is_held_with_nothing_written() {
    let long_ago = Timestamp::from_millis(1);
    let expired_in_the_trash = |f: &mut Root| {
        let local = f.profile("Local");
        f.set(TransportSubject::Profile(local), 1, TransportMode::P2p);
        let workspace = f.store.create_workspace(local, "Old", at()).unwrap().id;
        f.store.delete_workspace(workspace, long_ago).unwrap();
        workspace
    };
    let purge_evidence = |f: &Root, workspace: WorkspaceId| {
        let held = f.unit(workspace).expect("a durable unit");
        !WorkspaceCatalogUnit::from_snapshot(&held.snapshot, workspace, 1)
            .unwrap()
            .purged()
            .is_empty()
    };

    // Held while the account lists it; purged once it no longer does.
    let mut f = Root::new();
    let account = account();
    let kept = account_profile(&mut f, account, "Kept");
    let workspace = expired_in_the_trash(&mut f);
    f.store
        .reconcile_account_live(account, &[(workspace, kept)], at())
        .unwrap();

    let outcome = f.store.purge_deleted(at()).unwrap();
    assert_eq!(outcome.held, vec![(workspace, PurgeHold::AccountLive)]);
    assert!(outcome.purged.is_empty());
    assert!(!purge_evidence(&f, workspace), "no purge fact was written");
    assert!(
        f.store.workspace_including_deleted(workspace).is_ok(),
        "its row and content are untouched"
    );
    f.store
        .restore_workspace_to(workspace, kept)
        .expect("and it is still restorable");

    // The same, with the fact removed by the next observation.
    let mut f = Root::new();
    let kept = account_profile(&mut f, account, "Kept");
    let workspace = expired_in_the_trash(&mut f);
    f.store
        .reconcile_account_live(account, &[(workspace, kept)], at())
        .unwrap();
    assert!(f.store.purge_deleted(at()).unwrap().purged.is_empty());
    f.store.reconcile_account_live(account, &[], at()).unwrap();
    assert_eq!(f.store.purge_deleted(at()).unwrap().purged, vec![workspace]);

    // And with the fact left in place but its Profile detached.
    let mut f = Root::new();
    let kept = account_profile(&mut f, account, "Kept");
    let workspace = expired_in_the_trash(&mut f);
    f.store
        .reconcile_account_live(account, &[(workspace, kept)], at())
        .unwrap();
    let binding = ProfileAccountBinding::parse_identity_id(&account.to_string()).unwrap();
    f.store.clear_profile_account(kept, binding).unwrap();
    assert_eq!(f.store.purge_deleted(at()).unwrap().purged, vec![workspace]);
}

/// A Profile deletion batch whose target the account still holds is not
/// purged either (P3): the target keeps its row, so the batch is not all
/// gone, and the Profile's own purge evidence is not written.
#[test]
fn a_profile_deletion_batch_with_a_held_target_is_not_purged() {
    let long_ago = Timestamp::from_millis(1);
    let mut f = Root::new();
    let account = account();
    let kept = account_profile(&mut f, account, "Kept");
    let gone = f.profile("Gone");
    f.set(TransportSubject::Profile(gone), 1, TransportMode::P2p);
    let workspace = f.store.create_workspace(gone, "Old", at()).unwrap().id;
    let preview = f.store.profile_deletion_preview(gone).unwrap();
    f.store.delete_profile(gone, &preview, long_ago).unwrap();
    f.store
        .reconcile_account_live(account, &[(workspace, kept)], at())
        .unwrap();
    assert_eq!(listed(&f), vec![(workspace, kept)], "the fixture");

    let outcome = f.store.purge_deleted(at()).unwrap();
    assert_eq!(outcome.held, vec![(workspace, PurgeHold::AccountLive)]);
    assert!(outcome.purged.is_empty());
    assert!(
        outcome.profiles_purged.is_empty(),
        "its target still has its row"
    );
    f.store
        .restore_workspace_to(workspace, kept)
        .expect("still restorable, into the account's Profile");
}
