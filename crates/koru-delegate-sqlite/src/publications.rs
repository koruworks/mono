// SPDX-License-Identifier: MPL-2.0

//! The pending-publication list: §7.1's retry queue, §17.1's second step.
//!
//! A lifecycle fact, an artifact-registration reply or a usage fact is
//! committed here **before** it is published, under the stable `EventId`
//! the journal dedupes on. A lost acknowledgement is answered by retrying
//! the same id; a retried `enqueue` finds its row; an acknowledged row
//! stays, because a row is a fact. It is a retry queue, not a second audit
//! service: nothing reads it to say what happened, only to say what has
//! not yet been said.
//!
//! **The body is content.** It comes back only inside [`PublicationBody`],
//! whose `Debug` prints its length and never its text.
//!
//! The one thing read here about what *has* been said is retry identity:
//! [`RunsDb::publication`] and [`RunsDb::publications`] answer a row's
//! `EventId` and the moment it was queued, acknowledged or not, so a
//! re-publication after a restart — and every later row of a run, which
//! correlates to the id its `created` fact was published under — carries
//! the identity the first delivery carried rather than a fresh one.
//!
//! Queue order is insertion order (`rowid`), never UUID order or wall time:
//! multiple facts can share a clock tick, and the clock can move backwards.
//! Rows are retained after acknowledgement; rowids are not exposed as identity.

use std::fmt;

use koru_delegate_core::{EventId, PendingPublication, PublicationKind, RunId};
use rusqlite::{OptionalExtension, params};

use crate::{
    LedgerError, RunsDb, WriteOp, WriteOutcome,
    domains::{parse_publication_kind, publication_token},
    intents::parse_run,
};

/// The envelope a retry re-sends. Redacting `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicationBody(String);

impl PublicationBody {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PublicationBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicationBody(<{} bytes>)", self.0.len())
    }
}

/// One publication not yet acknowledged, with what re-sending it needs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OpenPublication {
    pub event: EventId,
    pub run: RunId,
    pub kind: PublicationKind,
    pub body: PublicationBody,
    /// Delivery attempts so far.
    pub attempts: u32,
    pub queued_at_ms: i64,
}

/// One publication as recorded, acknowledged or not. What a publisher
/// asks when it needs the identity a fact was first sent under.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PublicationRecord {
    pub event: EventId,
    pub run: RunId,
    pub kind: PublicationKind,
    pub body: PublicationBody,
    pub attempts: u32,
    /// The moment the fact was queued — its own moment, because a fact is
    /// queued as it is committed (§17.1).
    pub queued_at_ms: i64,
    pub acknowledged_at_ms: Option<i64>,
}

/// Ephemeral seek position, not a publication identity or a persisted cursor.
/// Event identity detects a deleted rowid that SQLite subsequently reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingPublicationRunCursor {
    rowid: i64,
    ledger_epoch: uuid::Uuid,
    guard: PendingPublicationCursorGuard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingPublicationCursorGuard {
    Event(EventId),
    // A malformed key is never copied or hashed without a byte bound. With no
    // intervening SQLite changes its rowid still denotes the same raw key;
    // any change, including a rolled-back write, makes this cursor stale.
    MalformedEpoch {
        total_changes: i64,
        data_version: i64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingPublicationRunSeek {
    Candidate {
        cursor: PendingPublicationRunCursor,
        run: RunId,
    },
    Malformed {
        cursor: PendingPublicationRunCursor,
    },
    Exhausted,
    StaleCursor,
}

const RECORD_COLUMNS: &str =
    "event_id, run_id, kind, body_json, attempts, queued_at_ms, acknowledged_at_ms";

fn records(
    conn: &rusqlite::Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<PublicationRecord>, LedgerError> {
    let mut statement = conn
        .prepare(sql)
        .map_err(|error| LedgerError::sql("read publications", error))?;
    let rows = statement
        .query_map(params, |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })
        .map_err(|error| LedgerError::sql("read publications", error))?;
    let mut records = Vec::new();
    for row in rows {
        let (event, run, kind, body, attempts, queued_at_ms, acknowledged_at_ms) =
            row.map_err(|error| LedgerError::sql("read publications", error))?;
        records.push(PublicationRecord {
            event: parse_event(&event)?,
            run: parse_run(&run)?,
            kind: parse_publication_kind(&kind)?,
            body: PublicationBody(body),
            attempts: attempts_of(attempts)?,
            queued_at_ms,
            acknowledged_at_ms,
        });
    }
    Ok(records)
}

impl RunsDb {
    /// Snapshot the highest unacknowledged Run key. The partial index keeps
    /// acknowledged history out of this bounded retry walk.
    pub fn pending_publication_run_anchor(
        &self,
    ) -> Result<Option<PendingPublicationRunCursor>, LedgerError> {
        let stamp = publication_change_stamp(self.conn())?;
        let mut query = self.conn().prepare(
            "SELECT rowid,event_id,run_id FROM pending_publications INDEXED BY pending_publications_open_run
             WHERE acknowledged_at_ms IS NULL AND EXISTS(SELECT 1 FROM run_intents i
                 WHERE i.run_id=pending_publications.run_id AND CASE WHEN json_valid(i.payload_json)
                   THEN json_extract(i.payload_json, '$.scope.kind') IN ('none','temporary','project_root','project_folder') ELSE 0 END)
             ORDER BY run_id DESC LIMIT 1",
        ).map_err(|e| LedgerError::sql("anchor pending publication Runs", e))?;
        let mut rows = query
            .query([])
            .map_err(|e| LedgerError::sql("anchor pending publication Runs", e))?;
        rows.next()
            .map_err(|e| LedgerError::sql("anchor pending publication Runs", e))?
            .map(|row| pending_candidate(row, self.publication_cursor_epoch(), stamp))
            .transpose()
            .map(|v| v.map(|(cursor, _)| cursor))
    }

    /// One distinct Run seek. The actor skips at most its bounded owned set
    /// between seeks; SQL does not scan an excluded historical prefix.
    pub fn next_pending_publication_run(
        &self,
        after: Option<PendingPublicationRunCursor>,
        through: PendingPublicationRunCursor,
        before_or_at: Option<PendingPublicationRunCursor>,
    ) -> Result<PendingPublicationRunSeek, LedgerError> {
        let stamp = publication_change_stamp(self.conn())?;
        if !cursor_present(self, through, stamp)?
            || after
                .map(|c| cursor_present(self, c, stamp))
                .transpose()?
                .is_some_and(|v| !v)
            || before_or_at
                .map(|c| cursor_present(self, c, stamp))
                .transpose()?
                .is_some_and(|v| !v)
        {
            return Ok(PendingPublicationRunSeek::StaleCursor);
        }
        // Keep malformed raw keys inside SQLite and choose the tighter wrap
        // bound with two point lookups. An OR-filtered suffix could scan a
        // large out-of-range backlog; the following seek has one exact bound.
        let through = if let Some(other) = before_or_at {
            let left: bool = self
                .conn()
                .query_row(
                    "SELECT (SELECT run_id FROM pending_publications WHERE rowid=?1)
                     <= (SELECT run_id FROM pending_publications WHERE rowid=?2)",
                    params![through.rowid, other.rowid],
                    |r| r.get(0),
                )
                .map_err(|e| LedgerError::sql("bound publication Run wrap", e))?;
            if left { through } else { other }
        } else {
            through
        };
        let query = if after.is_some() {
            "SELECT rowid,event_id,run_id FROM pending_publications INDEXED BY pending_publications_open_run
             WHERE acknowledged_at_ms IS NULL
               AND run_id>(SELECT run_id FROM pending_publications WHERE rowid=?1)
               AND run_id<=(SELECT run_id FROM pending_publications WHERE rowid=?2)
               AND EXISTS (SELECT 1 FROM run_intents i WHERE i.run_id=pending_publications.run_id
                   AND CASE WHEN json_valid(i.payload_json) THEN json_extract(i.payload_json, '$.scope.kind') IN ('none','temporary','project_root','project_folder') ELSE 0 END)
             ORDER BY run_id LIMIT 1"
        } else {
            "SELECT rowid,event_id,run_id FROM pending_publications INDEXED BY pending_publications_open_run
             WHERE acknowledged_at_ms IS NULL
               AND run_id<=(SELECT run_id FROM pending_publications WHERE rowid=?2)
               AND EXISTS (SELECT 1 FROM run_intents i WHERE i.run_id=pending_publications.run_id
                   AND CASE WHEN json_valid(i.payload_json) THEN json_extract(i.payload_json, '$.scope.kind') IN ('none','temporary','project_root','project_folder') ELSE 0 END)
             ORDER BY run_id LIMIT 1"
        };
        let mut statement = self
            .conn()
            .prepare(query)
            .map_err(|e| LedgerError::sql("seek pending publication Run", e))?;
        let mut rows = statement
            .query(params![after.map(|c| c.rowid), through.rowid])
            .map_err(|e| LedgerError::sql("seek pending publication Run", e))?;
        match rows
            .next()
            .map_err(|e| LedgerError::sql("seek pending publication Run", e))?
        {
            Some(row) => {
                let (cursor, run) = pending_candidate(row, self.publication_cursor_epoch(), stamp)?;
                Ok(match run {
                    Some(run) => PendingPublicationRunSeek::Candidate { cursor, run },
                    None => PendingPublicationRunSeek::Malformed { cursor },
                })
            }
            None => Ok(PendingPublicationRunSeek::Exhausted),
        }
    }

    /// Exact per-Run FIFO used by the bounded cross-Run drain. This is not a
    /// claim that old cross-Run rowid FIFO survives keyset scheduling.
    pub fn bounded_open_publications_for(
        &self,
        run: RunId,
        limit: usize,
    ) -> Result<Vec<OpenPublication>, LedgerError> {
        if !(1..=64).contains(&limit) {
            return Err(LedgerError::Corrupt {
                table: "pending_publications",
                column: "limit",
            });
        }
        let mut statement = self.conn().prepare(
            "SELECT CASE WHEN typeof(event_id)='text' AND length(CAST(event_id AS BLOB))=36 THEN event_id END,
                    CASE WHEN typeof(run_id)='text' AND length(CAST(run_id AS BLOB))=36 THEN run_id END,
                    CASE WHEN kind IN ('lifecycle','artifact_registration','usage') THEN kind END,
                    CASE WHEN typeof(body_json)='text' AND length(CAST(body_json AS BLOB)) BETWEEN 1 AND 4194304 THEN body_json END,
                    CASE WHEN typeof(attempts)='integer' THEN attempts END,
                    CASE WHEN typeof(queued_at_ms)='integer' THEN queued_at_ms END
             FROM pending_publications INDEXED BY pending_publications_open_run
             WHERE acknowledged_at_ms IS NULL AND run_id=?1 ORDER BY rowid LIMIT ?2",
        ).map_err(|e| LedgerError::sql("read bounded per-Run publications", e))?;
        let mut cursor = statement
            .query(params![run.to_storage(), limit as i64])
            .map_err(|e| LedgerError::sql("read bounded per-Run publications", e))?;
        let mut result = Vec::new();
        let mut remaining_bytes = 4 * 1024 * 1024;
        while let Some(row) = cursor
            .next()
            .map_err(|e| LedgerError::sql("read bounded per-Run publications", e))?
        {
            let body_len = match row
                .get_ref(3)
                .map_err(|e| LedgerError::sql("read bounded publication body", e))?
            {
                rusqlite::types::ValueRef::Text(bytes) => bytes.len(),
                _ => {
                    return Err(LedgerError::Corrupt {
                        table: "pending_publications",
                        column: "body_json",
                    });
                }
            };
            // Test the borrowed SQLite value before allocating its String.
            // Leave the first non-fitting row at the head of the next exact
            // FIFO read; a short prefix never proves this Run is drained.
            if body_len > remaining_bytes {
                break;
            }
            remaining_bytes -= body_len;
            let read = || -> rusqlite::Result<(String, String, String, String, i64, i64)> {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            };
            let (event, stored_run, kind, body, attempts, queued_at_ms) =
                read().map_err(|_| LedgerError::Corrupt {
                    table: "pending_publications",
                    column: "bounded_columns",
                })?;
            result.push(OpenPublication {
                event: parse_event(&event)?,
                run: parse_run(&stored_run)?,
                kind: parse_publication_kind(&kind)?,
                body: PublicationBody(body),
                attempts: attempts_of(attempts)?,
                queued_at_ms,
            });
        }
        Ok(result)
    }

    /// One publication by its id, acknowledged or not; `None` for an id
    /// never queued.
    pub fn publication(&self, event: EventId) -> Result<Option<PublicationRecord>, LedgerError> {
        Ok(records(
            self.conn(),
            &format!("SELECT {RECORD_COLUMNS} FROM pending_publications WHERE event_id = ?1"),
            params![event.to_string()],
        )?
        .pop())
    }

    /// Every publication of a run, oldest first, acknowledged included —
    /// the first lifecycle row is the `created` fact whose id every later
    /// row of the run correlates to.
    pub fn publications(&self, run: RunId) -> Result<Vec<PublicationRecord>, LedgerError> {
        records(
            self.conn(),
            &format!(
                "SELECT {RECORD_COLUMNS} FROM pending_publications
                  WHERE run_id = ?1
                  ORDER BY rowid"
            ),
            params![run.to_storage()],
        )
    }

    /// Commit a publication before it travels. The event id is the key: a
    /// retry is already applied, whatever it carries, because an event is
    /// one event forever.
    pub fn enqueue(
        &mut self,
        event: EventId,
        run: RunId,
        kind: PublicationKind,
        body_json: &str,
    ) -> Result<WriteOutcome, LedgerError> {
        serde_json::from_str::<serde_json::Value>(body_json)
            .map_err(|_| LedgerError::BodyNotJson)?;
        self.write(WriteOp::Enqueue, |tx, now| {
            let known: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM pending_publications WHERE event_id = ?1",
                    params![event.to_string()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| LedgerError::sql("read a publication", error))?;
            if known.is_some() {
                return Ok(WriteOutcome::AlreadyApplied);
            }
            insert_on(tx, event, run, kind, body_json, now)?;
            Ok(WriteOutcome::Applied)
        })
    }

    /// The unacknowledged publications, oldest first, at most `limit`.
    pub fn open_publications(&self, limit: usize) -> Result<Vec<OpenPublication>, LedgerError> {
        self.open_publications_for(None, limit)
    }

    /// A bounded drain for one run, independent of other runs' backlogs.
    pub fn open_publications_for(
        &self,
        run: Option<RunId>,
        limit: usize,
    ) -> Result<Vec<OpenPublication>, LedgerError> {
        let mut statement = self
            .conn()
            .prepare(
                "SELECT event_id, run_id, kind, body_json, attempts, queued_at_ms
                   FROM pending_publications
                  WHERE acknowledged_at_ms IS NULL
                    AND (?2 IS NULL OR run_id = ?2)
                    AND EXISTS (
                        SELECT 1 FROM run_intents i
                        WHERE i.run_id = pending_publications.run_id
                        AND CASE WHEN json_valid(i.payload_json) THEN json_extract(i.payload_json, '$.scope.kind') IN ('none','temporary','project_root','project_folder') ELSE 0 END
                    )
                  ORDER BY rowid
                  LIMIT ?1",
            )
            .map_err(|error| LedgerError::sql("list open publications", error))?;
        let rows = statement
            .query_map(
                params![
                    i64::try_from(limit).unwrap_or(i64::MAX),
                    run.map(|id| id.to_string())
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .map_err(|error| LedgerError::sql("list open publications", error))?;
        let mut open = Vec::new();
        for row in rows {
            let (event, run, kind, body, attempts, queued_at_ms) =
                row.map_err(|error| LedgerError::sql("list open publications", error))?;
            open.push(OpenPublication {
                event: parse_event(&event)?,
                run: parse_run(&run)?,
                kind: parse_publication_kind(&kind)?,
                body: PublicationBody(body),
                attempts: attempts_of(attempts)?,
                queued_at_ms,
            });
        }
        Ok(open)
    }

    /// The bus, the artifact service or the Run document confirmed it. A
    /// retry after a lost answer is already applied; an id never queued is
    /// [`LedgerError::UnknownPublication`].
    pub fn acknowledge(&mut self, event: EventId) -> Result<WriteOutcome, LedgerError> {
        self.write(WriteOp::Acknowledge, |tx, now| {
            let acknowledged: Option<Option<i64>> = tx
                .query_row(
                    "SELECT acknowledged_at_ms FROM pending_publications WHERE event_id = ?1",
                    params![event.to_string()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| LedgerError::sql("read a publication", error))?;
            match acknowledged {
                None => Err(LedgerError::UnknownPublication),
                Some(Some(_)) => Ok(WriteOutcome::AlreadyApplied),
                Some(None) => {
                    tx.execute(
                        "UPDATE pending_publications SET acknowledged_at_ms = ?2
                          WHERE event_id = ?1",
                        params![event.to_string(), now],
                    )
                    .map_err(|error| LedgerError::sql("acknowledge a publication", error))?;
                    Ok(WriteOutcome::Applied)
                }
            }
        })
    }

    /// One more delivery attempt was made. Returns the new count; the
    /// bound is the caller's policy.
    pub fn bump_attempts(&mut self, event: EventId) -> Result<u32, LedgerError> {
        self.write(WriteOp::BumpAttempts, |tx, _| {
            let changed = tx
                .execute(
                    "UPDATE pending_publications SET attempts = attempts + 1
                      WHERE event_id = ?1",
                    params![event.to_string()],
                )
                .map_err(|error| LedgerError::sql("count a delivery attempt", error))?;
            if changed == 0 {
                return Err(LedgerError::UnknownPublication);
            }
            let attempts: i64 = tx
                .query_row(
                    "SELECT attempts FROM pending_publications WHERE event_id = ?1",
                    params![event.to_string()],
                    |row| row.get(0),
                )
                .map_err(|error| LedgerError::sql("read a publication", error))?;
            attempts_of(attempts)
        })
    }

    /// The run's open publications as the vocabulary crate carries them —
    /// ids, kinds and counts, no body.
    pub fn pending_publications(&self, run: RunId) -> Result<Vec<PendingPublication>, LedgerError> {
        let mut statement = self
            .conn()
            .prepare(
                "SELECT event_id, kind, attempts FROM pending_publications
                  WHERE run_id = ?1 AND acknowledged_at_ms IS NULL
                  ORDER BY rowid",
            )
            .map_err(|error| LedgerError::sql("list open publications", error))?;
        let rows = statement
            .query_map(params![run.to_storage()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|error| LedgerError::sql("list open publications", error))?;
        let mut pending = Vec::new();
        for row in rows {
            let (event, kind, attempts) =
                row.map_err(|error| LedgerError::sql("list open publications", error))?;
            pending.push(PendingPublication {
                event: parse_event(&event)?,
                kind: parse_publication_kind(&kind)?,
                attempts: attempts_of(attempts)?,
            });
        }
        Ok(pending)
    }
}

fn insert_on(
    tx: &rusqlite::Transaction<'_>,
    event: EventId,
    run: RunId,
    kind: PublicationKind,
    body: &str,
    now: i64,
) -> Result<(), LedgerError> {
    tx.execute(
        "INSERT INTO pending_publications(event_id,run_id,kind,body_json,attempts,queued_at_ms)
         VALUES(?1,?2,?3,?4,0,?5)",
        params![
            event.to_string(),
            run.to_storage(),
            publication_token(kind),
            body,
            now
        ],
    )
    .map_err(|e| LedgerError::sql("enqueue a publication", e))?;
    Ok(())
}

fn pending_candidate(
    row: &rusqlite::Row<'_>,
    ledger_epoch: uuid::Uuid,
    stamp: (i64, i64),
) -> Result<(PendingPublicationRunCursor, Option<RunId>), LedgerError> {
    let bad = || LedgerError::Corrupt {
        table: "pending_publications",
        column: "seek_identity",
    };
    let event = match row.get_ref(1).map_err(|_| bad())? {
        rusqlite::types::ValueRef::Text(bytes) if bytes.len() == 36 => std::str::from_utf8(bytes)
            .ok()
            .and_then(|s| parse_event(s).ok()),
        _ => None,
    };
    let cursor = PendingPublicationRunCursor {
        rowid: row.get(0).map_err(|_| bad())?,
        ledger_epoch,
        guard: event.map(PendingPublicationCursorGuard::Event).unwrap_or(
            PendingPublicationCursorGuard::MalformedEpoch {
                total_changes: stamp.0,
                data_version: stamp.1,
            },
        ),
    };
    let run = match row.get_ref(2).map_err(|_| bad())? {
        rusqlite::types::ValueRef::Text(bytes) if bytes.len() == 36 => std::str::from_utf8(bytes)
            .ok()
            .and_then(|s| parse_run(s).ok()),
        _ => None,
    };
    Ok((cursor, if event.is_some() { run } else { None }))
}

fn cursor_present(
    db: &RunsDb,
    cursor: PendingPublicationRunCursor,
    stamp: (i64, i64),
) -> Result<bool, LedgerError> {
    if cursor.ledger_epoch != db.publication_cursor_epoch() {
        return Ok(false);
    }
    let event = match cursor.guard {
        PendingPublicationCursorGuard::Event(event) => Some(event.to_string()),
        PendingPublicationCursorGuard::MalformedEpoch {
            total_changes,
            data_version,
        } => {
            if stamp != (total_changes, data_version) {
                return Ok(false);
            }
            None
        }
    };
    db.conn().query_row(
        "SELECT EXISTS(SELECT 1 FROM pending_publications WHERE rowid=?1 AND (?2 IS NULL OR event_id=?2))",
        params![cursor.rowid,event], |r| r.get(0),
    ).map_err(|e| LedgerError::sql("check publication Run cursor", e))
}

fn publication_change_stamp(conn: &rusqlite::Connection) -> Result<(i64, i64), LedgerError> {
    conn.query_row(
        "SELECT total_changes(),data_version FROM pragma_data_version",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .map_err(|e| LedgerError::sql("observe publication cursor epoch", e))
}

fn parse_event(value: &str) -> Result<EventId, LedgerError> {
    uuid::Uuid::parse_str(value)
        .map(EventId)
        .map_err(|_| LedgerError::Corrupt {
            table: "pending_publications",
            column: "event_id",
        })
}

fn attempts_of(value: i64) -> Result<u32, LedgerError> {
    u32::try_from(value).map_err(|_| LedgerError::Corrupt {
        table: "pending_publications",
        column: "attempts",
    })
}

#[cfg(test)]
mod tests;
