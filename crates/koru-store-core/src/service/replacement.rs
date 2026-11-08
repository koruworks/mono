// SPDX-License-Identifier: MPL-2.0

//! Lifecycle authority commits before its listing, with any last-workspace
//! replacement prepared before either changes. A failed publication is a value.

use super::*;
use crate::ports::Removal;
use preparation::Prepared;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ListingState {
    Applied,
    Pending { cause: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "state", content = "workspace", rename_all = "snake_case")]
pub enum Replacement {
    Created(Workspace),
    Existing(Workspace),
    NotNeeded,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RemovedWorkspace {
    pub workspace: Workspace,
    pub authority_committed: bool,
    pub listing: ListingState,
    pub replacement: Replacement,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MovedWorkspace {
    pub workspace: Workspace,
    pub from: ProfileId,
    pub replacement: Replacement,
}

enum Ready<W> {
    NotNeeded,
    Existing(Workspace),
    Prepared(Prepared<W>),
}

#[derive(Clone, Copy)]
enum Lifecycle {
    Archive,
    Delete,
}

impl<C: Catalog, B: Backend> Store<C, B> {
    pub fn archive_workspace_replacing(
        &mut self,
        workspace: WorkspaceId,
        name: &str,
        at: Timestamp,
    ) -> Result<RemovedWorkspace> {
        self.remove_workspace_replacing(workspace, Lifecycle::Archive, name, None, at)
    }

    pub fn delete_workspace_replacing(
        &mut self,
        workspace: WorkspaceId,
        name: &str,
        at: Timestamp,
    ) -> Result<RemovedWorkspace> {
        self.remove_workspace_replacing(workspace, Lifecycle::Delete, name, None, at)
    }

    pub fn delete_workspace_for_removal(
        &mut self,
        workspace: WorkspaceId,
        attempt: crate::ids::RemovalAttemptId,
        at: Timestamp,
    ) -> Result<RemovedWorkspace> {
        self.remove_workspace_replacing(workspace, Lifecycle::Delete, "", Some(attempt), at)
    }

    pub fn move_workspace_replacing(
        &mut self,
        workspace: WorkspaceId,
        to: ProfileId,
        name: &str,
        at: Timestamp,
    ) -> Result<MovedWorkspace> {
        self.refuse_profile_removal(self.workspace_including_deleted(workspace)?.owner)?;
        self.refuse_profile_removal(to)?;
        self.profile(to)?;
        self.flush_workspace_manifest(workspace, at)?;
        let row = self.workspace_including_deleted(workspace)?;
        let authority = self.verified_removal_authority(workspace)?;
        let mut projected = availability::project(&row, &authority)?;
        if projected.deleted_at.is_some() {
            return Err(Error::bare(Code::WorkspaceNotFound));
        }
        let from = row.owner;
        if from == to {
            return Ok(MovedWorkspace {
                workspace: projected,
                from,
                replacement: Replacement::Unknown,
            });
        }
        projected.owner = to;
        projected.position = self.next_workspace_position(to)?;
        let ready = self.prepare_removal_replacement(&row, name, at)?;
        if let Err(error) = self
            .catalog
            .publish_removal(Removal::Move(&projected), ready.publication())
        {
            self.discard_replacement(ready);
            return Err(error);
        }
        let replacement = self.adopt_replacement(ready);
        Ok(MovedWorkspace {
            workspace: projected,
            from,
            replacement,
        })
    }

    fn remove_workspace_replacing(
        &mut self,
        workspace: WorkspaceId,
        lifecycle: Lifecycle,
        name: &str,
        removal_attempt: Option<crate::ids::RemovalAttemptId>,
        at: Timestamp,
    ) -> Result<RemovedWorkspace> {
        let owner = self.workspace_including_deleted(workspace)?.owner;
        if let Some(attempt) = removal_attempt {
            self.removal_step(owner, attempt, at)?;
        } else {
            self.refuse_profile_removal(owner)?;
        }
        // Complete previous accepted frames before preparing or inspecting.
        // flush can evict an entry; all references below are obtained afresh.
        self.flush_workspace_manifest(workspace, at)?;
        let row = self.workspace_including_deleted(workspace)?;
        let authority = self.verified_removal_authority(workspace)?;
        if matches!(lifecycle, Lifecycle::Archive) && authority.deleted_at.is_some() {
            return Err(Error::bare(Code::WorkspaceNotFound));
        }
        let mut projected = availability::project(&row, &authority)?;
        let ready = if removal_attempt.is_some() {
            Ready::NotNeeded
        } else {
            self.prepare_removal_replacement(&row, name, at)?
        };
        let already_committed = match lifecycle {
            Lifecycle::Archive => authority.archived,
            Lifecycle::Delete => authority.deleted_at.is_some(),
        };
        if !already_committed {
            if let Err(error) = self.commit_removal(workspace, lifecycle, at) {
                self.discard_replacement(ready);
                return Err(error);
            }
            match lifecycle {
                Lifecycle::Archive => projected.archived = true,
                Lifecycle::Delete => projected.deleted_at = Some(at),
            }
        }
        let removal = match lifecycle {
            Lifecycle::Archive => Removal::Archive(&projected),
            Lifecycle::Delete => Removal::Delete(&projected),
        };
        let publication = self.catalog.publish_removal(removal, ready.publication());
        // Never leave a durably deleted open document usable, including when
        // catalog publication fails. Its commit notification remains queued.
        if projected.deleted_at.is_some() {
            self.open.remove(&workspace);
        }
        match publication {
            Ok(()) => Ok(RemovedWorkspace {
                workspace: projected,
                authority_committed: true,
                listing: ListingState::Applied,
                replacement: self.adopt_replacement(ready),
            }),
            Err(error) => {
                // Keep the prepared repository's durable bytes unlisted. It is
                // no longer safe to undo a committed source to reclaim them.
                drop(ready);
                Ok(RemovedWorkspace {
                    workspace: projected,
                    authority_committed: true,
                    listing: ListingState::Pending {
                        cause: error.code().as_str().into(),
                    },
                    replacement: Replacement::Unknown,
                })
            }
        }
    }

    fn commit_removal(
        &mut self,
        workspace: WorkspaceId,
        lifecycle: Lifecycle,
        at: Timestamp,
    ) -> Result<()> {
        self.ensure_open_including_deleted(workspace)?;
        let open = self
            .open
            .get_mut(&workspace)
            .ok_or_else(|| Error::bare(Code::WorkspaceNotFound))?;
        let snapshot = open.manifest.export_snapshot()?;
        let peer = open.manifest.peer_id();
        let before = open.manifest.version();
        let result = (|| {
            match lifecycle {
                Lifecycle::Archive => open.manifest.set_archived(true)?,
                Lifecycle::Delete => open.manifest.set_workspace_deleted_at(Some(at))?,
            }
            let update = open.manifest.export_updates_since(&before)?;
            commit_one(
                open,
                workspace,
                &mut self.committed,
                &mut self.sync_dirty,
                open.manifest_id,
                update,
                at,
                crate::ports::UpdateOrigin::Store,
            )?;
            Ok(())
        })();
        if let Err(error) = result {
            // Only the manifest is rolled back. Other documents and commit
            // windows belong to their own acknowledged operations.
            let restored = Manifest::from_snapshot(&snapshot)?;
            restored.set_peer_id(peer)?;
            open.manifest = restored;
            return Err(error);
        }
        Ok(())
    }

    fn verified_removal_authority(
        &self,
        workspace: WorkspaceId,
    ) -> Result<VerifiedWorkspaceManifest> {
        match self.backend.inspect_workspace_manifest(workspace) {
            WorkspaceManifestInspection::Verified(authority) => Ok(authority),
            WorkspaceManifestInspection::Unavailable(error) => Err(error),
            _ => Err(Error::new(
                Code::CorruptState,
                "workspace authority is not verified",
            )),
        }
    }

    fn prepare_removal_replacement(
        &mut self,
        removed: &Workspace,
        name: &str,
        at: Timestamp,
    ) -> Result<Ready<B::Workspace>> {
        self.profile(removed.owner)?;
        // Whole-Profile removal has a separate, attempt-checked path. Ordinary
        // lifecycle requests must not create a replacement during that cascade.
        self.refuse_profile_removal(removed.owner)?;
        let mut rows: Vec<_> = self
            .catalog
            .workspaces_including_deleted()?
            .into_iter()
            .filter(|row| row.owner == removed.owner && row.id != removed.id)
            .collect();
        rows.sort_by_key(|row| (row.position, row.id));
        for row in &rows {
            self.flush_workspace_manifest(row.id, at)?;
        }
        let mut unresolved = None;
        for old in rows {
            let row = self.workspace_including_deleted(old.id)?;
            match self.verified_removal_authority(row.id) {
                Ok(authority) => {
                    if !authority.archived && authority.deleted_at.is_none() {
                        match self.reconcile_verified_workspace(&row, &authority, at) {
                            Ok(workspace) => return Ok(Ready::Existing(workspace)),
                            Err(error) => unresolved = Some(error),
                        }
                    }
                }
                Err(error) => unresolved = Some(error),
            }
        }
        // Unknown bytes are not evidence that this was the last workspace.
        if let Some(error) = unresolved {
            return Err(error);
        }
        self.prepare_workspace(removed.owner, name, None, None, at)
            .map(Ready::Prepared)
    }

    fn discard_replacement(&self, ready: Ready<B::Workspace>) {
        if let Ready::Prepared(prepared) = ready {
            self.discard_prepared(prepared);
        }
    }

    fn adopt_replacement(&mut self, ready: Ready<B::Workspace>) -> Replacement {
        match ready {
            Ready::NotNeeded => Replacement::NotNeeded,
            Ready::Existing(workspace) => Replacement::Existing(workspace),
            Ready::Prepared(prepared) => Replacement::Created(self.adopt_prepared(prepared)),
        }
    }
}

impl<W> Ready<W> {
    fn publication(&self) -> Option<(&Workspace, DocumentId)> {
        match self {
            Self::Prepared(prepared) => Some((&prepared.workspace, prepared.open.manifest_id)),
            Self::Existing(_) | Self::NotNeeded => None,
        }
    }
}
