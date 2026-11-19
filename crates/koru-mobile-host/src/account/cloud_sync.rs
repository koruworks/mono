// SPDX-License-Identifier: MPL-2.0

//! The phone's host for the per-Profile Cloud sync switch.
//!
//! The shared handler in `koru-account-runtime` does all the work: it reads
//! the hosted row before quoting a version, mirrors the hosted truth, and
//! signs activation challenges with the service's own device key. It needs
//! exactly two things from a host, and until now the phone supplied neither.
//!
//! Both are already here. Which Workspaces this device holds is the embedded
//! Store's catalog — the same catalog the Documents screen lists a Profile's
//! Workspaces from — and what the person already chose is a small file beside
//! the host's other durable state, next to `device.json` and `trust.db`.
//!
//! The file is deliberately not a database, and it is the same shape the
//! desktop writes (`DeviceCloudSync`, `STATE_VERSION`): ids, version numbers,
//! an incarnation and a stable failure code, nothing a person wrote. It is
//! rebuilt by re-activating if it is ever lost, so a torn write costs one
//! hosted round trip rather than content. It is still replaced atomically —
//! written to a temporary sibling, flushed, and renamed over the old file — so
//! a crash mid-write leaves the previous answer intact rather than a truncated
//! one that would read as "every Profile is off".

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use koru_account_core::{ProfileId, WorkspaceId};
use koru_account_runtime::account::ServiceError;
use koru_account_runtime::account::cloud_sync::{CloudSyncPort, DeviceCloudSync};

/// Beside `device.json`, `trust.db` and `profile-lifecycle.db` in the host
/// root, because it has the same lifetime as they do: it belongs to this
/// installation and is discarded with it.
const STATE_FILE: &str = "cloud-sync.json";

pub(crate) struct MobileCloudSync {
    /// The embedded Store worker. Held as the client rather than the host so
    /// the port cannot keep `MobileHost` alive: the host owns the account,
    /// which owns the service, which owns this.
    store: koru_store_server::StoreWorkerClient,
    state: CloudSyncState,
}

impl MobileCloudSync {
    pub(crate) fn new(store: koru_store_server::StoreWorkerClient, root: &Path) -> Self {
        Self {
            store,
            state: CloudSyncState::new(root.join(STATE_FILE)),
        }
    }
}

impl CloudSyncPort for MobileCloudSync {
    fn workspaces(&self) -> Result<Vec<(ProfileId, WorkspaceId)>, ServiceError> {
        // Deleted Workspaces are deliberately absent: the switch applies to
        // what this device still holds, and activating a row for a Workspace
        // the person threw away would be a hosted write nobody asked for.
        self.store
            .with_store(|store| store.workspaces())
            .map_err(|_| ServiceError::retryable("store_unavailable"))?
            .map(owners)
            .map_err(|_| ServiceError::retryable("store_unavailable"))
    }

    fn load(&self) -> Result<DeviceCloudSync, ServiceError> {
        self.state.load()
    }

    fn store(&self, state: &DeviceCloudSync) -> Result<(), ServiceError> {
        self.state.store(state)
    }

    /// The phone has a Store to open to Cloud, as the desktop does, so a
    /// Profile created with the account starts with Cloud sync on here too
    /// (ADR-000-createdwithaccount, M3.TECHNICAL.final §6).
    fn starts_created_profiles_on(&self) -> bool {
        true
    }

    /// The same group choice the desktop commits: the Profile and its
    /// Workspaces to Cloud, leaving alone a Workspace somebody set
    /// explicitly. A Profile already there writes nothing, so repeating it
    /// is free. Whether the phone asks the transport-mode gate before moving
    /// bytes is a separate decision this does not change.
    fn open_to_cloud(&self, profile: ProfileId) -> Result<(), ServiceError> {
        self.store
            .with_store(move |store| {
                store.choose_transport_mode(
                    profile,
                    koru_store_core::transport::TransportMode::Cloud,
                    true,
                )
            })
            .map_err(|_| ServiceError::retryable("store_unavailable"))?
            .map(|_| ())
            .map_err(|_| ServiceError::retryable("store_unavailable"))
    }
}

/// The catalog row carries a name and an icon; the switch needs neither. This
/// keeps the port content-free at the point it crosses into the runtime.
fn owners(workspaces: Vec<koru_store_core::Workspace>) -> Vec<(ProfileId, WorkspaceId)> {
    workspaces
        .into_iter()
        .map(|workspace| (workspace.owner, workspace.id))
        .collect()
}

/// The remembered choice on this device. Split from the Store half so it can
/// be exercised on its own: the file is the part with a failure mode.
struct CloudSyncState {
    path: PathBuf,
    /// Serialises the read-modify-write against itself. The account worker is
    /// one thread today; this keeps that from being load-bearing.
    write: Mutex<()>,
}

impl CloudSyncState {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            write: Mutex::new(()),
        }
    }

    fn load(&self) -> Result<DeviceCloudSync, ServiceError> {
        match std::fs::read(&self.path) {
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
            .path
            .parent()
            .ok_or_else(|| ServiceError::terminal("cloud_sync_state_invalid"))?;
        std::fs::create_dir_all(directory)
            .map_err(|_| ServiceError::retryable("cloud_sync_state_unavailable"))?;
        write_replacing(&self.path, &body)
            .map_err(|_| ServiceError::retryable("cloud_sync_state_unavailable"))
    }
}

/// Write `body` and replace `destination` with it, or leave `destination`
/// exactly as it was.
///
/// The desktop's `AgentCloudSync` has the same function for the same file.
/// It lives in the `koru-agent` binary and this crate cannot reach into it;
/// the shared atomic write in `koru-settings` is fenced off by that crate's
/// leaf-dependency rule, which a mobile host should not be the one to breach.
fn write_replacing(destination: &Path, body: &[u8]) -> std::io::Result<()> {
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
        // The bytes are on the disk before the name points at them; a durable
        // rename over contents that are not is a file that reads as "off".
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
