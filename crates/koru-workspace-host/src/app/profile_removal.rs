// SPDX-License-Identifier: MPL-2.0

//! Recoverable local cleanup after either a device-local removal or a hosted
//! Profile deletion.
//!
//! Hosted deletion is server-first. This module owns every later local stage
//! and acknowledges each durable boundary back to the Agent journal. Keeping
//! the sequence outside `app.rs` makes it one resumable workflow rather than a
//! set of UI callbacks that happen to run in the right order.

use super::*;

pub(super) struct ProfileRemovalCleanup {
    pub(super) profile: koru_core::ProfileId,
    pub(super) result: Result<(), String>,
}

pub(super) struct ProfileCascadeCompletion {
    pub(super) profile: koru_core::ProfileId,
    pub(super) result: Result<(), String>,
}

pub(super) struct ProfileOfflineCompletion {
    pub(super) profile: koru_core::ProfileId,
    pub(super) result: Result<(), String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HostedProfileRemovalSource {
    Origin,
    Peer,
    Reconciliation,
}

/// Move every active Workspace owned by one Profile to recoverable Trash.
fn cascade_profile_workspaces(
    root: Option<&std::path::Path>,
    profile: koru_core::ProfileId,
) -> Result<(), String> {
    const SCOPES: &[&str] = &[
        "store.read_documents",
        "store.manage_workspaces",
        koru_api_client::WORKSPACE_DESTROY_SCOPE,
    ];
    let segment = koru_workspace::fromstore::segment();
    let client = match root {
        Some(root) => koru_api_client::Client::connect_at_first_party(segment, root, SCOPES),
        None => koru_api_client::Client::connect_first_party(segment, SCOPES),
    }
    .map_err(|error| error.to_string())?;
    if !client.catalog().may_destroy() {
        return Err("this Koru window was not granted workspace deletion".into());
    }

    let store_profile = koru_store_core::ids::ProfileId::from_uuid(profile.as_uuid());
    let mut workspaces = client
        .catalog()
        .workspaces()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|workspace| workspace.owner == store_profile && workspace.deleted_at.is_none())
        .collect::<Vec<_>>();
    workspaces.sort_by_key(|workspace| workspace.id.as_uuid());

    for workspace in workspaces {
        let id = workspace.id.to_storage();
        let confirmation = client
            .catalog()
            .confirmation(&id, koru_api_client::WorkspaceLifecycleAction::SoftDelete)
            .map_err(|error| error.to_string())?;
        client
            .catalog()
            .lifecycle(
                &id,
                koru_api_client::WorkspaceLifecycleAction::SoftDelete,
                &confirmation.confirmation,
            )
            .map_err(|error| error.to_string())?;
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

    pub(super) fn start_profile_cascade(&mut self, profile: koru_core::ProfileId) {
        if !self.profile_cascades_in_flight.insert(profile) {
            return;
        }
        let root = self.gateway_root.clone();
        let tx = self.profile_cascade_tx.clone();
        let proxy = self.wake_proxy.clone();
        let started = std::thread::Builder::new()
            .name("koru-profile-cascade".into())
            .spawn(move || {
                let result = cascade_profile_workspaces(root.as_deref(), profile);
                let _ = tx.send(ProfileCascadeCompletion { profile, result });
                let _ = proxy.send_event(Wake::Profiles);
            });
        if started.is_err() {
            self.profile_cascades_in_flight.remove(&profile);
            let mut changed = false;
            for pane in self.panes.values_mut() {
                changed |= pane
                    .view
                    .local_profile_remove_failed(profile, "profile_cascade_incomplete");
            }
            if changed {
                self.invalidate_everywhere();
            }
        }
    }

    fn acknowledge_profile_cleanup(
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
            self.profile_cascades_in_flight.remove(&completion.profile);
            match completion.result {
                Ok(()) => {
                    if self
                        .hosted_profile_removals
                        .contains_key(&completion.profile)
                    {
                        self.acknowledge_profile_cleanup(
                            completion.profile,
                            koru_agent_wire::ProfileCleanupStage::WorkspacesRemoved,
                        );
                    }
                    self.host.remove_local_profile(completion.profile)
                }
                Err(_) => {
                    tracing::warn!(
                        target: "koru::profiles",
                        failure = "profile_cascade_incomplete",
                        "a Profile workspace cascade did not finish"
                    );
                    let mut changed = false;
                    for pane in self.panes.values_mut() {
                        changed |= pane.view.local_profile_remove_failed(
                            completion.profile,
                            "profile_cascade_incomplete",
                        );
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
        while let Some((raw_profile, outcome)) = self.host.take_local_profile_removal() {
            let Some(profile) = koru_core::ProfileId::parse(&raw_profile) else {
                continue;
            };
            if let Err(code) = outcome {
                // A trusted deletion can reach a device that retained no
                // local replica. There is no content to remove, so absence is
                // the idempotent local result and settings cleanup may
                // continue. A plain device-local removal still treats it as
                // an error rather than claiming it removed something.
                let hosted_absence = code == "profile_not_found"
                    && self.hosted_profile_removals.contains_key(&profile);
                if !hosted_absence {
                    let mut changed = false;
                    for pane in self.panes.values_mut() {
                        changed |= pane.view.local_profile_remove_failed(profile, &code);
                    }
                    if changed {
                        self.invalidate_everywhere();
                    }
                    continue;
                }
            }
            if !self.profile_removals_in_flight.insert(profile) {
                continue;
            }
            if self.hosted_profile_removals.contains_key(&profile) {
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
            let proxy = self.wake_proxy.clone();
            let _ = std::thread::Builder::new()
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
                    navigation_settled = self.switch_profile_after_removal(next);
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
}
