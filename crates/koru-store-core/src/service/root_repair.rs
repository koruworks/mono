// SPDX-License-Identifier: MPL-2.0

//! **The withheld unit, published at the v1 window's exit**
//! (`docs/features/root-catalog/QUESTIONS.md` Q10, decided 2026-11-09, case 1
//! of the four).
//!
//! Q7 decided that a device bootstrapping a Workspace from Cloud during the
//! v1 window publishes **no** Workspace unit
//! (`ReplicaPlacement::HintWithoutUnit`), so the first real unit to arrive
//! from a linked device is the only placement and the legacy owner column
//! cannot race a move the person already made. Nothing ever published the
//! withheld unit, so an account whose only reachable device bootstrapped that
//! way keeps the Workspace out of `root_export_inventory` for good: no Cloud
//! placement, no grant following a move, no personal sync for it.
//!
//! This module is the repair. The *decision* that it is time belongs to the
//! Cloud pass — once it has run under `CloudContract::V2` and read the
//! service's **full** inventory and finished its pulls, a row that is still
//! unit-less proves no unit exists on the service or here — and this module
//! is what it then asks for:
//!
//! ```text
//! repair_bootstrap_units(at)      per live Workspace with no unit at all
//!   1. hint          the `workspaces.owner` projection, the only evidence there is
//!   2. prepare       plan_placement_write: set_placement(Some(hint), FRESH move id)
//!                    routed by the hint Profile's ceiling — durable unit, or overlay
//!   3. identify      the planned snapshot's (writer_peer, version[writer_peer] - 1)
//!   4. publish       Catalog::publish_root, one transaction: the unit write and
//!                    store_state root_catalog.bootstrap_repair.<W>
//! ```
//!
//! Four things about it are load-bearing and easy to undo by accident:
//!
//! - **The move id is fresh** (`Uuid::new_v4()`), never the deterministic
//!   seed the 12→13 migration and `provision_replica_placed` use. Two devices
//!   that both repaired would otherwise agree on a move id they never shared,
//!   and the later concurrency check would have nothing to see.
//! - **`Store::move_workspace` cannot be reused.** A move to the Profile the
//!   Workspace already reads as writes nothing — `already_placed_under`
//!   treats a row with no unit as agreeing with its projection — so the
//!   repair reaches the planner directly.
//! - **The write is routed by the hint Profile's transport ceiling.** Under a
//!   Local-only or fenced Profile it lands in the private overlay and the
//!   Workspace stays out of the inventory. That is the Local-only rule
//!   working as decided (Q5/D258), not a defect to defeat: the existing
//!   publication job publishes it if the Profile is ever raised.
//! - **The record and the unit commit together or not at all**, through
//!   `Catalog::publish_root` — the same atomicity `commit_staged_root_unit`
//!   gives its join receipt. A record describing a unit that was not written
//!   would make the later check ask about an operation that does not exist;
//!   a unit with no record would make the repair invisible to it.
//!
//! What this module deliberately does **not** do: detect a later concurrent
//! placement (case 3 of the decision), and show the person anything (case 4).
//! Both build on [`BootstrapRepair`], which is why it is an interface rather
//! than a log line.

use super::root_move::plan_placement_write;
use super::*;
use crate::ids::UserScopeId;
use crate::ports::{RootPublication, RootUnitWrite};
use crate::root_catalog::{CatalogUnitRef, Placement, WorkspaceCatalogUnit};
use crate::transport::TransportSubject;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Prefix of the `store_state` key one repaired Workspace's record is kept
/// under: `root_catalog.bootstrap_repair.<workspace id>`, JSON of
/// [`BootstrapRepair`]. One key per Workspace under the `root_catalog.`
/// prefix, the shape [`K_PURGED_SUBJECT_EXPORT_PREFIX`] already established
/// for a private fact about one subject that the catalog unit must not carry.
///
/// A `store_state` row rather than a catalog-unit field because **only the
/// repairing device needs to know its own write was a bootstrap** (Q10's
/// "Case 3 is feasible"): recording it here leaves the unit format untouched
/// and teaches no other device anything.
///
/// [`K_PURGED_SUBJECT_EXPORT_PREFIX`]: super::K_PURGED_SUBJECT_EXPORT_PREFIX
pub const K_BOOTSTRAP_REPAIR_PREFIX: &str = "root_catalog.bootstrap_repair.";

pub fn bootstrap_repair_key(workspace: WorkspaceId) -> String {
    format!("{K_BOOTSTRAP_REPAIR_PREFIX}{}", workspace.to_storage())
}

/// What one repair wrote, as the two pieces that build on it need it.
///
/// It is durable, device-local and never replicated: it says what *this*
/// device did on a person's behalf, which is exactly the thing no other
/// device can be told without changing the unit format.
///
/// Three fields and a timestamp, and each one has a caller:
///
/// - the **operation's identity** (`writer_peer`, `counter`) is what the
///   later check compares against an arriving placement write. `set_placement`
///   is exactly one insert and one commit, so the repair is one operation and
///   its id is a single [`loro::ID`] — [`Self::operation`]. Loro's
///   `cmp_frontiers` answers `Ok(None)` for *concurrent* and
///   `Err(FrontiersNotIncluded)` for *that id is not in this history*, and
///   the second must be read as **don't know**, never as a conflict: the peer
///   id a device writes under is re-minted without being persisted when its
///   stored row cannot be parsed, so a recorded identity can be orphaned.
/// - the **hint Profile** is what the later confirmation names: *"Koru filed
///   this under &lt;Profile&gt; from Cloud's earlier record."* It is kept here
///   rather than re-read from the unit because by the time the person is
///   asked, a concurrent write may have moved the merged placement — and the
///   question is about what *the repair* asserted.
/// - the **fresh move id** is the placement value the repair wrote, so the
///   later check can compare the two Profile values and offer both.
///
/// Reading it back never changes it, and nothing here clears it: the later
/// surface deletes the key when the person answers, which is why `state`
/// pairs a key with `None` to delete.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapRepair {
    /// The Profile the repair placed the Workspace under: the `workspaces.owner`
    /// projection the v1 bootstrap wrote, which was the only evidence of where
    /// the Workspace was.
    pub profile: ProfileId,
    /// The move id the repair's `set_placement` wrote. **Fresh** by
    /// construction; never the deterministic seed.
    pub move_id: Uuid,
    /// The peer the repair operation was written under: the Store's writer for
    /// a durable write, the overlay's own writer for a private one. The
    /// operation keeps this identity when an overlay is later folded into the
    /// durable unit, because an import preserves operation ids.
    pub writer_peer: u64,
    /// The repair operation's counter — `version()[writer_peer] - 1`, because
    /// a Loro version-vector counter is an exclusive end.
    pub counter: i32,
    pub at_ms: i64,
}

impl BootstrapRepair {
    /// The one operation the repair wrote, as Loro names it.
    pub fn operation(&self) -> loro::ID {
        loro::ID::new(self.writer_peer, self.counter)
    }

    /// [`Self::operation`] as the single-id frontier `cmp_frontiers` takes.
    pub fn frontiers(&self) -> loro::Frontiers {
        loro::Frontiers::from_id(self.operation())
    }
}

/// One Workspace this pass repaired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepairedWorkspace {
    pub workspace: WorkspaceId,
    pub repair: BootstrapRepair,
    /// `true` when the unit reached the durable row and may now be exported;
    /// `false` when the hint Profile's ceiling put it in the private overlay,
    /// where it waits for the existing publication job.
    pub durable: bool,
}

impl<C: Catalog, B: Backend> Store<C, B> {
    /// What this device recorded about its own repair of `workspace`, or
    /// `None` when it never repaired it. The input to the later concurrency
    /// check and the later confirmation; reading it changes nothing.
    pub fn bootstrap_repair(&self, workspace: WorkspaceId) -> Result<Option<BootstrapRepair>> {
        self.catalog
            .state(&bootstrap_repair_key(workspace))?
            .map(|json| {
                serde_json::from_str(&json).map_err(|_| {
                    Error::new(
                        Code::CorruptState,
                        "a bootstrap repair record is not decodable",
                    )
                })
            })
            .transpose()
    }

    /// **Q10's repair.** Publish the withheld unit of every live Workspace
    /// this device holds with no unit at all, each one placed by the hint its
    /// row still projects, with a fresh move id, and each one's record written
    /// in the same transaction.
    ///
    /// The caller owns the *decision*: this is only called once a Cloud pass
    /// has run under `CloudContract::V2`, read the service's full inventory
    /// and finished its pulls, because only then does a still-unit-less row
    /// prove that no unit exists on the service or here. Called at any other
    /// moment it would publish a placement that a real unit still in flight
    /// could race — which is the hazard Q7 was decided to remove.
    ///
    /// Idempotent by construction rather than by a flag: a repaired Workspace
    /// has a unit (durable or overlay), so it is not a candidate again.
    ///
    /// A Workspace in the Trash is left alone. Its unit is not merely absent
    /// but *undecided*: the deletion the row projects came from a manifest
    /// hint, and the lifecycle writers already mint a unit carrying that
    /// deletion fact when they touch it (`root_lifecycle::plan_workspace_write`).
    /// Publishing a live placement here would assert the opposite.
    ///
    /// One Workspace's failure does not stop the others: it is logged with its
    /// code and skipped, exactly as a purge step is, and the next pass tries
    /// again. `Err` is reserved for not being able to read the candidate list
    /// at all.
    pub fn repair_bootstrap_units(&mut self, at: Timestamp) -> Result<Vec<RepairedWorkspace>> {
        let scope = self.catalog.local_user_scope()?;
        let rows = self.catalog.workspaces_including_deleted()?;
        let mut repaired = Vec::new();
        for row in rows {
            if row.deleted_at.is_some() {
                continue;
            }
            match self.repair_one(scope, &row, at) {
                Ok(None) => {}
                Ok(Some(one)) => repaired.push(one),
                Err(error) => {
                    tracing::warn!(
                        workspace = %row.id,
                        code = %error.code(),
                        "root repair: the withheld unit was not published; pending until the next pass"
                    );
                }
            }
        }
        if !repaired.is_empty() {
            // A placement write moves this device's ceiling for the
            // Workspace exactly as a local move does.
            self.transport.invalidate();
        }
        Ok(repaired)
    }

    /// `Ok(None)` when `row` already has a unit and is therefore not a
    /// candidate; `Ok(Some(..))` when the withheld unit was published.
    fn repair_one(
        &mut self,
        scope: UserScopeId,
        row: &Workspace,
        at: Timestamp,
    ) -> Result<Option<RepairedWorkspace>> {
        let workspace = row.id;
        let unit = CatalogUnitRef::Workspace { workspace };
        let durable = self.catalog.root_catalog_unit(scope, unit)?;
        let overlay = self.catalog.root_catalog_overlay(scope, unit)?;
        if durable.is_some() || overlay.is_some() {
            return Ok(None);
        }
        // The hint, and the ceiling it is routed by, are both the Profile the
        // row projects. Q10: if that Profile is gone, a Holder or absent, the
        // republished placement reads as unassigned and the Workspace surfaces
        // in the existing Unassigned recovery — which is intended, not a hole.
        let hint = row.owner;
        let move_id = Uuid::new_v4();
        let admission =
            self.transport_admission_under(TransportSubject::Workspace(workspace), hint)?;
        let write = plan_placement_write(
            workspace,
            None,
            None,
            admission.allows_peer(),
            self.peer_id,
            Placement {
                profile: Some(hint),
                move_id,
            },
        )?;
        let (writer_peer, counter) = repair_operation(&write, workspace, self.peer_id)?;
        let repair = BootstrapRepair {
            profile: hint,
            move_id,
            writer_peer,
            counter,
            at_ms: at.millis(),
        };
        let durable_write = matches!(write, RootUnitWrite::Durable { .. });
        let json = serde_json::to_string(&repair).map_err(|e| {
            Error::new(
                Code::Internal,
                format!("a bootstrap repair record is not serializable: {e}"),
            )
        })?;
        // No projection: the hint *is* the owner the row already names, so a
        // repair changes no `workspaces` column. The unit and the record are
        // the whole operation.
        self.catalog.publish_root(
            scope,
            &RootPublication {
                units: vec![(unit, write)],
                projections: Vec::new(),
                state: vec![(bootstrap_repair_key(workspace), Some(json))],
            },
        )?;
        tracing::info!(
            workspace = %workspace,
            profile = %hint,
            channel = if durable_write { "durable" } else { "overlay" },
            "root repair: withheld root unit published for the v1 Cloud bootstrap"
        );
        Ok(Some(RepairedWorkspace {
            workspace,
            repair,
            durable: durable_write,
        }))
    }
}

/// The repair operation's identity, read back off the bytes the planner
/// produced.
///
/// `set_placement` is exactly one insert and one commit, so the write added
/// exactly one operation and its id is `(writer_peer, version[writer_peer] - 1)`
/// — `VersionVector::get_last` *is* that subtraction, because a version-vector
/// counter is an exclusive end. The document's oplog frontiers immediately
/// after the commit is the same single id; either reads it.
///
/// **Why the snapshot is re-opened rather than the planner returning the
/// version.** `root_move::plan_unit_write` is the one planner every root write
/// goes through — a move, a trash, a restore, purge evidence, a Profile
/// deletion's markers — and its closure cannot hand a value back. Threading a
/// post-mutation version out of it would change its signature, its four arms
/// and every call site for one caller's benefit, and would leave the planner
/// knowing something about *why* it was called. Re-opening the planned bytes
/// is what `placement_of` beside it already does, it is the same two lines for
/// both arms of [`RootUnitWrite`], and it reads the authoritative result
/// rather than a value computed alongside it.
///
/// The writer must be the one the write was authored under — the Store's for a
/// durable commit, the overlay's own for a private one — because `load` hands
/// the writer stream to whatever peer it is given, so asking the reloaded
/// document for its `writer_peer` would only echo the argument.
fn repair_operation(
    write: &RootUnitWrite,
    workspace: WorkspaceId,
    store_peer: u64,
) -> Result<(u64, i32)> {
    let (snapshot, writer) = match write {
        RootUnitWrite::Durable { commit, .. } => (&commit.snapshot, store_peer),
        RootUnitWrite::Overlay(overlay) => (&overlay.private_snapshot, overlay.writer_peer_id),
    };
    let unit = WorkspaceCatalogUnit::from_snapshot(snapshot, workspace, writer)?;
    let counter = unit.version().get_last(writer).ok_or_else(|| {
        Error::new(
            Code::CorruptState,
            "the repaired unit has no operation under the writer it was authored by",
        )
    })?;
    Ok((writer, counter))
}

#[cfg(test)]
mod tests;
