// SPDX-License-Identifier: MPL-2.0
use super::{rows::*, *};
use crate::RunsDb;
use rusqlite::params;

/// Page the held owner/Bot index before subject filtering. An empty target page
/// can have a continuation: other subjects still consumed this bounded page.
/// Every candidate is validated, so missing or forged linkage cannot disappear
/// from a Routine drain and manufacture positive absence.
pub(super) fn pending_routine(
    conn: &rusqlite::Connection,
    owner: BotOwner,
    bot: BotId,
    schedule: ScheduleId,
    after: Option<&NativeRecoveryCursor>,
    limit: u16,
) -> Result<NativePendingEffects, BotLedgerError> {
    if !(RoutineSlot {
        owner,
        bot,
        schedule,
        occurrence_ms: 0,
    })
    .valid()
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
    let mut statement = conn.prepare(BOT_QUERY).map_err(sql)?;
    let keys = statement
        .query_map(
            params![
                owner.user_scope.to_storage(),
                owner.device.to_storage(),
                bot.to_storage(),
                time,
                run,
                operation,
                i64::from(limit)
            ],
            |r| Ok(key(r)),
        )
        .map_err(sql)?
        .map(|r| r.map_err(sql)?)
        .collect::<Result<Vec<_>, _>>()?;
    let full = keys.len() == usize::from(limit);
    let mut targets = Vec::new();
    let mut last = None;
    for key in keys {
        let record = required(conn, &key)?;
        let target = record.pending_effect()?;
        if record.lease_state != NativeLeaseState::Held
            || target.owner != owner
            || target.bot != bot
        {
            return Err(corrupt("drain_owner"));
        }
        last = Some(target.cursor.clone());
        if record
            .lease
            .as_ref()
            .and_then(|l| l.routine.as_ref())
            .is_some_and(|r| r.schedule == schedule)
        {
            targets.push(target);
        }
    }
    Ok(NativePendingEffects {
        targets,
        next: if full { last } else { None },
    })
}

pub(crate) fn held_routine(
    conn: &rusqlite::Connection,
    owner: BotOwner,
    bot: BotId,
    schedule: ScheduleId,
) -> Result<bool, BotLedgerError> {
    let mut after = None;
    loop {
        let page = pending_routine(conn, owner, bot, schedule, after.as_ref(), 64)?;
        if !page.targets.is_empty() {
            return Ok(true);
        }
        match page.next {
            Some(next) => after = Some(next),
            None => return Ok(false),
        }
    }
}

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
        // The full native read above validates retained subject evidence. This
        // final indexed owner check binds discovery to the requested owner.
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
