// SPDX-License-Identifier: MPL-2.0

//! **Lifecycle on the root** — trash, restore, the Profile-deletion batch
//! and its restore, and the purge with personal retirement
//! (`docs/features/root-catalog/TECHNICAL.final.md` §3.6–3.8, §5.3, §6).
//!
//! Since Plan 1.3 the root catalog unit is the **sole write authority** for
//! Workspace and Profile trash/restore/purge state. Every operation here is
//! the same shape as a move (`root_move.rs`): prepare the unit edit in
//! memory through the one planner, [`plan_unit_write`], then commit it with
//! its **one-way projections** in one `Catalog::publish_root` transaction,
//! and acknowledge after that commit.
//!
//! ```text
//! delete_workspace(W, at)          restore_workspace(W)
//!   unit: deletions += {at, 30d, —}   unit: restores += {observed: active ids, —}
//!   row:  deleted_at = at, routing −   row:  deleted_at = ∅, routing +
//!
//! delete_profile(P, preview, at)                 restore_profile(P, deletion)
//!   P unit:  deletions += {at, 30d, batch}         P unit:  restores += {[deletion], batch}
//!   each target W: deletions += {at, 30d, batch}   each job target W: restores += {[its batch id], batch}
//!   rows: targets deleted_at = at; P hidden        rows: P visible; W restored when nothing else holds it
//!   job:  store_state root_catalog.batch.<batch>   job:  phase → restored
//!
//! purge_deleted(before)              per expired Workspace unit, each step restartable
//!   1 unit: purged += {at}           2 evict readers   3 retire THIS scope's personal row
//!   4 backend bytes                  5 row and derived rows; the unit row stays
//! ```
//!
//! Three distinctions the code keeps and the reader should too:
//!
//! - **Shared deletion** is a root fact (`deletions[id]`) that converges
//!   between devices. **Device-local removal** (`profiles.removed_at`,
//!   `Store::remove_profile_local`, the attempts of `removal.rs`) is local
//!   suppression and never global-delete consent (§2.1). The two share one
//!   *visibility projection* — a deleted Profile leaves the ordinary listings
//!   through the same `removed_at` column — and nothing else; the unit says
//!   which it is.
//! - **The manifest is a hint.** `Manifest::set_workspace_deleted_at` is
//!   still written after a root commit so a pre-13 peer sees the Trash, but
//!   an incoming manifest value never produces a root mutation and never
//!   changes `workspaces.deleted_at` for a Workspace that has a unit
//!   ([`Store::root_deleted_at`]). Once a Workspace has a unit, an old
//!   manifest cannot restore or re-delete it.
//! - **The job is local; the markers are shared.** A Profile deletion's
//!   exact target list lives in `store_state` under
//!   [`batch_job_key`]; each target's unit carries only the opaque batch id.
//!   A Profile unit never lists its children (§4.2).

use super::root_move::plan_unit_write;
use super::*;
use crate::ids::{BatchId, DeletionId, RestoreId, UserScopeId};
use crate::ports::{PurgeHold, RootProjection, RootPublication};
use crate::root_catalog::{
    CatalogUnitKind, CatalogUnitRef, DeletionRecord, ProfileCatalogUnit, PurgeRecord,
    RestoreRecord, WorkspaceCatalogUnit,
};
use crate::transport::TransportSubject;
use serde::{Deserialize, Serialize};

/// Prefix of the `store_state` key a Profile deletion's application job is
/// kept under: `root_catalog.batch.<batch id>`, JSON of
/// [`ProfileDeletionJob`]. A `store_state` row because the exact target
/// list is private to this device (§4.2 "Private target lists and batch
/// application jobs remain in SQLite") and because the approved design
/// reuses the existing journals rather than adding a table.
pub const K_ROOT_CATALOG_BATCH_PREFIX: &str = "root_catalog.batch.";

pub fn batch_job_key(batch: BatchId) -> String {
    format!("{K_ROOT_CATALOG_BATCH_PREFIX}{}", batch.to_storage())
}

/// Where a Profile-deletion batch has got to on this device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum BatchPhase {
    /// The markers and projections are committed; the Profile is in the Trash.
    Deleted,
    /// A batch restore observed the Profile's deletion and each target's.
    Restored { restore: RestoreId, at_ms: i64 },
    /// Every target was purged and the Profile unit carries purge evidence.
    Purged { at_ms: i64 },
}

/// The local application job of one Profile deletion: the exact targets
/// captured at confirmation, so a restore or a purge never recomputes them
/// from a later catalog and never touches a Workspace that was not named
/// (§6.1 "Do not recompute the target list later").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProfileDeletionJob {
    pub batch_id: BatchId,
    pub profile: ProfileId,
    pub profile_deletion: DeletionId,
    pub at_ms: i64,
    pub retention_ms: i64,
    /// Each captured Workspace and the deletion fact this batch wrote on it.
    pub targets: Vec<(WorkspaceId, DeletionId)>,
    pub phase: BatchPhase,
}

/// What a Profile deletion would trash right now, with a revision the caller
/// echoes back so a confirmation taken over a stale list is refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProfileDeletionPreview {
    /// The live Workspaces currently placed under the Profile, by root
    /// placement, in listing order.
    pub targets: Vec<WorkspaceId>,
    /// BLAKE3 (first 64 bits) over the Profile unit's revision and, per
    /// target in order, its id, its durable revision and its overlay bytes.
    /// Any move, trash, restore or overlay write on an involved unit changes
    /// it; the number itself means nothing.
    pub placement_revision: u64,
}

/// The committed batch: what the caller publishes events for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProfileDeletionBatch {
    pub batch_id: BatchId,
    pub profile_deletion: DeletionId,
    pub targets: Vec<(WorkspaceId, DeletionId)>,
}

/// One restorable Profile deletion, as the Trash and Manage surfaces list it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DeletedProfile {
    pub profile: ProfileId,
    pub deletion: DeletionId,
    pub batch_id: BatchId,
    pub at_ms: i64,
    pub retention_ms: i64,
    /// The captured targets, from the job.
    pub targets: Vec<WorkspaceId>,
    /// Targets whose content is gone or past its retention at `now`: a
    /// restore will report them honestly rather than recreate them.
    pub expired_or_purged: Vec<WorkspaceId>,
}

/// What a batch restore did to each captured target (§6.3).
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProfileRestoreOutcome {
    /// Back in the listing under the restored Profile.
    pub restored: Vec<WorkspaceId>,
    /// The batch's deletion was observed, but an earlier or unseen
    /// independent deletion still holds the Workspace in the Trash.
    pub left_in_trash: Vec<WorkspaceId>,
    /// Already restored by somebody and placed under another Profile;
    /// nothing was written, the placement stands.
    pub moved_elsewhere: Vec<WorkspaceId>,
    /// Content purged; the unit's evidence is retained and nothing is
    /// recreated.
    pub purged: Vec<WorkspaceId>,
}

/// What one purge pass did (§5.3, §9). Never a bare count: a held or pending
/// Workspace is as much a result as a purged one.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PurgeOutcome {
    pub purged: Vec<WorkspaceId>,
    /// Content kept because a personal unit this purge may not retire holds
    /// it. Only that Workspace waits.
    pub held: Vec<(WorkspaceId, PurgeHold)>,
    /// A step failed; the purge fact is committed and the next pass resumes
    /// from the step that failed.
    pub pending: Vec<(WorkspaceId, Code)>,
    /// Profile units that gained purge evidence because every target of
    /// their deletion batch is purged.
    pub profiles_purged: Vec<ProfileId>,
}

/// Why a live Workspace is reachable from no active Profile (§3.6, §6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum UnassignedReason {
    /// `placement.current.profile_id` is null: somebody unassigned it.
    ExplicitlyUnassigned,
    /// Its Profile's unit holds an active deletion; this Workspace was not a
    /// captured target of that batch, so it stays live.
    ProfileDeleted,
    /// Its Profile has no row in this catalog.
    ProfileAbsent,
    /// Its register names a `ProfileKind::Holder` row (legacy inventory).
    HolderPlaced,
}

/// A live Workspace the Unassigned recovery item lists. Moving it is the
/// ordinary `Store::move_workspace`; restoring its Profile is
/// `Store::restore_profile`. No Holder is ever created for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UnassignedWorkspace {
    pub workspace: Workspace,
    pub former_profile: Option<ProfileId>,
    pub reason: UnassignedReason,
}

/// Which Workspace projection a lifecycle write carries.
enum Lifecycle {
    Deleted,
    Restored,
}

impl<C: Catalog, B: Backend> Store<C, B> {
    // ----------------------------------------------------------- readers

    /// The root's answer to "is this Workspace in the Trash": `Some(Some(at))`
    /// while a deletion is active (`at` is the earliest active one),
    /// `Some(None)` when the unit holds none, `None` when the Workspace has
    /// no unit and only the manifest/projection can answer (a row without
    /// one, which no creation or provisioning path produces any more). The
    /// seam at which an incoming manifest's
    /// `deleted_at` becomes a hint: every projection of lifecycle asks here
    /// first.
    pub(super) fn root_deleted_at(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Option<Option<Timestamp>>> {
        let Some(unit) = self.effective_workspace_unit(workspace)? else {
            return Ok(None);
        };
        Ok(Some(earliest_active_deletion(&unit)))
    }

    /// `deleted_at` as this device must treat it: the root fact when the
    /// Workspace has a unit, else the hint the caller holds.
    pub(super) fn effective_deleted_at(
        &self,
        workspace: WorkspaceId,
        hint: Option<Timestamp>,
    ) -> Result<Option<Timestamp>> {
        Ok(self.root_deleted_at(workspace)?.unwrap_or(hint))
    }

    /// Whether the Profile unit holds an active (unrestored, unpurged)
    /// deletion. `false` for a Profile with no unit.
    pub(super) fn profile_is_deleted(&self, profile: ProfileId) -> Result<bool> {
        Ok(self
            .effective_profile_unit(profile)?
            .is_some_and(|unit| unit.is_deleted()))
    }

    fn read_job(&self, batch: BatchId) -> Result<Option<ProfileDeletionJob>> {
        self.catalog
            .state(&batch_job_key(batch))?
            .map(|json| {
                serde_json::from_str(&json).map_err(|_| {
                    Error::new(Code::CorruptState, "invalid Profile deletion job record")
                })
            })
            .transpose()
    }

    fn job_state(job: &ProfileDeletionJob) -> Result<(String, Option<String>)> {
        let json = serde_json::to_string(job)
            .map_err(|e| Error::new(Code::Internal, format!("job is not serializable: {e}")))?;
        Ok((batch_job_key(job.batch_id), Some(json)))
    }

    // ------------------------------------------------------ the planner

    /// Plan one Workspace unit's write under its **current owner's**
    /// ceiling. A unit-less Workspace gains its unit here, placed where the
    /// projection says it is — the only evidence there is, as on a first
    /// move.
    fn plan_workspace_write(
        &self,
        row: &Workspace,
        mutate: impl Fn(&WorkspaceCatalogUnit) -> Result<()>,
    ) -> Result<(CatalogUnitRef, crate::ports::RootUnitWrite)> {
        let scope = self.catalog.local_user_scope()?;
        let unit = CatalogUnitRef::Workspace { workspace: row.id };
        let durable = self.catalog.root_catalog_unit(scope, unit)?;
        let overlay = self.catalog.root_catalog_overlay(scope, unit)?;
        let admitted = self
            .transport_admission(TransportSubject::Workspace(row.id))?
            .allows_peer();
        let owner = row.owner;
        let write = plan_unit_write::<WorkspaceCatalogUnit>(
            unit,
            durable.as_ref(),
            overlay.as_ref(),
            admitted,
            self.peer_id,
            |candidate| {
                if candidate.placement().is_none() {
                    candidate.set_placement(Some(owner), uuid::Uuid::new_v4())?;
                }
                mutate(candidate)
            },
        )?;
        Ok((unit, write))
    }

    /// Plan one Profile unit's write under the Profile's own ceiling. A
    /// Profile with no unit yet gains one (an account-provisioned row before
    /// M3; the migration seeded every other Profile).
    fn plan_profile_write(
        &self,
        profile: ProfileId,
        mutate: impl Fn(&ProfileCatalogUnit) -> Result<()>,
    ) -> Result<(CatalogUnitRef, crate::ports::RootUnitWrite)> {
        let scope = self.catalog.local_user_scope()?;
        let unit = CatalogUnitRef::Profile { profile };
        let durable = self.catalog.root_catalog_unit(scope, unit)?;
        let overlay = self.catalog.root_catalog_overlay(scope, unit)?;
        let admitted = self
            .transport_admission(TransportSubject::Profile(profile))?
            .allows_peer();
        let write = plan_unit_write::<ProfileCatalogUnit>(
            unit,
            durable.as_ref(),
            overlay.as_ref(),
            admitted,
            self.peer_id,
            mutate,
        )?;
        Ok((unit, write))
    }

    /// The compatibility hint (§6.1): the manifest's own deletion state,
    /// written **after** the root commit so a pre-13 peer sees the Trash.
    /// It has no authority — a failure here is logged and changes nothing
    /// about the committed lifecycle — and an incoming value of it never
    /// produces a root mutation ([`Self::root_deleted_at`]).
    pub(super) fn hint_manifest_deletion(
        &mut self,
        workspace: WorkspaceId,
        deleted_at: Option<Timestamp>,
        at: Timestamp,
    ) {
        let result = (|| -> Result<()> {
            self.ensure_open_including_deleted(workspace)?;
            let Some(open) = self.open.get_mut(&workspace) else {
                return Ok(());
            };
            if open.manifest.workspace_deleted_at() == deleted_at {
                return Ok(());
            }
            let before = open.manifest.version();
            open.manifest.set_workspace_deleted_at(deleted_at)?;
            let update = open.manifest.export_updates_since(&before)?;
            let manifest_id = open.manifest_id;
            commit_one(
                open,
                workspace,
                &mut self.committed,
                &mut self.sync_dirty,
                manifest_id,
                update,
                at,
                crate::ports::UpdateOrigin::Store,
            )?;
            Ok(())
        })();
        if let Err(error) = result {
            tracing::warn!(
                workspace = %workspace,
                error = %error,
                "the manifest deletion hint was not written; the root fact stands"
            );
        }
    }

    pub(super) fn workspace_documents(
        &mut self,
        workspace: WorkspaceId,
    ) -> Result<Vec<DocumentId>> {
        Ok(match self.open.get(&workspace) {
            Some(open) => open.repo.documents()?,
            None => self.backend.open_workspace(workspace)?.documents()?,
        }
        .into_iter()
        .map(|document| document.id)
        .collect())
    }

    // ------------------------------------------------- Workspace trash

    /// Trash one Workspace: a deletion fact on its unit and the one-way
    /// projection (`deleted_at`, routing removed) in one transaction, then
    /// the manifest hint, then the open state is evicted so no writer keeps
    /// a durably deleted Workspace usable. Idempotent on an already-deleted
    /// Workspace.
    pub fn delete_workspace(&mut self, id: WorkspaceId, at: Timestamp) -> Result<Workspace> {
        let row = self.workspace_including_deleted(id)?;
        self.refuse_profile_removal(row.owner)?;
        if self.effective_deleted_at(id, row.deleted_at)?.is_some() {
            return Ok(row);
        }
        let mut projected = row.clone();
        projected.deleted_at = Some(at);
        self.commit_workspace_lifecycle(&projected, Lifecycle::Deleted, |unit| {
            Ok(unit.record_deletion(DeletionId::new(), &DeletionRecord::new(at.millis(), None))?)
        })?;
        self.hint_manifest_deletion(id, Some(at), at);
        self.open.remove(&id);
        tracing::info!(workspace = %id, "root lifecycle: workspace trashed");
        Ok(projected)
    }

    /// Restore one Workspace: a restore fact observing **every active
    /// deletion this device sees** — so after it nothing holds the Workspace
    /// in the Trash here — and the one-way projection (`deleted_at` cleared,
    /// routing re-inserted). A deletion this device has not yet received
    /// stays active when it arrives (§6.2 "Restore versus unseen deletion").
    pub fn restore_workspace(&mut self, id: WorkspaceId) -> Result<Workspace> {
        let row = self.workspace_including_deleted(id)?;
        self.refuse_profile_removal(row.owner)?;
        let at = Timestamp::now();
        match self.effective_workspace_unit(id)? {
            None => {
                // A row with no unit: the legacy projection path is the only
                // lifecycle it has until a root write gives it one.
                if row.deleted_at.is_none() {
                    return Ok(row);
                }
                self.require_profile(row.owner)?;
                self.ensure_open_including_deleted(id)?;
                let documents = self.workspace_documents(id)?;
                self.catalog.set_workspace_deleted(id, None, &documents)?;
                self.hint_manifest_deletion(id, None, at);
                let mut restored = row;
                restored.deleted_at = None;
                Ok(restored)
            }
            Some(unit) => {
                let active: Vec<DeletionId> = unit.active_deletions().into_iter().collect();
                if active.is_empty() {
                    if row.deleted_at.is_some() {
                        // The projection lags the root (a crash between the
                        // unit commit and nothing — impossible in one
                        // transaction — or a legacy row): repair it one-way.
                        self.ensure_open_including_deleted(id)?;
                        let documents = self.workspace_documents(id)?;
                        self.catalog.set_workspace_deleted(id, None, &documents)?;
                        let mut restored = row;
                        restored.deleted_at = None;
                        return Ok(restored);
                    }
                    return Ok(row);
                }
                // A locally removed or deleted owner is recovered only by its
                // own explicit operation (`restore_profile_local`,
                // `restore_profile`); refuse before writing anything.
                self.require_profile(row.owner)?;
                self.ensure_open_including_deleted(id)?;
                let documents = self.workspace_documents(id)?;
                let mut projected = row.clone();
                projected.deleted_at = None;
                let record = RestoreRecord {
                    at_ms: at.millis(),
                    observed_deletions: active,
                    batch_id: None,
                };
                self.commit_workspace_lifecycle_with_documents(
                    &projected,
                    Lifecycle::Restored,
                    &documents,
                    |unit| Ok(unit.record_restore(RestoreId::new(), &record)?),
                )?;
                self.hint_manifest_deletion(id, None, at);
                tracing::info!(workspace = %id, "root lifecycle: workspace restored");
                Ok(projected)
            }
        }
    }

    fn commit_workspace_lifecycle(
        &mut self,
        projected: &Workspace,
        lifecycle: Lifecycle,
        mutate: impl Fn(&WorkspaceCatalogUnit) -> Result<()>,
    ) -> Result<()> {
        self.commit_workspace_lifecycle_with_documents(projected, lifecycle, &[], mutate)
    }

    /// A deletion fact with its projection, for the replacing delete route,
    /// which has already validated and projected the row.
    pub(super) fn commit_workspace_deletion(
        &mut self,
        projected: &Workspace,
        mutate: impl Fn(&WorkspaceCatalogUnit) -> Result<()>,
    ) -> Result<()> {
        self.commit_workspace_lifecycle(projected, Lifecycle::Deleted, mutate)
    }

    fn commit_workspace_lifecycle_with_documents(
        &mut self,
        projected: &Workspace,
        lifecycle: Lifecycle,
        documents: &[DocumentId],
        mutate: impl Fn(&WorkspaceCatalogUnit) -> Result<()>,
    ) -> Result<()> {
        let scope = self.catalog.local_user_scope()?;
        let write = self.plan_workspace_write(projected, mutate)?;
        let projection = match lifecycle {
            Lifecycle::Deleted => RootProjection::WorkspaceDeleted(projected),
            Lifecycle::Restored => RootProjection::WorkspaceRestored {
                workspace: projected,
                documents,
            },
        };
        self.catalog.publish_root(
            scope,
            &RootPublication {
                units: vec![write],
                projections: vec![projection],
                state: Vec::new(),
            },
        )
    }

    // ------------------------------------------- Profile deletion batch

    /// The live Workspaces a deletion of `profile` would trash right now,
    /// with the revision [`Self::delete_profile`] checks. Read-only.
    pub fn profile_deletion_preview(&self, profile: ProfileId) -> Result<ProfileDeletionPreview> {
        self.refuse_holder(profile, "deleted")?;
        self.require_profile(profile)?;
        let targets = self.live_targets(profile)?;
        let placement_revision = self.placement_revision(profile, &targets)?;
        Ok(ProfileDeletionPreview {
            targets,
            placement_revision,
        })
    }

    /// Live Workspaces placed under `profile` by root placement, in listing
    /// order. A Workspace in the Trash is not a target: it already has its
    /// own deletion and keeps it.
    fn live_targets(&self, profile: ProfileId) -> Result<Vec<WorkspaceId>> {
        let placed = self.placed_workspaces(profile)?;
        let mut targets = Vec::new();
        for row in self.catalog.workspaces_including_deleted()? {
            if !placed.contains(&row.id) {
                continue;
            }
            if self.effective_deleted_at(row.id, row.deleted_at)?.is_none() {
                targets.push(row.id);
            }
        }
        Ok(targets)
    }

    fn placement_revision(&self, profile: ProfileId, targets: &[WorkspaceId]) -> Result<u64> {
        let scope = self.catalog.local_user_scope()?;
        let mut hasher = blake3::Hasher::new();
        let unit = CatalogUnitRef::Profile { profile };
        hasher.update(profile.as_uuid().as_bytes());
        hasher.update(
            &self
                .catalog
                .root_catalog_unit(scope, unit)?
                .map_or(0, |row| row.revision)
                .to_be_bytes(),
        );
        if let Some(overlay) = self.catalog.root_catalog_overlay(scope, unit)? {
            hasher.update(&overlay.private_snapshot);
        }
        for workspace in targets {
            let unit = CatalogUnitRef::Workspace {
                workspace: *workspace,
            };
            hasher.update(workspace.as_uuid().as_bytes());
            hasher.update(
                &self
                    .catalog
                    .root_catalog_unit(scope, unit)?
                    .map_or(0, |row| row.revision)
                    .to_be_bytes(),
            );
            if let Some(overlay) = self.catalog.root_catalog_overlay(scope, unit)? {
                hasher.update(&overlay.private_snapshot);
            }
        }
        let digest = hasher.finalize();
        Ok(u64::from_be_bytes(
            digest.as_bytes()[..8]
                .try_into()
                .expect("a BLAKE3 digest has 32 bytes"),
        ))
    }

    /// **Shared deletion of a Profile** (§6.1 last paragraph, §6.3): refuse a
    /// Holder, a removed Profile and a live removal attempt; recompute the
    /// targets and their revision under the Store lock and refuse a stale
    /// `preview` — a materially destructive confirmation taken over a list
    /// that has since changed requires a refresh; then commit, in one catalog
    /// transaction, the Profile unit's deletion fact, one deletion fact with
    /// the same batch id on every target's unit, the targets' `deleted_at`
    /// projections, the Profile's visibility suppression and the local
    /// application job. The targets are then evicted from the open set so no
    /// writer keeps editing them; the host's `NeedsProfile` transition is the
    /// UI half of this plan.
    pub fn delete_profile(
        &mut self,
        profile: ProfileId,
        preview: &ProfileDeletionPreview,
        at: Timestamp,
    ) -> Result<ProfileDeletionBatch> {
        self.refuse_holder(profile, "deleted")?;
        self.require_profile(profile)?;
        self.refuse_profile_removal(profile)?;
        if self.profile_is_deleted(profile)? {
            return Err(Error::new(
                Code::ProfileRemoved,
                "this Profile is already in the Trash",
            ));
        }
        let targets = self.live_targets(profile)?;
        if targets != preview.targets
            || self.placement_revision(profile, &targets)? != preview.placement_revision
        {
            return Err(Error::new(
                Code::IdempotencyConflict,
                "the Profile deletion preview is stale; refresh it before confirming",
            ));
        }
        let scope = self.catalog.local_user_scope()?;
        let batch = BatchId::new();
        let profile_deletion = DeletionId::new();
        let at_ms = at.millis();
        let record = DeletionRecord::new(at_ms, Some(batch.as_uuid()));

        let mut units = Vec::with_capacity(targets.len() + 1);
        units.push(self.plan_profile_write(profile, |unit| {
            Ok(unit.record_deletion(profile_deletion, &record)?)
        })?);
        let mut captured = Vec::with_capacity(targets.len());
        let mut projected_rows = Vec::with_capacity(targets.len());
        for workspace in &targets {
            let deletion = DeletionId::new();
            let mut row = self.workspace_including_deleted(*workspace)?;
            row.deleted_at = Some(at);
            units.push(
                self.plan_workspace_write(&row, |unit| {
                    Ok(unit.record_deletion(deletion, &record)?)
                })?,
            );
            captured.push((*workspace, deletion));
            projected_rows.push(row);
        }
        let job = ProfileDeletionJob {
            batch_id: batch,
            profile,
            profile_deletion,
            at_ms,
            retention_ms: record.retention_ms,
            targets: captured.clone(),
            phase: BatchPhase::Deleted,
        };
        // Targets before the Profile: a `workspaces` row is written only
        // while its owner is visible.
        let mut projections: Vec<RootProjection<'_>> = projected_rows
            .iter()
            .map(RootProjection::WorkspaceDeleted)
            .collect();
        projections.push(RootProjection::ProfileHidden { profile, at });
        self.catalog.publish_root(
            scope,
            &RootPublication {
                units,
                projections,
                state: vec![Self::job_state(&job)?],
            },
        )?;
        // Fence writers first (§6.3): the targets leave the open set, and the
        // Profile's identity/private documents are dropped from memory.
        for (workspace, _) in &captured {
            self.hint_manifest_deletion(*workspace, Some(at), at);
            self.open.remove(workspace);
        }
        self.open_profiles.remove(&profile);
        self.private_profiles.remove(&profile);
        tracing::info!(
            profile = %profile,
            batch = %batch,
            targets = captured.len(),
            "root lifecycle: profile deletion batch committed"
        );
        Ok(ProfileDeletionBatch {
            batch_id: batch,
            profile_deletion,
            targets: captured,
        })
    }

    /// Every restorable Profile deletion: a Profile unit with an active
    /// deletion that belongs to a batch this device holds the job for, and no
    /// purge evidence. `now` decides which targets are reported as past
    /// their retention.
    pub fn profile_deletions(&self, now: Timestamp) -> Result<Vec<DeletedProfile>> {
        let scope = self.catalog.local_user_scope()?;
        let mut out = Vec::new();
        for (unit, _) in self
            .catalog
            .root_catalog_units(scope, CatalogUnitKind::Profile)?
        {
            let Some(profile) = unit.profile() else {
                continue;
            };
            let Some(unit) = self.effective_profile_unit(profile)? else {
                continue;
            };
            if !unit.purged().is_empty() {
                continue;
            }
            let deletions = unit.deletions();
            for deletion in unit.active_deletions() {
                let Some(record) = deletions.get(&deletion) else {
                    continue;
                };
                let Some(batch) = record.batch_id.map(BatchId::from_uuid) else {
                    continue;
                };
                let Some(job) = self.read_job(batch)? else {
                    tracing::warn!(profile = %profile, batch = %batch,
                        "a Profile deletion names a batch this device holds no job for");
                    continue;
                };
                let mut expired_or_purged = Vec::new();
                for (workspace, target_deletion) in &job.targets {
                    let Some(target) = self.effective_workspace_unit(*workspace)? else {
                        expired_or_purged.push(*workspace);
                        continue;
                    };
                    let purged = !target.purged().is_empty();
                    let expired = target
                        .deletions()
                        .get(target_deletion)
                        .is_some_and(|d| expires_at(d) <= now.millis());
                    if purged || expired {
                        expired_or_purged.push(*workspace);
                    }
                }
                out.push(DeletedProfile {
                    profile,
                    deletion,
                    batch_id: batch,
                    at_ms: record.at_ms,
                    retention_ms: record.retention_ms,
                    targets: job.targets.iter().map(|(w, _)| *w).collect(),
                    expired_or_purged,
                });
            }
        }
        Ok(out)
    }

    /// **Batch restore** (§6.3): a restore fact on the Profile unit observing
    /// exactly `deletion`, and on each captured target a restore fact
    /// observing exactly the deletion this batch wrote there — only that id,
    /// so an earlier independent deletion stays in the Trash, a later unseen
    /// one still wins when it arrives, a target somebody already restored and
    /// moved keeps its placement, and a purged target is reported, never
    /// recreated. Projections and the job receipt commit in the same
    /// transaction.
    pub fn restore_profile(
        &mut self,
        profile: ProfileId,
        deletion: DeletionId,
    ) -> Result<ProfileRestoreOutcome> {
        self.refuse_holder(profile, "restored")?;
        let Some(unit) = self.effective_profile_unit(profile)? else {
            return Err(Error::bare(Code::ProfileNotFound));
        };
        if !unit.purged().is_empty() {
            return Err(Error::new(
                Code::VersionUnavailable,
                "this Profile deletion was purged; its content is gone",
            ));
        }
        if !unit.active_deletions().contains(&deletion) {
            return Err(Error::new(
                Code::InvalidRequest,
                "that deletion is not active on this Profile",
            ));
        }
        let record = unit
            .deletions()
            .get(&deletion)
            .copied()
            .ok_or_else(|| Error::bare(Code::CorruptState))?;
        let Some(batch) = record.batch_id.map(BatchId::from_uuid) else {
            return Err(Error::new(
                Code::InvalidRequest,
                "that deletion belongs to no batch this device can restore",
            ));
        };
        let mut job = self.read_job(batch)?.ok_or_else(|| {
            Error::new(
                Code::CorruptState,
                "the Profile deletion batch has no local application job",
            )
        })?;
        let at = Timestamp::now();
        let at_ms = at.millis();
        let restore = RestoreId::new();
        let scope = self.catalog.local_user_scope()?;

        let profile_record = RestoreRecord {
            at_ms,
            observed_deletions: vec![deletion],
            batch_id: Some(batch.as_uuid()),
        };
        let mut units = vec![self.plan_profile_write(profile, |unit| {
            Ok(unit.record_restore(restore, &profile_record)?)
        })?];
        let mut outcome = ProfileRestoreOutcome::default();
        // (row, documents) of every target that leaves the Trash.
        let mut restored_rows: Vec<(Workspace, Vec<DocumentId>)> = Vec::new();
        for (workspace, target_deletion) in &job.targets {
            let Some(target) = self.effective_workspace_unit(*workspace)? else {
                outcome.purged.push(*workspace);
                continue;
            };
            if !target.purged().is_empty() || self.catalog.workspace(*workspace)?.is_none() {
                outcome.purged.push(*workspace);
                continue;
            }
            let active = target.active_deletions();
            if !active.contains(target_deletion) {
                // Already restored by somebody. Where is it now?
                match self.root_placement(*workspace)? {
                    Some(placement) if placement.profile == Some(profile) => {
                        outcome.restored.push(*workspace)
                    }
                    _ => outcome.moved_elsewhere.push(*workspace),
                }
                continue;
            }
            let row = self.workspace_including_deleted(*workspace)?;
            let others_remain = active.len() > 1;
            let target_record = RestoreRecord {
                at_ms,
                observed_deletions: vec![*target_deletion],
                batch_id: Some(batch.as_uuid()),
            };
            let mut projected = row.clone();
            if !others_remain {
                projected.deleted_at = None;
            }
            units.push(self.plan_workspace_write(&projected, |unit| {
                Ok(unit.record_restore(RestoreId::new(), &target_record)?)
            })?);
            if others_remain {
                outcome.left_in_trash.push(*workspace);
            } else {
                self.ensure_open_including_deleted(*workspace)?;
                let documents = self.workspace_documents(*workspace)?;
                restored_rows.push((projected, documents));
                outcome.restored.push(*workspace);
            }
        }
        job.phase = BatchPhase::Restored { restore, at_ms };
        // The Profile first: its targets' rows need a visible owner.
        let mut projections = vec![RootProjection::ProfileVisible { profile }];
        projections.extend(restored_rows.iter().map(|(workspace, documents)| {
            RootProjection::WorkspaceRestored {
                workspace,
                documents,
            }
        }));
        self.catalog.publish_root(
            scope,
            &RootPublication {
                units,
                projections,
                state: vec![Self::job_state(&job)?],
            },
        )?;
        for (row, _) in &restored_rows {
            self.hint_manifest_deletion(row.id, None, at);
        }
        tracing::info!(
            profile = %profile,
            batch = %batch,
            restored = outcome.restored.len(),
            left_in_trash = outcome.left_in_trash.len(),
            moved_elsewhere = outcome.moved_elsewhere.len(),
            purged = outcome.purged.len(),
            "root lifecycle: profile deletion batch restored"
        );
        Ok(outcome)
    }

    // ----------------------------------------------------------- purge

    /// Permanently remove the content of every Workspace whose active
    /// deletion has passed its retention at `before` (`at_ms + retention_ms
    /// <= before`), per Workspace and in this order, each step restartable
    /// on the next pass: (1) the purge fact on the unit, committed first so
    /// the intent survives a crash; (2) open readers evicted; (3) this
    /// scope's personal row and due rows retired — the only authorized path
    /// around the `RESTRICT` key, and only for this scope and this Workspace;
    /// (4) backend bytes; (5) the `workspaces` row and its derived rows. The
    /// unit row stays with its evidence (§6.4). A personal unit under another
    /// scope holds only its own Workspace; a failed step leaves that Workspace
    /// pending and the pass continues with the others.
    ///
    /// Then every Profile deletion batch whose targets are all purged and
    /// whose own retention has passed gains purge evidence on the Profile
    /// unit. The `profiles` row and the identity/private units are retained:
    /// their physical removal has `RESTRICT` keys of its own and is outside
    /// this plan; `Store::profiles_removed_before` keeps naming the row.
    ///
    /// The Store's start-up calls this with the current time.
    pub fn purge_deleted(&mut self, before: Timestamp) -> Result<PurgeOutcome> {
        let scope = self.catalog.local_user_scope()?;
        let rows: BTreeMap<WorkspaceId, Workspace> = self
            .catalog
            .workspaces_including_deleted()?
            .into_iter()
            .map(|row| (row.id, row))
            .collect();
        let mut outcome = PurgeOutcome::default();
        for (unit_ref, _) in self
            .catalog
            .root_catalog_units(scope, CatalogUnitKind::Workspace)?
        {
            let Some(workspace) = unit_ref.workspace() else {
                continue;
            };
            // Step 5 done: nothing of the content remains to purge.
            let Some(row) = rows.get(&workspace) else {
                continue;
            };
            let Some(unit) = self.effective_workspace_unit(workspace)? else {
                continue;
            };
            let deletions = unit.deletions();
            let expired: Vec<DeletionId> = unit
                .active_deletions()
                .into_iter()
                .filter(|id| {
                    deletions
                        .get(id)
                        .is_some_and(|d| expires_at(d) <= before.millis())
                })
                .collect();
            let already_purged = !unit.purged().is_empty();
            if expired.is_empty() && !already_purged {
                continue;
            }
            match self.purge_one(scope, row, &expired, already_purged, before) {
                Ok(None) => outcome.purged.push(workspace),
                Ok(Some(hold)) => outcome.held.push((workspace, hold)),
                Err(error) => {
                    tracing::warn!(workspace = %workspace, code = %error.code(),
                        "workspace purge step failed; pending until the next pass");
                    outcome.pending.push((workspace, error.code()));
                }
            }
        }
        outcome.profiles_purged = self.purge_expired_profiles(scope, before)?;
        Ok(outcome)
    }

    /// Steps 1–5 for one Workspace. `Ok(None)` purged, `Ok(Some(hold))`
    /// held at step 3, `Err` at the step that failed.
    fn purge_one(
        &mut self,
        scope: UserScopeId,
        row: &Workspace,
        expired: &[DeletionId],
        already_purged: bool,
        at: Timestamp,
    ) -> Result<Option<PurgeHold>> {
        let workspace = row.id;
        // (1) Intent first. A restart finds the evidence and resumes from
        // step 2 without writing it twice.
        if !already_purged {
            let at_ms = at.millis();
            let write = self.plan_workspace_write(row, |unit| {
                for id in expired {
                    unit.record_purge(*id, &PurgeRecord { at_ms })?;
                }
                Ok(())
            })?;
            self.catalog.publish_root(
                scope,
                &RootPublication {
                    units: vec![write],
                    projections: Vec::new(),
                    state: Vec::new(),
                },
            )?;
            tracing::info!(workspace = %workspace, "root lifecycle: purge evidence recorded");
        }
        // (2) No reader keeps the bytes about to go.
        self.open.remove(&workspace);
        // (3) This scope's personal unit, and only it. A foreign unit holds.
        if let Some(hold) = self.catalog.personal_unit_hold(scope, workspace)? {
            tracing::info!(workspace = %workspace, hold = ?hold, "workspace purge held");
            return Ok(Some(hold));
        }
        if self
            .catalog
            .retire_workspace_personal_unit(scope, workspace)?
        {
            tracing::info!(workspace = %workspace, "personal unit retired for purge");
        }
        // (4) Bytes, then (5) the row. The unit row stays.
        self.backend.delete_workspace(workspace)?;
        self.catalog.purge_workspace(workspace)?;
        tracing::info!(workspace = %workspace, "root lifecycle: workspace purged");
        Ok(None)
    }

    /// Purge evidence on every Profile unit whose batch deletion has passed
    /// its retention and whose every captured target is purged (or never had
    /// content to purge). The job moves to `Purged`; rows are retained.
    fn purge_expired_profiles(
        &mut self,
        scope: UserScopeId,
        before: Timestamp,
    ) -> Result<Vec<ProfileId>> {
        let mut purged = Vec::new();
        for (unit_ref, _) in self
            .catalog
            .root_catalog_units(scope, CatalogUnitKind::Profile)?
        {
            let Some(profile) = unit_ref.profile() else {
                continue;
            };
            let Some(unit) = self.effective_profile_unit(profile)? else {
                continue;
            };
            if !unit.purged().is_empty() {
                continue;
            }
            let deletions = unit.deletions();
            for deletion in unit.active_deletions() {
                let Some(record) = deletions.get(&deletion) else {
                    continue;
                };
                if expires_at(record) > before.millis() {
                    continue;
                }
                let Some(batch) = record.batch_id.map(BatchId::from_uuid) else {
                    continue;
                };
                let Some(mut job) = self.read_job(batch)? else {
                    continue;
                };
                let mut all_purged = true;
                for (workspace, _) in &job.targets {
                    let target = self.effective_workspace_unit(*workspace)?;
                    let content_gone = self.catalog.workspace(*workspace)?.is_none();
                    // No unit at all counts as purged: nothing can resurrect
                    // what has no row and no unit.
                    let evidenced = target.as_ref().is_none_or(|unit| !unit.purged().is_empty());
                    if !(content_gone && evidenced) {
                        all_purged = false;
                        break;
                    }
                }
                if !all_purged {
                    continue;
                }
                let at_ms = before.millis();
                job.phase = BatchPhase::Purged { at_ms };
                let write = self.plan_profile_write(profile, |unit| {
                    Ok(unit.record_purge(deletion, &PurgeRecord { at_ms })?)
                })?;
                self.catalog.publish_root(
                    scope,
                    &RootPublication {
                        units: vec![write],
                        projections: Vec::new(),
                        state: vec![Self::job_state(&job)?],
                    },
                )?;
                tracing::info!(profile = %profile, batch = %batch,
                    "root lifecycle: profile deletion purged; rows retained");
                purged.push(profile);
                break;
            }
        }
        Ok(purged)
    }

    // ------------------------------------------------------ unassigned

    /// Live Workspaces reachable from no active Profile (§3.6, §6.2 table,
    /// §6.3): placed under nobody, under a deleted Profile, under a Profile
    /// this catalog has no row for, or under a Holder. A Workspace with no
    /// unit yet is not listed — its placement is pending, not unassigned.
    pub fn unassigned_workspaces(&self) -> Result<Vec<UnassignedWorkspace>> {
        let mut out = Vec::new();
        for row in self.catalog.workspaces_including_deleted()? {
            if self.effective_deleted_at(row.id, row.deleted_at)?.is_some() {
                continue;
            }
            let Some(placement) = self.root_placement_raw(row.id)? else {
                continue;
            };
            let reason = match placement.profile {
                None => UnassignedReason::ExplicitlyUnassigned,
                Some(profile) => match self.catalog.profile_including_removed(profile)? {
                    None => UnassignedReason::ProfileAbsent,
                    Some(p) if p.is_holder() => UnassignedReason::HolderPlaced,
                    Some(_) if self.profile_is_deleted(profile)? => {
                        UnassignedReason::ProfileDeleted
                    }
                    Some(_) => continue,
                },
            };
            out.push(UnassignedWorkspace {
                workspace: row,
                former_profile: placement.profile,
                reason,
            });
        }
        Ok(out)
    }
}

/// When a deletion's content leaves the Trash: `at_ms + retention_ms`.
pub fn expires_at(record: &DeletionRecord) -> i64 {
    record.at_ms.saturating_add(record.retention_ms)
}

/// The earliest active deletion's time, or `None` for a live unit.
pub(super) fn earliest_active_deletion(unit: &WorkspaceCatalogUnit) -> Option<Timestamp> {
    let deletions = unit.deletions();
    unit.active_deletions()
        .into_iter()
        .filter_map(|id| deletions.get(&id).map(|d| d.at_ms))
        .min()
        .map(Timestamp::from_millis)
}

#[cfg(test)]
mod tests;
