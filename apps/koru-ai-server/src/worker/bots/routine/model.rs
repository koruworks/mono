// SPDX-License-Identifier: MPL-2.0
//! Own-Bot setup on the existing preparation lane, never a ManageBots context.
use super::*;
use koru_ai_sqlite::RoutineToolSubject;
mod contract;
mod driver;
mod job;
pub use contract::*;
pub(in crate::worker) use job::{RunRoutineCompletion, RunRoutineJob};

fn denied() -> Failure {
    failure(
        "permission_denied",
        "The current Run is not authorized for this Bot operation.",
        false,
    )
}
fn invalid() -> Failure {
    failure(
        "invalid_request",
        "The Routine arguments are invalid.",
        false,
    )
}
fn timed_out() -> Failure {
    failure(
        "ai_worker_timeout",
        "The Routine operation did not finish within its deadline.",
        true,
    )
}
fn unknown() -> Failure {
    failure(
        "save_status_unknown",
        "The Routine operation may have been accepted. Read creation state using the original request identity before retrying; do not create a new request ID.",
        true,
    )
}
fn tool_failure(error: BotError) -> Failure {
    if error == BotError::Forbidden {
        denied()
    } else {
        bot_failure(error)
    }
}
fn caller_failure(error: Failure) -> Failure {
    if matches!(error.code.as_str(), "wrong_owner" | "permission_denied") {
        denied()
    } else {
        error
    }
}
