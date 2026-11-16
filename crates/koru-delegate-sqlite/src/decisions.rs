// SPDX-License-Identifier: MPL-2.0

//! Retained decision ownership. Public methods only read; mutation bodies stay
//! crate-private until the publication owner composes the same transaction.
use crate::{BotLedgerError, LedgerError, RunsDb};
use koru_delegate_core::{decisions::*, *};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};

mod authority;
mod current;
mod drain;
mod resume_runs;
pub(crate) use current::acknowledged_turn_on;
pub(crate) use current::fresh_native_input_on;
pub(crate) use current::{acknowledged_continuation_on, fresh_activity_on, retained_consumer_on};
pub(crate) use publication::commit_child_control_on;
mod effect;
pub(crate) use effect::validate_native_on;
mod observation;
mod occupancy;
mod private;
pub(crate) use occupancy::inventory as execution_inventory_on;
mod publication;
mod rows;
// These transaction bodies deliberately have no public committing wrapper yet.
// C3 must add the control projection before committing their transaction.
#[cfg(test)]
mod tests;
#[allow(dead_code)]
mod write;
pub use resume_runs::DECISION_RESUME_RUN_PAGE;
pub use write::AnswerDecision;

/// Immutable identity of a cleanup mutation, independent of the projection
/// transition which currently carries it. This is not a wire request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecisionCleanupOrigin {
    Requested {
        wait: BotWaitId,
        expected_wait_revision: DecisionRevision,
        cause: DecisionClosureReason,
    },
    Denied {
        wait: BotWaitId,
        decision: DecisionId,
        answer: DecisionAnswerId,
        answer_revision: DecisionRevision,
    },
    NoDecision {
        wait: BotWaitId,
        reason: BotWaitReason,
        origin: DecisionFence,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionCleanupOwnership {
    pub expected: DecisionWaitExpected,
    pub projection: DecisionTransitionId,
    pub origin: DecisionCleanupOrigin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionRecord {
    pub receipt: DecisionReceipt,
    pub wait: DecisionWaitRecord,
    pub resume: Option<DecisionResume>,
    pub native_response: Option<DecisionNativeResponse>,
}

/// The durably acknowledged continuation of a recorded decision successor:
/// the consumer identity and the input turn C recorded on the host's positive
/// answer. History only; it is not permission to send input or execute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecisionAcknowledgedContinuation {
    pub decision: DecisionId,
    pub attempt: RunAttemptId,
    pub generation: AttemptGeneration,
    pub session: SessionId,
    pub turn: AgentTurnId,
}

/// Historical evidence, not an occupancy discount or permission to stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionWaitRecord {
    pub wait: BotWaitId,
    pub origin: DecisionFence,
    pub reason: BotWaitReason,
    pub phase: BotWaitPhase,
    pub revision: DecisionRevision,
    pub observed_sequence: Option<u64>,
    pub enrollment_request: Option<EnrollBotWaitCleanup>,
    pub enrollment: Option<BotWaitEnrollmentReply>,
    pub release: Option<OriginWaitStop>,
    pub cleanup: Option<ConsumerCleanup>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub settled_at_ms: Option<i64>,
}

/// One exact open runtime wait and its retained physical cleanup subject.
/// Absence means no open wait, never that a process does not exist. A wait
/// with a missing or mismatched attempt is an error, not an empty observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionWaitExecution {
    pub expected: DecisionWaitExpected,
    pub wait: DecisionWaitRecord,
    pub decision_state: Option<DecisionState>,
    pub resume: Option<DecisionResume>,
    pub cleanup_target: DecisionCleanupTarget,
    pub current_attempt: crate::AttemptRow,
}

fn sql(error: rusqlite::Error) -> BotLedgerError {
    LedgerError::sql("decision ledger", error).into()
}
fn corrupt(table: &'static str, column: &'static str) -> BotLedgerError {
    LedgerError::Corrupt { table, column }.into()
}
fn require(ok: bool) -> Result<(), BotLedgerError> {
    if ok {
        Ok(())
    } else {
        Err(BotLedgerError::Conflict)
    }
}
fn valid<T: DecisionValidate>(value: &T) -> Result<(), BotLedgerError> {
    value.validate().map_err(|_| BotLedgerError::Invalid)
}
fn encoded<T: Serialize>(value: &T, limit: usize) -> Result<String, BotLedgerError> {
    let bytes = encode_decision_value(value, limit).map_err(|_| BotLedgerError::Invalid)?;
    String::from_utf8(bytes).map_err(|_| BotLedgerError::Invalid)
}
fn token<T: Serialize>(value: T) -> Result<String, BotLedgerError> {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or(BotLedgerError::Invalid)
}
fn next(value: DecisionRevision) -> Result<DecisionRevision, BotLedgerError> {
    value
        .get()
        .checked_add(1)
        .and_then(|n| DecisionRevision::new(n).ok())
        .ok_or(BotLedgerError::Stale)
}

// M6: the narrow surface the child-request owner (`crate::children`) builds
// on. Each is the existing owner's own check or decoder, never a second one.
pub(crate) fn owner_on(
    conn: &Connection,
    owner: BotOwner,
    run: RunId,
) -> Result<(), BotLedgerError> {
    authority::owner(conn, owner, run)
}
pub(crate) fn origin_attempt_on(
    conn: &Connection,
    fence: &DecisionFence,
) -> Result<crate::AttemptRow, BotLedgerError> {
    authority::attempt(conn, fence)
}
pub(crate) fn child_wait_on(
    conn: &Connection,
    run: RunId,
    wait: BotWaitId,
) -> Result<Option<crate::ChildWaitRecord>, BotLedgerError> {
    rows::child::wait(conn, run, wait)
}
pub(crate) fn open_child_wait_on(
    conn: &Connection,
    run: RunId,
) -> Result<Option<crate::ChildWaitRecord>, BotLedgerError> {
    rows::child::open_wait(conn, run)
}
pub(crate) fn child_resume_on(
    conn: &Connection,
    wait: &crate::ChildWaitRecord,
) -> Result<Option<koru_delegate_core::children::ChildResume>, BotLedgerError> {
    rows::child::resume(conn, wait)
}
pub(crate) fn child_consumer_on(
    conn: &Connection,
    run: RunId,
    attempt: &crate::AttemptRow,
) -> Result<
    Option<(
        crate::ChildWaitRecord,
        koru_delegate_core::children::ChildResume,
    )>,
    BotLedgerError,
> {
    rows::child::consumer(conn, run, attempt)
}
pub(crate) fn correlate_child_release_on(
    conn: &Connection,
    wait: &crate::ChildWaitRecord,
) -> Result<(), BotLedgerError> {
    rows::child::correlate_release(conn, wait)
}
/// The open-wait cap, counted exactly as a decision's preparation counts it.
pub(crate) fn wait_cap_on(tx: &Connection, owner: BotOwner) -> Result<(), BotLedgerError> {
    write::cap(tx, owner)
}
/// What kind of wait a Run has open. A Run has at most one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenWaitKind {
    Decision,
    Child,
    Cleanup,
}
pub(crate) fn open_wait_reason_on(
    conn: &Connection,
    run: RunId,
) -> Result<Option<OpenWaitKind>, BotLedgerError> {
    let reason: Option<String> = conn
        .query_row(
            "SELECT CASE WHEN reason IN ('decision','capacity','native_expired','overlapping_prompt','child') THEN reason END
             FROM bot_wait_runtime INDEXED BY bot_wait_runtime_open_run WHERE run_id=?1 AND phase<>'settled'",
            [run.to_storage()],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(sql)?
        .map(|reason| reason.ok_or_else(|| corrupt("bot_wait_runtime", "reason")))
        .transpose()?;
    Ok(reason.map(|reason| match reason.as_str() {
        "decision" => OpenWaitKind::Decision,
        "child" => OpenWaitKind::Child,
        _ => OpenWaitKind::Cleanup,
    }))
}

impl RunsDb {
    /// Exact retained history, without fresh policy/clock qualification.
    /// Authentication is a caller obligation; the stored subject is rechecked.
    pub fn bot_decision(
        &self,
        owner: BotOwner,
        run: RunId,
        decision: DecisionId,
    ) -> Result<Option<DecisionRecord>, BotLedgerError> {
        authority::owner(self.conn(), owner, run)?;
        rows::read(self.conn(), owner, run, decision)
    }

    pub fn bot_decision_for_operation(
        &self,
        owner: BotOwner,
        run: RunId,
        operation: uuid::Uuid,
    ) -> Result<Option<DecisionRecord>, BotLedgerError> {
        authority::owner(self.conn(), owner, run)?;
        let id = rows::operation(self.conn(), run, operation)?;
        id.map(|id| {
            rows::read(self.conn(), owner, run, id)?
                .ok_or_else(|| corrupt("bot_decisions", "identity"))
        })
        .transpose()
    }
}
