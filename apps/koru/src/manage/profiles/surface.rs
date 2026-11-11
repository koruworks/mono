// SPDX-License-Identifier: MPL-2.0

//! What the controller's state says a view should offer: the account surface
//! for a Profile, the account's Profiles that are not on this device, the
//! different-account page, and which removal a Profile gets.
use super::*;

impl ProfilesController {
    /// The account surface for the selected Profile, if it should take the
    /// detail pane.
    pub fn account_surface(&self) -> Option<koru_workspace_host::account_ui::Surface> {
        self.account_surface_for(self.selected)
    }

    /// The account surface for `profile`, if it should take the detail pane.
    pub fn account_surface_for(
        &self,
        profile: Option<ProfileId>,
    ) -> Option<koru_workspace_host::account_ui::Surface> {
        // Confirmation owns the detail pane. Input already gives these modal
        // states priority; suppressing the account projection here keeps paint
        // on the same state machine instead of redrawing the page underneath.
        if self.confirm_remove || self.confirm_offline || self.account_warning_dismissed {
            return None;
        }
        // Local content stays usable while Cloud connects or retries. Account
        // repair remains available through Accounts, without replacing this pane.
        if profile.is_some_and(|id| self.cloud_sync_on(id))
            && !matches!(
                self.account.failure_code.as_deref(),
                Some("account_erased" | "identity_erased")
            )
        {
            return None;
        }
        if self.creation_submission.is_some()
            && !self.creation_decision_pending
            && self.account.stage == AccountStage::AwaitingAccountCreation
        {
            return Some(koru_workspace_host::account_ui::command_failure_surface());
        }
        // **Only a code the surface would act on counts** — see
        // `actionable_failure_code` for the one it ignores while signed in.
        let code = koru_workspace_host::account_ui::actionable_failure_code(&self.account);
        // **The Profile's own enrollment decides, never another member's.**
        // The surface drawn below is projected per Profile, and this gate used
        // to read the account-wide enrollment instead: while any other Profile
        // was waiting for a peer, every selection took the account surface, and
        // a local-only Profile got a bare *Signed in* card with no actions in
        // place of its detail pane.
        let profile_id = profile.map(|id| id.to_string());
        let projected = koru_workspace_host::account_ui::state_for_profile(
            &self.account,
            profile_id.as_deref(),
        );
        let enrollment_settled = matches!(
            projected.enrollment,
            koru_agent_wire::EnrollmentState::Idle | koru_agent_wire::EnrollmentState::Complete
        );
        let passive_unavailable = code.is_none()
            && matches!(
                self.account.stage,
                AccountStage::Unavailable | AccountStage::Failed
            )
            && enrollment_settled;
        if self.profile_is_local_only(profile) && passive_unavailable {
            return None;
        }
        let needs_surface = code.is_some()
            || matches!(
                self.account.stage,
                AccountStage::AwaitingAccountCreation
                    | AccountStage::Unavailable
                    | AccountStage::Failed
            )
            || !enrollment_settled;
        if !needs_surface {
            return None;
        }
        let profile_name = profile
            .and_then(|id| self.profiles.iter().find(|row| row.id == id))
            .map(|row| row.name.as_str());
        let mut surface = koru_workspace_host::account_ui::project_for_lang(
            &projected,
            profile_name,
            crate::format::lang(),
        );
        if self.creation_decision_pending {
            surface.actions.clear();
        }
        if matches!(code, Some("account_erased" | "identity_erased"))
            && self.erased_identity_of(profile).is_none()
        {
            surface
                .actions
                .retain(|action| *action != koru_workspace_host::account_ui::Action::SignInGoogle);
        }
        Some(surface)
    }

    /// The account-wide can't-connect state, which is no one Profile's to
    /// repair: Koru Cloud's attention card (maintainers' decision 3). Every other
    /// account surface is a Profile's, on that Profile's page.
    pub fn account_wide_surface(&self) -> Option<koru_workspace_host::account_ui::Surface> {
        self.account_surface_for(None).filter(|surface| {
            surface.kind == koru_workspace_host::account_ui::SurfaceKind::Unavailable
        })
    }

    /// The signed-in Google or Apple email, once the session has said it.
    pub fn signed_in_email(&self) -> Option<&str> {
        self.signed_in_identity
            .as_ref()
            .map(|(_, email)| email.as_str())
    }

    pub(super) fn account_profile(&self, profile: ProfileId) -> Option<&AccountProfileRow> {
        let storage = profile.to_storage();
        self.account_profiles
            .iter()
            .find(|row| row.profile_id == storage)
    }

    /// The account's Profiles that are not on this device.
    pub fn missing_account_profiles(&self) -> Vec<&AccountProfileRow> {
        if (self.account.stage != AccountStage::SignedIn
            && self
                .inline_connect
                .as_ref()
                .is_none_or(|pending| !pending.uncertain))
            || !self.account_profiles_loaded
        {
            return Vec::new();
        }
        self.account_profiles
            .iter()
            .filter(|row| {
                !self
                    .profiles
                    .iter()
                    .chain(self.retained_profiles.iter())
                    .any(|local| local.id.to_storage() == row.profile_id)
            })
            .collect()
    }

    /// The Google identities linked to `profile`.
    pub fn identities_of(
        &self,
        profile: ProfileId,
    ) -> Vec<koru_agent_wire::AccountAuthIdentityRow> {
        self.account_profile(profile)
            .map(|row| row.auth_identities.clone())
            .unwrap_or_default()
    }

    pub fn profile_is_connected_to_other_account(&self, profile: &ProfileSummary) -> bool {
        if self.account.stage != AccountStage::SignedIn || !self.account_profiles_loaded {
            return false;
        }
        // A canonical account binding is necessary, but authorization belongs
        // to the exact (Auth Identity, Profile) edge. Sharing a Koru Identity
        // with another returned Profile does not authorize this one.
        if profile
            .account
            .as_deref()
            .and_then(|account| account.strip_prefix("koru:"))
            .is_none()
        {
            return false;
        }
        let profile_id = profile.id.to_storage();
        !self
            .account_profiles
            .iter()
            .any(|authorized| authorized.profile_id == profile_id)
    }

    /// **A Profile of the signed-in account that this sign-in is not linked
    /// to**: it is bound to the account on this device, the account's list
    /// of Profiles has been read, and the Profile is not in it — the list
    /// holds only what the sign-in in use may reach. That is what unlinking
    /// a Google Account from a Profile leaves on the device it was done
    /// from (2026-11-11): the Profile is still here and still the account's,
    /// and Koru Cloud refuses every credential for it. It is on this
    /// computer only until a sign-in that is linked to it is used, so its
    /// row says so rather than offering a switch that cannot work.
    pub fn profile_unlinked_from_sign_in(&self, profile: &ProfileSummary) -> bool {
        use koru_store_core::profile_account::LocalProfileAttribution;
        if self.account.stage != AccountStage::SignedIn || !self.account_profiles_loaded {
            return false;
        }
        let LocalProfileAttribution::Account(binding) =
            LocalProfileAttribution::from_binding(profile.account.as_deref().unwrap_or(""))
        else {
            return false;
        };
        if self.account.active_account_id.as_deref() != Some(binding.identity_id().as_str()) {
            return false;
        }
        let profile_id = profile.id.to_storage();
        !self
            .account_profiles
            .iter()
            .any(|authorized| authorized.profile_id == profile_id)
    }

    /// The signed-in email, while this device's Profiles all belong to
    /// another account and the person has not put that aside.
    pub fn different_account_email(&self) -> Option<&str> {
        if self.confirm_remove
            || self.confirm_offline
            || self.different_account_dismissed
            || self.account.stage != AccountStage::SignedIn
            || !self.account_profiles_loaded
        {
            return None;
        }
        let email = self
            .signed_in_identity
            .as_ref()
            .and_then(|(provider, email)| {
                matches!(provider.as_str(), "google" | "apple").then_some(email.as_str())
            })?;
        let has_connected_local = self.profiles.iter().any(|profile| {
            profile
                .account
                .as_deref()
                .is_some_and(|account| account.starts_with("koru:"))
        });
        let has_current_account_profile = self.profiles.iter().any(|profile| {
            !self.profile_is_connected_to_other_account(profile) && profile.account.is_some()
        });
        (has_connected_local && !has_current_account_profile).then_some(email)
    }

    fn profile_is_local_only(&self, profile: Option<ProfileId>) -> bool {
        profile
            .and_then(|id| self.profiles.iter().find(|row| row.id == id))
            .is_some_and(|row| row.account.is_none())
    }

    /// Whether the selected Profile is one this sign-in is not linked to
    /// ([`Self::profile_unlinked_from_sign_in`]).
    pub fn selected_profile_is_unlinked(&self) -> bool {
        self.selected
            .and_then(|id| self.profiles.iter().find(|row| row.id == id))
            .is_some_and(|row| self.profile_unlinked_from_sign_in(row))
    }

    pub fn selected_profile_is_local_only(&self) -> bool {
        self.profile_is_local_only(self.selected)
    }

    fn erased_identity_of(&self, profile: Option<ProfileId>) -> Option<(ProfileId, String)> {
        let profile = profile?;
        let identity = self
            .profiles
            .iter()
            .find(|row| row.id == profile)?
            .account
            .as_deref()?
            .strip_prefix("koru:")?;
        Some((profile, identity.to_owned()))
    }

    pub(super) fn selected_erased_identity(&self) -> Option<(ProfileId, String)> {
        self.erased_identity_of(self.selected)
    }

    /// Which removal the selected Profile offers: `Some(true)` removes it
    /// with all its Workspaces, `Some(false)` removes an empty one.
    ///
    /// The destructive action is never hidden while the asynchronous Store
    /// count is still arriving. Unknown takes the safer cascade path, which
    /// re-reads the authoritative catalog and is also valid for an empty
    /// Profile; a count is presentation, not permission.
    pub fn selected_profile_removal(&self) -> Option<bool> {
        self.selected
            .map(|profile| self.workspace_counts.get(&profile).copied() != Some(0))
    }

    pub fn selected_profile_can_delete_from_koru(&self) -> bool {
        removal::can_delete(self.account.stage, self.selected, &self.account_profiles)
    }

    // ---- what a nav says about each Profile ----------------------------------

    /// Whether `profile` needs the person: a deletion found on another device
    /// that waits for an answer, or a deletion from Koru that failed or whose
    /// cleanup here failed. The nav's warning mark (maintainers' decision 19).
    pub fn needs_attention(&self, profile: ProfileId) -> bool {
        self.discovered_hosted_delete
            .as_ref()
            .is_some_and(|found| found.profile == profile)
            || self.pending_hosted_delete.as_ref().is_some_and(|pending| {
                pending.profile == profile
                    && matches!(
                        pending.stage,
                        HostedDeleteStage::Failed | HostedDeleteStage::CleanupFailed
                    )
            })
    }

    /// One Profile's status line, in today's account and Cloud vocabulary:
    /// how its connection and its Cloud sync stand, each a clause after a
    /// middle dot, after *Local only* for a Profile on no account.
    ///
    /// **No account id** (the user, 2026-11-10: "users don't care about
    /// that"). An account Profile used to open with *Account 02e1519b*, the
    /// tail of an identity UUID nobody can read; the clauses after it say
    /// what a person needs. The account switcher still names accounts that
    /// way, because there it is what tells two of them apart.
    pub fn status_line(&self, profile: &ProfileSummary) -> String {
        use koru_store_core::profile_account::LocalProfileAttribution;
        let lang = crate::format::lang();
        let mut clauses = Vec::new();
        match LocalProfileAttribution::from_binding(profile.account.as_deref().unwrap_or("")) {
            LocalProfileAttribution::Unbound => clauses
                .push(koru_i18n::t(lang, koru_i18n::Key::AccountSwitcherLocalOnly).to_owned()),
            LocalProfileAttribution::Account(_) => {}
            LocalProfileAttribution::Invalid => {
                clauses.push(koru_i18n::t(lang, koru_i18n::Key::AccountSwitcherUnknown).to_owned())
            }
        }
        clauses.extend(
            self.profile_connection_status(profile.id)
                .map(str::to_owned),
        );
        // The Cloud state is a clause of the same line, not a second line.
        clauses.extend(self.cloud_sync_subtitle(profile));
        clauses.join(" · ")
    }

    /// What the *Connect* control on an account Profile that is not on this
    /// device says, or `None` when it has none: only the one being connected
    /// shows, as *Retry*, while an uncertain attempt waits, and none shows
    /// while a certain one is in flight.
    pub fn connect_label(&self, profile: ProfileId) -> Option<&'static str> {
        match self.inline_connect() {
            None => Some("Connect"),
            Some((_, false)) => None,
            Some((pending, true)) => (pending == profile).then_some("Retry"),
        }
    }

    /// Whether the embedded create form is open.
    pub fn creating(&self) -> bool {
        self.profile_creation.is_some()
    }
}
