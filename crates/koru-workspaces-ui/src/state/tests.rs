// SPDX-License-Identifier: MPL-2.0

//! Where a row belongs, and what the three states mean.

use super::*;

fn row(pinned: bool, archived: bool, deleted: bool) -> Row {
    Row {
        pinned,
        archived,
        deleted,
        ..Row::default()
    }
}

/// **Deleted outranks archived, which outranks pinned.**
///
/// A workspace can be all three at once — archive and soft-deletion are
/// independent flags, and pinning survives both — so the order is a decision
/// rather than a coincidence. The trash wins because somebody looking for a
/// workspace they removed will look there, and a pinned row sitting in the
/// active list while it is in the trash would be the app disagreeing with
/// itself in two places on the same screen.
#[test]
fn a_row_that_is_several_things_at_once_is_grouped_by_the_most_consequential() {
    assert_eq!(Group::of(&row(true, true, true)), Group::Deleted);
    assert_eq!(Group::of(&row(true, true, false)), Group::Archived);
    assert_eq!(Group::of(&row(true, false, false)), Group::Pinned);
    assert_eq!(Group::of(&row(false, false, false)), Group::Active);
}

/// **Three availabilities, not two.** Never having heard is not the same as
/// having been cut off: the first draws nothing and says it is looking, the
/// second draws the last catalog it had and says it is stale (S9).
#[test]
fn loading_is_told_apart_from_gone() {
    assert!(Availability::Loading.is_loading());
    assert!(!Availability::Loading.is_down());
    assert!(Availability::Down.is_down());
    assert!(!Availability::Live.is_down());
    assert_eq!(Availability::default(), Availability::Loading);
}

/// **Unknown is not permission.** A surface that has not asked yet must not
/// offer a destructive action, because the answer may be no — and dimming a
/// control after somebody pressed it is worse than dimming it before.
#[test]
fn authorization_is_refused_until_it_is_granted() {
    assert!(!Authorization::Unknown.may_destroy());
    assert!(!Authorization::Refused.may_destroy());
    assert!(Authorization::Vouched.may_destroy());
    assert_eq!(Authorization::default(), Authorization::Unknown);
}

/// A notice offers a retry only when running the same thing again is the right
/// move. Everything else either cannot be helped by pressing again or never got
/// as far as doing anything.
#[test]
fn only_a_notice_carrying_an_operation_is_retryable() {
    let plain = Notice {
        text: "no".into(),
        retry: None,
    };
    assert!(!plain.is_retryable());
    let s24 = Notice {
        text: "partial".into(),
        retry: Some(Retry::Lifecycle(
            WorkspaceId::default(),
            WorkspaceLifecycleAction::SoftDelete,
        )),
    };
    assert!(s24.is_retryable());
}

/// **An Unassigned row's hint follows its reason** (root-catalog Plan 2.2,
/// §7.2): from a deleted Profile it says so and offers the restore; a
/// Profile that has not arrived from another device yet is *pending*, not
/// an error; anything else is plainly unassigned.
#[test]
fn an_unassigned_rows_hint_follows_its_reason() {
    use koru_api_client::UnassignedReason;
    let row = |reason, former: Option<ProfileId>| UnassignedRow {
        id: WorkspaceId::new(),
        name: "Orphan".into(),
        icon: None,
        color: None,
        former_profile: former,
        reason,
    };
    let former = ProfileId::new();
    let deleted = row(UnassignedReason::ProfileDeleted, Some(former));
    assert!(deleted.from_deleted_profile());
    assert_eq!(
        deleted.hint_key(),
        koru_i18n::Key::WorkspacesUnassignedHeading
    );
    let pending = row(UnassignedReason::ProfileAbsent, Some(former));
    assert!(!pending.from_deleted_profile());
    assert_eq!(
        pending.hint_key(),
        koru_i18n::Key::WorkspacesUnassignedProfilePending
    );
    assert_eq!(
        row(UnassignedReason::ExplicitlyUnassigned, None).hint_key(),
        koru_i18n::Key::WorkspacesUnassignedNoProfile
    );
    assert_eq!(
        row(UnassignedReason::HolderPlaced, Some(former)).hint_key(),
        koru_i18n::Key::WorkspacesUnassignedNoProfile
    );
}
