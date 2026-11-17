// SPDX-License-Identifier: MPL-2.0

//! Delivery while the Run has an open child wait (contract 4.1, "checked
//! against O127" and "checked against the single-delivery rule").
//!
//! A Run that waits delivers once. Two receipts can arrive while the wait is
//! open, and each goes through the one Delivery Control of the wait:
//!
//! - **The origin's own turn completed naturally** (the held answer timed
//!   out, or the turn ended by itself). That is an ordinary completion, never
//!   the outcome of the internal stop. The receipt is recorded, which settles
//!   every open request `parent_stopped` in the same transaction; then the
//!   wait's cleanup is requested and the children are stopped. No successor
//!   resumes it. If a successor was already reserved, the receipt is refused
//!   first, cleanup is requested, and the retried receipt completes the Run.
//! - **The acknowledged successor's turn completed** before the wait's last
//!   publication was applied. It is the Run's one delivery.
//!
//! The evidence that the origin's turn completed, when its session is gone,
//! is the `TurnCompleted` step this delegate recorded for that exact turn.

use super::{Orchestrator, RunError, refused};
use crate::orchestrator::state::DeliveryRetry;
use crate::publication_pending;
use koru_delegate_core::decisions::{BotWaitPhase, DecisionDeliveryOutcome, DecisionTransitionId};
use koru_delegate_core::{
    AgentTurnId, BotDeliveryReceipt, BotExecutionFence, BotRunKey, DeviceId, RunStatus, StepKind,
    TimelineEntryId, TimelineRef,
};
use koru_delegate_sqlite::ChildWaitExecution;

impl Orchestrator {
    pub(super) fn bot_acknowledge_child_delivery(
        &mut self,
        key: &BotRunKey,
        device: DeviceId,
        receipt: &BotDeliveryReceipt,
        execution: ChildWaitExecution,
    ) -> Result<(), RunError> {
        let run = key.run;
        let origin = execution.wait.origin;
        let header = self
            .runs
            .get(&run)
            .ok_or_else(publication_pending)?
            .doc
            .document()?
            .header()?;
        let bot = header.bot.as_ref().ok_or_else(|| refused("bot_fenced"))?;
        if bot.owner != key.owner
            || bot.bot != key.bot
            || bot.chat_id != receipt.chat_id
            || bot.ai_attempt_id != Some(receipt.ai_attempt_id)
            || bot.assistant_message_id != Some(receipt.assistant_message_id)
        {
            return Err(refused("bot_fenced"));
        }
        if header
            .chat_delivery
            .as_ref()
            .is_some_and(|old| old != receipt)
        {
            return Err(refused("bot_conflict"));
        }
        let of_origin =
            receipt.attempt == origin.attempt && receipt.generation == origin.generation;
        // The turn the receipt may complete, and the live fence that proves
        // it for a consumer whose session still exists.
        let (turn, live) = if of_origin {
            if execution.wait.phase == BotWaitPhase::ResumeReserved {
                // A successor was reserved: the origin's receipt is refused,
                // the wait is cleaned up and the successor stopped. The
                // retried receipt then completes the Run with this delivery.
                let _ = device;
                self.child_request_cleanup(run)?;
                return Err(refused("bot_delivery_pending"));
            }
            let row = &execution.current_attempt;
            let live = (row.attempt == origin.attempt
                && row.generation == origin.generation
                && !row.retired
                && !row.phase.is_terminal())
            .then_some(origin.session);
            (origin.turn, live)
        } else {
            let resume = execution
                .resume
                .as_ref()
                .ok_or_else(|| refused("bot_fenced"))?;
            let (attempt, generation, session, _) = resume
                .consumer_identity()
                .ok_or_else(|| refused("bot_fenced"))?;
            let row = &execution.current_attempt;
            if receipt.attempt != attempt
                || receipt.generation != generation
                || row.attempt != attempt
                || row.generation != generation
            {
                return Err(refused("bot_fenced"));
            }
            // Only the exact successor whose continuation was acknowledged.
            let turn = resume
                .accepted_turn()
                .ok_or_else(|| refused("bot_fenced"))?;
            (turn, Some(session.ok_or_else(|| refused("bot_fenced"))?))
        };
        // An exact retry reuses the retained transition, step and outcome.
        let retained = self
            .runs
            .get(&run)
            .and_then(|state| state.delivery_retry.clone())
            .filter(|retry| retry.receipt == *receipt);
        let retry = match retained {
            Some(retry) => retry,
            None => {
                // An earlier owed projection publishes first.
                if self
                    .bot_ledger()?
                    .next_decision_projection(key.owner, run)?
                    .is_some()
                {
                    return Err(refused("bot_delivery_pending"));
                }
                let outcome = if header.status.is_terminal()
                    || matches!(header.status, RunStatus::Interrupted { .. })
                {
                    // History on an absorbing status, never a revived task.
                    DecisionDeliveryOutcome::Retained
                } else {
                    let completed = match live {
                        Some(session) => {
                            let fence = BotExecutionFence {
                                owner: key.owner,
                                bot: key.bot,
                                run,
                                attempt: receipt.attempt,
                                generation: receipt.generation,
                                session,
                                turn: Some(turn),
                            };
                            self.bot_decision_delivery_completed(
                                &fence, device, &header, session, turn, !of_origin,
                            )?
                        }
                        // The origin's session is gone: only the step this
                        // delegate recorded for that exact turn is evidence.
                        None => self.child_origin_turn_completed(run, turn)?,
                    };
                    if !completed {
                        // Nothing is written: the AI retries the exact receipt.
                        return Err(refused("bot_delivery_pending"));
                    }
                    if self.native_outcomes_unknown(run)? {
                        DecisionDeliveryOutcome::InterruptedUnknown
                    } else {
                        DecisionDeliveryOutcome::Completed
                    }
                };
                let retry = DeliveryRetry {
                    receipt: receipt.clone(),
                    transition: DecisionTransitionId::mint(),
                    step: TimelineEntryId::mint(),
                    outcome,
                };
                if let Some(state) = self.runs.get_mut(&run) {
                    state.delivery_retry = Some(retry.clone());
                }
                retry
            }
        };
        let artifacts = u32::try_from(header.artifacts.len()).unwrap_or(u32::MAX);
        let now = self.ports.clock.now_ms();
        if let Err(error) = self.bot_ledger()?.record_decision_delivery(
            receipt,
            &execution.expected,
            retry.transition,
            retry.outcome,
            retry.step,
            artifacts,
            now,
        ) {
            // A lost reply is resolved by the exact transition.
            if self
                .bot_ledger()?
                .decision_projection(key.owner, run, retry.transition)?
                .is_none()
            {
                return Err(error.into());
            }
        }
        if of_origin {
            // The origin's turn stands as the Run's answer: the wait is
            // closed, never resumed, and its children are stopped. The
            // ledger already settled every request `parent_stopped` with the
            // receipt; reach the children now.
            self.child_cancel_children(key.owner, run, execution.wait.wait);
            self.child_retry_stops(device);
            // The wait's cleanup is requested only once the Delivery Control
            // is applied: requested earlier it would supersede that owed
            // Control and the Run would lose its answer. The AI retries the
            // exact receipt, which lands here again with nothing owed.
            // The ledger reports this closure as `ai_terminal` from the
            // delivered attempt; the cleanup request's own cause is one of
            // the four a request may carry.
            if self
                .bot_ledger()?
                .next_decision_projection(key.owner, run)?
                .is_none()
            {
                self.child_request_cleanup(run)?;
            }
        }
        // The receipt is durable; its header publication is the owed Control.
        Err(refused("bot_delivery_pending"))
    }

    /// Whether this delegate recorded the natural completion of exactly this
    /// turn: working memory's terminal turn of the origin, else the retained
    /// `TurnCompleted` step that names the turn. Never inferred from a stop.
    fn child_origin_turn_completed(
        &mut self,
        run: koru_delegate_core::RunId,
        turn: AgentTurnId,
    ) -> Result<bool, RunError> {
        let state = self.runs.get(&run).ok_or_else(publication_pending)?;
        if state
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.bot_terminal_turn == Some(turn))
        {
            return Ok(true);
        }
        let document = state.doc.document()?;
        Ok(document.timeline_all().iter().any(|item| {
            item.step().is_some_and(|entry| {
                matches!(entry.kind, StepKind::TurnCompleted { .. })
                    && entry
                        .refs
                        .iter()
                        .any(|reference| matches!(reference, TimelineRef::Turn { turn: named } if *named == turn))
            })
        }))
    }
}
