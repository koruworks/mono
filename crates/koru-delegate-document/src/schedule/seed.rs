// SPDX-License-Identifier: MPL-2.0
//! Retained Routine creation evidence belongs to the Schedule document owner.
use super::*;
use koru_delegate_schedule::SchedulePauseReason;

const SEED_LIMIT: usize = 16 * 1024 * 1024;

fn invalid() -> ScheduleDocumentError {
    ScheduleDocumentError::Malformed("routine_creation_seed")
}

fn expected(header: &ScheduleHeader) -> Result<()> {
    if header
        .definition
        .routine
        .as_ref()
        .is_none_or(|a| !a.valid() || a.owner.device != header.initial_owner)
        || header.definition.revision != 1
        || header.definition.effective_at_ms != header.created_at_ms
        || header.definition.lifecycle
            != (ScheduleLifecycle::Paused {
                reason: SchedulePauseReason::Manual,
            })
    {
        return Err(invalid());
    }
    Ok(())
}

impl ScheduleDocument {
    /// Build the actual Schedule2 empty seed as an update from the empty VV.
    /// Random CRDT peer bytes are not the semantic creation identity.
    pub fn routine_seed_update(header: &ScheduleHeader, name: &ScheduleName) -> Result<Vec<u8>> {
        expected(header)?;
        let doc = Self::create_named(header, name)?;
        let update = doc.export_updates_since(&VersionVector::default())?;
        if update.len() > SEED_LIMIT {
            return Err(invalid());
        }
        Ok(update)
    }

    /// Validate the retained original update, never a newly generated peer's
    /// bytes. The returned document owns the original semantic VV. Callers
    /// compare it with the live VV and retain its first encoding as evidence.
    /// The encoded cap is checked before import; it is not a Loro heap bound.
    pub fn verify_routine_seed(
        update: &[u8],
        header: &ScheduleHeader,
        name: &ScheduleName,
    ) -> Result<Self> {
        expected(header)?;
        if update.is_empty() || update.len() > SEED_LIMIT {
            return Err(invalid());
        }
        let raw = LoroDoc::new();
        let imported = raw.import(update)?;
        if imported.pending.is_some() || raw.oplog_vv() != raw.state_vv() {
            return Err(invalid());
        }
        let doc = Self::adopt(raw)?;
        if doc.header()? != *header
            || doc.name()? != *name
            || !doc.handoffs()?.is_empty()
            || !doc.firings()?.is_empty()
            || doc.root().get(K_RETIRED).is_some()
        {
            return Err(invalid());
        }
        // The complete owner shape rejects unknown roots/keys, legacy schema,
        // missing explicit name and otherwise hidden seed data.
        let canonical = Self::create_named(header, name)?;
        if doc.doc.get_deep_value() != canonical.doc.get_deep_value() {
            return Err(invalid());
        }
        let revision = doc.version().encode();
        if revision.is_empty() || revision.len() > 65536 {
            return Err(invalid());
        }
        Ok(doc)
    }
}

#[cfg(test)]
pub(super) mod tests;
