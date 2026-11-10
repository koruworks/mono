// SPDX-License-Identifier: MPL-2.0
use super::{rows::*, *};
use crate::RunsDb;
use rusqlite::params;

pub(super) fn pending(
    db: &RunsDb,
    owner: BotOwner,
    bot: Option<BotId>,
    after: Option<&NativeRecoveryCursor>,
    limit: u16,
) -> Result<NativePendingEffects, BotLedgerError> {
    if owner.user_scope.as_uuid().is_nil()
        || owner.device.as_uuid().is_nil()
        || limit == 0
        || limit > 64
    {
        return Err(BotLedgerError::Invalid);
    }
    let (time, run, operation) = after
        .map(|c| {
            (
                c.created_at_ms,
                c.run.to_storage(),
                c.operation_id.to_string(),
            )
        })
        .unwrap_or((i64::MIN, String::new(), String::new()));
    let (query, ids) = if let Some(bot) = bot {
        (
            BOT_QUERY,
            vec![
                owner.user_scope.to_storage(),
                owner.device.to_storage(),
                bot.to_storage(),
            ],
        )
    } else {
        (
            OWNER_QUERY,
            vec![owner.user_scope.to_storage(), owner.device.to_storage()],
        )
    };
    let mut statement = db.conn().prepare(query).map_err(sql)?;
    let mut values: Vec<rusqlite::types::Value> = ids.into_iter().map(Into::into).collect();
    values.extend([
        time.into(),
        run.into(),
        operation.into(),
        i64::from(limit).into(),
    ]);
    let keys = statement
        .query_map(rusqlite::params_from_iter(values), |row| Ok(key(row)))
        .map_err(sql)?
        .map(|r| r.map_err(sql)?)
        .collect::<Result<Vec<_>, _>>()?;
    let mut targets = Vec::with_capacity(keys.len());
    for key in keys {
        let record = required(db.conn(), &key)?;
        let target = record.pending_effect()?;
        // Recovery reads only immutable owner columns, never the admission's
        // task input or captured policy payload.
        let admission_matches: bool = db.conn().query_row("SELECT EXISTS(SELECT 1 FROM bot_admissions WHERE run_id=?1 AND user_scope=?2 AND device_id=?3 AND bot_id=?4)",params![key.run.to_storage(),owner.user_scope.to_storage(),owner.device.to_storage(),target.bot.to_storage()],|row|row.get(0)).map_err(sql)?;
        if record.lease_state != NativeLeaseState::Held
            || target.owner != owner
            || bot.is_some_and(|b| b != target.bot)
            || !admission_matches
        {
            return Err(corrupt("drain_owner"));
        }
        targets.push(target);
    }
    let next =
        (targets.len() == usize::from(limit)).then(|| targets.last().unwrap().cursor.clone());
    Ok(NativePendingEffects { targets, next })
}

pub(super) const OWNER_QUERY: &str = "SELECT run_id,operation_id,attempt_id,generation,turn_id FROM native_result_intents WHERE lease_user_scope=?1 AND lease_device_id=?2 AND lease_state='held' AND (created_at_ms,run_id,operation_id)>(?3,?4,?5) ORDER BY created_at_ms,run_id,operation_id LIMIT ?6";
pub(super) const BOT_QUERY: &str = "SELECT run_id,operation_id,attempt_id,generation,turn_id FROM native_result_intents WHERE lease_user_scope=?1 AND lease_device_id=?2 AND lease_bot_id=?3 AND lease_state='held' AND (created_at_ms,run_id,operation_id)>(?4,?5,?6) ORDER BY created_at_ms,run_id,operation_id LIMIT ?7";

pub(super) fn confirm(
    tx: &rusqlite::Transaction<'_>,
    record: &NativeIntentRecord,
    receipt: &NativeEffectDrainReceipt,
    now: i64,
) -> Result<(), BotLedgerError> {
    let expected = record.pending_effect()?.binding;
    if !receipt.binding.valid() || receipt.binding != expected {
        return Err(BotLedgerError::Conflict);
    }
    if let Some(old) = &record.drain {
        return if old == receipt {
            Ok(())
        } else {
            Err(BotLedgerError::Conflict)
        };
    }
    // A racing drain cannot erase already precise result evidence or turn a
    // definitely refused operation into an unknown one.
    if record.lease_state != NativeLeaseState::Held {
        return Err(BotLedgerError::Conflict);
    }
    tx.execute("UPDATE native_result_intents SET stage='settled_unknown',lease_state='settled',effect_fenced_at_ms=?3,effect_drained_at_ms=?4,updated_at_ms=?5 WHERE run_id=?1 AND operation_id=?2",params![record.key.run.to_storage(),record.key.operation_id.to_string(),receipt.fenced_at.millis(),receipt.drained_at.millis(),now]).map_err(sql)?;
    Ok(())
}
