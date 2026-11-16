// SPDX-License-Identifier: MPL-2.0

//! The ledger half of child requests (M6): one Bot Run asks other Bots and
//! waits. This module owns `bot_child_requests` and `bot_child_resumes`, and
//! the child forms of the wait steps that a decision takes through
//! `decisions::publication::lifecycle`.
//!
//! Every row here is ids, digests, closed tokens, numbers and times. No task
//! text, title, path or page content has a column, an error or a `Debug`.
use koru_delegate_core::children::{
    BotChildWaitCleanupIntentV1, BotChildWaitEnrollmentReply, ChildRequestReceipt, ChildResume,
};
use koru_delegate_core::decisions::{
    BotWaitId, BotWaitPhase, ConsumerCleanup, DecisionCleanupTarget, DecisionFence,
    DecisionRevision, DecisionWaitExpected, OriginWaitStop,
};
use koru_delegate_core::{
    AgentTurnId, AttemptGeneration, BotOwner, RunAttemptId, RunId, SessionId,
};

mod admission;
mod clock;
mod cycle;
mod observation;
mod resume;
pub(crate) mod rows;
mod settle;
mod steps;
#[cfg(test)]
mod tests;

pub(crate) use clock::{fold_parent_on, settle_child_ended_on};
pub(crate) use observation::is_child_run_on;
pub(crate) use resume::{close_on as close_resume_on, due_on, fresh_activity_on};
pub(crate) use settle::settle_parent_stopped_on;

use crate::{BotLedgerError, LedgerError};

fn sql(error: rusqlite::Error) -> BotLedgerError {
    LedgerError::sql("child request ledger", error).into()
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
fn next(value: DecisionRevision) -> Result<DecisionRevision, BotLedgerError> {
    value
        .get()
        .checked_add(1)
        .and_then(|n| DecisionRevision::new(n).ok())
        .ok_or(BotLedgerError::Stale)
}

/// How many open child waits one selector page may return.
pub const CHILD_RESUME_RUN_PAGE: usize = 16;
/// The most edges the wait graph may hold: 50 open waits of three requests.
pub const CHILD_GRAPH_EDGE_LIMIT: usize = 150;

/// The retained child wait. Its enrollment values are the child shapes; a
/// child wait never has a decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildWaitRecord {
    pub wait: BotWaitId,
    pub origin: DecisionFence,
    pub phase: BotWaitPhase,
    pub revision: DecisionRevision,
    /// Frozen in the transaction that admitted the ask.
    pub intent: BotChildWaitCleanupIntentV1,
    pub enrollment: Option<BotChildWaitEnrollmentReply>,
    pub release: Option<OriginWaitStop>,
    pub cleanup: Option<ConsumerCleanup>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub settled_at_ms: Option<i64>,
}

/// One exact open child wait and its retained physical cleanup subject.
/// Absence means no open child wait, never that no process exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildWaitExecution {
    pub expected: DecisionWaitExpected,
    pub wait: ChildWaitRecord,
    /// Every request of this wait, in ordinal order.
    pub requests: Vec<ChildRequestReceipt>,
    pub open_requests: u8,
    pub resume: Option<ChildResume>,
    pub cleanup_target: DecisionCleanupTarget,
    pub current_attempt: crate::AttemptRow,
}

/// Keyset position of the open child-wait selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildWaitCursor {
    pub created_at_ms: i64,
    pub wait: BotWaitId,
}

/// One open child wait as the selector reports it. Identity and two facts
/// the driver filters on; never authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildWaitRun {
    pub owner: BotOwner,
    pub run: RunId,
    pub wait: BotWaitId,
    pub phase: BotWaitPhase,
    pub open_requests: u8,
    pub cursor: ChildWaitCursor,
}

/// What a reservation attempt answered. Only `Reserved` and
/// `AlreadyReserved` opened (or found) a successor. `HostBusy` and `Paused`
/// leave the wait as it is. `OpenRequests` and `NotRetired` say the wait is
/// not resumable yet. `BudgetSpent` and `Unqualified` are definitive: the
/// wait can never be resumed and its owner cleans it up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChildSuccessorReservation {
    Reserved(Box<ChildWaitExecution>),
    AlreadyReserved(Box<ChildWaitExecution>),
    HostBusy,
    Paused,
    OpenRequests,
    NotRetired,
    BudgetSpent,
    Unqualified,
}

/// The durably acknowledged continuation of a recorded child-wait successor.
/// History only; it is not permission to send input or execute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildAcknowledgedContinuation {
    pub wait: BotWaitId,
    pub attempt: RunAttemptId,
    pub generation: AttemptGeneration,
    pub session: SessionId,
    pub turn: AgentTurnId,
}
