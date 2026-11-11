// SPDX-License-Identifier: MPL-2.0

//! Taking a local-only Profile into the signed-in account.
//!
//! A Profile made on this device before anyone signed in has no account
//! binding: its row says *Local only*, and the Cloud sync switch is hidden
//! because there is no account for it to sync under. The hosted side has had
//! the path for this since the first account milestone — `AttachProfile`
//! creates the hosted record under the signed-in identity and provisions it
//! for this device, and the local Store binds the Profile to that identity by
//! compare-and-set (ADR-306) — but no desktop surface sent it. *Add Google
//! Account* is not that path: it links a second Google identity to a Profile
//! the account already owns, and the server refuses it for one it does not.
//!
//! The shape here is the test plan's (docs/features/oauth/TESTPLAN.md, item 6): one
//! Profile at a time, nothing binds before an explicit confirmation, and
//! *Connected* is said only after both the hosted acknowledgement and the
//! local durable bind.
//!
//! **The hosted attach needs the person, not just the session.** It requires
//! a Google authentication from the last five minutes, and a session restored
//! from the vault has none. The server answers that with the same 428 the
//! last-authorization removal uses, and this flow does what Unlink does with
//! it: a fresh sign-in, then the same attach again.
use super::*;
use koru_i18n::{Key as TextKey, fill, t};
use koru_workspace_host::account_ui::{CommandFailure, CommandSubmissionId};

#[cfg(test)]
mod tests;

/// One attach from the confirmed click to the local bind. **Cleared by every
/// outcome**, because while it is set the button is withheld: a stale one
/// would leave a Profile that could be connected looking as if it could not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PendingAttach {
    pub profile: ProfileId,
    /// The submission the hosted command went out under, so a refusal at
    /// admission — which never produces a result — can still free the button.
    pub submission: CommandSubmissionId,
    /// The hosted side asked for a fresh sign-in; the attach is sent again
    /// once one lands, and dropped if the person cancels it.
    pub awaiting_fresh_sign_in: bool,
}

impl PendingAttach {
    fn new(profile: ProfileId) -> Self {
        Self {
            profile,
            submission: CommandSubmissionId::next(),
            awaiting_fresh_sign_in: false,
        }
    }
}

impl ProfilesController {
    /// The one account button under a selected Profile's identities, and its
    /// label. *Add Google Account* for a Profile the account owns; *Connect to
    /// your account* for a local-only one, unless its attach is in flight —
    /// then there is no button and [`Self::attach_status`] says why.
    ///
    /// **One function**, so the control list, hit testing and paint cannot
    /// disagree about which button is there.
    pub fn account_link_control(&self) -> Option<(LinkControl, String)> {
        if self.account.stage != AccountStage::SignedIn {
            return None;
        }
        let profile = self.selected?;
        // A Profile this sign-in is not linked to is connected the way a
        // local-only one is: the hosted attach puts the link back for a
        // Profile the account already owns, and the local bind answers
        // *already bound*. *Add Google Account* would be refused for it —
        // only a sign-in that is linked to a Profile may add another.
        if !self.selected_profile_is_local_only() && !self.selected_profile_is_unlinked() {
            return Some((
                LinkControl::AddGoogle(profile),
                "Add Google Account".to_owned(),
            ));
        }
        if self.pending_attach.is_some() {
            return None;
        }
        Some((
            LinkControl::Attach(profile),
            t(crate::format::lang(), TextKey::ProfileConnectButton).to_owned(),
        ))
    }

    /// What the detail pane says in the button's place while the selected
    /// Profile's attach is in flight.
    pub fn attach_status(&self) -> Option<String> {
        let pending = self.pending_attach.as_ref()?;
        if self.selected != Some(pending.profile) {
            return None;
        }
        let lang = crate::format::lang();
        Some(if pending.awaiting_fresh_sign_in {
            fill(
                t(lang, TextKey::ProfileConnectSignInAgain),
                &[&self.profile_name(pending.profile)],
            )
        } else {
            t(lang, TextKey::ProfileConnectConnecting).to_owned()
        })
    }

    fn profile_name(&self, profile: ProfileId) -> String {
        self.profiles
            .iter()
            .find(|row| row.id == profile)
            .map(|row| row.name.clone())
            .unwrap_or_default()
    }

    /// Raise the question. Nothing is sent until it is answered.
    pub(super) fn ask_to_attach(&mut self, profile: ProfileId) {
        if self.attach_prompt.is_some()
            || self.pending_attach.is_some()
            || self.pending.is_some()
            || self.account.stage != AccountStage::SignedIn
            || !self.profiles.iter().any(|row| {
                row.id == profile
                    && (row.account.is_none() || self.profile_unlinked_from_sign_in(row))
            })
        {
            return;
        }
        let lang = crate::format::lang();
        let name = self.profile_name(profile);
        let confirm = Confirm::asking(
            fill(t(lang, TextKey::ProfileConnectTitle), &[&name]),
            fill(t(lang, TextKey::ProfileConnectBody), &[&name]),
            t(lang, TextKey::ProfileConnectConfirm),
            t(lang, TextKey::ProfileConnectCancel),
        );
        // Enter answers Cancel, as `Confirm::asking` declares: the person
        // clicked the button, but a hosted write about their Profile is not
        // something the next keystroke should commit.
        self.attach_prompt = Some((profile, confirm));
    }

    /// Settle the question. Only *Connect* writes anything.
    pub(super) fn settle_attach_prompt(&mut self, outcome: ConfirmOutcome) -> bool {
        match outcome {
            ConfirmOutcome::Keep | ConfirmOutcome::Alternative => false,
            ConfirmOutcome::Cancel => self.attach_prompt.take().is_some(),
            ConfirmOutcome::Confirm => {
                let Some((profile, _)) = self.attach_prompt.take() else {
                    return false;
                };
                self.pending_attach = Some(PendingAttach::new(profile));
                self.send_attach(profile);
                true
            }
        }
    }

    /// The hosted command itself. The snapshots are what another device sees
    /// the Profile called before it holds a copy.
    fn send_attach(&mut self, profile: ProfileId) {
        let lang = crate::format::lang();
        self.pending = Some(AccountCommand::AttachProfile {
            profile_id: profile.to_storage(),
            name_snapshot: Some(self.profile_name(profile)),
            locale_snapshot: Some(lang.tag().to_owned()),
        });
        self.notice = Some(t(lang, TextKey::ProfileConnectConnecting).to_owned());
    }

    /// The submission the attach command goes out under.
    pub(super) fn attach_submission(
        &self,
        command: &AccountCommand,
    ) -> Option<CommandSubmissionId> {
        matches!(command, AccountCommand::AttachProfile { .. })
            .then(|| {
                self.pending_attach
                    .as_ref()
                    .map(|pending| pending.submission)
            })
            .flatten()
    }

    /// The command never reached the worker, or may not have. Either way the
    /// button comes back: the hosted attach is idempotent, so a second click is
    /// the recovery for the uncertain case too.
    pub(super) fn attach_failed(&mut self, failure: CommandFailure) -> bool {
        if self
            .pending_attach
            .as_ref()
            .is_none_or(|pending| pending.submission != failure.submission)
        {
            return false;
        }
        let pending = self.pending_attach.take().expect("checked above");
        self.notice = Some(self.attach_failed_notice(pending.profile));
        true
    }

    /// The hosted side answered. The Profile is now the account's; make it
    /// this device's by binding the local Profile to the identity the result
    /// names — never one inferred from a name or an email.
    pub(super) fn attach_result(&mut self, profile: &AccountProfileRow) -> bool {
        let Some(pending) = self.pending_attach.as_ref() else {
            return false;
        };
        if pending.profile.to_storage() != profile.profile_id {
            return false;
        }
        self.bind_request = Some((pending.profile, None, profile.koru_identity_id.clone()));
        self.notice = Some(t(crate::format::lang(), TextKey::ProfileConnectConnecting).to_owned());
        true
    }

    /// The hosted side refused. One refusal is a precondition rather than an
    /// answer: the session's last Google authentication is too old to speak
    /// for the person, and a fresh sign-in is the way through — the same
    /// 428 Unlink and the hosted delete already resume from. Anything else
    /// ends the attempt and gives the button back.
    pub(super) fn attach_hosted_failed(&mut self, code: &str) -> bool {
        let Some(pending) = self.pending_attach.as_mut() else {
            return false;
        };
        if matches!(
            code,
            "fresh_authentication_required" | "fresh_google_authentication_required"
        ) {
            pending.awaiting_fresh_sign_in = true;
            let profile = pending.profile;
            let lang = crate::format::lang();
            self.pending = Some(AccountCommand::StartGoogle {
                locale: lang.tag().to_owned(),
            });
            self.notice = Some(fill(
                t(lang, TextKey::ProfileConnectSignInAgain),
                &[&self.profile_name(profile)],
            ));
            return true;
        }
        let pending = self.pending_attach.take().expect("checked above");
        self.notice = Some(self.attach_failed_notice(pending.profile));
        true
    }

    /// A sign-in landed. If this attach was waiting for it, the same attach
    /// goes out again under a new submission.
    pub(super) fn attach_resume_after_sign_in(&mut self) -> bool {
        let Some(pending) = self.pending_attach.as_mut() else {
            return false;
        };
        if !pending.awaiting_fresh_sign_in {
            return false;
        }
        pending.awaiting_fresh_sign_in = false;
        pending.submission = CommandSubmissionId::next();
        let profile = pending.profile;
        self.send_attach(profile);
        true
    }

    /// The sign-in this attach was waiting for was cancelled. Nothing was
    /// written on either side, and the button comes back.
    pub(super) fn attach_sign_in_cancelled(&mut self) -> bool {
        if self
            .pending_attach
            .as_ref()
            .is_none_or(|pending| !pending.awaiting_fresh_sign_in)
        {
            return false;
        }
        let pending = self.pending_attach.take().expect("checked above");
        self.notice = Some(self.attach_failed_notice(pending.profile));
        true
    }

    /// The local bind landed: both acknowledgements are in, and only now is
    /// the Profile *connected*. The catalog is re-read so the row's account
    /// label and its Cloud sync switch follow.
    pub(super) fn attach_bound(&mut self, profile: ProfileId) -> bool {
        if self
            .pending_attach
            .as_ref()
            .is_none_or(|pending| pending.profile != profile)
        {
            return false;
        }
        self.pending_attach = None;
        let name = self.profile_name(profile);
        self.notice = Some(fill(
            t(crate::format::lang(), TextKey::ProfileConnectDone),
            &[&name],
        ));
        self.pending = Some(AccountCommand::ListProfiles);
        true
    }

    /// The hosted record exists but this device could not record the binding.
    /// The Profile stays local-only here; the next click retries both halves,
    /// and the hosted half answers as an exact retry.
    pub(super) fn attach_bind_failed(&mut self, profile: ProfileId) -> bool {
        if self
            .pending_attach
            .as_ref()
            .is_none_or(|pending| pending.profile != profile)
        {
            return false;
        }
        self.pending_attach = None;
        let name = self.profile_name(profile);
        self.notice = Some(fill(
            t(crate::format::lang(), TextKey::ProfileConnectSaveFailed),
            &[&name],
        ));
        true
    }

    fn attach_failed_notice(&self, profile: ProfileId) -> String {
        let name = self.profile_name(profile);
        fill(
            t(crate::format::lang(), TextKey::ProfileConnectFailed),
            &[&name],
        )
    }
}
