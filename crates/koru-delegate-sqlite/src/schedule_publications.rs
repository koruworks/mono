// SPDX-License-Identifier: MPL-2.0

//! The schedule half of `runs.db`'s one durable publication mechanism.
//!
//! Run publications require a [`koru_delegate_core::RunId`]. Schedule facts
//! cannot: a skipped occurrence creates no Run at all. They therefore live in
//! a separate table with schedule-shaped identity while following the same
//! rules: retain one stable [`EventId`], retry its original envelope, keep an
//! acknowledged row, and never use the optional event journal as authority.
//!
//! There is one extra state boundary. [`RunsDb::prepare_schedule_publication`]
//! commits the event identity and envelope before the authoritative Schedule
//! document or claim mutation. Only after that mutation commits may
//! [`RunsDb::mark_schedule_publication_ready`] expose it to
//! [`RunsDb::ready_schedule_publications`]. A restart reads
//! [`RunsDb::prepared_schedule_publications`], compares each content-free fact
//! with authoritative state, and marks only facts it can prove. Intent is
//! therefore never published as history. If authority conclusively proves the
//! mutation did not land, [`RunsDb::abandon_schedule_publication`] removes only
//! that unconfirmed intent; a ready or acknowledged fact is never removable.
//!
//! **The body is content.** It is returned only to the publisher through
//! [`SchedulePublicationBody`], whose `Debug` prints only its byte length.

use std::fmt;

use koru_delegate_core::{DocumentId, EventId, ScheduleId, WorkspaceId};
use rusqlite::{OptionalExtension, params};

use crate::{LedgerError, RunsDb, WriteOp, WriteOutcome};

/// The identities shared by every event in one schedule's chain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SchedulePublicationRef {
    pub schedule: ScheduleId,
    pub workspace: WorkspaceId,
    pub document: DocumentId,
}

/// The closed schedule-event domain stored by schema 4.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SchedulePublicationKind {
    Created,
    Updated,
    Paused,
    Fired,
    Skipped,
}

impl SchedulePublicationKind {
    pub const ALL: [Self; 5] = [
        Self::Created,
        Self::Updated,
        Self::Paused,
        Self::Fired,
        Self::Skipped,
    ];

    pub fn token(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Paused => "paused",
            Self::Fired => "fired",
            Self::Skipped => "skipped",
        }
    }

    fn parse(token: &str) -> Result<Self, LedgerError> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.token() == token)
            .ok_or(LedgerError::Corrupt {
                table: "schedule_publications",
                column: "kind",
            })
    }

    fn is_revision_fact(self) -> bool {
        matches!(self, Self::Created | Self::Updated | Self::Paused)
    }
}

/// A typed fact to prepare. The enum makes the occurrence/nullability rule
/// structural in Rust; schema 4 enforces the same rule for every writer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SchedulePublicationFact {
    Created { revision: u32 },
    Updated { revision: u32 },
    Paused { revision: u32 },
    Fired { revision: u32, occurrence_ms: i64 },
    Skipped { revision: u32, occurrence_ms: i64 },
}

impl SchedulePublicationFact {
    pub fn kind(self) -> SchedulePublicationKind {
        match self {
            Self::Created { .. } => SchedulePublicationKind::Created,
            Self::Updated { .. } => SchedulePublicationKind::Updated,
            Self::Paused { .. } => SchedulePublicationKind::Paused,
            Self::Fired { .. } => SchedulePublicationKind::Fired,
            Self::Skipped { .. } => SchedulePublicationKind::Skipped,
        }
    }

    pub fn revision(self) -> u32 {
        match self {
            Self::Created { revision }
            | Self::Updated { revision }
            | Self::Paused { revision }
            | Self::Fired { revision, .. }
            | Self::Skipped { revision, .. } => revision,
        }
    }

    pub fn occurrence_ms(self) -> Option<i64> {
        match self {
            Self::Created { .. } | Self::Updated { .. } | Self::Paused { .. } => None,
            Self::Fired { occurrence_ms, .. } | Self::Skipped { occurrence_ms, .. } => {
                Some(occurrence_ms)
            }
        }
    }

    fn from_columns(
        kind: SchedulePublicationKind,
        revision: i64,
        occurrence_ms: Option<i64>,
    ) -> Result<Self, LedgerError> {
        let revision = u32::try_from(revision)
            .ok()
            .filter(|revision| *revision > 0)
            .ok_or(LedgerError::Corrupt {
                table: "schedule_publications",
                column: "revision",
            })?;
        match (kind, occurrence_ms) {
            (SchedulePublicationKind::Created, None) => Ok(Self::Created { revision }),
            (SchedulePublicationKind::Updated, None) => Ok(Self::Updated { revision }),
            (SchedulePublicationKind::Paused, None) => Ok(Self::Paused { revision }),
            (SchedulePublicationKind::Fired, Some(occurrence_ms)) => Ok(Self::Fired {
                revision,
                occurrence_ms,
            }),
            (SchedulePublicationKind::Skipped, Some(occurrence_ms)) => Ok(Self::Skipped {
                revision,
                occurrence_ms,
            }),
            (_, _) => Err(LedgerError::Corrupt {
                table: "schedule_publications",
                column: "occurrence_ms",
            }),
        }
    }
}

/// The envelope a schedule retry re-sends. Redacting `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct SchedulePublicationBody(String);

impl SchedulePublicationBody {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SchedulePublicationBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SchedulePublicationBody(<{} bytes>)", self.0.len())
    }
}

/// One schedule publication as recorded, whether prepared, ready or acknowledged.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SchedulePublicationRecord {
    pub event: EventId,
    pub schedule: SchedulePublicationRef,
    pub fact: SchedulePublicationFact,
    pub body: SchedulePublicationBody,
    pub attempts: u32,
    pub queued_at_ms: i64,
    pub ready_at_ms: Option<i64>,
    pub acknowledged_at_ms: Option<i64>,
}

/// Stable position in the prepared queue. The partial index orders on this
/// exact pair, so a bounded reconciler can rotate past authority-ambiguous
/// rows without adding mutable queue state or scanning acknowledged history.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SchedulePublicationCursor {
    pub queued_at_ms: i64,
    pub event: EventId,
}

impl From<&SchedulePublicationRecord> for SchedulePublicationCursor {
    fn from(record: &SchedulePublicationRecord) -> Self {
        Self {
            queued_at_ms: record.queued_at_ms,
            event: record.event,
        }
    }
}

struct SchedulePublicationState {
    ready_at_ms: Option<i64>,
    acknowledged_at_ms: Option<i64>,
}

/// The durable row returned from prepare, and whether this call inserted it.
/// On a semantic retry the already-stored EventId and body are returned, so a
/// caller never regenerates either.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SchedulePublicationPreparation {
    pub outcome: WriteOutcome,
    pub publication: SchedulePublicationRecord,
}

const COLUMNS: &str = "event_id, schedule_id, workspace_id, schedule_document_id, kind, \
                       revision, occurrence_ms, body_json, attempts, queued_at_ms, \
                       ready_at_ms, acknowledged_at_ms";

impl RunsDb {
    /// Persist an event identity and its exact envelope before the authoritative
    /// mutation. Retrying the same EventId is idempotent and preserves the first
    /// body. A different EventId for the same revision or occurrence is refused:
    /// restart reconciliation must reuse the identity already on disk.
    pub fn prepare_schedule_publication(
        &mut self,
        event: EventId,
        schedule: SchedulePublicationRef,
        fact: SchedulePublicationFact,
        body_json: &str,
    ) -> Result<SchedulePublicationPreparation, LedgerError> {
        if fact.revision() == 0 {
            return Err(LedgerError::RevisionZero);
        }
        if matches!(fact, SchedulePublicationFact::Created { revision } if revision != 1) {
            return Err(LedgerError::InvalidSchedulePublicationFact);
        }
        serde_json::from_str::<serde_json::Value>(body_json)
            .map_err(|_| LedgerError::BodyNotJson)?;
        let outcome = self.write(WriteOp::PrepareSchedulePublication, |tx, now| {
            if let Some(existing) = publication_ref_for_schedule(tx, schedule.schedule)?
                && existing != schedule
            {
                return Err(LedgerError::SchedulePublicationConflict);
            }
            if let Some(existing) = publication_for_fact(tx, schedule.schedule, fact)? {
                return if existing.schedule == schedule
                    && existing.fact == fact
                    && existing.body.as_str() == body_json
                {
                    Ok(WriteOutcome::AlreadyApplied)
                } else {
                    Err(LedgerError::SchedulePublicationConflict)
                };
            }
            if schedule_publication_exists(tx, event)? {
                return Err(LedgerError::SchedulePublicationConflict);
            }
            tx.execute(
                "INSERT INTO schedule_publications
                   (event_id, schedule_id, workspace_id, schedule_document_id, kind,
                    revision, occurrence_ms, body_json, attempts, queued_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9)",
                params![
                    event.to_string(),
                    schedule.schedule.to_storage(),
                    schedule.workspace.to_storage(),
                    schedule.document.to_storage(),
                    fact.kind().token(),
                    i64::from(fact.revision()),
                    fact.occurrence_ms(),
                    body_json,
                    now,
                ],
            )
            .map_err(|error| LedgerError::sql("prepare a schedule publication", error))?;
            Ok(WriteOutcome::Applied)
        })?;
        let publication = match outcome {
            WriteOutcome::Applied => {
                self.schedule_publication(event)?
                    .ok_or(LedgerError::Corrupt {
                        table: "schedule_publications",
                        column: "event_id",
                    })?
            }
            WriteOutcome::AlreadyApplied => self
                .schedule_publication_for_fact(schedule.schedule, fact)?
                .ok_or(LedgerError::Corrupt {
                    table: "schedule_publications",
                    column: "event_id",
                })?,
        };
        Ok(SchedulePublicationPreparation {
            outcome,
            publication,
        })
    }

    /// Confirm that the authoritative mutation committed. Only ready rows are
    /// visible to the publisher. A lost reply is answered idempotently.
    pub fn mark_schedule_publication_ready(
        &mut self,
        event: EventId,
    ) -> Result<WriteOutcome, LedgerError> {
        self.write(WriteOp::MarkSchedulePublicationReady, |tx, now| {
            let state = schedule_publication_state(tx, event)?;
            match state {
                None => Err(LedgerError::UnknownPublication),
                Some(SchedulePublicationState {
                    ready_at_ms: Some(_),
                    ..
                }) => Ok(WriteOutcome::AlreadyApplied),
                Some(SchedulePublicationState {
                    ready_at_ms: None,
                    acknowledged_at_ms: None,
                }) => {
                    tx.execute(
                        "UPDATE schedule_publications SET ready_at_ms = ?2 WHERE event_id = ?1",
                        params![event.to_string(), now],
                    )
                    .map_err(|error| {
                        LedgerError::sql("mark a schedule publication ready", error)
                    })?;
                    Ok(WriteOutcome::Applied)
                }
                Some(SchedulePublicationState {
                    ready_at_ms: None,
                    acknowledged_at_ms: Some(_),
                }) => Err(LedgerError::Corrupt {
                    table: "schedule_publications",
                    column: "acknowledged_at_ms",
                }),
            }
        })
    }

    /// Remove an unconfirmed preparation after authoritative reconciliation
    /// proved its mutation did not commit. This is the only deletion in this
    /// mechanism: ready and acknowledged rows are facts and are refused.
    /// Repeating cleanup after a lost reply is idempotent.
    pub fn abandon_schedule_publication(
        &mut self,
        event: EventId,
    ) -> Result<WriteOutcome, LedgerError> {
        self.write(
            WriteOp::AbandonSchedulePublication,
            |tx, _| match schedule_publication_state(tx, event)? {
                None => Ok(WriteOutcome::AlreadyApplied),
                Some(SchedulePublicationState {
                    ready_at_ms: Some(_),
                    ..
                }) => Err(LedgerError::PublicationAlreadyReady),
                Some(SchedulePublicationState {
                    ready_at_ms: None,
                    acknowledged_at_ms: Some(_),
                }) => Err(LedgerError::Corrupt {
                    table: "schedule_publications",
                    column: "acknowledged_at_ms",
                }),
                Some(SchedulePublicationState {
                    ready_at_ms: None,
                    acknowledged_at_ms: None,
                }) => {
                    tx.execute(
                        "DELETE FROM schedule_publications WHERE event_id = ?1",
                        params![event.to_string()],
                    )
                    .map_err(|error| LedgerError::sql("abandon a schedule publication", error))?;
                    Ok(WriteOutcome::Applied)
                }
            },
        )
    }

    /// One row by stable EventId, in any state.
    pub fn schedule_publication(
        &self,
        event: EventId,
    ) -> Result<Option<SchedulePublicationRecord>, LedgerError> {
        Ok(read_records(
            self.conn(),
            &format!("SELECT {COLUMNS} FROM schedule_publications WHERE event_id = ?1"),
            params![event.to_string()],
        )?
        .pop())
    }

    /// All retained rows for one schedule, including acknowledgements. This is
    /// how a restart recovers the chain root and the EventId of a known fact.
    pub fn schedule_publications(
        &self,
        schedule: ScheduleId,
    ) -> Result<Vec<SchedulePublicationRecord>, LedgerError> {
        read_records(
            self.conn(),
            &format!(
                "SELECT {COLUMNS} FROM schedule_publications
                  WHERE schedule_id = ?1
                  ORDER BY queued_at_ms, event_id"
            ),
            params![schedule.to_storage()],
        )
    }

    /// The retained row that already owns one semantic revision or occurrence.
    /// A restart asks this before constructing an envelope, which is how legacy
    /// schedules seed one durable Created root and all retries reuse its id.
    pub fn schedule_publication_for_fact(
        &self,
        schedule: ScheduleId,
        fact: SchedulePublicationFact,
    ) -> Result<Option<SchedulePublicationRecord>, LedgerError> {
        publication_for_fact(self.conn(), schedule, fact)
    }

    /// Prepared rows whose authoritative mutation a restart must reconcile,
    /// oldest first and bounded by `limit`.
    pub fn prepared_schedule_publications(
        &self,
        limit: usize,
    ) -> Result<Vec<SchedulePublicationRecord>, LedgerError> {
        self.prepared_schedule_publications_after(None, limit)
    }

    /// A bounded page of prepared rows strictly after `cursor`. Callers wrap
    /// to `None` at the end; retaining that cursor in memory is sufficient
    /// because every restart begins a new fair rotation from the oldest row.
    pub fn prepared_schedule_publications_after(
        &self,
        cursor: Option<SchedulePublicationCursor>,
        limit: usize,
    ) -> Result<Vec<SchedulePublicationRecord>, LedgerError> {
        match cursor {
            Some(cursor) => read_records(
                self.conn(),
                &format!(
                    "SELECT {COLUMNS} FROM schedule_publications
                      WHERE ready_at_ms IS NULL
                        AND (queued_at_ms > ?1
                          OR (queued_at_ms = ?1 AND event_id > ?2))
                      ORDER BY queued_at_ms, event_id
                      LIMIT ?3"
                ),
                params![
                    cursor.queued_at_ms,
                    cursor.event.to_string(),
                    limit_i64(limit)
                ],
            ),
            None => read_records(
                self.conn(),
                &format!(
                    "SELECT {COLUMNS} FROM schedule_publications
                      WHERE ready_at_ms IS NULL
                      ORDER BY queued_at_ms, event_id
                      LIMIT ?1"
                ),
                params![limit_i64(limit)],
            ),
        }
    }

    /// Ready and unacknowledged rows the publisher retries, oldest first and
    /// bounded by `limit`. A child is withheld until this schedule's Created
    /// root is acknowledged, even when timestamps tie or the root is still
    /// prepared. The stored body and EventId are returned byte-for-byte.
    pub fn ready_schedule_publications(
        &self,
        limit: usize,
    ) -> Result<Vec<SchedulePublicationRecord>, LedgerError> {
        self.ready_schedule_publications_after(None, limit)
    }

    /// Continue the ready queue past retained, unavailable schedule records.
    /// The ordering remains ready time, queued time, then event identity.
    pub fn ready_schedule_publications_after(
        &self,
        after: Option<(i64, i64, EventId)>,
        limit: usize,
    ) -> Result<Vec<SchedulePublicationRecord>, LedgerError> {
        read_records(
            self.conn(),
            &format!(
                "SELECT {COLUMNS} FROM schedule_publications AS publication
                  WHERE publication.ready_at_ms IS NOT NULL
                    AND publication.acknowledged_at_ms IS NULL
                    AND (?2 IS NULL OR (publication.ready_at_ms, publication.queued_at_ms, publication.event_id) > (?2, ?3, ?4))
                    AND (
                      publication.kind = 'created'
                      OR EXISTS (
                        SELECT 1 FROM schedule_publications AS root
                         WHERE root.schedule_id = publication.schedule_id
                           AND root.kind = 'created'
                           AND root.acknowledged_at_ms IS NOT NULL
                      )
                    )
                  ORDER BY publication.ready_at_ms, publication.queued_at_ms,
                           publication.event_id
                  LIMIT ?1"
            ),
            params![limit_i64(limit), after.map(|v| v.0), after.map(|v| v.1), after.map(|v| v.2.to_string())],
        )
    }

    /// Record one delivery attempt against a ready, open row. The body and
    /// EventId never change; the retry policy remains the publisher's.
    pub fn bump_schedule_publication_attempts(
        &mut self,
        event: EventId,
    ) -> Result<u32, LedgerError> {
        self.write(WriteOp::BumpSchedulePublicationAttempts, |tx, _| {
            let state = schedule_publication_state(tx, event)?;
            match state {
                None => return Err(LedgerError::UnknownPublication),
                Some(SchedulePublicationState {
                    ready_at_ms: None, ..
                }) => return Err(LedgerError::PublicationNotReady),
                Some(SchedulePublicationState {
                    ready_at_ms: Some(_),
                    acknowledged_at_ms: Some(_),
                }) => {
                    return Err(LedgerError::PublicationAlreadyAcknowledged);
                }
                Some(SchedulePublicationState {
                    ready_at_ms: Some(_),
                    acknowledged_at_ms: None,
                }) => {}
            }
            tx.execute(
                "UPDATE schedule_publications SET attempts = attempts + 1 WHERE event_id = ?1",
                params![event.to_string()],
            )
            .map_err(|error| LedgerError::sql("count a schedule delivery attempt", error))?;
            let attempts: i64 = tx
                .query_row(
                    "SELECT attempts FROM schedule_publications WHERE event_id = ?1",
                    params![event.to_string()],
                    |row| row.get(0),
                )
                .map_err(|error| LedgerError::sql("read schedule delivery attempts", error))?;
            parse_attempts(attempts)
        })
    }

    /// The event journal confirmed the ready fact. A lost acknowledgement is
    /// idempotent; intent that was never made ready cannot be acknowledged.
    pub fn acknowledge_schedule_publication(
        &mut self,
        event: EventId,
    ) -> Result<WriteOutcome, LedgerError> {
        self.write(WriteOp::AcknowledgeSchedulePublication, |tx, now| {
            let state = schedule_publication_state(tx, event)?;
            match state {
                None => Err(LedgerError::UnknownPublication),
                Some(SchedulePublicationState {
                    ready_at_ms: None, ..
                }) => Err(LedgerError::PublicationNotReady),
                Some(SchedulePublicationState {
                    ready_at_ms: Some(_),
                    acknowledged_at_ms: Some(_),
                }) => Ok(WriteOutcome::AlreadyApplied),
                Some(SchedulePublicationState {
                    ready_at_ms: Some(_),
                    acknowledged_at_ms: None,
                }) => {
                    tx.execute(
                        "UPDATE schedule_publications SET acknowledged_at_ms = ?2
                          WHERE event_id = ?1",
                        params![event.to_string(), now],
                    )
                    .map_err(|error| {
                        LedgerError::sql("acknowledge a schedule publication", error)
                    })?;
                    Ok(WriteOutcome::Applied)
                }
            }
        })
    }
}

fn schedule_publication_exists(
    conn: &rusqlite::Connection,
    event: EventId,
) -> Result<bool, LedgerError> {
    conn.query_row(
        "SELECT 1 FROM schedule_publications WHERE event_id = ?1",
        params![event.to_string()],
        |_| Ok(()),
    )
    .optional()
    .map(|row| row.is_some())
    .map_err(|error| LedgerError::sql("read a schedule publication", error))
}

fn publication_ref_for_schedule(
    conn: &rusqlite::Connection,
    schedule: ScheduleId,
) -> Result<Option<SchedulePublicationRef>, LedgerError> {
    let stored = conn
        .query_row(
            "SELECT workspace_id, schedule_document_id
               FROM schedule_publications WHERE schedule_id = ?1
               ORDER BY queued_at_ms, event_id LIMIT 1",
            params![schedule.to_storage()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| LedgerError::sql("read a schedule publication subject", error))?;
    stored
        .map(|(workspace, document)| {
            Ok(SchedulePublicationRef {
                schedule,
                workspace: parse_workspace(&workspace)?,
                document: parse_document(&document)?,
            })
        })
        .transpose()
}

fn publication_for_fact(
    conn: &rusqlite::Connection,
    schedule: ScheduleId,
    fact: SchedulePublicationFact,
) -> Result<Option<SchedulePublicationRecord>, LedgerError> {
    let mut records = (if fact.kind() == SchedulePublicationKind::Created {
        read_records(
            conn,
            &format!(
                "SELECT {COLUMNS} FROM schedule_publications
                  WHERE schedule_id = ?1 AND kind = 'created'"
            ),
            params![schedule.to_storage()],
        )
    } else if fact.kind().is_revision_fact() {
        read_records(
            conn,
            &format!(
                "SELECT {COLUMNS} FROM schedule_publications
              WHERE schedule_id = ?1 AND revision = ?2
                    AND kind IN ('updated', 'paused')"
            ),
            params![schedule.to_storage(), i64::from(fact.revision())],
        )
    } else {
        read_records(
            conn,
            &format!(
                "SELECT {COLUMNS} FROM schedule_publications
              WHERE schedule_id = ?1 AND occurrence_ms = ?2
                    AND kind IN ('fired', 'skipped')"
            ),
            params![schedule.to_storage(), fact.occurrence_ms()],
        )
    })?;
    Ok(records.pop())
}

fn schedule_publication_state(
    conn: &rusqlite::Connection,
    event: EventId,
) -> Result<Option<SchedulePublicationState>, LedgerError> {
    conn.query_row(
        "SELECT ready_at_ms, acknowledged_at_ms
           FROM schedule_publications WHERE event_id = ?1",
        params![event.to_string()],
        |row| {
            Ok(SchedulePublicationState {
                ready_at_ms: row.get(0)?,
                acknowledged_at_ms: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(|error| LedgerError::sql("read a schedule publication state", error))
}

fn read_records(
    conn: &rusqlite::Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<SchedulePublicationRecord>, LedgerError> {
    let mut statement = conn
        .prepare(sql)
        .map_err(|error| LedgerError::sql("read schedule publications", error))?;
    let rows = statement
        .query_map(params, |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, Option<i64>>(11)?,
            ))
        })
        .map_err(|error| LedgerError::sql("read schedule publications", error))?;
    let mut records = Vec::new();
    for row in rows {
        let (
            event,
            schedule,
            workspace,
            document,
            kind,
            revision,
            occurrence_ms,
            body,
            attempts,
            queued_at_ms,
            ready_at_ms,
            acknowledged_at_ms,
        ) = row.map_err(|error| LedgerError::sql("read schedule publications", error))?;
        let kind = SchedulePublicationKind::parse(&kind)?;
        records.push(SchedulePublicationRecord {
            event: parse_event(&event)?,
            schedule: SchedulePublicationRef {
                schedule: parse_schedule(&schedule)?,
                workspace: parse_workspace(&workspace)?,
                document: parse_document(&document)?,
            },
            fact: SchedulePublicationFact::from_columns(kind, revision, occurrence_ms)?,
            body: SchedulePublicationBody(body),
            attempts: parse_attempts(attempts)?,
            queued_at_ms,
            ready_at_ms,
            acknowledged_at_ms,
        });
    }
    Ok(records)
}

fn parse_event(value: &str) -> Result<EventId, LedgerError> {
    uuid::Uuid::parse_str(value)
        .map(EventId)
        .map_err(|_| corrupt("event_id"))
}

fn parse_schedule(value: &str) -> Result<ScheduleId, LedgerError> {
    value.parse().map_err(|_| corrupt("schedule_id"))
}

fn parse_workspace(value: &str) -> Result<WorkspaceId, LedgerError> {
    WorkspaceId::parse(value).ok_or_else(|| corrupt("workspace_id"))
}

fn parse_document(value: &str) -> Result<DocumentId, LedgerError> {
    DocumentId::parse(value).ok_or_else(|| corrupt("schedule_document_id"))
}

fn parse_attempts(value: i64) -> Result<u32, LedgerError> {
    u32::try_from(value).map_err(|_| corrupt("attempts"))
}

fn corrupt(column: &'static str) -> LedgerError {
    LedgerError::Corrupt {
        table: "schedule_publications",
        column,
    }
}

fn limit_i64(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests;
