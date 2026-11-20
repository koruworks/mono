// SPDX-License-Identifier: MPL-2.0

//! Restore a Workspace **into a Profile**: out of the Trash and under the
//! Profile named, as one root operation
//! (`docs/features/trash-recovery/PLAN.final.md` P1).
//!
//! [`Store::restore_workspace`] refuses when the Workspace's own Profile is
//! removed or deleted, and [`Store::move_workspace`] refuses anything in the
//! Trash, so a Workspace trashed with a Profile that is now gone had no way
//! back short of restoring that Profile. It also could not be done as the
//! two existing operations in a row, in either order:
//!
//! - **A move first** would re-key the Workspace under a syncing Profile
//!   while its unit still says deleted, and the next pass would publish that
//!   deletion — the account would be told the Workspace is gone.
//! - **A restore first** has no Profile to restore it under.
//!
//! So both facts go into one unit write and one catalog transaction. The
//! found case: a device seeded a Workspace as *under a Profile since
//! deleted, in the Trash* while the account held it live under another
//! Profile (2026-11-17). A placement without the restore is the hazard; see
//! root-catalog TECHNICAL.final §6.2, *Delete versus move*.

use super::account_live::account_live_key;
use super::root_move::plan_unit_write;
use super::*;
use crate::ids::{DeletionId, RestoreId};
use crate::ports::{RootProjection, RootPublication};
use crate::root_catalog::{CatalogUnitRef, RestoreRecord, WorkspaceCatalogUnit};
use crate::transport::{TransportModeState, TransportSubject};

impl<C: Catalog, B: Backend> Store<C, B> {
    /// Restore `id` and file it under `to`.
    ///
    /// **The destination is checked as a move checks it**: absent, removed,
    /// being removed or a Holder refuses. The Profile the Workspace is filed
    /// under now is not asked about at all — being gone is the case this is
    /// for — except that a removal in progress on it still refuses, as it
    /// does everywhere.
    ///
    /// **The mode is decided before the write is planned.** A mode row
    /// nobody chose (`explicit_choice` unset) takes the destination
    /// Profile's; a choice somebody made is kept. The unit write is planned
    /// under that mode and the destination's ceiling, so a Workspace coming
    /// back into a syncing Profile is written to the durable unit, any
    /// private overlay folded in. Planned under the row as it stands — a
    /// peer-to-peer default inherited from the Profile it left — the restore
    /// could land in the overlay with the durable unit still saying deleted.
    ///
    /// **One unit write** carries the restore, observing every active
    /// deletion this device sees, and the placement under a new move id.
    /// **One publication** carries that unit, the row (owner, position,
    /// `deleted_at` cleared, documents routed again), the mode, and the
    /// removal of the account-live fact.
    ///
    /// A Workspace that is not in the Trash is simply moved.
    pub fn restore_workspace_to(&mut self, id: WorkspaceId, to: ProfileId) -> Result<Workspace> {
        let row = self.workspace_including_deleted(id)?;
        self.refuse_profile_removal(row.owner)?;
        self.refuse_profile_removal(to)?;
        if self.catalog.profile(to)?.is_none() {
            return Err(Error::bare(Code::ProfileNotFound));
        }
        // §6.3: a Holder is legacy inventory, never a destination.
        self.refuse_holder(to, "a restore destination")?;

        let held = self.effective_workspace_unit(id)?;
        if held.as_ref().is_some_and(|unit| !unit.purged().is_empty()) {
            // Purge evidence is terminal (§6.4): the content is gone, and a
            // restore that made the row live again would be an empty
            // replacement wearing the old name.
            return Err(Error::new(
                Code::WorkspaceNotFound,
                "that workspace's content was removed; it can no longer be restored",
            ));
        }
        let active: Vec<DeletionId> = held
            .map(|unit| unit.active_deletions().into_iter().collect())
            .unwrap_or_default();
        if active.is_empty() && row.deleted_at.is_none() {
            return self.move_workspace(id, to);
        }

        let subject = TransportSubject::Workspace(id);
        let stored = self.catalog.transport_mode(subject)?;
        let inherited = (!stored.explicit_choice).then_some(
            self.catalog
                .transport_mode(TransportSubject::Profile(to))?
                .mode,
        );
        let admitted = self
            .admission_of(
                TransportModeState {
                    mode: inherited.unwrap_or(stored.mode),
                    ..stored
                },
                to,
            )?
            .allows_peer();

        let at = Timestamp::now();
        let scope = self.catalog.local_user_scope()?;
        let unit = CatalogUnitRef::Workspace { workspace: id };
        let durable = self.catalog.root_catalog_unit(scope, unit)?;
        let overlay = self.catalog.root_catalog_overlay(scope, unit)?;
        let record = RestoreRecord {
            at_ms: at.millis(),
            observed_deletions: active,
            batch_id: None,
        };
        let move_id = uuid::Uuid::new_v4();
        let write = plan_unit_write::<WorkspaceCatalogUnit>(
            unit,
            durable.as_ref(),
            overlay.as_ref(),
            admitted,
            self.peer_id,
            |candidate| {
                // A unit-less row has no deletion fact to observe: its
                // `deleted_at` is the legacy projection's, cleared below.
                if !record.observed_deletions.is_empty() {
                    candidate.record_restore(RestoreId::new(), &record)?;
                }
                Ok(candidate.set_placement(Some(to), move_id)?)
            },
        )?;

        self.ensure_open_including_deleted(id)?;
        let documents = self.workspace_documents(id)?;
        let mut projected = row;
        projected.owner = to;
        projected.position = self.next_workspace_position(to)?;
        projected.deleted_at = None;
        let mut projections = vec![RootProjection::WorkspaceRestored {
            workspace: &projected,
            documents: &documents,
        }];
        if let Some(mode) = inherited {
            projections.push(RootProjection::InheritedMode {
                workspace: id,
                mode,
            });
        }
        self.catalog.publish_root(
            scope,
            &RootPublication {
                units: vec![(unit, write)],
                projections,
                state: vec![(account_live_key(id), None)],
            },
        )?;
        // The owner and perhaps the mode moved: every transport callback
        // holding an older answer asks again, as after a move.
        self.transport.invalidate();
        self.hint_manifest_deletion(id, None, at);
        tracing::info!(
            workspace = %id,
            inherited_mode = inherited.is_some(),
            export_admitted = admitted,
            "root lifecycle: workspace restored into a profile"
        );
        Ok(projected)
    }
}
