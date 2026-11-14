// SPDX-License-Identifier: MPL-2.0
//! Lookup-only preclaim inputs. No current execution configuration or authority grant.
use super::*;

impl Repository {
    /// Read the complete retained draft and creation through their sole decoders,
    /// with bounded current owner/home/policy metadata instead of the ordinary
    /// readers' execution-field guard. Currentness ends with this writer call:
    /// the worker must compare the entire result after each external phase.
    /// Missing or non-Prepared drafts are ineligible; corrupt evidence is an error.
    pub fn routine_preclaim_snapshot(
        &self,
        slot: &RoutineSlot,
        semantic_hash: RoutineDigest32,
    ) -> Result<
        Option<(
            BotRoutineStoredPreparation,
            BotRoutineCreation,
            BotPolicyMetadata,
        )>,
        BotError,
    > {
        if !slot.valid() {
            return Err(BotError::Invalid);
        }
        let owner = owner(slot);
        let home = self.routine_occurrence_home(owner, slot.bot)?;
        let Some(row) = rows::read(&self.connection, slot)? else {
            return Ok(None);
        };
        if row.prepared.semantic_hash != semantic_hash {
            return Err(BotError::Conflict);
        }
        if row.stage != BotRoutineStoredStage::Prepared {
            return Ok(None);
        }
        let metadata = self.bot_policy_metadata(owner, slot.bot)?;
        let creation = bounded::by_schedule(&self.connection, owner, slot.bot, slot.schedule)?
            .ok_or(BotError::NotFound)?;
        if creation.state != BotRoutineCreationState::Bound {
            return Err(BotError::Conflict);
        }
        if metadata.home_workspace != home
            || creation.identity.home != home
            || row.capture.canonical.home != home
        {
            return Err(BotError::Forbidden);
        }
        // Eligibility beyond visibility, both hashes, exact captured revisions,
        // route/grant equality and current Store Profile belong to the worker.
        Ok(Some((row, creation, metadata)))
    }
}
