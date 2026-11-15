// SPDX-License-Identifier: MPL-2.0

//! Payload-free restart discovery and conservative release of unused capacity.
use super::*;
use koru_delegate_core::{BOT_BOOT_PAGE_LIMIT, BotBootIdentity, BotBootPage};

pub(super) fn owned_page(
    db: &RunsDb,
    device: DeviceId,
    after: u64,
    through: Option<u64>,
) -> Result<BotBootPage, BotLedgerError> {
    let conn = db.conn();
    let through = match through {
        Some(value) => value,
        None => {
            let mut value = 0;
            for state in ["reserved", "executing"] {
                value = value.max(conn.query_row("SELECT COALESCE(MAX(queue_sequence),0) FROM bot_admissions INDEXED BY bot_admissions_queue WHERE state=?1 AND device_id=?2",
                    params![state,device.to_storage()], |r| r.get::<_,u64>(0)).map_err(sql)?);
            }
            value
        }
    };
    if after > through || through > i64::MAX as u64 {
        return Err(BotLedgerError::Invalid);
    }
    let mut entries = Vec::new();
    for state in ["reserved", "executing"] {
        let mut statement=conn.prepare("SELECT
            CASE WHEN typeof(run_id)='text' AND length(CAST(run_id AS BLOB))=36 THEN run_id END,
            CASE WHEN typeof(bot_id)='text' AND length(CAST(bot_id AS BLOB))=36 THEN bot_id END,
            CASE WHEN typeof(user_scope)='text' AND length(CAST(user_scope AS BLOB))=36 THEN user_scope END,
            CASE WHEN typeof(queue_sequence)='integer' AND queue_sequence>0 THEN queue_sequence END
            FROM bot_admissions INDEXED BY bot_admissions_queue WHERE state=?1 AND device_id=?2 AND queue_sequence>?3 AND queue_sequence<=?4 ORDER BY queue_sequence LIMIT ?5").map_err(sql)?;
        let mut rows = statement
            .query(params![
                state,
                device.to_storage(),
                after,
                through,
                BOT_BOOT_PAGE_LIMIT + 1
            ])
            .map_err(sql)?;
        while let Some(row) = rows.next().map_err(sql)? {
            let owner = BotOwner {
                device,
                user_scope: serde_json::from_value(row.get::<_, String>(2).map_err(sql)?.into())
                    .map_err(|_| corrupt("user_scope"))?,
            };
            entries.push(BotBootIdentity {
                owner,
                run: id(&row.get::<_, String>(0).map_err(sql)?, "run_id")?,
                bot: id(&row.get::<_, String>(1).map_err(sql)?, "bot_id")?,
                sequence: row.get(3).map_err(sql)?,
            });
        }
    }
    entries.sort_unstable_by_key(|entry| entry.sequence);
    let more = entries.len() > usize::from(BOT_BOOT_PAGE_LIMIT);
    entries.truncate(usize::from(BOT_BOOT_PAGE_LIMIT));
    let scanned_through = if more {
        entries.last().expect("full page").sequence
    } else {
        through
    };
    Ok(BotBootPage {
        through,
        scanned_through,
        entries,
        more,
    })
}

pub(super) fn inventory(
    db: &RunsDb,
    owner: BotOwner,
    after: u64,
    through: Option<u64>,
) -> Result<BotBootPage, BotLedgerError> {
    let conn = db.conn();
    let through = match through {
        Some(through) => through,
        None => {
            let mut through = 0;
            for state in ["queued", "reserved", "executing"] {
                through = through.max(
                    conn.query_row(
                        BOT_BOOT_HIGH_WATER_SQL,
                        params![
                            state,
                            owner.user_scope.to_storage(),
                            owner.device.to_storage()
                        ],
                        |r| r.get::<_, u64>(0),
                    )
                    .map_err(sql)?,
                );
            }
            through
        }
    };
    if after > through || through > i64::MAX as u64 {
        return Err(BotLedgerError::Invalid);
    }
    // Each exact-state seek uses (state, queue_sequence) without a temporary
    // sort. Merge at most 3 * 17 column-only rows, never an entire owner queue.
    let mut statement = conn.prepare(BOT_BOOT_ROWS_SQL).map_err(sql)?;
    let mut entries = Vec::new();
    for state in ["queued", "reserved", "executing"] {
        let rows = statement
            .query_map(
                params![
                    state,
                    owner.user_scope.to_storage(),
                    owner.device.to_storage(),
                    after,
                    through,
                    BOT_BOOT_PAGE_LIMIT + 1
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, u64>(2)?,
                    ))
                },
            )
            .map_err(sql)?;
        for row in rows {
            let (run, bot, sequence) = row.map_err(sql)?;
            entries.push(BotBootIdentity {
                owner,
                bot: id(&bot, "bot_id")?,
                run: id(&run, "run_id")?,
                sequence,
            });
        }
    }
    entries.sort_unstable_by_key(|entry| entry.sequence);
    let more = entries.len() > usize::from(BOT_BOOT_PAGE_LIMIT);
    entries.truncate(usize::from(BOT_BOOT_PAGE_LIMIT));
    let scanned_through = if more {
        entries.last().expect("full page").sequence
    } else {
        through
    };
    Ok(BotBootPage {
        through,
        scanned_through,
        entries,
        more,
    })
}

pub(super) fn has_unsettled(
    db: &RunsDb,
    owner: BotOwner,
    bot: BotId,
) -> Result<bool, BotLedgerError> {
    db.conn()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM bot_admissions WHERE user_scope=?1 AND device_id=?2
         AND bot_id=?3 AND state IN ('queued','reserved','executing'))",
            params![
                owner.user_scope.to_storage(),
                owner.device.to_storage(),
                bot.to_storage()
            ],
            |row| row.get(0),
        )
        .map_err(sql)
        .map_err(Into::into)
}

pub(super) fn recover(db: &mut RunsDb, key: &BotRunKey) -> Result<bool, BotLedgerError> {
    db.write_with_error(WriteOp::RecoverBotReservation, |tx, now| {
        if execution_projection(tx,key.run)?.is_none() { return Ok(false); }
        let record=super::super::routines::native_admission(tx,key.run)?;
        let p = &record.submission.provenance;
        if p.owner != key.owner || p.bot != key.bot { return Ok(false); }
        super::super::routines::execution::retained(tx,&record)?;
        let changed = tx.execute(
            "UPDATE bot_admissions SET state='queued',updated_at_ms=?5
             WHERE run_id=?1 AND user_scope=?2 AND device_id=?3 AND bot_id=?4 AND state='reserved'
             AND active_elapsed_ms=0 AND active_since_ms IS NULL
             AND initial_send_key IS NULL AND initial_send_fingerprint IS NULL
             AND initial_send_attempt_id IS NULL AND initial_send_generation IS NULL AND initial_send_turn_id IS NULL
             AND delivery_outcome='unconfirmed' AND delivery_attempt_id IS NULL AND delivery_generation IS NULL
             AND delivery_chat_id IS NULL AND delivery_ai_attempt_id IS NULL AND delivery_assistant_message_id IS NULL
             AND delivery_confirmed_at_ms IS NULL
             AND NOT EXISTS(SELECT 1 FROM run_attempts WHERE run_id=?1)
             AND NOT EXISTS(SELECT 1 FROM native_result_intents WHERE run_id=?1)
             AND NOT EXISTS(SELECT 1 FROM bot_wait_runtime WHERE run_id=?1 AND phase<>'settled')
             AND NOT EXISTS(SELECT 1 FROM bot_memory_edit_allocations WHERE run_id=?1)",
            params![key.run.to_storage(), key.owner.user_scope.to_storage(), key.owner.device.to_storage(), key.bot.to_storage(), now],
        ).map_err(sql)?;
        Ok(changed == 1)
    })
}
