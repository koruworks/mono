// SPDX-License-Identifier: MPL-2.0

//! What the controller asks the window to do, one at a time.
//!
//! Each flow parks its request in a slot of its own; [`ProfilesController::
//! take_effect`] drains them in one fixed order, the order the window always
//! asked in. **The order matters where two are ready at once**: a page to open
//! goes before any Store or Agent work, local Store work (the
//! compare-and-set bind, removal, cleanup) goes before the next Agent command
//! — several flows chain *Agent result → local write → Agent re-read*, and the
//! re-read must not overtake the write it follows.
use super::*;
use koru_workspace_host::account_ui::CommandSubmissionId;

/// One thing for the window to carry out.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// A koru.do page: the Cloud card's subscribe, manage or learn-more link.
    OpenWebsite(String),
    /// Any other link: a support chat, the embedded form's terms.
    OpenUri(&'static str),
    /// Create a Profile from the embedded form, for the form's account.
    CreateConnectedProfile {
        owner_scope: u64,
        draft: ConnectedProfileDraft,
    },
    /// Bring a Profile the account retains back onto this device.
    RestoreLocalProfile(ProfileId),
    /// Open this Profile in the window.
    SwitchProfile(ProfileId),
    /// Bind a local Profile to an account identity, by compare-and-set.
    BindAccount {
        profile: ProfileId,
        expected_identity_id: Option<String>,
        new_identity_id: String,
    },
    /// Remove a Profile from this device, with its Workspaces when asked.
    /// With `preview` — the deletion preview the person confirmed over —
    /// the shared deletion batch (root-catalog §6.1); without it, the
    /// device-local removal.
    RemoveLocalProfile {
        profile: ProfileId,
        delete_workspaces: bool,
        preview: Option<koru_api_client::ProfileDeletionPreview>,
    },
    /// The hosted Profile is deleted; finish removing it here.
    FinishHostedDeletion {
        profile: ProfileId,
        discovered_after_offline: bool,
    },
    /// A Profile deleted elsewhere stays here, unbound and offline-only.
    ContinueDeletedOffline {
        profile: ProfileId,
        expected_identity_id: String,
    },
    /// An Agent account command, owned by the form that raised it and tracked
    /// by `submission` when that form waits on its admission.
    Account {
        owner_scope: u64,
        command: AccountCommand,
        submission: Option<CommandSubmissionId>,
    },
}

impl ProfilesController {
    /// The next thing for the window to do, in the order described above.
    pub fn take_effect(&mut self) -> Option<Effect> {
        if let Some(url) = self.take_cloud_uri() {
            return Some(Effect::OpenWebsite(url));
        }
        if let Some(uri) = self.take_uri_request() {
            return Some(Effect::OpenUri(uri));
        }
        if let Some(draft) = self.take_profile_creation_request() {
            return Some(Effect::CreateConnectedProfile {
                owner_scope: self.account_scope(),
                draft,
            });
        }
        if let Some(profile) = self.take_profile_restore_request() {
            return Some(Effect::RestoreLocalProfile(profile));
        }
        if let Some(profile) = self.take_profile_switch_request() {
            return Some(Effect::SwitchProfile(profile));
        }
        if let Some((profile, expected_identity_id, new_identity_id)) = self.take_bind_request() {
            return Some(Effect::BindAccount {
                profile,
                expected_identity_id,
                new_identity_id,
            });
        }
        if let Some((profile, delete_workspaces, preview)) = self.take_remove_request() {
            return Some(Effect::RemoveLocalProfile {
                profile,
                delete_workspaces,
                preview,
            });
        }
        if let Some((profile, discovered_after_offline)) = self.take_hosted_delete_request() {
            return Some(Effect::FinishHostedDeletion {
                profile,
                discovered_after_offline,
            });
        }
        if let Some((profile, expected_identity_id)) = self.take_continue_offline_request() {
            return Some(Effect::ContinueDeletedOffline {
                profile,
                expected_identity_id,
            });
        }
        let (command, submission) = self.take_request_with_submission()?;
        Some(Effect::Account {
            owner_scope: self.account_scope(),
            command,
            submission,
        })
    }

    // ---- the slots, one at a time ------------------------------------------
    //
    // `take_effect` is the way out. These drain a single slot, which is what a
    // test that follows one flow wants: the order between slots is
    // `take_effect`'s business, and is tested there.

    /// The next Agent command alone, without its submission.
    pub(crate) fn take_request(&mut self) -> Option<AccountCommand> {
        self.pending.take().or_else(|| self.take_cloud_request())
    }

    pub(crate) fn take_request_with_submission(
        &mut self,
    ) -> Option<(AccountCommand, Option<CommandSubmissionId>)> {
        let command = self.take_request()?;
        if let Some(submission) = self.cloud_submission(&command) {
            return Some((command, Some(submission)));
        }
        if let Some(submission) = self.attach_submission(&command) {
            return Some((command, Some(submission)));
        }
        if matches!(command, AccountCommand::SelectProfiles { .. })
            && let Some(connect) = &self.inline_connect
        {
            return Some((command, Some(connect.submission)));
        }
        if let Some(submission) = self
            .profile_creation
            .as_ref()
            .and_then(|view| view.borrow().command_submission(&command))
        {
            return Some((command, Some(submission)));
        }
        let submission = matches!(
            command,
            AccountCommand::ConfirmAccountCreation
                | AccountCommand::CancelAccountCreation
                | AccountCommand::Retry
        )
        .then_some(self.creation_submission)
        .flatten();
        Some((command, submission))
    }

    pub(crate) fn take_cloud_uri(&mut self) -> Option<String> {
        self.cloud_uri.take()
    }

    pub(crate) fn take_uri_request(&mut self) -> Option<&'static str> {
        self.uri_request.take()
    }

    pub(crate) fn take_profile_creation_request(&mut self) -> Option<ConnectedProfileDraft> {
        self.profile_creation_request.take()
    }

    pub(crate) fn take_profile_restore_request(&mut self) -> Option<ProfileId> {
        self.profile_restore_request.take()
    }

    pub(crate) fn take_profile_switch_request(&mut self) -> Option<ProfileId> {
        self.profile_switch_request.take()
    }

    pub(crate) fn take_bind_request(&mut self) -> Option<(ProfileId, Option<String>, String)> {
        match self.take_local_effect(|effect| matches!(effect, LocalEffect::Bind(..)))? {
            LocalEffect::Bind(id, expected, next) => Some((id, expected, next)),
            _ => unreachable!(),
        }
    }

    pub(crate) fn take_remove_request(
        &mut self,
    ) -> Option<(
        ProfileId,
        bool,
        Option<koru_api_client::ProfileDeletionPreview>,
    )> {
        match self.take_local_effect(|effect| matches!(effect, LocalEffect::Remove(..)))? {
            LocalEffect::Remove(id, cascade, preview) => Some((id, cascade, preview)),
            _ => unreachable!(),
        }
    }

    pub(crate) fn take_hosted_delete_request(&mut self) -> Option<(ProfileId, bool)> {
        match self.take_local_effect(|effect| matches!(effect, LocalEffect::HostedDelete(..)))? {
            LocalEffect::HostedDelete(id, discovered) => Some((id, discovered)),
            _ => unreachable!(),
        }
    }

    pub(crate) fn take_continue_offline_request(&mut self) -> Option<(ProfileId, String)> {
        match self.take_local_effect(|effect| matches!(effect, LocalEffect::Offline(..)))? {
            LocalEffect::Offline(id, expected) => Some((id, expected)),
            _ => unreachable!(),
        }
    }
}
