// SPDX-License-Identifier: MPL-2.0
//! A Routine's access after creation (ai.db 23): the one reader of the grant
//! it runs under now, and the one writer of an edit. No Store, delegate or
//! worker effect happens here; the caller installs the Routine policy barrier
//! between `prepare` and `complete`.
//!
//! `bot_routine_grants` holds a row only for a Routine whose access was
//! edited. A Routine with no row is at its creation grant, which stays in
//! `bot_routines` and is never rewritten. Nothing else in this crate reads
//! either place to decide what a Routine may do.
use super::*;

pub struct RoutineAccessRecoveryPage {
    pub scanned_through: Option<i64>,
    pub pending: Vec<(i64, BotId, UserScopeId, BotRequestId)>,
}

/// The grant `creation` runs under now: its `bot_routine_grants` row when its
/// access was edited, otherwise its creation grant. Validated either way:
/// the creation grant by the creation decoder that produced `creation`, an
/// edited one here, against its repeated columns, its digest and its receipt.
pub(super) fn current_on(
    conn: &Connection,
    creation: &BotRoutineCreation,
) -> Result<BotRoutineCurrentGrant, BotError> {
    let identity = &creation.identity;
    let row: Option<(String, String, String, String, u64, u64, String, Vec<u8>)> = conn.query_row("SELECT
        CASE WHEN typeof(user_scope_id)='text' AND length(CAST(user_scope_id AS BLOB))=36 THEN user_scope_id END,
        CASE WHEN typeof(owner_device_id)='text' AND length(CAST(owner_device_id AS BLOB))=36 THEN owner_device_id END,
        CASE WHEN typeof(bot_id)='text' AND length(CAST(bot_id AS BLOB))=36 THEN bot_id END,
        CASE WHEN typeof(request_id)='text' AND length(CAST(request_id AS BLOB))=36 THEN request_id END,
        CASE WHEN typeof(source_bot_policy_revision)='integer' AND source_bot_policy_revision>0 THEN source_bot_policy_revision END,
        CASE WHEN typeof(grant_revision)='integer' AND grant_revision>1 THEN grant_revision END,
        CASE WHEN typeof(grant_json)='text' AND length(CAST(grant_json AS BLOB)) BETWEEN 1 AND 65536 THEN grant_json END,
        CASE WHEN typeof(grant_digest)='blob' AND length(grant_digest)=32 THEN grant_digest END
        FROM bot_routine_grants WHERE schedule_id=?1",
        [identity.schedule.to_storage()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
    ).optional().map_err(bad)?;
    let Some((scope, device, bot, request, source, revision, json, digest)) = row else {
        return Ok(BotRoutineCurrentGrant::original(identity));
    };
    if scope != identity.owner.user_scope.to_storage()
        || device != identity.owner.device.to_storage()
        || bot != identity.bot.to_storage()
    {
        return Err(BotError::Storage);
    }
    let grant: BotRoutineGrant = serde_json::from_str(&json).map_err(bad)?;
    let digest: [u8; 32] = digest.try_into().map_err(|_| BotError::Storage)?;
    let current = BotRoutineCurrentGrant {
        grant,
        request: request.parse().map_err(bad)?,
    };
    if current.grant.source_bot_policy_revision != source
        || current.grant.grant_revision != revision
        || current.grant.digest.as_bytes() != &digest
    {
        return Err(BotError::Storage);
    }
    current.validate(identity).map_err(|_| BotError::Storage)?;
    // The row names the edit that wrote it. That receipt must be this
    // Routine's applied edit to exactly this grant.
    let owner = owner_of(identity);
    let record = record_on(conn, owner, identity.bot, current.request)?.ok_or(BotError::Storage)?;
    if record.stage == BotReceiptStage::Rejected
        || record.receipt.schedule != identity.schedule
        || record.receipt.summary() != Some(current.grant.summary().map_err(|_| BotError::Storage)?)
        || !matches!(&record.receipt.outcome, BotRoutineAccessOutcome::Updated { grant_digest, .. }
            if *grant_digest == current.grant.digest)
    {
        return Err(BotError::Storage);
    }
    Ok(current)
}

/// One access-edit receipt through its bounded decoder. A request of another
/// command kind, or of another Bot, is a conflict and is never decoded.
fn record_on(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    request: BotRequestId,
) -> Result<Option<BotRoutineAccessRecord>, BotError> {
    let header: Option<(String, String)> = conn.query_row("SELECT
        CASE WHEN typeof(bot_id)='text' AND length(CAST(bot_id AS BLOB))=36 THEN bot_id END,
        CASE WHEN typeof(command_kind)='text' AND length(CAST(command_kind AS BLOB))<=32 THEN command_kind END
        FROM bot_command_receipts WHERE user_scope_id=?1 AND request_id=?2",
        params![owner.user_scope.to_storage(), request.to_storage()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional().map_err(bad)?;
    let Some((subject, kind)) = header else {
        return Ok(None);
    };
    if subject != bot.to_storage() || kind != BotOperationKind::UpdateRoutineAccess.token() {
        return Err(BotError::Conflict);
    }
    let (fingerprint, expected, stage, json): (String, u64, String, String) = conn.query_row("SELECT
        CASE WHEN typeof(fingerprint)='text' AND length(CAST(fingerprint AS BLOB))=64 THEN fingerprint END,
        CASE WHEN typeof(expected_revision)='integer' AND expected_revision>0 THEN expected_revision END,
        CASE WHEN typeof(stage)='text' AND length(CAST(stage AS BLOB))<=16 THEN stage END,
        CASE WHEN typeof(outcome_json)='text' AND length(CAST(outcome_json AS BLOB)) BETWEEN 1 AND 16384 THEN outcome_json END
        FROM bot_command_receipts WHERE user_scope_id=?1 AND request_id=?2
        AND planned_run_id IS NULL AND captured_config_revision IS NULL AND captured_policy_revision IS NULL
        AND submission_version IS NULL AND submission_json IS NULL AND desired_pause IS NULL AND pause_ack_stage IS NULL",
        params![owner.user_scope.to_storage(), request.to_storage()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).map_err(bad)?;
    let record = BotRoutineAccessRecord {
        request,
        fingerprint: fingerprint.try_into().map_err(bad)?,
        receipt: serde_json::from_str(&json).map_err(bad)?,
        stage: BotReceiptStage::parse(&stage).ok_or(BotError::Storage)?,
    };
    record.validate().map_err(|_| BotError::Storage)?;
    if record.receipt.bot != bot || record.receipt.expected_grant_revision != expected {
        return Err(BotError::Storage);
    }
    Ok(Some(record))
}

/// An exact retry returns what was retained; the same request with other
/// content, or another fingerprint, is a conflict.
fn replayed(
    record: BotRoutineAccessRecord,
    command: &BotRoutineAccessUpdate,
    fingerprint: RoutineDigest32,
    home: WorkspaceId,
) -> Result<BotRoutineAccessRecord, BotError> {
    if record.fingerprint != fingerprint || !record.receipt.answers(command, home) {
        return Err(BotError::Conflict);
    }
    Ok(record)
}

fn insert_receipt(
    conn: &Connection,
    owner: BotOwner,
    record: &BotRoutineAccessRecord,
    now: i64,
) -> Result<(), BotError> {
    record.validate()?;
    let stage = match record.stage {
        BotReceiptStage::Prepared => "prepared",
        BotReceiptStage::Rejected => "rejected",
        _ => return Err(BotError::Invalid),
    };
    conn.execute(
        "INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,expected_revision,outcome_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,'update_routine_access',?5,?6,?7,?8,?8)",
        params![
            owner.user_scope.to_storage(),
            record.request.to_storage(),
            record.receipt.bot.to_storage(),
            String::from(record.fingerprint),
            stage,
            record.receipt.expected_grant_revision,
            serde_json::to_string(&record.receipt).map_err(bad)?,
            now
        ],
    )
    .map_err(bad)?;
    Ok(())
}

/// The edit of this Routine whose barrier install is not confirmed, if any.
/// The grant row names the last edit, and a new edit is refused while that
/// one is `prepared`, so this is the only edit that can be pending.
fn pending_on(
    conn: &Connection,
    schedule: ScheduleId,
) -> Result<Option<BotRequestId>, BotError> {
    let request: Option<String> = conn.query_row(
        "SELECT CASE WHEN typeof(g.request_id)='text' AND length(CAST(g.request_id AS BLOB))=36 THEN g.request_id END
         FROM bot_routine_grants g JOIN bot_command_receipts r
           ON r.user_scope_id=g.user_scope_id AND r.request_id=g.request_id
         WHERE g.schedule_id=?1 AND r.command_kind='update_routine_access' AND r.stage='prepared'",
        [schedule.to_storage()],
        |r| r.get(0),
    ).optional().map_err(bad)?;
    request.map(|v| v.parse().map_err(bad)).transpose()
}

impl Repository {
    /// The grant a Routine runs under now. The only way to obtain it.
    pub fn routine_current_grant(
        &self,
        owner: BotOwner,
        bot: BotId,
        schedule: ScheduleId,
    ) -> Result<BotRoutineCurrentGrant, BotError> {
        visible(&self.connection, owner, bot)?;
        let creation = bounded::by_schedule(&self.connection, owner, bot, schedule)?
            .ok_or(BotError::NotFound)?;
        current_on(&self.connection, &creation)
    }

    /// The same one reader, for a caller that already holds `creation` from
    /// a reader that proved its Bot's owner and home: preclaim, which reads
    /// bounded metadata on purpose and must not depend on the Bot's current
    /// execution fields. It grants nothing by itself.
    pub fn routine_current_grant_of(
        &self,
        creation: &BotRoutineCreation,
    ) -> Result<BotRoutineCurrentGrant, BotError> {
        current_on(&self.connection, creation)
    }

    /// The access edit of this Routine that is still `prepared`, if any.
    pub fn routine_access_pending(
        &self,
        owner: BotOwner,
        bot: BotId,
        schedule: ScheduleId,
    ) -> Result<Option<BotRequestId>, BotError> {
        admission::guard_any_lifecycle(&self.connection, owner, bot)?;
        bounded::by_schedule(&self.connection, owner, bot, schedule)?.ok_or(BotError::NotFound)?;
        pending_on(&self.connection, schedule)
    }

    /// One retained access edit, by its request. A trashed Bot's are not read.
    pub fn routine_access_record(
        &self,
        owner: BotOwner,
        bot: BotId,
        request: BotRequestId,
    ) -> Result<Option<BotRoutineAccessRecord>, BotError> {
        visible(&self.connection, owner, bot)?;
        record_on(&self.connection, owner, bot, request)
    }

    /// One transaction: the `prepared` receipt and the grant row at the next
    /// revision. Receipt lookup comes first, so an exact retry returns its
    /// receipt even after a later edit, and never raises the revision twice.
    /// The creation grant in `bot_routines` is not written.
    ///
    /// The caller has checked what only it can: that the Schedule is live and
    /// paused, that no Run of the Routine is unsettled, and the person's
    /// rights to the home and to each page.
    pub fn prepare_routine_access_update(
        &mut self,
        owner: BotOwner,
        command: &BotRoutineAccessUpdate,
        fingerprint: RoutineDigest32,
        now: i64,
    ) -> Result<BotRoutineAccessAdmission, BotError> {
        command.validate()?;
        let tx = self.connection.transaction().map_err(bad)?;
        let retained = record_on(&tx, owner, command.bot, command.request)?;
        visible(&tx, owner, command.bot)?;
        let creation = bounded::by_schedule(&tx, owner, command.bot, command.schedule)?
            .ok_or(BotError::NotFound)?;
        let home = creation.identity.home;
        if let Some(record) = retained {
            return Ok(BotRoutineAccessAdmission::Retained(replayed(
                record,
                command,
                fingerprint,
                home,
            )?));
        }
        // Bot Active, nothing of the Bot pending, no Archive or Trash open.
        settled(&tx, owner, command.bot)?;
        if creation.state != BotRoutineCreationState::Bound {
            return Err(BotError::Conflict);
        }
        let current = current_on(&tx, &creation)?;
        if let Some(request) = pending_on(&tx, command.schedule)? {
            return Ok(BotRoutineAccessAdmission::Pending { request });
        }
        if current.grant.grant_revision != command.expected_grant_revision {
            return Err(BotError::RevisionConflict);
        }
        let observed = admission::guard(&tx, owner, command.bot)?;
        if observed.home != home {
            return Err(BotError::Forbidden);
        }
        let grant = command.grant(home, observed.policy)?;
        let record = BotRoutineAccessRecord {
            request: command.request,
            fingerprint,
            receipt: BotRoutineAccessReceipt::updated(command, &grant)?,
            stage: BotReceiptStage::Prepared,
        };
        insert_receipt(&tx, owner, &record, now)?;
        // Insert the first edit, or replace the earlier edit's row. The
        // replace is fenced on the revision it was read at.
        let changed = tx.execute(
            "INSERT INTO bot_routine_grants(schedule_id,user_scope_id,owner_device_id,bot_id,request_id,source_bot_policy_revision,grant_revision,grant_json,grant_digest,updated_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(schedule_id) DO UPDATE SET request_id=excluded.request_id,
               source_bot_policy_revision=excluded.source_bot_policy_revision,grant_revision=excluded.grant_revision,
               grant_json=excluded.grant_json,grant_digest=excluded.grant_digest,updated_at_ms=excluded.updated_at_ms
             WHERE bot_routine_grants.grant_revision=?11 AND bot_routine_grants.user_scope_id=excluded.user_scope_id
               AND bot_routine_grants.owner_device_id=excluded.owner_device_id AND bot_routine_grants.bot_id=excluded.bot_id",
            params![
                command.schedule.to_storage(),
                owner.user_scope.to_storage(),
                owner.device.to_storage(),
                command.bot.to_storage(),
                command.request.to_storage(),
                grant.source_bot_policy_revision,
                grant.grant_revision,
                serde_json::to_string(&grant).map_err(bad)?,
                grant.digest.as_bytes().as_slice(),
                now,
                command.expected_grant_revision
            ],
        )
        .map_err(bad)?;
        if changed != 1 {
            return Err(BotError::RevisionConflict);
        }
        // Decode what was written through the one reader before commit, so a
        // representation mismatch rolls the receipt and the row back together.
        let written = current_on(&tx, &creation)?;
        if written.grant != grant || written.request != command.request {
            return Err(BotError::Storage);
        }
        tx.commit().map_err(bad)?;
        Ok(BotRoutineAccessAdmission::Prepared(record))
    }

    /// Retain a refusal the same request would meet again, in place of a
    /// `prepared` receipt. No grant row is written. An exact retry, of this
    /// or of an applied edit, returns what was retained.
    pub fn refuse_routine_access_update(
        &mut self,
        owner: BotOwner,
        command: &BotRoutineAccessUpdate,
        fingerprint: RoutineDigest32,
        reason: BotRoutineAccessRefusal,
        now: i64,
    ) -> Result<BotRoutineAccessRecord, BotError> {
        command.validate()?;
        let tx = self.connection.transaction().map_err(bad)?;
        let retained = record_on(&tx, owner, command.bot, command.request)?;
        visible(&tx, owner, command.bot)?;
        let creation = bounded::by_schedule(&tx, owner, command.bot, command.schedule)?
            .ok_or(BotError::NotFound)?;
        if let Some(record) = retained {
            return replayed(record, command, fingerprint, creation.identity.home);
        }
        let record = BotRoutineAccessRecord {
            request: command.request,
            fingerprint,
            receipt: BotRoutineAccessReceipt::refused(command, reason)?,
            stage: BotReceiptStage::Rejected,
        };
        insert_receipt(&tx, owner, &record, now)?;
        tx.commit().map_err(bad)?;
        Ok(record)
    }

    /// The barrier for the edited revision is installed: the receipt becomes
    /// `completed`. Repeating it returns the completed record. A `prepared`
    /// edit always completes, whatever happened to the Bot since: its grant
    /// row was replaced in the same transaction and has no history to return
    /// to, so only the owner fence is checked here, not the Bot's lifecycle.
    pub fn complete_routine_access_update(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        request: BotRequestId,
        now: i64,
    ) -> Result<BotRoutineAccessRecord, BotError> {
        let tx = self.connection.transaction().map_err(bad)?;
        admission::guard_any_lifecycle(&tx, owner, bot)?;
        let mut record = record_on(&tx, owner, bot, request)?.ok_or(BotError::NotFound)?;
        match record.stage {
            BotReceiptStage::Completed => return Ok(record),
            BotReceiptStage::Prepared => {}
            _ => return Err(BotError::Conflict),
        }
        // Still the edit its Routine's row names: nothing can replace a row
        // while its edit is prepared.
        if pending_on(&tx, record.receipt.schedule)? != Some(request) {
            return Err(BotError::Storage);
        }
        let changed = tx.execute(
            "UPDATE bot_command_receipts SET stage='completed',updated_at_ms=MAX(updated_at_ms,?4)
             WHERE user_scope_id=?1 AND request_id=?2 AND bot_id=?3 AND command_kind='update_routine_access' AND stage='prepared'",
            params![owner.user_scope.to_storage(), request.to_storage(), bot.to_storage(), now],
        )
        .map_err(bad)?;
        if changed != 1 {
            return Err(BotError::Conflict);
        }
        tx.commit().map_err(bad)?;
        record.stage = BotReceiptStage::Completed;
        Ok(record)
    }

    /// One page of access edits still `prepared`, read through the ai23
    /// partial index (`bot_pending_routine_access`), so settled history is
    /// never walked. A restart repeats each one's barrier install.
    pub fn pending_routine_access(
        &self,
        after: i64,
        limit: usize,
    ) -> Result<RoutineAccessRecoveryPage, BotError> {
        let mut query = self.connection.prepare(
            "SELECT rowid,bot_id,user_scope_id,request_id FROM bot_command_receipts INDEXED BY bot_pending_routine_access WHERE rowid>?1 AND command_kind='update_routine_access' AND stage='prepared' ORDER BY rowid LIMIT ?2",
        ).map_err(bad)?;
        let rows = query
            .query_map(params![after, limit.clamp(1, 64)], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(bad)?;
        let mut page = RoutineAccessRecoveryPage {
            scanned_through: None,
            pending: Vec::new(),
        };
        for row in rows {
            let (row, bot, scope, request) = row.map_err(bad)?;
            page.scanned_through = Some(row);
            page.pending.push((
                row,
                bot.parse().map_err(bad)?,
                UserScopeId::parse(&scope).ok_or(BotError::Storage)?,
                request.parse().map_err(bad)?,
            ));
        }
        Ok(page)
    }
}

#[cfg(test)]
mod tests;
