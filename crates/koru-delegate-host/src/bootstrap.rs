// SPDX-License-Identifier: MPL-2.0

//! The one line the Agent writes on the worker's stdin.
//!
//! The same shape the sessions worker reads: who launched it, which
//! channel, which root, where the store is, whether the root was explicit,
//! the device it executes for, and the per-child authority that stops it.
//! `Debug` redacts the authority; nothing else here is a secret, and none
//! of it is content.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct LaunchBootstrap {
    pub launch_id: String,
    pub segment: String,
    /// The channel root the Agent runs under.
    pub root: PathBuf,
    /// The store root, beside which `runs.db` and the log live.
    pub store_directory: PathBuf,
    pub explicit_root: bool,
    /// The executing device (D3, D16), as the Agent's `device.json` spells
    /// it.
    pub device_id: String,
    authority: String,
    #[serde(default)]
    bot_authority: Option<koru_delegate_api::bots::BotDelegateAuthority>,
}

impl LaunchBootstrap {
    pub fn new(
        launch_id: String,
        segment: String,
        root: PathBuf,
        store_directory: PathBuf,
        explicit_root: bool,
        device_id: String,
        authority: String,
    ) -> Self {
        Self {
            launch_id,
            segment,
            root,
            store_directory,
            explicit_root,
            device_id,
            authority,
            bot_authority: None,
        }
    }

    /// The per-child stop authority: a stop carrying it ends this worker.
    pub fn authority(&self) -> &str {
        &self.authority
    }

    pub fn with_bot_authority(
        mut self,
        authority: koru_delegate_api::bots::BotDelegateAuthority,
    ) -> Self {
        self.bot_authority = Some(authority);
        self
    }

    pub fn bot_authority(&self) -> Option<&koru_delegate_api::bots::BotDelegateAuthority> {
        self.bot_authority.as_ref()
    }
}

impl std::fmt::Debug for LaunchBootstrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchBootstrap")
            .field("launch_id", &self.launch_id)
            .field("segment", &self.segment)
            .field("explicit_root", &self.explicit_root)
            .field("device_id", &self.device_id)
            .field("authority", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}
