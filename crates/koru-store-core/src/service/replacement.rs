// SPDX-License-Identifier: MPL-2.0

//! Lifecycle authority commits before its listing, with any last-workspace
//! replacement **selected** before either changes. A failed publication is a
//! value.
//!
//! **No replacement is ever created here.** Until Plan 1.2 of the root
//! catalog these routes manufactured a default Workspace when the source
//! Profile would be left empty. `docs/features/root-catalog/TECHNICAL.final.md`
//! §3.10 and §6.3 (D17; ADR-000-rootcatalog) settle that an empty Profile
//! stays empty until the person creates a Workspace, so the replacing routes
//! may *select* another verified live Workspace of the source Profile and
//! otherwise answer [`Replacement::NotNeeded`]. `EnsureLive`'s own creation
//! and the `NeedsProfile` host flow are Plan 1.3's.

use super::*;
use crate::ports::Removal;
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
    /// A Workspace manufactured for the emptied Profile. **Never produced
    /// since Plan 1.2 of the root catalog.** Kept on the wire so the
    /// generated schemas, an older client's decoder and every consumer that
    /// handled it are unchanged; new code branches on the other three.
    Created(Workspace),
    /// Another verified live Workspace of the source Profile.
    Existing(Workspace),
    /// The source Profile has no other live Workspace and stays empty, or a
    /// whole-Profile removal is in progress and wants none.
    NotNeeded,
    /// The catalog publication did not commit, so nothing is known.
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

/// What `prepare_removal_replacement` selected. Nothing here owns storage,
/// so there is nothing to discard on a failed publication.
enum Ready {
    NotNeeded,
    Existing(Workspace),
}

#[derive(Clone, Copy)]
enum Lifecycle {
    Archive,
    Delete,
}

impl<C: Catalog, B: Backend> Store<C, B> {
    /// `name` was the localized name of a manufactured replacement. It is
    /// accepted for wire compatibility and unused: no replacement is created.
    pub fn archive_workspace_replacing(
        &mut self,
        workspace: WorkspaceId,
        _name: &str,
        at: Timestamp,
    ) -> Result<RemovedWorkspace> {
        self.remove_workspace_replacing(workspace, Lifecycle::Archive, None, at)
    }

    /// See [`Self::archive_workspace_replacing`] for `name`.
    pub fn delete_workspace_replacing(
        &mut self,
        workspace: WorkspaceId,
        _name: &str,
        at: Timestamp,
    ) -> Result<RemovedWorkspace> {
        self.remove_workspace_replacing(workspace, Lifecycle::Delete, None, at)
    }

    pub fn delete_workspace_for_removal(
        &mut self,
        workspace: WorkspaceId,
        attempt: crate::ids::RemovalAttemptId,
        at: Timestamp,
    ) -> Result<RemovedWorkspace> {
        self.remove_workspace_replacing(workspace, Lifecycle::Delete, Some(attempt), at)
    }

    /// A move with the verified authority projection and a replacement
    /// selected for the source Profile. The move itself is the root operation
    /// of `root_move::commit_root_move`, published with the projection in one
    /// catalog transaction; see [`Self::move_workspace`] for `name`, see
    /// [`Self::archive_workspace_replacing`].
    pub fn move_workspace_replacing(
        &mut self,
        workspace: WorkspaceId,
        to: ProfileId,
        _name: &str,
        at: Timestamp,
    ) -> Result<MovedWorkspace> {
        self.refuse_profile_removal(self.workspace_including_deleted(workspace)?.owner)?;
        self.refuse_profile_removal(to)?;
        self.profile(to)?;
        self.refuse_holder(to, "a move destination")?;
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
        let ready = self.prepare_removal_replacement(&row, at)?;
        self.commit_root_move(&projected)?;
        Ok(MovedWorkspace {
            workspace: projected,
            from,
            replacement: adopt_replacement(ready),
        })
    }

    fn remove_workspace_replacing(
        &mut self,
        workspace: WorkspaceId,
        lifecycle: Lifecycle,
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
            self.prepare_removal_replacement(&row, at)?
        };
        let already_committed = match lifecycle {
            Lifecycle::Archive => authority.archived,
            Lifecycle::Delete => authority.deleted_at.is_some(),
        };
        if !already_committed {
            self.commit_removal(workspace, lifecycle, at)?;
            match lifecycle {
                Lifecycle::Archive => projected.archived = true,
                Lifecycle::Delete => projected.deleted_at = Some(at),
            }
        }
        let removal = match lifecycle {
            Lifecycle::Archive => Removal::Archive(&projected),
            Lifecycle::Delete => Removal::Delete(&projected),
        };
        // No prepared replacement travels with the removal any more: a
        // selected Workspace is already listed, and nothing is created.
        let publication = self.catalog.publish_removal(removal, None);
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
                replacement: adopt_replacement(ready),
            }),
            // The removal is committed in the manifest and cannot be undone
            // to repair a listing; the listing is repaired on the retry.
            Err(error) => Ok(RemovedWorkspace {
                workspace: projected,
                authority_committed: true,
                listing: ListingState::Pending {
                    cause: error.code().as_str().into(),
                },
                replacement: Replacement::Unknown,
            }),
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

    /// Select another verified live Workspace of the source Profile, or
    /// answer that none is needed. **Never creates one** (§6.3): a Profile
    /// about to be left empty stays empty, as a valid state.
    fn prepare_removal_replacement(&mut self, removed: &Workspace, at: Timestamp) -> Result<Ready> {
        self.profile(removed.owner)?;
        // Whole-Profile removal has a separate, attempt-checked path. Ordinary
        // lifecycle requests must not select around that cascade.
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
        // Unknown bytes are not evidence that this was the last workspace, and
        // the refusal stands even though nothing would be created: the caller
        // is about to retire the one Workspace it can read.
        if let Some(error) = unresolved {
            return Err(error);
        }
        Ok(Ready::NotNeeded)
    }
}

fn adopt_replacement(ready: Ready) -> Replacement {
    match ready {
        Ready::NotNeeded => Replacement::NotNeeded,
        Ready::Existing(workspace) => Replacement::Existing(workspace),
    }
}
