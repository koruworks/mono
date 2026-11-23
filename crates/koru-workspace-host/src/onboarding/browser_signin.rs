// SPDX-License-Identifier: MPL-2.0

//! Local admission for browser sign-in. Only the worker settles the operation.
use super::*;
use crate::account_ui::{CommandAdmission, CommandFailure, CommandSubmissionId};
use koru_agent_wire::AccountCommand;

pub(super) struct Submission {
    start: CommandSubmissionId,
    generation: Option<u64>,
    cancel_requested: bool,
    cancel: Option<CommandSubmissionId>,
}

impl OnboardingView {
    /// `provider` picks Google's, Apple's, or Koru's email login page;
    /// everything after the browser returns is the same journey.
    pub(super) fn begin_browser_signin(&mut self, provider: BrowserProvider) {
        self.browser_signin = Some(Submission {
            start: CommandSubmissionId::next(),
            generation: None,
            cancel_requested: false,
            cancel: None,
        });
        self.screen = Screen::Browser;
        self.keyboard_focus = false;
        self.browser_link_copied_at = None;
        self.focus = 0;
        // The retry supersedes the last failure, exactly as `show_profile`
        // clears it when the other welcome action is taken. Without this the
        // message the welcome screen now draws would still be on it if any
        // route came back here without failing again.
        self.error = None;
        self.error_field = None;
        self.last_attempt = Some(match provider {
            BrowserProvider::Google => welcome::Action::Google,
            BrowserProvider::Apple => welcome::Action::Apple,
            BrowserProvider::Email => welcome::Action::ContinueWithEmail,
        });
        self.browser_provider = provider;
        self.pending = Some(match provider {
            BrowserProvider::Google => Action::StartGoogle,
            BrowserProvider::Apple => Action::StartApple,
            BrowserProvider::Email => Action::StartEmailBrowser,
        });
    }

    /// Re-arm the browser wait state for a sign-in command dispatched right
    /// after clearing a prior bound scope during onboarding.
    pub(crate) fn arm_browser_signin(&mut self, command: &koru_agent_wire::AccountCommand) {
        let provider = match command {
            koru_agent_wire::AccountCommand::StartGoogle { .. } => BrowserProvider::Google,
            koru_agent_wire::AccountCommand::StartApple { .. } => BrowserProvider::Apple,
            koru_agent_wire::AccountCommand::StartEmailBrowser { .. } => BrowserProvider::Email,
            _ => return,
        };
        self.browser_signin = Some(Submission {
            start: CommandSubmissionId::next(),
            generation: None,
            cancel_requested: false,
            cancel: None,
        });
        self.modal_account_flow = true;
        self.screen = Screen::Browser;
        self.keyboard_focus = false;
        self.browser_link_copied_at = None;
        self.focus = 0;
        self.error = None;
        self.error_field = None;
        self.browser_provider = provider;
        self.last_attempt = Some(match provider {
            BrowserProvider::Google => welcome::Action::Google,
            BrowserProvider::Apple => welcome::Action::Apple,
            BrowserProvider::Email => welcome::Action::ContinueWithEmail,
        });
    }

    /// The welcome screen's submit, admitted the same way the Google one is.
    ///
    /// It uses the same tracker because it is the same thing from this form's
    /// side: one command in flight that ends in a session, cancellable until
    /// the worker admits it. Which screen follows is the worker's answer, so
    /// this parks on the wait.
    pub(super) fn begin_email_journey(&mut self, address: String, password: String) {
        self.browser_signin = Some(Submission {
            start: CommandSubmissionId::next(),
            generation: None,
            cancel_requested: false,
            cancel: None,
        });
        self.screen = Screen::EmailWorking;
        self.focus = 0;
        self.error = None;
        self.error_field = None;
        self.last_attempt = Some(welcome::Action::ContinueWithEmail);
        self.pending = Some(Action::ContinueWithEmail { address, password });
    }

    pub(super) fn cancel_browser_signin(&mut self) {
        if matches!(
            self.pending,
            Some(
                Action::StartGoogle
                    | Action::StartApple
                    | Action::StartEmailBrowser
                    | Action::ContinueWithEmail { .. }
            )
        ) {
            // The action has not left this form, so no hosted operation exists.
            self.pending = None;
            self.account_cancelled();
            return;
        }
        if let Some(signin) = &mut self.browser_signin {
            signin.cancel_requested = true;
            if signin.generation.is_none() || signin.cancel.is_some() {
                return;
            }
            signin.cancel = Some(CommandSubmissionId::next());
        }
        self.pending = Some(Action::CancelGoogle);
    }

    pub(super) fn browser_generation(&self) -> Option<u64> {
        self.browser_signin.as_ref()?.generation
    }

    pub(super) fn browser_submission(
        &self,
        command: &AccountCommand,
    ) -> Option<CommandSubmissionId> {
        let signin = self.browser_signin.as_ref()?;
        match command {
            AccountCommand::StartGoogle { .. }
            | AccountCommand::StartApple { .. }
            | AccountCommand::StartEmailBrowser { .. }
            | AccountCommand::ContinueWithEmail { .. } => Some(signin.start),
            AccountCommand::Cancel { .. } => signin.cancel,
            _ => None,
        }
    }

    pub(super) fn browser_command_admitted(&mut self, admission: CommandAdmission) -> bool {
        if admission.owner_scope != self.account_scope() {
            return false;
        }
        let Some(signin) = &mut self.browser_signin else {
            return false;
        };
        if signin.start == admission.submission {
            signin.generation = Some(admission.generation);
            if signin.cancel_requested {
                self.cancel_browser_signin();
            }
            return true;
        }
        signin.cancel == Some(admission.submission)
    }

    pub(super) fn browser_command_failed(&mut self, failure: CommandFailure) -> bool {
        let Some(signin) = &mut self.browser_signin else {
            return false;
        };
        if failure.submission == signin.start && !failure.uncertain {
            // Rejected before admission: return to a usable sign-in/offline choice.
            self.browser_signin = None;
            self.account_failed(crate::account_ui::command_failure_surface().body);
            return true;
        }
        if failure.submission != signin.start && Some(failure.submission) != signin.cancel {
            return false;
        }
        signin.cancel = None;
        let mut surface = crate::account_ui::command_failure_surface();
        surface.actions = vec![crate::account_ui::Action::ContinueOffline];
        if signin.generation.is_some() {
            surface.actions.push(crate::account_ui::Action::Cancel);
        }
        self.account_surface = Some(surface);
        self.screen = Screen::AccountStatus;
        self.focus = 0;
        true
    }
}

#[cfg(test)]
mod tests;
