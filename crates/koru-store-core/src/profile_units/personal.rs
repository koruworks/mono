// SPDX-License-Identifier: MPL-2.0

//! One person's reminders and personal attributes for one Workspace.
//!
//! Two addresses, one content shape. A v1 unit is `(owner Profile,
//! Workspace)` and stays strict against that pair, because a legacy source is
//! validated by its own contract before it is migrated. A v2 unit is the
//! Workspace alone: the owning user scope is the SQL row key the caller
//! resolved from its validated binding, and it is never written into the
//! document, so a Profile can be removed without hiding the data that
//! followed its Workspace elsewhere.

use super::content::ProfileContent;
use super::*;
use crate::ids::{ActionId, ReminderId};
use crate::personal_attributes::{EntityRef, PersonalAttributeValue, PersonalAttributes};
use crate::reminders::{OccurrenceAction, ReminderReductionInput, ReminderRule, StoredReminder};
use std::collections::{BTreeMap, BTreeSet};

pub struct PersonalManifest {
    pub(super) inner: UnitDoc<PersonalScope>,
}
impl std::fmt::Debug for PersonalManifest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonalManifest")
            .field("scope", &self.inner.unit)
            .field("version", &self.version())
            .finish()
    }
}
impl PersonalManifest {
    /// A fresh v1 unit. Legacy storage only; new personal units are v2.
    pub fn create(owner: ProfileId, workspace: WorkspaceId) -> Result<Self> {
        let unit = ProfileUnitRef::Personal { owner, workspace };
        Ok(Self {
            inner: UnitDoc::fresh(PersonalScope::Legacy { owner, workspace }, |doc| {
                unit.write_header(doc)
            })?,
        })
    }

    /// Load v1 bytes, strictly against the `(owner, workspace)` their header
    /// names. A v2 document has no owner in its header and is refused here.
    pub fn from_snapshot(bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            inner: UnitDoc::load_with(bytes, |doc| {
                let meta = doc.get_map("meta");
                let owner = string(&meta, "owner_profile_id")
                    .or_else(|| string(&meta, "profile_id"))
                    .and_then(|id| ProfileId::parse(&id))
                    .ok_or_else(|| corrupt("unit has no Profile scope"))?;
                let workspace = string(&meta, "workspace_id")
                    .and_then(|id| WorkspaceId::parse(&id))
                    .ok_or_else(scope_mismatch)?;
                Ok(PersonalScope::Legacy { owner, workspace })
            })?,
        })
    }

    /// A fresh v2 unit written by `writer_peer` from its first operation.
    /// The header carries the format and the Workspace, nothing else: no
    /// owner, no scope, no transport choice.
    pub fn create_v2(workspace: WorkspaceId, writer_peer: u64) -> Result<Self> {
        let unit = PersonalUnitRef { workspace };
        Ok(Self {
            inner: UnitDoc::fresh(PersonalScope::Workspace(unit), |doc| {
                doc.set_peer_id(writer_peer).map_err(loro_failed)?;
                let meta = doc.get_map("meta");
                meta.insert("schema_version", PERSONAL_V2_CRDT_SCHEMA_VERSION)
                    .map_err(loro_failed)?;
                meta.insert("workspace_id", workspace.to_storage())
                    .map_err(loro_failed)?;
                Ok(())
            })?,
        })
    }

    /// Load v2 bytes for the Workspace the caller resolved under its user
    /// scope. A v1 document is refused: its scope was a Profile pair, and the
    /// migration — not a relabel — is what moves it to this address.
    pub fn from_snapshot_v2(bytes: &[u8], expected_workspace: WorkspaceId) -> Result<Self> {
        Ok(Self {
            inner: UnitDoc::load(
                bytes,
                PersonalScope::Workspace(PersonalUnitRef {
                    workspace: expected_workspace,
                }),
            )?,
        })
    }

    pub fn scope(&self) -> PersonalScope {
        self.inner.unit
    }
    /// The v1 wire address; `None` for a v2 unit, which has no owner.
    pub fn unit(&self) -> Option<ProfileUnitRef> {
        self.inner.unit.legacy_unit()
    }
    /// The v1 owner; `None` for a v2 unit. Never read `meta.owner_profile_id`
    /// for this: on a v2 document that key is history, not ownership.
    pub fn owner(&self) -> Option<ProfileId> {
        match self.inner.unit {
            PersonalScope::Legacy { owner, .. } => Some(owner),
            PersonalScope::Workspace(_) => None,
        }
    }
    pub fn workspace(&self) -> WorkspaceId {
        self.inner.unit.workspace()
    }
    /// The peer id every new operation of this replica is written under. For
    /// a v2 unit this is the persisted per-unit writer identity
    /// (`workspace_personal_crdts.writer_peer_id`), never the Store's global
    /// stream (`docs/features/root-catalog/TECHNICAL.final.md` §5.2 step 6).
    pub fn writer_peer(&self) -> u64 {
        self.inner.doc.peer_id()
    }
    /// The due-index hints of every recognized rule: the next scheduled or
    /// snoozed instant, or `None` where only the authoritative reducer can
    /// say. One computation for the v1 index, the v2 index and the migration.
    pub(crate) fn reminder_due_hints(&self) -> Vec<(ReminderId, Option<i64>)> {
        use crate::reminders::{EffectiveState, OccurrenceMoment, TargetState};
        self.reminder_reduction_inputs()
            .into_iter()
            .map(|row| {
                let due_at_ms = match row.reduce(TargetState::Active, None).effective {
                    EffectiveState::Scheduled {
                        moment: OccurrenceMoment::Zoned { value },
                        ..
                    } => Some(value.instant_ms()),
                    EffectiveState::Snoozed { until_ms, .. } => Some(until_ms),
                    _ => None,
                };
                (row.stored().rule.id, due_at_ms)
            })
            .collect()
    }
    fn check_target(&self, target: &EntityRef) -> Result<()> {
        if target.workspace() == self.workspace() {
            Ok(())
        } else {
            Err(scope_mismatch())
        }
    }
    unit_methods!();
    fn content(&self) -> ProfileContent<'_> {
        ProfileContent {
            doc: &self.inner.doc,
        }
    }

    pub fn set_personal_attribute(
        &self,
        entity: &EntityRef,
        key: &str,
        value: &PersonalAttributeValue,
    ) -> Result<()> {
        self.inner.require_writable()?;
        self.check_target(entity)?;
        self.content().set_personal_attribute(entity, key, value)
    }

    pub fn clear_personal_attribute(&self, entity: &EntityRef, key: &str) -> Result<bool> {
        self.inner.require_writable()?;
        self.check_target(entity)?;
        self.content().clear_personal_attribute(entity, key)
    }

    pub fn personal_attributes(&self, entity: &EntityRef) -> PersonalAttributes {
        if self.check_target(entity).is_err() || self.is_read_only() {
            return Default::default();
        }
        self.content().personal_attributes(entity)
    }

    pub fn put_reminder_rule(&self, rule: &ReminderRule) -> Result<()> {
        self.inner.require_writable()?;
        self.check_target(&rule.target)?;
        self.content().put_reminder_rule(rule)
    }

    pub fn delete_reminder(&self, id: ReminderId) -> Result<()> {
        self.inner.require_writable()?;
        self.content().delete_reminder(id)
    }

    pub fn delete_reminders_for_target(&self, target: &EntityRef) -> Result<Vec<ReminderId>> {
        self.inner.require_writable()?;
        self.check_target(target)?;
        self.content().delete_reminders_for_target(target)
    }

    pub fn append_reminder_action(
        &self,
        reminder: ReminderId,
        action: &OccurrenceAction,
    ) -> Result<()> {
        self.inner.require_writable()?;
        self.content().append_reminder_action(reminder, action)
    }

    pub fn clear_handled_reminder_history(&self) -> Result<usize> {
        self.inner.require_writable()?;
        self.content().clear_handled_reminder_history()
    }

    pub fn clear_handled_reminder_history_for_target(&self, target: &EntityRef) -> Result<usize> {
        self.inner.require_writable()?;
        self.check_target(target)?;
        self.content()
            .clear_handled_reminder_history_for_target(target)
    }

    pub fn reminder(&self, id: ReminderId) -> Option<StoredReminder> {
        if self.is_read_only() {
            return None;
        }
        self.content().reminder(id)
    }

    pub fn raw_reminder_action(
        &self,
        reminder: ReminderId,
        action: ActionId,
    ) -> Option<OccurrenceAction> {
        if self.is_read_only() {
            return None;
        }
        self.content().raw_reminder_action(reminder, action)
    }

    pub(crate) fn raw_reminder_actions(
        &self,
    ) -> BTreeMap<(ReminderId, ActionId), OccurrenceAction> {
        if self.is_read_only() {
            return BTreeMap::new();
        }
        self.content().raw_reminder_actions()
    }

    pub(crate) fn cleared_reminder_actions(&self) -> BTreeSet<(ReminderId, ActionId)> {
        if self.is_read_only() {
            return BTreeSet::new();
        }
        self.content().cleared_reminder_actions()
    }

    pub fn reminders(&self) -> Vec<StoredReminder> {
        if self.is_read_only() {
            return Vec::new();
        }
        self.content().reminders()
    }

    pub(crate) fn reminder_reduction_inputs(&self) -> Vec<ReminderReductionInput> {
        if self.is_read_only() {
            return Vec::new();
        }
        self.content().reminder_reduction_inputs()
    }
}
