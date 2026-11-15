// SPDX-License-Identifier: MPL-2.0
//! Routine creation on the existing writer; no Store, worker or launch effects.
use super::*;
use koru_delegate_core::{RoutineDigest32, ScheduleId};
mod bounded;
mod detail;
pub use detail::RoutineDetailSnapshot;
mod listing;
pub use listing::{RoutineListCheckpoint, RoutineListSnapshot, RoutineReadSubject};
pub(super) mod model;
pub use model::RoutineToolSubject;
mod occurrence;
mod page;
mod recovery;
#[cfg(test)]
mod tests;

fn owner_of(identity: &BotRoutineCreationIdentity) -> BotOwner {
    BotOwner {
        user_scope: identity.owner.user_scope,
        device: identity.owner.device,
    }
}

fn visible(conn: &Connection, owner: BotOwner, bot: BotId) -> Result<(), BotError> {
    let (_, lifecycle) = admission::guard_any_lifecycle(conn, owner, bot)?;
    if lifecycle == BotLifecycle::Trashed {
        return Err(BotError::NotFound);
    }
    Ok(())
}

fn settled(conn: &Connection, owner: BotOwner, bot: BotId) -> Result<(), BotError> {
    admission::guard(conn, owner, bot)?;
    policy::require_policy_settled(conn, bot)?;
    admission::require_pause_settled(conn, owner.user_scope, bot)?;
    if lifecycle::whole_bot_operation_open(conn, bot)? {
        return Err(BotError::Conflict);
    }
    // Paused is deliberately allowed: this only creates an initially Paused seed.
    Ok(())
}

fn state(value: &str) -> Result<BotRoutineCreationState, BotError> {
    use BotRoutineCreationState::*;
    match value {
        "prepared" => Ok(Prepared),
        "unconfirmed" => Ok(Unconfirmed),
        "bound" => Ok(Bound),
        "conflict" => Ok(Conflict),
        _ => Err(BotError::Storage),
    }
}
fn state_name(value: BotRoutineCreationState) -> &'static str {
    use BotRoutineCreationState::*;
    match value {
        Prepared => "prepared",
        Unconfirmed => "unconfirmed",
        Bound => "bound",
        Conflict => "conflict",
    }
}
fn refusal(value: &str) -> Result<BotRoutineCreationRefusal, BotError> {
    use BotRoutineCreationRefusal::*;
    match value {
        "origin_mismatch" => Ok(OriginMismatch),
        "document_changed" => Ok(DocumentChanged),
        "owner_changed" => Ok(OwnerChanged),
        "authority_changed" => Ok(AuthorityChanged),
        _ => Err(BotError::Storage),
    }
}
fn refusal_name(value: BotRoutineCreationRefusal) -> &'static str {
    use BotRoutineCreationRefusal::*;
    match value {
        OriginMismatch => "origin_mismatch",
        DocumentChanged => "document_changed",
        OwnerChanged => "owner_changed",
        AuthorityChanged => "authority_changed",
    }
}
fn receipt_for(c: &BotRoutineCreation) -> BotRoutineCreationReceipt {
    BotRoutineCreationReceipt {
        version: 1,
        route: c.identity.route(),
        state: if c.state == BotRoutineCreationState::Unconfirmed {
            BotRoutineCreationState::Prepared
        } else {
            c.state
        },
        refusal: c.refusal,
    }
}
fn receipt_stage(c: &BotRoutineCreation) -> &'static str {
    match c.state {
        BotRoutineCreationState::Bound => "completed",
        BotRoutineCreationState::Conflict => "rejected",
        _ => "prepared",
    }
}

impl Repository {
    /// Receipt lookup precedes all Schedule/Store ID allocation. Current config
    /// and grant comparisons apply only to first preparation, not exact replay.
    pub fn prepare_bot_routine_creation(
        &mut self,
        owner: BotOwner,
        command: &BotRoutineCreate,
        fingerprint: RoutineDigest32,
        now: i64,
    ) -> Result<BotRoutineCreationReceipt, BotError> {
        command.validate()?;
        let tx = self.connection.transaction().map_err(bad)?;
        let retained = bounded::receipt(&tx, owner, command.bot, command.request)?;
        visible(&tx, owner, command.bot)?;
        if let Some(header) = retained {
            let creation = bounded::by_request(&tx, owner, command.bot, command.request, &header)?;
            if creation.identity.command_fingerprint != fingerprint
                || !command.matches_retained(&creation.identity)
            {
                return Err(BotError::Conflict);
            }
            return Ok(header.outcome);
        }
        settled(&tx, owner, command.bot)?;
        let source = bounded::source(&tx, owner, command.bot)?;
        if source.revision != command.expected_bot_revision {
            return Err(BotError::RevisionConflict);
        }
        source.validate_for(command)?;
        let identity = command.freeze(
            &source,
            ScheduleId::mint(),
            uuid::Uuid::new_v4(),
            fingerprint,
            now,
        )?;
        let creation = BotRoutineCreation {
            identity,
            state: BotRoutineCreationState::Prepared,
            origin_edit_revision: None,
            refusal: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        let receipt = receipt_for(&creation);
        receipt.validate(&creation.identity)?;
        let id = &creation.identity;
        tx.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,expected_revision,outcome_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,'create_routine','prepared',?5,?6,?7,?7)",
            params![owner.user_scope.to_storage(),command.request.to_storage(),command.bot.to_storage(),String::from(fingerprint),command.expected_bot_revision,serde_json::to_string(&receipt).map_err(bad)?,now]).map_err(bad)?;
        tx.execute("INSERT INTO bot_routines(schedule_id,user_scope_id,owner_device_id,bot_id,workspace_id,creation_request_id,store_request_id,seed_json,seed_hash,seed_title,source_bot_policy_revision,grant_revision,grant_json,grant_digest,creation_state,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,1,?12,?13,'prepared',?14,?14)",
            params![id.schedule.to_storage(),owner.user_scope.to_storage(),owner.device.to_storage(),id.bot.to_storage(),id.home.to_storage(),id.creation_request.to_storage(),id.store_request.to_string(),serde_json::to_string(&id.seed).map_err(bad)?,id.seed_hash.as_bytes().as_slice(),id.seed.name.as_str(),id.grant.source_bot_policy_revision,serde_json::to_string(&id.grant).map_err(bad)?,id.grant.digest.as_bytes().as_slice(),now]).map_err(bad)?;
        tx.commit().map_err(bad)?;
        Ok(receipt)
    }

    pub fn bot_routine_creation_by_request(
        &self,
        owner: BotOwner,
        bot: BotId,
        request: BotRequestId,
    ) -> Result<Option<BotRoutineCreation>, BotError> {
        visible(&self.connection, owner, bot)?;
        bounded::receipt(&self.connection, owner, bot, request)?
            .map(|r| bounded::by_request(&self.connection, owner, bot, request, &r))
            .transpose()
    }
    pub fn bot_routine_creation(
        &self,
        owner: BotOwner,
        bot: BotId,
        schedule: ScheduleId,
    ) -> Result<Option<BotRoutineCreation>, BotError> {
        visible(&self.connection, owner, bot)?;
        bounded::by_schedule(&self.connection, owner, bot, schedule)
    }
    pub fn bot_routine_creation_receipt(
        &self,
        owner: BotOwner,
        bot: BotId,
        request: BotRequestId,
    ) -> Result<Option<BotRoutineCreationReceipt>, BotError> {
        Ok(self
            .bot_routine_creation_by_request(owner, bot, request)?
            .as_ref()
            .map(receipt_for))
    }

    /// The trusted adapter owns retained-origin and live human/Profile proof.
    /// SQL fences its canonical owner/config/policy/Profile snapshot at bind.
    /// Unknown completion remains pending; only proved disagreement is terminal.
    pub fn transition_bot_routine_creation(
        &mut self,
        identity: &BotRoutineCreationIdentity,
        observed: BotRoutineCreationObserved,
        transition: BotRoutineCreationTransition,
        now: i64,
    ) -> Result<BotRoutineCreation, BotError> {
        identity.validate_digests()?;
        let tx = self.connection.transaction().map_err(bad)?;
        visible(&tx, owner_of(identity), identity.bot)?;
        let mut creation =
            bounded::by_schedule(&tx, owner_of(identity), identity.bot, identity.schedule)?
                .ok_or(BotError::NotFound)?;
        if creation.identity != *identity {
            return Err(BotError::Conflict);
        }
        // Terminal exact replay checks evidence before any fresh source read.
        match (&transition, creation.state) {
            (BotRoutineCreationTransition::Confirmed(proof), BotRoutineCreationState::Bound) => {
                proof.validate(identity)?;
                if creation.origin_edit_revision.as_ref() != Some(&proof.origin_edit_revision) {
                    return Err(BotError::Conflict);
                }
                return Ok(creation);
            }
            (BotRoutineCreationTransition::Conflict(reason), BotRoutineCreationState::Conflict)
                if creation.refusal == Some(*reason) =>
            {
                return Ok(creation);
            }
            (_, BotRoutineCreationState::Bound | BotRoutineCreationState::Conflict) => {
                return Err(BotError::Conflict);
            }
            _ => {}
        }
        if creation.observed() != observed {
            return Err(BotError::Conflict);
        }
        match transition {
            BotRoutineCreationTransition::Unconfirmed => {
                if creation.state == BotRoutineCreationState::Unconfirmed {
                    return Ok(creation);
                }
                creation.state = BotRoutineCreationState::Unconfirmed;
            }
            BotRoutineCreationTransition::Confirmed(proof) => {
                proof.validate(identity)?;
                settled(&tx, owner_of(identity), identity.bot)?;
                let source = bounded::source(&tx, owner_of(identity), identity.bot)?;
                if !identity.matches_source(&source)? {
                    return Err(BotError::RevisionConflict);
                }
                creation.state = BotRoutineCreationState::Bound;
                creation.origin_edit_revision = Some(proof.origin_edit_revision);
            }
            BotRoutineCreationTransition::Conflict(reason) => {
                creation.state = BotRoutineCreationState::Conflict;
                creation.refusal = Some(reason);
            }
        }
        persist_transition(&tx, &mut creation, observed, now)?;
        tx.commit().map_err(bad)?;
        Ok(creation)
    }
}

fn persist_transition(
    tx: &rusqlite::Transaction<'_>,
    creation: &mut BotRoutineCreation,
    observed: BotRoutineCreationObserved,
    now: i64,
) -> Result<(), BotError> {
    creation.updated_at_ms = now.max(
        creation
            .updated_at_ms
            .checked_add(1)
            .ok_or(BotError::Storage)?,
    );
    let identity = &creation.identity;
    let changed = tx.execute("UPDATE bot_routines SET creation_state=?2,origin_edit_revision=?3,refusal=?4,updated_at_ms=?5 WHERE schedule_id=?1 AND creation_state=?6 AND updated_at_ms=?7",
        params![identity.schedule.to_storage(),state_name(creation.state),creation.origin_edit_revision,creation.refusal.map(refusal_name),creation.updated_at_ms,state_name(observed.state),observed.updated_at_ms]).map_err(bad)?;
    if changed != 1 {
        return Err(BotError::Conflict);
    }
    if matches!(
        creation.state,
        BotRoutineCreationState::Bound | BotRoutineCreationState::Conflict
    ) {
        let changed = tx.execute("UPDATE bot_command_receipts SET stage=?3,outcome_json=?4,updated_at_ms=?5 WHERE user_scope_id=?1 AND request_id=?2 AND command_kind='create_routine' AND stage='prepared'",
            params![identity.owner.user_scope.to_storage(),identity.creation_request.to_storage(),receipt_stage(creation),serde_json::to_string(&receipt_for(creation)).map_err(bad)?,creation.updated_at_ms]).map_err(bad)?;
        if changed != 1 {
            return Err(BotError::Storage);
        }
    }
    Ok(())
}
