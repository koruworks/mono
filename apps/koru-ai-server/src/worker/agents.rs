// SPDX-License-Identifier: MPL-2.0

//! The worker's side of a local agent chat: what a provider cannot own.
//!
//! - **the work registry** — an attempt lives here from its creation
//!   transaction until its terminal is recorded *and* every job the
//!   dispatcher handed out for it has returned; deletion and clearing read
//!   this, never the UI delivery table;
//! - **the activity write path** — an agent's tool and permission facts
//!   become rows in `tool_invocations` under closed `agent.*` kinds, with
//!   the agent's own wording kept **live only** (AGENTUX D5-1);
//! - **approvals** — an answer is dropped into the turn's channel and the
//!   turn's own thread sends it; the dispatcher never waits on the wire;
//! - **deletion and clearing** — a chat's row outlives its teardown: the
//!   session-control thread drains the chat's sessions, reports back as an
//!   envelope, and only then is the row deleted and the held reply
//!   completed. Clearing holds a worker-wide gate meanwhile so nothing new
//!   can slip into its snapshot.

use std::collections::HashSet;

use koru_ai_core::{
    AgentActivityEvent, AgentActivityLive, AgentToolKind, AiProviderId, ChatId, InferenceAttempt,
    InferenceAttemptId, MessageId, ToolActivity, ToolActivityStatus, ToolApproval, ToolErrorCode,
    ToolInvocationId, bound_agent_content,
};
use koru_ai_provider_agent::{
    AgentSessions, AnswerError, EndOutcome, SteerError, agent_kind_of, is_agent_provider,
};
use koru_ai_wire::{
    AgentModelView, AgentModelsResponse, ClearHistoryResponse, DeleteChatResponse,
    ProviderValidationResponse, ProviderValidationStatus, SteerAttemptRequest,
    SteerAttemptResponse,
};
use koru_session_protocol::ExecutableAvailability;

use super::*;

/// One attempt's outstanding work: the jobs handed out and not yet back,
/// and whether its terminal has been recorded. It retires on one condition,
/// `terminal && admitted == 0`, checked whenever either changes.
#[derive(Default)]
pub(super) struct AttemptWork {
    admitted: u32,
    terminal: bool,
}

/// A chat whose deletion is waiting on the host: the replies to complete
/// when the drain reports, and whether a clear owns it.
pub(super) struct ClosingChat {
    replies: Vec<SyncSender<ApiResult<DeleteChatResponse>>>,
}

/// A clear in progress: the chats admitted into it, the ones whose drain
/// has not reported yet, how many could not be stopped, and the reply.
pub(super) struct Clearing {
    snapshot: Vec<ChatId>,
    pending: HashSet<ChatId>,
    failed: u32,
    reply: SyncSender<ApiResult<ClearHistoryResponse>>,
}

impl WorkerState {
    // ----- the work registry -------------------------------------------

    pub(super) fn work_started(&mut self, attempt: InferenceAttemptId) {
        self.active_work.insert(attempt, AttemptWork::default());
    }

    /// A job was handed out for the attempt.
    pub(super) fn work_admitted(&mut self, attempt: InferenceAttemptId) {
        if let Some(work) = self.active_work.get_mut(&attempt) {
            work.admitted = work.admitted.saturating_add(1);
        }
    }

    /// A job came back, whatever it brought.
    pub(super) fn work_returned(&mut self, attempt: InferenceAttemptId) {
        if let Some(work) = self.active_work.get_mut(&attempt) {
            work.admitted = work.admitted.saturating_sub(1);
        }
        self.retire_if_done(attempt);
    }

    /// The attempt's terminal was recorded, by whichever path did it.
    pub(super) fn work_terminal(&mut self, attempt: InferenceAttemptId) {
        if let Some(work) = self.active_work.get_mut(&attempt) {
            work.terminal = true;
        }
        self.retire_if_done(attempt);
    }

    fn retire_if_done(&mut self, attempt: InferenceAttemptId) {
        if self
            .active_work
            .get(&attempt)
            .is_some_and(|work| work.terminal && work.admitted == 0)
        {
            self.active_work.remove(&attempt);
        }
    }

    // ----- admission --------------------------------------------------

    /// Refuse work a delete or a clear has closed the door on. `chat` is the
    /// chat the work touches, or none for a creation.
    pub(super) fn admit_chat_work(&self, chat: Option<ChatId>) -> ApiResult<()> {
        if self.clearing.is_some() {
            return Err(history_clearing());
        }
        if chat.is_some_and(|chat| self.closing.contains_key(&chat)) {
            return Err(failure("chat_closing", "This chat is being deleted.", true));
        }
        Ok(())
    }

    fn chat_uses_agent(&self, chat: ChatId) -> ApiResult<bool> {
        Ok(self
            .repository
            .chat(chat)
            .map_err(ai_failure)?
            .is_some_and(|chat| is_agent_provider(&chat.current_model.provider)))
    }

    // ----- deletion ---------------------------------------------------

    /// Delete a chat. A chat with an agent behind it keeps its row until the
    /// host has confirmed nothing of it runs; the reply is held meanwhile
    /// and the dispatcher carries on with everything else.
    pub(super) fn begin_delete_chat(
        &mut self,
        context: RequestContext,
        chat: ChatId,
        reply: SyncSender<ApiResult<DeleteChatResponse>>,
    ) {
        if let Err(error) = authorize(&context, AiScope::Chat) {
            let _ = reply.send(Err(error));
            return;
        }
        if self.clearing.is_some() {
            let _ = reply.send(Err(history_clearing()));
            return;
        }
        if let Some(closing) = self.closing.get_mut(&chat) {
            closing.replies.push(reply);
            return;
        }
        match self.chat_has_active_attempt(chat) {
            Ok(true) => {
                let _ = reply.send(Err(active_history_conflict()));
                return;
            }
            Ok(false) => {}
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        }
        let uses_agent = match self.chat_uses_agent(chat) {
            Ok(uses_agent) => uses_agent,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let Some(agents) = self.agents.clone().filter(|_| uses_agent) else {
            let _ = reply.send(self.delete_chat_now(chat));
            return;
        };
        let output = self.output.clone();
        let started = agents.end_chat_sessions(
            chat,
            Box::new(move |outcome| {
                let _ = output.send(Envelope::ChatSessionsEnded { chat, outcome });
            }),
        );
        match started {
            Ok(()) => {
                self.closing.insert(
                    chat,
                    ClosingChat {
                        replies: vec![reply],
                    },
                );
            }
            Err(error) => {
                let _ = reply.send(Err(ai_failure(error)));
            }
        }
    }

    pub(super) fn delete_chat_now(&mut self, chat: ChatId) -> ApiResult<DeleteChatResponse> {
        let deleted = self.repository.delete_chat(chat).map_err(ai_failure)?;
        // Rows first, folders after the commit; a crash in between leaves
        // an orphan the next start's sweep removes.
        self.forget_chat_folders(&[chat]);
        Ok(DeleteChatResponse { deleted })
    }

    fn delete_snapshot(&mut self, snapshot: &[ChatId]) -> ApiResult<ClearHistoryResponse> {
        let chats_deleted = self.repository.delete_chats(snapshot).map_err(ai_failure)?;
        self.forget_chat_folders(snapshot);
        Ok(ClearHistoryResponse { chats_deleted })
    }

    /// The host reported on one chat's sessions: finish its delete, or count
    /// it for the clear that owns it.
    pub(super) fn on_chat_sessions_ended(&mut self, chat: ChatId, outcome: EndOutcome) {
        // Its agent is gone: a set it asked to be answered by the next
        // message has nobody to take the answers (D108). The rows go with
        // the chat; only the live state is left to forget.
        if matches!(outcome, EndOutcome::Ended) {
            self.forget_next_message_sets(chat);
        }
        if let Some(closing) = self.closing.remove(&chat) {
            let answer = match outcome {
                EndOutcome::Ended => self.delete_chat_now(chat),
                EndOutcome::Failed { code } => {
                    tracing::warn!(
                        target: "koru_ai::agent",
                        chat = %chat,
                        code,
                        "a chat's agent could not be stopped; the chat is kept"
                    );
                    Err(failure(
                        "agent_still_running",
                        "Couldn't stop the agent. Try deleting again.",
                        true,
                    ))
                }
            };
            for reply in closing.replies {
                let _ = reply.send(answer.clone());
            }
        }
        let Some(clearing) = &mut self.clearing else {
            return;
        };
        if !clearing.pending.remove(&chat) {
            return;
        }
        if matches!(outcome, EndOutcome::Failed { .. }) {
            clearing.failed = clearing.failed.saturating_add(1);
        }
        if clearing.pending.is_empty() {
            self.finish_clear_history();
        }
    }

    // ----- clearing ---------------------------------------------------

    /// Clear history. Every chat with an agent behind it is drained first,
    /// under a worker-wide gate that admits no new chat or agent work; the
    /// snapshot is deleted only when every drain reported, and only the
    /// snapshot.
    pub(super) fn begin_clear_history(
        &mut self,
        context: RequestContext,
        reply: SyncSender<ApiResult<ClearHistoryResponse>>,
    ) {
        if let Err(error) = authorize(&context, AiScope::Chat) {
            let _ = reply.send(Err(error));
            return;
        }
        if self.clearing.is_some() {
            let _ = reply.send(Err(history_clearing()));
            return;
        }
        if !self.active_work.is_empty() || !self.closing.is_empty() {
            let _ = reply.send(Err(active_history_conflict()));
            return;
        }
        let chats = match self.repository.chats() {
            Ok(chats) => chats,
            Err(error) => {
                let _ = reply.send(Err(ai_failure(error)));
                return;
            }
        };
        let snapshot: Vec<ChatId> = chats.iter().map(|chat| chat.id).collect();
        let agent_chats: Vec<ChatId> = chats
            .iter()
            .filter(|chat| is_agent_provider(&chat.current_model.provider))
            .map(|chat| chat.id)
            .collect();
        let Some(agents) = self.agents.clone().filter(|_| !agent_chats.is_empty()) else {
            let _ = reply.send(self.delete_snapshot(&snapshot));
            return;
        };
        let mut pending = HashSet::new();
        for chat in agent_chats {
            let output = self.output.clone();
            match agents.end_chat_sessions(
                chat,
                Box::new(move |outcome| {
                    let _ = output.send(Envelope::ChatSessionsEnded { chat, outcome });
                }),
            ) {
                Ok(()) => {
                    pending.insert(chat);
                }
                Err(error) => {
                    let _ = reply.send(Err(ai_failure(error)));
                    return;
                }
            }
        }
        self.clearing = Some(Clearing {
            snapshot,
            pending,
            failed: 0,
            reply,
        });
    }

    fn finish_clear_history(&mut self) {
        let Some(clearing) = self.clearing.take() else {
            return;
        };
        let answer = if clearing.failed > 0 {
            tracing::warn!(
                target: "koru_ai::agent",
                failed = clearing.failed,
                "clear history stopped: agents could not be stopped; every chat is kept"
            );
            Err(failure(
                "agents_still_running",
                "Couldn't stop every agent. Nothing was removed; try again.",
                true,
            ))
        } else {
            // The gate admitted nothing since the snapshot, so the snapshot
            // is the whole set; a chat outside it would be a broken gate,
            // and the transaction is aborted rather than trusted.
            match self.repository.chats() {
                Ok(now) if now.iter().any(|chat| !clearing.snapshot.contains(&chat.id)) => {
                    tracing::error!(
                        target: "koru_ai::agent",
                        "clear history found a chat outside its snapshot; aborting"
                    );
                    Err(failure(
                        "history_clearing",
                        "History could not be cleared. Try again.",
                        true,
                    ))
                }
                Ok(_) => self.delete_snapshot(&clearing.snapshot),
                Err(error) => Err(ai_failure(error)),
            }
        };
        let _ = clearing.reply.send(answer);
    }

    // ----- activity rows ----------------------------------------------

    /// One agent fact becomes one row write and one public event. The
    /// wording stays in `agent_live` for the length of the turn.
    pub(super) fn apply_agent_activity(
        &mut self,
        attempt: &InferenceAttempt,
        activity: AgentActivityEvent,
        at: Timestamp,
    ) {
        let (Some(chat_id), Some(assistant_message_id)) =
            (attempt.chat_id, attempt.assistant_message_id)
        else {
            return;
        };
        let activity = match activity {
            AgentActivityEvent::ModeChanged { started_in, mode } => {
                return self.follow_agent_mode(chat_id, started_in, mode);
            }
            AgentActivityEvent::Notice { text } => {
                return self.record_agent_notice(attempt, chat_id, assistant_message_id, text, at);
            }
            activity => activity,
        };
        let result = match activity {
            AgentActivityEvent::ModeChanged { .. } | AgentActivityEvent::Notice { .. } => return,
            AgentActivityEvent::ToolStarted {
                invocation,
                label,
                input,
            } => {
                let mut row = agent_row(
                    invocation,
                    attempt.id,
                    chat_id,
                    assistant_message_id,
                    AgentToolKind::Tool,
                    ToolApproval::NotRequired,
                    at,
                );
                // The agent's words are the row's, stored: the label it gave
                // the call and what it asked the tool to do (schema 8).
                row.agent_label = (!label.is_empty()).then_some(label);
                row.agent_input = input.as_deref().and_then(bound_agent_content);
                self.repository.create_tool_activity(&row).and_then(|()| {
                    row.text_offset = self.recorded_text_offset(invocation)?;
                    row.status = ToolActivityStatus::Running;
                    row.started_at = Some(at);
                    self.repository.advance_agent_activity(&row).map(|()| {
                        self.remember_agent_row(attempt.id, invocation);
                        row
                    })
                })
            }
            AgentActivityEvent::ToolCompleted {
                invocation,
                tool,
                succeeded,
                failure,
                detail,
                output,
            } => self.agent_row_mut(invocation, |row| {
                // The failure's own words stay with the row: the column is
                // the one Koru's own tools already keep theirs in. What the
                // tool produced is the row's too (schema 8, UC17).
                row.error_message = if succeeded { None } else { detail };
                row.agent_output = output.as_deref().and_then(bound_agent_content);
                // A kind the agent left unnamed is read from the call's
                // label: *Read README.md* is filed as a read.
                row.tool_name = tool
                    .refined(row.agent_label.as_deref())
                    .as_token()
                    .to_owned();
                row.status = if succeeded {
                    ToolActivityStatus::Complete
                } else {
                    ToolActivityStatus::Failed
                };
                row.completed_at = Some(at);
                row.error_code = (!succeeded).then(|| {
                    failure.map_or(ToolErrorCode::ExecutionFailed, |failure| {
                        failure.error_code()
                    })
                });
            }),
            AgentActivityEvent::ApprovalRequired {
                invocation,
                summary,
                choices,
            } => {
                let mut row = agent_row(
                    invocation,
                    attempt.id,
                    chat_id,
                    assistant_message_id,
                    AgentToolKind::Approval,
                    ToolApproval::Pending,
                    at,
                );
                row.agent_summary = (!summary.is_empty()).then_some(summary);
                self.repository.create_tool_activity(&row).and_then(|()| {
                    row.text_offset = self.recorded_text_offset(invocation)?;
                    row.status = ToolActivityStatus::ApprovalRequired;
                    self.repository.advance_agent_activity(&row).map(|()| {
                        self.remember_agent_row(attempt.id, invocation);
                        // Only the choices are live: a person answers them
                        // once, and nothing reopens them.
                        self.agent_live.insert(
                            invocation,
                            AgentActivityLive {
                                choices,
                                questions: Vec::new(),
                            },
                        );
                        row
                    })
                })
            }
            AgentActivityEvent::ApprovalResolved {
                invocation,
                approved,
            } => {
                if let Some(live) = self.agent_live.get_mut(&invocation) {
                    live.choices.clear();
                }
                self.agent_row_mut(invocation, |row| {
                    if approved {
                        row.status = ToolActivityStatus::Complete;
                        row.approval = ToolApproval::Approved;
                        row.started_at = Some(at);
                    } else {
                        row.status = ToolActivityStatus::Denied;
                        row.approval = ToolApproval::Denied;
                        row.error_code = Some(ToolErrorCode::PermissionDenied);
                    }
                    row.completed_at = Some(at);
                })
            }
            AgentActivityEvent::QuestionsAsked {
                invocation,
                questions,
                blocking,
                next_message,
            } => {
                // A permission request's row (D108): it reads as one in
                // every build. The set's words are kept on it — a plain
                // line, and the questions as a record — so it reads the
                // same after a reload; the options are live only.
                let mut row = agent_row(
                    invocation,
                    attempt.id,
                    chat_id,
                    assistant_message_id,
                    AgentToolKind::Approval,
                    ToolApproval::Pending,
                    at,
                );
                let record = koru_ai_core::QuestionsRecord::of(&questions)
                    .answered_by_next_message(next_message);
                let summary = koru_ai_core::bound_live_text(record.summary().trim());
                row.agent_summary = (!summary.is_empty()).then_some(summary);
                row.agent_input = record.encode();
                self.repository.create_tool_activity(&row).and_then(|()| {
                    row.text_offset = self.recorded_text_offset(invocation)?;
                    // A set the agent waits on is the person's to answer —
                    // the chat needs them; one it does not wait on runs.
                    if blocking {
                        row.status = ToolActivityStatus::ApprovalRequired;
                    } else {
                        row.status = ToolActivityStatus::Running;
                        row.started_at = Some(at);
                    }
                    self.repository.advance_agent_activity(&row).map(|()| {
                        self.remember_agent_row(attempt.id, invocation);
                        // A set answered by the next message outlives its
                        // reply, with its questions live (D108).
                        if next_message {
                            self.next_message_rows.insert(invocation, chat_id);
                        }
                        self.agent_live.insert(
                            invocation,
                            AgentActivityLive {
                                choices: Vec::new(),
                                questions,
                            },
                        );
                        row
                    })
                })
            }
            AgentActivityEvent::QuestionsAnswered {
                invocation,
                answers,
            } => {
                self.next_message_rows.remove(&invocation);
                if let Some(live) = self.agent_live.get_mut(&invocation) {
                    live.questions.clear();
                }
                self.agent_row_mut(invocation, |row| {
                    match answers {
                        // Answered: allowed, with what was said — a
                        // secret's arrives withheld and is kept so.
                        Some(answers) => {
                            row.status = ToolActivityStatus::Complete;
                            row.approval = ToolApproval::Approved;
                            row.started_at.get_or_insert(at);
                            row.agent_output = koru_ai_core::AnswersRecord { answers }.encode();
                        }
                        // Skipped: denied, as a refused request is.
                        None => {
                            row.status = ToolActivityStatus::Denied;
                            row.approval = ToolApproval::Denied;
                            row.started_at = None;
                            row.error_code = Some(ToolErrorCode::PermissionDenied);
                        }
                    }
                    row.completed_at = Some(at);
                })
            }
            AgentActivityEvent::ApprovalExpired { invocation } => {
                // A set passed over here first has nothing left to expire.
                if self.next_message_rows.remove(&invocation).is_none()
                    && matches!(
                        self.repository.tool_activity(invocation),
                        Ok(Some(row)) if row.status.is_terminal()
                    )
                {
                    return;
                }
                if let Some(live) = self.agent_live.get_mut(&invocation) {
                    live.choices.clear();
                    live.questions.clear();
                }
                self.agent_row_mut(invocation, |row| {
                    row.status = ToolActivityStatus::Cancelled;
                    if row.approval == ToolApproval::Pending {
                        row.approval = ToolApproval::Denied;
                    }
                    row.completed_at = Some(at);
                    row.error_code = Some(ToolErrorCode::Cancelled);
                })
            }
        };
        match result {
            Ok(mut row) => {
                row.live = self.agent_live.get(&row.id).cloned();
                // On the reply the fact came with: a set answered by the
                // next message closes in a reply after the one that asked.
                self.publish(AiEvent::ToolActivity {
                    attempt_id: attempt.id,
                    activity: row,
                });
            }
            Err(error) => {
                tracing::warn!(
                    target: "koru_ai::agent",
                    attempt = %attempt.id,
                    class = ?error.class,
                    "an agent activity could not be recorded"
                );
            }
        }
    }

    /// **A local agent's notice is kept with its reply** (koru-ai D69): one
    /// row, at the reply's length when it came, so a reopened chat shows it
    /// where it showed while the reply ran. A notice that cannot be written
    /// is logged and dropped; it never stops the reply.
    fn record_agent_notice(
        &mut self,
        attempt: &InferenceAttempt,
        chat_id: ChatId,
        assistant_message_id: MessageId,
        text: String,
        at: Timestamp,
    ) {
        let notice = koru_ai_core::AgentNotice {
            id: koru_ai_core::AgentNoticeId::new(),
            attempt_id: attempt.id,
            chat_id,
            assistant_message_id,
            text,
            text_offset: None,
            created_at: at,
        };
        match self.repository.create_agent_notice(&notice) {
            Ok(notice) => self.publish_agent_notice(notice),
            Err(error) => tracing::warn!(
                target: "koru_ai::agent",
                attempt = %attempt.id,
                class = ?error.class,
                "an agent notice could not be recorded"
            ),
        }
    }

    /// **The chat follows a mode the agent moved to itself** (koru-ai D7) —
    /// Claude leaving plan mode once its plan is approved — so its next
    /// message is sent in the mode the agent is in. Only while the chat is
    /// still in the mode the reply was sent in: a mode the person chose
    /// during the reply is theirs, and is the one the next message takes.
    fn follow_agent_mode(&mut self, chat_id: ChatId, started_in: Option<String>, mode: String) {
        let followed = self.repository.chat(chat_id).and_then(|chat| match chat {
            Some(chat) if chat.agent_mode == started_in => self
                .repository
                .set_chat_agent_mode(chat_id, Some(&mode))
                .map(|chat| chat.is_some()),
            _ => Ok(false),
        });
        match followed {
            Ok(followed) => tracing::info!(
                target: "koru_ai::agent",
                chat = %chat_id,
                followed,
                "the agent moved its approval mode"
            ),
            Err(error) => tracing::warn!(
                target: "koru_ai::agent",
                chat = %chat_id,
                class = ?error.class,
                "the agent's approval mode could not be recorded"
            ),
        }
    }

    /// Where the reply stood when the row was written, as the repository
    /// recorded it (schema 10): the public event carries it from the start.
    fn recorded_text_offset(&self, invocation: ToolInvocationId) -> Result<Option<u32>, AiError> {
        Ok(self
            .repository
            .tool_activity(invocation)?
            .and_then(|row| row.text_offset))
    }

    fn remember_agent_row(&mut self, attempt: InferenceAttemptId, invocation: ToolInvocationId) {
        self.tool_attempts.insert(invocation, attempt);
        self.agent_rows.entry(attempt).or_default().push(invocation);
    }

    fn agent_row_mut(
        &mut self,
        invocation: ToolInvocationId,
        change: impl FnOnce(&mut ToolActivity),
    ) -> Result<ToolActivity, AiError> {
        let mut row = self.repository.tool_activity(invocation)?.ok_or_else(|| {
            AiError::new(
                AiErrorClass::Storage,
                "The agent activity row is missing.",
                false,
            )
        })?;
        if row.status.is_terminal() {
            return Err(AiError::new(
                AiErrorClass::Storage,
                "The agent activity row already ended.",
                false,
            ));
        }
        change(&mut row);
        self.repository.advance_agent_activity(&row)?;
        Ok(row)
    }

    /// The turn ended: every agent row still open is cancelled, and the
    /// agent's wording leaves the live table with the turn.
    pub(super) fn finish_agent_rows(&mut self, attempt: InferenceAttemptId) {
        let Some(invocations) = self.agent_rows.remove(&attempt) else {
            return;
        };
        let at = Timestamp::now();
        for invocation in invocations {
            self.tool_attempts.remove(&invocation);
            // A set answered by the next message stays open past its reply
            // (D108): it closes when it is answered, skipped or passed over.
            if self.next_message_rows.contains_key(&invocation) {
                continue;
            }
            self.agent_live.remove(&invocation);
            let result = self.agent_row_mut(invocation, |row| {
                row.status = ToolActivityStatus::Cancelled;
                if row.approval == ToolApproval::Pending {
                    row.approval = ToolApproval::Denied;
                }
                row.completed_at = Some(at);
                row.error_code = Some(ToolErrorCode::Cancelled);
            });
            if let Ok(row) = result {
                self.publish_tool_activity(row);
            }
        }
    }

    /// A transcript read carries the live wording for rows whose turn is
    /// still running, and nothing for the rest.
    pub(super) fn decorate_live(&self, activities: &mut [ToolActivity]) {
        for activity in activities {
            activity.live = self.agent_live.get(&activity.id).cloned();
        }
    }

    // ----- approvals --------------------------------------------------

    /// Answer a local agent's permission request. The dispatcher drops the
    /// answer into the turn's channel; the turn's thread sends it. The row
    /// stays pending until the agent's own resolution event moves it.
    pub(super) fn answer_agent_approval(
        &mut self,
        activity: &ToolActivity,
        choice: Option<String>,
        approve: bool,
    ) -> ApiResult<ToolDecisionResponse> {
        let Some(agents) = &self.agents else {
            return Err(tool_not_resumable());
        };
        let attempt = self
            .tool_attempts
            .get(&activity.id)
            .copied()
            .ok_or_else(tool_not_resumable)?;
        agents
            .live()
            .answer(attempt, activity.id, choice.as_deref(), approve)
            .map_err(|error| match error {
                AnswerError::NotLive | AnswerError::Closed => tool_not_resumable(),
                AnswerError::NoSuchApproval => tool_decision_conflict(),
                AnswerError::ChoiceRequired => failure(
                    "choice_required",
                    "This permission request offers choices; name one.",
                    false,
                ),
                AnswerError::UnknownChoice => {
                    failure("invalid_request", "That choice was not offered.", false)
                }
            })?;
        let mut activity = activity.clone();
        activity.live = self.agent_live.get(&activity.id).cloned();
        Ok(ToolDecisionResponse { activity })
    }

    /// Whether a row is a set of an agent's questions (D108): it has them
    /// live, or keeps their record.
    pub(super) fn is_question_row(&self, activity: &ToolActivity) -> bool {
        self.agent_live
            .get(&activity.id)
            .is_some_and(|live| !live.questions.is_empty())
            || activity
                .agent_input
                .as_deref()
                .and_then(koru_ai_core::QuestionsRecord::decode)
                .is_some()
    }

    /// **Answer a local agent's questions, or skip them** (`None`, D108).
    /// Like an approval, the answer is dropped into the turn's channel and
    /// the row moves only when the agent's own resolution arrives. A set
    /// the agent does not wait on is answerable while its row runs.
    pub(super) fn answer_agent_questions(
        &mut self,
        activity: &ToolActivity,
        answers: Option<Vec<koru_ai_core::AgentAnswer>>,
        approve: bool,
    ) -> ApiResult<ToolDecisionResponse> {
        if activity.approval != ToolApproval::Pending
            || !matches!(
                activity.status,
                ToolActivityStatus::ApprovalRequired | ToolActivityStatus::Running
            )
        {
            return Err(tool_decision_conflict());
        }
        if !self.is_question_row(activity) {
            return Err(failure(
                "invalid_request",
                "This request asks no questions.",
                false,
            ));
        }
        if approve && answers.is_none() {
            return Err(failure(
                "answers_required",
                "These questions take answers; send one for each.",
                false,
            ));
        }
        let live_attempt = self
            .tool_attempts
            .get(&activity.id)
            .copied()
            .filter(|attempt| {
                self.agents
                    .as_ref()
                    .is_some_and(|agents| agents.live().is_live(*attempt))
            });
        // A set answered by the next message whose reply ended (D108): its
        // answers are a message of their own, and a skip is the chat's
        // alone — the agent hears nothing.
        if self.next_message_rows.contains_key(&activity.id) && live_attempt.is_none() {
            if approve {
                return Err(failure(
                    "answer_as_message",
                    "These questions are answered by your next message; send the answers as one.",
                    false,
                ));
            }
            return self.skip_next_message(activity);
        }
        let Some(agents) = &self.agents else {
            return Err(tool_not_resumable());
        };
        let attempt = self
            .tool_attempts
            .get(&activity.id)
            .copied()
            .ok_or_else(tool_not_resumable)?;
        agents
            .live()
            .answer_questions(attempt, activity.id, answers)
            .map_err(|error| match error {
                AnswerError::NotLive | AnswerError::Closed => tool_not_resumable(),
                AnswerError::NoSuchApproval => tool_decision_conflict(),
                AnswerError::ChoiceRequired | AnswerError::UnknownChoice => failure(
                    "invalid_request",
                    "Those answers do not fit the questions.",
                    false,
                ),
            })?;
        let mut activity = activity.clone();
        activity.live = self.agent_live.get(&activity.id).cloned();
        Ok(ToolDecisionResponse { activity })
    }

    // ----- questions answered by the next message (D108) --------------

    /// **Skip a set answered by the next message** after its reply ended:
    /// the row is denied as any skipped set is, and nothing reaches the
    /// agent — a skip costs nothing.
    fn skip_next_message(&mut self, activity: &ToolActivity) -> ApiResult<ToolDecisionResponse> {
        self.next_message_rows.remove(&activity.id);
        self.agent_live.remove(&activity.id);
        if let Some(agents) = &self.agents {
            agents.live().forget_next_message(activity.id);
        }
        let at = Timestamp::now();
        let row = self
            .agent_row_mut(activity.id, |row| {
                row.status = ToolActivityStatus::Denied;
                row.approval = ToolApproval::Denied;
                row.started_at = None;
                row.error_code = Some(ToolErrorCode::PermissionDenied);
                row.completed_at = Some(at);
            })
            .map_err(ai_failure)?;
        tracing::info!(target: "koru_ai::agent", "agent questions skipped after their reply");
        Ok(ToolDecisionResponse { activity: row })
    }

    /// **Close a set answered by the next message unanswered**: the person
    /// sent another message, or its agent's session is gone. The row is
    /// cancelled — the card says it can no longer be answered — and shown on
    /// `publish_on`'s reply when one is running.
    fn close_next_message_row(
        &mut self,
        invocation: ToolInvocationId,
        publish_on: Option<InferenceAttemptId>,
    ) {
        self.next_message_rows.remove(&invocation);
        self.agent_live.remove(&invocation);
        let at = Timestamp::now();
        let closed = self.agent_row_mut(invocation, |row| {
            row.status = ToolActivityStatus::Cancelled;
            if row.approval == ToolApproval::Pending {
                row.approval = ToolApproval::Denied;
            }
            row.completed_at = Some(at);
            row.error_code = Some(ToolErrorCode::Cancelled);
        });
        match (closed, publish_on) {
            (Ok(row), Some(attempt_id)) => self.publish(AiEvent::ToolActivity {
                attempt_id,
                activity: row,
            }),
            (Ok(_), None) => {}
            (Err(error), _) => tracing::warn!(
                target: "koru_ai::agent",
                class = ?error.class,
                "a passed-over question set could not be closed"
            ),
        }
    }

    /// **Another message passes over the chat's open sets** (D108): they
    /// can no longer be answered, here and in the agent's adapter, which
    /// expires its own when the message reaches it.
    pub(super) fn pass_over_questions(&mut self, chat: ChatId, attempt: InferenceAttemptId) {
        let passed: Vec<ToolInvocationId> = self
            .next_message_rows
            .iter()
            .filter(|(_, owner)| **owner == chat)
            .map(|(invocation, _)| *invocation)
            .collect();
        if passed.is_empty() {
            return;
        }
        if let Some(agents) = &self.agents {
            agents.live().pass_over_next_message(chat);
        }
        for invocation in passed {
            self.close_next_message_row(invocation, Some(attempt));
        }
        tracing::info!(target: "koru_ai::agent", "agent questions passed over by a message");
    }

    /// The chat's agent is gone: its sets answered by the next message are
    /// forgotten with it.
    fn forget_next_message_sets(&mut self, chat: ChatId) {
        let gone: Vec<ToolInvocationId> = self
            .next_message_rows
            .iter()
            .filter(|(_, owner)| **owner == chat)
            .map(|(invocation, _)| *invocation)
            .collect();
        for invocation in gone {
            self.next_message_rows.remove(&invocation);
            self.agent_live.remove(&invocation);
        }
        if let Some(agents) = &self.agents {
            agents.live().pass_over_next_message(chat);
        }
    }

    /// **A reply that carried a set's answers ended** (D108). Answered, the
    /// set closed in it; still open and no longer answerable — its agent's
    /// session ended under it — the set is closed now, on this reply,
    /// before the reply's own end is published. Still answerable — the
    /// reply failed on the way — it waits to be answered again.
    pub(super) fn finish_answer_attempt(&mut self, attempt: InferenceAttemptId) {
        let Some(invocation) = self.answer_attempts.remove(&attempt) else {
            return;
        };
        let answerable = self
            .agents
            .as_ref()
            .is_some_and(|agents| agents.live().next_message_answerable(invocation));
        if self.next_message_rows.contains_key(&invocation) && !answerable {
            self.close_next_message_row(invocation, Some(attempt));
        }
    }

    /// **Send a set's answers as the person's next message** (koru-ai
    /// D108): a set the agent asked in its reply and moved on from. The
    /// answers are a message of their own — a new reply, which costs what
    /// any message costs — and show as the set's card, never as a message
    /// of the person's: the reply follows the asking one, and the agent
    /// writes the message in its own form. The set closes in the reply when
    /// the agent takes the answers.
    pub(super) fn send_answers_turn(
        &mut self,
        request: SendTurnRequest,
        next: koru_ai_core::NextMessageAnswers,
    ) -> ApiResult<AiResponseStream> {
        if !request.markdown.trim().is_empty()
            || !request.attachment_ids.is_empty()
            || !request.explicit_refs.is_empty()
        {
            return Err(failure(
                "invalid_request",
                "Answers to an agent's questions are sent on their own.",
                false,
            ));
        }
        let chat = self
            .repository
            .chat(request.chat_id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        let activity = self
            .repository
            .tool_activity(next.invocation)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        if activity.chat_id != chat.id
            || !is_agent_provider(&chat.current_model.provider)
            || !self.next_message_rows.contains_key(&activity.id)
        {
            return Err(failure(
                "invalid_request",
                "These are not questions answered by a message.",
                false,
            ));
        }
        if activity.approval != ToolApproval::Pending || activity.status.is_terminal() {
            return Err(tool_decision_conflict());
        }
        let messages = self.repository.messages(chat.id).map_err(ai_failure)?;
        if messages.iter().any(|message| {
            message.role == ChatRole::Assistant && message.state == MessageState::Pending
        }) {
            return Err(failure(
                "chat_busy",
                "This chat already has a response in progress.",
                true,
            ));
        }
        let user = messages
            .iter()
            .rev()
            .find(|message| message.role == ChatRole::User)
            .ok_or_else(|| failure("invalid_request", "The chat has no message.", false))?
            .id;
        let Some(agents) = self.agents.clone() else {
            return Err(tool_not_resumable());
        };
        match agents
            .live()
            .check_next_message_answers(chat.id, activity.id, &next.answers)
        {
            Ok(()) => {}
            Err(AnswerError::NotLive) => {
                self.close_next_message_row(activity.id, None);
                return Err(failure(
                    "questions_closed",
                    koru_ai_core::AGENT_QUESTIONS_CLOSED,
                    false,
                ));
            }
            Err(AnswerError::ChoiceRequired) => {
                return Err(failure(
                    "answers_required",
                    "These questions take answers; send one for each.",
                    false,
                ));
            }
            Err(_) => {
                return Err(failure(
                    "invalid_request",
                    "Those answers do not fit the questions.",
                    false,
                ));
            }
        }
        let ordinal = messages.last().map_or(Ok(0), |message| {
            message.ordinal.checked_add(1).ok_or_else(invariant_failure)
        })?;
        let now = Timestamp::now();
        let assistant_id = MessageId::new();
        let attempt_id = InferenceAttemptId::new();
        let reasoning = chat.current_model.default_reasoning;
        let assistant_message = Message {
            id: assistant_id,
            chat_id: chat.id,
            ordinal,
            role: ChatRole::Assistant,
            state: MessageState::Pending,
            markdown: String::new(),
            origin_message_id: None,
            provider: Some(chat.current_model.provider.clone()),
            requested_model: Some(chat.current_model.requested_model.clone()),
            resolved_model: None,
            error_class: None,
            created_at: now,
            completed_at: None,
        };
        let provider_messages: Vec<AiRequestMessage> = messages
            .iter()
            .filter(|message| message.state == MessageState::Complete)
            .map(|message| AiRequestMessage {
                role: message.role,
                markdown: message.markdown.clone(),
                attachments: Vec::new(),
            })
            .collect();
        let attempt = InferenceAttempt {
            id: attempt_id,
            operation_id: request.operation_id,
            chat_id: Some(chat.id),
            assistant_message_id: Some(assistant_id),
            surface: request.surface,
            intent: AiIntent::Chat,
            provider: chat.current_model.provider.clone(),
            requested_model: chat.current_model.requested_model.clone(),
            resolved_model: None,
            requested_reasoning: reasoning,
            effective_reasoning: None,
            prompt_template_version: PROMPT_TEMPLATE_VERSION.into(),
            status: AttemptStatus::Pending,
            queued_at: now,
            context_started_at: None,
            context_completed_at: None,
            provider_started_at: None,
            first_token_at: None,
            completed_at: None,
            request_payload_bytes: None,
            web_search_requested: false,
            web_search_calls: 0,
            context_payload_bytes: 0,
            message_count: provider_messages.len().try_into().unwrap_or(u32::MAX),
            context_document_count: 0,
            context_message_block_count: 0,
            usage: Default::default(),
            stop_reason: None,
            error_class: None,
            provider_response_id: None,
        };
        // A reply of its own, after the one that asked; no message of the
        // person's is written — the answers are read on the set's card.
        self.repository
            .begin_retry(&PendingRetry {
                assistant_message,
                attempt: attempt.clone(),
            })
            .map_err(ai_failure)?;
        let (stream_sender, events) = sync_channel(STREAM_CAPACITY);
        self.streams.insert(
            attempt_id,
            StreamState {
                sender: stream_sender,
                next_sequence: 1,
            },
        );
        let cancellation = Cancellation::default();
        self.cancellations.insert(attempt_id, cancellation.clone());
        self.answer_attempts.insert(attempt_id, activity.id);
        let base_request = AiRequest {
            attempt_id,
            provider: attempt.provider.clone(),
            model: attempt.requested_model.clone(),
            reasoning,
            prompt_template_version: PROMPT_TEMPLATE_VERSION.into(),
            system: CHAT_PROMPT.into(),
            messages: provider_messages,
            context: Vec::new(),
            tools: self.tool_definitions.clone(),
            tool_history: Vec::new(),
            options: AiRequestOptions {
                chat_id: Some(chat.id),
                working_folder: chat.agent_working_folder.clone(),
                agent_mode: chat.agent_mode.clone(),
                agent_fast: chat.agent_fast,
                agent_turn_reasoning: self.agent_turn_reasoning(&chat.current_model, reasoning),
                agent_answers: Some(next),
                ..AiRequestOptions::default()
            },
        };
        self.work_started(attempt_id);
        let started_at = Timestamp::now();
        // No context goes with answers: the chat's pinned documents are not
        // read again, and nothing is recorded against the asking message.
        let job = ContextJob {
            attempt_id,
            user_message_id: user,
            chat_id: chat.id,
            request: base_request,
            refs: Vec::new(),
            known_documents: HashMap::new(),
            replace_pinned_documents: false,
            started_at,
            cancellation,
        };
        match self.context_pool.try_submit(job) {
            Ok(()) => self.work_admitted(attempt_id),
            Err(error) => self.fail_context(attempt_id, started_at, error),
        }
        tracing::info!(attempt = %attempt_id, "AI attempt queued with an agent's answers");
        Ok(AiResponseStream {
            accepted: SendTurnAccepted {
                attempt_id,
                user_message_id: user,
                assistant_message_id: assistant_id,
                replayed: false,
            },
            events,
        })
    }

    // ----- steering ---------------------------------------------------

    /// **Steer a running agent reply**: the person's text goes into the turn
    /// that is running. The dispatcher only checks and hands it over — the
    /// turn's own thread sends it on its lease, and reports back as an
    /// envelope ([`Self::on_steer_delivered`]); the reply is held meanwhile.
    /// Only once the agent took it is the message recorded, after the
    /// running reply, so the transcript never shows a steer the agent did
    /// not get.
    pub(super) fn begin_steer_attempt(
        &mut self,
        context: RequestContext,
        attempt_id: InferenceAttemptId,
        request: SteerAttemptRequest,
        reply: SyncSender<ApiResult<SteerAttemptResponse>>,
    ) {
        match self.admit_steer(context, attempt_id, &request) {
            Ok(Admitted::Replayed(response)) => {
                let _ = reply.send(Ok(response));
            }
            Ok(Admitted::Ready { chat, agents }) => {
                let operation = request.operation_id;
                let output = self.output.clone();
                let markdown = request.markdown.clone();
                // The turn answers through `done`; a refusal before the
                // hand-over is answered here, on this copy.
                let refused = reply.clone();
                let handed = agents.live().steer(
                    attempt_id,
                    &operation.hyphenated().to_string(),
                    request.markdown.clone(),
                    Box::new(move |outcome| {
                        let _ = output.send(Envelope::SteerDelivered(Box::new(SteerDelivery {
                            attempt: attempt_id,
                            chat,
                            operation,
                            markdown,
                            outcome,
                            reply,
                        })));
                    }),
                );
                match handed {
                    Ok(()) => {
                        self.steering.insert(operation);
                    }
                    Err(error) => {
                        tracing::info!(
                            target: "koru_ai::agent",
                            attempt = %attempt_id,
                            error = ?error,
                            "a steer could not be handed to its turn"
                        );
                        let _ = refused.send(Err(steer_unavailable()));
                    }
                }
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    /// The checks a steer passes before it is handed over: a live agent
    /// turn of a chat that is not closing, and text to send. A steer already
    /// recorded answers as it did.
    fn admit_steer(
        &mut self,
        context: RequestContext,
        attempt_id: InferenceAttemptId,
        request: &SteerAttemptRequest,
    ) -> ApiResult<Admitted> {
        authorize(&context, AiScope::Chat)?;
        let recorded = self
            .repository
            .message(MessageId::from_uuid(request.operation_id))
            .map_err(ai_failure)?;
        if let Some(message) = recorded {
            return Ok(Admitted::Replayed(SteerAttemptResponse {
                attempt_id,
                user_message_id: message.id,
                replayed: true,
            }));
        }
        if self.steering.contains(&request.operation_id) {
            return Err(failure(
                "idempotency_conflict",
                "This message is already being sent into the reply.",
                true,
            ));
        }
        if request.markdown.trim().is_empty() {
            return Err(failure("invalid_request", "A message is required.", false));
        }
        let attempt = self
            .repository
            .attempt(attempt_id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        let chat = attempt.chat_id.ok_or_else(steer_unavailable)?;
        self.admit_chat_work(Some(chat))?;
        if attempt.status.is_terminal() || !is_agent_provider(&attempt.provider) {
            return Err(steer_unavailable());
        }
        let Some(agents) = self.agents.clone() else {
            return Err(steer_unavailable());
        };
        if !agents.live().is_live(attempt_id) {
            return Err(steer_unavailable());
        }
        Ok(Admitted::Ready { chat, agents })
    }

    /// The turn's thread reported on a steer: the message is recorded after
    /// the running reply and answered, or the refusal is.
    pub(super) fn on_steer_delivered(&mut self, delivery: SteerDelivery) {
        self.steering.remove(&delivery.operation);
        let answer = match delivery.outcome {
            Ok(()) => {
                // Another message: the chat's open sets are passed over.
                self.pass_over_questions(delivery.chat, delivery.attempt);
                let now = Timestamp::now();
                let message = Message {
                    id: MessageId::from_uuid(delivery.operation),
                    chat_id: delivery.chat,
                    ordinal: 0,
                    role: ChatRole::User,
                    state: MessageState::Complete,
                    markdown: delivery.markdown,
                    origin_message_id: None,
                    provider: None,
                    requested_model: None,
                    resolved_model: None,
                    error_class: None,
                    created_at: now,
                    completed_at: Some(now),
                };
                self.repository
                    .append_user_message(&message)
                    .map(|stored| {
                        tracing::info!(
                            target: "koru_ai::agent",
                            attempt = %delivery.attempt,
                            "a running agent reply was steered"
                        );
                        SteerAttemptResponse {
                            attempt_id: delivery.attempt,
                            user_message_id: stored.id,
                            replayed: false,
                        }
                    })
                    .map_err(ai_failure)
            }
            Err(error) => {
                tracing::info!(
                    target: "koru_ai::agent",
                    attempt = %delivery.attempt,
                    error = ?error,
                    "a steer did not reach its turn"
                );
                Err(match error {
                    SteerError::Failed => failure(
                        "steer_failed",
                        "The message could not be sent into the reply.",
                        true,
                    ),
                    SteerError::NotLive
                    | SteerError::Busy
                    | SteerError::Refused
                    | SteerError::Closed => steer_unavailable(),
                })
            }
        };
        let _ = delivery.reply.send(answer);
    }
}

/// What a steer came to on the turn's thread, back on the dispatcher.
pub(super) struct SteerDelivery {
    attempt: InferenceAttemptId,
    chat: ChatId,
    operation: Uuid,
    markdown: String,
    outcome: Result<(), SteerError>,
    reply: SyncSender<ApiResult<SteerAttemptResponse>>,
}

enum Admitted {
    Replayed(SteerAttemptResponse),
    Ready { chat: ChatId, agents: AgentSessions },
}

/// The reply cannot take text now: it ended, it is not an agent's, or the
/// agent has no mid-turn input. The client sends the message after the
/// reply instead.
fn steer_unavailable() -> Failure {
    failure(
        "steer_unavailable",
        "This reply cannot take a message now; it will be sent when the reply ends.",
        true,
    )
}

fn agent_row(
    invocation: ToolInvocationId,
    attempt_id: InferenceAttemptId,
    chat_id: ChatId,
    assistant_message_id: MessageId,
    kind: AgentToolKind,
    approval: ToolApproval,
    at: Timestamp,
) -> ToolActivity {
    ToolActivity {
        id: invocation,
        attempt_id,
        chat_id,
        assistant_message_id,
        tool_name: kind.as_token().to_owned(),
        read_only: false,
        status: ToolActivityStatus::Proposed,
        approval,
        proposed_at: at,
        started_at: None,
        completed_at: None,
        result_bytes: None,
        error_code: None,
        target_workspace_id: None,
        target_document_id: None,
        error_message: None,
        agent_label: None,
        agent_summary: None,
        agent_input: None,
        agent_output: None,
        text_offset: None,
        display_target: None,
        live: None,
    }
}

/// An agent's readiness as a provider validation, for the picker and the
/// Settings card. Status words are closed codes the surface localizes.
pub(super) fn agent_validation(
    agents: Option<&AgentSessions>,
    provider: AiProviderId,
) -> ApiResult<ProviderValidationResponse> {
    let Some(kind) = agent_kind_of(&provider) else {
        return Err(failure(
            "invalid_request",
            "The selected AI provider is not supported.",
            false,
        ));
    };
    let Some(agents) = agents else {
        return Ok(ProviderValidationResponse {
            provider,
            status: ProviderValidationStatus::Unavailable,
            models: Vec::new(),
            selected_model: None,
            failure: Some(failure(
                "agent_service_unavailable",
                "Local agents are not available in this AI service.",
                false,
            )),
        });
    };
    let readiness = agents.readiness(&kind.kind).map_err(ai_failure)?;
    let (status, code) = match readiness.map(|row| row.availability) {
        Some(ExecutableAvailability::Available { .. }) => (ProviderValidationStatus::Ready, None),
        Some(ExecutableAvailability::Checking) => (
            ProviderValidationStatus::Unavailable,
            Some("agent_checking"),
        ),
        Some(ExecutableAvailability::Missing) | None => (
            ProviderValidationStatus::Unavailable,
            Some("agent_not_installed"),
        ),
        Some(ExecutableAvailability::SignedOut { .. }) => (
            ProviderValidationStatus::Unavailable,
            Some("agent_signed_out"),
        ),
        Some(ExecutableAvailability::Incompatible { .. }) => (
            ProviderValidationStatus::Unavailable,
            Some("agent_unsupported_version"),
        ),
        Some(ExecutableAvailability::Failed { .. }) => {
            (ProviderValidationStatus::Failed, Some("agent_check_failed"))
        }
    };
    Ok(ProviderValidationResponse {
        provider,
        status,
        models: Vec::new(),
        selected_model: None,
        failure: code.map(|code| failure(code, "The agent is not ready.", true)),
    })
}

/// The code a client reads when an agent's models could not be answered.
/// `Backpressure` from the host — or a host that did not answer in time — is
/// a list still being built: `agent_models_listing`, which a client asks
/// again for rather than calling a failure. Anything else is
/// `agent_models_failed`.
pub(super) fn agent_models_failure(code: koru_session_protocol::ErrorCode) -> Failure {
    if code == koru_session_protocol::ErrorCode::Backpressure {
        failure(
            "agent_models_listing",
            "The agent's models are still being listed.",
            true,
        )
    } else {
        failure(
            "agent_models_failed",
            "The agent could not list its models.",
            true,
        )
    }
}

/// **What an agent can be told to do by name in a folder** (koru-ai D121):
/// Codex's skills for the composer's `$`, in the session host's order. The
/// folder is the chat's — absent, the home folder its agent starts in — and
/// must be an existing folder, as a chat's folder must be. An agent with
/// nothing to offer, or a host too old to say, answers an empty list: `$`
/// is then only a character.
pub(super) fn agent_commands(
    agents: &AgentSessions,
    provider: AiProviderId,
    folder: Option<String>,
) -> ApiResult<koru_ai_wire::AgentCommandsResponse> {
    let Some(kind) = agent_kind_of(&provider) else {
        return Err(failure(
            "invalid_request",
            "The selected AI provider is not supported.",
            false,
        ));
    };
    let folder = match folder.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(folder) => {
            let path = std::path::PathBuf::from(folder);
            if !path.is_absolute() || !path.is_dir() {
                return Err(failure(
                    "invalid_request",
                    "The working folder must be an existing folder on this computer.",
                    false,
                ));
            }
            Some(path)
        }
    };
    let commands = agents.discover_commands(&kind, folder).map_err(|code| {
        if code == koru_session_protocol::ErrorCode::Backpressure {
            failure(
                "agent_commands_busy",
                "The agent is busy listing; try again.",
                true,
            )
        } else {
            failure(
                "agent_commands_failed",
                "The agent could not list its skills.",
                true,
            )
        }
    })?;
    use koru_ai_wire::AgentCommandKindView as Kind;
    use koru_session_protocol::AgentCommandKind;
    Ok(koru_ai_wire::AgentCommandsResponse {
        provider,
        commands: commands
            .into_iter()
            .map(|command| koru_ai_wire::AgentCommandView {
                kind: match command.kind {
                    AgentCommandKind::RepoSkill => Kind::RepoSkill,
                    AgentCommandKind::UserSkill => Kind::UserSkill,
                    AgentCommandKind::SystemSkill => Kind::SystemSkill,
                    AgentCommandKind::AdminSkill => Kind::AdminSkill,
                },
                name: command.name,
                description: command.description,
                display_name: command.display_name,
            })
            .collect(),
    })
}

/// The models an agent lists, in the picker's shape. An agent that lists
/// none answers with an empty list, which the picker shows as no level.
/// With `refresh`, the list is rebuilt first (the manual refresh).
pub(super) fn agent_models(
    agents: &AgentSessions,
    provider: AiProviderId,
    refresh: bool,
) -> ApiResult<AgentModelsResponse> {
    let Some(kind) = agent_kind_of(&provider) else {
        return Err(failure(
            "invalid_request",
            "The selected AI provider is not supported.",
            false,
        ));
    };
    let catalog = agents
        .discover_models(&kind, refresh)
        .map_err(agent_models_failure)?;
    let label = |label: &Option<koru_session_protocol::ModelLabel>| {
        label.as_ref().map(|label| label.as_str().to_owned())
    };
    let (modes, default_mode) = catalog
        .modes
        .as_ref()
        .map_or_else(Default::default, |modes| {
            (
                modes
                    .modes()
                    .iter()
                    .map(|mode| koru_ai_wire::AgentModeView {
                        id: mode.id.as_str().to_owned(),
                        name: label(&mode.name),
                        description: label(&mode.description),
                    })
                    .collect(),
                modes.current().map(|id| id.as_str().to_owned()),
            )
        });
    Ok(AgentModelsResponse {
        provider,
        modes,
        default_mode,
        levels_per_turn: catalog.levels_per_turn,
        models: catalog
            .models
            .models()
            .iter()
            .map(|model| AgentModelView {
                id: model.id.as_str().to_owned(),
                group: model.group.as_str().to_owned(),
                name: label(&model.name),
                description: label(&model.description),
                group_name: label(&model.group_name),
                legacy: model.legacy,
                reasoning: model.reasoning.as_ref().and_then(reasoning_control),
                context: model
                    .context
                    .as_ref()
                    .map(|context| koru_ai_wire::AgentModelContext {
                        id: context.id.as_str().to_owned(),
                        tokens: context.tokens,
                    }),
                fast: model.fast.map(|fast| koru_ai_wire::AgentModelFast {
                    on_by_default: fast.on_by_default,
                }),
            })
            .collect(),
    })
}

/// A model's levels as the chat's effort control (agent-models D14): the
/// levels in Koru's order, the agent's stated default when it states one,
/// and *off* when it offers that. `None` when no level is one a chat can
/// choose.
pub(super) fn reasoning_control(
    reasoning: &koru_session_protocol::ModelReasoning,
) -> Option<ReasoningControl> {
    let level = |level: koru_session_protocol::LaunchReasoning| {
        koru_ai_core::ReasoningLevel::parse(level.as_str())
    };
    let values: Vec<_> = reasoning.levels.iter().filter_map(|l| level(*l)).collect();
    (!values.is_empty()).then(|| ReasoningControl::Levels {
        values,
        default: reasoning.default.and_then(level),
        supports_off: reasoning.off,
    })
}

/// Each listed model's control, as the dispatcher's index keeps it — under
/// its longer context's id too, which runs the same model (koru-ai D10).
pub(super) fn agent_controls(response: &AgentModelsResponse) -> Vec<(String, ReasoningControl)> {
    response
        .models
        .iter()
        .filter_map(|model| Some((model, model.reasoning.clone()?)))
        .flat_map(|(model, control)| {
            let context = model
                .context
                .as_ref()
                .map(|context| (context.id.clone(), control.clone()));
            std::iter::once((model.id.clone(), control)).chain(context)
        })
        .collect()
}

fn history_clearing() -> Failure {
    failure(
        "history_clearing",
        "History is being cleared. Try again in a moment.",
        true,
    )
}
