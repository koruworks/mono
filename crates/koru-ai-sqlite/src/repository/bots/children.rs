// SPDX-License-Identifier: MPL-2.0
//! One Bot asking another (M6, ai.db 26): the owner of `bot_child_chats`,
//! `bot_child_wait_enrollments` and `bot_child_replay_rollovers`.
//!
//! Nothing here calls the delegate, the Store or a provider, and nothing here
//! holds a task, a title or a page. A child request is the recipient's
//! ordinary `start` receipt plus one row that names who asked; the wait's
//! enrollment and its replay rollover mirror the decision tables, keyed by
//! the wait because a child wait has no decision.
use super::*;
use koru_ai_core::bots::replay::{BotExecutionFence, BotReplayState};
use koru_delegate_core::ChildRequestId;
use koru_delegate_core::children::*;
use koru_delegate_core::decisions::*;

mod chats;
mod enrollment;
pub(super) mod rollover;
pub use rollover::BotChildReplayRollover;

/// The parent Run's retained admission, its AI attempt and that attempt's
/// replay state: the one subject every child-wait writer proves first.
fn subject_on(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    run: koru_delegate_core::RunId,
) -> Result<(InferenceAttemptId, BotSubmissionReceipt, BotReplayState), BotError> {
    decisions::run_subject_on(conn, owner, bot, run)
}

/// The fence the AI owner's replay state uses, for a wait's origin.
fn origin_fence(origin: &DecisionFence) -> BotExecutionFence {
    BotExecutionFence {
        owner: origin.owner,
        bot: origin.bot,
        run: origin.run,
        attempt: origin.attempt,
        generation: origin.generation,
        session: origin.session,
        turn: Some(origin.turn),
    }
}

/// Whether the attempt still runs: not terminal and not stopped.
fn attempt_live_on(conn: &Connection, attempt: InferenceAttemptId) -> Result<bool, BotError> {
    conn.query_row(
        "SELECT status IN ('pending','streaming') AND stop_reason IS NULL FROM inference_attempts WHERE id=?1",
        [attempt.to_storage()],
        |r| r.get(0),
    )
    .map_err(bad)
}

#[cfg(test)]
pub(super) mod tests;
