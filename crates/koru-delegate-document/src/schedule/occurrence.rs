// SPDX-License-Identifier: MPL-2.0
//! Current Routine definition proof, separate from immutable seed validation.
use super::*;

impl ScheduleDocument {
    /// Bounded encoded import with complete ancestry. Physical Store location
    /// and current human/Profile authority remain the adapter's responsibility.
    pub fn verify_routine_occurrence_snapshot(
        snapshot: &[u8],
        schedule: ScheduleId,
        definition: &ScheduleDefinition,
        owner: DeviceId,
        epoch: Option<HandoffId>,
    ) -> Result<Self> {
        let invalid = || ScheduleDocumentError::Malformed("routine_occurrence_definition");
        if snapshot.is_empty() || snapshot.len() > 16 * 1024 * 1024 {
            return Err(invalid());
        }
        let raw = LoroDoc::new();
        let imported = raw.import(snapshot)?;
        if imported.pending.is_some() || raw.oplog_vv() != raw.state_vv() {
            return Err(invalid());
        }
        let doc = Self::adopt(raw)?;
        let header = doc.header()?;
        let handoff = doc.handoff_state()?;
        if !doc.is_writable()
            || header.schedule != schedule
            || header.definition != *definition
            || definition
                .routine
                .as_ref()
                .is_none_or(|a| !a.valid() || a.owner.device != owner)
            || definition.lifecycle != ScheduleLifecycle::Active
            || handoff.owner != owner
            || handoff.epoch != epoch
            || !handoff.pending.is_empty()
            || doc.root().get(K_RETIRED).is_some()
        {
            return Err(invalid());
        }
        // Decode retained collections as well; malformed history cannot prove
        // a coherent current Schedule. Prior valid firings remain ordinary data.
        doc.firings()?;
        Ok(doc)
    }
}

#[cfg(test)]
mod tests;
