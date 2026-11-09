// SPDX-License-Identifier: MPL-2.0

//! Profile and account management, as state.
//!
//! [`ProfilesController`] is everything the Manage tab knows about Profiles: the device's
//! Profiles, the signed-in account and what it says about them, and every flow
//! in flight — unlinking an identity, adding a Google Account, attaching a
//! local-only Profile, removing one from this device or deleting it from Koru,
//! the embedded create form, Koru Cloud and its per-Profile switches.
//!
//! **It is a projection.** Agent commands leave through [`Effect`] and account
//! state is pushed back in by the shell, so browser, network, keyring and
//! Store work never run on the winit thread. The two exceptions are the
//! device-local transport-mode write (D258) and the Cloud progress poll, which
//! each run on a worker thread of their own and are only ever read back here.
//!
//! A view drives it with [`Intent`]s and draws from its accessors. What the
//! controller cannot do itself — move keyboard focus, clamp a scroll offset,
//! size the embedded form — it asks for through [`ViewHints`], which the view
//! takes after every call.

mod cloud;
mod creation;
mod effect;
mod inline_connect;
mod intent;
mod progress;
mod removal;
mod results;
mod scopes;
mod surface;

mod attach;
#[cfg(test)]
mod browser_signin_tests;
#[cfg(test)]
mod selection_tests;
#[cfg(test)]
mod tests;

pub use cloud::{Status as CloudStatus, SyncAvailability};
pub use effect::Effect;
pub use intent::{Intent, LinkControl, SessionAction};
pub use progress::{ProgressOwner, WAITING_CAP, waiting_lines};
#[cfg(test)]
pub(crate) use progress::{Waiting, fixtures as progress_fixtures};
pub use removal::HostedDeleteStage;

pub(crate) use koru_agent_wire::{
    AccountCommand, AccountOperationResult, AccountProfileRow, AccountStage, AccountState,
};
pub(crate) use koru_core::{ProfileId, ProfileSummary, WorkspaceEntry};
pub(crate) use koru_ui::confirm::{Confirm, Outcome as ConfirmOutcome};
pub(crate) use koru_workspace_host::ConnectedProfileDraft;

use attach::PendingAttach;
use removal::{DiscoveredHostedDeletion, PendingHostedDelete};
use scopes::{AccountForm, LocalEffect};

#[derive(Clone)]
pub(crate) struct PendingUnlink {
    profile_id: String,
    auth_identity_id: String,
    pub(crate) profile_name: String,
    awaiting_fresh_google: bool,
}

#[derive(Clone)]
pub(crate) struct PendingAddGoogle {
    candidate_id: String,
    profile_id: String,
    pub(crate) verified_email: String,
    pub(crate) profile_name: String,
    expected_identity_id: Option<String>,
    pub(crate) finishing: bool,
    authorization_committed: bool,
}

/// What a controller call asks of the view that drives it. Each flag is set
/// where the old view changed its own presentation state directly, and is
/// applied by the view as soon as the call returns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewHints {
    /// Keyboard focus goes back to the first control.
    pub reset_focus: bool,
    /// Nothing is hovered any more: the controls under the pointer changed.
    pub clear_hover: bool,
    /// The Profile list changed length; keep its scroll offset in range.
    pub clamp_scroll: bool,
    /// The embedded create form was just opened and has not been sized yet.
    pub size_creation_form: bool,
}

pub struct ProfilesController {
    cloud: cloud::State,
    cloud_uri: Option<String>,
    cloud_visible: bool,
    /// One Profile's Cloud sync progress, polled while it shows.
    progress: progress::Poll,
    /// Which Profile that is: the selected one, or Koru Cloud's disclosure.
    progress_owner: ProgressOwner,
    local_effects: std::collections::VecDeque<LocalEffect>,
    account_forms: std::collections::BTreeMap<u64, AccountForm>,
    retained_profiles: Vec<ProfileSummary>,
    hidden_profile_count: usize,
    profiles: Vec<ProfileSummary>,
    current: Option<ProfileId>,
    current_workspace_count: usize,
    workspace_counts: std::collections::HashMap<ProfileId, usize>,
    selected: Option<ProfileId>,
    account: AccountState,
    account_profiles: Vec<AccountProfileRow>,
    account_profiles_loaded: bool,
    /// Correlated session metadata only. Never derive this from a Profile edge
    /// or copy it into broadcast AccountState.
    signed_in_identity: Option<(String, String)>,
    pending_unlink: Option<PendingUnlink>,
    pending_add_google: Option<PendingAddGoogle>,
    pending_rebind: Option<(ProfileId, String)>,
    /// The *Connect to your account* question, while it is up.
    attach_prompt: Option<(ProfileId, Confirm)>,
    /// A local-only Profile's attach in flight: the hosted write, then the
    /// local bind on its result.
    pending_attach: Option<PendingAttach>,
    inline_connect: Option<inline_connect::PendingConnect>,
    account_warning_dismissed: bool,
    creation_decision_pending: bool,
    creation_submission: Option<koru_workspace_host::account_ui::CommandSubmissionId>,
    different_account_dismissed: bool,
    profile_creation: Option<std::cell::RefCell<koru_workspace_host::onboarding::OnboardingView>>,
    profile_creation_request: Option<ConnectedProfileDraft>,
    profile_restore_request: Option<ProfileId>,
    profile_switch_request: Option<ProfileId>,
    uri_request: Option<&'static str>,
    confirm_offline: bool,
    confirm_remove: bool,
    confirm_hosted_delete: Option<(ProfileId, Confirm)>,
    pending_hosted_delete: Option<PendingHostedDelete>,
    discovered_hosted_delete: Option<DiscoveredHostedDeletion>,
    lifecycle_reconciled_generation: Option<u64>,
    pending: Option<AccountCommand>,
    remove_request: Option<(ProfileId, bool)>,
    /// The deletion preview the remove confirmation is shown over, when the
    /// removal is the shared batch (root-catalog §6.1): what the dialog
    /// lists, and what the Store checks against when the person confirms.
    deletion_preview: Option<koru_api_client::ProfileDeletionPreview>,
    /// A preview being read on a worker, for `profile`.
    preview_request: Option<(
        ProfileId,
        std::sync::mpsc::Receiver<
            koru_store_core::errors::Result<koru_api_client::ProfileDeletionPreview>,
        >,
    )>,
    hosted_delete_request: Option<(ProfileId, bool)>,
    continue_offline_request: Option<(ProfileId, String)>,
    remove_all_workspaces: bool,
    bind_request: Option<(ProfileId, Option<String>, String)>,
    notice: Option<String>,
    /// The local Store, for the one thing this controller writes that is not
    /// an Agent command: a Profile's **device-local transport mode** (D258),
    /// and for the Cloud progress poll.
    ///
    /// Pushed in by the frame, like every other panel's, and cloned onto a
    /// worker thread before it is used — no Store work runs on the winit
    /// thread.
    client: Option<koru_api_client::Client>,
    hints: ViewHints,
}

impl Default for ProfilesController {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfilesController {
    pub fn new() -> Self {
        Self {
            cloud: Default::default(),
            cloud_uri: None,
            cloud_visible: false,
            progress: Default::default(),
            progress_owner: ProgressOwner::Selected,
            local_effects: Default::default(),
            account_forms: Default::default(),
            retained_profiles: Vec::new(),
            hidden_profile_count: 0,
            profiles: Vec::new(),
            current: None,
            current_workspace_count: 0,
            workspace_counts: std::collections::HashMap::new(),
            selected: None,
            account: AccountState::default(),
            account_profiles: Vec::new(),
            account_profiles_loaded: false,
            signed_in_identity: None,
            pending_unlink: None,
            pending_add_google: None,
            pending_rebind: None,
            attach_prompt: None,
            pending_attach: None,
            inline_connect: None,
            account_warning_dismissed: false,
            creation_decision_pending: false,
            creation_submission: None,
            different_account_dismissed: false,
            profile_creation: None,
            profile_creation_request: None,
            profile_restore_request: None,
            profile_switch_request: None,
            uri_request: None,
            confirm_offline: false,
            confirm_remove: false,
            confirm_hosted_delete: None,
            pending_hosted_delete: None,
            discovered_hosted_delete: None,
            lifecycle_reconciled_generation: None,
            // Resolve the device-wide session only when this singleton tab is
            // opened. Signed-in completion then requests the targeted identity.
            pending: Some(AccountCommand::Status),
            remove_request: None,
            deletion_preview: None,
            preview_request: None,
            hosted_delete_request: None,
            continue_offline_request: None,
            remove_all_workspaces: false,
            bind_request: None,
            notice: None,
            client: None,
            hints: ViewHints::default(),
        }
    }

    /// What the last calls asked of the view, cleared as it is read.
    pub fn take_hints(&mut self) -> ViewHints {
        std::mem::take(&mut self.hints)
    }

    // ---- pushed in -----------------------------------------------------------

    /// The frame's already-discovered Store client. No request happens here.
    pub fn set_client(&mut self, client: Option<koru_api_client::Client>) {
        self.client = client;
    }

    pub fn set_retained_profiles(&mut self, profiles: Vec<ProfileSummary>) {
        self.retained_profiles = profiles;
    }

    pub fn set_hidden_profile_count(&mut self, count: usize) {
        self.hidden_profile_count = count;
    }

    pub fn set_workspace_counts(
        &mut self,
        counts: std::collections::HashMap<ProfileId, usize>,
    ) -> bool {
        if self.workspace_counts == counts {
            return false;
        }
        self.workspace_counts = counts;
        true
    }

    pub fn set_profiles(
        &mut self,
        profiles: Vec<ProfileSummary>,
        current: ProfileId,
        workspaces: &[WorkspaceEntry],
    ) -> bool {
        let changed = self.profiles != profiles
            || self.current != Some(current)
            || self.current_workspace_count != workspaces.len();
        if !changed {
            return false;
        }
        self.profiles = profiles;
        self.inline_connect_catalog_ready();
        self.current = Some(current);
        self.current_workspace_count = workspaces.len();
        if self
            .selected
            .is_none_or(|id| !self.profiles.iter().any(|profile| profile.id == id))
        {
            self.selected = self
                .profiles
                .iter()
                .find(|profile| profile.id == current)
                .or_else(|| self.profiles.first())
                .map(|profile| profile.id);
        }
        self.hints.clamp_scroll = true;
        true
    }

    pub fn set_account(&mut self, state: AccountState) -> bool {
        if self.account == state {
            return false;
        }
        let changed_scope = self.account.active_scope != state.active_scope;
        self.activate_scope(state.active_scope);
        if changed_scope
            || self.account.active_account_id != state.active_account_id
            || matches!(
                state.stage,
                AccountStage::SignedOut
                    | AccountStage::Cancelled
                    | AccountStage::Failed
                    | AccountStage::Unavailable
            )
        {
            self.cloud.invalidate();
            self.cloud_uri = None;
        }
        if self.pending == Some(AccountCommand::Status) {
            self.pending = None;
        }
        let session_changed = self.account.active_account_id != state.active_account_id
            || self.account.stage != state.stage;
        if matches!(
            state.stage,
            AccountStage::SignedIn | AccountStage::SignedOut | AccountStage::Cancelled
        ) {
            self.creation_decision_pending = false;
            self.creation_submission = None;
        }
        if let Some(creation) = self
            .profile_creation
            .as_mut()
            .map(std::cell::RefCell::get_mut)
        {
            creation.observe_account_projection(&state);
        }
        if let Some(creation) = self
            .profile_creation
            .as_mut()
            .map(std::cell::RefCell::get_mut)
            && (matches!(
                state.stage,
                AccountStage::AwaitingAccountCreation
                    | AccountStage::Restoring
                    | AccountStage::WaitingForBrowser
                    | AccountStage::Exchanging
                    | AccountStage::Refreshing
                    | AccountStage::Unavailable
                    | AccountStage::Failed
            ) || state.enrollment != koru_agent_wire::EnrollmentState::Idle)
        {
            creation.show_account_state(&state);
        }
        self.account = state;
        if self.account.stage != AccountStage::SignedIn {
            self.signed_in_identity = None;
            self.account_profiles.clear();
            self.account_profiles_loaded = false;
            self.different_account_dismissed = false;
            self.lifecycle_reconciled_generation = None;
            self.discovered_hosted_delete = None;
        } else {
            if session_changed {
                self.account_profiles.clear();
                self.account_profiles_loaded = false;
                self.different_account_dismissed = false;
            }
            if self.signed_in_identity.is_none() && self.pending.is_none() {
                self.pending = Some(AccountCommand::InspectAccountSession);
            }
        }
        self.account_warning_dismissed = false;
        self.drain_profile_creation_action();
        true
    }

    // ---- read by a view ------------------------------------------------------

    pub fn account_scope(&self) -> u64 {
        self.account.active_scope
    }
    pub fn account(&self) -> &AccountState {
        &self.account
    }
    pub fn profiles(&self) -> &[ProfileSummary] {
        &self.profiles
    }
    pub fn selected(&self) -> Option<ProfileId> {
        self.selected
    }
    /// The page moved to `profile` by itself — a removed Profile's page
    /// falling back to the open one — rather than because somebody chose it.
    ///
    /// Unlike [`Intent::Select`] this keeps the notice: what it says is the
    /// result that caused the move ("Profile removed from this device"), and
    /// the page it lands on is where that result is read.
    pub fn follow_page(&mut self, profile: ProfileId) {
        self.selected = Some(profile);
        self.confirm_offline = false;
    }
    /// The Profile this window has open.
    pub fn current(&self) -> Option<ProfileId> {
        self.current
    }
    /// The selected Profile's row, when it is one of this device's.
    pub fn selected_profile(&self) -> Option<&ProfileSummary> {
        self.selected
            .and_then(|id| self.profiles.iter().find(|profile| profile.id == id))
    }
    pub fn hidden_profile_count(&self) -> usize {
        self.hidden_profile_count
    }
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }
    pub fn pending_add_google(&self) -> Option<&PendingAddGoogle> {
        self.pending_add_google.as_ref()
    }
    /// The name the offline-only question is about, while it is up.
    pub fn pending_unlink_name(&self) -> Option<&str> {
        self.pending_unlink
            .as_ref()
            .map(|pending| pending.profile_name.as_str())
    }
    pub fn confirming_offline(&self) -> bool {
        self.confirm_offline
    }
    /// `Some(cascade)` while the remove-from-this-device question is up.
    pub fn confirming_remove(&self) -> Option<bool> {
        self.confirm_remove.then_some(self.remove_all_workspaces)
    }
    /// The deletion preview the open remove question is about, when the
    /// removal is the shared batch: its targets are what the dialog lists.
    pub fn deletion_preview(&self) -> Option<&koru_api_client::ProfileDeletionPreview> {
        self.deletion_preview.as_ref()
    }

    /// Read what deleting `profile` would trash, off this thread, and raise
    /// the remove question over it when it arrives (root-catalog §6.1: the
    /// confirmation is about the captured list, and a stale one is refused).
    pub(crate) fn request_deletion_preview(&mut self, profile: ProfileId) -> bool {
        let Some(client) = self.client.clone() else {
            return false;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let started = std::thread::Builder::new()
            .name("koru-profile-deletion-preview".into())
            .spawn(move || {
                let store_profile = koru_store_core::ids::ProfileId::from_uuid(profile.as_uuid());
                let _ = tx.send(client.store().profile_deletion_preview(store_profile));
            });
        if started.is_err() {
            return false;
        }
        self.deletion_preview = None;
        self.preview_request = Some((profile, rx));
        true
    }

    fn drain_deletion_preview(&mut self) -> bool {
        let Some((profile, receiver)) = &self.preview_request else {
            return false;
        };
        let profile = *profile;
        let answer = match receiver.try_recv() {
            Ok(answer) => answer,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(koru_store_core::Error::bare(
                koru_store_core::Code::StorageFailed,
            )),
        };
        self.preview_request = None;
        // The person moved on; what the preview says is about a Profile no
        // longer under the question.
        if self.selected != Some(profile) {
            return true;
        }
        match answer {
            Ok(preview) => {
                self.deletion_preview = Some(preview);
                self.remove_all_workspaces = true;
                self.confirm_remove = true;
                self.hints.reset_focus = true;
            }
            Err(_) => {
                self.notice = Some(
                    koru_i18n::t(
                        crate::format::lang(),
                        koru_i18n::Key::ProfileDeletePreviewFailed,
                    )
                    .into(),
                );
            }
        }
        true
    }
    /// The hosted deletion in flight: its Profile and stage.
    pub fn hosted_delete(&self) -> Option<(ProfileId, HostedDeleteStage)> {
        self.pending_hosted_delete
            .as_ref()
            .map(|pending| (pending.profile, pending.stage))
    }
    /// The hosted deletion's heading and body, for the Profile it is about.
    pub fn hosted_delete_copy_for(&self, profile: ProfileId) -> Option<(String, String)> {
        self.pending_hosted_delete
            .as_ref()
            .filter(|pending| pending.profile == profile)
            .map(removal::pending_copy)
    }
    /// A deletion found on another device: its Profile and name.
    pub fn discovered_deletion(&self) -> Option<(ProfileId, &str)> {
        self.discovered_hosted_delete
            .as_ref()
            .map(|found| (found.profile, found.profile_name.as_str()))
    }
    /// The Profile an inline Connect is for, and whether it is uncertain.
    pub fn inline_connect(&self) -> Option<(ProfileId, bool)> {
        self.inline_connect
            .as_ref()
            .map(|pending| (pending.profile, pending.uncertain))
    }
    /// The embedded form, for input that raises nothing (pointer moves, the
    /// wheel, release) and for sizing.
    pub fn creation_form_mut(
        &mut self,
    ) -> Option<&mut koru_workspace_host::onboarding::OnboardingView> {
        self.profile_creation
            .as_mut()
            .map(std::cell::RefCell::get_mut)
    }
    /// The embedded form, for what it says about itself (its pointer shape).
    pub fn creation_form(
        &self,
    ) -> Option<std::cell::Ref<'_, koru_workspace_host::onboarding::OnboardingView>> {
        self.profile_creation
            .as_ref()
            .map(std::cell::RefCell::borrow)
    }

    /// Semantic projection measures the existing form only when a provider
    /// asks. Interior mutability is limited to that form's layout/identity.
    pub fn creation_semantics(
        &self,
        window: koru_workspace_host::accessibility::WindowSlot,
        view: koru_workspace_host::accessibility::ViewSlot,
    ) -> Vec<koru_workspace_host::accessibility::SemanticNode> {
        self.profile_creation
            .as_ref()
            .map(|form| form.borrow_mut().accessibility_semantics(window, view))
            .unwrap_or_default()
    }
    /// Input into the embedded form that may raise an action: run `input`,
    /// then act on whatever the form asked for.
    pub fn with_creation_form<R>(
        &mut self,
        input: impl FnOnce(&mut koru_workspace_host::onboarding::OnboardingView) -> R,
    ) -> Option<R> {
        let result = input(self.profile_creation.as_mut()?.get_mut());
        self.drain_profile_creation_action();
        Some(result)
    }
    /// Every question up, in paint order.
    pub fn prompts(&self) -> impl Iterator<Item = &Confirm> {
        [
            self.confirm_hosted_delete.as_ref(),
            self.cloud.move_prompt.as_ref(),
            self.attach_prompt.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(|(_, confirm)| confirm)
    }
    /// The question a view forwards input to, with the intent that settles it.
    pub fn prompt_mut(&mut self) -> Option<(&mut Confirm, fn(ConfirmOutcome) -> Intent)> {
        if let Some((_, confirm)) = self.confirm_hosted_delete.as_mut() {
            return Some((confirm, Intent::AnswerHostedDelete));
        }
        if let Some((_, confirm)) = self.cloud.move_prompt.as_mut() {
            return Some((confirm, Intent::AnswerCloudMove));
        }
        self.attach_prompt
            .as_mut()
            .map(|(_, confirm)| (confirm, Intent::AnswerAttach as fn(_) -> _))
    }

    // ---- the clock -----------------------------------------------------------

    /// **The one clock this controller contributes**, and only while a
    /// transport mode write it started is unanswered or the Cloud progress
    /// section is showing.
    pub fn maintenance_deadline(&self) -> Option<std::time::Instant> {
        let transport = (self.cloud_move_pending() || self.preview_request.is_some())
            .then(|| std::time::Instant::now() + std::time::Duration::from_millis(16));
        match (transport, self.progress.deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    pub fn maintain(&mut self, now: std::time::Instant) -> bool {
        self.drain_transport_answer()
            | self.drain_deletion_preview()
            | self.maintain_cloud_progress(now)
    }
}

#[cfg(test)]
impl ProfilesController {
    pub(crate) fn profiles_mut(&mut self) -> &mut Vec<ProfileSummary> {
        &mut self.profiles
    }
    pub(crate) fn account_mut(&mut self) -> &mut AccountState {
        &mut self.account
    }
    pub(crate) fn account_profiles_mut(&mut self) -> &mut Vec<AccountProfileRow> {
        &mut self.account_profiles
    }
    pub(crate) fn clear_pending(&mut self) {
        self.pending = None;
    }
    pub(crate) fn set_cloud_status(&mut self, status: CloudStatus) {
        self.cloud.status = status;
    }
    pub(crate) fn offer_add_google(&mut self, profile: ProfileId, email: &str, name: &str) {
        self.pending_add_google = Some(PendingAddGoogle {
            candidate_id: "candidate".into(),
            profile_id: profile.to_storage(),
            verified_email: email.into(),
            profile_name: name.into(),
            expected_identity_id: None,
            finishing: false,
            authorization_committed: false,
        });
    }
    pub(crate) fn progress_mut(&mut self) -> &mut progress::Poll {
        &mut self.progress
    }
    pub(crate) fn set_signed_in_identity(&mut self, provider: &str, email: &str) {
        self.signed_in_identity = Some((provider.into(), email.into()));
    }
    pub(crate) fn set_account_profiles_loaded(&mut self, loaded: bool) {
        self.account_profiles_loaded = loaded;
    }
    pub(crate) fn start_hosted_delete(
        &mut self,
        profile: ProfileId,
        name: &str,
        stage: HostedDeleteStage,
    ) {
        self.pending_hosted_delete = Some(PendingHostedDelete {
            profile,
            profile_name: name.into(),
            stage,
        });
    }
}
