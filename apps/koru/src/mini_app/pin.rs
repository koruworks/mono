// SPDX-License-Identifier: MPL-2.0
//! One frozen save-and-pin intent. Retrying never captures newer live state.
use super::*;
use koru_settings::pins::{Mutation, Pin, PinId};
#[derive(Default)]
pub struct State {
    pub saving: Option<koru_artifact_wire::RegistrationId>,
    pub target: Option<Pin>,
    pub mutation: Option<Mutation>,
    pub requested: bool,
    pub in_flight: bool,
    pub failed: bool,
    pub pinned: bool,
}
impl State {
    pub fn pending(&self) -> bool {
        self.requested || self.in_flight || self.saving.is_some() && !self.failed
    }
    pub fn saved(&mut self, saved: &artifact::Saved, title: &str) {
        self.saving = None;
        let title = if title.is_empty() {
            koru_i18n::t(
                crate::format::lang(),
                match &saved.snapshot {
                    koru_ai_core::mini_apps::artifact::SavedEnvelope::TaskDocument(_) => {
                        koru_i18n::Key::MiniAppTitle
                    }
                    koru_ai_core::mini_apps::artifact::SavedEnvelope::Composition(_) => {
                        koru_i18n::Key::MiniAppCompositionTitle
                    }
                },
            )
        } else {
            title
        };
        let target = Pin {
            id: PinId(uuid::Uuid::new_v4().as_u128()),
            family: saved.family.to_storage(),
            version: saved.version.to_storage(),
            title: title.into(),
        };
        self.mutation = Some(Mutation::Add {
            pin: target.clone(),
        });
        self.target = Some(target);
        self.requested = true;
        self.failed = false;
    }
}
impl MiniApp {
    pub fn pin_label(&self) -> &'static str {
        use koru_i18n::Key::*;
        koru_i18n::t(
            crate::format::lang(),
            if self.pin.pending() {
                MiniAppPinning
            } else if self.pin.failed {
                MiniAppPinRetry
            } else if self.pin.pinned {
                MiniAppUnpin
            } else {
                MiniAppPin
            },
        )
    }
    pub fn request_pin(&mut self) {
        if self.pin.pending() {
            return;
        }
        // Retrying an accepted intent preserves the saved reference or capture.
        if self.pin.failed {
            self.pin.failed = false;
            if self.pin.saving.is_some() {
                self.artifact.request();
            } else {
                self.pin.requested = true;
            }
            return;
        }
        if self.pin.pinned {
            if let Some(target) = &self.pin.target {
                self.pin.mutation = Some(Mutation::Remove { id: target.id });
                self.pin.requested = true;
            }
            return;
        }
        let Ok(current) = self.artifact_snapshot() else {
            self.artifact.notice = Some(koru_i18n::Key::MiniAppPinWait);
            return;
        };
        if self
            .artifact
            .operation
            .as_ref()
            .is_some_and(|op| op.snapshot != current)
        {
            self.artifact.notice = Some(koru_i18n::Key::MiniAppPinWait);
            return;
        }
        if self.artifact.operation.is_none() && self.saved_arrangement_is_current() {
            if let Some(saved) = &self.artifact.saved {
                self.pin.saved(saved, &self.artifact.title);
            }
        } else {
            self.request_artifact_save();
            if let Some(operation) = &self.artifact.operation {
                self.pin.saving = Some(operation.registration.registration_id);
                self.pin.failed = false;
            }
        }
    }
}

#[cfg(test)]
mod tests;
