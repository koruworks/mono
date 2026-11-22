// SPDX-License-Identifier: MPL-2.0

//! Device-local sync settings. Only the visible Profile is read, on a worker;
//! writes remain owned by their Profile even if selection changes meanwhile.
use super::*;
use koru_i18n::{Key, t};
use koru_store_core::{TransportMode, ids::WorkspaceId};
use koru_store_wire::ProfileTransportSettings;
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct State {
    target: Option<ProfileId>,
    visible: bool,
    pub settings: Option<ProfileTransportSettings>,
    receiver: Option<mpsc::Receiver<(ProfileId, Result<ProfileTransportSettings, String>, bool)>>,
    pub saving: bool,
    due: Option<Instant>,
    pub error: Option<String>,
}

impl State {
    pub fn deadline(&self) -> Option<Instant> {
        if self.receiver.is_some() {
            Some(Instant::now() + Duration::from_millis(30))
        } else if self.visible {
            self.due
        } else {
            None
        }
    }
}

impl ProfilesController {
    #[cfg(test)]
    pub(crate) fn set_sync_settings_for_test(&mut self, settings: ProfileTransportSettings) {
        self.sync_modes.target = self.selected;
        self.sync_modes.settings = Some(settings);
    }
    pub fn set_sync_visible(&mut self, visible: bool) {
        if visible && !self.sync_modes.visible {
            self.sync_modes.due = Some(Instant::now());
        }
        self.sync_modes.visible = visible;
    }

    pub fn sync_settings(&self) -> Option<&ProfileTransportSettings> {
        (self.sync_modes.target == self.selected)
            .then_some(self.sync_modes.settings.as_ref())
            .flatten()
    }

    pub fn sync_notice(&self) -> Option<&str> {
        if self.sync_modes.saving {
            Some(t(crate::format::lang(), Key::SyncModeSaving))
        } else {
            self.sync_modes.error.as_deref()
        }
    }

    pub fn sync_mode_enabled(&self, workspace: Option<WorkspaceId>, mode: TransportMode) -> bool {
        let Some(settings) = self.sync_settings() else {
            return false;
        };
        if self.sync_modes.saving
            || self
                .selected
                .is_some_and(|profile| self.cloud_sync_pending_for(profile).is_some())
        {
            return false;
        }
        if workspace.is_some()
            && TransportMode::parse(&settings.mode).is_ok_and(|ceiling| mode > ceiling)
        {
            return false;
        }
        mode != TransportMode::Cloud
            || self.selected_profile().is_some_and(|profile| {
                self.cloud_sync_availability(profile) == SyncAvailability::Enabled
            })
    }

    pub fn choose_sync_mode(&mut self, workspace: Option<WorkspaceId>, mode: TransportMode) {
        if !self.sync_mode_enabled(workspace, mode) {
            return;
        }
        let Some(profile) = self.selected else {
            return;
        };
        let Some(client) = self.client.clone() else {
            return;
        };
        let revision = workspace.and_then(|id| {
            self.sync_settings()?
                .workspaces
                .iter()
                .find(|row| row.id == id.to_storage())
                .map(|row| row.revision)
        });
        if workspace.is_some() && revision.is_none() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.sync_modes.receiver = Some(rx);
        self.sync_modes.saving = true;
        self.sync_modes.error = None;
        std::thread::spawn(move || {
            let store_profile = koru_store_core::ProfileId::from_uuid(profile.as_uuid());
            let answer = if let Some(workspace) = workspace {
                client.set_workspace_transport_mode(workspace, mode.as_storage(), revision.unwrap())
            } else {
                client
                    .set_profile_transport_mode(store_profile, mode.as_storage(), true)
                    .and_then(|_| client.profile_transport_mode(store_profile))
            }
            .map_err(|error| {
                format!(
                    "{} ({error})",
                    t(crate::format::lang(), Key::SyncModeSaveFailed)
                )
            });
            let _ = tx.send((
                profile,
                answer,
                workspace.is_none() && mode == TransportMode::Cloud,
            ));
        });
    }

    pub(super) fn maintain_sync_modes(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if let Some(receiver) = &self.sync_modes.receiver {
            match receiver.try_recv() {
                Ok((profile, answer, enable_cloud)) => {
                    self.sync_modes.receiver = None;
                    let was_saving = self.sync_modes.saving;
                    self.sync_modes.saving = false;
                    if answer.is_ok() && was_saving {
                        self.refresh_cloud_sync();
                    }
                    if answer.is_ok()
                        && enable_cloud
                        && self.profiles.iter().any(|row| {
                            row.id == profile
                                && row.account.as_deref()
                                    == self
                                        .account
                                        .active_account_id
                                        .as_ref()
                                        .map(|id| format!("koru:{id}"))
                                        .as_deref()
                        })
                    {
                        self.cloud.set_request = Some((profile.to_storage(), true));
                    }
                    if Some(profile) == self.selected {
                        match answer {
                            Ok(settings) => {
                                self.sync_modes.settings = Some(settings);
                                self.sync_modes.error = None;
                            }
                            Err(error) => self.sync_modes.error = Some(error),
                        }
                    }
                    self.sync_modes.due = Some(now + Duration::from_secs(3));
                    changed = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.sync_modes.receiver = None;
                    self.sync_modes.saving = false;
                    self.sync_modes.error =
                        Some(t(crate::format::lang(), Key::SyncModeReadFailed).into());
                    self.sync_modes.due = Some(now + Duration::from_secs(3));
                    changed = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.sync_modes.target != self.selected {
            self.sync_modes.target = self.selected;
            self.sync_modes.settings = None;
            self.sync_modes.error = None;
            self.sync_modes.due = Some(now);
            changed = true;
        }
        if self.sync_modes.visible
            && self.sync_modes.receiver.is_none()
            && self.sync_modes.due.is_none_or(|due| due <= now)
            && let (Some(profile), Some(client)) = (self.selected, self.client.clone())
        {
            let (tx, rx) = mpsc::channel();
            self.sync_modes.receiver = Some(rx);
            std::thread::spawn(move || {
                let answer = client
                    .profile_transport_mode(koru_store_core::ProfileId::from_uuid(
                        profile.as_uuid(),
                    ))
                    .map_err(|error| {
                        format!(
                            "{} ({error})",
                            t(crate::format::lang(), Key::SyncModeReadFailed)
                        )
                    });
                let _ = tx.send((profile, answer, false));
            });
        }
        changed
    }
}

#[cfg(test)]
mod tests;
