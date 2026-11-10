// SPDX-License-Identifier: MPL-2.0

//! One writer transaction for admission, folder reservation and the AI turn.
use super::*;
use koru_ai_core::bots::BotSubmissionIntent;
use koru_ai_core::{AgentAnswerValue, AnswersRecord, QuestionsRecord};
use rusqlite::TransactionBehavior;

impl Repository {
    pub fn prepare_bot_chat(
        &mut self,
        preparation: &BotChatPreparation,
        intent: &BotSubmissionIntent,
        fingerprint: &str,
        expected_revision: u64,
        now: Timestamp,
    ) -> Result<InferenceAttempt, BotPreparationError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_failed)?;
        super::super::bots::admission::prepare_bot_submission_tx(
            &tx,
            intent,
            fingerprint,
            expected_revision,
            now.millis(),
        )?;
        let attempt = prepare_association_on(&tx, preparation, &intent.submission, now)?;
        tx.commit().map_err(storage_failed)?;
        Ok(attempt)
    }
}

pub(super) fn prepare_association_on(
    tx: &Connection,
    preparation: &BotChatPreparation,
    submission: &BotSubmission,
    now: Timestamp,
) -> Result<InferenceAttempt, AiError> {
    validate_prepared_submission(tx, preparation, submission)?;
    let attempt = preparation.attempt();
    let assistant = preparation.assistant();
    let p = &submission.provenance;
    let existing: Option<String> = tx
        .query_row(
            "SELECT run_id FROM bot_chat_runs WHERE ai_attempt_id=?1",
            [attempt.id.to_storage()],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage_failed)?;
    if let Some(run) = existing {
        if run != submission.request.run.to_storage() {
            return Err(invariant_failed());
        }
        let current =
            query_attempt(tx, "id", &attempt.id.to_storage())?.ok_or_else(invariant_failed)?;
        if current.operation_id != attempt.operation_id
            || current.chat_id != attempt.chat_id
            || current.assistant_message_id != attempt.assistant_message_id
        {
            return Err(invariant_failed());
        }
        validate_link(tx, attempt.id, submission)?;
        validate_preparation(tx, preparation, submission, true)?;
        return Ok(current);
    }
    // Exact receipt retries above remain readable after a later pause/settings
    // change; a new AI turn must still satisfy current admission fences.
    let admissible: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM bots b
        JOIN bot_command_receipts r ON r.bot_id=b.bot_id
        WHERE b.bot_id=?1 AND b.revision=?2 AND b.policy_revision=?3 AND b.paused=0
        AND r.user_scope_id=?4 AND r.request_id=?5 AND r.stage='prepared')",
            params![
                p.bot.to_storage(),
                required_u64(p.config_revision)?,
                required_u64(p.policy_revision)?,
                p.owner.user_scope.to_string(),
                p.submission.to_storage()
            ],
            |r| r.get(0),
        )
        .map_err(storage_failed)?;
    let busy: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE chat_id=?1 AND role='assistant' AND state='pending')",
        [assistant.chat_id.to_storage()], |r|r.get(0)).map_err(storage_failed)?;
    if !admissible
        || busy
        || attempt.status != AttemptStatus::Pending
        || query_attempt(tx, "operation_id", &attempt.operation_id.to_string())?.is_some()
    {
        return Err(invariant_failed());
    }
    validate_preparation(tx, preparation, submission, false)?;
    let current = match preparation {
        BotChatPreparation::Turn(turn) => mutations::begin_turn_on(tx, turn)?,
        BotChatPreparation::Retry { retry, .. }
        | BotChatPreparation::AnsweredContinuation { retry, .. } => {
            mutations::begin_retry_on(tx, retry)?
        }
    };
    tx.execute("INSERT INTO bot_chat_runs (ai_attempt_id,user_scope_id,owner_device_id,submission_id,
        bot_id,chat_id,assistant_message_id,run_id,record_workspace_id,continued_from_run_id,created_at_ms)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params![current.id.to_storage(),p.owner.user_scope.to_string(),
            p.owner.device.to_string(),p.submission.to_storage(),p.bot.to_storage(),p.chat_id.to_string(),
            assistant.id.to_storage(),submission.request.run.to_storage(),submission.workspace.to_string(),
            p.continued_from.map(|id|id.to_storage()),now.millis()]).map_err(storage_failed)?;
    tx.execute(
        "INSERT INTO bot_run_replay(ai_attempt_id,updated_at_ms) VALUES (?1,?2)",
        params![current.id.to_storage(), now.millis()],
    )
    .map_err(storage_failed)?;
    if let BotChatPreparation::AnsweredContinuation { question, .. } = preparation {
        let mut row = query_tool_activity(tx, question.invocation)?.ok_or_else(invariant_failed)?;
        row.agent_output = Some(
            question
                .answers
                .encode_lossless()
                .ok_or_else(invariant_failed)?,
        );
        row.status = ToolActivityStatus::Complete;
        row.approval = ToolApproval::Approved;
        row.started_at.get_or_insert(now.max(row.proposed_at));
        row.completed_at = Some(now.max(row.proposed_at));
        mutations::advance_agent_activity_on(tx, &row)?;
    }
    Ok(current)
}

fn validate_preparation(
    tx: &Connection,
    preparation: &BotChatPreparation,
    submission: &BotSubmission,
    replayed: bool,
) -> Result<(), AiError> {
    let prior = match preparation {
        BotChatPreparation::Turn(turn) => {
            validate_turn(turn)?;
            return Ok(());
        }
        BotChatPreparation::Retry { prior_attempt, .. } => *prior_attempt,
        BotChatPreparation::AnsweredContinuation { question, .. } => question.prior_attempt,
    };
    let p = &submission.provenance;
    let prior_run = p.continued_from.ok_or_else(invariant_failed)?;
    let linked: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM bot_chat_runs WHERE ai_attempt_id=?1
        AND user_scope_id=?2 AND owner_device_id=?3 AND bot_id=?4 AND chat_id=?5 AND run_id=?6)",
            params![
                prior.to_storage(),
                p.owner.user_scope.to_string(),
                p.owner.device.to_string(),
                p.bot.to_storage(),
                p.chat_id.to_string(),
                prior_run.to_storage()
            ],
            |r| r.get(0),
        )
        .map_err(storage_failed)?;
    let previous = query_attempt(tx, "id", &prior.to_storage())?.ok_or_else(invariant_failed)?;
    if !linked || !previous.status.is_terminal() {
        return Err(invariant_failed());
    }
    match preparation {
        BotChatPreparation::Retry { retry, .. } => {
            if !matches!(
                previous.status,
                AttemptStatus::Failed | AttemptStatus::Interrupted
            ) || retry.assistant_message.origin_message_id != previous.assistant_message_id
            {
                return Err(invariant_failed());
            }
            // An assistant-only answer/retry has no corresponding user bubble.
            // Its prior immutable admission, not nearest Markdown, is the task.
            let prior_request: String = tx
                .query_row(
                    "SELECT submission_id FROM bot_chat_runs WHERE ai_attempt_id=?1",
                    [prior.to_storage()],
                    |r| r.get(0),
                )
                .map_err(storage_failed)?;
            let prior_request = prior_request
                .parse::<koru_ai_core::bots::BotRequestId>()
                .map_err(|_| invariant_failed())?;
            let original = super::super::bots::admission::read_bot_submission_on(
                tx,
                p.owner.user_scope,
                prior_request,
            )
            .map_err(|_| invariant_failed())?
            .ok_or_else(invariant_failed)?;
            let original_binding = &original.intent.submission.provenance;
            if original_binding.owner != p.owner
                || original_binding.bot != p.bot
                || original_binding.chat_id != p.chat_id
                || original.intent.submission.request.run != prior_run
                || original_binding.ai_attempt_id != Some(prior.as_uuid())
                || original_binding.assistant_message_id
                    != previous.assistant_message_id.map(|id| id.as_uuid())
            {
                return Err(invariant_failed());
            }
            let text = &original.intent.submission.input.text;
            // Missing secret input requires explicit re-entry, never an automatic
            // retry that sends the durable Withheld marker as the answer.
            let withheld = AnswersRecord::decode(text).is_some_and(|record| {
                record
                    .answers
                    .iter()
                    .any(|a| matches!(a.value, AgentAnswerValue::Withheld))
            });
            if text != &submission.input.text || withheld {
                return Err(invariant_failed());
            }
        }
        BotChatPreparation::AnsweredContinuation { question, .. } => {
            if question.source.kind != BotToolSourceKind::Questions
                || tool_source_on(tx, prior, &question.source)? != Some(question.invocation)
            {
                return Err(invariant_failed());
            }
            let row = query_tool_activity(tx, question.invocation)?.ok_or_else(invariant_failed)?;
            let record = row
                .agent_input
                .as_deref()
                .and_then(QuestionsRecord::decode)
                .ok_or_else(invariant_failed)?;
            let encoded = question
                .answers
                .encode_lossless()
                .ok_or_else(invariant_failed)?;
            if row.attempt_id != prior
                || row.chat_id.as_uuid() != p.chat_id
                || !record.next_message
                || encoded != submission.input.text
                || record.questions.len() != question.answers.answers.len()
            {
                return Err(invariant_failed());
            }
            let mut ids = std::collections::BTreeSet::new();
            for answer in &question.answers.answers {
                let q = record
                    .questions
                    .iter()
                    .find(|q| q.id == answer.question_id)
                    .ok_or_else(invariant_failed)?;
                if !ids.insert(&answer.question_id)
                    || (q.secret != matches!(answer.value, AgentAnswerValue::Withheld))
                {
                    return Err(invariant_failed());
                }
            }
            if replayed {
                if row.status != ToolActivityStatus::Complete
                    || row.approval != ToolApproval::Approved
                    || row.agent_output.as_deref() != Some(encoded.as_str())
                {
                    return Err(invariant_failed());
                }
            } else if row.status.is_terminal() || row.approval != ToolApproval::Pending {
                return Err(invariant_failed());
            }
        }
        BotChatPreparation::Turn(_) => unreachable!(),
    }
    Ok(())
}
