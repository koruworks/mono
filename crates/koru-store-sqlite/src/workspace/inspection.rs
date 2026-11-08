// SPDX-License-Identifier: MPL-2.0

//! Reuse the repository's readers and the recovery loader without its opener.

use super::*;
use koru_store_core::ids::WorkspaceId;
use koru_store_core::manifest::Manifest;
use koru_store_core::ports::Opened;
use koru_store_core::service::{VerifiedWorkspaceManifest, WorkspaceManifestInspection};

// Explicit disposable persistence lane, never part of ordinary unit tests.
#[cfg(all(test, feature = "disk-tests"))]
mod disk_tests;

impl SqliteWorkspace {
    /// Pin the SQLite snapshot while serialized with Store catalog changes.
    /// Reading user_version starts the deferred transaction's read view.
    pub(crate) fn prepare_manifest_inspection(connection: Connection) -> Result<Self> {
        // Connection-local guards only. No configure, migrations or DDL. BEGIN
        // pins one consistent snapshot for schema, discovery and journal reads.
        connection
            .execute_batch("PRAGMA query_only = ON; BEGIN DEFERRED")
            .map_err(sql_failed)?;
        let version = db::user_version(&connection)?;
        if version != WORKSPACE_SCHEMA_VERSION {
            return Err(Error::new(
                if version > WORKSPACE_SCHEMA_VERSION {
                    Code::SchemaTooNew
                } else {
                    Code::CorruptState
                },
                format!(
                    "workspace inspection requires schema {WORKSPACE_SCHEMA_VERSION}; found {version}"
                ),
            ));
        }
        // Intentionally bypass from_connection: it creates/migrates. This
        // private read-only handle uses the exact ordinary repository readers.
        Ok(Self { connection })
    }

    /// Replay only the captured durable view, with no Store mutex held.
    pub(crate) fn inspect_prepared_manifest(
        self,
        workspace: WorkspaceId,
    ) -> Result<WorkspaceManifestInspection> {
        let repo = self;
        let documents = repo.documents()?;
        let mut manifests = documents
            .iter()
            .filter(|d| d.kind == DocumentKind::Manifest);
        let Some(state) = manifests.next() else {
            return Err(Error::new(Code::CorruptState, "workspace has no manifest"));
        };
        if manifests.next().is_some() {
            return Err(Error::new(
                Code::CorruptState,
                "workspace has multiple manifests",
            ));
        }
        let live = match koru_store_core::recovery::open_document(&repo, state.id)? {
            Opened::Recovered(_, recovery) => {
                return Ok(WorkspaceManifestInspection::Recovered(recovery));
            }
            Opened::Fresh(_) => {
                return Err(Error::new(
                    Code::CorruptState,
                    "manifest disappeared during inspection",
                ));
            }
            Opened::Normal(live) => live,
        };
        let manifest = Manifest::from_snapshot(&live.export_snapshot()?)?;
        if manifest.version().is_empty() {
            return Ok(WorkspaceManifestInspection::Pending { manifest: state.id });
        }
        if manifest.workspace_id() != Some(workspace)
            || !matches!(
                manifest.schema_version(),
                Some(1..=koru_store_core::version::CRDT_SCHEMA_VERSION)
            )
        {
            return Err(Error::new(
                Code::CorruptState,
                "manifest identity or format is invalid",
            ));
        }
        let name = manifest
            .name()
            .ok_or_else(|| Error::new(Code::CorruptState, "manifest name is missing"))?;
        let created_at = manifest
            .created_at()
            .ok_or_else(|| Error::new(Code::CorruptState, "manifest creation time is missing"))?;
        if name.trim().is_empty() {
            return Err(Error::new(Code::CorruptState, "manifest name is empty"));
        }
        let members = documents
            .iter()
            .filter(|d| d.id == state.id || manifest.contains(d.id))
            .map(|d| d.id)
            .collect();
        Ok(WorkspaceManifestInspection::Verified(
            VerifiedWorkspaceManifest {
                manifest: state.id,
                workspace,
                name,
                created_at,
                icon: manifest.icon_state(),
                color: manifest.color_state(),
                pinned: manifest.pinned().unwrap_or(false),
                archived: manifest.archived().unwrap_or(false),
                deleted_at: manifest.workspace_deleted_at(),
                documents: members,
            },
        ))
    }
}
