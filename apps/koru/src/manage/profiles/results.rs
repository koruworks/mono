// SPDX-License-Identifier: MPL-2.0

//! What comes back: account results, command admission and failure, and the
//! local Store's acknowledgements of the requests this controller raised.
use super::*;

impl ProfilesController {
    pub fn account_command_failed(
        &mut self,
        failure: koru_workspace_host::account_ui::CommandFailure,
    ) -> bool {
        self.apply_to_forms(|form| form.account_command_failed_active(failure))
    }
    fn account_command_failed_active(
        &mut self,
        failure: koru_workspace_host::account_ui::CommandFailure,
    ) -> bool {
        if self.cloud_failed(failure) {
            return true;
        }
        if self.inline_connect_failed(failure) {
            return true;
        }
        if self.attach_failed(failure) {
            return true;
        }
        if self
            .profile_creation
            .as_mut()
            .is_some_and(|view| view.account_command_failed(failure))
        {
            return true;
        }
        if self.creation_submission != Some(failure.submission) || !self.creation_decision_pending {
            return false;
        }
        self.creation_decision_pending = false;
        self.account.stage = AccountStage::Unavailable;
        self.account.failure_code = Some("account_command_unavailable".into());
        self.account_warning_dismissed = false;
        if let Some(creation) = &mut self.profile_creation {
            creation.account_command_failed(failure);
        }
        true
    }
    pub fn account_command_admitted(
        &mut self,
        admission: koru_workspace_host::account_ui::CommandAdmission,
    ) -> bool {
        self.apply_to_forms(|form| {
            if form.cloud_admitted(admission) {
                return true;
            }
            if form.inline_connect_admitted(admission) {
                return true;
            }
            let changed = form
                .profile_creation
                .as_mut()
                .is_some_and(|creation| creation.account_command_admitted(admission));
            if changed {
                form.drain_profile_creation_action();
            }
            changed
        })
    }

    pub fn deleted_profile_continued_offline(&mut self, profile: ProfileId) -> bool {
        self.apply_to_forms(|form| form.deleted_profile_continued_offline_active(profile))
    }
    fn deleted_profile_continued_offline_active(&mut self, profile: ProfileId) -> bool {
        if self
            .discovered_hosted_delete
            .as_ref()
            .is_none_or(|pending| pending.profile != profile)
        {
            return false;
        }
        self.discovered_hosted_delete = None;
        if let Some(local) = self.profiles.iter_mut().find(|row| row.id == profile) {
            local.account = None;
        }
        self.notice = Some("Profile is now offline-only and will not sync.".into());
        // The resolver is bounded and may have returned more than one exact
        // tombstone. Re-read the now-smaller local binding inventory so each
        // Profile receives its own blocking decision.
        self.pending = Some(AccountCommand::ReconcileProfileLifecycles);
        true
    }

    pub fn deleted_profile_continue_offline_failed(&mut self, profile: ProfileId) -> bool {
        self.apply_to_forms(|form| form.deleted_profile_continue_offline_failed_active(profile))
    }
    fn deleted_profile_continue_offline_failed_active(&mut self, profile: ProfileId) -> bool {
        if self
            .discovered_hosted_delete
            .as_ref()
            .is_none_or(|pending| pending.profile != profile)
        {
            return false;
        }
        self.notice =
            Some("Koru couldn't make this Profile offline-only. Nothing was changed.".into());
        true
    }

    pub fn connected_profile_creation_failed(&mut self, message: &str) -> bool {
        let Some(creation) = self.profile_creation.as_mut() else {
            return false;
        };
        creation.creation_failed(message);
        true
    }

    pub fn local_profile_restore_failed(&mut self, message: &str) -> bool {
        let Some(creation) = self.profile_creation.as_mut() else {
            return false;
        };
        creation.restore_failed(message);
        true
    }

    pub fn local_profile_restored(&mut self, profile: ProfileId) -> bool {
        self.finish_local_profile_choice(profile, "Profile restored on this device")
    }

    pub fn local_profile_account_bound(&mut self, profile: ProfileId) -> bool {
        self.apply_to_forms(|form| form.local_profile_account_bound_active(profile))
    }
    fn local_profile_account_bound_active(&mut self, profile: ProfileId) -> bool {
        if self
            .pending_add_google
            .as_ref()
            .is_some_and(|candidate| candidate.profile_id == profile.to_storage())
        {
            self.pending_add_google = None;
            self.notice = Some("Google Account connected to this Profile.".into());
            self.pending = Some(AccountCommand::ListProfiles);
            return true;
        }
        if self
            .pending_rebind
            .as_ref()
            .is_some_and(|(pending, _)| *pending == profile)
        {
            self.pending_rebind = None;
            self.account_warning_dismissed = true;
            self.notice = Some("Profile reconnected to the new Koru account.".into());
            self.pending = Some(AccountCommand::ListProfiles);
            return true;
        }
        self.attach_bound(profile)
    }

    pub fn local_profile_account_bind_failed(&mut self, profile: ProfileId, code: &str) -> bool {
        self.apply_to_forms(|form| form.local_profile_account_bind_failed_active(profile, code))
    }
    fn local_profile_account_bind_failed_active(&mut self, profile: ProfileId, code: &str) -> bool {
        if self
            .pending_add_google
            .as_ref()
            .is_some_and(|candidate| candidate.profile_id == profile.to_storage())
        {
            self.pending_add_google = None;
            self.notice = Some(if code == "profile_account_conflict" {
                "This Profile is connected to another Koru account on this device. It remains available offline.".into()
            } else {
                "Koru could not save the account connection on this device. This Profile remains available offline.".into()
            });
            return true;
        }
        if self
            .pending_rebind
            .as_ref()
            .is_some_and(|(pending, _)| *pending == profile)
        {
            self.pending_rebind = None;
            self.notice = Some(if code == "profile_account_conflict" {
                "This Profile changed account connection on this device. Koru did not overwrite it; the Profile remains available offline.".into()
            } else {
                "Koru could not save the new account connection on this device. This Profile remains available offline.".into()
            });
            return true;
        }
        if self.attach_bind_failed(profile) {
            return true;
        }
        false
    }

    pub fn local_profile_removed(&mut self, profile: ProfileId) -> bool {
        self.retained_profiles.retain(|row| row.id != profile);
        self.apply_to_forms(|form| form.local_profile_removed_active(profile))
    }
    fn local_profile_removed_active(&mut self, profile: ProfileId) -> bool {
        let owns_choice = self.selected == Some(profile)
            || self
                .pending_hosted_delete
                .as_ref()
                .is_some_and(|pending| pending.profile == profile);
        self.profiles.retain(|row| row.id != profile);
        self.account_profiles
            .retain(|row| row.profile_id != profile.to_storage());
        self.workspace_counts.remove(&profile);
        if !owns_choice {
            return false;
        }
        self.confirm_remove = false;
        self.remove_all_workspaces = false;
        self.selected = self.profiles.first().map(|row| row.id);
        self.hints.clamp_scroll = true;
        self.notice = Some(
            koru_i18n::t(
                crate::format::lang(),
                koru_i18n::Key::ProfilesRemovedFromDevice,
            )
            .into(),
        );
        if self
            .pending_hosted_delete
            .as_ref()
            .is_some_and(|pending| pending.profile == profile)
        {
            self.pending_hosted_delete = None;
            self.notice = Some("Profile deleted from Koru and removed from this device.".into());
            self.pending = Some(AccountCommand::ReconcileProfileLifecycles);
        }
        true
    }

    pub fn local_profile_remove_failed(&mut self, code: &str) -> bool {
        self.confirm_remove = false;
        self.remove_all_workspaces = false;
        if self.pending_hosted_delete.as_ref().is_some_and(|pending| {
            pending.stage == HostedDeleteStage::CleanupRunning
                && self.selected == Some(pending.profile)
        }) {
            if let Some(pending) = self.pending_hosted_delete.as_mut() {
                pending.stage = HostedDeleteStage::CleanupFailed;
            }
            self.notice = Some("hosted_cleanup_failed".into());
            return true;
        }
        self.notice = Some(match code {
            "profile_not_empty" => "Delete all Workspaces in this Profile first.".into(),
            "profile_cascade_incomplete" => koru_i18n::t(
                crate::format::lang(),
                koru_i18n::Key::ProfilesRemoveCascadeIncomplete,
            )
            .into(),
            // Refused before anything moved, and refused again on every retry:
            // the window holds no first-party grant (ADR-267). Saying "try
            // again" here sent people round a loop that could not end.
            "profile_cascade_not_permitted" => koru_i18n::t(
                crate::format::lang(),
                koru_i18n::Key::ProfilesRemoveNotPermitted,
            )
            .into(),
            _ => "Koru couldn't remove this Profile. Nothing was deleted.".into(),
        });
        true
    }
    pub fn local_profile_remove_failed_for(&mut self, profile: ProfileId, code: &str) -> bool {
        self.apply_to_forms(|form| {
            if form.selected == Some(profile)
                || form
                    .pending_hosted_delete
                    .as_ref()
                    .is_some_and(|pending| pending.profile == profile)
            {
                form.local_profile_remove_failed(code)
            } else {
                false
            }
        })
    }

    pub fn complete(&mut self, result: AccountOperationResult) -> bool {
        if let Some(creation) = self.profile_creation.as_mut() {
            match &result {
                AccountOperationResult::SignedIn | AccountOperationResult::SignedOut => {
                    creation.account_creation_finished()
                }
                AccountOperationResult::Profiles { profiles } => {
                    creation.show_account_profiles(profiles.clone());
                }
                AccountOperationResult::Profile { profile } => {
                    creation.show_account_profiles(vec![profile.clone()]);
                }
                AccountOperationResult::ProfilesSelected { profiles } => {
                    creation.profiles_selected(profiles.clone())
                }
                AccountOperationResult::Cancelled => creation.account_cancelled(),
                AccountOperationResult::Failed { code } => {
                    let mut state = self.account.clone();
                    state.stage = AccountStage::Failed;
                    state.failure_code = Some(code.clone());
                    creation.show_account_state(&state);
                }
                _ => {}
            }
        }
        match result {
            // Only the scope/request-correlated branch may accept this result.
            AccountOperationResult::CloudSubscription { .. }
            | AccountOperationResult::CloudSync { .. } => return false,
            AccountOperationResult::AccountCreationRequired => {
                self.creation_submission = None;
                self.account.stage = AccountStage::AwaitingAccountCreation;
                self.account.failure_code = None;
                self.account_warning_dismissed = false;
                self.creation_decision_pending = false;
                self.notice = None;
                if let Some(creation) = self.profile_creation.as_mut() {
                    creation.account_creation_required(&self.account);
                }
            }
            AccountOperationResult::SignedIn => {
                self.creation_submission = None;
                self.creation_decision_pending = false;
                // The correlated terminal result is itself authoritative. The
                // whole-state push normally arrives first, but the reader and
                // window loop are independent; making the view wait for that
                // second message left a completed browser sign-in painted as
                // "Not signed in" until another account event happened.
                self.account.stage = AccountStage::SignedIn;
                if self
                    .pending_hosted_delete
                    .as_ref()
                    .is_some_and(|pending| pending.stage == HostedDeleteStage::Reauthenticating)
                {
                    if let Some(pending) = self.pending_hosted_delete.as_mut() {
                        pending.stage = HostedDeleteStage::Inspecting;
                        self.pending = Some(AccountCommand::InspectProfileLifecycle {
                            profile_id: pending.profile.to_storage(),
                        });
                    }
                    self.notice = Some("Finishing...".into());
                } else if let Some(unlink) = self.pending_unlink.as_mut()
                    && unlink.awaiting_fresh_google
                {
                    unlink.awaiting_fresh_google = false;
                    self.pending = Some(AccountCommand::RemoveProfileAuthorization {
                        profile_id: unlink.profile_id.clone(),
                        auth_identity_id: unlink.auth_identity_id.clone(),
                        confirm_profile_offline_only: true,
                    });
                    self.notice = Some("Finishing…".into());
                } else if self.attach_resume_after_sign_in() {
                    // The attach that asked for this sign-in is on its way
                    // again; attach.rs set the notice.
                } else {
                    self.pending = Some(AccountCommand::InspectAccountSession);
                    self.notice = Some("Signed in".into());
                }
            }
            AccountOperationResult::SignedInIdentity {
                provider,
                verified_email,
            } => {
                // Both browser providers land here; email has its own journey.
                if matches!(provider.as_str(), "google" | "apple") {
                    self.signed_in_identity = Some((provider, verified_email));
                    self.account_profiles.clear();
                    self.account_profiles_loaded = false;
                    self.different_account_dismissed = false;
                    self.pending = Some(if self.profile_creation.is_some() {
                        AccountCommand::ListProfileChoices
                    } else {
                        AccountCommand::ListProfiles
                    });
                    self.notice = None;
                } else {
                    self.signed_in_identity = None;
                    self.notice = Some("Koru couldn't inspect this account session".into());
                }
            }
            AccountOperationResult::SignedOut => {
                self.cloud.invalidate();
                self.cloud_uri = None;
                self.creation_submission = None;
                self.creation_decision_pending = false;
                self.account.stage = AccountStage::SignedOut;
                self.signed_in_identity = None;
                self.account_profiles.clear();
                self.account_profiles_loaded = false;
                self.different_account_dismissed = false;
                self.notice = Some("Signed out on this device".into());
            }
            AccountOperationResult::Cancelled => {
                self.attach_sign_in_cancelled();
                if self.account.stage == AccountStage::AwaitingAccountCreation
                    || self.creation_decision_pending
                    || self.creation_submission.is_some()
                {
                    self.account.stage = AccountStage::Cancelled;
                    self.creation_decision_pending = false;
                    self.creation_submission = None;
                }
                if self
                    .pending_unlink
                    .as_ref()
                    .is_some_and(|unlink| unlink.awaiting_fresh_google)
                {
                    self.pending_unlink = None;
                }
                self.notice = None;
            }
            AccountOperationResult::Profiles { profiles } => {
                if let Some(candidate) = self.pending_add_google.as_ref()
                    && candidate.authorization_committed
                    && let Some(row) = profiles
                        .iter()
                        .find(|row| row.profile_id == candidate.profile_id)
                    && let Some(profile) = ProfileId::parse(&candidate.profile_id)
                {
                    self.bind_request = Some((
                        profile,
                        candidate.expected_identity_id.clone(),
                        row.koru_identity_id.clone(),
                    ));
                }
                self.account_profiles = profiles;
                self.account_profiles_loaded = true;
                if self.lifecycle_reconciled_generation != Some(self.account.generation)
                    && self.pending.is_none()
                {
                    self.pending = Some(AccountCommand::ReconcileProfileLifecycles);
                }
            }
            AccountOperationResult::ProfileChoices {
                display_name,
                choices,
            } => {
                if let Some(creation) = self.profile_creation.as_mut() {
                    creation.show_account_profile_choices(&display_name, choices.clone());
                }
                self.account_profiles = choices.into_iter().map(|choice| choice.profile).collect();
                self.account_profiles_loaded = true;
            }
            AccountOperationResult::Profile { profile } => {
                if let Some((local_profile, expected_identity_id)) = self.pending_rebind.as_ref()
                    && local_profile.to_storage() == profile.profile_id
                {
                    self.bind_request = Some((
                        *local_profile,
                        Some(expected_identity_id.clone()),
                        profile.koru_identity_id.clone(),
                    ));
                    self.notice = Some("Finishing...".into());
                }
                self.attach_result(&profile);
                if let Some(existing) = self
                    .account_profiles
                    .iter_mut()
                    .find(|row| row.profile_id == profile.profile_id)
                {
                    *existing = profile;
                } else {
                    self.account_profiles.push(profile);
                }
            }
            AccountOperationResult::ProfileAuthorizationRemoved {
                profile_id,
                profile_offline_only,
                already_removed,
            } => {
                let removed_identity = self
                    .pending_unlink
                    .take()
                    .map(|pending| pending.auth_identity_id);
                self.confirm_offline = false;
                self.notice = Some(if already_removed {
                    "That Google Account was already unlinked.".into()
                } else if profile_offline_only {
                    "Profile is now offline-only.".into()
                } else {
                    "Google Account unlinked from this Profile.".into()
                });
                if let Some(row) = self
                    .account_profiles
                    .iter_mut()
                    .find(|row| row.profile_id == profile_id)
                {
                    if let Some(removed_identity) = removed_identity {
                        row.auth_identities
                            .retain(|identity| identity.auth_identity_id != removed_identity);
                    }
                }
                self.pending = Some(AccountCommand::ListProfiles);
            }
            AccountOperationResult::AddGoogleCandidate {
                candidate_id,
                profile_id,
                verified_email,
                expires_at_millis: _,
            } => {
                let profile_name = self
                    .profiles
                    .iter()
                    .find(|profile| profile.id.to_storage() == profile_id)
                    .map(|profile| profile.name.clone())
                    .unwrap_or_else(|| "this Profile".into());
                let expected_identity_id = self
                    .profiles
                    .iter()
                    .find(|row| row.id.to_storage() == profile_id)
                    .and_then(|row| row.account.as_deref())
                    .and_then(|account| account.strip_prefix("koru:"))
                    .map(str::to_owned);
                self.pending_add_google = Some(PendingAddGoogle {
                    candidate_id,
                    profile_id,
                    verified_email,
                    profile_name,
                    expected_identity_id,
                    finishing: false,
                    authorization_committed: false,
                });
                self.notice = None;
                self.hints.reset_focus = true;
            }
            AccountOperationResult::GoogleAuthorizationAdded {
                koru_identity_id,
                profile_id,
                already_authorized: _,
            } => {
                if let Some(candidate) = self.pending_add_google.as_mut()
                    && candidate.profile_id == profile_id
                    && let Some(profile) = ProfileId::parse(&profile_id)
                {
                    candidate.authorization_committed = true;
                    self.bind_request = Some((
                        profile,
                        candidate.expected_identity_id.clone(),
                        koru_identity_id,
                    ));
                    self.notice = Some("Finishing...".into());
                }
            }
            AccountOperationResult::AddGoogleCancelled { profile_id, status } => {
                let matches_candidate = self
                    .pending_add_google
                    .as_ref()
                    .is_some_and(|candidate| candidate.profile_id == profile_id);
                self.notice = match status {
                    koru_agent_wire::AddGoogleCancellationStatus::Cancelled
                    | koru_agent_wire::AddGoogleCancellationStatus::AlreadyCancelled => {
                        if matches_candidate {
                            self.pending_add_google = None;
                        }
                        None
                    }
                    koru_agent_wire::AddGoogleCancellationStatus::AlreadyConfirmed => {
                        if matches_candidate
                            && let Some(candidate) = self.pending_add_google.as_mut()
                        {
                            candidate.authorization_committed = true;
                        }
                        self.pending = Some(AccountCommand::ListProfiles);
                        Some("Google Account was already connected. Finishing...".into())
                    }
                };
            }
            AccountOperationResult::ProfilesSelected { profiles } => {
                self.account.enrollments = profiles;
            }
            AccountOperationResult::AccountSelected { .. } => {}
            AccountOperationResult::ProfileLifecycle { lifecycle }
                if self
                    .pending_hosted_delete
                    .as_ref()
                    .is_some_and(|pending| pending.stage == HostedDeleteStage::Inspecting) =>
            {
                let Some(pending) = self.pending_hosted_delete.as_mut() else {
                    return true;
                };
                if lifecycle.as_ref().is_some_and(|row| {
                    row.lifecycle_state == koru_agent_wire::AccountProfileLifecycleState::Deleted
                }) {
                    pending.stage = HostedDeleteStage::CleanupRunning;
                    self.hosted_delete_request = Some((pending.profile, false));
                } else {
                    let expected_lifecycle_revision = lifecycle
                        .as_ref()
                        .map(|row| row.lifecycle_revision)
                        .unwrap_or(0);
                    pending.stage = HostedDeleteStage::Deleting;
                    self.pending = Some(AccountCommand::DeleteProfile {
                        profile_id: pending.profile.to_storage(),
                        expected_lifecycle_revision,
                    });
                }
            }
            AccountOperationResult::ProfileDeleted { lifecycle, .. } => {
                if let Some(pending) = self.pending_hosted_delete.as_mut()
                    && pending.profile.to_storage() == lifecycle.profile_id
                {
                    pending.stage = HostedDeleteStage::CleanupRunning;
                    self.hosted_delete_request = Some((pending.profile, false));
                    self.notice = None;
                }
            }
            AccountOperationResult::ProfileLifecycleReconciliation { profiles } => {
                self.lifecycle_reconciled_generation = Some(self.account.generation);
                self.discovered_hosted_delete = profiles.into_iter().find_map(|lifecycle| {
                    if lifecycle.lifecycle_state
                        != koru_agent_wire::AccountProfileLifecycleState::Deleted
                    {
                        return None;
                    }
                    let profile = ProfileId::parse(&lifecycle.profile_id)?;
                    let local = self.profiles.iter().find(|row| row.id == profile)?;
                    let identity_id = local.account.as_deref()?.strip_prefix("koru:")?;
                    Some(DiscoveredHostedDeletion {
                        profile,
                        profile_name: local.name.clone(),
                        identity_id: identity_id.to_owned(),
                        lifecycle_revision: lifecycle.lifecycle_revision,
                    })
                });
                if let Some(discovered) = &self.discovered_hosted_delete {
                    self.selected = Some(discovered.profile);
                    self.hints.reset_focus = true;
                }
            }
            AccountOperationResult::ProfileLifecycle { .. } => {}
            AccountOperationResult::ProfileRestored { lifecycle, .. } => {
                if self
                    .discovered_hosted_delete
                    .as_ref()
                    .is_some_and(|pending| pending.profile.to_storage() == lifecycle.profile_id)
                {
                    self.discovered_hosted_delete = None;
                    self.notice =
                        Some("Profile added to Koru again. Sync is ready on this device.".into());
                    self.pending = Some(AccountCommand::ListProfiles);
                }
            }
            AccountOperationResult::Failed { code }
                if code == "profile_lifecycle_owner_unproven" =>
            {
                self.pending_hosted_delete = None;
                self.confirm_hosted_delete = None;
                self.account.failure_code = Some(code);
                self.account_warning_dismissed = false;
            }
            AccountOperationResult::Failed { code }
                if code == "profile_offline_only_confirmation_required" =>
            {
                self.confirm_offline = true;
                self.notice = None;
            }
            AccountOperationResult::Failed { code }
                if code == "fresh_google_authentication_required"
                    && self.pending_unlink.is_some() =>
            {
                if let Some(unlink) = self.pending_unlink.as_mut() {
                    unlink.awaiting_fresh_google = true;
                    self.pending = Some(AccountCommand::StartGoogle {
                        locale: "en".into(),
                    });
                    self.notice = Some("Sign in again to continue".into());
                }
            }
            AccountOperationResult::Failed { code }
                if code == "fresh_google_authentication_required"
                    && self.pending_hosted_delete.is_some() =>
            {
                if let Some(pending) = self.pending_hosted_delete.as_mut() {
                    pending.stage = HostedDeleteStage::Reauthenticating;
                    self.pending = Some(AccountCommand::StartGoogle {
                        locale: "en".into(),
                    });
                    self.notice = Some("Sign in again to continue".into());
                }
            }
            AccountOperationResult::Failed { .. }
                if self.pending_hosted_delete.as_ref().is_some_and(|pending| {
                    matches!(
                        pending.stage,
                        HostedDeleteStage::Inspecting | HostedDeleteStage::Deleting
                    )
                }) =>
            {
                if let Some(pending) = self.pending_hosted_delete.as_mut() {
                    pending.stage = HostedDeleteStage::Failed;
                }
                self.notice = None;
                self.hints.reset_focus = true;
            }
            AccountOperationResult::Failed { code } if self.pending_attach.is_some() => {
                self.attach_hosted_failed(&code);
            }
            AccountOperationResult::Failed { code } => {
                if self.creation_decision_pending || code == "account_confirmation_expired" {
                    self.creation_decision_pending = false;
                    self.account.stage = AccountStage::Failed;
                    self.account.failure_code = Some(code);
                    self.account_warning_dismissed = false;
                }
                self.notice = Some("Koru couldn't finish that account operation".into());
            }
        }
        true
    }
}
