// SPDX-License-Identifier: MPL-2.0

//! `open_from_store` needs a running store, so its real coverage lives in
//! `apps/koru-store-server/tests/document_roundtrip.rs` — `CARGO_BIN_EXE_koru-store-server`
//! only exists in tests of the package that declares the binary.
//!
//! What is left here is the part that needs no store at all.

#[test]
fn opening_requires_applied_live_authority_and_settled_listing() {
    use koru_store_core::{
        model::{Timestamp, Workspace},
        service::{WorkspaceAuthority, WorkspaceAuthorityState},
    };
    let owner = koru_store_core::ProfileId::new();
    let live = WorkspaceAuthority {
        workspace: Workspace::new(owner, "Live", Timestamp::from_millis(1)).unwrap(),
        authority: WorkspaceAuthorityState::Read {
            applied: true,
            archived: false,
            deleted_at: None,
            pinned: false,
        },
        listing_differs: false,
    };
    let mut pending = live.clone();
    pending.authority = WorkspaceAuthorityState::Read {
        applied: false,
        archived: false,
        deleted_at: None,
        pinned: false,
    };
    let mut archived = live.clone();
    archived.authority = WorkspaceAuthorityState::Read {
        applied: true,
        archived: true,
        deleted_at: None,
        pinned: false,
    };
    let mut deleted = live.clone();
    deleted.authority = WorkspaceAuthorityState::Read {
        applied: true,
        archived: false,
        deleted_at: Some(Timestamp::from_millis(2)),
        pinned: false,
    };
    let mut repair = live.clone();
    repair.listing_differs = true;
    let mut unreadable = live.clone();
    unreadable.authority = WorkspaceAuthorityState::Unreadable {
        cause: "missing".into(),
        recovery: None,
    };
    assert_eq!(
        super::eligible_workspaces(vec![
            pending,
            archived,
            deleted,
            repair,
            unreadable,
            live.clone()
        ]),
        vec![live.workspace]
    );
}

#[test]
fn explicit_profile_opens_first_active_workspace_while_startup_keeps_its_preference() {
    use koru_store_core::{
        ids::ProfileId,
        model::{Timestamp, Workspace},
    };
    let owner = ProfileId::new();
    let mut first = Workspace::new(owner, "First", Timestamp::from_millis(1)).unwrap();
    first.position = 0;
    let mut second = Workspace::new(owner, "Second", Timestamp::from_millis(2)).unwrap();
    second.position = 1;
    let mut archived = Workspace::new(owner, "Archived", Timestamp::from_millis(3)).unwrap();
    archived.position = -2;
    archived.archived = true;
    let mut deleted = Workspace::new(owner, "Deleted", Timestamp::from_millis(4)).unwrap();
    deleted.position = -1;
    deleted.deleted_at = Some(Timestamp::from_millis(5));
    let rows = [second.clone(), archived, deleted, first.clone()];
    assert_eq!(
        super::choose_workspace(&rows, None, true).unwrap().id,
        first.id
    );
    assert_eq!(
        super::choose_workspace(&rows, None, false).unwrap().id,
        second.id
    );
    let wanted = koru_core::WorkspaceId::from_uuid(second.id.as_uuid());
    assert_eq!(
        super::choose_workspace(&rows, Some(wanted), true)
            .unwrap()
            .id,
        second.id
    );
    assert!(super::choose_workspace(&rows[1..3], None, true).is_none());
}

/// The struct carries whether anything had to be created, and that is not the
/// same question as whether the workspace is empty — a brand-new store and a
/// store whose contents failed to load both list zero workspaces.
#[test]
fn needs_profile_owns_no_open_state() {
    assert!(super::StoreLaunch::NeedsProfile.into_open().is_none());
}

#[test]
fn a_failed_open_after_creation_ack_is_not_a_retryable_creation_error() {
    let profile = koru_core::ProfileId::new();
    let outcome = super::open_acknowledged_profile(profile, || {
        Err(koru_store_core::Error::new(
            koru_store_core::Code::StoreUnreachable,
            "strict open failed after acknowledgement",
        ))
    });

    match outcome {
        super::LocalProfileCreation::AcknowledgedButNotOpened {
            profile: acknowledged,
            error,
        } => {
            assert_eq!(acknowledged, profile);
            assert_eq!(error.code(), koru_store_core::Code::StoreUnreachable);
        }
        super::LocalProfileCreation::Open(_) => panic!("the failed strict open cannot be Open"),
    }
}

#[test]
fn catalog_refresh_replaces_a_transitional_workspace_name() {
    let owner = koru_core::ProfileId::new();
    let mut profile = koru_core::profile::ProfileDoc::create(owner, "Owner", None).unwrap();
    let mut workspace = koru_store_core::model::Workspace::new(
        koru_store_core::ids::ProfileId::from_uuid(owner.as_uuid()),
        "Before",
        koru_store_core::model::Timestamp::from_millis(1),
    )
    .unwrap();

    super::apply_workspace_catalog(&mut profile, owner, &[workspace.clone()]).unwrap();
    let id = koru_core::WorkspaceId::from_uuid(workspace.id.as_uuid());
    assert_eq!(profile.workspace(id).unwrap().name, "Before");

    workspace.name = "Renamed on mobile".into();
    super::apply_workspace_catalog(&mut profile, owner, &[workspace]).unwrap();
    assert_eq!(profile.workspace(id).unwrap().name, "Renamed on mobile");
}
#[test]
fn legacy_holders_never_enter_the_persona_catalog() {
    let at = koru_store_core::model::Timestamp::from_millis(1);
    let locale = koru_store_core::Locale::default();
    let persona = koru_store_core::model::Profile::new("Person", locale.clone(), at).unwrap();
    let holder = koru_store_core::model::Profile::holder(locale, at).unwrap();

    assert_eq!(
        super::personas_only(vec![holder, persona.clone()]),
        vec![persona]
    );
}
