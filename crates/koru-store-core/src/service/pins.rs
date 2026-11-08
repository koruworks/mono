// SPDX-License-Identifier: MPL-2.0

//! A pin commits one manifest key before updating its disposable listing.

use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PinnedWorkspace {
    pub workspace: Workspace,
    pub listing: ListingState,
}

struct RestoredManifest {
    manifest: Manifest,
    reloaded: bool,
}

/// Preserve the writer's peer identity even when the saved replica cannot be
/// decoded. The caller owns the ordinary document replicas and commit windows;
/// neither recovery path replaces that owner.
fn restore_manifest(
    snapshot: &[u8],
    peer: u64,
    reload: impl FnOnce() -> Result<Manifest>,
) -> Result<RestoredManifest> {
    if let Ok(manifest) = Manifest::from_snapshot(snapshot).and_then(|manifest| {
        manifest.set_peer_id(peer)?;
        Ok(manifest)
    }) {
        return Ok(RestoredManifest {
            manifest,
            reloaded: false,
        });
    }
    let manifest = reload()?;
    manifest.set_peer_id(peer)?;
    Ok(RestoredManifest {
        manifest,
        reloaded: true,
    })
}

impl<C: Catalog, B: Backend> Store<C, B> {
    pub fn set_workspace_pinned(
        &mut self,
        workspace: WorkspaceId,
        pinned: bool,
        at: Timestamp,
    ) -> Result<PinnedWorkspace> {
        self.refuse_profile_removal(self.workspace_including_deleted(workspace)?.owner)?;
        // This may evict a remotely retired workspace. Inspect and reacquire
        // only after its accepted manifest frames have reached durability.
        self.flush_workspace_manifest(workspace, at)?;
        let row = self.workspace_including_deleted(workspace)?;
        let authority = match self.backend.inspect_workspace_manifest(workspace) {
            WorkspaceManifestInspection::Verified(authority) => authority,
            WorkspaceManifestInspection::Unavailable(error) => return Err(error),
            _ => {
                return Err(Error::new(
                    Code::CorruptState,
                    "workspace authority is not verified",
                ));
            }
        };
        let mut projected = availability::project(&row, &authority)?;
        if projected.deleted_at.is_some() || projected.archived {
            return Err(Error::bare(Code::WorkspaceNotFound));
        }
        self.ensure_open(workspace)?;
        let open = self
            .open
            .get_mut(&workspace)
            .ok_or_else(|| Error::bare(Code::WorkspaceNotFound))?;
        let snapshot = open.manifest.export_snapshot()?;
        let peer = open.manifest.peer_id();
        let before = open.manifest.version();
        if authority.pinned != pinned {
            let committed = (|| {
                open.manifest.set_pinned(pinned)?;
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
            if let Err(error) = committed {
                // Document replicas and their queued windows remain owned by
                // this Open. Rolling the whole workspace back would lose them.
                let restored = restore_manifest(&snapshot, peer, || {
                    let durable = open_document(&open.repo, open.manifest_id)?;
                    Manifest::from_snapshot(&durable.into_value().export_snapshot()?)
                });
                match restored {
                    Ok(restored) => {
                        open.manifest = restored.manifest;
                        if restored.reloaded {
                            return Err(Error::new(
                                Code::CorruptState,
                                "pin rollback required a durable manifest reload",
                            ));
                        }
                    }
                    Err(_) => {
                        return Err(Error::new(
                            Code::CorruptState,
                            "pin rollback could not reload durable manifest authority",
                        ));
                    }
                }
                return Err(error);
            }
        }
        projected.pinned = pinned;
        let listing = match self.catalog.put_workspace(&projected) {
            Ok(()) => ListingState::Applied,
            Err(_) => match self.reconcile_workspace_listing(workspace, at) {
                Ok(repaired) => {
                    projected = repaired;
                    ListingState::Applied
                }
                Err(error) => ListingState::Pending {
                    cause: error.code().as_str().into(),
                },
            },
        };
        Ok(PinnedWorkspace {
            workspace: projected,
            listing,
        })
    }
}

#[cfg(test)]
mod tests;
