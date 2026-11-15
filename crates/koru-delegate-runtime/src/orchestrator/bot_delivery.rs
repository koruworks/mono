// SPDX-License-Identifier: MPL-2.0

//! Durable AI response receipt reconciliation. This owner completes only the
//! admitted Bot task; it never turns a conversational response into a page.
use super::{Orchestrator, RunError, bots::refused};
use crate::RunFact;
use koru_delegate_core::{
    BotAdmissionState, BotDeliveryReceipt, BotExecutionFence, BotRunKey, ControlState, DeviceId,
    EventId, PublicationKind, RunStatus, StepKind, TimelineEntry, TimelineRef, TimelineRefs,
};
use koru_delegate_sqlite::AttemptPhase;
use koru_session_protocol::{AgentEvent, HandoverState, ManagedState};

mod decision;

impl Orchestrator {
    pub fn bot_acknowledge_delivery(
        &mut self,
        key: &BotRunKey,
        device: DeviceId,
        receipt: &BotDeliveryReceipt,
    ) -> Result<(), RunError> {
        let record = self.bot_record(key, device)?;
        if !receipt.valid() || receipt.run != key.run {
            return Err(refused("bot_fenced"));
        }
        // While the sole handle is held for decision publication the receipt
        // and its owed header publication go through one Delivery Control.
        self.restore_publication_owner(key.run)?;
        if self.run_publication_pending(key.run) {
            return self.bot_acknowledge_decision_delivery(key, device, receipt, &record);
        }
        let row = self
            .ports
            .store
            .current_attempt(key.run)?
            .ok_or_else(|| refused("bot_fenced"))?;
        let send = record
            .initial_send
            .as_ref()
            .ok_or_else(|| refused("bot_fenced"))?;
        let turn = send.turn.ok_or_else(|| refused("bot_fenced"))?;
        let session = row
            .provider_session
            .as_ref()
            .and_then(|key| key.session)
            .ok_or_else(|| refused("bot_fenced"))?;
        if receipt.attempt != row.attempt
            || receipt.generation != row.generation
            || send.attempt != row.attempt
            || send.generation != row.generation
        {
            return Err(refused("bot_fenced"));
        }
        let header = self.header(key.run)?;
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
        // The receipt and its publication ID commit together before any
        // canonical completion. An exact legacy receipt repairs its missing ID.
        let publication = self.bot_ledger()?.record_bot_delivery(receipt)?;
        if header.chat_delivery.is_none() {
            self.open(key.run)?
                .doc
                .commit(&mut |document| document.record_bot_chat_delivery(receipt))?;
        }
        if matches!(
            header.status,
            RunStatus::Failed { .. } | RunStatus::Cancelled | RunStatus::Interrupted { .. }
        ) {
            // Applied is delivery history, not a successful task. Never revive
            // an absorbing AI/Run attempt or infer that unknown work stopped.
            self.bot_settle_delivery(key.run)?;
            // Canonical interruption is absorbing even when later exact
            // evidence upgrades every unknown result before cleanup retries.
            if matches!(
                header.status,
                RunStatus::Interrupted {
                    prior_attempt: koru_delegate_core::PriorAttempt::Unknown
                }
            ) && !self.native_effects_pending(key.run)?
                && record.state != BotAdmissionState::Settled
            {
                self.ports.mcp.revoke(key.run);
                self.bot_finish_delivery_cleanup(key.run, session, row.retired)?;
            }
            return Ok(());
        }
        if header.status == RunStatus::Completed {
            self.bot_settle_delivery(key.run)?;
            self.ports.mcp.revoke(key.run);
            self.bot_publish_delivery_completion(key.run, receipt, publication)?;
            if self.native_effects_pending(key.run)? {
                return Err(refused("bot_delivery_pending"));
            }
            if record.state != BotAdmissionState::Settled {
                self.bot_finish_delivery_cleanup(key.run, session, row.retired)?;
            }
            return Ok(());
        }
        let fence = BotExecutionFence {
            owner: key.owner,
            bot: key.bot,
            run: key.run,
            attempt: row.attempt,
            generation: row.generation,
            session,
            turn: Some(turn),
        };
        // This reconciles an acknowledged initial turn and exact AI receipt.
        // Preserve live identity/controller checks without asking current policy
        // to reauthorize historical delivery; no new input or effect is sent.
        if self
            .bot_live_fence_inner(&fence, device, true, true, false)
            .is_err()
            || header.status.control() != Some(ControlState::Orchestrator)
            || self.native_effects_pending(key.run)?
        {
            return Err(refused("bot_delivery_pending"));
        }
        let attempt = self.attempt_mut(key.run)?;
        if !attempt.pending_approvals.is_empty()
            || attempt.release_owed
            || attempt.recovered_unowned_turn.is_some()
        {
            return Err(refused("bot_delivery_pending"));
        }
        // A fresh finite owner observation proves the exact last native turn.
        // The AI receipt alone cannot claim an unrelated human turn or a gap.
        let summary = self
            .ports
            .sessions
            .summary(session)?
            .ok_or_else(|| refused("bot_delivery_pending"))?;
        if summary.id != session
            || summary.active_turn_id.is_some()
            || summary.managed != Some(ManagedState::Completed)
            || !matches!(
                summary.handover,
                HandoverState::Idle | HandoverState::Returned
            )
        {
            return Err(refused("bot_delivery_pending"));
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
        if !terminal.is_some_and(|event| {
            matches!(event.event,
            AgentEvent::TurnCompleted { turn_id } if turn_id == turn)
        }) {
            return Err(refused("bot_delivery_pending"));
        }
        let refs = TimelineRefs::new([TimelineRef::Attempt {
            attempt: receipt.attempt,
        }])
        .expect("one reference");
        let entry = TimelineEntry::new(StepKind::Completed, refs, self.ports.clock.now_ms());
        if self.native_outcomes_unknown(key.run)? {
            let mut entry = entry;
            entry.kind = StepKind::Interrupted {
                prior_attempt: koru_delegate_core::PriorAttempt::Unknown,
            };
            self.open(key.run)?
                .doc
                .commit(&mut |document| document.interrupt_bot_delivery_unknown(receipt, &entry))?;
            self.bot_settle_delivery(key.run)?;
            self.ports.mcp.revoke(key.run);
            self.bot_finish_delivery_cleanup(key.run, session, false)?;
            return Ok(());
        }
        self.open(key.run)?
            .doc
            .commit(&mut |document| document.complete_bot_chat_delivery(receipt, &entry))?;
        self.bot_settle_delivery(key.run)?;
        self.ports.mcp.revoke(key.run);
        self.bot_publish_delivery_completion(key.run, receipt, publication)?;
        self.bot_finish_delivery_cleanup(key.run, session, false)?;
        Ok(())
    }

    fn bot_finish_delivery_cleanup(
        &mut self,
        run: koru_delegate_core::RunId,
        session: koru_delegate_core::SessionId,
        retired: bool,
    ) -> Result<(), RunError> {
        // Completion may have committed before a transport or cleanup failure.
        // A fresh summary is advisory; only the session owner's current lease
        // check can prevent takeover between that observation and termination.
        self.arm_bot_cleanup(run)?;
        if !retired {
            if let Some(summary) = self.ports.sessions.summary(session)? {
                if summary.id != session
                    || summary.active_turn_id.is_some()
                    || !matches!(
                        summary.handover,
                        HandoverState::Idle | HandoverState::Returned
                    )
                {
                    return Err(refused("bot_delivery_pending"));
                }
                if !matches!(
                    summary.process,
                    koru_session_protocol::ProcessState::Exited { .. }
                        | koru_session_protocol::ProcessState::Failed { .. }
                ) {
                    self.ports
                        .sessions
                        .terminate_owned(session)
                        .map_err(|_| refused("bot_delivery_pending"))?;
                }
            }
            // No summary never authorizes the lease-free terminate fallback.
            // The creation owner must positively prove ended/fenced instead.
            if !self.bot_prove_end(run, false)? {
                return Err(refused("bot_stop_unconfirmed"));
            }
        }
        self.bot_end_attempt(run, AttemptPhase::Ended, false)
    }

    fn bot_settle_delivery(&mut self, run: koru_delegate_core::RunId) -> Result<(), RunError> {
        self.settle(run)?;
        if !self.open(run)?.doc.durable() {
            return Err(refused("bot_delivery_pending"));
        }
        Ok(())
    }

    fn bot_publish_delivery_completion(
        &mut self,
        run: koru_delegate_core::RunId,
        receipt: &BotDeliveryReceipt,
        event: EventId,
    ) -> Result<(), RunError> {
        let header = self.header(run)?;
        if header.status != RunStatus::Completed || header.chat_delivery.as_ref() != Some(receipt) {
            return Err(refused("bot_fenced"));
        }
        let fact = RunFact::Completed {
            run,
            attempt: receipt.attempt,
            generation: receipt.generation,
            artifacts: u32::try_from(header.artifacts.len()).unwrap_or(u32::MAX),
        };
        // Generic enqueue intentionally deduplicates by ID alone. This Bot-only
        // path validates the retained payload before using that existing seam.
        let fact = if let Some(stored) = self.ports.store.publication(event)? {
            let expected = serde_json::to_value(&fact).map_err(|_| refused("bot_conflict"))?;
            let body: serde_json::Value =
                serde_json::from_str(stored.body.as_str()).map_err(|_| refused("bot_conflict"))?;
            if stored.run != run || stored.kind != PublicationKind::Lifecycle || body != expected {
                return Err(refused("bot_conflict"));
            }
            serde_json::from_value(body).map_err(|_| refused("bot_conflict"))?
        } else {
            fact
        };
        self.publish_as(run, event, &fact)?;
        Ok(())
    }
}
