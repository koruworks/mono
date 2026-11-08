// SPDX-License-Identifier: MPL-2.0

//! The state for permanent hosted Profile removal.
//!
//! Network and Store work stay outside this module. It owns which Profiles may
//! expose the action, the safe confirmation, the customer-facing lifecycle
//! stages, and the later-discovery decision copied into the controller.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostedDeleteStage {
    Inspecting,
    Deleting,
    Reauthenticating,
    CleanupRunning,
    CleanupFailed,
    Failed,
}

pub(super) struct PendingHostedDelete {
    pub(super) profile: ProfileId,
    pub(super) profile_name: String,
    pub(super) stage: HostedDeleteStage,
}

pub(super) struct DiscoveredHostedDeletion {
    pub(super) profile: ProfileId,
    pub(super) profile_name: String,
    pub(super) identity_id: String,
    pub(super) lifecycle_revision: u64,
}

pub(super) fn confirmation(profile: ProfileId, profile_name: &str) -> (ProfileId, Confirm) {
    let title = format!("Delete \u{201c}{profile_name}\u{201d} from Koru?");
    let body = concat!(
        "This removes the Profile from your Koru account and connected devices. ",
        "On this device its Workspaces move to Trash and its AI chats are kept for ",
        "30 days, then both are deleted. The Profile will stop syncing. Devices that ",
        "are offline will be asked what to do when they next sign in."
    );
    (profile, Confirm::new(title, body, "Delete from Koru"))
}

pub(super) fn can_delete(
    account_stage: AccountStage,
    selected: Option<ProfileId>,
    account_profiles: &[AccountProfileRow],
) -> bool {
    account_stage == AccountStage::SignedIn
        && selected.is_some_and(|selected| {
            let storage = selected.to_storage();
            account_profiles
                .iter()
                .find(|profile| profile.profile_id == storage)
                .is_some_and(|profile| !profile.auth_identities.is_empty())
        })
}

pub(super) fn pending_copy(pending: &PendingHostedDelete) -> (String, String) {
    match pending.stage {
        HostedDeleteStage::Failed => (
            "Koru couldn't delete this Profile".to_owned(),
            "Nothing was changed. Try again when Koru can connect, or remove this Profile from only this device."
                .to_owned(),
        ),
        HostedDeleteStage::CleanupRunning | HostedDeleteStage::CleanupFailed => (
            "Profile deleted from Koru".to_owned(),
            format!(
                "Koru deleted \u{201c}{}\u{201d} from your account, but still needs to finish removing it from this device. Your local data remains recoverable.",
                pending.profile_name
            ),
        ),
        HostedDeleteStage::Inspecting
        | HostedDeleteStage::Deleting
        | HostedDeleteStage::Reauthenticating => (
            "Deleting Profile from Koru...".to_owned(),
            "Koru is confirming the hosted deletion before changing anything on this device."
                .to_owned(),
        ),
    }
}
