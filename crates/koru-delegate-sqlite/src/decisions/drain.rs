// SPDX-License-Identifier: MPL-2.0

//! Correlate bounded drain evidence with the native effect owner's scalars.
use super::*;

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
