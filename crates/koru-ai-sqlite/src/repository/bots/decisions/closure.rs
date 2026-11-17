// SPDX-License-Identifier: MPL-2.0
//! The final authoritative Needed-you closure of a decision-held AI attempt.
//!
//! This is its own owning transaction, not a flag on `finish_bot_locally`
//! (which writes the replay vocabulary's `owner_unavailable`) or on the Routine
//! settlement (which writes `routine_*` reasons). It reuses only the shared
//! statements: message completion and live invocation cleanup. The replay
//! fence, cursor and replay stop reason are left exactly as they were.
use super::*;
use koru_ai_core::bots::replay::{BotExecutionFence, BotTerminalCleanup};

/// The open `inference_attempts.stop_reason` value this closure records
/// for every ending but a declined one (`STOP_DECISION_DECLINED`). Neither is
/// a replay stop reason; the replay CHECK vocabulary is untouched.
pub const BOT_DECISION_NEEDED_YOU: &str = koru_ai_core::STOP_DECISION_NEEDED_YOU;

/// What one closure call established. None of these is a default success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotDecisionClosure {
    /// This call ended the attempt as Needed you, for the reason it
    /// positively knows (absent when the reasons cannot be told apart).
    Closed {
        reason: Option<koru_ai_core::NeededYouReason>,
    },
    /// An exact earlier closure ended it; nothing was written again.
    ClosedBefore,
    /// Another cause already ended the attempt. Its ending is kept and this
    /// decision does not claim to have caused it.
    AlreadyEnded,
    /// The authoritative decision is not final yet: still open, cleanup not
    /// confirmed, or its final publication not acknowledged. Nothing written;
    /// the closure stays owed.
    Owed,
}

/// What the settled read positively says about why the work stopped: a
/// Denied closure is the person declining (O109); an expired or cancelled one
/// was not answered in time. Every other closure reason is not told apart.
pub(super) fn needed_you_reason(
    observed: &BotDecisionWaitObservation,
) -> Option<koru_ai_core::NeededYouReason> {
    match &observed.disposition {
        DecisionDisposition::Settled { reason, .. } => match reason {
            DecisionClosureReason::Denied => Some(koru_ai_core::NeededYouReason::Declined),
            DecisionClosureReason::Expired | DecisionClosureReason::Canceled => {
                Some(koru_ai_core::NeededYouReason::NotAnswered)
            }
            _ => None,
        },
        DecisionDisposition::Open { .. } => None,
    }
}

fn origin_fence(key: &BotWaitEnrollmentKey) -> BotExecutionFence {
    BotExecutionFence {
        owner: key.owner,
        bot: key.bot,
        run: key.run,
        attempt: key.origin_attempt,
        generation: key.origin_generation,
        session: key.origin_session,
        turn: Some(key.origin_turn),
    }
}

/// `Ok(None)` is owed. The final target is the separate consumer cleanup
/// proof; the historical origin stop is never accepted in its place.
fn final_target(
    observed: &BotDecisionWaitObservation,
) -> Result<Option<&DecisionCleanupTarget>, BotError> {
    match &observed.disposition {
        DecisionDisposition::Open { .. } => return Ok(None),
        // A completed decision continues through its consumer; its terminal
        // reaches the AI through ordinary replay, never through this closure.
        DecisionDisposition::Settled { reason, .. }
            if *reason == DecisionClosureReason::Completed =>
        {
            return Err(BotError::Conflict);
        }
        DecisionDisposition::Settled { .. } => {}
    }
    let ConsumerCleanup::Confirmed { target, .. } = &observed.consumer_cleanup else {
        return Ok(None);
    };
    if !matches!(observed.publication, DecisionPublication::Applied { .. }) {
        return Ok(None);
    }
    Ok(Some(target))
}

impl Repository {
    /// `observed` must be the caller's independently authenticated delegate
    /// read of this exact wait (IPC16 `ReadBotDecisionWait`); its DTO shape is
    /// not authority. Exact replay comes first: an attempt that already ended
    /// is absorbing and is reported as it is, before any fresh check.
    pub fn close_bot_decision_needed_you(
        &mut self,
        owner: BotOwner,
        enrollment: &BotWaitEnrollmentKey,
        observed: &BotDecisionWaitObservation,
        now: Timestamp,
    ) -> Result<BotDecisionClosure, BotError> {
        enrollment::validate_owner(owner, enrollment)?;
        let tx = self.connection.transaction().map_err(bad)?;
        let enrolled = enrollment::retained_on(&tx, enrollment)?.ok_or(BotError::NotFound)?;
        let (attempt, _, replay) = enrollment::subject_on(&tx, owner, enrollment)?;
        if !matches!(enrolled, BotWaitEnrollmentReply::Enrolled { ai_attempt, .. } if ai_attempt == attempt.as_uuid())
        {
            return Err(BotError::Storage);
        }
        // Booleans only: no attempt text is allocated to classify it.
        let (live, unstopped, ours): (bool, bool, bool) = tx
            .query_row(
                "SELECT status IN ('pending','streaming'),stop_reason IS NULL,
                 status='interrupted' AND stop_reason IN (?2,?3) FROM inference_attempts WHERE id=?1",
                params![
                    attempt.to_storage(),
                    BOT_DECISION_NEEDED_YOU,
                    koru_ai_core::STOP_DECISION_DECLINED
                ],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(bad)?;
        if !live {
            return Ok(if ours {
                BotDecisionClosure::ClosedBefore
            } else {
                BotDecisionClosure::AlreadyEnded
            });
        }
        if !unstopped || replay.stop_reason.is_some() {
            return Err(BotError::Conflict);
        }
        observed.validate().map_err(|_| BotError::Invalid)?;
        if observed.key != *enrollment {
            return Err(BotError::Conflict);
        }
        let Some(target) = final_target(observed)? else {
            return Ok(BotDecisionClosure::Owed);
        };
        let reason = needed_you_reason(observed);
        // The replay fence is the origin, or the consumer this very decision
        // installed; then the confirmed cleanup must name that consumer.
        let current = replay.fence.as_ref().ok_or(BotError::Storage)?;
        if *current != origin_fence(enrollment) {
            let archive = replay::installed_decision_consumer_on(&tx, attempt, current)?
                .ok_or(BotError::Conflict)?;
            let named = match target {
                DecisionCleanupTarget::Consumer {
                    attempt: consumer,
                    generation,
                    session,
                    accepted_turn,
                    ..
                } => {
                    *consumer == current.attempt
                        && *generation == current.generation
                        && *session == Some(current.session)
                        && *accepted_turn == current.turn
                }
                DecisionCleanupTarget::Origin { .. } => false,
            };
            if archive.decision != enrollment.decision_id || !named {
                return Err(BotError::Conflict);
            }
        }
        let assistant = crate::repository::query_attempt(&tx, "id", &attempt.to_storage())
            .map_err(|_| BotError::Storage)?
            .ok_or(BotError::Storage)?
            .assistant_message_id
            .map(|id| id.to_storage());
        let changed = tx
            .execute(
                "UPDATE inference_attempts SET status='interrupted',completed_at_ms=max(queued_at_ms,?2),stop_reason=?3
                 WHERE id=?1 AND status IN ('pending','streaming') AND stop_reason IS NULL",
                params![
                    attempt.to_storage(),
                    now.millis(),
                    koru_ai_core::NeededYouReason::stop_code(reason)
                ],
            )
            .map_err(bad)?;
        if changed != 1 {
            return Err(BotError::Conflict);
        }
        crate::repository::complete_message(
            &tx,
            assistant.as_deref(),
            koru_ai_core::MessageState::Interrupted,
            None,
            now,
        )
        .map_err(|_| BotError::Storage)?;
        crate::repository::bot_replay::finish_agent_rows_on(
            &tx,
            attempt,
            &BotTerminalCleanup {
                retained_question_ids: Vec::new(),
            },
            now,
        )
        .map_err(|_| BotError::Storage)?;
        // The reply ended as "Needed you": its record, and its
        // conversation's, which stops running.
        crate::repository::bots::client_feed::touch_attempt(&tx, attempt, true).map_err(bad)?;
        tx.commit().map_err(bad)?;
        Ok(BotDecisionClosure::Closed { reason })
    }
}

#[cfg(test)]
mod tests;
