// SPDX-License-Identifier: MPL-2.0

//! What a person can ask of their Profiles, in the domain's words.
//!
//! A view maps its own controls onto these — a button, a key, a row — and the
//! controller decides what each means from its state. No intent carries a
//! position, an index into a control list or anything else a layout chose.
use super::*;

/// One thing a person asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    // ---- the session -------------------------------------------------------
    /// The session button, read as [`ProfilesController::session_action`].
    Session,
    SignInGoogle,
    SignOut,
    /// Stop waiting for the browser.
    CancelSignIn,
    /// Bring back an account-creation choice that was put aside.
    ResumeAccountChoice,
    /// Answer the account-creation question with *don't create*.
    DeclineAccountCreation,
    /// A command chosen in the account switcher.
    AccountSwitcher(AccountCommand),
    /// One of the actions the account surface offers. Ignored unless the
    /// surface for the selected Profile offers it right now.
    AccountAction(koru_workspace_host::account_ui::Action),
    /// Escape on the account surface: its cancel, or put it aside.
    DismissAccountSurface,
    /// One of the actions the account-wide can't-connect state offers (Koru
    /// Cloud's attention card). Ignored unless it offers it right now.
    AccountWideAction(koru_workspace_host::account_ui::Action),

    // ---- Profiles ----------------------------------------------------------
    /// Show this Profile's details.
    Select(ProfileId),
    /// Open this Profile in the window: the Profile page's *Switch to this
    /// profile*. Leaves as [`Effect::SwitchProfile`], the same way a Profile
    /// the create form finished does.
    SwitchTo(ProfileId),
    /// Bring an account Profile that is not on this device here.
    Connect(ProfileId),
    /// Ask to take a local-only Profile into the signed-in account.
    Attach(ProfileId),
    AddGoogle(ProfileId),
    ConfirmAddGoogle,
    CancelAddGoogle,
    /// Unlink the `n`th Google identity of a Profile.
    Unlink(ProfileId, usize),
    ConfirmOffline,
    CancelOffline,
    /// Ask to delete the selected Profile with its workspaces as the shared,
    /// restorable batch (root-catalog §6.1), over the Store's preview of
    /// what goes. The question opens when the preview arrives.
    DeleteProfile,
    /// Ask to remove the selected Profile from this device only — its
    /// workspaces go to Trash too when `cascade`. Never the shared batch.
    RemoveFromDevice {
        cascade: bool,
    },
    /// Answer the open question, whichever of the two it is.
    ConfirmRemove,
    CancelRemove,
    /// Ask to delete a Profile from Koru everywhere.
    DeleteFromKoru(ProfileId),
    RetryHostedDelete,
    RemoveAfterHostedDeleteFailure,
    CancelHostedDeleteFailure,
    FinishHostedCleanup,
    PostponeHostedCleanup,
    /// A Profile deleted elsewhere: add it to Koru again.
    RestoreDiscoveredDeletion,
    /// A Profile deleted elsewhere: remove it from this device too.
    RemoveDiscoveredDeletion,
    /// A Profile deleted elsewhere: keep it here, offline-only.
    ContinueDeletedOffline,
    /// Signed in as another account: make a new Profile with it.
    CreateWithSignedInAccount,
    /// Signed in as another account: sign in with a different one.
    SignInAnotherAccount,
    /// Signed in as another account: keep the Profiles here offline.
    DismissDifferentAccount,

    // ---- Koru Cloud ----------------------------------------------------------
    /// The card's primary action: sign in, subscribe, manage, or retry.
    CloudPrimary,
    CloudLearnMore,
    /// Turn one Profile's Cloud sync on or off.
    CloudSync(ProfileId),
    /// Open or close a held Workspace's row in the progress section.
    ToggleProgress(koru_store_core::ids::WorkspaceId),

    // ---- answers to the modal questions --------------------------------------
    AnswerHostedDelete(ConfirmOutcome),
    AnswerAttach(ConfirmOutcome),
}

/// What the session button does now. One function decides both its label and
/// its meaning, so the two cannot disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionAction {
    /// The build has no Koru Cloud: there is nothing to sign in to, and no
    /// button. Its row says why instead.
    NotConfigured,
    ResumeAccountChoice,
    DeclineAccountCreation,
    CancelSignIn,
    SignOut,
    SignInGoogle,
}

/// The one account button under a selected Profile's identities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkControl {
    /// A Profile the account owns: link another Google identity.
    AddGoogle(ProfileId),
    /// A local-only Profile: take it into the signed-in account.
    Attach(ProfileId),
}

impl ProfilesController {
    pub fn session_action(&self) -> SessionAction {
        if self.account.stage == AccountStage::NotConfigured {
            SessionAction::NotConfigured
        } else if self.account_warning_dismissed
            && (self.account.stage == AccountStage::AwaitingAccountCreation
                || self.creation_submission.is_some())
        {
            SessionAction::ResumeAccountChoice
        } else if self.account.stage == AccountStage::AwaitingAccountCreation {
            SessionAction::DeclineAccountCreation
        } else if matches!(
            self.account.stage,
            AccountStage::WaitingForBrowser | AccountStage::Exchanging
        ) {
            SessionAction::CancelSignIn
        } else if self.account.stage == AccountStage::SignedIn {
            SessionAction::SignOut
        } else {
            SessionAction::SignInGoogle
        }
    }

    /// Do what a person asked. Returns whether anything was taken up — only
    /// the modal answers report it, as `Confirm` input needs to know.
    pub fn perform(&mut self, intent: Intent) -> bool {
        // A build with no Koru Cloud starts no account operation. Nothing
        // offers one; an intent that arrives anyway — a key, a control laid
        // out a frame earlier — is dropped here rather than sent to an Agent
        // that has nothing to start it with.
        if self.account.stage == AccountStage::NotConfigured
            && matches!(
                intent,
                Intent::Session
                    | Intent::SignInGoogle
                    | Intent::SignInAnotherAccount
                    | Intent::AccountSwitcher(_)
                    | Intent::CloudPrimary
                    | Intent::CloudSync(_)
                    | Intent::Connect(_)
                    | Intent::Attach(_)
                    | Intent::AddGoogle(_)
            )
        {
            return false;
        }
        match intent {
            Intent::Session => {
                let intent = match self.session_action() {
                    SessionAction::NotConfigured => return false,
                    SessionAction::ResumeAccountChoice => Intent::ResumeAccountChoice,
                    SessionAction::DeclineAccountCreation => Intent::DeclineAccountCreation,
                    SessionAction::CancelSignIn => Intent::CancelSignIn,
                    SessionAction::SignOut => Intent::SignOut,
                    SessionAction::SignInGoogle => Intent::SignInGoogle,
                };
                return self.perform(intent);
            }
            Intent::ResumeAccountChoice => {
                self.account_warning_dismissed = false;
                self.hints.reset_focus = true;
            }
            Intent::DeclineAccountCreation => self.submit_creation_decision(false),
            Intent::CancelSignIn => {
                self.pending = Some(AccountCommand::Cancel {
                    generation: self.account.generation,
                });
            }
            Intent::SignOut => self.pending = Some(AccountCommand::Logout),
            Intent::SignInGoogle => {
                self.pending = Some(AccountCommand::StartGoogle {
                    locale: "en".into(),
                })
            }
            Intent::AccountSwitcher(command) => self.pending = Some(command),
            Intent::AccountAction(action) => {
                if self
                    .account_surface()
                    .is_some_and(|surface| surface.actions.contains(&action))
                {
                    self.account_action(action);
                }
            }
            Intent::AccountWideAction(action) => {
                if self
                    .account_wide_surface()
                    .is_some_and(|surface| surface.actions.contains(&action))
                {
                    self.account_action(action);
                }
            }
            Intent::DismissAccountSurface => {
                use koru_workspace_host::account_ui::Action;
                let cancel = self.account_surface().and_then(|surface| {
                    surface.actions.into_iter().find(|action| {
                        *action == Action::Cancel || *action == Action::CancelAccountCreation
                    })
                });
                match cancel {
                    Some(action) => self.account_action(action),
                    None => self.account_warning_dismissed = true,
                }
            }
            Intent::Select(profile) => {
                // The notice is drawn under the selected Profile's header, so
                // one left over from the last Profile reads as news about this
                // one — "Profile removed from this device" under a Profile
                // that was not. Only a change clears it: re-selecting the same
                // row is not a reason to lose what it was just told.
                if self.selected != Some(profile) {
                    self.notice = None;
                }
                self.follow_page(profile);
            }
            Intent::SwitchTo(profile) => {
                if self.profiles.iter().any(|row| row.id == profile) {
                    self.profile_switch_request = Some(profile);
                }
            }
            Intent::Connect(profile) => self.begin_inline_connect(profile),
            Intent::Attach(profile) => self.ask_to_attach(profile),
            Intent::AddGoogle(profile) => {
                self.pending = Some(AccountCommand::AddGoogle {
                    profile_id: profile.to_storage(),
                    locale: "en".into(),
                });
                self.notice = Some("Waiting for Google...".into());
            }
            Intent::ConfirmAddGoogle => {
                if let Some(candidate) = self.pending_add_google.as_mut() {
                    candidate.finishing = true;
                    self.pending = Some(AccountCommand::ConfirmAddGoogle {
                        candidate_id: candidate.candidate_id.clone(),
                        profile_id: candidate.profile_id.clone(),
                    });
                    self.notice = Some("Finishing...".into());
                }
            }
            Intent::CancelAddGoogle => {
                if let Some(candidate) = self.pending_add_google.as_mut() {
                    candidate.finishing = true;
                    self.pending = Some(AccountCommand::CancelAddGoogle {
                        candidate_id: candidate.candidate_id.clone(),
                        profile_id: candidate.profile_id.clone(),
                    });
                    self.notice = Some("Finishing...".into());
                }
            }
            Intent::Unlink(profile, index) => self.unlink(profile, index),
            Intent::ConfirmOffline => {
                if let Some(unlink) = &self.pending_unlink {
                    self.pending = Some(AccountCommand::RemoveProfileAuthorization {
                        profile_id: unlink.profile_id.clone(),
                        auth_identity_id: unlink.auth_identity_id.clone(),
                        confirm_profile_offline_only: true,
                    });
                    self.confirm_offline = false;
                    self.notice = Some("Finishing…".into());
                }
            }
            Intent::CancelOffline => {
                self.confirm_offline = false;
                self.pending_unlink = None;
            }
            Intent::DeleteProfile => {
                // The shared batch (root-catalog §6.1): the question is asked
                // over the Store's own list of what goes, read off this
                // thread first, and opens when it arrives. A window with no
                // Store client cannot ask, so it says so and changes nothing
                // — it never falls back to the device-local removal, which
                // is a different action with a different label.
                self.deletion_preview = None;
                self.remove_all_workspaces = true;
                let previewed = self
                    .selected
                    .is_some_and(|profile| self.request_deletion_preview(profile));
                if !previewed {
                    self.notice = Some(
                        koru_i18n::t(
                            crate::format::lang(),
                            koru_i18n::Key::ProfileDeletePreviewFailed,
                        )
                        .into(),
                    );
                }
            }
            Intent::RemoveFromDevice { cascade } => {
                // Device-local, so no preview: one still in flight for the
                // shared deletion is dropped, or its answer would reopen
                // this question as that one.
                self.preview_request = None;
                self.deletion_preview = None;
                self.remove_all_workspaces = cascade;
                self.confirm_remove = true;
            }
            Intent::ConfirmRemove => {
                // The caller performs Store mutation and settings cleanup. The
                // row remains visible until that whole operation acknowledges.
                self.remove_request = self
                    .selected
                    .map(|profile| (profile, self.remove_all_workspaces));
                self.confirm_remove = false;
                self.notice = Some("Finishing…".into());
            }
            Intent::CancelRemove => {
                self.confirm_remove = false;
                // A preview left behind would travel with the next removal
                // request — the attention card's, say — and make it the batch.
                self.deletion_preview = None;
            }
            Intent::DeleteFromKoru(profile) => {
                let profile_name = self.profile_name_or_default(profile);
                self.confirm_hosted_delete = Some(removal::confirmation(profile, &profile_name));
            }
            Intent::RetryHostedDelete => {
                if let Some(pending) = self.pending_hosted_delete.as_mut() {
                    pending.stage = HostedDeleteStage::Inspecting;
                    self.pending = Some(AccountCommand::InspectProfileLifecycle {
                        profile_id: pending.profile.to_storage(),
                    });
                    self.notice = Some("Finishing...".into());
                }
            }
            Intent::RemoveAfterHostedDeleteFailure => {
                if let Some(pending) = self.pending_hosted_delete.take() {
                    self.remove_request = Some((pending.profile, true));
                    self.notice = Some("Finishing...".into());
                }
            }
            Intent::CancelHostedDeleteFailure | Intent::PostponeHostedCleanup => {
                self.pending_hosted_delete = None;
                self.notice = None;
            }
            Intent::FinishHostedCleanup => {
                if let Some(pending) = self.pending_hosted_delete.as_mut() {
                    pending.stage = HostedDeleteStage::CleanupRunning;
                    self.hosted_delete_request = Some((pending.profile, false));
                    self.notice = None;
                }
            }
            Intent::RemoveDiscoveredDeletion => {
                if let Some(discovered) = self.discovered_hosted_delete.take() {
                    self.pending_hosted_delete = Some(PendingHostedDelete {
                        profile: discovered.profile,
                        profile_name: discovered.profile_name,
                        stage: HostedDeleteStage::CleanupRunning,
                    });
                    self.hosted_delete_request = Some((discovered.profile, true));
                    self.notice = None;
                }
            }
            Intent::ContinueDeletedOffline => {
                if let Some(discovered) = self.discovered_hosted_delete.as_ref() {
                    self.continue_offline_request =
                        Some((discovered.profile, discovered.identity_id.clone()));
                    self.notice = Some("Finishing...".into());
                }
            }
            Intent::RestoreDiscoveredDeletion => {
                if let Some(discovered) = self.discovered_hosted_delete.as_ref() {
                    self.pending = Some(AccountCommand::RestoreProfile {
                        profile_id: discovered.profile.to_storage(),
                        expected_lifecycle_revision: discovered.lifecycle_revision,
                        name_snapshot: discovered.profile_name.clone(),
                        locale_snapshot: Some("en".into()),
                    });
                    self.notice = Some("Adding Profile to Koru again...".into());
                }
            }
            Intent::CreateWithSignedInAccount => self.begin_connected_profile_creation(),
            Intent::SignInAnotherAccount => {
                self.pending = Some(AccountCommand::StartGoogle {
                    locale: "en".into(),
                });
                self.notice = Some("Waiting for Google...".into());
            }
            Intent::DismissDifferentAccount => {
                self.different_account_dismissed = true;
                self.notice = None;
            }
            Intent::CloudPrimary => self.cloud_activate(),
            Intent::CloudLearnMore => self.cloud_learn_more_activate(),
            // Deliberately not a selection: the switch changes one Profile's
            // Cloud sync and leaves the details pane where the person left it.
            Intent::CloudSync(profile) => self.cloud_sync_activate(profile),
            Intent::ToggleProgress(workspace) => {
                self.progress.toggle(workspace, std::time::Instant::now());
            }
            Intent::AnswerHostedDelete(outcome) => {
                return self.settle_hosted_delete_confirmation(outcome);
            }
            Intent::AnswerAttach(outcome) => return self.settle_attach_prompt(outcome),
        }
        true
    }

    fn unlink(&mut self, profile: ProfileId, index: usize) {
        let identities = self.identities_of(profile);
        let Some(identity) = identities.get(index).cloned() else {
            return;
        };
        let profile_name = self.profile_name_or_default(profile);
        let is_last = identities.len() == 1;
        self.pending_unlink = Some(PendingUnlink {
            profile_id: profile.to_storage(),
            auth_identity_id: identity.auth_identity_id,
            profile_name,
            awaiting_fresh_google: false,
        });
        if is_last {
            self.confirm_offline = true;
        } else if let Some(unlink) = &self.pending_unlink {
            self.pending = Some(AccountCommand::RemoveProfileAuthorization {
                profile_id: unlink.profile_id.clone(),
                auth_identity_id: unlink.auth_identity_id.clone(),
                confirm_profile_offline_only: false,
            });
            self.notice = Some("Finishing…".into());
        }
    }

    pub(super) fn profile_name_or_default(&self, profile: ProfileId) -> String {
        self.profiles
            .iter()
            .find(|row| row.id == profile)
            .map(|row| row.name.clone())
            .unwrap_or_else(|| "this Profile".into())
    }

    fn account_action(&mut self, action: koru_workspace_host::account_ui::Action) {
        use koru_workspace_host::account_ui::Action;
        match action {
            Action::ConfirmAccountCreation => self.submit_creation_decision(true),
            Action::CancelAccountCreation => self.submit_creation_decision(false),
            Action::Retry => {
                self.account_warning_dismissed = true;
                if self.creation_submission.is_some() {
                    self.creation_submission =
                        Some(koru_workspace_host::account_ui::CommandSubmissionId::next());
                    self.creation_decision_pending = true;
                }
                self.pending = Some(AccountCommand::Retry);
            }
            Action::RetryEnrollment => {
                self.account_warning_dismissed = true;
                self.pending = Some(AccountCommand::RetryEnrollment {
                    profile_id: self.selected.map(|id| id.to_string()),
                });
            }
            Action::ContactSupport => {
                self.uri_request = Some(koru_ui::welcome::CHAT_URL);
            }
            Action::SignInGoogle
                if matches!(
                    self.account.failure_code.as_deref(),
                    Some("account_erased" | "identity_erased")
                ) =>
            {
                if let Some((profile, erased_identity_id)) = self.selected_erased_identity() {
                    self.pending_rebind = Some((profile, erased_identity_id.clone()));
                    self.account_warning_dismissed = true;
                    self.pending = Some(AccountCommand::RebindProfile {
                        profile_id: profile.to_storage(),
                        erased_identity_id,
                    });
                    self.notice = Some("Waiting for Google...".into());
                }
            }
            Action::SignInGoogle
                if matches!(
                    self.account.failure_code.as_deref(),
                    Some("collision" | "collision_required")
                ) =>
            {
                // Retry preserves the exact paused collision flow and opens the
                // account chooser for proof of the already-connected identity.
                self.account_warning_dismissed = true;
                self.pending = Some(AccountCommand::Retry);
            }
            Action::SignInGoogle | Action::SignInAnotherGoogle => {
                self.pending = Some(AccountCommand::StartGoogle {
                    locale: "en".into(),
                });
            }
            Action::ChooseProfile | Action::ContinueOffline => {
                self.account_warning_dismissed = true;
            }
            Action::Cancel => {
                self.pending = Some(AccountCommand::Cancel {
                    generation: self.account.generation,
                });
            }
            Action::CancelEnrollment => {
                self.pending = Some(AccountCommand::CancelEnrollment);
            }
        }
    }

    pub(super) fn submit_creation_decision(&mut self, confirm: bool) {
        if self.account.stage != AccountStage::AwaitingAccountCreation
            || self.creation_decision_pending
        {
            return;
        }
        self.creation_decision_pending = true;
        self.creation_submission = Some(
            self.profile_creation
                .as_ref()
                .and_then(|creation| creation.borrow().account_submission())
                .unwrap_or_else(koru_workspace_host::account_ui::CommandSubmissionId::next),
        );
        self.pending = Some(if confirm {
            AccountCommand::ConfirmAccountCreation
        } else {
            AccountCommand::CancelAccountCreation
        });
    }

    fn settle_hosted_delete_confirmation(&mut self, outcome: ConfirmOutcome) -> bool {
        match outcome {
            // Two answers here; the third belongs to a dialog that declared
            // one (`Confirm::with_alternative`) and never reaches this.
            ConfirmOutcome::Keep | ConfirmOutcome::Alternative => false,
            ConfirmOutcome::Cancel => {
                self.confirm_hosted_delete = None;
                true
            }
            ConfirmOutcome::Confirm => {
                let Some((profile, _)) = self.confirm_hosted_delete.take() else {
                    return false;
                };
                let profile_name = self.profile_name_or_default(profile);
                self.pending_hosted_delete = Some(PendingHostedDelete {
                    profile,
                    profile_name,
                    stage: HostedDeleteStage::Inspecting,
                });
                self.pending = Some(AccountCommand::InspectProfileLifecycle {
                    profile_id: profile.to_storage(),
                });
                self.notice = Some("Finishing...".into());
                true
            }
        }
    }
}
