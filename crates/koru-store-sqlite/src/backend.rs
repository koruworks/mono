// SPDX-License-Identifier: MPL-2.0

//! Where a workspace's storage lives on disk.
//!
//! The one place that turns a [`WorkspaceId`] into a directory, and therefore
//! the one place that knows the physical layout at all:
//!
//! ```text
//! <root>/blobs/objects/b3-<digest>                     every blob, once
//! <root>/workspaces/<workspace-id>/workspace.db
//! <root>/workspaces/<workspace-id>/backups/<name>-<epoch_ms>.db
//! <root>/workspaces/<workspace-id>/blobs/<hash>        legacy, read-only
//! ```
//!
//! **Bytes are per store, not per Workspace.** A picture used in two
//! Workspaces is one file; which Workspaces may read it is the
//! `workspace.db.blobs` row, and this module has no opinion about that at all.
//! The last line is the layout older builds wrote: it is still read, is
//! promoted into the object store the first time somebody asks for those
//! bytes, and is never moved or deleted — which is what lets an older build
//! still open this store.
//!
//! **Everything physical is id-based.** Not the workspace's name, not its
//! owner's, not a profile hierarchy — names are translatable, renameable and
//! full of characters that are legal in one filesystem and not another, and a
//! directory named after one is a rename away from being lost. An id is a v4
//! UUID: 36 ASCII characters, valid everywhere, and immutable.
//!
//! This is also why a workspace can be a replica in several channels at once.
//! Each channel has its own root; the id under it is the same, and nothing
//! about the path says which channel it belongs to.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use koru_store_core::errors::Result;
use koru_store_core::ids::WorkspaceId;
use koru_store_core::model::BlobHash;
use koru_store_core::ports::LegacyPromotion;
use koru_store_core::service::Backend;
use koru_store_core::version::WORKSPACE_SCHEMA_VERSION;

use crate::backup::SqliteBackups;
use crate::workspace::SqliteWorkspace;

pub const WORKSPACES_DIR: &str = "workspaces";
pub const WORKSPACE_FILE: &str = "workspace.db";
pub const BLOBS_DIR: &str = "blobs";
pub const BACKUPS_DIR: &str = "backups";

/// Workspace storage under one store root, or in this process's memory.
pub struct SqliteBackend {
    root: PathBuf,
    /// `Some` for a Store kept entirely in memory ([`SqliteBackend::in_memory`]).
    memory: Option<MemoryWorkspaces>,
}

/// Every Workspace database of an in-memory backend, each held open by one
/// keeper connection.
///
/// **The keeper is what makes a Workspace outlive one open.** The Store opens
/// a Workspace's repository more than once — to create it, then again to
/// serve it, and once more to count a deleted one's documents — and a private
/// in-memory database would be a new, empty one each time. A named database
/// in memory is the same one for every connection to its name, and lives
/// while the keeper does; deleting the Workspace drops the keeper.
struct MemoryWorkspaces {
    held: Mutex<HashMap<WorkspaceId, (koru_sqlite::MemoryName, rusqlite::Connection)>>,
}

impl SqliteBackend {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            memory: None,
        }
    }

    /// Workspace storage that lives in this process's memory and nowhere else.
    ///
    /// Every `workspace.db` is a named database in memory, journal and
    /// temporary storage included, so a Store over this backend writes no
    /// file at all. **It keeps no file bytes**: [`Backend::objects`] refuses,
    /// because a blob store is a directory. The paths this type otherwise
    /// answers — [`Self::root`], [`Self::workspace_dir`], [`Self::backups`] —
    /// describe a disk layout this backend does not have; nothing on the
    /// in-memory path asks for them.
    pub fn in_memory() -> Self {
        Self {
            root: PathBuf::new(),
            memory: Some(MemoryWorkspaces {
                held: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Whether this backend keeps its Workspaces in memory.
    pub fn is_memory(&self) -> bool {
        self.memory.is_some()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn workspace_dir(&self, id: WorkspaceId) -> PathBuf {
        self.root.join(WORKSPACES_DIR).join(id.to_storage())
    }

    pub fn workspace_db(&self, id: WorkspaceId) -> PathBuf {
        self.workspace_dir(id).join(WORKSPACE_FILE)
    }

    /// Where an older build filed this Workspace's bytes.
    ///
    /// Read, never written: [`promote_legacy_object`](Backend::promote_legacy_object)
    /// copies out of it and leaves it exactly as it found it.
    pub fn legacy_blobs_dir(&self, id: WorkspaceId) -> PathBuf {
        self.workspace_dir(id).join(BLOBS_DIR)
    }

    /// The object store's own directory, shared by every Workspace.
    pub fn objects_dir(&self) -> PathBuf {
        self.root.join(BLOBS_DIR)
    }

    pub fn objects(&self) -> crate::blobs::SqliteBlobs {
        crate::blobs::SqliteBlobs::new(self.objects_dir())
    }

    /// Clear the object store's staging directory of work a crash left behind.
    ///
    /// Called once at start-up. It hashes nothing that is not already stale
    /// and unreferenced, so it is not the eager migration B1 refused to build.
    pub fn sweep_pending_objects(
        &self,
        now: koru_store_core::model::Timestamp,
    ) -> Result<crate::blobs::PendingSweep> {
        if self.is_memory() {
            return Ok(crate::blobs::PendingSweep::default());
        }
        self.objects()
            .sweep_staging(now, crate::blobs::PENDING_MAX_AGE_MS)
    }

    pub fn backups(&self, id: WorkspaceId) -> SqliteBackups {
        SqliteBackups::new(
            self.workspace_db(id),
            self.workspace_dir(id).join(BACKUPS_DIR),
        )
    }
}

impl Backend for SqliteBackend {
    type Workspace = SqliteWorkspace;
    type Blobs = crate::blobs::SqliteBlobs;

    fn open_workspace(&self, id: WorkspaceId) -> Result<Self::Workspace> {
        if let Some(memory) = &self.memory {
            return memory.open(id);
        }
        // `SqliteWorkspace::open` creates the directory and migrates, so
        // opening a workspace that has never existed is how one is made. There
        // is no separate "create" step to forget.
        let path = self.workspace_db(id);
        SqliteWorkspace::open(&path).map_err(|error| {
            koru_store_core::Error::new(
                error.code(),
                format!(
                    "workspace {id} at {} failed to open (this build supports schema {WORKSPACE_SCHEMA_VERSION}): {}",
                    path.display(),
                    error.message()
                ),
            )
        })
    }

    fn delete_workspace(&self, id: WorkspaceId) -> Result<()> {
        if let Some(memory) = &self.memory {
            memory.forget(id);
            return Ok(());
        }
        let directory = self.workspace_dir(id);
        if !directory.exists() {
            return Ok(());
        }
        std::fs::remove_dir_all(&directory).map_err(|error| {
            koru_store_core::Error::new(
                koru_store_core::Code::StorageFailed,
                format!("remove expired workspace: {error}"),
            )
        })
    }

    fn objects(&self) -> Result<Self::Blobs> {
        if self.is_memory() {
            return Err(koru_store_core::Error::new(
                koru_store_core::Code::BlobTransferUnavailable,
                "this Store is kept in memory and holds no file bytes",
            ));
        }
        Ok(SqliteBackend::objects(self))
    }

    fn promote_legacy_object(
        &self,
        workspace: WorkspaceId,
        hash: &BlobHash,
    ) -> Result<LegacyPromotion> {
        if self.is_memory() {
            return Ok(LegacyPromotion::NoCandidate);
        }
        // The name is built from the hash, never from anything a person typed,
        // so this cannot be made to read outside the Workspace's directory.
        let candidate = self.legacy_blobs_dir(workspace).join(hash.file_name());
        SqliteBackend::objects(self).promote(&candidate, hash)
    }
}

impl MemoryWorkspaces {
    fn open(&self, id: WorkspaceId) -> Result<SqliteWorkspace> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let name = match held.get(&id) {
            Some((name, _)) => name.clone(),
            None => {
                let name = koru_sqlite::MemoryName::fresh("workspace.db");
                let keeper = koru_sqlite::open_memory(&name).map_err(|error| {
                    koru_store_core::Error::new(
                        koru_store_core::Code::StorageFailed,
                        format!("workspace {id} in memory failed to open: {error}"),
                    )
                })?;
                held.insert(id, (name.clone(), keeper));
                name
            }
        };
        // Opened while the map is held, so a concurrent delete cannot drop the
        // keeper between the name being read and the database being joined.
        SqliteWorkspace::in_named_memory(&name)
    }

    fn forget(&self, id: WorkspaceId) {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }
}

#[cfg(test)]
mod tests;
