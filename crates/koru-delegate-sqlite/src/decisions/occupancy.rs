// SPDX-License-Identifier: MPL-2.0

//! Host occupancy and Bot ownership share a single bounded scalar inventory.
//! Large decision bindings, admitted prompts and context are never decoded here.
use super::*;
use crate::{BotExecutionCapacity, BotExecutionInventory, BotExecutionRecord, BotOwnedExecution};

const DECISION: &[(&str, &str, usize, bool)] = &[
    ("decision_id", "text", 36, false),
    ("run_id", "text", 36, false),
    ("user_scope", "text", 36, false),
    ("device_id", "text", 36, false),
    ("bot_id", "text", 36, false),
    ("origin_attempt_id", "text", 36, false),
    ("origin_generation", "integer", 0, false),
    ("origin_session_id", "text", 36, false),
    ("origin_turn_id", "text", 36, false),
    ("kind", "text", 32, false),
    ("authorization", "text", 32, false),
    ("phase", "text", 32, false),
    ("revision", "integer", 0, false),
];

/// No payload parsing: compare the current companion to the original scalar
/// subject and closed phase. Full binding/AI provenance stays with release,
/// answer and effect writers, not each actor capacity refresh.
fn state(
    conn: &Connection,
    wait: &DecisionWaitRecord,
    decision: DecisionId,
) -> Result<DecisionState, BotLedgerError> {
    conn.query_row(
        &rows::projection("bot_decisions", DECISION, "decision_id=?1"),
        [decision.to_storage()],
        |row| {
            Ok((|| {
                let c = rows::Cells {
                    row,
                    table: "bot_decisions",
                };
                c.check()?;
                let f = wait.origin;
                c.same(
                    c.id::<DecisionId>("decision_id")? == decision
                        && c.id::<RunId>("run_id")? == f.run
                        && c.get::<String>("user_scope")? == f.owner.user_scope.to_storage()
                        && c.id::<DeviceId>("device_id")? == f.owner.device
                        && c.id::<BotId>("bot_id")? == f.bot
                        && c.id::<RunAttemptId>("origin_attempt_id")? == f.attempt
                        && c.generation("origin_generation")? == f.generation
                        && c.id::<SessionId>("origin_session_id")? == f.session
                        && c.id::<AgentTurnId>("origin_turn_id")? == f.turn,
                )?;
                let state = DecisionState {
                    kind: c.id("kind")?,
                    authorization: c.id("authorization")?,
                    phase: c.id("phase")?,
                    revision: c.revision("revision")?,
                };
                valid(&state)?;
                if let Some(request) = &wait.enrollment_request {
                    c.same(
                        state.kind.resumes_by_successor()
                            && request.key.decision_revision <= state.revision,
                    )?;
                }
                Ok::<_, BotLedgerError>(state)
            })())
        },
    )
    .optional()
    .map_err(sql)?
    .transpose()?
    .ok_or_else(|| corrupt("bot_wait_runtime", "missing_decision"))
}

fn open_wait(
    conn: &Connection,
    execution: &BotExecutionRecord,
) -> Result<Option<DecisionWaitRecord>, BotLedgerError> {
    let id=conn.query_row("SELECT CASE WHEN typeof(wait_id)='text' AND length(CAST(wait_id AS BLOB))=36 THEN wait_id END FROM bot_wait_runtime INDEXED BY bot_wait_runtime_open_run WHERE run_id=?1 AND phase<>'settled'",[execution.run.to_storage()],|r|r.get::<_,String>(0)).optional().map_err(sql)?;
    id.map(|id| {
        let id = id
            .parse()
            .map_err(|_| corrupt("bot_wait_runtime", "wait_id"))?;
        let wait = rows::by_wait(conn, execution.run, id)?
            .ok_or_else(|| corrupt("bot_wait_runtime", "missing"))?;
        require(
            wait.origin.owner == execution.owner
                && wait.origin.bot == execution.bot
                && wait.origin.run == execution.run,
        )?;
        Ok(wait)
    })
    .transpose()
}

pub(crate) fn inventory(
    conn: &Connection,
    device: DeviceId,
) -> Result<BotExecutionInventory, BotLedgerError> {
    let executions = crate::bots::bounded_owned_executions(conn, device)?;
    let mut result = BotExecutionInventory {
        owned: Vec::with_capacity(executions.len()),
        occupied: 0,
    };
    let (mut decisions, mut nondecision) = (0usize, 0usize);
    for execution in executions {
        let mut capacity = BotExecutionCapacity::Occupied;
        if let Some(wait) = open_wait(conn, &execution)? {
            match wait.reason {
                BotWaitReason::Decision { decision } => {
                    decisions += 1;
                    let state = state(conn, &wait, decision)?;
                    // Retained release is not an occupancy decision. Only an
                    // exact currently retired origin can use it; production
                    // SameSession Held remains unsupported.
                    if matches!(
                        wait.phase,
                        BotWaitPhase::Retired | BotWaitPhase::CleanupPending
                    ) && wait.release.is_some()
                    {
                        let phase_matches = if wait.phase == BotWaitPhase::Retired {
                            state.phase == DecisionPhase::Held
                        } else {
                            matches!(
                                state.phase,
                                DecisionPhase::StopPending | DecisionPhase::Settled
                            )
                        };
                        require(
                            state.kind.resumes_by_successor()
                                && phase_matches
                                && execution.state == BotAdmissionState::Executing
                                && execution.active_since_ms.is_none(),
                        )?;
                        rows::correlate_release(conn, &wait)?;
                        require(wait.release.is_some() && wait.enrollment.is_some())?;
                        let resumed:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM bot_decision_resumes WHERE decision_id=?1)",[decision.to_storage()],|r|r.get(0)).map_err(sql)?;
                        if resumed {
                            // An old origin proof never discounts a resumed or
                            // unknown consumer. Its own final cleanup is a
                            // separate qualification below the cleanup owner.
                            if wait.phase == BotWaitPhase::Retired {
                                return Err(BotLedgerError::Conflict);
                            }
                            result.occupied += 1;
                            result.owned.push(BotOwnedExecution {
                                execution,
                                capacity,
                            });
                            continue;
                        }
                        let latest: i64 = conn
                            .query_row(
                                "SELECT max(generation) FROM run_attempts WHERE run_id=?1",
                                [execution.run.to_storage()],
                                |r| r.get(0),
                            )
                            .map_err(sql)?;
                        require(latest == i64::from(wait.origin.generation.get()))?;
                        if crate::native_results::held_for_bot(
                            conn,
                            execution.owner,
                            execution.bot,
                        )? {
                            return Err(BotLedgerError::Fenced);
                        }
                        capacity = BotExecutionCapacity::QualifiedDecisionRelease {
                            wait: wait.wait,
                            decision,
                            revision: wait.revision,
                            origin: wait.origin,
                        };
                    }
                }
                _ => nondecision += 1,
            }
        }
        if capacity == BotExecutionCapacity::Occupied {
            result.occupied += 1;
        }
        result.owned.push(BotOwnedExecution {
            execution,
            capacity,
        });
    }
    if decisions > DECISION_WAIT_LIMIT
        || nondecision > crate::BOT_HOST_EXECUTION_LIMIT
        || result.occupied > crate::BOT_HOST_EXECUTION_LIMIT
    {
        return Err(corrupt("bot_admissions", "host_capacity"));
    }
    Ok(result)
}

impl RunsDb {
    pub fn bot_execution_inventory(
        &self,
        device: DeviceId,
    ) -> Result<BotExecutionInventory, BotLedgerError> {
        let snapshot = self.conn().unchecked_transaction().map_err(sql)?;
        let result = inventory(&snapshot, device)?;
        snapshot.commit().map_err(sql)?;
        Ok(result)
    }
}
