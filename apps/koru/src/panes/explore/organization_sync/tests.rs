// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_agent_wire::RootSyncSubject;
use koru_i18n::{Lang, t};

fn subject(
    peer: &str,
    profile: Option<ProfileId>,
    workspace: Option<WorkspaceId>,
    state: RootSyncState,
) -> RootSyncSubject {
    RootSyncSubject {
        peer_device_id: peer.into(),
        profile_id: profile.map(|id| id.to_string()),
        workspace_id: workspace.map(|id| id.to_string()),
        state,
    }
}

/// **A mixed inventory reads right per row** (Plan 2.2's verification): a
/// Local-only Profile is paused by this device's settings, a P2P Profile
/// whose units all arrived is up to date, a Cloud Profile whose root unit is
/// still owed is pending, a Workspace whose stored choice sits above its
/// Profile's ceiling is blocked, and one a peer keeps back is held.
#[test]
fn a_mixed_inventory_reads_one_state_per_row() {
    let local_only = ProfileId::new();
    let p2p = ProfileId::new();
    let cloud = ProfileId::new();
    let p2p_workspace = WorkspaceId::new();
    let mismatched = WorkspaceId::new();
    let held = WorkspaceId::new();
    let status = RootSyncStatus {
        subjects: vec![
            subject(
                "laptop",
                Some(local_only),
                None,
                RootSyncState::PausedBySettings,
            ),
            subject("laptop", Some(p2p), None, RootSyncState::UpToDate),
            subject("laptop", None, Some(p2p_workspace), RootSyncState::UpToDate),
            subject("laptop", Some(cloud), None, RootSyncState::Pending),
            subject(
                "laptop",
                None,
                Some(mismatched),
                RootSyncState::BlockedByMismatch,
            ),
            subject("laptop", None, Some(held), RootSyncState::Held),
        ],
    };
    let readings = OrganizationReadings::from_status(&status);
    assert_eq!(
        readings.profile(local_only, []),
        Some(OrganizationSync::PausedBySettings)
    );
    assert_eq!(
        readings.profile(p2p, [p2p_workspace]),
        Some(OrganizationSync::UpToDate)
    );
    assert_eq!(readings.profile(cloud, []), Some(OrganizationSync::Pending));
    assert_eq!(
        readings.workspace(mismatched),
        Some(OrganizationSync::BlockedByMismatch)
    );
    assert_eq!(readings.workspace(held), Some(OrganizationSync::Held));
    assert_eq!(
        readings.workspace(WorkspaceId::new()),
        None,
        "unknown says nothing"
    );
    assert_eq!(readings.profile(ProfileId::new(), []), None);
}

/// **Up to date is never claimed while an eligible unit is unapplied**: a
/// Profile whose own unit is applied but whose Workspace's placement is
/// still pending reads pending; and across two linked devices the worse
/// reading wins, so a third device that is up to date does not hide the
/// second one still catching up.
#[test]
fn a_profile_inherits_its_workspaces_and_the_worse_peer_wins() {
    let profile = ProfileId::new();
    let placed = WorkspaceId::new();
    let other = WorkspaceId::new();
    let status = RootSyncStatus {
        subjects: vec![
            subject("laptop", Some(profile), None, RootSyncState::UpToDate),
            subject("laptop", None, Some(placed), RootSyncState::Pending),
            subject("phone", None, Some(placed), RootSyncState::UpToDate),
            subject("phone", None, Some(other), RootSyncState::UpToDate),
        ],
    };
    let readings = OrganizationReadings::from_status(&status);
    assert_eq!(readings.workspace(placed), Some(OrganizationSync::Pending));
    assert_eq!(
        readings.profile(profile, [placed, other]),
        Some(OrganizationSync::Pending)
    );
    // A Workspace from another Profile does not drag this one down.
    assert_eq!(
        readings.profile(profile, [other]),
        Some(OrganizationSync::UpToDate)
    );
}

/// The surviving replica (Plan 2.2 / 2.1): with the original device gone,
/// the one peer left is the whole truth, and it says up to date.
#[test]
fn a_surviving_replica_reads_as_ordinary_up_to_date() {
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    let status = RootSyncStatus {
        subjects: vec![
            subject("second", Some(profile), None, RootSyncState::UpToDate),
            subject("second", None, Some(workspace), RootSyncState::UpToDate),
        ],
    };
    let readings = OrganizationReadings::from_status(&status);
    assert_eq!(
        readings.profile(profile, [workspace]),
        Some(OrganizationSync::UpToDate)
    );
}

/// **No rendered word or sentence names a hidden Profile.** The status layer
/// is fed a sentinel standing for a Profile this device must not learn
/// about — as its name and as its id — and every string any state renders,
/// in every language, is checked for both. A `Held` reading is about the
/// Workspace this device asked for; the sentence never says what hides it.
#[test]
fn no_rendered_status_string_names_a_hidden_profile() {
    let hidden_name = "SENTINEL-HIDDEN-PROFILE";
    let hidden = ProfileId::new();
    let asked = WorkspaceId::new();
    let status = RootSyncStatus {
        subjects: vec![subject(hidden_name, None, Some(asked), RootSyncState::Held)],
    };
    let readings = OrganizationReadings::from_status(&status);
    assert_eq!(readings.workspace(asked), Some(OrganizationSync::Held));
    for reading in [
        OrganizationSync::UpToDate,
        OrganizationSync::Pending,
        OrganizationSync::Held,
        OrganizationSync::PausedBySettings,
        OrganizationSync::BlockedByMismatch,
    ] {
        let status = super::super::SyncStatus::from_organization(reading);
        for lang in Lang::ALL {
            let word = t(lang, status.key());
            assert!(!word.contains(hidden_name) && !word.contains(&hidden.to_string()));
            if let Some(hint) = status.hint() {
                let sentence = t(lang, hint);
                assert!(
                    !sentence.contains(hidden_name) && !sentence.contains(&hidden.to_string()),
                    "{reading:?} in {} names the hidden subject",
                    lang.tag()
                );
            }
        }
    }
    // The readings themselves hold only the subject this device asked for.
    assert!(readings.profiles.is_empty());
    assert_eq!(readings.workspaces.len(), 1);
}

#[test]
fn the_worse_reading_wins_in_a_fixed_order() {
    use OrganizationSync::*;
    assert_eq!(UpToDate.worse(Pending), Pending);
    assert_eq!(Pending.worse(Held), Held);
    assert_eq!(Held.worse(PausedBySettings), PausedBySettings);
    assert_eq!(PausedBySettings.worse(BlockedByMismatch), BlockedByMismatch);
    assert_eq!(BlockedByMismatch.worse(UpToDate), BlockedByMismatch);
}
