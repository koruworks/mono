// SPDX-License-Identifier: MPL-2.0

//! The embedded create form: the same onboarding form the first-run window
//! shows, reused inside an already-open Profile manager, and what its actions
//! become here.
use super::*;

impl ProfilesController {
    pub(super) fn begin_connected_profile_creation(&mut self) {
        let mut creation = koru_workspace_host::onboarding::OnboardingView::default();
        creation.select_account_context(&self.account);
        // The page frames the form: no first-run pane, lined up with its head.
        creation.set_embedded(true);
        self.hints.size_creation_form = true;
        creation.begin_connected_profile_creation();
        self.profile_creation = Some(creation);
        self.hints.reset_focus = true;
    }

    /// Open the create form, as *Create Profile* does. The switcher's
    /// *New profile* row lands here.
    pub fn start_profile_creation(&mut self) {
        if self.profile_creation.is_none() {
            self.begin_profile_creation();
        }
    }

    /// Put the create form away without finishing it — the page's X. What
    /// was typed goes with it.
    pub fn cancel_profile_creation(&mut self) {
        if self.profile_creation.take().is_some() {
            self.hints.reset_focus = true;
        }
    }

    pub(super) fn begin_profile_creation(&mut self) {
        if self.account.stage == AccountStage::SignedIn {
            self.begin_connected_profile_creation();
            return;
        }
        let mut creation = koru_workspace_host::onboarding::OnboardingView::default();
        creation.select_account_context(&self.account);
        // The page frames the form: no first-run pane, lined up with its head.
        creation.set_embedded(true);
        self.hints.size_creation_form = true;
        self.profile_creation = Some(creation);
        self.hints.reset_focus = true;
    }

    pub(super) fn drain_profile_creation_action(&mut self) {
        if let Some(creation) = self.profile_creation.as_mut() {
            // These facts belong to first-run onboarding. The same form is
            // reused here, inside an already-open Profile manager.
            let _ = creation.take_telemetry();
        }
        let action = self
            .profile_creation
            .as_mut()
            .and_then(|creation| creation.take_action());
        use koru_workspace_host::onboarding::Action;
        match action {
            Some(Action::Account(command)) => self.pending = Some(command),
            Some(Action::OpenCreatedProfile(profile)) => {
                self.profile_switch_request = Some(profile);
                self.profile_creation = None;
            }
            Some(Action::ConfirmAccountCreation) => self.submit_creation_decision(true),
            Some(Action::CancelAccountCreation) => self.submit_creation_decision(false),
            Some(Action::StartGoogle) => {
                self.pending = Some(AccountCommand::StartGoogle {
                    locale: "en".into(),
                });
            }
            Some(Action::StartApple) => {
                self.pending = Some(AccountCommand::StartApple {
                    locale: "en".into(),
                });
            }
            // The same four steps the first-run window raises, from the same
            // screens: this surface embeds the identical form.
            Some(Action::ContinueWithEmail { address, password }) => {
                self.pending = Some(AccountCommand::ContinueWithEmail {
                    locale: "en".into(),
                    address,
                    password,
                });
            }
            Some(Action::SubmitEmailCode { code }) => {
                self.pending = Some(AccountCommand::SubmitEmailCode { code });
            }
            Some(Action::SubmitEmailPassword { password }) => {
                self.pending = Some(AccountCommand::SubmitEmailPassword { password });
            }
            Some(Action::ResendEmailCode) => {
                self.pending = Some(AccountCommand::ResendEmailCode);
            }
            Some(Action::OpenUri(uri)) => self.uri_request = Some(uri),
            Some(Action::RetryAccount) => {
                if let Some(submission) = self
                    .profile_creation
                    .as_ref()
                    .and_then(|creation| creation.account_submission())
                {
                    self.creation_submission = Some(submission);
                    self.creation_decision_pending = true;
                }
                self.pending = Some(AccountCommand::Retry);
            }
            Some(Action::RetryEnrollment) => {
                self.pending = Some(AccountCommand::RetryEnrollment {
                    profile_id: self
                        .profile_creation
                        .as_ref()
                        .and_then(|view| view.selected_account_profile_id())
                        .map(str::to_owned),
                });
            }
            Some(Action::CancelGoogle) => {
                self.pending = Some(AccountCommand::Cancel {
                    generation: self
                        .profile_creation
                        .as_ref()
                        .map_or(self.account.generation, |view| view.account_generation()),
                });
            }
            Some(Action::CancelEnrollment) => {
                self.pending = Some(AccountCommand::CancelEnrollment);
            }
            Some(Action::SelectAccountProfiles { profile_ids }) => {
                self.pending = Some(AccountCommand::SelectProfiles { profile_ids });
            }
            Some(Action::RestoreLocalProfile { profile_id }) => {
                if let Some(profile) = ProfileId::parse(&profile_id) {
                    if self.profiles.iter().any(|row| row.id == profile) {
                        // The hosted availability catalog is device-wide. An
                        // already-visible local Profile therefore appears as
                        // "on this device" too; selecting it is a switch, not
                        // another Store restore.
                        let _ = self.finish_local_profile_choice(profile, "Profile selected");
                        self.profile_switch_request = Some(profile);
                    } else {
                        self.profile_restore_request = Some(profile);
                    }
                } else if let Some(creation) = self.profile_creation.as_mut() {
                    creation.restore_failed(
                        "Koru couldn't identify that Profile. Nothing was changed.",
                    );
                }
            }
            Some(Action::CreateLocalProfile {
                owner_display_name,
                name,
                workspace_name,
                template,
                workspace_icon,
                workspace_color,
                profile_icon,
                profile_color,
                connect_account,
            }) => {
                self.profile_creation_request = Some(ConnectedProfileDraft {
                    owner_display_name,
                    profile_name: name,
                    workspace_name,
                    template,
                    workspace_icon,
                    workspace_color,
                    profile_icon,
                    profile_color,
                    connect_account,
                });
            }
            None => {}
        }
    }

    pub(super) fn finish_local_profile_choice(&mut self, profile: ProfileId, notice: &str) -> bool {
        if self.profile_creation.take().is_none() {
            return false;
        }
        self.selected = Some(profile);
        self.notice = Some(notice.into());
        self.pending = Some(AccountCommand::ListProfiles);
        true
    }
}
