// SPDX-License-Identifier: MPL-2.0

//! Seeding root units from the legacy catalog projection
//! (`docs/features/root-catalog/TECHNICAL.final.md` §8.3): one Profile unit
//! per `profiles` row and one Workspace unit per `workspaces` row, with the
//! exact UUIDs, under the Store's writer, with a one-time receipt.
//!
//! The facts a seed writes carry **deterministic ids**: two devices that
//! migrate the same legacy state independently must agree on the id of the
//! move that placed a Workspace and of the deletion that trashed it, or the
//! first sync between them would see two placements and two deletions of
//! one subject. Both ids are UUIDv5 under [`ROOT_SEED_NAMESPACE`] over the
//! Workspace UUID (plus the deletion time for a deletion), so the same
//! legacy row seeds the same fact everywhere.
//!
//! Two legacy facts are deliberately *not* carried over as shared facts
//! (§2.1): a Profile's device-local `removed_at` is local suppression, not a
//! deletion, so its unit is seeded live; and a `ProfileKind::Holder` row is
//! legacy inventory, never a root Profile (§6.3), so it gets no unit at all
//! and is recorded as excluded in the receipt.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{CatalogUnitRefusal, DeletionRecord, ProfileCatalogUnit, WorkspaceCatalogUnit};
use crate::ids::{DeletionId, ProfileId, WorkspaceId};

/// The UUIDv5 namespace of every id a seed derives. A fixed constant: its
/// value is arbitrary and its stability is the whole point, so it is spelled
/// out once here and never computed. Generated as a v4 UUID on 2026-11-09.
pub const ROOT_SEED_NAMESPACE: Uuid = Uuid::from_bytes([
    0x5e, 0x1d, 0x9c, 0x42, 0x7a, 0x3b, 0x4f, 0x8e, 0x9d, 0x2c, 0x6b, 0x1f, 0x0a, 0x8d, 0x3e, 0x57,
]);

/// The `move_id` of the placement a seed writes for `workspace`: UUIDv5 over
/// the Workspace UUID alone. Legacy state holds one placement per Workspace,
/// so one move id per Workspace is exactly what two devices must agree on.
pub fn seeded_move_id(workspace: WorkspaceId) -> Uuid {
    Uuid::new_v5(&ROOT_SEED_NAMESPACE, workspace.as_uuid().as_bytes())
}

/// The id of the deletion fact a seed writes for a trashed `workspace`:
/// UUIDv5 over the Workspace UUID followed by `deleted_at_ms` big-endian.
pub fn seeded_deletion_id(workspace: WorkspaceId, deleted_at_ms: i64) -> DeletionId {
    let mut name = Vec::with_capacity(24);
    name.extend_from_slice(workspace.as_uuid().as_bytes());
    name.extend_from_slice(&deleted_at_ms.to_be_bytes());
    DeletionId::from_uuid(Uuid::new_v5(&ROOT_SEED_NAMESPACE, &name))
}

/// What one seed did, stored by SQL as JSON text in
/// `root_catalog_migration_receipts` under `kind = 'root_seed'`. Ids only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "seed", rename_all = "snake_case")]
pub enum RootSeedReceipt {
    /// A Profile unit was seeded live. `legacy_removed_at_ms` records the
    /// device-local suppression that was *not* turned into a deletion.
    Profile {
        profile: ProfileId,
        writer_peer_id: u64,
        legacy_removed_at_ms: Option<i64>,
    },
    /// A `ProfileKind::Holder` row: no unit, by §6.3.
    HolderExcluded { profile: ProfileId },
    /// A Workspace unit was seeded with its placement and, for a trashed
    /// Workspace, one deletion fact.
    Workspace {
        workspace: WorkspaceId,
        writer_peer_id: u64,
        placement_profile: ProfileId,
        move_id: Uuid,
        deleted_at_ms: Option<i64>,
        deletion_id: Option<DeletionId>,
        /// The projection's owner is a Holder row. The placement still names
        /// it, because the projection is this commit's authority (Plan 1.2
        /// makes the root unit authoritative); the receipt says so.
        owner_is_holder: bool,
    },
}

/// A live Profile unit for `profile`, written by `writer`.
pub fn seed_profile_unit(profile: ProfileId, writer: u64) -> Result<Vec<u8>, CatalogUnitRefusal> {
    ProfileCatalogUnit::create(profile, writer)?.snapshot()
}

/// A Workspace unit for `workspace` placed under `owner` by
/// [`seeded_move_id`], plus one deletion fact at `deleted_at_ms` when the
/// row is in the Trash — [`DeletionRecord::new`]'s default retention, no
/// batch, under [`seeded_deletion_id`].
pub fn seed_workspace_unit(
    workspace: WorkspaceId,
    owner: ProfileId,
    deleted_at_ms: Option<i64>,
    writer: u64,
) -> Result<Vec<u8>, CatalogUnitRefusal> {
    let unit = WorkspaceCatalogUnit::create(workspace, writer)?;
    unit.set_placement(Some(owner), seeded_move_id(workspace))?;
    if let Some(at_ms) = deleted_at_ms {
        unit.record_deletion(
            seeded_deletion_id(workspace, at_ms),
            &DeletionRecord::new(at_ms, None),
        )?;
    }
    unit.snapshot()
}

#[cfg(test)]
mod tests;
