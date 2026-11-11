// SPDX-License-Identifier: MPL-2.0
//! Terminal question cards use a fresh task, never the old native prompt.
use super::*;
use koru_ai_core::{AgentAnswerValue, AnswersRecord, NextMessageAnswers, QuestionsRecord};
use koru_delegate_core::RunId;

const RETAINED_CARDS: usize = 64;
const RETAINED_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub(in crate::worker) struct TerminalQuestionProof {
    prior: InferenceAttemptId,
    invocation: ToolInvocationId,
    source: BotToolSource,
    fence: BotExecutionFence,
    key: BotSubmissionKey,
    intent: BotSubmissionIntent,
    fingerprint: String,
    replay: BotReplayState,
    status: AttemptStatus,
    delivery: Option<BotDeliveryReceipt>,
}
impl TerminalQuestionProof {
    #[cfg(test)]
    pub(in crate::worker) fn fixture(
        prior: InferenceAttemptId,
        invocation: ToolInvocationId,
        source: BotToolSource,
        fence: BotExecutionFence,
        key: BotSubmissionKey,
        intent: BotSubmissionIntent,
        fingerprint: String,
        replay: BotReplayState,
        status: AttemptStatus,
        delivery: Option<BotDeliveryReceipt>,
    ) -> Self {
        Self {
            prior,
            invocation,
            source,
            fence,
            key,
            intent,
            fingerprint,
            replay,
            status,
            delivery,
        }
    }
    pub(in crate::worker) fn verify(
        &self,
        port: Option<&Arc<dyn BotDelegatePort>>,
    ) -> ApiResult<()> {
        let port = port.ok_or_else(|| bot_failure(BotError::Unavailable))?;
        // A read, on preparation's own lane: bounded, so a hung execution
        // owner holds a person's other requests for at most this long.
        match port.execute_within(
            DelegateCommand::Reconcile {
                key: self.key.clone(),
            },
            super::super::preparation::PREPARATION_OWNER_WITHIN,
        ) {
            Ok(BotReply::Admission { admission })
                if admission.key == self.key
                    && admission.run == self.fence.run
                    && matches!(
                        admission.state,
                        BotAdmissionState::Settled | BotAdmissionState::Refused
                    ) =>
            {
                Ok(())
            }
            _ => Err(failure(
                "chat_busy",
                "The previous Bot task has not settled yet.",
                true,
            )),
        }
    }
}

pub(in crate::worker) struct BotAnswer {
    pub question: BotQuestionContinuation,
    pub previous: RunId,
    pub prompt: String,
}

fn closed() -> Failure {
    failure(
        "questions_closed",
        koru_ai_core::AGENT_QUESTIONS_CLOSED,
        false,
    )
}

fn native_questions(
    questions: &[koru_ai_core::AgentQuestion],
) -> Vec<koru_session_protocol::AgentQuestion> {
    questions
        .iter()
        .map(|q| koru_session_protocol::AgentQuestion {
            id: q.id.clone(),
            header: q.header.clone(),
            question: q.question.clone(),
            options: q
                .options
                .iter()
                .map(|o| koru_session_protocol::QuestionOption {
                    label: o.label.clone(),
                    description: o.description.clone(),
                })
                .collect(),
            allow_other: q.allow_other,
            secret: q.secret,
        })
        .collect()
}

impl WorkerState {
    pub(in crate::worker) fn bot_terminal_question_capture(
        &self,
        command: &WorkerCommand,
    ) -> ApiResult<Option<TerminalQuestionProof>> {
        let WorkerCommand::SendTurn { request, .. } = command else {
            return Ok(None);
        };
        let Some(next) = &request.answers else {
            return Ok(None);
        };
        if self
            .repository
            .bot_for_chat(request.chat_id)
            .map_err(bot_failure)?
            .is_none()
            || self
                .repository
                .attempt_by_operation(request.operation_id)
                .map_err(ai_failure)?
                .is_some()
        {
            return Ok(None);
        }
        let row = self
            .repository
            .tool_activity(next.invocation)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        if row.chat_id != request.chat_id {
            return Err(not_found());
        }
        let prior = self
            .repository
            .attempt(row.attempt_id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        if !prior.status.is_terminal() {
            return Err(failure(
                "chat_busy",
                "The asking Bot task is still running.",
                true,
            ));
        }
        if !self.bot_next_message_ids.contains(&row.id)
            || self.next_message_rows.get(&row.id) != Some(&row.chat_id)
            || self
                .agent_live
                .get(&row.id)
                .is_none_or(|live| live.questions.is_empty())
        {
            return Err(closed());
        }
        let source = self
            .repository
            .bot_source_for_tool(row.attempt_id, row.id)
            .map_err(ai_failure)?
            .ok_or_else(closed)?;
        if source.kind != BotToolSourceKind::Questions {
            return Err(closed());
        }
        let replay = self
            .repository
            .bot_replay_state(row.attempt_id)
            .map_err(ai_failure)?;
        if replay.stop_reason.is_some() {
            return Err(closed());
        }
        let fence = replay.fence.clone().ok_or_else(closed)?;
        let receipt = self
            .repository
            .bot_submission_for_attempt(row.attempt_id)
            .map_err(ai_failure)?
            .ok_or_else(closed)?;
        let p = &receipt.intent.submission.provenance;
        if p.chat_id != row.chat_id.as_uuid()
            || receipt.intent.submission.request.run != fence.run
            || p.bot != fence.bot
            || p.owner != fence.owner
        {
            return Err(closed());
        }
        Ok(Some(TerminalQuestionProof {
            prior: row.attempt_id,
            invocation: row.id,
            source,
            fence,
            key: BotSubmissionKey {
                owner: p.owner,
                bot: p.bot,
                request: p.submission,
            },
            intent: receipt.intent,
            fingerprint: receipt.fingerprint,
            replay,
            status: prior.status,
            delivery: self
                .repository
                .bot_delivery_receipt(row.attempt_id)
                .map_err(ai_failure)?,
        }))
    }

    pub(in crate::worker) fn prepare_bot_answer(
        &mut self,
        context: &CommandContext,
        chat: &Chat,
        row: &ToolActivity,
        next: &NextMessageAnswers,
    ) -> ApiResult<Option<BotAnswer>> {
        if self
            .repository
            .bot_for_chat(chat.id)
            .map_err(bot_failure)?
            .is_none()
        {
            return Ok(None);
        }
        let proof = context
            .prepared
            .as_ref()
            .and_then(|p| p.terminal_question.as_ref())
            .ok_or_else(closed)?;
        let receipt = self
            .repository
            .bot_submission_for_attempt(row.attempt_id)
            .map_err(ai_failure)?
            .ok_or_else(closed)?;
        let prior = self
            .repository
            .attempt(row.attempt_id)
            .map_err(ai_failure)?
            .ok_or_else(closed)?;
        let replay = self
            .repository
            .bot_replay_state(row.attempt_id)
            .map_err(ai_failure)?;
        if proof.invocation != row.id
            || proof.prior != row.attempt_id
            || row.chat_id != chat.id
            || next.invocation != row.id
            || receipt.intent != proof.intent
            || receipt.fingerprint != proof.fingerprint
            || !prior.status.is_terminal()
            || prior.status != proof.status
            || replay != proof.replay
            || replay.stop_reason.is_some()
            || self
                .repository
                .bot_delivery_receipt(row.attempt_id)
                .map_err(ai_failure)?
                != proof.delivery
            || self
                .repository
                .bot_source_for_tool(row.attempt_id, row.id)
                .map_err(ai_failure)?
                .as_ref()
                != Some(&proof.source)
            || self
                .repository
                .bot_replay_state(row.attempt_id)
                .map_err(ai_failure)?
                .fence
                .as_ref()
                != Some(&proof.fence)
        {
            return Err(closed());
        }
        if row.approval != ToolApproval::Pending || row.status.is_terminal() {
            return Err(tool_decision_conflict());
        }
        let record = row
            .agent_input
            .as_deref()
            .and_then(QuestionsRecord::decode)
            .ok_or_else(closed)?;
        if !record.next_message
            || !self.bot_next_message_ids.contains(&row.id)
            || self.next_message_rows.get(&row.id) != Some(&chat.id)
        {
            return Err(closed());
        }
        let Some(live) = self
            .agent_live
            .get(&row.id)
            .filter(|live| !live.questions.is_empty())
        else {
            self.expire_bot_question(row.id)?;
            return Err(closed());
        };
        let questions = native_questions(&live.questions);
        let answers = super::controls::validate_bot_answers(&questions, next.answers.clone())?;
        let saved = koru_session_protocol::withhold_secrets(&questions, &answers);
        let answers_record = AnswersRecord {
            answers: saved
                .into_iter()
                .map(|answer| {
                    use koru_session_protocol::QuestionAnswerValue as V;
                    koru_ai_core::AgentAnswer {
                        question_id: answer.question_id,
                        value: match answer.value {
                            V::Option { label } => AgentAnswerValue::Option { label },
                            V::Text { text } => AgentAnswerValue::Text { text },
                            V::Withheld => AgentAnswerValue::Withheld,
                        },
                    }
                })
                .collect(),
        };
        answers_record.encode_lossless().ok_or_else(|| {
            failure(
                "invalid_request",
                "Those answers are too large to save intact.",
                false,
            )
        })?;
        // Actual secret values live only in this current-task prompt. Admission
        // and subsequent history carry the withheld AnswersRecord instead.
        let prompt = format!(
            "The user answered the prior conversation's questions:\n{}",
            serde_json::to_string(&(questions, answers)).map_err(|_| invariant_failure())?
        );
        super::preparation::encoded_size(&prompt).map_err(ai_failure)?;
        Ok(Some(BotAnswer {
            question: BotQuestionContinuation {
                prior_attempt: row.attempt_id,
                invocation: row.id,
                source: proof.source.clone(),
                answers: answers_record,
            },
            previous: proof.fence.run,
            prompt,
        }))
    }

    pub(in crate::worker) fn finish_bot_answer(
        &mut self,
        invocation: ToolInvocationId,
        attempt: InferenceAttemptId,
    ) -> ApiResult<()> {
        self.bot_next_message_ids.remove(&invocation);
        self.next_message_rows.remove(&invocation);
        self.agent_live.remove(&invocation);
        let row = self
            .repository
            .tool_activity(invocation)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        self.publish(AiEvent::ToolActivity {
            attempt_id: attempt,
            activity: row,
        });
        Ok(())
    }

    fn expire_bot_question(&mut self, invocation: ToolInvocationId) -> ApiResult<ToolActivity> {
        self.bot_next_message_ids.remove(&invocation);
        self.next_message_rows.remove(&invocation);
        self.agent_live.remove(&invocation);
        let at = Timestamp::now();
        self.agent_row_mut(invocation, |row| {
            row.status = ToolActivityStatus::Cancelled;
            row.approval = ToolApproval::Denied;
            row.completed_at = Some(at);
            row.error_code = Some(ToolErrorCode::Cancelled);
        })
        .map_err(ai_failure)
    }

    pub(super) fn remember_bot_question(
        &mut self,
        row: &mut ToolActivity,
        terminal: bool,
    ) -> Result<(), AiError> {
        if row.status.is_terminal() {
            self.bot_next_message_ids.remove(&row.id);
            self.next_message_rows.remove(&row.id);
            self.agent_live.remove(&row.id);
            return Ok(());
        }
        if !row
            .agent_input
            .as_deref()
            .and_then(QuestionsRecord::decode)
            .is_some_and(|r| r.next_message)
            || self.bot_next_message_ids.contains(&row.id)
        {
            return Ok(());
        }
        // Active definitions are already bounded by LiveChat. Local expiration
        // must wait until terminal persistence, or a later native resolution
        // would encounter a prematurely terminal activity row.
        if !terminal {
            self.next_message_rows.insert(row.id, row.chat_id);
            return Ok(());
        }
        let size = self
            .agent_live
            .get(&row.id)
            .map(|live| super::preparation::encoded_size(&live.questions))
            .transpose()?
            .unwrap_or(0);
        let used = self
            .bot_next_message_ids
            .iter()
            .try_fold(0usize, |sum, id| {
                let size = self
                    .agent_live
                    .get(id)
                    .map(|live| super::preparation::encoded_size(&live.questions))
                    .transpose()?
                    .unwrap_or(0);
                sum.checked_add(size).ok_or_else(context_too_large)
            })?;
        if size == 0
            || self.bot_next_message_ids.len() >= RETAINED_CARDS
            || used.saturating_add(size) > RETAINED_BYTES
        {
            *row = self
                .expire_bot_question(row.id)
                .map_err(|_| bot_route_error())?;
        } else {
            self.bot_next_message_ids.insert(row.id);
            self.next_message_rows.insert(row.id, row.chat_id);
        }
        Ok(())
    }

    pub(in crate::worker) fn bot_answer_history(
        &self,
        chat: ChatId,
        messages: &[Message],
        history: &mut Vec<AiRequestMessage>,
    ) -> ApiResult<()> {
        if self
            .repository
            .bot_for_chat(chat)
            .map_err(bot_failure)?
            .is_none()
        {
            return Ok(());
        }
        let mut rows = self.repository.tool_activities(chat).map_err(ai_failure)?;
        rows.sort_by_key(|row| (row.text_offset, row.proposed_at.millis(), row.id));
        let mut by_message = HashMap::<MessageId, Vec<ToolActivity>>::new();
        for row in rows {
            if row.approval == ToolApproval::Approved && row.agent_output.is_some() {
                by_message
                    .entry(row.assistant_message_id)
                    .or_default()
                    .push(row);
            }
        }
        if history.len()
            != messages
                .iter()
                .filter(|m| m.state == MessageState::Complete)
                .count()
        {
            return Err(invariant_failure());
        }
        let mut result = Vec::new();
        let mut bytes = 0usize;
        let mut requests = history.drain(..);
        for message in messages {
            if message.state == MessageState::Complete {
                let request = requests.next().ok_or_else(invariant_failure)?;
                bytes = bytes
                    .checked_add(request.markdown.len())
                    .ok_or_else(|| ai_failure(context_too_large()))?;
                if bytes > MAX_REPLAY_BYTES {
                    return Err(ai_failure(context_too_large()));
                }
                result.push(request);
            }
            // Accepted user input survives even when the reply that asked
            // failed. Its incomplete prose stays excluded from AI history.
            for row in by_message.remove(&message.id).unwrap_or_default() {
                let Some(answers) = row.agent_output.as_deref().and_then(AnswersRecord::decode)
                else {
                    continue;
                };
                let Some(questions) = row.agent_input.as_deref().and_then(QuestionsRecord::decode)
                else {
                    continue;
                };
                if !self
                    .repository
                    .bot_source_for_tool(row.attempt_id, row.id)
                    .map_err(ai_failure)?
                    .is_some_and(|source| source.kind == BotToolSourceKind::Questions)
                {
                    continue;
                }
                let markdown = koru_ai_core::render_prompt_text(
                    &serde_json::to_string(&(questions, answers))
                        .map_err(|_| invariant_failure())?,
                );
                bytes = bytes
                    .checked_add(markdown.len())
                    .ok_or_else(|| ai_failure(context_too_large()))?;
                if bytes > MAX_REPLAY_BYTES {
                    return Err(ai_failure(context_too_large()));
                }
                result.push(AiRequestMessage {
                    role: ChatRole::User,
                    markdown,
                    attachments: Vec::new(),
                });
            }
        }
        drop(requests);
        *history = result;
        Ok(())
    }
}
