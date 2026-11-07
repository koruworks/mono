// SPDX-License-Identifier: MPL-2.0

//! Selection reads durable authority before repairing a disposable listing.

use super::*;
use crate::profile_removal::{RemovalMark, removal_key};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WorkspaceAvailabilityIssue {
    pub workspace: WorkspaceId,
    /// Machine-readable cause, localized by the client.
    pub cause: String,
    pub recovery: Option<crate::ports::Recovery>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EnsuredWorkspace {
    Active {
        workspace: Workspace,
        issues: Vec<WorkspaceAvailabilityIssue>,
    },
    Created {
        workspace: Workspace,
    },
    Pending {
        workspaces: Vec<WorkspaceId>,
    },
    ListingPending {
        issues: Vec<WorkspaceAvailabilityIssue>,
    },
    Uncertain {
        issues: Vec<WorkspaceAvailabilityIssue>,
    },
    Removing {
        mark: RemovalMark,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WorkspaceAuthority {
    pub workspace: Workspace,
    pub authority: WorkspaceAuthorityState,
    pub listing_differs: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkspaceAuthorityState {
    Read {
        archived: bool,
        deleted_at: Option<Timestamp>,
        pinned: bool,
        applied: bool,
    },
    Unreadable {
        cause: String,
        recovery: Option<crate::ports::Recovery>,
    },
}

/// A recovered value deliberately has no verified projection. Callers may
/// present recovery, but may not infer absence or lifecycle from its prefix.
pub enum WorkspaceManifestInspection {
    Verified(VerifiedWorkspaceManifest),
    Pending { manifest: DocumentId },
    Unavailable(Error),
    Recovered(crate::ports::Recovery),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedWorkspaceManifest {
    pub manifest: DocumentId,
    pub workspace: WorkspaceId,
    pub name: String,
    pub created_at: Timestamp,
    pub icon: Option<Option<String>>,
    pub color: Option<Option<String>>,
    pub pinned: bool,
    pub archived: bool,
    pub deleted_at: Option<Timestamp>,
    /// Only physically retained documents claimed by the manifest, plus the
    /// manifest itself. Orphaned bytes cannot regain routing by inspection.
    pub documents: Vec<DocumentId>,
}

impl<C: Catalog, B: Backend> Store<C, B> {
    /// Owner-scoped, read-only overlay. No live Loro state participates.
    pub fn workspace_authority(&self, owner: ProfileId) -> Result<Vec<WorkspaceAuthority>> {
        self.profile(owner)?;
        self.catalog
            .workspaces_including_deleted()?
            .into_iter()
            .filter(|w| w.owner == owner)
            .map(|workspace| {
                let (authority, listing_differs) =
                    match self.backend.inspect_workspace_manifest(workspace.id) {
                        WorkspaceManifestInspection::Verified(verified) => {
                            let projected = project(&workspace, &verified)?;
                            let current: BTreeSet<_> = self
                                .catalog
                                .workspace_locations(workspace.id)?
                                .into_iter()
                                .collect();
                            let desired: BTreeSet<_> = if verified.deleted_at.is_some() {
                                BTreeSet::new()
                            } else {
                                verified.documents.into_iter().collect()
                            };
                            (
                                WorkspaceAuthorityState::Read {
                                    archived: verified.archived,
                                    deleted_at: verified.deleted_at,
                                    pinned: verified.pinned,
                                    applied: true,
                                },
                                projected != workspace || desired != current,
                            )
                        }
                        WorkspaceManifestInspection::Pending { .. } => (
                            WorkspaceAuthorityState::Read {
                                archived: false,
                                deleted_at: None,
                                pinned: false,
                                applied: false,
                            },
                            false,
                        ),
                        WorkspaceManifestInspection::Unavailable(error) => (
                            WorkspaceAuthorityState::Unreadable {
                                cause: error.code().as_str().into(),
                                recovery: None,
                            },
                            false,
                        ),
                        WorkspaceManifestInspection::Recovered(recovery) => (
                            WorkspaceAuthorityState::Unreadable {
                                cause: "workspace_recovered".into(),
                                recovery: Some(recovery),
                            },
                            false,
                        ),
                    };
                Ok(WorkspaceAuthority {
                    workspace,
                    authority,
                    listing_differs,
                })
            })
            .collect()
    }

    pub fn profile_removal(&self, owner: ProfileId) -> Result<Option<RemovalMark>> {
        self.catalog
            .state(&removal_key(owner))?
            .map(|json| {
                serde_json::from_str(&json)
                    .map_err(|_| Error::new(Code::CorruptState, "invalid Profile removal record"))
            })
            .transpose()
    }

    /// Read-only authority for controllers and gateways. No open, flush or
    /// projection is implicit in this method.
    pub fn inspect_workspace_manifest(
        &self,
        workspace: WorkspaceId,
    ) -> Result<WorkspaceManifestInspection> {
        self.workspace_including_deleted(workspace)?;
        Ok(self.backend.inspect_workspace_manifest(workspace))
    }

    pub fn ensure_live_workspace(
        &mut self,
        owner: ProfileId,
        name: &str,
        at: Timestamp,
    ) -> Result<EnsuredWorkspace> {
        // Removal wins even for an already-hidden owner; opening never clears it.
        if let Some(mark) = self.profile_removal(owner)? {
            return Ok(EnsuredWorkspace::Removing { mark });
        }
        self.profile(owner)?;
        let mut rows: Vec<_> = self
            .catalog
            .workspaces_including_deleted()?
            .into_iter()
            .filter(|w| w.owner == owner)
            .collect();
        rows.sort_by_key(|w| (w.position, w.id));
        // Finish prerequisite windows before any consequential repair/default.
        // A flush may evict an open workspace. Never retain a reference to it.
        for row in &rows {
            self.flush_workspace_manifest(row.id, at)?;
        }
        let mut active = None;
        let mut uncertain = Vec::new();
        let mut pending = Vec::new();
        let mut listing = Vec::new();
        for old in rows {
            let row = self.workspace_including_deleted(old.id)?;
            match self.backend.inspect_workspace_manifest(row.id) {
                WorkspaceManifestInspection::Verified(authority) => {
                    match self.reconcile_verified_workspace(&row, &authority, at) {
                        Ok(repaired) if !repaired.archived && repaired.deleted_at.is_none() => {
                            if active.is_none() {
                                active = Some(repaired);
                            }
                        }
                        Ok(_) => {}
                        Err(error) => listing.push(issue(row.id, error.code().as_str(), None)),
                    }
                }
                WorkspaceManifestInspection::Pending { .. } => pending.push(row.id),
                WorkspaceManifestInspection::Unavailable(error) => {
                    uncertain.push(issue(row.id, error.code().as_str(), None))
                }
                WorkspaceManifestInspection::Recovered(recovery) => {
                    uncertain.push(issue(row.id, "workspace_recovered", Some(recovery)))
                }
            }
        }
        if let Some(workspace) = active {
            let mut issues = uncertain;
            issues.extend(listing);
            issues.extend(
                pending
                    .into_iter()
                    .map(|id| issue(id, "workspace_pending", None)),
            );
            return Ok(EnsuredWorkspace::Active { workspace, issues });
        }
        if !uncertain.is_empty() {
            return Ok(EnsuredWorkspace::Uncertain { issues: uncertain });
        }
        if !pending.is_empty() {
            return Ok(EnsuredWorkspace::Pending {
                workspaces: pending,
            });
        }
        if !listing.is_empty() {
            return Ok(EnsuredWorkspace::ListingPending { issues: listing });
        }
        Ok(EnsuredWorkspace::Created {
            workspace: self.create_workspace(owner, name, at)?,
        })
    }

    pub(super) fn flush_workspace_manifest(
        &mut self,
        workspace: WorkspaceId,
        at: Timestamp,
    ) -> Result<()> {
        if let Some(manifest) = self.open.get(&workspace).map(|open| open.manifest_id) {
            self.flush(workspace, manifest, at)?;
        }
        Ok(())
    }

    /// Explicit repair, never the implementation of the read-only endpoint.
    pub fn reconcile_workspace_listing(
        &mut self,
        workspace: WorkspaceId,
        at: Timestamp,
    ) -> Result<Workspace> {
        self.flush_workspace_manifest(workspace, at)?;
        let row = self.workspace_including_deleted(workspace)?;
        match self.backend.inspect_workspace_manifest(workspace) {
            WorkspaceManifestInspection::Verified(authority) => {
                self.reconcile_verified_workspace(&row, &authority, at)
            }
            WorkspaceManifestInspection::Unavailable(error) => Err(error),
            _ => Err(Error::new(
                Code::CorruptState,
                "workspace authority is not verified",
            )),
        }
    }

    pub(super) fn reconcile_verified_workspace(
        &mut self,
        row: &Workspace,
        authority: &VerifiedWorkspaceManifest,
        at: Timestamp,
    ) -> Result<Workspace> {
        let projected = project(row, authority)?;
        let desired: BTreeSet<_> = if projected.deleted_at.is_some() {
            BTreeSet::new()
        } else {
            authority.documents.iter().copied().collect()
        };
        let current: BTreeSet<_> = self
            .catalog
            .workspace_locations(row.id)?
            .into_iter()
            .collect();
        if &projected != row || desired != current {
            // A stale location can be repaired only after its old owner's
            // durable manifest disclaims it. Uncertainty never transfers data.
            for document in &desired {
                if let Some(other) = self.catalog.locate(*document)?
                    && other != row.id
                {
                    self.flush_workspace_manifest(other, at)?;
                    if self.catalog.workspace(other)?.is_some() {
                        match self.backend.inspect_workspace_manifest(other) {
                            WorkspaceManifestInspection::Verified(other)
                                if other.deleted_at.is_some()
                                    || !other.documents.contains(document) => {}
                            _ => {
                                return Err(Error::new(
                                    Code::CorruptState,
                                    "document routing authority is unresolved",
                                ));
                            }
                        }
                    }
                }
            }
            self.catalog.reconcile_workspace_listing(
                &projected,
                &desired.into_iter().collect::<Vec<_>>(),
            )?;
        }
        if projected.deleted_at.is_some() {
            self.open.remove(&row.id);
        }
        Ok(projected)
    }
}

pub(super) fn project(row: &Workspace, authority: &VerifiedWorkspaceManifest) -> Result<Workspace> {
    let mut projected = row.clone();
    projected.rename(&authority.name)?;
    if let Some(icon) = &authority.icon {
        projected.icon = icon.clone();
    }
    if let Some(color) = &authority.color {
        projected.color = color.clone();
    }
    projected.pinned = authority.pinned;
    projected.archived = authority.archived;
    projected.deleted_at = authority.deleted_at;
    Ok(projected)
}

fn issue(
    workspace: WorkspaceId,
    cause: &str,
    recovery: Option<crate::ports::Recovery>,
) -> WorkspaceAvailabilityIssue {
    WorkspaceAvailabilityIssue {
        workspace,
        cause: cause.to_owned(),
        recovery,
    }
}
