// SPDX-License-Identifier: MPL-2.0

//! Atomic Bot replay in the existing AI writer. No provider or delegate calls.
use super::*;
use koru_ai_core::bots::replay::*;
use std::io::Write;

impl Repository {
    /// Foundation seam: the ai16 intent must already be prepared. P1.3's typed
    /// transaction helper will compose intent/lock preparation with this turn.
    /// This method neither creates nor repairs an admission receipt.
    pub fn prepare_bot_turn(
        &mut self,
        turn: &PendingTurn,
        submission: &BotSubmission,
        now: Timestamp,
    ) -> Result<InferenceAttempt, AiError> {
        let tx = self.connection.transaction().map_err(storage_failed)?;
        validate_prepared_submission(&tx, turn, submission)?;
        let p = &submission.provenance;
        let existing: Option<String> = tx
            .query_row(
                "SELECT run_id FROM bot_chat_runs WHERE ai_attempt_id=?1",
                [turn.attempt.id.to_storage()],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage_failed)?;
        if let Some(run) = existing {
            if run != submission.request.run.to_storage() {
                return Err(invariant_failed());
            }
            let current = query_attempt(&tx, "id", &turn.attempt.id.to_storage())?
                .ok_or_else(invariant_failed)?;
            if current.operation_id != turn.attempt.operation_id
                || current.chat_id != turn.attempt.chat_id
                || current.assistant_message_id != turn.attempt.assistant_message_id
            {
                return Err(invariant_failed());
            }
            validate_link(&tx, turn.attempt.id, submission)?;
            return Ok(current);
        }
        // Existing associations are receipt replays, not fresh admissions.
        // Only a new turn must still match the current revision/pause fence.
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
        if !admissible {
            return Err(invariant_failed());
        }
        if query_attempt(&tx, "operation_id", &turn.attempt.operation_id.to_string())?.is_some() {
            return Err(invariant_failed());
        }
        let current = mutations::begin_turn_on(&tx, turn)?;
        tx.execute("INSERT INTO bot_chat_runs (ai_attempt_id,user_scope_id,owner_device_id,submission_id,
            bot_id,chat_id,assistant_message_id,run_id,record_workspace_id,continued_from_run_id,created_at_ms)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)", params![
            current.id.to_storage(), p.owner.user_scope.to_string(), p.owner.device.to_string(),
            p.submission.to_storage(), p.bot.to_storage(), p.chat_id.to_string(),
            turn.assistant_message.id.to_storage(), submission.request.run.to_storage(),
            submission.workspace.to_string(), p.continued_from.map(|id|id.to_storage()), now.millis(),
        ]).map_err(storage_failed)?;
        tx.execute(
            "INSERT INTO bot_run_replay(ai_attempt_id,updated_at_ms) VALUES (?1,?2)",
            params![current.id.to_storage(), now.millis()],
        )
        .map_err(storage_failed)?;
        tx.commit().map_err(storage_failed)?;
        Ok(current)
    }

    pub fn bind_bot_execution(
        &mut self,
        attempt: InferenceAttemptId,
        fence: &BotExecutionFence,
        now: Timestamp,
    ) -> Result<(), AiError> {
        let tx = self.connection.transaction().map_err(storage_failed)?;
        validate_fence_link(&tx, attempt, fence)?;
        let state = replay_on(&tx, attempt)?;
        if state.stop_reason.is_some() {
            return Err(invariant_failed());
        }
        if let Some(old) = state.fence {
            let mut offered = fence.clone();
            offered.turn = old.turn;
            if old != offered || (old.turn.is_some() && old.turn != fence.turn) {
                return Err(invariant_failed());
            }
            if old.turn == fence.turn {
                return Ok(());
            }
        }
        if query_attempt(&tx, "id", &attempt.to_storage())?
            .ok_or_else(invariant_failed)?
            .status
            .is_terminal()
        {
            return Err(invariant_failed());
        }
        tx.execute("UPDATE bot_run_replay SET run_attempt_id=?2,generation=?3,session_id=?4,turn_id=?5,updated_at_ms=?6 WHERE ai_attempt_id=?1",
            params![attempt.to_storage(),fence.attempt.to_storage(),fence.generation.get(),fence.session.to_string(),
                fence.turn.map(|id|id.to_string()),now.millis()]).map_err(storage_failed)?;
        tx.commit().map_err(storage_failed)
    }

    pub fn bot_replay_state(&self, attempt: InferenceAttemptId) -> Result<BotReplayState, AiError> {
        replay_on(&self.connection, attempt)
    }

    pub fn bot_tool_source(
        &self,
        attempt: InferenceAttemptId,
        source: &BotToolSource,
    ) -> Result<Option<ToolInvocationId>, AiError> {
        validate_source(source)?;
        tool_source_on(&self.connection, attempt, source)
    }

    pub fn apply_bot_replay(
        &mut self,
        batch: &BotReplayBatch,
        now: Timestamp,
    ) -> Result<BotReplayDisposition, AiError> {
        validate_batch(batch)?;
        let tx = self.connection.transaction().map_err(storage_failed)?;
        validate_fence_link(&tx, batch.ai_attempt, &batch.fence)?;
        let state = replay_on(&tx, batch.ai_attempt)?;
        if state.fence.as_ref() != Some(&batch.fence) {
            return Err(invariant_failed());
        }
        let last = batch.events.last().ok_or_else(invariant_failed)?.sequence;
        if state.last_sequence.is_some_and(|cursor| last <= cursor) {
            return Ok(BotReplayDisposition::Duplicate);
        }
        let current = query_attempt(&tx, "id", &batch.ai_attempt.to_storage())?
            .ok_or_else(invariant_failed)?;
        if current.status.is_terminal() {
            return Ok(BotReplayDisposition::Terminal);
        }
        if state.stop_reason.is_some() || state.last_sequence != batch.expected_after {
            return Err(invariant_failed());
        }
        let chat = current.chat_id.ok_or_else(invariant_failed)?;
        let assistant = current.assistant_message_id.ok_or_else(invariant_failed)?;
        let mut terminal = false;
        for source_event in &batch.events {
            for effect in &source_event.effects {
                if terminal {
                    return Err(invariant_failed());
                }
                match effect {
                    BotReplayEffect::Event {
                        event,
                        finalized_blocks,
                        terminal_cleanup,
                    } => {
                        if event.attempt_id() != batch.ai_attempt
                            || matches!(
                                event,
                                AiEvent::AgentActivity { .. }
                                    | AiEvent::ToolActivity { .. }
                                    | AiEvent::AgentNotice { .. }
                                    | AiEvent::ToolCalls { .. }
                            )
                        {
                            return Err(invariant_failed());
                        }
                        if event.is_terminal() != terminal_cleanup.is_some() {
                            return Err(invariant_failed());
                        }
                        if let Some(cleanup) = terminal_cleanup {
                            let at = match event {
                                AiEvent::Completed { at, .. }
                                | AiEvent::Failed { at, .. }
                                | AiEvent::Cancelled { at, .. } => *at,
                                _ => return Err(invariant_failed()),
                            };
                            finish_agent_rows_on(&tx, batch.ai_attempt, cleanup, at)?;
                        }
                        mutations::apply_event_on(&tx, event, finalized_blocks)?;
                        terminal = event.is_terminal();
                        if let AiEvent::Completed { at, .. } = event {
                            tx.execute("UPDATE bot_chat_runs SET delivery_state='pending',delivery_confirmed_at_ms=?2 WHERE ai_attempt_id=?1 AND delivery_state='none'",
                                params![batch.ai_attempt.to_storage(),at.millis()]).map_err(storage_failed)?;
                        }
                    }
                    BotReplayEffect::CreateTool { source, activity }
                    | BotReplayEffect::AdvanceTool { source, activity } => {
                        validate_source(source)?;
                        if activity.attempt_id != batch.ai_attempt
                            || activity.chat_id != chat
                            || activity.assistant_message_id != assistant
                            || activity.live.is_some()
                            || activity.display_target.is_some()
                            || !AgentToolKind::is_agent_token(&activity.tool_name)
                        {
                            return Err(invariant_failed());
                        }
                        let mapped = tool_source_on(&tx, batch.ai_attempt, source)?;
                        if matches!(effect, BotReplayEffect::CreateTool { .. }) {
                            if mapped.is_some() {
                                return Err(invariant_failed());
                            }
                            mutations::create_tool_activity_on(&tx, activity)?;
                            tx.execute("UPDATE tool_invocations SET bot_source_kind=?2,bot_source_id=?3 WHERE id=?1",
                                params![activity.id.to_storage(),source.kind.token(),source.id]).map_err(storage_failed)?;
                        } else {
                            if mapped != Some(activity.id) {
                                return Err(invariant_failed());
                            }
                            mutations::advance_agent_activity_on(&tx, activity)?;
                        }
                    }
                    BotReplayEffect::Notice(notice) => {
                        if notice.attempt_id != batch.ai_attempt
                            || notice.chat_id != chat
                            || notice.assistant_message_id != assistant
                        {
                            return Err(invariant_failed());
                        }
                        mutations::create_agent_notice_on(&tx, notice)?;
                    }
                    BotReplayEffect::Mode(mode) => {
                        if mode.as_ref().is_some_and(|v| v.len() > 256) {
                            return Err(invariant_failed());
                        }
                        tx.execute(
                            "UPDATE chats SET agent_mode=?2 WHERE id=?1",
                            params![chat.to_storage(), mode],
                        )
                        .map_err(storage_failed)?;
                    }
                }
            }
        }
        tx.execute(
            "UPDATE bot_run_replay SET last_sequence=?2,updated_at_ms=?3 WHERE ai_attempt_id=?1",
            params![batch.ai_attempt.to_storage(), sequence(last)?, now.millis()],
        )
        .map_err(storage_failed)?;
        tx.commit().map_err(storage_failed)?;
        Ok(BotReplayDisposition::Applied)
    }

    /// A gap is explicit durable interruption, never an invitation to relaunch.
    pub fn stop_bot_replay(
        &mut self,
        attempt: InferenceAttemptId,
        fence: &BotExecutionFence,
        reason: BotReplayStop,
        now: Timestamp,
    ) -> Result<(), AiError> {
        let tx = self.connection.transaction().map_err(storage_failed)?;
        validate_fence_link(&tx, attempt, fence)?;
        let state = replay_on(&tx, attempt)?;
        if state.fence.as_ref() != Some(fence) {
            return Err(invariant_failed());
        }
        let current =
            query_attempt(&tx, "id", &attempt.to_storage())?.ok_or_else(invariant_failed)?;
        if current.status.is_terminal() {
            return Ok(());
        }
        tx.execute("UPDATE bot_run_replay SET stop_reason=?2,updated_at_ms=?3 WHERE ai_attempt_id=?1 AND stop_reason IS NULL",
            params![attempt.to_storage(),reason.token(),now.millis()]).map_err(storage_failed)?;
        tx.execute("UPDATE inference_attempts SET status='interrupted',completed_at_ms=max(queued_at_ms,?2) WHERE id=?1",
            params![attempt.to_storage(),now.millis()]).map_err(storage_failed)?;
        complete_message(
            &tx,
            current
                .assistant_message_id
                .map(MessageId::to_storage)
                .as_deref(),
            MessageState::Interrupted,
            None,
            now,
        )?;
        tx.execute("UPDATE tool_invocations SET status='cancelled',approval=CASE WHEN approval='pending' THEN 'denied' ELSE approval END,
            completed_at_ms=max(proposed_at_ms,?2),error_code='cancelled' WHERE attempt_id=?1 AND status IN ('proposed','approval_required','running')",
            params![attempt.to_storage(),now.millis()]).map_err(storage_failed)?;
        tx.commit().map_err(storage_failed)
    }

    pub fn bot_delivery_receipt(
        &self,
        attempt: InferenceAttemptId,
    ) -> Result<Option<BotDeliveryReceipt>, AiError> {
        delivery_on(&self.connection, attempt)
    }

    /// Only a trusted owner's exact acknowledgment may move the durable outbox.
    pub fn acknowledge_bot_delivery(
        &mut self,
        attempt: InferenceAttemptId,
        receipt: &BotDeliveryReceipt,
        now: Timestamp,
    ) -> Result<(), AiError> {
        let tx = self.connection.transaction().map_err(storage_failed)?;
        if delivery_on(&tx, attempt)?.as_ref() != Some(receipt) {
            return Err(invariant_failed());
        }
        tx.execute(
            "UPDATE bot_chat_runs SET delivery_state='acknowledged',delivery_acknowledged_at_ms=?2
            WHERE ai_attempt_id=?1 AND delivery_state IN ('pending','unconfirmed')",
            params![attempt.to_storage(), now.millis()],
        )
        .map_err(storage_failed)?;
        tx.commit().map_err(storage_failed)
    }

    pub fn mark_bot_delivery_unconfirmed(
        &mut self,
        attempt: InferenceAttemptId,
        receipt: &BotDeliveryReceipt,
    ) -> Result<(), AiError> {
        let tx = self.connection.transaction().map_err(storage_failed)?;
        if delivery_on(&tx, attempt)?.as_ref() != Some(receipt) {
            return Err(invariant_failed());
        }
        tx.execute("UPDATE bot_chat_runs SET delivery_state='unconfirmed' WHERE ai_attempt_id=?1 AND delivery_state='pending'",
            [attempt.to_storage()]).map_err(storage_failed)?;
        tx.commit().map_err(storage_failed)
    }

    /// Bounded owner-scoped recovery inventory; it grants no delegate authority.
    pub fn pending_bot_deliveries(
        &self,
        owner: koru_ai_core::bots::BotOwner,
        after: Option<BotDeliveryCursor>,
        limit: u16,
    ) -> Result<BotDeliveryPage, AiError> {
        if limit == 0 || limit > 256 {
            return Err(invariant_failed());
        }
        // A real keyset predicate, not OFFSET or a nullable OR that scans every
        // previous page. The partial index contains only unresolved deliveries.
        let (time, id) = after
            .map(|c| (c.created_at_ms, c.ai_attempt.to_storage()))
            .unwrap_or((i64::MIN, String::new()));
        let mut statement = self.connection.prepare("SELECT ai_attempt_id,created_at_ms FROM bot_chat_runs
            WHERE user_scope_id=?1 AND owner_device_id=?2 AND delivery_state IN ('pending','unconfirmed')
            AND (created_at_ms,ai_attempt_id)>(?3,?4)
            ORDER BY created_at_ms,ai_attempt_id LIMIT ?5").map_err(storage_failed)?;
        let rows = statement
            .query_map(
                params![
                    owner.user_scope.to_string(),
                    owner.device.to_string(),
                    time,
                    id,
                    u32::from(limit) + 1
                ],
                |r| {
                    Ok(BotDeliveryCursor {
                        ai_attempt: required_id(
                            r,
                            0,
                            InferenceAttemptId::parse,
                            "InferenceAttemptId",
                        )?,
                        created_at_ms: r.get(1)?,
                    })
                },
            )
            .map_err(storage_failed)?;
        let mut positions = collect_rows(rows)?;
        let more = positions.len() > usize::from(limit);
        positions.truncate(usize::from(limit));
        let next_cursor = more.then(|| positions.last().copied()).flatten();
        let mut receipts = Vec::with_capacity(positions.len());
        for position in positions {
            receipts.push(
                delivery_on(&self.connection, position.ai_attempt)?.ok_or_else(invariant_failed)?,
            );
        }
        Ok(BotDeliveryPage {
            receipts,
            next_cursor,
        })
    }
}

fn finish_agent_rows_on(
    connection: &Connection,
    attempt: InferenceAttemptId,
    cleanup: &BotTerminalCleanup,
    at: Timestamp,
) -> Result<(), AiError> {
    if cleanup.retained_question_ids.len() > MAX_REPLAY_EFFECTS {
        return Err(invariant_failed());
    }
    let retained: std::collections::BTreeSet<_> =
        cleanup.retained_question_ids.iter().copied().collect();
    if retained.len() != cleanup.retained_question_ids.len() {
        return Err(invariant_failed());
    }
    let ids =
        serde_json::to_string(&cleanup.retained_question_ids).map_err(|_| invariant_failed())?;
    // Validate only the bounded explicit exception list by indexed identity.
    // Never materialize all activity rows or scan them once per retained ID.
    let invalid: bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM json_each(?2) retained
        LEFT JOIN tool_invocations t ON t.id=retained.value AND t.attempt_id=?1
          AND t.bot_source_kind='questions' AND t.status IN ('proposed','approval_required','running')
        WHERE t.id IS NULL)",params![attempt.to_storage(),ids],|r|r.get(0)).map_err(storage_failed)?;
    if invalid {
        return Err(invariant_failed());
    }
    // Same terminal row fields as the shared agent cleanup, applied as a set
    // inside the source transaction. Explicit next-message questions stay open.
    connection
        .execute(
            "UPDATE tool_invocations SET status='cancelled',
        approval=CASE WHEN approval='pending' THEN 'denied' ELSE approval END,
        completed_at_ms=max(proposed_at_ms,?2),error_code='cancelled'
        WHERE attempt_id=?1 AND status IN ('proposed','approval_required','running')
          AND id NOT IN (SELECT value FROM json_each(?3))",
            params![attempt.to_storage(), at.millis(), ids],
        )
        .map_err(storage_failed)?;
    Ok(())
}

fn validate_prepared_submission(
    connection: &Connection,
    turn: &PendingTurn,
    submission: &BotSubmission,
) -> Result<(), AiError> {
    let p = &submission.provenance;
    if !submission.valid()
        || p.ai_attempt_id != Some(turn.attempt.id.as_uuid())
        || p.assistant_message_id != Some(turn.assistant_message.id.as_uuid())
        || p.chat_id != turn.user_message.chat_id.as_uuid()
        || turn.user_message.markdown != submission.input.text
    {
        return Err(invariant_failed());
    }
    // Read-side only. Replace this strict wrapper decode with P1.3's committed
    // read_bot_submission_on helper when the admission slice is integrated.
    let raw: String=connection.query_row("SELECT r.submission_json FROM bot_command_receipts r
        JOIN bots b ON b.bot_id=r.bot_id JOIN bot_chats c ON c.bot_id=b.bot_id
        WHERE r.user_scope_id=?1 AND r.request_id=?2 AND r.bot_id=?3 AND r.planned_run_id=?4
        AND r.command_kind IN ('start','continue') AND r.stage IN ('prepared','accepted','completed')
        AND r.submission_version=1 AND r.captured_config_revision=?5 AND r.captured_policy_revision=?6
        AND b.user_scope_id=?1 AND b.owner_device_id=?7 AND b.lifecycle='active'
        AND b.home_workspace_id=?8 AND c.chat_id=?9",
        params![p.owner.user_scope.to_string(),p.submission.to_storage(),p.bot.to_storage(),submission.request.run.to_storage(),
            required_u64(p.config_revision)?,required_u64(p.policy_revision)?,p.owner.device.to_string(),submission.workspace.to_string(),p.chat_id.to_string()],
        |r|r.get(0)).map_err(storage_failed)?;
    if raw.len() > 4 * 1024 * 1024 {
        return Err(invariant_failed());
    }
    let wrapper: serde_json::Value = serde_json::from_str(&raw).map_err(|_| invariant_failed())?;
    let object = wrapper.as_object().ok_or_else(invariant_failed)?;
    if object.len() != 2 || object.get("version").and_then(|v| v.as_u64()) != Some(1) {
        return Err(invariant_failed());
    }
    let stored: BotSubmission = serde_json::from_value(
        object
            .get("submission")
            .ok_or_else(invariant_failed)?
            .clone(),
    )
    .map_err(|_| invariant_failed())?;
    if stored != *submission {
        return Err(invariant_failed());
    }
    Ok(())
}

fn validate_link(
    connection: &Connection,
    attempt: InferenceAttemptId,
    submission: &BotSubmission,
) -> Result<(), AiError> {
    let p = &submission.provenance;
    let matches:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM bot_chat_runs WHERE ai_attempt_id=?1 AND user_scope_id=?2
        AND owner_device_id=?3 AND submission_id=?4 AND bot_id=?5 AND chat_id=?6 AND assistant_message_id=?7
        AND run_id=?8 AND record_workspace_id=?9 AND continued_from_run_id IS ?10)",params![attempt.to_storage(),p.owner.user_scope.to_string(),p.owner.device.to_string(),
        p.submission.to_storage(),p.bot.to_storage(),p.chat_id.to_string(),p.assistant_message_id.map(|id|id.to_string()),submission.request.run.to_storage(),
        submission.workspace.to_string(),p.continued_from.map(|id|id.to_storage())],|r|r.get(0)).map_err(storage_failed)?;
    if !matches {
        return Err(invariant_failed());
    }
    Ok(())
}

fn validate_fence_link(
    connection: &Connection,
    attempt: InferenceAttemptId,
    fence: &BotExecutionFence,
) -> Result<(), AiError> {
    if fence.generation.get() == 0 {
        return Err(invariant_failed());
    }
    let matches:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM bot_chat_runs l
        JOIN bot_chats c ON c.chat_id=l.chat_id AND c.bot_id=l.bot_id
        JOIN inference_attempts a ON a.id=l.ai_attempt_id AND a.chat_id=l.chat_id AND a.assistant_message_id=l.assistant_message_id
        WHERE l.ai_attempt_id=?1 AND l.user_scope_id=?2 AND l.owner_device_id=?3 AND l.bot_id=?4 AND l.run_id=?5)",
        params![attempt.to_storage(),fence.owner.user_scope.to_string(),fence.owner.device.to_string(),fence.bot.to_storage(),fence.run.to_storage()],|r|r.get(0)).map_err(storage_failed)?;
    if !matches {
        return Err(invariant_failed());
    }
    Ok(())
}

fn replay_on(
    connection: &Connection,
    attempt: InferenceAttemptId,
) -> Result<BotReplayState, AiError> {
    connection.query_row("SELECT l.user_scope_id,l.owner_device_id,l.bot_id,l.run_id,r.run_attempt_id,r.generation,r.session_id,r.turn_id,r.last_sequence,r.stop_reason
        FROM bot_chat_runs l JOIN bot_run_replay r USING(ai_attempt_id) WHERE l.ai_attempt_id=?1",[attempt.to_storage()],|r| {
        let run_attempt:Option<String>=r.get(4)?;
        let fence=if let Some(run_attempt)=run_attempt {
            Some(serde_json::from_value::<BotExecutionFence>(serde_json::json!({
                "owner":{"user_scope":r.get::<_,String>(0)?,"device":r.get::<_,String>(1)?},
                "bot":r.get::<_,String>(2)?,"run":r.get::<_,String>(3)?,"attempt":run_attempt,
                "generation":r.get::<_,u32>(5)?,"session":r.get::<_,String>(6)?,"turn":r.get::<_,Option<String>>(7)?
            })).map_err(|_|decode_failed(4,"Bot execution fence"))?)
        } else {None};
        let stop_reason=match r.get::<_,Option<String>>(9)?.as_deref() {
            None=>None,Some("replay_gap")=>Some(BotReplayStop::ReplayGap),Some("execution_changed")=>Some(BotReplayStop::ExecutionChanged),
            Some("ai_restart")=>Some(BotReplayStop::AiRestart),Some("owner_unavailable")=>Some(BotReplayStop::OwnerUnavailable),
            _=>return Err(decode_failed(9,"Bot replay stop")),
        };
        Ok(BotReplayState{fence,last_sequence:r.get(8)?,stop_reason})
    }).map_err(storage_failed)
}

fn delivery_on(
    connection: &Connection,
    attempt: InferenceAttemptId,
) -> Result<Option<BotDeliveryReceipt>, AiError> {
    let row: Option<(String, String, i64)> = connection
        .query_row(
            "SELECT chat_id,assistant_message_id,delivery_confirmed_at_ms FROM bot_chat_runs
        WHERE ai_attempt_id=?1 AND delivery_state<>'none'",
            [attempt.to_storage()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(storage_failed)?;
    let Some((chat, message, at)) = row else {
        return Ok(None);
    };
    let fence = replay_on(connection, attempt)?
        .fence
        .ok_or_else(invariant_failed)?;
    Ok(Some(BotDeliveryReceipt {
        run: fence.run,
        attempt: fence.attempt,
        generation: fence.generation,
        chat_id: Uuid::parse_str(&chat).map_err(|_| invariant_failed())?,
        ai_attempt_id: attempt.as_uuid(),
        assistant_message_id: Uuid::parse_str(&message).map_err(|_| invariant_failed())?,
        confirmed_at_ms: at,
    }))
}

fn tool_source_on(
    connection: &Connection,
    attempt: InferenceAttemptId,
    source: &BotToolSource,
) -> Result<Option<ToolInvocationId>, AiError> {
    connection.query_row("SELECT id FROM tool_invocations WHERE attempt_id=?1 AND bot_source_kind=?2 AND bot_source_id=?3",
        params![attempt.to_storage(),source.kind.token(),source.id],|r|required_id(r,0,ToolInvocationId::parse,"ToolInvocationId")).optional().map_err(storage_failed)
}
fn validate_source(source: &BotToolSource) -> Result<(), AiError> {
    if source.id.is_empty() || source.id.len() > 4096 {
        return Err(invariant_failed());
    }
    Ok(())
}
fn sequence(value: u64) -> Result<i64, AiError> {
    i64::try_from(value).map_err(|_| invariant_failed())
}
fn validate_batch(batch: &BotReplayBatch) -> Result<(), AiError> {
    if batch.events.is_empty() || batch.events.len() > MAX_REPLAY_EVENTS {
        return Err(invariant_failed());
    }
    let mut next = batch
        .expected_after
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(invariant_failed)?;
    let mut effects = 0usize;
    for event in &batch.events {
        sequence(event.sequence)?;
        if event.sequence != next || (event.turn.is_some() && event.turn != batch.fence.turn) {
            return Err(invariant_failed());
        }
        next = next.checked_add(1).ok_or_else(invariant_failed)?;
        effects = effects
            .checked_add(event.effects.len())
            .ok_or_else(invariant_failed)?;
        if effects > MAX_REPLAY_EFFECTS {
            return Err(invariant_failed());
        }
    }
    struct Bound(usize);
    impl Write for Bound {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|v| *v <= MAX_REPLAY_BYTES)
                .ok_or_else(|| std::io::Error::other("Bot replay batch exceeds bound"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Bound(0), batch).map_err(|_| invariant_failed())
}

#[cfg(test)]
mod tests;
