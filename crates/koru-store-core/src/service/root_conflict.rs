// SPDX-License-Identifier: MPL-2.0

//! **A Workspace filed in two places** (`docs/features/root-catalog/QUESTIONS.md`
//! Q10, decided 2026-11-09, case 3 of the four, under
//! [rule D](../../../../docs/features/root-catalog/QUESTIONS.md): ambiguity
//! asks the person, it is never resolved silently).
//!
//! Case 1 is [`root_repair`](super::root_repair): at the v1 window's exit the
//! device publishes the unit the window withheld, placed by the hint, and
//! records what it did. That repair is the first write of a unit nobody else
//! had seen, so it starts a lineage of its own. If a device that held a real
//! unit all along is absent across the whole window and the inventory read,
//! its placement arrives afterwards and merges against the repair — and the
//! two resolve by Loro's map ordering, which can let the hint win over a
//! move the person actually made.
//!
//! The merged value still stands: nothing here rewrites a placement. What
//! this module adds is that the device **knows** it happened, durably, so
//! part three can ask *"this Workspace was filed in two places"* and offer
//! both Profiles. Why that is enough, and why no format changes: **only the
//! repairing device needs to know its own write was a bootstrap, and it can
//! know that locally.**
//!
//! ```text
//! commit_workspace_root_import                     the one inbound choke point
//!   bootstrap_repair(W)              no record -> nothing at all, as before
//!   placement_writes_concurrent_with(staged.before, repair.operation(), rivals)
//!     Unknown                        -> nothing: an orphaned id is not a conflict
//!     Ordered { settled: false }     -> nothing: it saw the repair
//!     Ordered { settled: true }      -> delete the record: answered elsewhere
//!     Concurrent(rivals)             -> rival Profile == the repair's? nothing
//!                                       (two devices agreeing is not ambiguity)
//!                                    -> else append, in the commit's own
//!                                       transaction
//! ```
//!
//! Four things here are load-bearing and easy to undo by accident:
//!
//! - **The gate is not "the placement changed".** In the dangerous case the
//!   merged placement does *not* change, so gating on a change would stay
//!   silent in exactly the case this exists for and speak up only when
//!   things resolved the way the person wanted. The gate is that a
//!   concurrent operation touched the placement at all.
//! - **The repair's Profile comes from the record, never from the unit.** By
//!   the time this runs the merged placement may already have moved, and the
//!   question is about what *the repair* asserted.
//! - **Comparing the two Profiles is not cosmetic.** Disjoint lineages are
//!   normal here — even the migration seed authors a per-device lineage and
//!   agrees with other devices only through a deterministic move id — so
//!   "concurrent" is a weak predicate system-wide. It is strong *here* only
//!   because the repair is the first write of a unit nobody else had seen,
//!   and rule D says in as many words that two devices agreeing on the same
//!   placement is clear and stays automatic. A rival that names the repair's
//!   own Profile is therefore not recorded.
//! - **The record is written in the commit's own transaction**, by being
//!   appended to the `state` the publication already carries, as the join
//!   receipt beside it is. A conflict recorded without the import, or an
//!   import without its conflict, would both be lies about the same merge.

use super::*;
use crate::ports::RootPublication;
use crate::root_catalog::{PlacementConcurrency, WorkspaceCatalogUnit};
use loro::VersionVector;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Prefix of the `store_state` key one Workspace's conflict record is kept
/// under: `root_catalog.bootstrap_conflict.<workspace id>`, JSON of
/// [`BootstrapPlacementConflict`]. One key per Workspace under the
/// `root_catalog.` prefix, the shape
/// [`K_BOOTSTRAP_REPAIR_PREFIX`](super::K_BOOTSTRAP_REPAIR_PREFIX) and the
/// join receipt already established.
///
/// A `store_state` row rather than a catalog-unit field for the reason case
/// 3 exists at all: only the repairing device needs any of this, and
/// recording it here leaves the unit format untouched and teaches no other
/// device anything.
pub const K_BOOTSTRAP_CONFLICT_PREFIX: &str = "root_catalog.bootstrap_conflict.";

pub fn bootstrap_conflict_key(workspace: WorkspaceId) -> String {
    format!("{K_BOOTSTRAP_CONFLICT_PREFIX}{}", workspace.to_storage())
}

/// **One Workspace this device filed by a bootstrap hint and somebody else
/// filed somewhere else.** Durable, device-local, never replicated.
///
/// It exists to be read by the question: *"this Workspace was filed in two
/// places"*, offering the repair's Profile and the rival's. Everything in it
/// is a value that **was asserted**, not a value that currently stands —
/// what stands is the merged placement, which
/// [`Store::root_placement`](crate::service::Store::root_placement) answers
/// and which may move again before the person is asked. Comparing
/// [`Self::repair_move_id`] and [`RivalPlacement::move_id`] with that
/// placement's own `move_id` is how a reader tells which of the two is in
/// force, without this record having to be rewritten when it changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapPlacementConflict {
    /// The Profile the repair asserted, copied from
    /// [`BootstrapRepair::profile`](super::BootstrapRepair::profile) rather
    /// than re-read from the unit.
    pub repaired_profile: ProfileId,
    /// The repair's fresh move id: the placement value it wrote.
    pub repair_move_id: Uuid,
    /// Every concurrent placement write seen so far, oldest first. Normally
    /// one; a second device that was also absent adds a second. Never empty:
    /// the record is written only when there is something to put in it.
    pub rivals: Vec<RivalPlacement>,
}

/// One placement write that was concurrent with the repair.
///
/// The Profile is **raw**, as the write asserted it: a Profile this device
/// does not hold, holds hidden or reads as a Holder is still what the other
/// device said, and the rules for rendering such a Profile are the ones the
/// existing Unassigned recovery already applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RivalPlacement {
    /// The Profile that write placed the Workspace under. `None` is
    /// *explicitly unassigned* — a value somebody wrote, not an absence.
    pub profile: Option<ProfileId>,
    /// The move id that write wrote, which identifies its placement value.
    pub move_id: Uuid,
    /// The peer the concurrent operation was written under, and its counter:
    /// the identity that keeps a rival from being recorded twice and lets a
    /// later write be checked for having seen it.
    pub writer_peer: u64,
    pub counter: i32,
    /// When this device noticed it, not when the write was made: the
    /// authoring device's clock is not readable here and would not be
    /// trusted if it were.
    pub at_ms: i64,
}

impl RivalPlacement {
    /// The concurrent operation, as Loro names it.
    pub fn operation(&self) -> loro::ID {
        loro::ID::new(self.writer_peer, self.counter)
    }
}

impl<C: Catalog, B: Backend> Store<C, B> {
    /// What this device recorded about `workspace` having been filed in two
    /// places, or `None` when it has not. The input to part three's
    /// question; reading it changes nothing.
    pub fn bootstrap_placement_conflict(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Option<BootstrapPlacementConflict>> {
        self.catalog
            .state(&bootstrap_conflict_key(workspace))?
            .map(|json| {
                serde_json::from_str(&json).map_err(|_| {
                    Error::new(
                        Code::CorruptState,
                        "a bootstrap placement conflict record is not decodable",
                    )
                })
            })
            .transpose()
    }

    /// Clear the record for `workspace`: **the person answered.** Their
    /// answer is an ordinary move, and this is the other half of it — the
    /// question is settled whichever Profile they chose, including the one
    /// the Workspace is already under.
    ///
    /// Idempotent, and it is not an error to clear a Workspace that has no
    /// record: a question answered on two surfaces at once must not fail the
    /// second time.
    pub fn clear_bootstrap_placement_conflict(&mut self, workspace: WorkspaceId) -> Result<()> {
        if self.bootstrap_placement_conflict(workspace)?.is_none() {
            return Ok(());
        }
        self.catalog.publish_root(
            self.catalog.local_user_scope()?,
            &RootPublication {
                units: Vec::new(),
                projections: Vec::new(),
                state: vec![(bootstrap_conflict_key(workspace), None)],
            },
        )?;
        tracing::info!(
            workspace = %workspace,
            "root conflict: the two-places record was cleared"
        );
        Ok(())
    }

    /// The `store_state` row an inbound Workspace import implies for case 3,
    /// to be published **inside the commit's own transaction**, or `None`
    /// when there is nothing to say.
    ///
    /// `merged` is the candidate the import produced and `before` the
    /// version it merged into — the two things the choke point already has
    /// in hand. A Workspace this device never repaired returns `None` before
    /// anything else happens, so the ordinary commit is untouched.
    pub(super) fn bootstrap_conflict_state(
        &self,
        workspace: WorkspaceId,
        merged: &WorkspaceCatalogUnit,
        before: &VersionVector,
    ) -> Result<Option<(String, Option<String>)>> {
        let Some(repair) = self.bootstrap_repair(workspace)? else {
            return Ok(None);
        };
        let held = self.bootstrap_placement_conflict(workspace)?;
        let recorded: Vec<loro::ID> = held
            .iter()
            .flat_map(|record| record.rivals.iter().map(RivalPlacement::operation))
            .collect();
        let rivals = match merged.placement_writes_concurrent_with(
            before,
            repair.operation(),
            &recorded,
        ) {
            // The recorded identity is not in this history. A device's writer
            // id is re-minted without being persisted when its stored row
            // cannot be parsed, so this is an orphan rather than a conflict,
            // and silence is the only honest answer.
            PlacementConcurrency::Unknown => {
                tracing::debug!(
                    workspace = %workspace,
                    "root conflict: the repair operation is not in this history; nothing recorded"
                );
                return Ok(None);
            }
            PlacementConcurrency::Ordered { settled: false } => return Ok(None),
            PlacementConcurrency::Ordered { settled: true } => {
                if held.is_none() {
                    return Ok(None);
                }
                // A placement write authored where the repair and every
                // recorded rival were both visible is the answer the
                // question would have asked for (rule D: a write whose
                // causal history already contains the other is clear).
                tracing::info!(
                    workspace = %workspace,
                    "root conflict: an arriving move saw both placements; the record is cleared"
                );
                return Ok(Some((bootstrap_conflict_key(workspace), None)));
            }
            PlacementConcurrency::Concurrent(rivals) => rivals,
        };
        let mut record = held.unwrap_or(BootstrapPlacementConflict {
            repaired_profile: repair.profile,
            repair_move_id: repair.move_id,
            rivals: Vec::new(),
        });
        let at_ms = Timestamp::now().millis();
        let mut added = 0usize;
        for rival in rivals {
            let Some(asserted) = rival.asserted else {
                // Concurrent, and touched the register, but its own version
                // does not read a placement. A question that cannot name the
                // other Profile is not one a person can answer.
                tracing::warn!(
                    workspace = %workspace,
                    "root conflict: a concurrent placement write asserted no readable value"
                );
                continue;
            };
            if asserted.profile == Some(record.repaired_profile) {
                // Two devices agreeing on the same placement is clear and
                // stays automatic (rule D), however the lineages ran.
                continue;
            }
            if record
                .rivals
                .iter()
                .any(|held| held.operation() == rival.operation)
            {
                continue;
            }
            record.rivals.push(RivalPlacement {
                profile: asserted.profile,
                move_id: asserted.move_id,
                writer_peer: rival.operation.peer,
                counter: rival.operation.counter,
                at_ms,
            });
            added += 1;
        }
        if added == 0 {
            return Ok(None);
        }
        let json = serde_json::to_string(&record).map_err(|e| {
            Error::new(
                Code::Internal,
                format!("a bootstrap placement conflict record is not serializable: {e}"),
            )
        })?;
        tracing::info!(
            workspace = %workspace,
            repaired = %record.repaired_profile,
            rivals = record.rivals.len(),
            "root conflict: this Workspace was filed in two places"
        );
        Ok(Some((bootstrap_conflict_key(workspace), Some(json))))
    }
}

#[cfg(test)]
mod tests;
