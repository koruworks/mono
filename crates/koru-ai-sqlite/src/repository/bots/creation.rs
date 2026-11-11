// SPDX-License-Identifier: MPL-2.0

//! Creating a Bot with its Role page and folder (P1.8 B2), on ai.db 18 with
//! no schema change. The `creation_v2` receipt lives in the existing
//! `bot_command_receipts` table as `command_kind = 'create'` with the
//! existing outer stages; its sub-phases live in the typed outcome.
//!
//! A BotId is reserved against the `bots` primary key, the purge tombstone
//! primary key `(scope, 'bot', id)`, and every unfinished or completed
//! `create` receipt through ai17's `bot_command_guard (bot_id, command_kind,
//! stage)`. Legacy Create shares the same check ([`reserve_bot_id`]).
//!
//! Nothing indexes unfinished creations without a BotId, so the restart
//! scan walks `bot_command_guard` once per start, a bounded number of
//! distinct BotIds per step (two index seeks each), and the writer keeps the
//! list in memory from then on. Wanted at the next ai.db version: a partial
//! index `bot_command_receipts(created_at_ms) WHERE command_kind = 'create'
//! AND stage IN ('prepared','accepted')`.
use super::*;
use rusqlite::TransactionBehavior;

/// Whether a BotId was ever reserved by a `create` that is not rejected.
pub(crate) const CREATION_RESERVED_SQL: &str =
    "SELECT EXISTS(SELECT 1 FROM bot_command_receipts INDEXED BY bot_command_guard
  WHERE bot_id=?1 AND command_kind='create' AND stage IN ('prepared','accepted','completed'))";
/// The next BotId named by any receipt, after `?1`: one seek.
pub(crate) const CREATION_NEXT_BOT_SQL: &str =
    "SELECT bot_id FROM bot_command_receipts INDEXED BY bot_command_guard
  WHERE bot_id>?1 ORDER BY bot_id LIMIT 1";
/// The unfinished `create` receipts of one BotId (at most one): one seek.
pub(crate) const CREATION_OPEN_SQL: &str =
    "SELECT user_scope_id,request_id FROM bot_command_receipts INDEXED BY bot_command_guard
  WHERE bot_id=?1 AND command_kind='create' AND stage IN ('prepared','accepted')";

/// The one BotId reservation both Create commands use, inside the caller's
/// transaction: a live or archived Bot, a purged one's tombstone, or another
/// unfinished or completed creation holds it.
pub(crate) fn reserve_bot_id(
    conn: &Connection,
    scope: UserScopeId,
    bot: BotId,
) -> Result<(), BotError> {
    // Production always opens the current schema, so it always checks both.
    // Only this crate's migration fixtures write Bots with this code at
    // ai16/ai17 (purge tombstones arrived with ai18, when nothing could have
    // been purged yet, and `bot_command_guard` with ai17); the older form
    // exists for them alone and is not compiled outside tests.
    #[cfg(test)]
    let version = crate::db::user_version(conn).map_err(bad)?;
    #[cfg(not(test))]
    let version = crate::db::SCHEMA_VERSION;
    let tombstones = if version >= 18 {
        "EXISTS(SELECT 1 FROM bot_purge_tombstones WHERE user_scope_id=?2 AND subject_kind='bot' AND subject_id=?1)"
    } else {
        "?2 IS NULL"
    };
    let reserved = if version >= 17 {
        CREATION_RESERVED_SQL.to_owned()
    } else {
        CREATION_RESERVED_SQL.replace(" INDEXED BY bot_command_guard", "")
    };
    let (live, purged, reserved): (bool, bool, bool) = conn
        .query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM bots WHERE bot_id=?1), {tombstones}, ({reserved})"
            ),
            params![bot.to_storage(), scope.to_storage()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(bad)?;
    if live || purged || reserved {
        return Err(BotError::Conflict);
    }
    Ok(())
}

/// One bounded step of the startup scan of unfinished creations.
#[derive(Debug, Default)]
pub struct BotCreationScan {
    /// The unfinished creations found, by owner scope and request.
    pub found: Vec<(UserScopeId, BotRequestId)>,
    /// BotIds whose unfinished creation row could not be parsed. Skipped,
    /// so one damaged row never stops the scan; the caller reports each.
    pub skipped: Vec<String>,
    /// Where the next step starts; `None` once every BotId was visited.
    pub next: Option<String>,
}

/// The outer stage a record may be stored at.
fn check_stage(record: &BotCreationRecord, stage: BotReceiptStage) -> Result<(), BotError> {
    let seed = match &record.role {
        BotCreationRole::Existing { .. } => None,
        BotCreationRole::Authored { seed, .. } => Some(seed.is_some()),
    };
    let ok = match stage {
        BotReceiptStage::Prepared => {
            record.phase <= BotCreationPhase::RolePending
                && (record.authored() || record.phase <= BotCreationPhase::FolderReady)
                && seed != Some(false)
                && !record.abandoned()
        }
        BotReceiptStage::Accepted => {
            record.authored()
                && record.phase == BotCreationPhase::RoleVerified
                && seed == Some(true)
                && !record.abandoned()
        }
        // Finished: the Bot made, or the person abandoned it. Either way the
        // seed is gone and the BotId is reserved for good.
        BotReceiptStage::Completed => (record.ready() || record.abandoned()) && seed != Some(true),
        BotReceiptStage::Rejected => {
            record.phase <= BotCreationPhase::FolderPending
                && matches!(
                    record.reason,
                    Some(BotCreationReason::FolderOccupied | BotCreationReason::FolderInvalid)
                )
                && seed != Some(true)
                && !record.abandoned()
        }
    };
    if ok { Ok(()) } else { Err(BotError::Invalid) }
}

fn stage_token(stage: BotReceiptStage) -> &'static str {
    match stage {
        BotReceiptStage::Prepared => "prepared",
        BotReceiptStage::Accepted => "accepted",
        BotReceiptStage::Rejected => "rejected",
        BotReceiptStage::Completed => "completed",
    }
}

/// The `creation_v2` receipt of one request, decoded strictly. `Conflict`
/// when the request names a receipt of another kind (a legacy Create's or
/// any other command's); `Storage` when it does not decode.
fn load(
    conn: &Connection,
    scope: UserScopeId,
    request: BotRequestId,
) -> Result<Option<BotCreationReceipt>, BotError> {
    let Some(row) = admission::raw(conn, scope, request)? else {
        return Ok(None);
    };
    if row.kind != "create" {
        return Err(BotError::Conflict);
    }
    let json = row.outcome.ok_or(BotError::Storage)?;
    let record = BotCreationRecord::decode(&json)?.ok_or(BotError::Conflict)?;
    let stage = BotReceiptStage::parse(&row.stage).ok_or(BotError::Storage)?;
    if row.bot != record.bot.to_storage()
        || record.user_scope != scope
        || row.fingerprint.len() != 64
        || !row.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(BotError::Storage);
    }
    check_stage(&record, stage).map_err(|_| BotError::Storage)?;
    Ok(Some(BotCreationReceipt {
        request,
        record,
        fingerprint: row.fingerprint,
        stage,
    }))
}

impl Repository {
    /// T1: record the intent and reserve its BotId, before any folder, Store
    /// call, Bot or Chat exists. The same request and fingerprint returns
    /// the stored receipt; anything else under the request, or another
    /// request for a reserved BotId, is `Conflict`.
    pub fn begin_bot_creation(
        &mut self,
        request: BotRequestId,
        record: &BotCreationRecord,
        fingerprint: &str,
        now: i64,
    ) -> Result<BotCreationReceipt, BotError> {
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(BotError::Invalid);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        if let Some(existing) = load(&tx, record.user_scope, request)? {
            return if existing.fingerprint == fingerprint && existing.record.bot == record.bot {
                Ok(existing)
            } else {
                Err(BotError::Conflict)
            };
        }
        if record.phase != BotCreationPhase::Intent
            || record.reason.is_some()
            || !matches!(
                record.role,
                BotCreationRole::Existing { .. } | BotCreationRole::Authored { seed: Some(_), .. }
            )
        {
            return Err(BotError::Invalid);
        }
        let json = record.encode()?;
        reserve_bot_id(&tx, record.user_scope, record.bot)?;
        tx.execute(
            "INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,outcome_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,'create','prepared',?5,?6,?6)",
            params![record.user_scope.to_storage(), request.to_storage(), record.bot.to_storage(), fingerprint, json, now],
        )
        .map_err(bad)?;
        let stored = load(&tx, record.user_scope, request)?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(stored)
    }

    /// One creation receipt, or `None`. A request naming another kind of
    /// receipt is `Conflict`. The caller has checked current authority.
    pub fn bot_creation(
        &self,
        scope: UserScopeId,
        request: BotRequestId,
    ) -> Result<Option<BotCreationReceipt>, BotError> {
        load(&self.connection, scope, request)
    }

    /// T2/T3, a park or a rejection: move an unfinished creation from exactly
    /// `expected` to `next`. Only the phase (forward), reason and verified
    /// evidence may change; a stale `expected` is `Conflict` and writes
    /// nothing. A rejection drops the prepared seed.
    pub fn advance_bot_creation(
        &mut self,
        scope: UserScopeId,
        request: BotRequestId,
        expected: &BotCreationRecord,
        next: &BotCreationRecord,
        stage: BotReceiptStage,
        now: i64,
    ) -> Result<BotCreationReceipt, BotError> {
        let next = if stage == BotReceiptStage::Rejected {
            next.without_seed()
        } else {
            next.clone()
        };
        if stage == BotReceiptStage::Completed || !expected.same_intent(&next) {
            return Err(BotError::Invalid);
        }
        check_stage(&next, stage)?;
        let json = next.encode()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let current = load(&tx, scope, request)?.ok_or(BotError::NotFound)?;
        if !current.open() || current.record != *expected {
            return Err(BotError::Conflict);
        }
        tx.execute(
            "UPDATE bot_command_receipts SET stage=?3,outcome_json=?4,updated_at_ms=?5 WHERE user_scope_id=?1 AND request_id=?2",
            params![scope.to_storage(), request.to_storage(), stage_token(stage), json, now],
        )
        .map_err(bad)?;
        let stored = load(&tx, scope, request)?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(stored)
    }

    /// Abandon: end an unfinished creation exactly at `expected`, as the
    /// person asked. The receipt becomes `completed`, marked `abandoned`
    /// with the reason it had stopped for kept, and its seed dropped, so the BotId stays reserved for good; its phase
    /// stays where it stopped, naming what may exist. Nothing else is
    /// written or removed. An already abandoned creation answers as it is;
    /// any other finished one, or a stale `expected`, is `Conflict`.
    pub fn abandon_bot_creation(
        &mut self,
        scope: UserScopeId,
        request: BotRequestId,
        expected: &BotCreationRecord,
        now: i64,
    ) -> Result<BotCreationReceipt, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let current = load(&tx, scope, request)?.ok_or(BotError::NotFound)?;
        if !current.open() {
            return if current.stage == BotReceiptStage::Completed && current.record.abandoned() {
                Ok(current)
            } else {
                Err(BotError::Conflict)
            };
        }
        if current.record != *expected {
            return Err(BotError::Conflict);
        }
        let mut ended = expected.without_seed();
        ended.abandoned = true;
        if !expected.same_intent(&ended) {
            return Err(BotError::Invalid);
        }
        check_stage(&ended, BotReceiptStage::Completed)?;
        tx.execute(
            "UPDATE bot_command_receipts SET stage='completed',outcome_json=?3,updated_at_ms=?4 WHERE user_scope_id=?1 AND request_id=?2",
            params![scope.to_storage(), request.to_storage(), ended.encode()?, now],
        )
        .map_err(bad)?;
        let stored = load(&tx, scope, request)?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(stored)
    }

    /// T4: in one transaction the Bot (owning its Role page when the receipt
    /// created it), its main Chat, the association and the selected pages,
    /// and the receipt `completed` with its seed dropped. The Bot is built
    /// from the stored record alone. Any failure rolls every row back and
    /// leaves the receipt as it was, so the step can be repeated alone.
    pub fn complete_bot_creation(
        &mut self,
        scope: UserScopeId,
        request: BotRequestId,
        expected: &BotCreationRecord,
        profile: ProfileId,
        now: i64,
    ) -> Result<(BotCreationReceipt, Bot), BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let current = load(&tx, scope, request)?.ok_or(BotError::NotFound)?;
        let record = &current.record;
        let staged = if record.authored() {
            BotReceiptStage::Accepted
        } else {
            BotReceiptStage::Prepared
        };
        if current.record != *expected || !record.ready() || current.stage != staged {
            return Err(BotError::Conflict);
        }
        let live: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM bots WHERE bot_id=?1)
                    OR EXISTS(SELECT 1 FROM bot_purge_tombstones WHERE user_scope_id=?2 AND subject_kind='bot' AND subject_id=?1)",
                params![record.bot.to_storage(), scope.to_storage()],
                |r| r.get(0),
            )
            .map_err(bad)?;
        if live {
            return Err(BotError::Conflict);
        }
        let bot = record.bot(now)?;
        let at = Timestamp::from_millis(now);
        insert_chat(
            &tx,
            &Chat {
                id: bot.main_chat,
                title: bot.config.name.clone(),
                private: false,
                current_model: bot.config.model.clone(),
                branched_from: None,
                agent_working_folder: Some(bot.config.working_folder.clone()),
                agent_mode: bot.config.mode.clone(),
                agent_fast: bot.config.fast,
                pinned_at: None,
                profile_id: Some(profile),
                bot: None,
                created_at: at,
                updated_at: at,
            },
        )
        .map_err(|_| BotError::Storage)?;
        let owned = record.authored();
        tx.execute(
            "INSERT INTO bots(bot_id,user_scope_id,home_workspace_id,owner_device_id,name,role_workspace_id,role_document_id,role_owned,role_creation_request_id,working_folder,provider,model,reasoning,mode,fast,revision,policy_revision,access,created_at_ms,updated_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,1,1,?16,?17,?17)",
            params![
                bot.id.to_storage(),
                bot.user_scope.to_storage(),
                bot.home_workspace.to_storage(),
                bot.owner_device.to_storage(),
                bot.config.name,
                bot.config.role.workspace.to_storage(),
                bot.config.role.document.to_storage(),
                owned,
                owned.then(|| request.to_storage()),
                bot.config.working_folder,
                bot.config.model.provider.as_str(),
                bot.config.model.requested_model,
                bot.config.model.default_reasoning.as_token(),
                bot.config.mode,
                bot.config.fast,
                match bot.access {
                    BotAccess::Default => "default",
                    BotAccess::Workspace => "workspace",
                },
                now
            ],
        )
        .map_err(bad)?;
        tx.execute(
            "INSERT INTO bot_chats(chat_id,bot_id,purpose,created_at_ms) VALUES(?1,?2,'main',?3)",
            params![bot.main_chat.to_storage(), bot.id.to_storage(), now],
        )
        .map_err(bad)?;
        for page in &bot.selected_pages {
            tx.execute(
                "INSERT INTO bot_selected_pages(bot_id,workspace_id,document_id,selected_at_ms) VALUES(?1,?2,?3,?4)",
                params![bot.id.to_storage(), page.workspace.to_storage(), page.document.to_storage(), now],
            )
            .map_err(bad)?;
        }
        let done = record.without_seed();
        tx.execute(
            "UPDATE bot_command_receipts SET stage='completed',outcome_json=?3,updated_at_ms=?4 WHERE user_scope_id=?1 AND request_id=?2",
            params![scope.to_storage(), request.to_storage(), done.encode()?, now],
        )
        .map_err(bad)?;
        let stored = load(&tx, scope, request)?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok((stored, bot))
    }

    /// One bounded step of the startup scan: up to `bots` distinct BotIds
    /// after `after` (start from `""`), each with its unfinished creation if
    /// any. Two index seeks per BotId; no outcome is read. A row whose keys
    /// do not parse is skipped and named, never a reason to stop.
    pub fn bot_creation_scan(&self, after: &str, bots: u16) -> Result<BotCreationScan, BotError> {
        let mut next = self
            .connection
            .prepare_cached(CREATION_NEXT_BOT_SQL)
            .map_err(bad)?;
        let mut open = self
            .connection
            .prepare_cached(CREATION_OPEN_SQL)
            .map_err(bad)?;
        let mut step = BotCreationScan::default();
        let mut cursor = after.to_owned();
        for _ in 0..bots.clamp(1, 64) {
            let Some(bot) = next
                .query_row([&cursor], |r| r.get::<_, String>(0))
                .optional()
                .map_err(bad)?
            else {
                return Ok(step);
            };
            let rows = open
                .query_map([&bot], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(bad)?;
            for row in rows {
                let (scope, request) = row.map_err(bad)?;
                match (UserScopeId::parse(&scope), request.parse::<BotRequestId>()) {
                    (Some(scope), Ok(request)) => step.found.push((scope, request)),
                    _ => step.skipped.push(bot.clone()),
                }
            }
            cursor = bot;
        }
        step.next = Some(cursor);
        Ok(step)
    }

    /// Fixture only: raw SQL against the AI database, for failure injection
    /// (a trigger that makes the final transaction fail) and seeded state.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn fixture_execute_batch(&self, sql: &str) -> Result<(), BotError> {
        self.connection.execute_batch(sql).map_err(bad)
    }
}

#[cfg(test)]
mod tests;
