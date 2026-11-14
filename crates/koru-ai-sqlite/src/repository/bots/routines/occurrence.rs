// SPDX-License-Identifier: MPL-2.0
//! Private draft/link transactions. No Store, IPC, provider or ordinary Submit.
use super::*;
use koru_ai_core::bots::BotRoutineDraftReceipt as DraftReceipt;
use koru_delegate_core::{
    RoutineClaimObservation, RoutineClaimRecord, RoutineDefinitionIdentity, RoutineExecutionIds,
    RoutineLinkState, RoutinePrepared, RoutineSlot, RoutineTerminalReason,
};
mod finalize;
mod recovery;
mod rows;
#[cfg(test)]
mod tests;

fn owner(slot: &RoutineSlot) -> BotOwner {
    BotOwner {
        user_scope: slot.owner.user_scope,
        device: slot.owner.device,
    }
}

fn close_name(reason: BotRoutineCloseReason) -> &'static str {
    match reason {
        BotRoutineCloseReason::ExpiredUnclaimed => "expired_unclaimed",
        BotRoutineCloseReason::SlotSkipped => "slot_skipped",
        BotRoutineCloseReason::OtherWinner => "other_winner",
        BotRoutineCloseReason::ConfigurationChanged => "configuration_changed",
        BotRoutineCloseReason::AuthorityChanged => "authority_changed",
        BotRoutineCloseReason::Stopped => "stopped",
    }
}

fn has_capacity(conn: &Connection, slot: &RoutineSlot) -> Result<bool, BotError> {
    // Existing partial index: exact owner/device prefix, then Bot filter. This
    // bounds returned/count state, not rows inspected across this owner's Bots.
    let count: u32 = conn.query_row("SELECT COUNT(*) FROM (SELECT 1 FROM bot_routine_preparations INDEXED BY bot_routine_preparation_recovery WHERE user_scope_id=?1 AND owner_device_id=?2 AND bot_id=?3 AND stage='prepared' LIMIT 51)",
        params![slot.owner.user_scope.to_storage(),slot.owner.device.to_storage(),slot.bot.to_storage()], |r| r.get(0)).map_err(bad)?;
    Ok(count < 50)
}

fn canonical(
    conn: &Connection,
    slot: &RoutineSlot,
) -> Result<(BotRoutineCanonical, ProfileId), BotError> {
    settled(conn, owner(slot), slot.bot)?;
    let source = bounded::source(conn, owner(slot), slot.bot)?;
    let (name,role_workspace,role_document,mode,fast,paused):(String,String,String,Option<String>,Option<bool>,bool) = conn.query_row("SELECT
        CASE WHEN typeof(name)='text' AND length(CAST(name AS BLOB)) BETWEEN 1 AND 512 THEN name END,
        CASE WHEN typeof(role_workspace_id)='text' AND length(CAST(role_workspace_id AS BLOB))=36 THEN role_workspace_id END,
        CASE WHEN typeof(role_document_id)='text' AND length(CAST(role_document_id AS BLOB))=36 THEN role_document_id END,
        CASE WHEN mode IS NULL OR (typeof(mode)='text' AND length(CAST(mode AS BLOB))<=128) THEN mode ELSE 1 END,
        CASE WHEN fast IS NULL OR (typeof(fast)='integer' AND fast IN (0,1)) THEN fast ELSE 'invalid' END,
        CASE WHEN typeof(paused)='integer' AND paused IN (0,1) THEN paused END
        FROM bots WHERE bot_id=?1", [slot.bot.to_storage()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).map_err(bad)?;
    if paused {
        return Err(BotError::Settling);
    }
    let canonical = BotRoutineCanonical {
        home: source.home,
        config_revision: source.revision,
        policy_revision: source.policy_revision,
        config: BotConfig {
            name,
            role: BotPage {
                workspace: WorkspaceId::parse(&role_workspace).ok_or(BotError::Storage)?,
                document: DocumentId::parse(&role_document).ok_or(BotError::Storage)?,
            },
            working_folder: source.folder,
            model: source.model,
            mode,
            fast,
        },
    };
    canonical.validate().map_err(|_| BotError::Storage)?;
    Ok((canonical, source.profile))
}

fn fresh(conn: &Connection, input: &BotRoutineCapturedSources) -> Result<(), BotError> {
    let (current, profile) = canonical(conn, &input.slot)?;
    if current != input.canonical || profile != input.profile {
        return Err(BotError::RevisionConflict);
    }
    let creation = bounded::by_schedule(
        conn,
        owner(&input.slot),
        input.slot.bot,
        input.slot.schedule,
    )?
    .ok_or(BotError::NotFound)?;
    if creation.state != BotRoutineCreationState::Bound
        || creation.identity.route() != input.creation
        || creation.identity.grant != input.grant
    {
        return Err(BotError::Conflict);
    }
    input.grant.validate(current.home)?;
    if input.grant.canonical_digest()? != input.grant.digest {
        return Err(BotError::Storage);
    }
    if library::knowledge_on(conn, input.slot.bot, current.home)?
        != input
            .knowledge
            .iter()
            .map(|p| p.page.clone())
            .collect::<Vec<_>>()
    {
        return Err(BotError::Conflict);
    }
    let binding = library::bound_memory_on(conn, owner(&input.slot), input.slot.bot, current.home)?;
    match (&input.memory, binding) {
        (BotCapturedMemory::Absent {}, None) => {}
        (
            BotCapturedMemory::Captured {
                page,
                creation_request,
                ..
            },
            Some(binding),
        ) if *page == binding.page && *creation_request == binding.creation_request => {}
        _ => return Err(BotError::Conflict),
    }
    Ok(())
}

impl Repository {
    /// Current owner/lifecycle and immutable home only. Replay authorization
    /// must not decode current model, settings, Library content or captures.
    pub fn routine_occurrence_home(
        &self,
        owner: BotOwner,
        bot: BotId,
    ) -> Result<WorkspaceId, BotError> {
        let raw: Option<(String, String, String, String)> = self.connection.query_row(
            "SELECT CASE WHEN typeof(user_scope_id)='text' AND length(CAST(user_scope_id AS BLOB))=36 THEN user_scope_id END,
             CASE WHEN typeof(owner_device_id)='text' AND length(CAST(owner_device_id AS BLOB))=36 THEN owner_device_id END,
             CASE WHEN typeof(home_workspace_id)='text' AND length(CAST(home_workspace_id AS BLOB))=36 THEN home_workspace_id END,
             CASE WHEN typeof(lifecycle)='text' AND length(CAST(lifecycle AS BLOB))<=16 THEN lifecycle END FROM bots WHERE bot_id=?1",
            [bot.to_storage()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).optional().map_err(bad)?;
        let (scope, device, home, lifecycle) = raw.ok_or(BotError::NotFound)?;
        if scope != owner.user_scope.to_storage() || device != owner.device.to_storage() {
            return Err(BotError::Forbidden);
        }
        if BotLifecycle::parse(&lifecycle).ok_or(BotError::Storage)? == BotLifecycle::Trashed {
            return Err(BotError::NotFound);
        }
        WorkspaceId::parse(&home).ok_or(BotError::Storage)
    }

    /// A caller's claim is a lookup key only. The worker independently reads
    /// the delegate before using it as evidence or returning an acknowledgement.
    pub fn routine_preparation_for_claim(
        &self,
        claim: &koru_delegate_core::RoutineClaimIdentity,
    ) -> Result<Option<BotRoutineStoredPreparation>, BotError> {
        if !claim.valid() {
            return Err(BotError::Invalid);
        }
        visible(&self.connection, owner(&claim.slot), claim.slot.bot)?;
        let row = rows::read(&self.connection, &claim.slot)?;
        if row
            .as_ref()
            .is_some_and(|row| !claim.matches_prepared(&row.prepared))
        {
            return Err(BotError::Conflict);
        }
        Ok(row)
    }

    pub fn read_routine_preparation(
        &self,
        slot: &RoutineSlot,
        hash: RoutineDigest32,
    ) -> Result<Option<BotRoutineStoredPreparation>, BotError> {
        visible(&self.connection, owner(slot), slot.bot)?;
        let row = rows::read(&self.connection, slot)?;
        if row
            .as_ref()
            .is_some_and(|r| r.prepared.semantic_hash != hash)
        {
            return Err(BotError::Conflict);
        }
        Ok(row)
    }
    pub fn routine_preparation_capacity(&self, slot: &RoutineSlot) -> Result<bool, BotError> {
        visible(&self.connection, owner(slot), slot.bot)?;
        has_capacity(&self.connection, slot)
    }
    pub fn routine_occurrence_source(
        &self,
        slot: &RoutineSlot,
    ) -> Result<(BotRoutineCreationIdentity, BotRoutineCanonical, ProfileId), BotError> {
        if !slot.valid() {
            return Err(BotError::Invalid);
        }
        let (current, profile) = canonical(&self.connection, slot)?;
        let creation =
            bounded::by_schedule(&self.connection, owner(slot), slot.bot, slot.schedule)?
                .ok_or(BotError::NotFound)?;
        if creation.state != BotRoutineCreationState::Bound {
            return Err(BotError::Conflict);
        }
        Ok((creation.identity, current, profile))
    }
    pub fn prepare_routine_occurrence(
        &mut self,
        input: &BotRoutineCapturedSources,
        now: i64,
    ) -> Result<BotRoutinePreparationAdmission, BotError> {
        let hash = input.semantic_hash()?;
        let tx = self.connection.transaction().map_err(bad)?;
        visible(&tx, owner(&input.slot), input.slot.bot)?;
        // This lookup is before current settings, capacity and all six IDs.
        if let Some(row) = rows::read(&tx, &input.slot)? {
            if row.prepared.semantic_hash != hash {
                return Err(BotError::Conflict);
            }
            return Ok(BotRoutinePreparationAdmission::Ready(Box::new(row)));
        }
        if !has_capacity(&tx, &input.slot)? {
            return Ok(BotRoutinePreparationAdmission::Capacity);
        }
        fresh(&tx, input)?;
        let ids = RoutineExecutionIds {
            submission: BotRequestId::mint(),
            run: koru_delegate_core::RunId::mint(),
            chat: uuid::Uuid::new_v4(),
            attempt: uuid::Uuid::new_v4(),
            user_message: uuid::Uuid::new_v4(),
            assistant_message: uuid::Uuid::new_v4(),
        };
        let row = input.freeze(ids, now)?;
        rows::insert(&tx, &row)?;
        tx.commit().map_err(bad)?;
        Ok(BotRoutinePreparationAdmission::Ready(Box::new(row)))
    }
    pub fn close_unclaimed_preparation(
        &mut self,
        slot: &RoutineSlot,
        hash: RoutineDigest32,
        reason: BotRoutineCloseReason,
        observation: &RoutineClaimObservation,
        now: i64,
    ) -> Result<BotRoutineStoredPreparation, BotError> {
        let tx = self.connection.transaction().map_err(bad)?;
        visible(&tx, owner(slot), slot.bot)?;
        let mut row = rows::read(&tx, slot)?.ok_or(BotError::NotFound)?;
        if row.prepared.semantic_hash != hash {
            return Err(BotError::Conflict);
        }
        let closure = BotRoutineStoredClosure::Unclaimed { reason };
        if row.stage == BotRoutineStoredStage::Closed(closure) {
            return Ok(row);
        }
        let check = BotRoutinePreparation {
            prepared: row.prepared.clone(),
            capture: row.capture.clone(),
            stage: BotRoutinePreparationStage::Prepared {},
            created_at_ms: row.created_at_ms,
            updated_at_ms: row.updated_at_ms,
        };
        if row.stage != BotRoutineStoredStage::Prepared
            || !check.can_close(reason, observation, now)
        {
            return Err(BotError::Conflict);
        }
        rows::close(&tx, &mut row, closure, now)?;
        tx.commit().map_err(bad)?;
        Ok(row)
    }
}
