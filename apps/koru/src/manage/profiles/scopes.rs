// SPDX-License-Identifier: MPL-2.0

//! Form state is owned by an immutable account scope. Local CAS/cleanup requests
//! stay on the controller's shared output lane and are drained even for parked
//! scopes.
//!
//! **Every controller field is either swapped with its scope or deliberately
//! shared**, and [`AccountForm::swap`] says which by destructuring both structs
//! without `..`: a field added to either one does not compile until it is put
//! on one side of that line.
use super::*;

#[cfg(test)]
mod tests;

pub(super) enum LocalEffect {
    Bind(ProfileId, Option<String>, String),
    Remove(
        ProfileId,
        bool,
        Option<koru_api_client::ProfileDeletionPreview>,
    ),
    HostedDelete(ProfileId, bool),
    Offline(ProfileId, String),
}

#[derive(Default)]
pub(super) struct AccountForm {
    cloud: cloud::State,
    selected: Option<ProfileId>,
    account: AccountState,
    account_profiles: Vec<AccountProfileRow>,
    account_profiles_loaded: bool,
    signed_in_identity: Option<(String, String)>,
    pending_unlink: Option<PendingUnlink>,
    pending_add_google: Option<PendingAddGoogle>,
    pending_rebind: Option<(ProfileId, String)>,
    attach_prompt: Option<(ProfileId, Confirm)>,
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
    remove_all_workspaces: bool,
    notice: Option<String>,
}

impl AccountForm {
    pub(super) fn swap(&mut self, controller: &mut ProfilesController) {
        let AccountForm {
            cloud,
            selected,
            account,
            account_profiles,
            account_profiles_loaded,
            signed_in_identity,
            pending_unlink,
            pending_add_google,
            pending_rebind,
            attach_prompt,
            pending_attach,
            inline_connect,
            account_warning_dismissed,
            creation_decision_pending,
            creation_submission,
            different_account_dismissed,
            profile_creation,
            profile_creation_request,
            profile_restore_request,
            profile_switch_request,
            uri_request,
            confirm_offline,
            confirm_remove,
            confirm_hosted_delete,
            pending_hosted_delete,
            discovered_hosted_delete,
            lifecycle_reconciled_generation,
            pending,
            remove_all_workspaces,
            notice,
        } = self;
        let ProfilesController {
            cloud: active_cloud,
            selected: active_selected,
            account: active_account,
            account_profiles: active_account_profiles,
            account_profiles_loaded: active_account_profiles_loaded,
            signed_in_identity: active_signed_in_identity,
            pending_unlink: active_pending_unlink,
            pending_add_google: active_pending_add_google,
            pending_rebind: active_pending_rebind,
            attach_prompt: active_attach_prompt,
            pending_attach: active_pending_attach,
            inline_connect: active_inline_connect,
            account_warning_dismissed: active_account_warning_dismissed,
            creation_decision_pending: active_creation_decision_pending,
            creation_submission: active_creation_submission,
            different_account_dismissed: active_different_account_dismissed,
            profile_creation: active_profile_creation,
            profile_creation_request: active_profile_creation_request,
            profile_restore_request: active_profile_restore_request,
            profile_switch_request: active_profile_switch_request,
            uri_request: active_uri_request,
            confirm_offline: active_confirm_offline,
            confirm_remove: active_confirm_remove,
            confirm_hosted_delete: active_confirm_hosted_delete,
            pending_hosted_delete: active_pending_hosted_delete,
            discovered_hosted_delete: active_discovered_hosted_delete,
            lifecycle_reconciled_generation: active_lifecycle_reconciled_generation,
            pending: active_pending,
            remove_all_workspaces: active_remove_all_workspaces,
            notice: active_notice,
            // Shared by every scope, deliberately. What this device holds and
            // what the window pushed in is the same whichever account is
            // active; the Cloud link, the poll and the Store client belong to
            // the tab; and the local requests are one output lane that is
            // drained for parked scopes too (see `capture_local_effects`).
            cloud_uri: _,
            cloud_visible: _,
            progress: _,
            progress_owner: _,
            local_effects: _,
            account_forms: _,
            retained_profiles: _,
            hidden_profile_count: _,
            profiles: _,
            current: _,
            current_workspace_count: _,
            workspace_counts: _,
            remove_request: _,
            deletion_preview: _,
            preview_request: _,
            hosted_delete_request: _,
            continue_offline_request: _,
            bind_request: _,
            client: _,
            hints: _,
        } = controller;
        std::mem::swap(cloud, active_cloud);
        std::mem::swap(selected, active_selected);
        std::mem::swap(account, active_account);
        std::mem::swap(account_profiles, active_account_profiles);
        std::mem::swap(account_profiles_loaded, active_account_profiles_loaded);
        std::mem::swap(signed_in_identity, active_signed_in_identity);
        std::mem::swap(pending_unlink, active_pending_unlink);
        std::mem::swap(pending_add_google, active_pending_add_google);
        std::mem::swap(pending_rebind, active_pending_rebind);
        std::mem::swap(attach_prompt, active_attach_prompt);
        std::mem::swap(pending_attach, active_pending_attach);
        std::mem::swap(inline_connect, active_inline_connect);
        std::mem::swap(account_warning_dismissed, active_account_warning_dismissed);
        std::mem::swap(creation_decision_pending, active_creation_decision_pending);
        std::mem::swap(creation_submission, active_creation_submission);
        std::mem::swap(
            different_account_dismissed,
            active_different_account_dismissed,
        );
        std::mem::swap(profile_creation, active_profile_creation);
        std::mem::swap(profile_creation_request, active_profile_creation_request);
        std::mem::swap(profile_restore_request, active_profile_restore_request);
        std::mem::swap(profile_switch_request, active_profile_switch_request);
        std::mem::swap(uri_request, active_uri_request);
        std::mem::swap(confirm_offline, active_confirm_offline);
        std::mem::swap(confirm_remove, active_confirm_remove);
        std::mem::swap(confirm_hosted_delete, active_confirm_hosted_delete);
        std::mem::swap(pending_hosted_delete, active_pending_hosted_delete);
        std::mem::swap(discovered_hosted_delete, active_discovered_hosted_delete);
        std::mem::swap(
            lifecycle_reconciled_generation,
            active_lifecycle_reconciled_generation,
        );
        std::mem::swap(pending, active_pending);
        std::mem::swap(remove_all_workspaces, active_remove_all_workspaces);
        std::mem::swap(notice, active_notice);
    }
}

impl ProfilesController {
    pub fn connection_paused(
        &mut self,
        owner_scope: u64,
        profile: ProfileId,
        retry: AccountCommand,
    ) -> bool {
        let active = self.account.active_scope;
        self.activate_scope(owner_scope);
        if let Some(form) = self
            .profile_creation
            .as_mut()
            .map(std::cell::RefCell::get_mut)
        {
            form.connection_paused(profile, retry);
        }
        self.activate_scope(active);
        true
    }
    pub fn connection_ready(&mut self, owner_scope: u64, profile: ProfileId) -> bool {
        let active = self.account.active_scope;
        self.activate_scope(owner_scope);
        self.profile_creation = None;
        self.selected = Some(profile);
        self.notice = Some(
            koru_i18n::t(
                crate::format::lang(),
                koru_i18n::Key::AccountProfileSavedTitle,
            )
            .into(),
        );
        self.activate_scope(active);
        true
    }
    pub(super) fn capture_local_effects(&mut self) {
        if let Some((profile, expected, next)) = self.bind_request.take() {
            self.local_effects
                .push_back(LocalEffect::Bind(profile, expected, next));
        }
        if let Some((profile, cascade)) = self.remove_request.take() {
            // The preview the confirmation was shown over travels with the
            // request, once: a second request asks for a fresh one.
            let preview = self.deletion_preview.take();
            self.local_effects
                .push_back(LocalEffect::Remove(profile, cascade, preview));
        }
        if let Some((profile, discovered)) = self.hosted_delete_request.take() {
            self.local_effects
                .push_back(LocalEffect::HostedDelete(profile, discovered));
        }
        if let Some((profile, expected)) = self.continue_offline_request.take() {
            self.local_effects
                .push_back(LocalEffect::Offline(profile, expected));
        }
    }
    pub(super) fn take_local_effect(
        &mut self,
        matches: impl Fn(&LocalEffect) -> bool,
    ) -> Option<LocalEffect> {
        self.capture_local_effects();
        let index = self.local_effects.iter().position(matches)?;
        self.local_effects.remove(index)
    }
    pub(super) fn apply_to_forms(&mut self, mut apply: impl FnMut(&mut Self) -> bool) -> bool {
        let active = self.account.active_scope;
        let mut changed = apply(self);
        self.capture_local_effects();
        let scopes: Vec<_> = self.account_forms.keys().copied().collect();
        for scope in scopes {
            self.activate_scope(scope);
            changed |= apply(self);
            self.capture_local_effects();
        }
        self.activate_scope(active);
        changed
    }
    pub(super) fn activate_scope(&mut self, scope: u64) {
        if self.account.active_scope == scope {
            return;
        }
        let old = self.account.active_scope;
        let mut next = self.account_forms.remove(&scope).unwrap_or_default();
        next.swap(self);
        self.account_forms.insert(old, next);
        self.account.active_scope = scope;
        // A different form is showing: its controls are not the ones focus or
        // the pointer were on.
        self.hints.reset_focus = true;
        self.hints.clear_hover = true;
    }

    pub fn complete_scoped(
        &mut self,
        owner_scope: u64,
        generation: u64,
        mut result: AccountOperationResult,
    ) -> bool {
        // Both Cloud reads are owner-correlated: a subscription answer and a
        // per-Profile sync answer are only ever accepted by the form that asked
        // for them, and neither is allowed to fall through to the general
        // handler, which would let a stale reply touch an unrelated account.
        let cloud = matches!(
            result,
            AccountOperationResult::CloudSubscription { .. }
                | AccountOperationResult::CloudSync { .. }
        );
        if cloud {
            if owner_scope != self.account.active_scope
                && !self.account_forms.contains_key(&owner_scope)
            {
                return false;
            }
            let active = self.account.active_scope;
            self.activate_scope(owner_scope);
            let handled = self.cloud_result(generation, &result);
            self.activate_scope(active);
            return handled;
        }
        // A bare `Failed` carries no request id, so it is offered to a Cloud
        // write that is waiting on this very generation before anything else
        // claims it — otherwise a refused switch would spin for ever.
        if matches!(result, AccountOperationResult::Failed { .. })
            && (owner_scope == self.account.active_scope
                || self.account_forms.contains_key(&owner_scope))
        {
            let active = self.account.active_scope;
            self.activate_scope(owner_scope);
            let handled = self.cloud_result(generation, &result);
            self.activate_scope(active);
            if handled {
                return true;
            }
        }
        self.capture_local_effects();
        if self.inline_connect_result_scoped(owner_scope, generation, &result) {
            return true;
        }
        if owner_scope == self.account.active_scope {
            if self.profile_creation.as_mut().is_some_and(|view| {
                view.get_mut()
                    .selection_result(owner_scope, generation, &result)
            }) {
                self.drain_profile_creation_action();
                return true;
            }
            if let AccountOperationResult::ProfilesSelected { profiles } = &mut result {
                *profiles = koru_workspace_host::account_ui::selection_result_rows(
                    &self.account,
                    generation,
                    std::mem::take(profiles),
                );
            }
            let changed = self.complete(result);
            self.capture_local_effects();
            return changed;
        }
        // Results are targeted by the shared worker. An unobserved scope has no
        // form intent to finish; canonical hosted acknowledgements are also
        // processed independently by the workspace host.
        if !self.account_forms.contains_key(&owner_scope) {
            return false;
        }
        let active = self.account.active_scope;
        self.activate_scope(owner_scope);
        if self.profile_creation.as_mut().is_some_and(|view| {
            view.get_mut()
                .selection_result(owner_scope, generation, &result)
        }) {
            self.activate_scope(active);
            return true;
        }
        let visible = std::mem::replace(&mut self.profiles, self.retained_profiles.clone());
        self.complete(result);
        self.capture_local_effects();
        self.profiles = visible;
        self.activate_scope(active);
        true
    }
}
