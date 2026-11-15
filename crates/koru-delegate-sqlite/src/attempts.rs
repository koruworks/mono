// SPDX-License-Identifier: MPL-2.0

//! The attempts: one row per generation, and the fence between them.
//!
//! An attempt is how far this machine got with one try at a run. Its phase
//! moves through a closed domain with no `failed` in it (D10), its provider
//! session is bound to it and to no other attempt ever (the key is `UNIQUE`
//! across the table), and **a fresh attempt cannot open while the one before
//! it is unretired**. Retiring is §7.6's fence: it is what authorises a fresh
//! attempt, and a delayed create carrying a retired key is refused by the
//! host. The generation is the counter [`AttemptGeneration::admits`] fences
//! writes with; an older generation, or a newer one the caller invented, is
//! refused before any row is touched.
//!
//! **Bind before you ask.** `bind_session` must be durable before the create
//! is sent to the host: a restart finds the key here and asks the host's
//! non-creating lookup about it. An attempt with no key never asked a host
//! for anything, which is what lets recovery treat it as nothing-running.

use koru_delegate_core::{
    AttemptGeneration, AttemptState, HostInstanceId, IdempotencyKey, ProcessFacts,
    ProviderSessionKey, RunAttemptId, RunId, SessionId,
};
use rusqlite::{OptionalExtension, Row, params};

use crate::{
    AttemptPhase, IntentState, LedgerError, Projection, RunsDb, WriteOp, intents::parse_run,
};

/// One attempt as the ledger holds it. Facts, not health.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AttemptRow {
    pub run: RunId,
    pub attempt: RunAttemptId,
    pub generation: AttemptGeneration,
    pub phase: AttemptPhase,
    /// Bound before the host is asked to create; the session id arrives once
    /// creation completes.
    pub provider_session: Option<ProviderSessionKey>,
    /// The host incarnation the session was asked of (§7.6).
    pub host_instance: Option<HostInstanceId>,
    /// §7.6's fence. A retired attempt accepts no write and authorises the
    /// next generation.
    pub retired: bool,
    pub started_at_ms: Option<i64>,
    pub updated_at_ms: i64,
}

impl AttemptRow {
    /// Unretired and not in a terminal phase: a process may exist for it.
    pub fn is_live(&self) -> bool {
        !self.retired && !self.phase.is_terminal()
    }
}

const COLUMNS: &str = "run_id, generation, attempt_id, state, session_key, session_id, \
                       host_instance_id, retired, started_at_ms, updated_at_ms";

impl RunsDb {
    /// The next generation, `created`, for a run that exists. Refused while
    /// the run is a request ([`LedgerError::RunNotCreated`]) and while the
    /// latest attempt is unretired ([`LedgerError::AttemptLive`]).
    pub fn open_attempt(&mut self, run: RunId) -> Result<AttemptRow, LedgerError> {
        self.write(WriteOp::OpenAttempt, |tx, now| {
            crate::bots::routines::deny_run(tx, run)?;
            open_on(tx, run, now)
        })
    }

    /// Move the current attempt through the closed domain. Moving to the
    /// phase already held is already applied, so a retry after a lost
    /// answer is harmless; any other pair the domain does not list is
    /// refused.
    pub fn advance(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
        phase: AttemptPhase,
    ) -> Result<AttemptRow, LedgerError> {
        self.write(WriteOp::AdvanceAttempt, |tx, now| {
            let attempt = writable(tx, run, generation)?;
            if attempt.phase == phase {
                return Ok(attempt);
            }
            if !attempt.phase.may_advance_to(phase) {
                return Err(LedgerError::IllegalAttemptTransition {
                    from: attempt.phase,
                    to: phase,
                });
            }
            tx.execute(
                "UPDATE run_attempts
                    SET state = ?3,
                        started_at_ms = CASE WHEN ?3 = 'starting'
                                             THEN COALESCE(started_at_ms, ?4)
                                             ELSE started_at_ms END,
                        updated_at_ms = ?4
                  WHERE run_id = ?1 AND generation = ?2",
                params![run.to_storage(), generation.get(), phase.token(), now],
            )
            .map_err(|error| LedgerError::sql("advance an attempt", error))?;
            current(tx, run)
        })
    }

    /// Bind the provider session to the current attempt — the idempotency
    /// key before the host is asked, the session id once it answers, the
    /// host incarnation both times. Rebinding the same facts is harmless;
    /// a different key, session or host, or a key another attempt holds, is
    /// [`LedgerError::SessionKeyConflict`].
    pub fn bind_session(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
        session: ProviderSessionKey,
        host: HostInstanceId,
    ) -> Result<AttemptRow, LedgerError> {
        self.write(WriteOp::BindSession, |tx, now| {
            let attempt = writable(tx, run, generation)?;
            if attempt.phase.is_terminal() {
                return Err(LedgerError::AttemptEnded {
                    phase: attempt.phase,
                });
            }
            if let Some(bound) = attempt.provider_session {
                let same_key = bound.idempotency == session.idempotency;
                let same_session = match (bound.session, session.session) {
                    (Some(stored), Some(offered)) => stored == offered,
                    _ => true,
                };
                if !same_key || !same_session || attempt.host_instance != Some(host) {
                    return Err(LedgerError::SessionKeyConflict);
                }
            }
            let held_elsewhere: i64 = tx
                .query_row(
                    "SELECT count(*) FROM run_attempts
                      WHERE session_key = ?1 AND NOT (run_id = ?2 AND generation = ?3)",
                    params![
                        session.idempotency.to_string(),
                        run.to_storage(),
                        generation.get()
                    ],
                    |row| row.get(0),
                )
                .map_err(|error| LedgerError::sql("check a session key", error))?;
            if held_elsewhere > 0 {
                return Err(LedgerError::SessionKeyConflict);
            }
            tx.execute(
                "UPDATE run_attempts
                    SET session_key = ?3,
                        session_id = COALESCE(?4, session_id),
                        host_instance_id = ?5,
                        updated_at_ms = ?6
                  WHERE run_id = ?1 AND generation = ?2",
                params![
                    run.to_storage(),
                    generation.get(),
                    session.idempotency.to_string(),
                    session.session.map(|id| id.to_string()),
                    host.as_uuid().to_string(),
                    now
                ],
            )
            .map_err(|error| LedgerError::sql("bind a session", error))?;
            current(tx, run)
        })
    }

    /// §7.6's fence. Only a terminal attempt retires; retiring twice is
    /// harmless. After this the next generation may open.
    pub fn retire(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
    ) -> Result<AttemptRow, LedgerError> {
        self.write(WriteOp::RetireAttempt, |tx, now| {
            let attempt = current(tx, run)?;
            attempt.generation.admits(generation)?;
            if attempt.retired {
                return Ok(attempt);
            }
            if !attempt.phase.is_terminal() {
                return Err(LedgerError::AttemptNotEnded {
                    phase: attempt.phase,
                });
            }
            tx.execute(
                "UPDATE run_attempts SET retired = 1, updated_at_ms = ?3
                  WHERE run_id = ?1 AND generation = ?2",
                params![run.to_storage(), generation.get(), now],
            )
            .map_err(|error| LedgerError::sql("retire an attempt", error))?;
            current(tx, run)
        })
    }

    /// The highest generation, if any.
    pub fn current_attempt(&self, run: RunId) -> Result<Option<AttemptRow>, LedgerError> {
        latest(self.conn(), run)
    }

    /// Every generation of a run, oldest first.
    pub fn attempts(&self, run: RunId) -> Result<Vec<AttemptRow>, LedgerError> {
        query(
            self.conn(),
            &format!("SELECT {COLUMNS} FROM run_attempts WHERE run_id = ?1 ORDER BY generation"),
            params![run.to_storage()],
        )
    }

    /// Every attempt a process may still exist for: unretired and not in a
    /// terminal phase. What P1.5's durable claims and P1.17's diagnostics
    /// read.
    pub fn live_attempts(&self) -> Result<Vec<AttemptRow>, LedgerError> {
        query(
            self.conn(),
            &format!(
                "SELECT {COLUMNS} FROM run_attempts
                  WHERE retired = 0 AND state IN ('created', 'starting', 'running')
                  ORDER BY updated_at_ms, run_id, generation"
            ),
            [],
        )
    }

    /// The current attempt as the vocabulary crate's [`AttemptState`]: the
    /// row, the run's open publications and the header-summary cursor. The
    /// worker's own pid is not stored — a restart is a new process, so the
    /// old pid can only ever name something gone — and reads `None`.
    pub fn attempt_state(&self, run: RunId) -> Result<Option<AttemptState>, LedgerError> {
        let Some(row) = self.current_attempt(run)? else {
            return Ok(None);
        };
        Ok(Some(AttemptState {
            run,
            attempt: row.attempt,
            generation: row.generation,
            started_at_ms: row.started_at_ms,
            provider_session: row.provider_session,
            process: ProcessFacts {
                host_instance: row.host_instance,
                worker_pid: None,
                observed_at_ms: Some(row.updated_at_ms),
            },
            pending_publications: self.pending_publications(run)?,
            projection_cursor: self.cursor(run, Projection::HeaderSummary)?,
        }))
    }
}

/// Shared insertion owner. The public ordinary entry performs marker denial;
/// the Bot entry validates its full subject and current execution proof first.
pub(crate) fn open_on(
    conn: &rusqlite::Connection,
    run: RunId,
    now: i64,
) -> Result<AttemptRow, LedgerError> {
    let state: Option<String> = conn
        .query_row(
            "SELECT state FROM run_intents WHERE run_id=?1",
            [run.to_storage()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| LedgerError::sql("read an intent", error))?;
    if IntentState::parse(&state.ok_or(LedgerError::UnknownRun)?)? != IntentState::Satisfied {
        return Err(LedgerError::RunNotCreated);
    }
    let generation = match latest(conn, run)? {
        Some(previous) if !previous.retired => {
            return Err(LedgerError::AttemptLive {
                generation: previous.generation,
            });
        }
        Some(previous) => previous.generation.next(),
        None => AttemptGeneration::FIRST,
    };
    let attempt = RunAttemptId::mint();
    conn.execute(
        "INSERT INTO run_attempts (run_id,generation,attempt_id,state,updated_at_ms) VALUES(?1,?2,?3,'created',?4)",
        params![run.to_storage(),generation.get(),attempt.to_storage(),now],
    ).map_err(|error| LedgerError::sql("open an attempt", error))?;
    current(conn, run)
}

/// The current attempt, admitting only `generation` and only unretired.
fn writable(
    tx: &rusqlite::Connection,
    run: RunId,
    generation: AttemptGeneration,
) -> Result<AttemptRow, LedgerError> {
    let attempt = current(tx, run)?;
    attempt.generation.admits(generation)?;
    if attempt.retired {
        return Err(LedgerError::AttemptRetired {
            generation: attempt.generation,
        });
    }
    Ok(attempt)
}

fn current(tx: &rusqlite::Connection, run: RunId) -> Result<AttemptRow, LedgerError> {
    latest(tx, run)?.ok_or(LedgerError::NoAttempt)
}

pub(crate) fn latest(
    conn: &rusqlite::Connection,
    run: RunId,
) -> Result<Option<AttemptRow>, LedgerError> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM run_attempts WHERE run_id = ?1
              ORDER BY generation DESC LIMIT 1"
        ),
        params![run.to_storage()],
        from_row,
    )
    .optional()
    .map_err(|error| LedgerError::sql("read an attempt", error))?
    .transpose()
}

/// Exact historical attempt through the same decoder, with raw cells bounded
/// before allocation. Decision origin evidence must not read the newest row.
pub(crate) fn retained_bounded(conn:&rusqlite::Connection,run:RunId,generation:AttemptGeneration)
    ->Result<Option<AttemptRow>,LedgerError>
{
    conn.query_row("SELECT
        CASE WHEN typeof(run_id)='text' AND length(CAST(run_id AS BLOB))=36 THEN run_id END,
        CASE WHEN typeof(generation)='integer' THEN generation END,
        CASE WHEN typeof(attempt_id)='text' AND length(CAST(attempt_id AS BLOB))=36 THEN attempt_id END,
        CASE WHEN typeof(state)='text' AND length(CAST(state AS BLOB))<=32 THEN state END,
        CASE WHEN session_key IS NULL OR (typeof(session_key)='text' AND length(CAST(session_key AS BLOB))=36) THEN session_key ELSE zeroblob(1) END,
        CASE WHEN session_id IS NULL OR (typeof(session_id)='text' AND length(CAST(session_id AS BLOB))=36) THEN session_id ELSE zeroblob(1) END,
        CASE WHEN host_instance_id IS NULL OR (typeof(host_instance_id)='text' AND length(CAST(host_instance_id AS BLOB))=36) THEN host_instance_id ELSE zeroblob(1) END,
        CASE WHEN typeof(retired)='integer' AND retired IN (0,1) THEN retired END,
        CASE WHEN started_at_ms IS NULL OR typeof(started_at_ms)='integer' THEN started_at_ms ELSE zeroblob(1) END,
        CASE WHEN typeof(updated_at_ms)='integer' THEN updated_at_ms END
        FROM run_attempts WHERE run_id=?1 AND generation=?2",params![run.to_storage(),generation.get()],from_row)
        .optional().map_err(|e|LedgerError::sql("read retained bounded attempt",e))?.transpose()
}

fn query<P: rusqlite::Params>(
    conn: &rusqlite::Connection,
    sql: &str,
    params: P,
) -> Result<Vec<AttemptRow>, LedgerError> {
    let mut statement = conn
        .prepare(sql)
        .map_err(|error| LedgerError::sql("list attempts", error))?;
    let rows = statement
        .query_map(params, from_row)
        .map_err(|error| LedgerError::sql("list attempts", error))?;
    rows.map(|row| row.map_err(|error| LedgerError::sql("list attempts", error))?)
        .collect()
}

/// A row to a typed attempt. Each column that fails to parse names itself
/// and never its value.
fn from_row(row: &Row<'_>) -> rusqlite::Result<Result<AttemptRow, LedgerError>> {
    let run: String = row.get(0)?;
    let generation: i64 = row.get(1)?;
    let attempt: String = row.get(2)?;
    let state: String = row.get(3)?;
    let session_key: Option<String> = row.get(4)?;
    let session_id: Option<String> = row.get(5)?;
    let host: Option<String> = row.get(6)?;
    let retired: i64 = row.get(7)?;
    let started_at_ms: Option<i64> = row.get(8)?;
    let updated_at_ms: i64 = row.get(9)?;
    Ok(typed(
        &run,
        generation,
        &attempt,
        &state,
        session_key.as_deref(),
        session_id.as_deref(),
        host.as_deref(),
        retired,
        started_at_ms,
        updated_at_ms,
    ))
}

#[allow(clippy::too_many_arguments)]
fn typed(
    run: &str,
    generation: i64,
    attempt: &str,
    state: &str,
    session_key: Option<&str>,
    session_id: Option<&str>,
    host: Option<&str>,
    retired: i64,
    started_at_ms: Option<i64>,
    updated_at_ms: i64,
) -> Result<AttemptRow, LedgerError> {
    let corrupt = |column: &'static str| LedgerError::Corrupt {
        table: "run_attempts",
        column,
    };
    let idempotency = session_key
        .map(|key| {
            key.parse::<IdempotencyKey>()
                .map_err(|_| corrupt("session_key"))
        })
        .transpose()?;
    let session = session_id
        .map(|id| id.parse::<SessionId>().map_err(|_| corrupt("session_id")))
        .transpose()?;
    Ok(AttemptRow {
        run: parse_run(run)?,
        attempt: attempt.parse().map_err(|_| corrupt("attempt_id"))?,
        generation: u32::try_from(generation)
            .ok()
            .and_then(AttemptGeneration::from_raw)
            .ok_or_else(|| corrupt("generation"))?,
        phase: AttemptPhase::parse(state)?,
        provider_session: idempotency.map(|idempotency| ProviderSessionKey {
            idempotency,
            session,
        }),
        host_instance: host
            .map(|host| {
                uuid::Uuid::parse_str(host)
                    .map(HostInstanceId::from_uuid)
                    .map_err(|_| corrupt("host_instance_id"))
            })
            .transpose()?,
        retired: retired != 0,
        started_at_ms,
        updated_at_ms,
    })
}

#[cfg(test)]
mod tests;
