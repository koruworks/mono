// SPDX-License-Identifier: MPL-2.0

use super::{Orchestrator, RunError, bots::refused};
use crate::RunFact;
use koru_delegate_core::{
    AgentEventObservation, AgentTurnId, BotExecutionFence, BotRunKey, ControlState, DeviceId,
    PendingInput, RunEvent, RunStatus, StepKind, StreamSequence, TimelineRef, TimelineRefs,
};
use koru_delegate_sqlite::{
    AttemptPhase, BOT_ACTIVE_BUDGET_MS, BotExecutionRecord, BotInitialSend,
};

impl Orchestrator {
    pub(crate) fn bot_live_fence(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
        controls: bool,
        exact_turn: bool,
    ) -> Result<BotExecutionRecord, RunError> {
        let record = self.bot_record(
            &BotRunKey {
                owner: fence.owner,
                bot: fence.bot,
                run: fence.run,
            },
            device,
        )?;
        let row = self
            .ports
            .store
            .current_attempt(fence.run)?
            .ok_or_else(|| refused("bot_fenced"))?;
        if row.retired
            || row.phase.is_terminal()
            || row.attempt != fence.attempt
            || row.generation != fence.generation
            || row.provider_session.and_then(|key| key.session) != Some(fence.session)
        {
            return Err(refused("bot_fenced"));
        }
        let status = self.status(fence.run)?;
        let attempt = self
            .open(fence.run)?
            .attempt
            .as_ref()
            .ok_or_else(|| refused("bot_fenced"))?;
        if attempt.id != fence.attempt
            || attempt.generation != fence.generation
            || attempt.session != Some(fence.session)
        {
            return Err(refused("bot_fenced"));
        }
        let current = attempt
            .orchestrator_turn
            .or_else(|| record.initial_send.as_ref().and_then(|send| send.turn));
        if exact_turn && fence.turn != current {
            return Err(refused("bot_turn_stale"));
        }
        if controls {
            if !attempt.holds_lease
                || attempt.release_owed
                || attempt.recovered_unowned_turn.is_some()
                || (!matches!(status, RunStatus::Starting)
                    && status.control() != Some(ControlState::Orchestrator))
            {
                return Err(refused("bot_controller_required"));
            }
            if record.active_elapsed_at(self.ports.clock.now_ms()) >= BOT_ACTIVE_BUDGET_MS {
                return Err(refused("bot_budget_exhausted"));
            }
        }
        Ok(record)
    }

    pub fn bot_observe(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
        after: Option<StreamSequence>,
    ) -> Result<AgentEventObservation, RunError> {
        self.bot_record(
            &BotRunKey {
                owner: fence.owner,
                bot: fence.bot,
                run: fence.run,
            },
            device,
        )?;
        let row = self
            .ports
            .store
            .current_attempt(fence.run)?
            .ok_or_else(|| refused("bot_fenced"))?;
        if row.attempt != fence.attempt
            || row.generation != fence.generation
            || row.provider_session.and_then(|key| key.session) != Some(fence.session)
        {
            return Err(refused("bot_fenced"));
        }
        // Immutable identity permits retained terminal replay too. Only mutation
        // paths require a live attempt/controller; observation takes no lease.
        let mut observation = self.ports.sessions.observe(fence.session)?;
        if observation
            .events
            .iter()
            .any(|event| event.session_id != fence.session)
        {
            return Err(refused("bot_observation_mismatch"));
        }
        // Filtering never acknowledges delivery or changes the persisted cursor.
        if let Some(after) = after {
            observation.events.retain(|event| event.sequence > after);
        }
        if let Some(attempt) = self
            .runs
            .get_mut(&fence.run)
            .and_then(|state| state.attempt.as_mut())
        {
            if attempt.id == fence.attempt
                && attempt.generation == fence.generation
                && attempt.session == Some(fence.session)
            {
                attempt.bot_observed = true;
            }
        }
        Ok(observation)
    }

    pub fn bot_begin_turn(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
        mut input: PendingInput,
    ) -> Result<AgentTurnId, RunError> {
        let record = self.bot_live_fence(fence, device, true, false)?;
        if fence.turn.is_some()
            || !self
                .open(fence.run)?
                .attempt
                .as_ref()
                .expect("validated")
                .bot_observed
        {
            return Err(refused("bot_observation_required"));
        }
        let current = self
            .open(fence.run)?
            .attempt
            .as_ref()
            .expect("validated")
            .orchestrator_turn;
        if current.is_some() && current != record.initial_send.as_ref().and_then(|send| send.turn) {
            return Err(refused("bot_turn_stale"));
        }
        let workspace = self
            .ports
            .mcp
            .run_workspace(fence.run)?
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| refused("bot_folder_unavailable"))?;
        // AI supplies uncomposed history/current task. The owner adds its
        // existing context-file/tool instructions once, and fingerprints the
        // actual forwarded input. No history or preamble is persisted here.
        let preamble = self.ports.preamble.resolve();
        input.text = super::start::compose_first_submission(
            &preamble.text,
            fence.run,
            &workspace,
            &input.text,
        );
        // Struct field order and ordered input vectors are semantic. PendingInput
        // has no unordered maps; the digest binds forwarded bytes, not a transcript.
        let encoded = serde_json::to_vec(&(fence, &input)).map_err(|_| refused("bot_invalid"))?;
        let send = BotInitialSend {
            attempt: fence.attempt,
            generation: fence.generation,
            key: input.idempotency_key,
            fingerprint: *blake3::hash(&encoded).as_bytes(),
            turn: None,
        };
        let mut durable = self
            .bot_ledger()?
            .prepare_bot_initial_send(fence.run, &send)?;
        let turn = match durable.turn {
            Some(turn) => turn,
            None => {
                let turn = self.ports.sessions.bot_submit(fence.session, input)?;
                durable.turn = Some(turn);
                self.bot_ledger()?
                    .acknowledge_bot_initial_send(fence.run, &durable)?;
                turn
            }
        };
        if self
            .open(fence.run)?
            .attempt
            .as_ref()
            .expect("validated")
            .bot_terminal_turn
            != Some(turn)
        {
            self.open(fence.run)?
                .attempt
                .as_mut()
                .expect("validated")
                .orchestrator_turn = Some(turn);
        }
        self.bot_mark_started(fence.run)?;
        Ok(turn)
    }

    pub(crate) fn bot_mark_started(
        &mut self,
        run: koru_delegate_core::RunId,
    ) -> Result<(), RunError> {
        if matches!(self.status(run)?, RunStatus::Starting) {
            let attempt = self.attempt_mut(run)?;
            let (id, generation, session) = (
                attempt.id,
                attempt.generation,
                attempt.session.ok_or_else(|| refused("bot_fenced"))?,
            );
            self.ports
                .store
                .advance(run, generation, AttemptPhase::Running)?;
            self.apply(run, &RunEvent::ProviderStarted)?;
            self.step(
                run,
                StepKind::Started,
                TimelineRefs::new([
                    TimelineRef::Attempt { attempt: id },
                    TimelineRef::Session { session },
                ])
                .expect("two refs"),
                Some(RunFact::Started {
                    run,
                    attempt: id,
                    generation,
                    session,
                }),
            )?;
        }
        Ok(())
    }

    pub fn bot_steer(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
        input: PendingInput,
    ) -> Result<AgentTurnId, RunError> {
        self.bot_live_fence(fence, device, true, true)?;
        let turn = fence.turn.ok_or_else(|| refused("bot_turn_stale"))?;
        if self
            .open(fence.run)?
            .attempt
            .as_ref()
            .expect("validated")
            .orchestrator_turn
            != Some(turn)
        {
            return Err(refused("bot_turn_stale"));
        }
        Ok(self.ports.sessions.bot_steer(fence.session, turn, input)?)
    }

    pub fn bot_answer_approval(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
        approval: koru_delegate_core::ApprovalId,
        decision: koru_delegate_core::ApprovalDecision,
        choice: Option<koru_delegate_core::ApprovalChoiceId>,
    ) -> Result<(), RunError> {
        self.bot_live_fence(fence, device, true, true)?;
        let turn = fence.turn.ok_or_else(|| refused("bot_turn_stale"))?;
        if self
            .open(fence.run)?
            .attempt
            .as_ref()
            .expect("validated")
            .pending_approvals
            .get(&approval)
            != Some(&turn)
        {
            return Err(refused("bot_approval_stale"));
        }
        Ok(self
            .ports
            .sessions
            .bot_answer_approval(fence.session, approval, decision, choice)?)
    }

    pub fn bot_answer_questions(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
        request: koru_delegate_core::QuestionRequestId,
        answers: Vec<koru_delegate_core::QuestionAnswer>,
    ) -> Result<(), RunError> {
        self.bot_live_fence(fence, device, true, true)?;
        let turn = fence.turn.ok_or_else(|| refused("bot_turn_stale"))?;
        if self
            .open(fence.run)?
            .attempt
            .as_ref()
            .expect("validated")
            .pending_questions
            .get(&request)
            != Some(&turn)
        {
            return Err(refused("bot_question_stale"));
        }
        Ok(self
            .ports
            .sessions
            .bot_answer_questions(fence.session, request, answers)?)
    }
}

impl Orchestrator {
    pub fn bot_ingest_attachment(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
        id: koru_delegate_core::AttachmentId,
        source: koru_delegate_core::AttachmentSource,
        data: String,
    ) -> Result<koru_delegate_core::Attachment, RunError> {
        self.bot_live_fence(fence, device, true, true)?;
        // Existing session upload cap is 8MiB decoded; the session owner also
        // validates decoded size/content. Private IPC has a separate 16MiB cap.
        if data.len() > (8 * 1024 * 1024 + 2) / 3 * 4 {
            return Err(refused("bot_attachment_too_large"));
        }
        Ok(self
            .ports
            .sessions
            .bot_ingest_attachment(fence.session, id, source, data)?)
    }
    pub fn bot_remove_attachment(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
        id: koru_delegate_core::AttachmentId,
    ) -> Result<koru_delegate_core::Attachment, RunError> {
        self.bot_live_fence(fence, device, true, true)?;
        Ok(self
            .ports
            .sessions
            .bot_remove_attachment(fence.session, id)?)
    }
    pub fn bot_attachments(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
    ) -> Result<Vec<koru_delegate_core::Attachment>, RunError> {
        self.bot_live_fence(fence, device, false, true)?;
        Ok(self.ports.sessions.bot_attachments(fence.session)?)
    }
}

impl Orchestrator {
    pub(crate) fn bot_refresh_active_clock(
        &mut self,
        run: koru_delegate_core::RunId,
    ) -> Result<(), RunError> {
        let Some(attempt) = self.runs.get(&run).and_then(|state| state.attempt.as_ref()) else {
            return Ok(());
        };
        if !attempt.is_bot {
            return Ok(());
        }
        let proven_idle = attempt.bot_terminal_turn.is_some()
            && attempt.orchestrator_turn.is_none()
            && attempt.recovered_unowned_turn.is_none()
            && attempt.turns.is_empty();
        let (id, generation, active) = (
            attempt.id,
            attempt.generation,
            !proven_idle
                && (attempt.pending_approvals.is_empty() || !attempt.approval_evidence_complete),
        );
        self.bot_ledger()?
            .set_bot_active(run, id, generation, active)?;
        Ok(())
    }
}
