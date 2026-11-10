// SPDX-License-Identifier: MPL-2.0

//! Shared mutations: the caller owns the transaction and publication boundary.
use super::*;

pub(super) fn begin_retry_on(
    transaction: &Connection,
    retry: &PendingRetry,
) -> Result<InferenceAttempt, AiError> {
    if retry.assistant_message.role != ChatRole::Assistant
        || retry.assistant_message.state != MessageState::Pending
        || retry.attempt.assistant_message_id != Some(retry.assistant_message.id)
        || retry.attempt.chat_id != Some(retry.assistant_message.chat_id)
    {
        return Err(invariant_failed());
    }
    if let Some(existing) = query_attempt(
        transaction,
        "operation_id",
        &retry.attempt.operation_id.to_string(),
    )? {
        return Ok(existing);
    }
    insert_message(transaction, &retry.assistant_message)?;
    insert_attempt(transaction, &retry.attempt)?;
    transaction
        .execute(
            "UPDATE chats SET updated_at_ms = max(updated_at_ms, ?2) WHERE id = ?1",
            params![
                retry.assistant_message.chat_id.to_storage(),
                retry.assistant_message.created_at.millis()
            ],
        )
        .map_err(storage_failed)?;
    Ok(retry.attempt.clone())
}

pub(super) fn begin_turn_on(
    transaction: &Connection,
    turn: &PendingTurn,
) -> Result<InferenceAttempt, AiError> {
    validate_turn(turn)?;
    if let Some(existing) = query_attempt(
        &transaction,
        "operation_id",
        &turn.attempt.operation_id.to_string(),
    )? {
        return Ok(existing);
    }

    insert_message(&transaction, &turn.user_message)?;
    insert_message(&transaction, &turn.assistant_message)?;
    insert_attempt(&transaction, &turn.attempt)?;
    attachments::bind_attachments(
        &transaction,
        turn.user_message.chat_id,
        turn.user_message.id,
        &turn.attachment_ids,
        turn.user_message.created_at,
    )?;
    transaction
        .execute(
            "UPDATE chats
                    SET updated_at_ms = max(updated_at_ms, ?2)
                  WHERE id = ?1",
            params![
                turn.user_message.chat_id.to_storage(),
                turn.assistant_message.created_at.millis()
            ],
        )
        .map_err(storage_failed)?;
    Ok(turn.attempt.clone())
}

pub(super) fn apply_event_on(
    transaction: &Connection,
    event: &AiEvent,
    finalized_blocks: &[MessageBlock],
) -> Result<InferenceAttempt, AiError> {
    if !matches!(event, AiEvent::Completed { .. }) && !finalized_blocks.is_empty() {
        return Err(invariant_failed());
    }
    let attempt_id = event.attempt_id();
    let current =
        query_attempt(&transaction, "id", &attempt_id.to_storage())?.ok_or_else(storage_missing)?;
    if current.status.is_terminal() {
        return Ok(current);
    }
    let assistant = current.assistant_message_id.map(MessageId::to_storage);

    match event {
        AiEvent::Started {
            provider_response_id,
            request_payload_bytes,
            resolved_model,
            effective_reasoning,
            at,
            ..
        } => {
            transaction
                .execute(
                    "UPDATE inference_attempts
                            SET status = 'streaming', provider_started_at_ms =
                                    coalesce(provider_started_at_ms, ?2),
                                provider_response_id = ?3, resolved_model = ?4,
                                effective_reasoning = ?5,
                                request_payload_bytes = coalesce(?6, request_payload_bytes)
                          WHERE id = ?1",
                    params![
                        attempt_id.to_storage(),
                        at.millis(),
                        provider_response_id,
                        resolved_model,
                        effective_reasoning.map(ReasoningChoice::as_token),
                        optional_u64(*request_payload_bytes)?,
                    ],
                )
                .map_err(storage_failed)?;
            update_message_resolved_model(&transaction, assistant.as_deref(), resolved_model)?;
        }
        AiEvent::TextDelta { text, at, .. } => {
            transaction
                .execute(
                    "UPDATE inference_attempts
                            SET status = 'streaming',
                                provider_started_at_ms = coalesce(provider_started_at_ms, ?2),
                                first_token_at_ms = coalesce(first_token_at_ms, ?2)
                          WHERE id = ?1",
                    params![attempt_id.to_storage(), at.millis()],
                )
                .map_err(storage_failed)?;
            if let Some(message) = assistant.as_deref() {
                transaction
                    .execute(
                        "UPDATE messages SET markdown = markdown || ?2
                              WHERE id = ?1 AND state = 'pending'",
                        params![message, text],
                    )
                    .map_err(storage_failed)?;
            }
        }
        AiEvent::ReasoningDelta { at, .. } => {
            transaction
                .execute(
                    "UPDATE inference_attempts
                            SET status = 'streaming',
                                provider_started_at_ms = coalesce(provider_started_at_ms, ?2)
                          WHERE id = ?1",
                    params![attempt_id.to_storage(), at.millis()],
                )
                .map_err(storage_failed)?;
        }
        AiEvent::Usage { usage, .. } => {
            transaction
                .execute(
                    "UPDATE inference_attempts
                            SET prompt_tokens = ?2, cached_prompt_tokens = ?3,
                                completion_tokens = ?4, reasoning_tokens = ?5,
                                total_tokens = ?6, usage_source = ?7
                          WHERE id = ?1",
                    params![
                        attempt_id.to_storage(),
                        optional_u64(usage.prompt_tokens)?,
                        optional_u64(usage.cached_prompt_tokens)?,
                        optional_u64(usage.completion_tokens)?,
                        optional_u64(usage.reasoning_tokens)?,
                        optional_u64(usage.total_tokens)?,
                        usage_source_token(usage.source),
                    ],
                )
                .map_err(storage_failed)?;
        }
        AiEvent::ContextUsage { used, size, .. } => {
            transaction
                .execute(
                    "UPDATE inference_attempts
                            SET context_used_tokens = ?2, context_limit_tokens = ?3
                          WHERE id = ?1",
                    params![
                        attempt_id.to_storage(),
                        required_u64(*used)?,
                        required_u64(*size)?,
                    ],
                )
                .map_err(storage_failed)?;
        }
        AiEvent::ToolCalls { at, .. } => {
            // Tool-call details become their own sanitized durable records
            // in the workspace-tool migration. Until then this event keeps
            // the parent attempt active without writing arguments or
            // provider reasoning into ai.db.
            transaction
                .execute(
                    "UPDATE inference_attempts
                            SET status = 'streaming',
                                provider_started_at_ms = coalesce(provider_started_at_ms, ?2)
                          WHERE id = ?1",
                    params![attempt_id.to_storage(), at.millis()],
                )
                .map_err(storage_failed)?;
        }
        AiEvent::ToolActivity { .. }
        | AiEvent::AgentActivity { .. }
        | AiEvent::AgentNotice { .. }
        | AiEvent::Notice { .. } => {}
        AiEvent::WebSearch { phase, at, .. } => {
            transaction
                .execute(
                    "UPDATE inference_attempts
                            SET status = 'streaming',
                                provider_started_at_ms = coalesce(provider_started_at_ms, ?2),
                                web_search_calls = web_search_calls + ?3
                          WHERE id = ?1",
                    params![
                        attempt_id.to_storage(),
                        at.millis(),
                        i64::from(matches!(phase, WebSearchPhase::Completed)),
                    ],
                )
                .map_err(storage_failed)?;
        }
        AiEvent::Citation { source, .. } => {
            if let Some(message) = assistant.as_deref() {
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO message_citations (message_id, ordinal, url, title)
                             SELECT ?1, COALESCE(MAX(ordinal) + 1, 0), ?2, ?3
                               FROM message_citations WHERE message_id = ?1",
                        params![message, source.url, source.title],
                    )
                    .map_err(storage_failed)?;
            }
        }
        AiEvent::Completed {
            stop_reason, at, ..
        } => {
            transaction
                .execute(
                    "UPDATE inference_attempts
                            SET status = 'complete', completed_at_ms = ?2, stop_reason = ?3
                          WHERE id = ?1",
                    params![attempt_id.to_storage(), at.millis(), stop_reason],
                )
                .map_err(storage_failed)?;
            complete_message(
                &transaction,
                assistant.as_deref(),
                MessageState::Complete,
                None,
                *at,
            )?;
            let Some(assistant) = current.assistant_message_id else {
                if !finalized_blocks.is_empty() {
                    return Err(invariant_failed());
                }
                let updated = query_attempt(&transaction, "id", &attempt_id.to_storage())?
                    .ok_or_else(storage_missing)?;
                return Ok(updated);
            };
            for block in finalized_blocks {
                if block.message_id != assistant {
                    return Err(invariant_failed());
                }
                insert_block(&transaction, block)?;
            }
        }
        AiEvent::Failed { error, at, .. } => {
            transaction
                .execute(
                    "UPDATE inference_attempts
                            SET status = 'failed', completed_at_ms = ?2, error_class = ?3
                          WHERE id = ?1",
                    params![
                        attempt_id.to_storage(),
                        at.millis(),
                        error_class_token(error.class)
                    ],
                )
                .map_err(storage_failed)?;
            complete_message(
                &transaction,
                assistant.as_deref(),
                MessageState::Failed,
                Some(error.class),
                *at,
            )?;
        }
        AiEvent::Cancelled { at, .. } => {
            transaction
                .execute(
                    "UPDATE inference_attempts
                            SET status = 'cancelled', completed_at_ms = ?2,
                                error_class = 'cancelled'
                          WHERE id = ?1",
                    params![attempt_id.to_storage(), at.millis()],
                )
                .map_err(storage_failed)?;
            complete_message(
                &transaction,
                assistant.as_deref(),
                MessageState::Interrupted,
                Some(AiErrorClass::Cancelled),
                *at,
            )?;
        }
    }

    let updated =
        query_attempt(&transaction, "id", &attempt_id.to_storage())?.ok_or_else(storage_missing)?;
    Ok(updated)
}

pub(super) fn create_agent_notice_on(
    connection: &Connection,
    notice: &AgentNotice,
) -> Result<AgentNotice, AiError> {
    let attempt = query_attempt(connection, "id", &notice.attempt_id.to_storage())?
        .ok_or_else(invariant_failed)?;
    if attempt.chat_id != Some(notice.chat_id)
        || attempt.assistant_message_id != Some(notice.assistant_message_id)
    {
        return Err(invariant_failed());
    }
    // Where the reply stood when the notice came: text deltas are
    // written before the next event is applied, so the length is exact.
    connection
        .execute(
            "INSERT INTO agent_notices (
                    id, attempt_id, chat_id, text, text_offset, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4,
                           (SELECT length(CAST(markdown AS BLOB)) FROM messages WHERE id = ?5),
                           ?6)",
            params![
                notice.id.to_storage(),
                notice.attempt_id.to_storage(),
                notice.chat_id.to_storage(),
                notice.text,
                notice.assistant_message_id.to_storage(),
                notice.created_at.millis(),
            ],
        )
        .map_err(storage_failed)?;
    connection
        .query_row(
            &format!("SELECT {AGENT_NOTICE_COLUMNS} FROM agent_notices WHERE id = ?1"),
            [notice.id.to_storage()],
            agent_notice_from_row,
        )
        .map_err(storage_failed)
}

pub(super) fn create_tool_activity_on(
    connection: &Connection,
    activity: &ToolActivity,
) -> Result<(), AiError> {
    if activity.status != ToolActivityStatus::Proposed
        || activity.started_at.is_some()
        || activity.completed_at.is_some()
        || activity.result_bytes.is_some()
        || activity.error_code.is_some()
        || activity.error_message.is_some()
    {
        return Err(invariant_failed());
    }
    let attempt = query_attempt(connection, "id", &activity.attempt_id.to_storage())?
        .ok_or_else(invariant_failed)?;
    if attempt.chat_id != Some(activity.chat_id)
        || attempt.assistant_message_id != Some(activity.assistant_message_id)
    {
        return Err(invariant_failed());
    }
    connection
        .execute(
            "INSERT INTO tool_invocations (
                    id, attempt_id, chat_id, tool_name, read_only, status, approval,
                    proposed_at_ms, started_at_ms, completed_at_ms, result_bytes, error_code,
                    target_workspace_id, target_document_id, error_message,
                    agent_label, agent_summary, agent_input, agent_output, text_offset
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, NULL, NULL, ?9, ?10, NULL,
                           ?11, ?12, ?13, ?14,
                           CASE WHEN ?15 THEN (
                               SELECT length(CAST(markdown AS BLOB)) FROM messages
                                WHERE id = ?16
                           ) END)",
            params![
                activity.id.to_storage(),
                activity.attempt_id.to_storage(),
                activity.chat_id.to_storage(),
                activity.tool_name,
                activity.read_only,
                tool_activity_status_token(activity.status),
                tool_approval_token(activity.approval),
                activity.proposed_at.millis(),
                activity.target_workspace_id.map(|id| id.to_storage()),
                activity.target_document_id.map(|id| id.to_storage()),
                activity.agent_label,
                activity.agent_summary,
                activity.agent_input,
                activity.agent_output,
                // Where the reply stood when a local agent's call began
                // (schema 10). Text deltas are written before the next
                // event is applied, so the length is exact.
                AgentToolKind::is_agent_token(&activity.tool_name),
                activity.assistant_message_id.to_storage(),
            ],
        )
        .map_err(storage_failed)?;
    Ok(())
}

pub(super) fn advance_agent_activity_on(
    connection: &Connection,
    activity: &ToolActivity,
) -> Result<(), AiError> {
    if !AgentToolKind::is_agent_token(&activity.tool_name) {
        return Err(invariant_failed());
    }
    // The same timestamp invariants as a workspace tool's row: a running
    // or finished tool has started; a proposed, pending or denied one has
    // not; a terminal row has completed; prose sits only on a failure.
    let start_required = matches!(
        activity.status,
        ToolActivityStatus::Running | ToolActivityStatus::Complete | ToolActivityStatus::Failed
    );
    let start_forbidden = matches!(
        activity.status,
        ToolActivityStatus::Proposed
            | ToolActivityStatus::ApprovalRequired
            | ToolActivityStatus::Denied
    );
    if activity.completed_at.is_some() != activity.status.is_terminal()
        || (start_required && activity.started_at.is_none())
        || (start_forbidden && activity.started_at.is_some())
        || activity
            .completed_at
            .is_some_and(|completed| completed < activity.proposed_at)
        || (activity.error_message.is_some()
            && !matches!(
                activity.status,
                ToolActivityStatus::Denied
                    | ToolActivityStatus::Failed
                    | ToolActivityStatus::Cancelled
            ))
    {
        return Err(invariant_failed());
    }
    // Identity is the row, not its kind: an agent names a tool's kind
    // only when it completes, so `tool_name` is what this call may
    // change. A row that is already terminal stays terminal, so a stale
    // or duplicate event changes nothing — and that is an error here.
    let updated = connection
        .execute(
            "UPDATE tool_invocations
                    SET tool_name = ?4, status = ?5, approval = ?6, started_at_ms = ?7,
                        completed_at_ms = ?8, error_code = ?9, error_message = ?10,
                        agent_label = ?11, agent_summary = ?12, agent_input = ?13,
                        agent_output = ?14
                  WHERE id = ?1 AND attempt_id = ?2 AND chat_id = ?3
                    AND status NOT IN ('complete', 'denied', 'failed', 'cancelled')",
            params![
                activity.id.to_storage(),
                activity.attempt_id.to_storage(),
                activity.chat_id.to_storage(),
                activity.tool_name,
                tool_activity_status_token(activity.status),
                tool_approval_token(activity.approval),
                activity.started_at.map(Timestamp::millis),
                activity.completed_at.map(Timestamp::millis),
                activity.error_code.map(tool_error_code_token),
                activity.error_message,
                activity.agent_label,
                activity.agent_summary,
                activity.agent_input,
                activity.agent_output,
            ],
        )
        .map_err(storage_failed)?;
    if updated == 0 {
        return Err(invariant_failed());
    }
    Ok(())
}
