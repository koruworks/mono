// SPDX-License-Identifier: MPL-2.0

//! The creation intent: §7.1 steps 1 and 4, and what a restart completes.
//!
//! Step 1 writes the intent with the whole delegation payload, keyed by the
//! `RunId` the asking client minted. Until step 4 marks it satisfied it is a
//! bounded pending request, listed as a request and never as a run; on
//! restart an unsatisfied intent is completed forward from its own payload
//! or abandoned if the person cancelled. There is no interval in which two
//! records both claim to be the run.
//!
//! **The payload is stored and returned, never shown.** It comes back only
//! through [`PendingIntent`] — as the `DelegationRequest` it was, whose
//! `Debug` redacts the objective — and appears in no error: a duplicate
//! request with a different payload is refused with a code, not a diff.

use koru_delegate_core::{
    ActorRef, DelegationRequest, DocumentId, RunId, SessionProfileKey, WorkspaceId, WorkspaceScope,
};
use rusqlite::{OptionalExtension, params};

use crate::{IntentState, LedgerError, RunsDb, WriteOp, WriteOutcome};

/// An intent's state, without its payload. What a listing shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IntentRecord {
    pub run: RunId,
    pub workspace: WorkspaceId,
    pub state: IntentState,
    /// Named once the intent is satisfied.
    pub run_document: Option<DocumentId>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// What a run was asked for, without what it was asked to do.
///
/// The payload holds the objective, the pinned context and the grants;
/// this reads back only the three answers that are settled at step 1 and
/// never rewritten — who asked, which launch profile, what kind of
/// working folder. A publisher rebuilding `delegate.run.created` after a
/// restart needs exactly these and must not be handed the objective to
/// get them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct IntentFacts {
    pub run: RunId,
    pub workspace: WorkspaceId,
    pub requested_by: ActorRef,
    pub agent: SessionProfileKey,
    /// Only its *kind* is ever read from here; the folder inside it is a
    /// person's path and is redacted like every path.
    pub scope: WorkspaceScope,
    pub context_items: u32,
}

/// An intent a restart completes forward: the request, as given.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PendingIntent {
    pub run: RunId,
    pub workspace: WorkspaceId,
    pub payload: DelegationRequest,
    pub created_at_ms: i64,
}

impl RunsDb {
    /// Validate a retry against the durable payload without inserting a new
    /// intent. `None` means this RunId is new; a matching row returns its
    /// payload-free state, and a different workspace/payload is the same
    /// conflict `record_intent` would report.
    pub fn validate_intent(
        &self,
        request: &DelegationRequest,
        workspace: WorkspaceId,
    ) -> Result<Option<IntentRecord>, LedgerError> {
        if matches!(request.scope, WorkspaceScope::BotFolder { .. }) {
            return Err(LedgerError::IntentConflict);
        }
        let row: Option<(String, String)> = self
            .conn()
            .query_row(
                "SELECT workspace_id, payload_json FROM run_intents WHERE run_id = ?1 AND CASE WHEN json_valid(payload_json) THEN coalesce(json_extract(payload_json, '$.scope.kind') != 'bot_folder', 1) ELSE 1 END",
                params![request.run.to_storage()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| LedgerError::sql("validate an intent", error))?;
        let Some((stored_workspace, stored_payload)) = row else {
            return Ok(None);
        };
        let stored: DelegationRequest =
            serde_json::from_str(&stored_payload).map_err(|_| LedgerError::Corrupt {
                table: "run_intents",
                column: "payload_json",
            })?;
        if stored_workspace != workspace.to_storage() || stored != *request {
            return Err(LedgerError::IntentConflict);
        }
        self.intent(request.run)
    }

    /// Step 1. Keyed by `request.run`, which the asking client minted so a
    /// retried request is the same run. A second call for a known run is
    /// idempotent when the payload and workspace are the same, and refused
    /// with [`LedgerError::IntentConflict`] when they differ — never a
    /// second row, never an overwrite.
    pub fn record_intent(
        &mut self,
        request: &DelegationRequest,
        workspace: WorkspaceId,
    ) -> Result<WriteOutcome, LedgerError> {
        if matches!(request.scope, WorkspaceScope::BotFolder { .. }) {
            return Err(LedgerError::IntentConflict);
        }
        let run = request.run;
        let payload =
            serde_json::to_string(request).map_err(|_| LedgerError::PayloadUnencodable)?;
        self.write(WriteOp::RecordIntent, |tx, now| {
            let existing: Option<(String, String)> = tx
                .query_row(
                    "SELECT workspace_id, payload_json FROM run_intents WHERE run_id = ?1 AND CASE WHEN json_valid(payload_json) THEN coalesce(json_extract(payload_json, '$.scope.kind') != 'bot_folder', 1) ELSE 1 END",
                    params![run.to_storage()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|error| LedgerError::sql("read an intent", error))?;
            if let Some((stored_workspace, stored_payload)) = existing {
                let stored: DelegationRequest =
                    serde_json::from_str(&stored_payload).map_err(|_| LedgerError::Corrupt {
                        table: "run_intents",
                        column: "payload_json",
                    })?;
                return if stored_workspace == workspace.to_storage() && stored == *request {
                    Ok(WriteOutcome::AlreadyApplied)
                } else {
                    Err(LedgerError::IntentConflict)
                };
            }
            tx.execute(
                "INSERT INTO run_intents
                   (run_id, workspace_id, payload_json, state, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, 'pending', ?4, ?4)",
                params![run.to_storage(), workspace.to_storage(), payload, now],
            )
            .map_err(|error| LedgerError::sql("record an intent", error))?;
            Ok(WriteOutcome::Applied)
        })
    }

    /// Step 4. The Run document is committed; the intent names it. A retry
    /// naming the same document is idempotent; a different one is a
    /// conflict; an abandoned intent is refused.
    pub fn satisfy_intent(
        &mut self,
        run: RunId,
        run_document: DocumentId,
    ) -> Result<WriteOutcome, LedgerError> {
        self.write(WriteOp::SatisfyIntent, |tx, now| {
            let (state, existing) = read_state(tx, run)?;
            match state {
                IntentState::Pending => {
                    tx.execute(
                        "UPDATE run_intents
                            SET state = 'satisfied', run_document_id = ?2, updated_at_ms = ?3
                          WHERE run_id = ?1",
                        params![run.to_storage(), run_document.to_storage(), now],
                    )
                    .map_err(|error| LedgerError::sql("satisfy an intent", error))?;
                    Ok(WriteOutcome::Applied)
                }
                IntentState::Satisfied if existing == Some(run_document) => {
                    Ok(WriteOutcome::AlreadyApplied)
                }
                IntentState::Satisfied => Err(LedgerError::IntentConflict),
                IntentState::Abandoned => Err(LedgerError::IntentAbandoned),
            }
        })
    }

    /// The person cancelled, or a capture failed (§7.1 step 2), before the
    /// run existed. Idempotent; refused once a run exists (D31: removing a
    /// run is its own action).
    pub fn abandon_intent(&mut self, run: RunId) -> Result<WriteOutcome, LedgerError> {
        self.write(WriteOp::AbandonIntent, |tx, now| {
            let (state, _) = read_state(tx, run)?;
            match state {
                IntentState::Pending => {
                    tx.execute(
                        "UPDATE run_intents SET state = 'abandoned', updated_at_ms = ?2
                          WHERE run_id = ?1",
                        params![run.to_storage(), now],
                    )
                    .map_err(|error| LedgerError::sql("abandon an intent", error))?;
                    Ok(WriteOutcome::Applied)
                }
                IntentState::Abandoned => Ok(WriteOutcome::AlreadyApplied),
                IntentState::Satisfied => Err(LedgerError::RunExists),
            }
        })
    }

    /// Every intent a restart must complete forward or abandon, oldest
    /// first. The payload rides along because that is what completing
    /// forward needs.
    pub fn pending_intents(&self) -> Result<Vec<PendingIntent>, LedgerError> {
        let mut statement = self
            .conn()
            .prepare(
                "SELECT run_id, workspace_id, payload_json, created_at_ms
                   FROM run_intents WHERE state = 'pending' AND CASE WHEN json_valid(payload_json) THEN coalesce(json_extract(payload_json, '$.scope.kind') != 'bot_folder', 1) ELSE 1 END
                  ORDER BY created_at_ms, run_id",
            )
            .map_err(|error| LedgerError::sql("list pending intents", error))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|error| LedgerError::sql("list pending intents", error))?;
        let mut pending = Vec::new();
        for row in rows {
            let (run, workspace, payload, created_at_ms) =
                row.map_err(|error| LedgerError::sql("list pending intents", error))?;
            pending.push(PendingIntent {
                run: parse_run(&run)?,
                workspace: parse_workspace(&workspace)?,
                payload: serde_json::from_str(&payload).map_err(|_| LedgerError::Corrupt {
                    table: "run_intents",
                    column: "payload_json",
                })?,
                created_at_ms,
            });
        }
        Ok(pending)
    }

    /// Every intent, oldest first, payload excluded — what a restart walks
    /// to find a run that exists and never started, and what a listing of
    /// this device's runs is made from.
    pub fn intents(&self) -> Result<Vec<IntentRecord>, LedgerError> {
        let mut statement = self
            .conn()
            .prepare(
                "SELECT run_id, workspace_id, state, run_document_id, created_at_ms, updated_at_ms
                   FROM run_intents WHERE CASE WHEN json_valid(payload_json) THEN coalesce(json_extract(payload_json, '$.scope.kind') != 'bot_folder', 1) ELSE 1 END
                  ORDER BY created_at_ms, run_id",
            )
            .map_err(|error| LedgerError::sql("list intents", error))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(|error| LedgerError::sql("list intents", error))?;
        let mut records = Vec::new();
        for row in rows {
            let (run, workspace, state, document, created_at_ms, updated_at_ms) =
                row.map_err(|error| LedgerError::sql("list intents", error))?;
            records.push(IntentRecord {
                run: parse_run(&run)?,
                workspace: parse_workspace(&workspace)?,
                state: IntentState::parse(&state)?,
                run_document: document.as_deref().map(parse_document).transpose()?,
                created_at_ms,
                updated_at_ms,
            });
        }
        Ok(records)
    }

    /// The settled facts of one creation, objective excluded. `None` for a
    /// run this device never recorded an intent for.
    pub fn intent_facts(&self, run: RunId) -> Result<Option<IntentFacts>, LedgerError> {
        let row: Option<(String, String)> = self
            .conn()
            .query_row(
                "SELECT workspace_id, payload_json FROM run_intents WHERE run_id = ?1 AND CASE WHEN json_valid(payload_json) THEN coalesce(json_extract(payload_json, '$.scope.kind') != 'bot_folder', 1) ELSE 1 END",
                params![run.to_storage()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| LedgerError::sql("read an intent", error))?;
        let Some((workspace, payload)) = row else {
            return Ok(None);
        };
        let request: DelegationRequest =
            serde_json::from_str(&payload).map_err(|_| LedgerError::Corrupt {
                table: "run_intents",
                column: "payload_json",
            })?;
        Ok(Some(IntentFacts {
            run,
            workspace: parse_workspace(&workspace)?,
            requested_by: request.requested_by,
            agent: request.agent,
            scope: request.scope,
            context_items: u32::try_from(request.context.len()).unwrap_or(u32::MAX),
        }))
    }

    /// One intent's state, payload excluded.
    pub fn intent(&self, run: RunId) -> Result<Option<IntentRecord>, LedgerError> {
        self.conn()
            .query_row(
                "SELECT workspace_id, state, run_document_id, created_at_ms, updated_at_ms
                   FROM run_intents WHERE run_id = ?1 AND CASE WHEN json_valid(payload_json) THEN coalesce(json_extract(payload_json, '$.scope.kind') != 'bot_folder', 1) ELSE 1 END",
                params![run.to_storage()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| LedgerError::sql("read an intent", error))?
            .map(
                |(workspace, state, document, created_at_ms, updated_at_ms)| {
                    Ok(IntentRecord {
                        run,
                        workspace: parse_workspace(&workspace)?,
                        state: IntentState::parse(&state)?,
                        run_document: document.as_deref().map(parse_document).transpose()?,
                        created_at_ms,
                        updated_at_ms,
                    })
                },
            )
            .transpose()
    }
}

fn read_state(
    tx: &rusqlite::Connection,
    run: RunId,
) -> Result<(IntentState, Option<DocumentId>), LedgerError> {
    let row: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT state, run_document_id FROM run_intents WHERE run_id = ?1 AND CASE WHEN json_valid(payload_json) THEN coalesce(json_extract(payload_json, '$.scope.kind') != 'bot_folder', 1) ELSE 1 END",
            params![run.to_storage()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| LedgerError::sql("read an intent", error))?;
    let (state, document) = row.ok_or(LedgerError::UnknownRun)?;
    Ok((
        IntentState::parse(&state)?,
        document.as_deref().map(parse_document).transpose()?,
    ))
}

pub(crate) fn parse_run(value: &str) -> Result<RunId, LedgerError> {
    value.parse().map_err(|_| LedgerError::Corrupt {
        table: "run_intents",
        column: "run_id",
    })
}

fn parse_workspace(value: &str) -> Result<WorkspaceId, LedgerError> {
    WorkspaceId::parse(value).ok_or(LedgerError::Corrupt {
        table: "run_intents",
        column: "workspace_id",
    })
}

fn parse_document(value: &str) -> Result<DocumentId, LedgerError> {
    DocumentId::parse(value).ok_or(LedgerError::Corrupt {
        table: "run_intents",
        column: "run_document_id",
    })
}

#[cfg(test)]
mod tests;
