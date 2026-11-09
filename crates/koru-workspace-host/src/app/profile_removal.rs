// SPDX-License-Identifier: MPL-2.0

//! Recoverable local cleanup after either a device-local removal or a hosted
//! Profile deletion.
//!
//! Hosted deletion is server-first. This module owns every later local stage
//! and acknowledges each durable boundary back to the Agent journal. Keeping
//! the sequence outside `app.rs` makes it one resumable workflow rather than a
//! set of UI callbacks that happen to run in the right order.

use super::*;
use koru_store_core::ids::RemovalAttemptId;
use koru_store_core::profile_removal::RemovalKind;

pub(super) struct ProfileRemovalCleanup {
    pub(super) profile: koru_core::ProfileId,
    pub(super) result: Result<(), String>,
}

pub(super) struct ProfileCascadeCompletion {
    pub(super) profile: koru_core::ProfileId,
    pub(super) result: Result<CascadeReady, CascadeFailure>,
}

pub(super) enum CascadeReady {
    Complete(RemovalAttemptId),
    AlreadyRemoved,
    /// The shared deletion batch committed (root-catalog §6.1): the Profile
    /// is hidden and its captured workspaces are in the Trash together.
    /// What follows is the same local cleanup a removal gets.
    Deleted,
}

/// Why a cascade stopped, as the code the Profiles page words.
///
/// Two codes because the two failures want opposite advice. One that stopped
/// part-way (the Store or Gateway refused, or went away) can be retried and
/// carries on from where it got to. One this window was not *permitted* to
/// run — no first-party grant, so `workspace.destroy` was never admitted
/// (ADR-267) — fails identically on every retry, and telling somebody to try
/// again would send them round the same loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CascadeFailure {
    NotPermitted,
    Unsupported,
    Incomplete,
    /// The Store refused the deletion batch because the workspaces under the
    /// Profile changed after the preview was shown (`idempotency_conflict`,
    /// root-catalog §6.1). Nothing moved; the page refreshes the preview and
    /// asks again.
    StalePreview,
}

impl CascadeFailure {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::NotPermitted => "profile_cascade_not_permitted",
            Self::Unsupported => "profile_cascade_unsupported",
            Self::Incomplete => "profile_cascade_incomplete",
            Self::StalePreview => "profile_deletion_preview_stale",
        }
    }
}

/// **Delete a Profile as the shared batch** (root-catalog §6.1, §6.3) over
/// the preview the person confirmed. The Store commits the Profile's deletion
/// fact, one on every captured workspace, their Trash projections and the
/// local application job in one transaction, or refuses: a stale preview is
/// [`CascadeFailure::StalePreview`], a Profile already gone is verified as
/// such, and anything else is retryable. The device-local cleanup that
/// follows (settings, the remembered Profile) is the same as a removal's.
fn delete_profile_batch(
    root: Option<&std::path::Path>,
    profile: koru_core::ProfileId,
    preview: &koru_api_client::ProfileDeletionPreview,
) -> Result<CascadeReady, CascadeFailure> {
    let client = cascade_client(root)?;
    // Checked first, so an unvouched window is told so rather than refused
    // by the Store's scope check with a less useful word.
    if !client.catalog().may_destroy() {
        return Err(CascadeFailure::NotPermitted);
    }
    let store_profile = koru_store_core::ids::ProfileId::from_uuid(profile.as_uuid());
    match client.store().delete_profile(store_profile, preview) {
        Ok(_batch) => Ok(CascadeReady::Deleted),
        Err(error) if error.code() == koru_store_core::Code::IdempotencyConflict => {
            Err(CascadeFailure::StalePreview)
        }
        Err(error)
            if matches!(
                error.code(),
                koru_store_core::Code::ProfileNotFound | koru_store_core::Code::ProfileRemoved
            ) =>
        {
            verify_removed(&client, store_profile).map(|()| CascadeReady::AlreadyRemoved)
        }
        Err(_) => Err(CascadeFailure::Incomplete),
    }
}

pub(super) struct ProfileOfflineCompletion {
    pub(super) profile: koru_core::ProfileId,
    pub(super) result: Result<(), String>,
}

/// Whether a Profile that left the catalog is a shared deletion the Store
/// can list (root-catalog Plan 2.2). `deleted: false` is a real answer —
/// a device-local removal in another window, or a Store that could not be
/// asked — and leaves the window alone.
pub(super) struct RemoteDeletionCheck {
    pub(super) profile: koru_core::ProfileId,
    pub(super) deleted: bool,
}

/// Ask the Store whether `profile` carries an active shared deletion. Off
/// the loop: a gateway round trip. A read, so it asks for the reading
/// scopes only — a window the Agent will not vouch for the destructive
/// scope still has to settle when its Profile is gone.
fn remote_deletion_confirmed(
    root: Option<&std::path::Path>,
    profile: koru_core::ProfileId,
) -> bool {
    const SCOPES: &[&str] = &["store.read_documents", "store.manage_workspaces"];
    let segment = koru_workspace::fromstore::segment();
    let client = match root {
        Some(root) => koru_api_client::Client::connect_at_first_party(segment, root, SCOPES),
        None => koru_api_client::Client::connect_first_party(segment, SCOPES),
    };
    let Ok(client) = client else {
        return false;
    };
    let store_profile = koru_store_core::ids::ProfileId::from_uuid(profile.as_uuid());
    client
        .store()
        .profile_deletions()
        .map(|deletions| {
            deletions
                .iter()
                .any(|listing| listing.deletion.profile == store_profile)
        })
        .unwrap_or(false)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HostedProfileRemovalSource {
    Origin,
    Peer,
    Reconciliation,
}

/// Move every active Workspace owned by one Profile to recoverable Trash.
///
/// The error's detail is not kept: no caller showed it, and the code is what
/// the page needs.
fn cascade_profile_workspaces(
    root: Option<&std::path::Path>,
    profile: koru_core::ProfileId,
    kind: RemovalKind,
) -> Result<CascadeReady, CascadeFailure> {
    let client = cascade_client(root)?;
    // Checked before Begin, so a permission refusal persists no mark.
    if !client.catalog().may_destroy() {
        return Err(CascadeFailure::NotPermitted);
    }
    let store_profile = koru_store_core::ids::ProfileId::from_uuid(profile.as_uuid());
    let mark = match client.catalog().begin_profile_removal(store_profile, kind) {
        Ok(mark) => mark,
        Err(koru_api_client::WorkspaceError::Api(failure))
            if failure.code == "profile_not_found" =>
        {
            return verify_removed(&client, store_profile).map(|()| CascadeReady::AlreadyRemoved);
        }
        Err(koru_api_client::WorkspaceError::Api(failure)) if failure.status == 404 => {
            return Err(CascadeFailure::Unsupported);
        }
        Err(_) => return Err(CascadeFailure::Incomplete),
    };
    let result = cascade_steps(&client, store_profile, mark.attempt);
    if result.is_err() {
        let _ = client
            .catalog()
            .stop_profile_removal(store_profile, mark.attempt);
    }
    result.map(|()| CascadeReady::Complete(mark.attempt))
}

/// Completion clears the attempt atomically with removing the Profile. A lost
/// reply therefore cannot be recovered by replaying the attempt. Require both
/// authoritative listings before continuing the still-owed settings cleanup.
fn verify_removed(
    client: &koru_api_client::Client,
    profile: koru_store_core::ProfileId,
) -> Result<(), CascadeFailure> {
    let workspaces = client
        .catalog()
        .workspaces()
        .map_err(|_| CascadeFailure::Incomplete)?;
    let profiles = client
        .store()
        .profiles()
        .map_err(|_| CascadeFailure::Incomplete)?;
    removal_absence(
        profiles.iter().any(|row| row.id == profile),
        workspaces
            .iter()
            .any(|row| row.owner == profile && row.deleted_at.is_none()),
    )
}

fn removal_absence(
    profile_present: bool,
    has_retained_workspace: bool,
) -> Result<(), CascadeFailure> {
    if profile_present || has_retained_workspace {
        Err(CascadeFailure::Incomplete)
    } else {
        Ok(())
    }
}

pub(super) fn cascade_client(
    root: Option<&std::path::Path>,
) -> Result<koru_api_client::Client, CascadeFailure> {
    const SCOPES: &[&str] = &[
        "store.read_documents",
        "store.manage_workspaces",
        koru_api_client::WORKSPACE_DESTROY_SCOPE,
    ];
    let segment = koru_workspace::fromstore::segment();
    match root {
        Some(root) => koru_api_client::Client::connect_at_first_party(segment, root, SCOPES),
        None => koru_api_client::Client::connect_first_party(segment, SCOPES),
    }
    .map_err(|_| CascadeFailure::Incomplete)
}

fn cascade_steps(
    client: &koru_api_client::Client,
    store_profile: koru_store_core::ids::ProfileId,
    attempt: RemovalAttemptId,
) -> Result<(), CascadeFailure> {
    let mut workspaces = client
        .catalog()
        .workspaces()
        .map_err(|_| CascadeFailure::Incomplete)?
        .into_iter()
        .filter(|workspace| workspace.owner == store_profile && workspace.deleted_at.is_none())
        .collect::<Vec<_>>();
    workspaces.sort_by_key(|workspace| workspace.id.as_uuid());

    for workspace in workspaces {
        let id = workspace.id.to_storage();
        let confirmation = client
            .catalog()
            .confirmation(&id, koru_api_client::WorkspaceLifecycleAction::SoftDelete)
            .map_err(|_| CascadeFailure::Incomplete)?;
        client
            .catalog()
            .lifecycle_for_removal(&id, &confirmation.confirmation, attempt)
            .map_err(|_| CascadeFailure::Incomplete)?;
    }
    Ok(())
}

impl<H: Host> App<H> {
    pub(super) fn start_continue_profile_offline(
        &mut self,
        profile: koru_core::ProfileId,
        expected_identity_id: String,
    ) {
        if !self.profile_offline_in_flight.insert(profile) {
            return;
        }
        let root = self.gateway_root.clone();
        let tx = self.profile_offline_tx.clone();
        let proxy = self.wake_proxy.clone();
        let started = std::thread::Builder::new()
            .name("koru-profile-offline".into())
            .spawn(move || {
                let result = crate::profile_lifecycle_decision::continue_offline(
                    root.as_deref(),
                    profile,
                    &expected_identity_id,
                );
                let _ = tx.send(ProfileOfflineCompletion { profile, result });
                let _ = proxy.send_event(Wake::Profiles);
            });
        if started.is_err() {
            self.profile_offline_in_flight.remove(&profile);
            for pane in self.panes.values_mut() {
                let _ = pane.view.deleted_profile_continue_offline_failed(profile);
            }
            self.invalidate_everywhere();
        }
    }

    fn service_profile_offline(&mut self) {
        while let Ok(completion) = self.profile_offline_rx.try_recv() {
            self.profile_offline_in_flight.remove(&completion.profile);
            let mut changed = false;
            for pane in self.panes.values_mut() {
                changed |= if completion.result.is_ok() {
                    pane.view
                        .deleted_profile_continued_offline(completion.profile)
                } else {
                    pane.view
                        .deleted_profile_continue_offline_failed(completion.profile)
                };
            }
            if completion.result.is_ok() {
                if let Some(profile) = self
                    .runtime
                    .profile_catalog
                    .iter_mut()
                    .find(|row| row.id == completion.profile)
                {
                    profile.account = None;
                }
                self.runtime.request_catalog_refresh();
            }
            if changed {
                self.invalidate_everywhere();
            }
        }
    }

    /// Confirm a vanished open Profile as a shared deletion before the
    /// window settles (root-catalog Plan 2.2, §6.3). One check per Profile
    /// at a time; the answer arrives through
    /// [`Self::service_remote_deletion_checks`].
    pub(super) fn start_remote_deletion_check(&mut self, profile: koru_core::ProfileId) {
        if !self.remote_deletion_checks_in_flight.insert(profile) {
            return;
        }
        let root = self.gateway_root.clone();
        let tx = self.remote_deletion_tx.clone();
        let proxy = self.wake_proxy.clone();
        let started = std::thread::Builder::new()
            .name("koru-profile-remote-deletion".into())
            .spawn(move || {
                let deleted = remote_deletion_confirmed(root.as_deref(), profile);
                let _ = tx.send(RemoteDeletionCheck { profile, deleted });
                let _ = proxy.send_event(Wake::Profiles);
            });
        if started.is_err() {
            self.remote_deletion_checks_in_flight.remove(&profile);
        }
    }

    /// A confirmed shared deletion of the open Profile settles the window
    /// the way a committed local batch does: the Store has already fenced
    /// its writers and trashed its Workspaces, so what remains is this
    /// device's cleanup and the move to another Profile or to
    /// `NeedsProfile`. Nothing is created and nothing is recreated.
    fn service_remote_deletion_checks(&mut self) {
        while let Ok(check) = self.remote_deletion_rx.try_recv() {
            self.remote_deletion_checks_in_flight.remove(&check.profile);
            if !check.deleted {
                continue;
            }
            // Still the open Profile, and still gone: a Profile that came
            // back (a restore raced the check) is left alone.
            if self.runtime.profile_id() != check.profile
                || self
                    .runtime
                    .profile_catalog
                    .iter()
                    .any(|row| row.id == check.profile)
            {
                continue;
            }
            self.remote_profile_deletions.insert(check.profile);
            self.begin_removed_profile_cleanup(check.profile);
        }
    }

    pub(super) fn start_profile_cascade(&mut self, profile: koru_core::ProfileId) {
        if !self.profile_cascades_in_flight.insert(profile) {
            return;
        }
        let root = self.gateway_root.clone();
        let tx = self.profile_cascade_tx.clone();
        let proxy = self.wake_proxy.clone();
        let kind = if self.hosted_profile_removals.contains_key(&profile) {
            RemovalKind::HostedCleanup
        } else {
            RemovalKind::Local
        };
        let started = std::thread::Builder::new()
            .name("koru-profile-cascade".into())
            .spawn(move || {
                let result = cascade_profile_workspaces(root.as_deref(), profile, kind);
                let _ = tx.send(ProfileCascadeCompletion { profile, result });
                let _ = proxy.send_event(Wake::Profiles);
            });
        if started.is_err() {
            self.profile_cascades_in_flight.remove(&profile);
            let mut changed = false;
            for pane in self.panes.values_mut() {
                changed |= pane
                    .view
                    .local_profile_remove_failed(profile, CascadeFailure::Incomplete.code());
            }
            if changed {
                self.invalidate_everywhere();
            }
        }
    }

    /// The shared deletion batch for a Profile the person confirmed over
    /// `preview` (Manage's *Delete profile…*). Off the loop, like the cascade;
    /// its completion takes the cascade's channel and the same cleanup.
    pub(super) fn start_profile_deletion(
        &mut self,
        profile: koru_core::ProfileId,
        preview: koru_api_client::ProfileDeletionPreview,
    ) {
        if !self.profile_cascades_in_flight.insert(profile) {
            return;
        }
        let root = self.gateway_root.clone();
        let tx = self.profile_cascade_tx.clone();
        let proxy = self.wake_proxy.clone();
        let started = std::thread::Builder::new()
            .name("koru-profile-delete".into())
            .spawn(move || {
                let result = delete_profile_batch(root.as_deref(), profile, &preview);
                let _ = tx.send(ProfileCascadeCompletion { profile, result });
                let _ = proxy.send_event(Wake::Profiles);
            });
        if started.is_err() {
            self.profile_cascades_in_flight.remove(&profile);
            let mut changed = false;
            for pane in self.panes.values_mut() {
                changed |= pane
                    .view
                    .local_profile_remove_failed(profile, CascadeFailure::Incomplete.code());
            }
            if changed {
                self.invalidate_everywhere();
            }
        }
    }

    pub(super) fn acknowledge_profile_cleanup(
        &self,
        profile: koru_core::ProfileId,
        stage: koru_agent_wire::ProfileCleanupStage,
    ) {
        self.host
            .account_command(koru_agent_wire::AccountCommand::AcknowledgeProfileCleanup {
                profile_id: profile.to_storage(),
                stage,
            });
    }

    pub(super) fn service_profile_cascades(&mut self) {
        while let Ok(completion) = self.profile_cascade_rx.try_recv() {
            match completion.result {
                Ok(CascadeReady::Complete(attempt)) => {
                    self.profile_removal_attempts
                        .insert(completion.profile, attempt);
                    if self
                        .hosted_profile_removals
                        .contains_key(&completion.profile)
                    {
                        self.acknowledge_profile_cleanup(
                            completion.profile,
                            koru_agent_wire::ProfileCleanupStage::WorkspacesRemoved,
                        );
                    }
                    self.host.remove_local_profile(completion.profile, attempt)
                }
                // The batch is committed and the Profile hidden; the Store
                // has already fenced its writers. What remains is this
                // device's own cleanup, then the window settles — onto another
                // Profile, or by relaunching into `NeedsProfile` when none is
                // left (root-catalog §6.3).
                Ok(CascadeReady::AlreadyRemoved | CascadeReady::Deleted) => {
                    self.begin_removed_profile_cleanup(completion.profile);
                }
                Err(failure) => {
                    self.profile_cascades_in_flight.remove(&completion.profile);
                    tracing::warn!(
                        target: "koru::profiles",
                        failure = failure.code(),
                        "a Profile workspace cascade did not finish"
                    );
                    let mut changed = false;
                    for pane in self.panes.values_mut() {
                        changed |= pane
                            .view
                            .local_profile_remove_failed(completion.profile, failure.code());
                    }
                    if changed {
                        self.invalidate_everywhere();
                    }
                }
            }
        }
    }

    pub(super) fn service_profile_removals(&mut self, event_loop: &ActiveEventLoop) {
        self.service_profile_offline();
        self.service_remote_deletion_checks();
        while let Some((raw_profile, outcome)) = self.host.take_local_profile_removal() {
            let Some(profile) = koru_core::ProfileId::parse(&raw_profile) else {
                continue;
            };
            let Some(attempt) = self.profile_removal_attempts.remove(&profile) else {
                // A duplicate/late Agent reply owns no current completion.
                continue;
            };
            if outcome.is_err() {
                let root = self.gateway_root.clone();
                let tx = self.profile_cascade_tx.clone();
                let failure_tx = tx.clone();
                let proxy = self.wake_proxy.clone();
                let spawned = std::thread::Builder::new()
                    .name("koru-profile-stop".into())
                    .spawn(move || {
                        let mut result = Err(CascadeFailure::Incomplete);
                        if let Ok(client) = cascade_client(root.as_deref()) {
                            let store_profile =
                                koru_store_core::ProfileId::from_uuid(profile.as_uuid());
                            if verify_removed(&client, store_profile).is_ok() {
                                result = Ok(CascadeReady::AlreadyRemoved);
                            } else {
                                let _ = client
                                    .catalog()
                                    .stop_profile_removal(store_profile, attempt);
                            }
                        }
                        let _ = tx.send(ProfileCascadeCompletion { profile, result });
                        let _ = proxy.send_event(Wake::Profiles);
                    });
                if spawned.is_err() {
                    let _ = failure_tx.send(ProfileCascadeCompletion {
                        profile,
                        result: Err(CascadeFailure::Incomplete),
                    });
                }
                continue;
            }
            self.begin_removed_profile_cleanup(profile);
        }

        while let Ok(completion) = self.profile_removal_rx.try_recv() {
            self.profile_removals_in_flight.remove(&completion.profile);
            if let Err(code) = completion.result {
                let mut changed = false;
                for pane in self.panes.values_mut() {
                    changed |= pane
                        .view
                        .local_profile_remove_failed(completion.profile, &code);
                }
                if changed {
                    self.invalidate_everywhere();
                }
                continue;
            }
            let removed_profile_name = self
                .runtime
                .profile_catalog
                .iter()
                .find(|profile| profile.id == completion.profile)
                .map(|profile| profile.name.clone())
                .unwrap_or_else(|| "This Profile".into());
            // Cancellation of the asynchronous next-Profile open must not
            // select the just-removed Profile from the previous cached list.
            self.runtime
                .profile_catalog
                .retain(|row| row.id != completion.profile);
            let remaining_workspaces = self
                .runtime
                .catalog_workspaces
                .iter()
                .filter(|workspace| {
                    koru_core::ProfileId::from_uuid(workspace.owner.as_uuid()) != completion.profile
                })
                .map(|workspace| WorkspaceId::from_uuid(workspace.id.as_uuid()))
                .collect::<HashSet<_>>();
            self.runtime
                .settings
                .retain_workspace_state(&remaining_workspaces);
            self.runtime
                .settings
                .profile_mru
                .retain(|profile| *profile != completion.profile);
            let hosted_source = self
                .hosted_profile_removals
                .get(&completion.profile)
                .copied();
            if hosted_source.is_some() {
                self.acknowledge_profile_cleanup(
                    completion.profile,
                    koru_agent_wire::ProfileCleanupStage::SettingsCleared,
                );
            }
            for pane in self.panes.values_mut() {
                let _ = pane.view.local_profile_removed(completion.profile);
            }
            let removed_current = self.runtime.profile_id() == completion.profile;
            let next = self
                .runtime
                .settings
                .profile_mru
                .iter()
                .copied()
                .find(|candidate| {
                    *candidate != completion.profile
                        && self
                            .runtime
                            .profile_catalog
                            .iter()
                            .any(|row| row.id == *candidate)
                })
                .or_else(|| {
                    self.runtime
                        .profile_catalog
                        .iter()
                        .find(|row| row.id != completion.profile)
                        .map(|row| row.id)
                });
            self.runtime.request_catalog_refresh();
            self.invalidate_everywhere();
            let mut navigation_settled = true;
            if removed_current {
                if let Some(next) = next {
                    navigation_settled = false;
                    if let Some(source) = self.primary.or_else(|| self.panes.keys().copied().next())
                    {
                        self.begin_profile_switch_after_removal(source, next, completion.profile);
                    }
                } else {
                    startup::launch(&self.runtime.profile_dir, None);
                    self.primary = None;
                    event_loop.exit();
                }
            }
            if hosted_source == Some(HostedProfileRemovalSource::Peer) {
                let message = format!(
                    "\u{201c}{removed_profile_name}\u{201d} was deleted from Koru\n\nAnother trusted device deleted this Profile. It no longer syncs. Its Workspaces on this device were moved to Trash and, with its AI chats, remain recoverable for 30 days."
                );
                for pane in self.panes.values_mut() {
                    let _ = if next.is_some() {
                        pane.view.show_notice_open_another_profile(message.clone())
                    } else {
                        pane.view.show_notice(message.clone())
                    };
                    pane.window.request_redraw();
                }
            } else if self.remote_profile_deletions.remove(&completion.profile) {
                // A shared deletion that arrived through the root units
                // (Plan 2.2): the Workspaces are in the Trash by the Store's
                // own projection and the batch is restorable from here.
                let lang = koru_i18n::Lang::for_system(
                    koru_window::system_locale::system_locale().as_deref(),
                );
                let message = koru_i18n::fill(
                    koru_i18n::t(lang, koru_i18n::Key::ProfileDeletedOnAnotherDevice),
                    &[&removed_profile_name],
                );
                for pane in self.panes.values_mut() {
                    let _ = if next.is_some() {
                        pane.view.show_notice_open_another_profile(message.clone())
                    } else {
                        pane.view.show_notice(message.clone())
                    };
                    pane.window.request_redraw();
                }
            }
            if hosted_source.is_some() && navigation_settled {
                self.acknowledge_profile_cleanup(
                    completion.profile,
                    koru_agent_wire::ProfileCleanupStage::NavigationSettled,
                );
                self.hosted_profile_removals.remove(&completion.profile);
            }
        }
    }

    fn begin_removed_profile_cleanup(&mut self, profile: koru_core::ProfileId) {
        self.profile_removal_attempts.remove(&profile);
        self.profile_cascades_in_flight.remove(&profile);
        if !self.profile_removals_in_flight.insert(profile) {
            return;
        }
        if self.hosted_profile_removals.contains_key(&profile) {
            self.acknowledge_profile_cleanup(
                profile,
                koru_agent_wire::ProfileCleanupStage::WorkspacesRemoved,
            );
            self.acknowledge_profile_cleanup(
                profile,
                koru_agent_wire::ProfileCleanupStage::ProfileRemoved,
            );
        }
        let remaining_workspaces = self
            .runtime
            .catalog_workspaces
            .iter()
            .filter(|workspace| {
                koru_core::ProfileId::from_uuid(workspace.owner.as_uuid()) != profile
            })
            .map(|workspace| WorkspaceId::from_uuid(workspace.id.as_uuid()))
            .collect::<HashSet<_>>();
        let profile_dir = self.runtime.profile_dir.clone();
        let gateway_root = self.gateway_root.clone();
        let tx = self.profile_removal_tx.clone();
        let failure_tx = tx.clone();
        let proxy = self.wake_proxy.clone();
        let started = std::thread::Builder::new()
            .name("koru-profile-cleanup".into())
            .spawn(move || {
                let result = crate::settings::cleanup_removed_profile_state(
                    &profile_dir,
                    gateway_root.as_deref(),
                    profile,
                    &remaining_workspaces,
                )
                .and_then(|()| startup::forget_store_profile(profile))
                .map_err(|error| error.to_string());
                let _ = tx.send(ProfileRemovalCleanup { profile, result });
                let _ = proxy.send_event(Wake::Profiles);
            });
        if let Err(error) = started {
            let _ = failure_tx.send(ProfileRemovalCleanup {
                profile,
                result: Err(error.to_string()),
            });
        }
    }
}

#[cfg(test)]
mod tests;
