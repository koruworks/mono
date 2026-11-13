// SPDX-License-Identifier: MPL-2.0
//! Exact Bot context authority and immutable message-level retry provenance.
use super::*;
use std::collections::HashSet;

type RefKey = (MessageId, DocumentId);

fn message(connection: &Connection, id: MessageId) -> Result<Option<Message>, AiError> {
    connection
        .query_row(
            "SELECT id,chat_id,ordinal,role,state,markdown,origin_message_id,provider,
            requested_model,resolved_model,error_class,created_at_ms,completed_at_ms
         FROM messages WHERE id=?1",
            [id.to_storage()],
            message_from_row,
        )
        .optional()
        .map_err(storage_failed)
}

/// Return only existing refs that a verified Bot context may retain unchanged.
pub(in crate::repository) fn retained_refs(
    connection: &Connection,
    current: &InferenceAttempt,
    context: &ContextCommit,
) -> Result<HashSet<RefKey>, AiError> {
    let mut retained = HashSet::new();
    if context.document_refs.is_empty() && context.pinned_documents.is_empty() {
        return Ok(retained);
    }
    let Some(receipt) = preparation::bot_submission_for_attempt_on(connection, current.id)? else {
        let bot_chat: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM bot_chats WHERE chat_id=?1)",
                [current.chat_id.map(|chat| chat.to_storage())],
                |r| r.get(0),
            )
            .map_err(storage_failed)?;
        if bot_chat {
            return Err(invariant_failed());
        }
        return Ok(retained);
    };
    let submission = &receipt.intent.submission;
    let provenance = &submission.provenance;
    let chat = current.chat_id.ok_or_else(invariant_failed)?;
    let assistant_id = current.assistant_message_id.ok_or_else(invariant_failed)?;
    if provenance.chat_id != chat.as_uuid()
        || provenance.assistant_message_id != Some(assistant_id.as_uuid())
    {
        return Err(invariant_failed());
    }
    let assistant = message(connection, assistant_id)?.ok_or_else(invariant_failed)?;
    if assistant.chat_id != chat || assistant.role != ChatRole::Assistant {
        return Err(invariant_failed());
    }
    let answer_anchor =
        provenance.continued_from.is_some() && assistant.origin_message_id.is_none();
    let selected = if let Some(previous_run) = provenance.continued_from {
        let (prior_attempt, prior_id): (String, String) = connection
            .query_row(
                "SELECT ai_attempt_id,assistant_message_id FROM bot_chat_runs WHERE run_id=?1 AND
                user_scope_id=?2 AND owner_device_id=?3 AND bot_id=?4 AND chat_id=?5",
                params![
                    previous_run.to_storage(),
                    provenance.owner.user_scope.to_string(),
                    provenance.owner.device.to_string(),
                    provenance.bot.to_storage(),
                    chat.to_storage()
                ],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(storage_failed)?;
        let prior_id = MessageId::parse(&prior_id).ok_or_else(invariant_failed)?;
        if assistant.origin_message_id.is_some_and(|id| id != prior_id) {
            return Err(invariant_failed());
        }
        if assistant.origin_message_id.is_none() {
            validate_answered_question(connection, &prior_attempt, chat, &submission.input.text)?;
        }
        let prior = message(connection, prior_id)?.ok_or_else(invariant_failed)?;
        if prior.chat_id != chat
            || prior.ordinal >= assistant.ordinal
            || prior.role != ChatRole::Assistant
        {
            return Err(invariant_failed());
        }
        prior
    } else {
        if assistant.origin_message_id.is_some() {
            return Err(invariant_failed());
        }
        assistant
    };
    let user =
        if answer_anchor {
            None
        } else {
            original_bot_task_user(
                &selected,
                &submission.input.text,
                |id| message(connection, id),
                |chat, ordinal| {
                    connection.query_row(
            "SELECT id,chat_id,ordinal,role,state,markdown,origin_message_id,provider,
                requested_model,resolved_model,error_class,created_at_ms,completed_at_ms
             FROM messages WHERE chat_id=?1 AND ordinal=?2",
            params![chat.to_storage(),ordinal], message_from_row,
        ).optional().map_err(storage_failed)
                },
            )?
        };
    if user.is_none() && provenance.continued_from.is_none() {
        return Err(invariant_failed());
    }
    let anchor = user.map_or(selected.id, |user| user.id);
    let captured: HashSet<_> = submission
        .request
        .context
        .iter()
        .filter_map(|pin| match pin {
            koru_delegate_core::RunContextItem::Document {
                workspace,
                document,
                ..
            } if *workspace == submission.workspace => Some(*document),
            _ => None,
        })
        .collect();
    let pins: HashSet<_> = context
        .pinned_documents
        .iter()
        .map(|pin| pin.document_id)
        .collect();
    let refs: HashSet<_> = context
        .document_refs
        .iter()
        .map(|reference| reference.document_id)
        .collect();
    if pins != refs
        || refs.len() > 16
        || pins.len() != context.pinned_documents.len()
        || refs.len() != context.document_refs.len()
        || !refs.is_subset(&captured)
        || context
            .pinned_documents
            .iter()
            .any(|pin| pin.chat_id != chat || !pin.pinned)
    {
        return Err(invariant_failed());
    }
    for reference in &context.document_refs {
        if reference.message_id != anchor
            || reference.relation != DocumentRelation::Context
            || reference
                .source_revision
                .as_ref()
                .is_none_or(|revision| revision.0.is_empty() || revision.0.len() > 65_536)
        {
            return Err(invariant_failed());
        }
        let existing: Option<Option<Vec<u8>>> = connection
            .query_row(
                "SELECT source_revision FROM message_document_refs
             WHERE message_id=?1 AND document_id=?2 AND relation='context'",
                params![anchor.to_storage(), reference.document_id.to_storage()],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage_failed)?;
        if let Some(revision) = existing {
            if revision.is_none_or(|bytes| bytes.is_empty() || bytes.len() > 65_536) {
                return Err(invariant_failed());
            }
            retained.insert((anchor, reference.document_id));
        }
    }
    Ok(retained)
}

pub(super) fn validate_answered_question(
    connection: &Connection,
    prior: &str,
    chat: ChatId,
    input: &str,
) -> Result<(), AiError> {
    use koru_ai_core::{AgentAnswerValue, AnswersRecord, QuestionsRecord};
    // The same finite card/byte budgets as terminal-question retention. SQL
    // removes ordinary question cards; the Rust decoder still proves each row.
    let mut statement = connection
        .prepare(
            "SELECT agent_input FROM tool_invocations WHERE attempt_id=?1 AND chat_id=?2
            AND bot_source_kind='questions' AND status='complete' AND approval='approved'
            AND agent_output=?3
            AND CASE WHEN json_valid(agent_input)
                THEN json_extract(agent_input,'$.koru_questions.next_message')=1 ELSE 0 END
            ORDER BY id LIMIT 65",
        )
        .map_err(storage_failed)?;
    let rows = statement
        .query_map(params![prior, chat.to_storage(), input], |r| {
            r.get::<_, String>(0)
        })
        .map_err(storage_failed)?;
    let mut candidates = Vec::new();
    let mut bytes = 0usize;
    for row in rows {
        let encoded = row.map_err(storage_failed)?;
        bytes = bytes
            .checked_add(encoded.len())
            .ok_or_else(invariant_failed)?;
        if candidates.len() == 64 || bytes > 1024 * 1024 {
            return Err(AiError::new(
                AiErrorClass::ContextTooLarge,
                "Too much retained question evidence.",
                false,
            ));
        }
        candidates.push(encoded);
    }
    let answers = AnswersRecord::decode(input).ok_or_else(invariant_failed)?;
    for encoded in candidates {
        let Some(questions) = QuestionsRecord::decode(&encoded) else {
            continue;
        };
        if !questions.next_message || questions.questions.len() != answers.answers.len() {
            continue;
        }
        let mut seen = HashSet::new();
        let valid = answers.answers.iter().all(|answer| {
            seen.insert(answer.question_id.as_str())
                && questions
                    .questions
                    .iter()
                    .find(|question| question.id == answer.question_id)
                    .is_some_and(|question| {
                        question.secret == matches!(answer.value, AgentAnswerValue::Withheld)
                    })
        });
        if valid {
            return Ok(());
        }
    }
    Err(invariant_failed())
}
