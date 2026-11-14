// SPDX-License-Identifier: MPL-2.0

//! Bounded read-only facts for later scheduler composition. No execution is enabled.
use super::*;
use crate::{SlotClaim, SlotOutcome, SlotState};
use koru_delegate_core::{RoutineClaimObservation, RoutineClaimRecord, RoutineSlot};

const PAGE: usize = 16;

#[cfg(test)]
pub(super) fn query_shapes() -> [&'static str; 6] {
    [
        HELD_FIRST,
        HELD_AFTER,
        QUEUED_FIRST,
        QUEUED_AFTER,
        SLOTS_FIRST,
        SLOTS_AFTER,
    ]
}

/// Retained-row anchors keep malformed SQL sort text out of Rust allocations.
/// They are process-local traversal cursors, never authority or wire values.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RoutineHeldCursor {
    rowid: i64,
    updated_at_ms: Option<i64>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RoutineQueuedCursor {
    rowid: i64,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RoutineSlotCursor {
    rowid: i64,
}
macro_rules! redacted_cursor {
    ($($t:ty),+) => { $(impl std::fmt::Debug for $t {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(concat!(stringify!($t), "([REDACTED])"))
        }
    })+ };
}
redacted_cursor!(RoutineHeldCursor, RoutineQueuedCursor, RoutineSlotCursor);

#[derive(Debug)]
pub struct RoutinePage<C, T> {
    /// Every physical entry has its own continuation, including corrupt rows.
    pub entries: Vec<(C, Result<T, BotLedgerError>)>,
    /// Continuation after the last returned entry, only when lookahead exists.
    pub next: Option<C>,
}
#[derive(Debug)]
pub struct RoutineRecoveryEntry {
    pub route: RoutineRouteIdentity,
    pub record: RoutineClaimRecord,
}
#[derive(Debug)]
pub struct RoutineOccurrenceFact {
    pub observation: RoutineClaimObservation,
    pub slot: SlotClaim,
}

fn bad(column: &'static str) -> BotLedgerError {
    rows::bad(column)
}

// Cursor subqueries keep even an oversized malformed key in SQLite. Freeze a
// valid Held timestamp so acknowledgement cannot advance a retained anchor.
const HELD_FIRST: &str =
    "SELECT rowid, CASE WHEN typeof(updated_at_ms)='integer' THEN updated_at_ms END,
 CASE WHEN typeof(run_id)='text' AND length(CAST(run_id AS BLOB))=36 THEN run_id END
 FROM bot_routine_links INDEXED BY bot_routine_link_recovery WHERE state='held'
 ORDER BY user_scope,device_id,updated_at_ms,run_id LIMIT 17";
const HELD_AFTER: &str = "SELECT rowid, CASE WHEN typeof(updated_at_ms)='integer' THEN updated_at_ms END,
 CASE WHEN typeof(run_id)='text' AND length(CAST(run_id AS BLOB))=36 THEN run_id END
 FROM bot_routine_links INDEXED BY bot_routine_link_recovery WHERE state='held'
 AND (user_scope,device_id,updated_at_ms,run_id) >
 (SELECT user_scope,device_id,COALESCE(?2,updated_at_ms),run_id FROM bot_routine_links WHERE rowid=?1)
 ORDER BY user_scope,device_id,updated_at_ms,run_id LIMIT 17";
const QUEUED_FIRST: &str = "SELECT rowid,
 CASE WHEN typeof(run_id)='text' AND length(CAST(run_id AS BLOB))=36 THEN run_id END
 ,CASE WHEN typeof(queue_sequence)='integer' AND queue_sequence>0 THEN queue_sequence END
 FROM bot_admissions INDEXED BY bot_admissions_queue WHERE state='queued'
 ORDER BY queue_sequence LIMIT 17";
const QUEUED_AFTER: &str = "SELECT rowid,
 CASE WHEN typeof(run_id)='text' AND length(CAST(run_id AS BLOB))=36 THEN run_id END
 ,CASE WHEN typeof(queue_sequence)='integer' AND queue_sequence>0 THEN queue_sequence END
 FROM bot_admissions INDEXED BY bot_admissions_queue WHERE state='queued'
 AND queue_sequence>(SELECT queue_sequence FROM bot_admissions WHERE rowid=?1)
 ORDER BY queue_sequence LIMIT 17";
const SLOTS_FIRST: &str =
    "SELECT rowid, CASE WHEN typeof(occurrence_ms)='integer' THEN occurrence_ms END
 FROM schedule_claims WHERE schedule_id=?1 ORDER BY occurrence_ms LIMIT 17";
const SLOTS_AFTER: &str =
    "SELECT rowid, CASE WHEN typeof(occurrence_ms)='integer' THEN occurrence_ms END
 FROM schedule_claims WHERE schedule_id=?1
 AND occurrence_ms>(SELECT occurrence_ms FROM schedule_claims WHERE rowid=?2 AND schedule_id=?1)
 ORDER BY occurrence_ms LIMIT 17";

fn anchor(conn: &Connection, query: &str, rowid: i64) -> Result<(), BotLedgerError> {
    let exists: bool = conn.query_row(query, [rowid], |r| r.get(0)).map_err(sql)?;
    if exists {
        Ok(())
    } else {
        Err(bad("recovery_cursor"))
    }
}
fn parse_run(value: Option<String>) -> Result<RunId, BotLedgerError> {
    value
        .ok_or_else(|| bad("run_id"))?
        .parse()
        .map_err(|_| bad("run_id"))
}
fn recovery(
    conn: &Connection,
    run: RunId,
    now: i64,
) -> Result<RoutineRecoveryEntry, BotLedgerError> {
    rows::bounded_admission(conn, run)?;
    let admission = super::super::repository::read(conn, run)?.ok_or_else(|| bad("admission"))?;
    let p = &admission.submission.provenance;
    let routine = p.routine.ok_or_else(|| bad("recovery_provenance"))?;
    let slot = RoutineSlot {
        owner: p.owner,
        bot: p.bot,
        schedule: routine.schedule,
        occurrence_ms: routine.occurrence_ms,
    };
    let record = match rows::observation_with_admission(conn, &slot, now, Some(&admission))? {
        RoutineClaimObservation::Claimed { record } if record.claim.ids.run == run => record,
        _ => return Err(bad("recovery_claim")),
    };
    let route = rows::subject(conn, &slot)?;
    Ok(RoutineRecoveryEntry { route, record })
}

impl RunsDb {
    pub fn held_routine_page(
        &self,
        after: Option<RoutineHeldCursor>,
    ) -> Result<RoutinePage<RoutineHeldCursor, RoutineRecoveryEntry>, BotLedgerError> {
        let tx = self.conn().unchecked_transaction().map_err(sql)?;
        if let Some(c) = after {
            anchor(
                &tx,
                "SELECT EXISTS(SELECT 1 FROM bot_routine_links WHERE rowid=?1)",
                c.rowid,
            )?;
        }
        let query = if after.is_some() {
            HELD_AFTER
        } else {
            HELD_FIRST
        };
        let mut statement = tx.prepare(query).map_err(sql)?;
        let args = after
            .map(|c| {
                vec![
                    rusqlite::types::Value::Integer(c.rowid),
                    c.updated_at_ms.map_or(
                        rusqlite::types::Value::Null,
                        rusqlite::types::Value::Integer,
                    ),
                ]
            })
            .unwrap_or_default();
        let mut found = statement
            .query(rusqlite::params_from_iter(args))
            .map_err(sql)?;
        let mut entries = Vec::with_capacity(PAGE);
        let mut more = false;
        while let Some(r) = found.next().map_err(sql)? {
            if entries.len() == PAGE {
                more = true;
                break;
            }
            let cursor = RoutineHeldCursor {
                rowid: r.get(0).map_err(sql)?,
                updated_at_ms: r.get(1).map_err(sql)?,
            };
            let result = (|| {
                if cursor.updated_at_ms.is_none() {
                    return Err(bad("updated_at_ms"));
                }
                let run = parse_run(r.get(2).map_err(sql)?)?;
                let entry = recovery(&tx, run, self.now_ms())?;
                if entry.record.link_state != (koru_delegate_core::RoutineLinkState::Held {}) {
                    return Err(bad("recovery_state"));
                }
                Ok(entry)
            })();
            entries.push((cursor, result));
        }
        let next = more.then(|| entries.last().expect("full page").0);
        drop(found);
        drop(statement);
        tx.commit().map_err(sql)?;
        Ok(RoutinePage { entries, next })
    }

    pub fn queued_routine_page(
        &self,
        after: Option<RoutineQueuedCursor>,
    ) -> Result<RoutinePage<RoutineQueuedCursor, Option<RoutineRecoveryEntry>>, BotLedgerError>
    {
        let tx = self.conn().unchecked_transaction().map_err(sql)?;
        if let Some(c) = after {
            anchor(
                &tx,
                "SELECT EXISTS(SELECT 1 FROM bot_admissions WHERE rowid=?1)",
                c.rowid,
            )?;
        }
        let mut statement = tx
            .prepare(if after.is_some() {
                QUEUED_AFTER
            } else {
                QUEUED_FIRST
            })
            .map_err(sql)?;
        let args = after.map(|c| vec![c.rowid]).unwrap_or_default();
        let mut found = statement
            .query(rusqlite::params_from_iter(args))
            .map_err(sql)?;
        let mut entries = Vec::with_capacity(PAGE);
        let mut more = false;
        while let Some(r) = found.next().map_err(sql)? {
            if entries.len() == PAGE {
                more = true;
                break;
            }
            let cursor = RoutineQueuedCursor {
                rowid: r.get(0).map_err(sql)?,
            };
            let result = (|| {
                if r.get::<_, Option<i64>>(2).map_err(sql)?.is_none() {
                    return Err(bad("queue_sequence"));
                }
                let run = parse_run(r.get(1).map_err(sql)?)?;
                match deny_run(&tx, run) {
                    Ok(()) => Ok(None),
                    Err(LedgerError::InvalidScheduleAuthority) => {
                        recovery(&tx, run, self.now_ms()).map(Some)
                    }
                    Err(error) => Err(error.into()),
                }
            })();
            entries.push((cursor, result));
        }
        let next = more.then(|| entries.last().expect("full page").0);
        drop(found);
        drop(statement);
        tx.commit().map_err(sql)?;
        Ok(RoutinePage { entries, next })
    }

    pub fn routine_slots_page(
        &self,
        route: &RoutineRouteIdentity,
        after: Option<RoutineSlotCursor>,
    ) -> Result<RoutinePage<RoutineSlotCursor, RoutineOccurrenceFact>, BotLedgerError> {
        let tx = self.conn().unchecked_transaction().map_err(sql)?;
        if !route.valid() || read(&tx, route.schedule)?.as_ref() != Some(route) {
            return Err(BotLedgerError::Fenced);
        }
        if let Some(c) = after {
            let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM schedule_claims WHERE rowid=?1 AND schedule_id=?2)",
                params![c.rowid, route.schedule.to_storage()], |r| r.get(0)).map_err(sql)?;
            if !exists {
                return Err(bad("recovery_cursor"));
            }
        }
        let mut statement = tx
            .prepare(if after.is_some() {
                SLOTS_AFTER
            } else {
                SLOTS_FIRST
            })
            .map_err(sql)?;
        let mut args = vec![rusqlite::types::Value::Text(route.schedule.to_storage())];
        if let Some(c) = after {
            args.push(rusqlite::types::Value::Integer(c.rowid));
        }
        let mut found = statement
            .query(rusqlite::params_from_iter(args))
            .map_err(sql)?;
        let mut entries = Vec::with_capacity(PAGE);
        let mut more = false;
        while let Some(r) = found.next().map_err(sql)? {
            if entries.len() == PAGE {
                more = true;
                break;
            }
            let cursor = RoutineSlotCursor {
                rowid: r.get(0).map_err(sql)?,
            };
            let result = (|| {
                let occurrence_ms: i64 = r
                    .get::<_, Option<i64>>(1)
                    .map_err(sql)?
                    .ok_or_else(|| bad("occurrence_ms"))?;
                let identity = RoutineSlot {
                    owner: route.owner,
                    bot: route.bot,
                    schedule: route.schedule,
                    occurrence_ms,
                };
                // Validate/bound the physical cells before the existing ordinary slot decoder.
                let observation = rows::observation(&tx, &identity, self.now_ms())?;
                let slot = self
                    .slot(route.schedule, occurrence_ms)?
                    .ok_or_else(|| bad("slot"))?;
                match &observation {
                    RoutineClaimObservation::Claimed { record }
                        if record.claim.ids.run == slot.run
                            && record.claim.definition.revision == slot.revision
                            && record.claim.claimed_at_ms == slot.claimed_at_ms
                            && matches!(
                                slot.state,
                                SlotState::Claimed
                                    | SlotState::Settled(
                                        SlotOutcome::Fired | SlotOutcome::SkippedRetired
                                    )
                            ) => {}
                    RoutineClaimObservation::Skipped { record }
                        if record.planned_run == slot.run
                            && record.definition_revision == slot.revision
                            && record.recorded_at_ms == slot.claimed_at_ms
                            && match record.outcome {
                                koru_delegate_core::RoutineSkipOutcome::SkippedMissed {} => {
                                    slot.state == SlotState::Settled(SlotOutcome::SkippedMissed)
                                        && slot.busy_holder.is_none()
                                }
                                koru_delegate_core::RoutineSkipOutcome::SkippedBusy { holder } => {
                                    slot.state == SlotState::Settled(SlotOutcome::SkippedBusy)
                                        && slot.busy_holder == Some(holder)
                                }
                            } => {}
                    _ => return Err(bad("slot_identity")),
                }
                Ok(RoutineOccurrenceFact { observation, slot })
            })();
            entries.push((cursor, result));
        }
        let next = more.then(|| entries.last().expect("full page").0);
        drop(found);
        drop(statement);
        tx.commit().map_err(sql)?;
        Ok(RoutinePage { entries, next })
    }
}
