// SPDX-License-Identifier: MPL-2.0
//! Blocking namespace; native hosts call it only from their bounded worker.
use crate::{
    Client,
    settings::{SettingsResult, map_public},
};
use koru_settings::pins::{Pin, Request, Scope};
use koru_store_client::http::RetryPolicy;

/// The dedicated collection invalidation, separate from scalar Sections.
pub const CHANGED_EVENT: &str =
    <koru_events::SettingsMiniAppPinsChanged as koru_events::EventPayload>::NAME;

pub fn changed(event_name: &str) -> bool {
    event_name == CHANGED_EVENT
}

impl Client {
    pub fn mini_app_pins(&self, scope: &Scope) -> SettingsResult<Vec<Pin>> {
        self.store()
            .get_public_json(&format!(
                "/v1/mini-app-pins?profile={}&workspace={}",
                scope.profile, scope.workspace
            ))
            .map_err(map_public)
    }
    pub fn mutate_mini_app_pins(&self, request: &Request) -> SettingsResult<Vec<Pin>> {
        self.store()
            .patch_public_json("/v1/mini-app-pins", request, RetryPolicy::Safe)
            .map_err(map_public)
    }
}
