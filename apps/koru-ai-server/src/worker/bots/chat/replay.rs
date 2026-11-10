// SPDX-License-Identifier: MPL-2.0
//! Stage native mapping, commit effects/cursor together, then publish.
use super::*;
use koru_ai_core::{AgentActivityEvent as A, AgentActivityLive, AgentToolKind};
use koru_delegate_api::bots::BotObservation;
#[cfg(test)]
mod tests;

fn source(value: AgentSource) -> BotToolSource {
    let (kind, id) = match value {
        AgentSource::Tool(id) => (BotToolSourceKind::Tool, id.to_string()),
        AgentSource::Approval(id) => (BotToolSourceKind::Approval, id.to_string()),
        AgentSource::Questions(id) => (BotToolSourceKind::Questions, id.to_string()),
    };
    BotToolSource { kind, id }
}
/// Exhaustive classification. New native semantics require an explicit choice;
/// the shared ordinary mapper's tolerant wildcard is not an acknowledgment.
fn turn(event: &AgentEvent) -> Result<Option<koru_session_protocol::AgentTurnId>, AiError> {
    Ok(match event {
        AgentEvent::CommandsUpdated { .. }
        | AgentEvent::ContextUsage { .. }
        | AgentEvent::ProviderState { .. } => None,
        AgentEvent::TurnStarted { turn_id }
        | AgentEvent::MessageDelta { turn_id, .. }
        | AgentEvent::ToolStarted { turn_id, .. }
        | AgentEvent::ToolCompleted { turn_id, .. }
        | AgentEvent::ApprovalRequired { turn_id, .. }
        | AgentEvent::ApprovalResolved { turn_id, .. }
        | AgentEvent::ApprovalExpired { turn_id, .. }
        | AgentEvent::WaitingForInput { turn_id }
        | AgentEvent::Usage { turn_id, .. }
        | AgentEvent::TurnCompleted { turn_id }
        | AgentEvent::TurnCancelled { turn_id, .. }
        | AgentEvent::TurnFailed { turn_id, .. }
        | AgentEvent::ModeChanged { turn_id, .. }
        | AgentEvent::Notice { turn_id, .. }
        | AgentEvent::QuestionsRequired { turn_id, .. }
        | AgentEvent::QuestionsResolved { turn_id, .. }
        | AgentEvent::AttachmentAccepted { turn_id, .. } => Some(*turn_id),
        // A rejected attachment must never disappear behind a successful reply;
        // native child execution is disabled by the approved Bot launch policy.
        AgentEvent::AttachmentRejected { .. } | AgentEvent::ChildSessionCreated { .. } => {
            return Err(bot_route_error());
        }
    })
}
fn update_live(live: &mut Vec<AgentLiveAction>, action: AgentLiveAction) {
    match &action {
        AgentLiveAction::OfferApproval {approval,..}|AgentLiveAction::SettleApproval {approval}=>
            live.retain(|old|!matches!(old,AgentLiveAction::OfferApproval {approval:id,..} if id==approval)),
        AgentLiveAction::OfferQuestions {request,..}|AgentLiveAction::SettleQuestions {request}=>
            live.retain(|old|!matches!(old,AgentLiveAction::OfferQuestions {request:id,..} if id==request)),
        AgentLiveAction::ForgetNextMessage {invocation}=>
            live.retain(|old|!matches!(old,AgentLiveAction::OfferQuestions {invocation:id,..} if id==invocation)),
    }
    if matches!(
        action,
        AgentLiveAction::OfferApproval { .. } | AgentLiveAction::OfferQuestions { .. }
    ) {
        live.push(action);
    }
}

impl WorkerState {
    pub(super) fn commit_bot_observation(
        &mut self,
        id: InferenceAttemptId,
        chat: &mut LiveChat,
        observation: BotObservation,
    ) -> Result<bool, AiError> {
        let staged = if let Some(pending) = chat.pending.take() {
            Some(pending)
        } else {
            if self.repository.bot_delivery_receipt(id)?.is_some() {
                return Ok(true);
            }
            stage_observation(&self.repository, id, chat, observation)?
        };
        let Some(staged) = staged else {
            return Ok(false);
        };
        // Keep the exact candidate on uncertain acknowledgment/readback. Its
        // random source identities and live choices must not be remapped.
        match commit_staged(&mut self.repository, &staged.batch) {
            Ok(true) => {}
            Ok(false) => return Ok(false),
            Err(error) => {
                chat.pending = Some(staged);
                return Err(error);
            }
        }
        let Staged {
            batch,
            mapper,
            live,
            transient,
            published,
            terminal,
            completed,
        } = staged;
        let _ = batch;
        chat.mapper = Some(mapper);
        chat.live = live;
        for (id, details) in transient {
            self.agent_live.insert(id, details);
        }
        for mut event in published {
            if let AiEvent::ToolActivity { activity, .. } = &mut event {
                if activity.status.is_terminal() {
                    self.agent_live.remove(&activity.id);
                }
                activity.live = self.agent_live.get(&activity.id).cloned();
            }
            self.publish(event);
        }
        if terminal {
            self.streams.remove(&id);
            self.cancellations.remove(&id);
            self.work_terminal(id);
        }
        if terminal && !completed {
            return Err(bot_route_error());
        }
        Ok(completed)
    }
}

fn commit_staged(repository: &mut Repository, batch: &BotReplayBatch) -> Result<bool, AiError> {
    let result = repository.apply_bot_replay(batch, Timestamp::now());
    if matches!(result, Ok(BotReplayDisposition::Applied)) {
        return Ok(true);
    }
    // A commit can be durable despite a lost/error acknowledgment. Only the
    // exact candidate fence and terminal cursor prove that its live state may
    // now be installed. Otherwise keep the original mapping for a retry.
    let durable = repository.bot_replay_state(batch.ai_attempt)?;
    if durable.fence.as_ref() == Some(&batch.fence)
        && durable.last_sequence == batch.events.last().map(|event| event.sequence)
    {
        return Ok(true);
    }
    result.map(|_| false)
}

pub(super) struct Staged {
    batch: BotReplayBatch,
    mapper: AgentEventMapper,
    live: Vec<AgentLiveAction>,
    transient: HashMap<ToolInvocationId, AgentActivityLive>,
    published: Vec<AiEvent>,
    terminal: bool,
    completed: bool,
}
fn stage_observation(
    repository: &Repository,
    id: InferenceAttemptId,
    chat: &LiveChat,
    observation: BotObservation,
) -> Result<Option<Staged>, AiError> {
    let fence = chat.fence.clone().ok_or_else(bot_route_error)?;
    if observation.fence != fence {
        return Err(bot_route_error());
    }
    let state = repository.bot_replay_state(id)?;
    if state.fence.as_ref() != Some(&fence) || state.stop_reason.is_some() {
        return Err(bot_route_error());
    }
    let mut raw = observation.observation.events;
    raw.retain(|frame| {
        state
            .last_sequence
            .is_none_or(|cursor| frame.sequence.get() > cursor)
    });
    if raw.is_empty() {
        if observation.observation.cursor.get() > state.last_sequence.unwrap_or(0)
            && observation.observation.earlier_truncated
        {
            return Err(bot_route_error());
        }
        return Ok(None);
    }
    if observation.observation.earlier_truncated
        && raw[0].sequence.get() != state.last_sequence.unwrap_or(0).saturating_add(1)
    {
        return Err(bot_route_error());
    }
    // The owner snapshot has bounded retention, but the AI frame budget is
    // stricter. Process a prefix and ask again after its committed cursor.
    raw.truncate(MAX_REPLAY_EVENTS);
    let mut bytes = 0usize;
    for frame in &raw {
        bytes = bytes
            .checked_add(super::preparation::encoded_size(frame)?)
            .ok_or_else(context_too_large)?;
        if bytes > MAX_REPLAY_BYTES {
            return Err(context_too_large());
        }
    }
    let attempt = repository.attempt(id)?.ok_or_else(bot_route_error)?;
    let assistant = attempt.assistant_message_id.ok_or_else(bot_route_error)?;
    let chat_id = attempt.chat_id.ok_or_else(bot_route_error)?;
    let mut text_offset = repository.bot_reply_bytes(id)?;
    let mut batch_text = String::new();
    let mut mapper = chat.mapper.clone().ok_or_else(bot_route_error)?;
    let mut live = chat.live.clone();
    let mut rows = HashMap::<ToolInvocationId, ToolActivity>::new();
    let mut bindings = Vec::<(AgentSource, ToolInvocationId)>::new();
    let mut transient = HashMap::<ToolInvocationId, AgentActivityLive>::new();
    let mut events = Vec::new();
    let mut published = Vec::new();
    let mut completed = false;
    let mut terminal = false;
    let mut cursor = state.last_sequence;
    for frame in raw {
        if terminal {
            break;
        }
        if frame.session_id != fence.session
            || frame.sequence.get() != cursor.unwrap_or(0).saturating_add(1)
        {
            return Err(bot_route_error());
        }
        let frame_turn = turn(&frame.event)?;
        if frame_turn.is_some() && frame_turn != fence.turn {
            return Err(bot_route_error());
        }
        let native_source = AgentSource::of(&frame.event);
        if let Some(s) = native_source {
            let known = bindings
                .iter()
                .find(|(old, _)| *old == s)
                .map(|(_, id)| *id)
                .or(repository.bot_tool_source(id, &source(s))?);
            if let Some(known) = known {
                mapper.seed(s, known);
            }
        }
        let at = Timestamp::now();
        let started = matches!(frame.event, AgentEvent::TurnStarted { .. });
        let mut mapped = mapper.apply(frame.event, at, false);
        if started {
            mapped.events.insert(
                0,
                AiEvent::Started {
                    attempt_id: id,
                    provider_response_id: fence.turn.map(|t| t.to_string()),
                    request_payload_bytes: None,
                    resolved_model: None,
                    effective_reasoning: None,
                    at,
                },
            );
        }
        for (s, invocation) in mapped.sources {
            if !bindings.iter().any(|(old, _)| *old == s) {
                bindings.push((s, invocation));
            }
        }
        for action in mapped.live {
            update_live(&mut live, action);
        }
        if mapper.pending_sources() > MAX_REPLAY_EFFECTS || live.len() > MAX_REPLAY_EFFECTS {
            return Err(context_too_large());
        }
        let live_bytes = live.iter().try_fold(0usize, |bytes, action| {
            let size = match action {
                AgentLiveAction::OfferQuestions { questions, .. } => {
                    super::preparation::encoded_size(questions)?
                }
                _ => 256,
            };
            bytes
                .checked_add(size)
                .filter(|bytes| *bytes <= MAX_REPLAY_BYTES)
                .ok_or_else(context_too_large)
        })?;
        let _ = live_bytes;
        let mut effects = Vec::new();
        for event in mapped.events {
            match event {
                AiEvent::AgentActivity { activity, at, .. } => match activity {
                    A::ModeChanged { started_in, mode } => {
                        if repository
                            .chat_with_access(chat_id, &ChatReadAccess::authorized(vec![chat_id]))?
                            .is_some_and(|c| c.agent_mode == started_in)
                        {
                            effects.push(BotReplayEffect::Mode(Some(mode)));
                        }
                    }
                    A::Notice { text } => {
                        let notice = koru_ai_core::AgentNotice {
                            id: koru_ai_core::AgentNoticeId::new(),
                            attempt_id: id,
                            chat_id,
                            assistant_message_id: assistant,
                            text,
                            text_offset: Some(
                                text_offset.try_into().map_err(|_| context_too_large())?,
                            ),
                            created_at: at,
                        };
                        effects.push(BotReplayEffect::Notice(notice.clone()));
                        published.push(AiEvent::AgentNotice {
                            attempt_id: id,
                            notice,
                        });
                    }
                    activity => {
                        let s = native_source.ok_or_else(bot_route_error)?;
                        let invocation = bindings
                            .iter()
                            .find(|(old, _)| *old == s)
                            .map(|(_, id)| *id)
                            .ok_or_else(bot_route_error)?;
                        let existing = rows
                            .get(&invocation)
                            .cloned()
                            .or(repository.tool_activity(invocation)?);
                        let create = existing.is_none();
                        let mut row =
                            normalize_activity(&attempt, activity, existing, at, &mut transient)?;
                        if row.id != invocation {
                            return Err(bot_route_error());
                        }
                        if create {
                            row.text_offset =
                                Some(text_offset.try_into().map_err(|_| context_too_large())?);
                            let mut proposed = row.clone();
                            proposed.status = ToolActivityStatus::Proposed;
                            proposed.started_at = None;
                            proposed.completed_at = None;
                            proposed.error_code = None;
                            proposed.error_message = None;
                            effects.push(BotReplayEffect::CreateTool {
                                source: source(s),
                                activity: proposed,
                            });
                        }
                        effects.push(BotReplayEffect::AdvanceTool {
                            source: source(s),
                            activity: row.clone(),
                        });
                        rows.insert(invocation, row.clone());
                        published.push(AiEvent::ToolActivity {
                            attempt_id: id,
                            activity: row,
                        });
                    }
                },
                event => {
                    if let AiEvent::TextDelta { text, .. } = &event {
                        batch_text.push_str(text);
                        text_offset = text_offset
                            .checked_add(text.len() as u64)
                            .ok_or_else(context_too_large)?;
                    }
                    terminal = event.is_terminal();
                    completed = matches!(event, AiEvent::Completed { .. });
                    let cleanup = terminal.then(|| BotTerminalCleanup {
                        retained_question_ids: live
                            .iter()
                            .filter_map(|a| match a {
                                AgentLiveAction::OfferQuestions {
                                    invocation,
                                    next_message: true,
                                    ..
                                } => Some(*invocation),
                                _ => None,
                            })
                            .collect(),
                    });
                    let blocks = if completed {
                        let mut markdown = repository
                            .message(assistant)?
                            .ok_or_else(bot_route_error)?
                            .markdown;
                        markdown.push_str(&batch_text);
                        finalize_markdown(assistant, &markdown)
                    } else {
                        Vec::new()
                    };
                    effects.push(BotReplayEffect::Event {
                        event: event.clone(),
                        finalized_blocks: blocks,
                        terminal_cleanup: cleanup,
                    });
                    published.push(event);
                }
            }
        }
        cursor = Some(frame.sequence.get());
        events.push(BotReplayEvent {
            sequence: frame.sequence.get(),
            turn: frame_turn,
            effects,
        });
        if events
            .iter()
            .map(|event| event.effects.len())
            .sum::<usize>()
            > MAX_REPLAY_EFFECTS
        {
            return Err(context_too_large());
        }
    }
    let batch = BotReplayBatch {
        ai_attempt: id,
        fence,
        expected_after: state.last_sequence,
        events,
    };
    Ok(Some(Staged {
        batch,
        mapper,
        live,
        transient,
        published,
        terminal,
        completed,
    }))
}

fn normalize_activity(
    attempt: &InferenceAttempt,
    activity: A,
    existing: Option<ToolActivity>,
    at: Timestamp,
    live: &mut HashMap<ToolInvocationId, AgentActivityLive>,
) -> Result<ToolActivity, AiError> {
    let fresh = |id, kind, approval| {
        crate::worker::agents::agent_row(
            id,
            attempt.id,
            attempt.chat_id.expect("Chat attempt"),
            attempt.assistant_message_id.expect("Chat attempt"),
            kind,
            approval,
            at,
        )
    };
    let mut row = match &activity {
        A::ToolStarted { invocation, .. } => existing
            .unwrap_or_else(|| fresh(*invocation, AgentToolKind::Tool, ToolApproval::NotRequired)),
        A::ApprovalRequired { invocation, .. } | A::QuestionsAsked { invocation, .. } => existing
            .unwrap_or_else(|| fresh(*invocation, AgentToolKind::Approval, ToolApproval::Pending)),
        _ => existing.ok_or_else(bot_route_error)?,
    };
    if row.status.is_terminal() {
        return Err(bot_route_error());
    }
    match activity {
        A::ToolStarted { label, input, .. } => {
            row.agent_label = (!label.is_empty()).then_some(label);
            row.agent_input = input;
            row.status = ToolActivityStatus::Running;
            row.started_at = Some(at);
        }
        A::ToolCompleted {
            tool,
            succeeded,
            failure,
            detail,
            output,
            ..
        } => {
            row.tool_name = tool
                .refined(row.agent_label.as_deref())
                .as_token()
                .to_owned();
            row.agent_output = output;
            row.status = if succeeded {
                ToolActivityStatus::Complete
            } else {
                ToolActivityStatus::Failed
            };
            row.completed_at = Some(at);
            row.error_message = if succeeded { None } else { detail };
            row.error_code = (!succeeded)
                .then(|| failure.map_or(ToolErrorCode::ExecutionFailed, |f| f.error_code()));
        }
        A::ApprovalRequired {
            summary, choices, ..
        } => {
            row.agent_summary = (!summary.is_empty()).then_some(summary);
            row.status = ToolActivityStatus::ApprovalRequired;
            live.insert(
                row.id,
                AgentActivityLive {
                    choices,
                    questions: Vec::new(),
                },
            );
        }
        A::ApprovalResolved { approved, .. } => {
            row.status = if approved {
                ToolActivityStatus::Complete
            } else {
                ToolActivityStatus::Denied
            };
            row.approval = if approved {
                ToolApproval::Approved
            } else {
                ToolApproval::Denied
            };
            row.completed_at = Some(at);
            if approved {
                row.started_at = Some(at);
            } else {
                row.error_code = Some(ToolErrorCode::PermissionDenied);
            }
        }
        A::ApprovalExpired { .. } => {
            row.status = ToolActivityStatus::Cancelled;
            row.approval = ToolApproval::Denied;
            row.completed_at = Some(at);
            row.error_code = Some(ToolErrorCode::Cancelled);
        }
        A::QuestionsAsked {
            questions,
            blocking,
            next_message,
            ..
        } => {
            let record = koru_ai_core::QuestionsRecord::of(&questions)
                .answered_by_next_message(next_message);
            row.agent_summary = Some(koru_ai_core::bound_live_text(record.summary().trim()));
            row.agent_input = record.encode();
            row.status = if blocking {
                ToolActivityStatus::ApprovalRequired
            } else {
                ToolActivityStatus::Running
            };
            if !blocking {
                row.started_at = Some(at);
            }
            live.insert(
                row.id,
                AgentActivityLive {
                    choices: Vec::new(),
                    questions,
                },
            );
        }
        A::QuestionsAnswered { answers, .. } => {
            row.completed_at = Some(at);
            match answers {
                Some(answers) => {
                    row.status = ToolActivityStatus::Complete;
                    row.approval = ToolApproval::Approved;
                    row.started_at.get_or_insert(at);
                    row.agent_output = Some(
                        koru_ai_core::AnswersRecord { answers }
                            .encode_lossless()
                            .ok_or_else(context_too_large)?,
                    );
                }
                None => {
                    row.status = ToolActivityStatus::Denied;
                    row.approval = ToolApproval::Denied;
                    row.started_at = None;
                    row.error_code = Some(ToolErrorCode::PermissionDenied);
                }
            }
        }
        A::ModeChanged { .. } | A::Notice { .. } => return Err(bot_route_error()),
    }
    Ok(row)
}
