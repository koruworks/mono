// SPDX-License-Identifier: MPL-2.0

//! Correlate bounded drain evidence with the native effect owner's scalars.
use super::*;

/// More fenced/drained effects than this for one attempt cannot be the
/// complete list inside the 4KiB evidence body anyway; the probe reads one more
/// identity to detect that and refuses rather than truncating.
const DRAIN_PROBE: usize = 64;

/// The complete retained drain list for one exact retired attempt, built from
/// the sole native-result decoder one row at a time. It is evidence, not a
/// cleanup decision: a held lease, a live attempt, an overflow or a corrupt row
/// refuses, and the result passes the same correlation the cleanup writers use.
pub(super) fn retained_on(
    conn: &Connection,
    owner: BotOwner,
    run: RunId,
    attempt: RunAttemptId,
    generation: AttemptGeneration,
    observed_at_ms: i64,
) -> Result<DecisionNativeDrain, BotLedgerError> {
    authority::owner(conn, owner, run)?;
    let bot: Option<String> = conn
        .query_row(
            "SELECT CASE WHEN typeof(bot_id)='text' AND length(CAST(bot_id AS BLOB))=36 THEN bot_id END
             FROM bot_admissions WHERE run_id=?1",
            [run.to_storage()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    let bot: BotId = bot
        .ok_or_else(|| corrupt("bot_admissions", "bot_id"))?
        .parse()
        .map_err(|_| corrupt("bot_admissions", "bot_id"))?;
    // Only a retired, ended attempt can no longer gain a native effect.
    let row =
        crate::attempts::retained_bounded(conn, run, generation)?.ok_or(BotLedgerError::Fenced)?;
    require(row.attempt == attempt)?;
    if !row.retired || !row.phase.is_terminal() {
        return Err(BotLedgerError::Fenced);
    }
    let held: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM native_result_intents WHERE run_id=?1 AND attempt_id=?2 AND generation=?3 AND lease_state='held')", params![run.to_storage(),attempt.to_storage(),generation.get()], |r| r.get(0)).map_err(sql)?;
    if held {
        return Err(BotLedgerError::Fenced);
    }
    let mut statement = conn
        .prepare(
            "SELECT CASE WHEN typeof(operation_id)='text' AND length(CAST(operation_id AS BLOB))=36 THEN operation_id END,
                    CASE WHEN typeof(turn_id)='text' AND length(CAST(turn_id AS BLOB))=36 THEN turn_id END
             FROM native_result_intents WHERE run_id=?1 AND attempt_id=?2 AND generation=?3
             AND (effect_fenced_at_ms IS NOT NULL OR effect_drained_at_ms IS NOT NULL)
             ORDER BY operation_id LIMIT ?4",
        )
        .map_err(sql)?;
    let mut cursor = statement
        .query(params![
            run.to_storage(),
            attempt.to_storage(),
            generation.get(),
            DRAIN_PROBE + 1
        ])
        .map_err(sql)?;
    let mut effects = Vec::new();
    while let Some(found) = cursor.next().map_err(sql)? {
        if effects.len() == DRAIN_PROBE {
            return Err(BotLedgerError::Capacity);
        }
        let operation: Option<String> = found.get(0).map_err(sql)?;
        let turn: Option<String> = found.get(1).map_err(sql)?;
        let key = NativeResultKey {
            run,
            attempt,
            generation,
            turn: turn
                .ok_or_else(|| corrupt("native_result_intents", "turn_id"))?
                .parse()
                .map_err(|_| corrupt("native_result_intents", "turn_id"))?,
            operation_id: operation
                .ok_or_else(|| corrupt("native_result_intents", "operation_id"))?
                .parse()
                .map_err(|_| corrupt("native_result_intents", "operation_id"))?,
        };
        let intent = crate::native_results::retained_result_on(conn, &key)?
            .ok_or_else(|| corrupt("native_result_intents", "drain"))?;
        effects.push(
            intent
                .drain
                .ok_or_else(|| corrupt("native_result_intents", "drain"))?,
        );
    }
    let drain = DecisionNativeDrain {
        run,
        attempt,
        generation,
        observed_at_ms,
        effects: DecisionList::new(effects).map_err(|_| BotLedgerError::Capacity)?,
    };
    // Too many receipts for one 4KiB evidence body cannot be stated completely.
    encoded(&drain, DECISION_EVIDENCE_BYTES).map_err(|_| BotLedgerError::Capacity)?;
    correlate(conn, owner, bot, &drain)?;
    Ok(drain)
}

impl RunsDb {
    /// Complete retained native drain evidence for one exact retired attempt,
    /// for a final cleanup proof. Read-only; it grants no cleanup by itself.
    pub fn decision_native_drain(
        &self,
        owner: BotOwner,
        run: RunId,
        attempt: RunAttemptId,
        generation: AttemptGeneration,
        observed_at_ms: i64,
    ) -> Result<DecisionNativeDrain, BotLedgerError> {
        retained_on(self.conn(), owner, run, attempt, generation, observed_at_ms)
    }
}

/// A drain list is complete only when every retained drained effect of this
/// attempt is present, and no effect still owns a live lease. Definitive
/// Durable/NotApplied receipts without a drain do not need a synthetic one.
pub(super) fn correlate(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    drain: &DecisionNativeDrain,
) -> Result<(), BotLedgerError> {
    valid(drain)?;
    let held: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM native_result_intents WHERE run_id=?1 AND attempt_id=?2 AND generation=?3 AND lease_state='held')", params![drain.run.to_storage(),drain.attempt.to_storage(),drain.generation.get()], |r| r.get(0)).map_err(sql)?;
    if held {
        return Err(BotLedgerError::Fenced);
    }
    let receipts = drain.effects.as_slice();
    let mut statement = conn.prepare(
        "SELECT CASE WHEN typeof(store_request_id)='text' AND length(CAST(store_request_id AS BLOB))=36 THEN store_request_id END
         FROM native_result_intents WHERE run_id=?1 AND attempt_id=?2 AND generation=?3
         AND (effect_fenced_at_ms IS NOT NULL OR effect_drained_at_ms IS NOT NULL)
         ORDER BY operation_id LIMIT ?4",
    ).map_err(sql)?;
    let mut stored = statement
        .query(params![
            drain.run.to_storage(),
            drain.attempt.to_storage(),
            drain.generation.get(),
            receipts.len() + 1
        ])
        .map_err(sql)?;
    let mut count = 0;
    while let Some(row) = stored.next().map_err(sql)? {
        count += 1;
        let key: String = row.get(0).map_err(sql)?;
        let key: uuid::Uuid = key
            .parse()
            .map_err(|_| corrupt("native_result_intents", "store_request_id"))?;
        let mut matching = receipts.iter().filter(|r| r.binding.request_key == key);
        let receipt = matching.next().ok_or(BotLedgerError::Fenced)?;
        require(matching.next().is_none())?;
        let binding = &receipt.binding;
        require(
            binding.user_scope == owner.user_scope
                && binding.scope == native_store_scope(owner, bot, drain.run)
                && receipt.drained_at.millis() <= drain.observed_at_ms,
        )?;
        let kind = match binding.kind {
            NativeEffectKind::CreateDocument => "create_page",
            NativeEffectKind::EditDocument => "edit_page",
        };
        // Compare complete retained identity without allocating title, payload,
        // expected VV, or another receipt body. NULL never means a match.
        let same: bool = conn
            .query_row(
                "SELECT COALESCE(run_id=?2 AND attempt_id=?3 AND generation=?4
             AND lease_user_scope=?5 AND lease_device_id=?6 AND lease_bot_id=?7
             AND destination_workspace_id=?8 AND target_document_id=?9 AND kind=?10
             AND request_fingerprint=?11 AND effect_fenced_at_ms=?12 AND effect_drained_at_ms=?13
             AND lease_state='settled' AND stage IN ('settled_unknown','verified','registered'),0)
             FROM native_result_intents WHERE store_request_id=?1",
                params![
                    key.to_string(),
                    drain.run.to_storage(),
                    drain.attempt.to_storage(),
                    drain.generation.get(),
                    owner.user_scope.to_storage(),
                    owner.device.to_storage(),
                    bot.to_storage(),
                    binding.workspace.to_storage(),
                    binding.document.to_storage(),
                    kind,
                    binding.request_fingerprint.as_slice(),
                    receipt.fenced_at.millis(),
                    receipt.drained_at.millis()
                ],
                |r| r.get(0),
            )
            .map_err(sql)?;
        require(same)?;
    }
    require(count == receipts.len())
}
