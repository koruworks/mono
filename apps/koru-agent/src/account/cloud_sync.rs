// SPDX-License-Identifier: MPL-2.0

//! The desktop host for the per-Profile Cloud sync switch.
//!
//! Two facts the runtime cannot reach on its own: which Workspaces this
//! machine holds (the Store catalog), and what the person already chose (a
//! small file beside the Agent's other durable state).
//!
//! The file is deliberately not a database. It holds ids, version numbers, an
//! incarnation and a stable failure code — nothing a person wrote — and it is
//! rebuilt by re-activating if it is ever lost, so a torn write costs one
//! hosted round trip rather than content. It is still replaced atomically:
//! written to a temporary sibling, flushed, and renamed over the old file, so
//! a crash mid-write leaves the previous answer intact rather than a truncated
//! one that would read as "every Profile is off".

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use koru_account_runtime::account::cloud_sync::{CloudSyncPort, DeviceCloudSync};

use super::ServiceError;

pub struct AgentCloudSync {
    root: Option<PathBuf>,
    state_path: PathBuf,
    /// Serialises the read-modify-write against itself. The account worker is
    /// one thread today; this keeps that from being load-bearing.
    write: Mutex<()>,
}

impl AgentCloudSync {
    pub fn new(paths: &crate::Paths) -> Self {
        Self {
            root: paths.explicit_root().map(Path::to_path_buf),
            state_path: paths.cloud_sync_state(),
            write: Mutex::new(()),
        }
    }

    fn store_client(&self) -> Result<koru_api_client::Client, ServiceError> {
        match &self.root {
            Some(root) => koru_api_client::Client::open_at(koru_channel::segment(), root),
            None => koru_api_client::Client::open(koru_channel::segment()),
        }
        .map_err(|_| ServiceError::retryable("store_unavailable"))
    }
}

impl CloudSyncPort for AgentCloudSync {
    fn workspaces(
        &self,
    ) -> Result<Vec<(koru_account_core::ProfileId, koru_account_core::WorkspaceId)>, ServiceError>
    {
        Ok(self
            .store_client()?
            .store()
            .workspaces()
            .map_err(|_| ServiceError::retryable("store_unavailable"))?
            .into_iter()
            .map(|workspace| (workspace.owner, workspace.id))
            .collect())
    }

    fn load(&self) -> Result<DeviceCloudSync, ServiceError> {
        match std::fs::read(&self.state_path) {
            Ok(bytes) => Ok(serde_json::from_slice::<DeviceCloudSync>(&bytes)
                // A file this build cannot read is not a reason to refuse the
                // switch: the remembered choice is re-derivable, so start from
                // the default and let the next apply rewrite it.
                .map(DeviceCloudSync::accepted)
                .unwrap_or_default()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(DeviceCloudSync::default())
            }
            Err(_) => Err(ServiceError::retryable("cloud_sync_state_unavailable")),
        }
    }

    fn store(&self, state: &DeviceCloudSync) -> Result<(), ServiceError> {
        let _guard = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let body = serde_json::to_vec_pretty(state)
            .map_err(|_| ServiceError::terminal("cloud_sync_state_invalid"))?;
        let directory = self
            .state_path
            .parent()
            .ok_or_else(|| ServiceError::terminal("cloud_sync_state_invalid"))?;
        std::fs::create_dir_all(directory)
            .map_err(|_| ServiceError::retryable("cloud_sync_state_unavailable"))?;
        write_replacing(&self.state_path, &body)
            .map_err(|_| ServiceError::retryable("cloud_sync_state_unavailable"))
    }

    /// The desktop can open its Store to Cloud, so a Profile created with
    /// the account starts with Cloud sync on (ADR-000-createdwithaccount).
    fn starts_created_profiles_on(&self) -> bool {
        true
    }

    /// The Store operation behind the Profile prompt's *Move everything*
    /// (D258): the Profile and its Workspaces to Cloud, as one group. A
    /// Profile already there writes nothing, so repeating it is free.
    fn open_to_cloud(&self, profile: koru_account_core::ProfileId) -> Result<(), ServiceError> {
        self.store_client()?
            .set_profile_transport_mode(
                profile,
                koru_store_core::TransportMode::Cloud.as_storage(),
                true,
            )
            .map(|_| ())
            .map_err(|_| ServiceError::retryable("store_unavailable"))
    }
}

/// Write `body` and replace `destination` with it, or leave `destination`
/// exactly as it was.
pub(crate) fn write_replacing(destination: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let directory = destination
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent directory"))?;
    let temporary = directory.join(format!(
        ".{}.{}.tmp",
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("cloud-sync"),
        uuid::Uuid::new_v4()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(body)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, destination)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests;
