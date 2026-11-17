// SPDX-License-Identifier: MPL-2.0

//! Delivery while the sole Run handle is held for decision publication. The
//! receipt and its owed header publication commit together through the one C
//! writer as a Delivery Control; this owner alone qualifies the outcome. A
//! receipt is never accepted as proof that native work finished.

use super::{Orchestrator, RunError, refused};
use crate::orchestrator::state::DeliveryRetry;
use crate::publication_pending;
use koru_delegate_core::decisions::{DecisionDeliveryOutcome, DecisionResumeState};
use koru_delegate_core::{
    BotDeliveryReceipt, BotExecutionFence, BotRunKey, ControlState, DeviceId, RunStatus,
    TimelineEntryId,
};
use koru_delegate_sqlite::BotExecutionRecord;
use koru_session_protocol::{AgentEvent, HandoverState, ManagedState};

impl Orchestrator {
    pub(super) fn bot_acknowledge_decision_delivery(
        &mut self,
        key: &BotRunKey,
        device: DeviceId,
        receipt: &BotDeliveryReceipt,
        record: &BotExecutionRecord,
    ) -> Result<(), RunError> {
        let run = key.run;
        // A child wait has no decision: its delivery is the child arm's (M6).
        // A ledger that does not model child requests has none.
        if let Ok(Some(child)) = self.bot_ledger()?.child_wait_execution(key.owner, run) {
            return self.bot_acknowledge_child_delivery(key, device, receipt, child);
        }
        let execution = self
            .bot_ledger()?
            .decision_wait_execution(key.owner, run)?
            .ok_or_else(publication_pending)?;
        let row = execution.current_attempt.clone();
        let session = row
            .provider_session
            .as_ref()
            .and_then(|provider| provider.session)
            .ok_or_else(|| refused("bot_fenced"))?;
        let send = record
            .initial_send
            .as_ref()
            .ok_or_else(|| refused("bot_fenced"))?;
        // The original attempt's initial turn, or only the exact recorded
        // successor whose continuation input was acknowledged.
        let (turn, successor) = if send.attempt == row.attempt && send.generation == row.generation
        {
            (send.turn.ok_or_else(|| refused("bot_fenced"))?, false)
        } else {
            let resume = execution
                .resume
                .as_ref()
                .ok_or_else(|| refused("bot_fenced"))?;
            let acknowledgment = match &resume.state {
                DecisionResumeState::InputAcknowledged { acknowledgment }
                | DecisionResumeState::Closed {
                    acknowledgment: Some(acknowledgment),
                } => acknowledgment,
                _ => return Err(refused("bot_fenced")),
            };
            let (attempt, generation, consumer, _) = resume.consumer_identity();
            if attempt != row.attempt || generation != row.generation || consumer != Some(session) {
                return Err(refused("bot_fenced"));
            }
            (acknowledgment.turn, true)
        };
        if receipt.attempt != row.attempt || receipt.generation != row.generation {
            return Err(refused("bot_fenced"));
        }
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
        // An exact retry reuses the retained transition, step and outcome, so
        // the C writer replays instead of minting a second Delivery Control.
        let retained = self
            .runs
            .get(&run)
            .and_then(|state| state.delivery_retry.clone())
            .filter(|retry| retry.receipt == *receipt);
        let retry = match retained {
            Some(retry) => retry,
            None => {
                // An earlier owed projection publishes first: a fresh Delivery
                // Control never supersedes an intermediate Control it follows.
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
                    let fence = BotExecutionFence {
                        owner: key.owner,
                        bot: key.bot,
                        run,
                        attempt: row.attempt,
                        generation: row.generation,
                        session,
                        turn: Some(turn),
                    };
                    if !self.bot_decision_delivery_completed(
                        &fence, device, &header, session, turn, successor,
                    )? {
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
                    transition: koru_delegate_core::decisions::DecisionTransitionId::mint(),
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
            // A lost reply is resolved by the exact transition, never by a
            // second Control or a recomputed outcome.
            if self
                .bot_ledger()?
                .decision_projection(key.owner, run, retry.transition)?
                .is_none()
            {
                return Err(error.into());
            }
        }
        // The receipt is durable; its header publication is the owed Control.
        // Delivery is not durable until that projection is acknowledged and
        // the fence ends, after which the ordinary owner finishes the task.
        Err(refused("bot_delivery_pending"))
    }

    /// The same native completion evidence the ordinary owner requires: live
    /// exact consumer under the Orchestrator's control, no pending effect,
    /// approval or handback, and the exact last native turn completed.
    pub(super) fn bot_decision_delivery_completed(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
        header: &koru_delegate_core::RunHeader,
        session: koru_delegate_core::SessionId,
        turn: koru_delegate_core::AgentTurnId,
        successor: bool,
    ) -> Result<bool, RunError> {
        // A successor's current turn is its acknowledged continuation turn,
        // which the stale-turn check (keyed on the initial send) cannot name.
        if self
            .bot_live_fence_inner(fence, device, true, !successor, false)
            .is_err()
            || header.status.control() != Some(ControlState::Orchestrator)
            || self.native_effects_pending(fence.run)?
        {
            return Ok(false);
        }
        let attempt = self.attempt_mut(fence.run)?;
        if !attempt.pending_approvals.is_empty()
            || attempt.release_owed
            || attempt.recovered_unowned_turn.is_some()
        {
            return Ok(false);
        }
        let Some(summary) = self.ports.sessions.summary(session)? else {
            return Ok(false);
        };
        if summary.id != session
            || summary.active_turn_id.is_some()
            || summary.managed != Some(ManagedState::Completed)
            || !matches!(
                summary.handover,
                HandoverState::Idle | HandoverState::Returned
            )
        {
            return Ok(false);
        }
        let observation = self.ports.sessions.observe(session)?;
        if observation
            .events
            .iter()
            .any(|event| event.session_id != session)
        {
            return Err(refused("bot_fenced"));
        }
        let terminal = observation.events.iter().rev().find(|event| {
            matches!(
                event.event,
                AgentEvent::TurnStarted { .. }
                    | AgentEvent::TurnCompleted { .. }
                    | AgentEvent::TurnFailed { .. }
                    | AgentEvent::TurnCancelled { .. }
            )
        });
        Ok(terminal.is_some_and(
            |event| matches!(event.event, AgentEvent::TurnCompleted { turn_id } if turn_id == turn),
        ))
    }
}
