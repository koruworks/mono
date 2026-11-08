// SPDX-License-Identifier: MPL-2.0

use super::*;

fn profile(n: u8) -> ProfileId {
    ProfileId::from_uuid(Uuid::from_bytes([
        n, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, n,
    ]))
}

fn candidate(profile_n: u8, workspace_n: u8) -> CloudWorkspaceCandidate {
    CloudWorkspaceCandidate {
        profile_id: profile(profile_n),
        workspace_id: WorkspaceId::from_uuid(Uuid::from_bytes([
            workspace_n,
            1,
            0,
            0,
            0,
            0,
            0x40,
            0,
            0x80,
            0,
            0,
            0,
            0,
            0,
            0,
            workspace_n,
        ])),
        workspace_incarnation_id: Uuid::from_u128(u128::from(workspace_n)),
        documents_write: true,
        authorization_version: 1,
        membership_epoch: 1,
        cloud_access_epoch: 1,
        manifest_document_id: None,
    }
}

fn decision(n: u8, access: ProfileAccess) -> ProfileDecision {
    ProfileDecision {
        profile_id: profile(n),
        access,
    }
}

/// F4: allow Work, refuse Personal, set the later flag, then a third Profile
/// turns Cloud on. Personal stays out, the third appears, and an explicit
/// `not_allowed` is never reopened by the flag.
#[test]
fn allowed_profiles_and_the_later_flag_resolve_exactly() {
    let work = 1;
    let personal = 2;
    let third = 3;
    let decisions = [
        decision(work, ProfileAccess::Allowed),
        decision(personal, ProfileAccess::NotAllowed),
    ];
    let before = vec![
        candidate(work, 10),
        candidate(work, 11),
        candidate(personal, 20),
    ];
    assert_eq!(
        resolve_workspaces(before.clone(), &decisions, true),
        vec![candidate(work, 10), candidate(work, 11)]
    );
    let after = vec![
        candidate(work, 10),
        candidate(personal, 20),
        candidate(third, 30),
    ];
    assert_eq!(
        resolve_workspaces(after.clone(), &decisions, true),
        vec![candidate(work, 10), candidate(third, 30)]
    );
    // Without the flag, an undecided Profile stays out.
    assert_eq!(
        resolve_workspaces(after, &decisions, false),
        vec![candidate(work, 10)]
    );
    // Nothing decided and no flag: nothing.
    assert!(resolve_workspaces(before, &[], false).is_empty());
}

#[test]
fn an_entitlement_ends_at_its_boundary() {
    assert!(CloudEntitlement::Active { until_millis: None }.is_active_at(i64::MAX));
    let until = CloudEntitlement::Active {
        until_millis: Some(100),
    };
    assert!(until.is_active_at(99));
    assert!(!until.is_active_at(100));
    assert!(!CloudEntitlement::None.is_active_at(0));
}
