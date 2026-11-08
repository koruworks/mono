// SPDX-License-Identifier: MPL-2.0

//! A conflicting create identity offers the existing Profile manager. Opening
//! it is selection only: it must not publish a Profile or ensure a workspace.

use super::*;

impl<H: Host> OnboardingApp<H> {
    pub(super) fn open_existing_profiles(&mut self) {
        self.connect_created_profile = false;
        self.first_minutes = None;
        self.open_manage_after_creation = true;
        let root = self.gateway_root.clone();
        let tx = self.creation_tx.clone();
        let proxy = self.proxy.clone();
        let started = std::thread::Builder::new()
            .name("koru-onboarding-manage".into())
            .spawn(move || {
                let result = match koru_workspace::fromstore::open_from_store_preferring_empty(
                    koru_workspace::fromstore::segment(),
                    root.as_deref(),
                    None,
                ) {
                    Ok(koru_workspace::fromstore::StoreLaunch::Open(opened)) => {
                        OnboardingCreation::Opened(opened)
                    }
                    Ok(koru_workspace::fromstore::StoreLaunch::NeedsProfile) => {
                        OnboardingCreation::Conflict
                    }
                    Err(error) => OnboardingCreation::Retryable {
                        message: error.to_string(),
                    },
                };
                let _ = tx.send(result);
                let _ = proxy.send_event(Wake::Onboarding);
            });
        if let Err(error) = started {
            self.view.creation_failed(error.to_string());
            self.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests;
