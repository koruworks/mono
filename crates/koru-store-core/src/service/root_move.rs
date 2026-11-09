// SPDX-License-Identifier: MPL-2.0

//! **Where a Workspace is, and how it moves** — the root Workspace unit's
//! `placement.current` as the authority for this user's organization, the
//! one prepared root operation a move is, and the private overlay a placement
//! waits in while it may not leave the device
//! (`docs/features/root-catalog/TECHNICAL.final.md` §4.3, §4.4, §6.2, §7.1).
//!
//! ```text
//! move_workspace(W, B)
//!   1. validate            B exists, is not a Holder, is not being removed; W is not trashed
//!   2. read                W's durable unit row, its overlay row, the mode rows
//!   3. prepare (Loro)      plan_placement_write: set_placement(Some(B), fresh move id)
//!                          on the durable unit under the Store's writer, or on the
//!                          overlay fork under its own writer — never both
//!   4. publish (SQL)       Catalog::publish_root_unit, one IMMEDIATE transaction:
//!                          the unit or overlay row, workspaces.owner/position,
//!                          local_transport_modes.owner_profile_id re-keyed
//!   5. acknowledge         after the commit; then bump the policy generation
//! ```
//!
//! Three rules hold the module together:
//!
//! - **Reads prefer the overlay.** [`Store::root_placement`] answers the
//!   private fork when one is held, else the durable unit, else `None` —
//!   *pending*, never an invented Profile. A placement under a
//!   `ProfileKind::Holder` reads as unassigned (§6.3: a Holder is neither a
//!   persona nor a recovery destination).
//! - **The destination decides the channel.** Whether the placement write may
//!   reach the durable unit is the Workspace's effective transport admission
//!   evaluated under the *destination* Profile's ceiling. Local-only or
//!   fenced means the overlay; anything exportable means the durable unit,
//!   folding a pending overlay in on the way (§4.3: "stage/import the overlay
//!   ... then atomically publish ... and clear the overlay").
//! - **Loro before SQL, validate at commit.** Every snapshot is authored and
//!   validated before the catalog transaction opens; the transaction holds the
//!   compare-and-set revision and nothing else is computed under it (§7.2's
//!   discipline, applied to a local operation).
//!
//! What is *not* root-authoritative yet: Workspace creation, replica
//! provisioning and the lifecycle markers (trash, restore, purge) still write
//! the `workspaces` projection directly — Plan 1.3 and M2. A Workspace that
//! has no unit yet is therefore read from the projection, as the only
//! evidence there is, and gains its unit on its first move.

use super::*;
use crate::ports::{RootCatalogOverlay, RootCatalogUnitCommit, RootCatalogUnitRow, RootUnitWrite};
use crate::root_catalog::{
    CatalogUnitKind, CatalogUnitRef, Placement, Staged, WorkspaceCatalogUnit,
};
use crate::transport::TransportSubject;
use loro::VersionVector;

/// Author the placement write for one move, deciding between the durable
/// unit and the private overlay from what the catalog holds and whether the
/// subject may export under its destination. Pure: in-memory Loro only.
///
/// | overlay held | export admitted | result |
/// | --- | --- | --- |
/// | no  | yes | durable unit (loaded or fresh) under the Store's writer |
/// | no  | no  | a new overlay forked at the durable unit's version, fresh writer; the durable bytes are untouched |
/// | yes | no  | the same overlay, same writer and base, with the new placement |
/// | yes | yes | the overlay's operations staged into the durable unit, then the new placement; the overlay is cleared with the commit |
///
/// A subject with no durable unit and no admission exists only in its
/// overlay (`base_vv` empty) until it is enabled — §4.3's "entirely
/// LocalOnly subject".
pub(super) fn plan_placement_write(
    workspace: WorkspaceId,
    durable: Option<&RootCatalogUnitRow>,
    overlay: Option<&RootCatalogOverlay>,
    export_admitted: bool,
    store_peer: u64,
    placement: Placement,
) -> Result<RootUnitWrite> {
    let unit = CatalogUnitRef::Workspace { workspace };
    match (overlay, export_admitted) {
        (Some(overlay), false) => {
            // The private fork keeps receiving edits until it is published.
            let fork = WorkspaceCatalogUnit::from_snapshot(
                &overlay.private_snapshot,
                workspace,
                overlay.writer_peer_id,
            )?;
            fork.set_placement(placement.profile, placement.move_id)?;
            Ok(RootUnitWrite::Overlay(RootCatalogOverlay {
                base_vv: overlay.base_vv.clone(),
                private_snapshot: fork.snapshot()?,
                writer_peer_id: overlay.writer_peer_id,
            }))
        }
        (None, false) => {
            // A fork at the durable unit's version with its own writer, so
            // the private operations keep their causal place and can later be
            // staged back without colliding with anything the Store writes.
            let writer = mint_peer_id();
            let (fork, base_vv) = match durable {
                Some(row) => {
                    let fork =
                        WorkspaceCatalogUnit::from_snapshot(&row.snapshot, workspace, writer)?;
                    let base_vv = fork.version().encode();
                    (fork, base_vv)
                }
                None => (
                    WorkspaceCatalogUnit::create(workspace, writer)?,
                    VersionVector::new().encode(),
                ),
            };
            fork.set_placement(placement.profile, placement.move_id)?;
            Ok(RootUnitWrite::Overlay(RootCatalogOverlay {
                base_vv,
                private_snapshot: fork.snapshot()?,
                writer_peer_id: writer,
            }))
        }
        (Some(overlay), true) => {
            let candidate = fold_overlay(workspace, durable, overlay, store_peer)?;
            candidate.set_placement(placement.profile, placement.move_id)?;
            Ok(RootUnitWrite::Durable {
                commit: RootCatalogUnitCommit {
                    unit,
                    snapshot: candidate.snapshot()?,
                    expected_revision: durable.map(|row| row.revision),
                },
                clear_overlay: true,
            })
        }
        (None, true) => {
            let candidate = match durable {
                Some(row) => {
                    WorkspaceCatalogUnit::from_snapshot(&row.snapshot, workspace, store_peer)?
                }
                None => WorkspaceCatalogUnit::create(workspace, store_peer)?,
            };
            candidate.set_placement(placement.profile, placement.move_id)?;
            Ok(RootUnitWrite::Durable {
                commit: RootCatalogUnitCommit {
                    unit,
                    snapshot: candidate.snapshot()?,
                    expected_revision: durable.map(|row| row.revision),
                },
                clear_overlay: false,
            })
        }
    }
}

/// Stage an overlay's operations since its `base_vv` into the durable unit
/// and hand back the validated candidate under the Store's writer. With no
/// durable unit the overlay's whole history *is* the candidate: the subject
/// existed only privately until now. Pure.
pub(super) fn fold_overlay(
    workspace: WorkspaceId,
    durable: Option<&RootCatalogUnitRow>,
    overlay: &RootCatalogOverlay,
    store_peer: u64,
) -> Result<WorkspaceCatalogUnit> {
    let private = WorkspaceCatalogUnit::from_snapshot(
        &overlay.private_snapshot,
        workspace,
        overlay.writer_peer_id,
    )?;
    let Some(row) = durable else {
        return Ok(WorkspaceCatalogUnit::from_snapshot(
            &private.snapshot()?,
            workspace,
            store_peer,
        )?);
    };
    let base = VersionVector::decode(&overlay.base_vv).map_err(|_| {
        Error::new(
            Code::CorruptState,
            "a root overlay's base version is undecodable",
        )
    })?;
    let frames = private.updates_since(&base)?;
    let unit = WorkspaceCatalogUnit::from_snapshot(&row.snapshot, workspace, store_peer)?;
    match unit.staged_import(&[frames])? {
        Staged::Applied(candidate) => Ok(candidate),
        // Unreachable for a fork of this very unit; said out loud rather than
        // adopted, because an `Ok` with pending ancestors is not applied state.
        Staged::Pending => Err(Error::new(
            Code::CorruptState,
            "a root overlay's operations do not follow from the unit it was forked from",
        )),
    }
}

fn placement_of(snapshot: &[u8], workspace: WorkspaceId, writer: u64) -> Result<Option<Placement>> {
    Ok(WorkspaceCatalogUnit::from_snapshot(snapshot, workspace, writer)?.placement())
}

fn transport_subject(unit: CatalogUnitRef) -> TransportSubject {
    match unit {
        CatalogUnitRef::Profile { profile } => TransportSubject::Profile(profile),
        CatalogUnitRef::Workspace { workspace } => TransportSubject::Workspace(workspace),
    }
}

impl<C: Catalog, B: Backend> Store<C, B> {
    /// This user's current placement of `workspace`: the private overlay's
    /// register when an overlay is held, else the durable unit's. `None` is
    /// *pending* — no unit yet, as for a Workspace created or provisioned
    /// before the root wrote it — and never an invented Profile.
    ///
    /// A placement under a `ProfileKind::Holder` Profile reads as
    /// **unassigned**: the migration seeded such placements from the legacy
    /// projection (`RootSeedReceipt::Workspace::owner_is_holder`), and §6.3
    /// says a Holder is legacy inventory, never a persona or a recovery
    /// destination. The stored register is left as it is; only the reading
    /// resolves it to `profile: None`.
    pub fn root_placement(&self, workspace: WorkspaceId) -> Result<Option<Placement>> {
        let scope = self.catalog.local_user_scope()?;
        let unit = CatalogUnitRef::Workspace { workspace };
        let placement = match self.catalog.root_catalog_overlay(scope, unit)? {
            Some(overlay) => {
                placement_of(&overlay.private_snapshot, workspace, overlay.writer_peer_id)?
            }
            None => match self.catalog.root_catalog_unit(scope, unit)? {
                Some(row) => placement_of(&row.snapshot, workspace, self.peer_id)?,
                None => None,
            },
        };
        placement
            .map(|placement| self.holder_reads_unassigned(placement))
            .transpose()
    }

    fn profile_is_holder(&self, profile: ProfileId) -> Result<bool> {
        Ok(self
            .catalog
            .profile_including_removed(profile)?
            .is_some_and(|row| row.is_holder()))
    }

    fn holder_reads_unassigned(&self, placement: Placement) -> Result<Placement> {
        match placement.profile {
            Some(profile) if self.profile_is_holder(profile)? => Ok(Placement {
                profile: None,
                move_id: placement.move_id,
            }),
            _ => Ok(placement),
        }
    }

    /// The Workspaces whose current placement is `profile`, Trash included —
    /// what the Profile-oriented personal facade answers over.
    ///
    /// Root placement first ([`Self::root_placement`]'s rule, overlay before
    /// durable unit). A Workspace with no unit yet falls back to the
    /// `workspaces.owner` projection, which is the only evidence of where it
    /// is until a root operation writes it. A Holder places nothing (§6.3).
    pub(super) fn placed_workspaces(&self, profile: ProfileId) -> Result<BTreeSet<WorkspaceId>> {
        if self.profile_is_holder(profile)? {
            return Ok(BTreeSet::new());
        }
        let scope = self.catalog.local_user_scope()?;
        let units: BTreeMap<WorkspaceId, RootCatalogUnitRow> = self
            .catalog
            .root_catalog_units(scope, CatalogUnitKind::Workspace)?
            .into_iter()
            .filter_map(|(unit, row)| unit.workspace().map(|workspace| (workspace, row)))
            .collect();
        let mut placed = BTreeSet::new();
        for row in self.catalog.workspaces_including_deleted()? {
            let unit = CatalogUnitRef::Workspace { workspace: row.id };
            let placement = match self.catalog.root_catalog_overlay(scope, unit)? {
                Some(overlay) => {
                    placement_of(&overlay.private_snapshot, row.id, overlay.writer_peer_id)?
                }
                None => match units.get(&row.id) {
                    Some(held) => placement_of(&held.snapshot, row.id, self.peer_id)?,
                    None => None,
                },
            };
            let here = match placement {
                Some(placement) => placement.profile == Some(profile),
                None => row.owner == profile,
            };
            if here {
                placed.insert(row.id);
            }
        }
        Ok(placed)
    }

    /// The root half of a move, after the caller has validated it and
    /// projected `projected` (new owner and position): prepare the placement
    /// write in memory, publish it with the projection in one catalog
    /// transaction, then invalidate the policy generation so every transport
    /// callback holding an older answer asks again under the new ceiling.
    ///
    /// The channel is decided under the **destination** Profile's ceiling: a
    /// move into a Local-only or fenced Profile goes to the overlay and the
    /// durable bytes stay exactly as they were, so nothing about the new
    /// placement can be advertised. A stored choice above the new ceiling is
    /// kept and reported through `transport_admission`'s `ceiling_mismatch`
    /// — never repaired here (§4.4); the local move succeeds regardless (§9).
    pub(super) fn commit_root_move(&mut self, projected: &Workspace) -> Result<()> {
        let workspace = projected.id;
        let scope = self.catalog.local_user_scope()?;
        let unit = CatalogUnitRef::Workspace { workspace };
        let durable = self.catalog.root_catalog_unit(scope, unit)?;
        let overlay = self.catalog.root_catalog_overlay(scope, unit)?;
        let admission = self
            .transport_admission_under(TransportSubject::Workspace(workspace), projected.owner)?;
        let write = plan_placement_write(
            workspace,
            durable.as_ref(),
            overlay.as_ref(),
            admission.allows_peer(),
            self.peer_id,
            Placement {
                profile: Some(projected.owner),
                move_id: uuid::Uuid::new_v4(),
            },
        )?;
        let channel = match &write {
            RootUnitWrite::Durable { .. } => "durable",
            RootUnitWrite::Overlay(_) => "overlay",
        };
        self.catalog
            .publish_root_unit(scope, unit, write, Some(projected))?;
        // After the durable commit. Late callbacks recheck rather than reuse;
        // the ceiling (and a fence closed by a failed restrictive write) is
        // what stops a disallowed path, so nothing is closed here that the
        // existing mode-correction path would not know to reopen.
        self.transport.invalidate();
        tracing::info!(
            workspace = %workspace,
            channel,
            ceiling_mismatch = admission.ceiling_mismatch,
            "root placement moved"
        );
        Ok(())
    }

    /// Publish a Workspace's pending overlay into its durable unit, once the
    /// subject's effective admission allows export: stage the overlay's
    /// operations since `base_vv`, validate the complete candidate, recheck
    /// the policy generation, then commit the unit and clear the overlay in
    /// one transaction. `Ok(false)` when there is nothing to publish or the
    /// subject may still not export; the overlay is left exactly as it was.
    ///
    /// Called from the transport-mode commit path when a subject's admission
    /// turns from no-export to export, and folded into a move whose
    /// destination admits export. Profile units carry no overlay in this
    /// plan (their lifecycle writer is Plan 1.3) and answer `Ok(false)`.
    pub fn publish_pending_overlay(&mut self, unit: CatalogUnitRef) -> Result<bool> {
        let CatalogUnitRef::Workspace { workspace } = unit else {
            return Ok(false);
        };
        let scope = self.catalog.local_user_scope()?;
        let Some(overlay) = self.catalog.root_catalog_overlay(scope, unit)? else {
            return Ok(false);
        };
        let generation = self.policy_generation();
        if !self
            .transport_admission(TransportSubject::Workspace(workspace))?
            .allows_peer()
        {
            return Ok(false);
        }
        let durable = self.catalog.root_catalog_unit(scope, unit)?;
        let candidate = fold_overlay(workspace, durable.as_ref(), &overlay, self.peer_id)?;
        let commit = RootCatalogUnitCommit {
            unit,
            snapshot: candidate.snapshot()?,
            expected_revision: durable.map(|row| row.revision),
        };
        // §4.4: a late publication rechecks the policy generation before it
        // commits. Under the Store lock nothing can have moved it, and saying
        // so here is what keeps the rule in the code rather than in a comment.
        if self.policy_generation() != generation {
            return Ok(false);
        }
        self.catalog.publish_root_unit(
            scope,
            unit,
            RootUnitWrite::Durable {
                commit,
                clear_overlay: true,
            },
            None,
        )?;
        tracing::info!(workspace = %workspace, "pending root overlay published");
        Ok(true)
    }

    /// **The export projection's seam** (§4.3 "Safe history export"): the
    /// authoritative durable snapshot of `unit`, only while the subject's
    /// effective admission allows a transport. `None` when it does not, when
    /// no unit exists yet, or while a private overlay is pending — the
    /// overlay is published first ([`Self::publish_pending_overlay`]) so the
    /// export never carries a placement the device has already superseded.
    ///
    /// Redaction of historical Profile references is the export module's
    /// business, not this method's: these are the bytes it starts from.
    pub fn eligible_export_source(&self, unit: CatalogUnitRef) -> Result<Option<Vec<u8>>> {
        if !self
            .transport_admission(transport_subject(unit))?
            .allows_peer()
        {
            return Ok(None);
        }
        let scope = self.catalog.local_user_scope()?;
        if self.catalog.root_catalog_overlay(scope, unit)?.is_some() {
            return Ok(None);
        }
        Ok(self
            .catalog
            .root_catalog_unit(scope, unit)?
            .map(|row| row.snapshot))
    }
}

#[cfg(test)]
mod tests;
