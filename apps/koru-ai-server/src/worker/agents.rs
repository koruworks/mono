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
    AgentSessions, AnswerError, EndOutcome, agent_kind_of, is_agent_provider,
};
use koru_ai_wire::{
    AgentModelView, AgentModelsResponse, ClearHistoryResponse, DeleteChatResponse,
    ProviderValidationResponse, ProviderValidationStatus,
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
        let result = match activity {
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
                row.tool_name = tool.as_token().to_owned();
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
                    row.status = ToolActivityStatus::ApprovalRequired;
                    self.repository.advance_agent_activity(&row).map(|()| {
                        self.remember_agent_row(attempt.id, invocation);
                        // Only the choices are live: a person answers them
                        // once, and nothing reopens them.
                        self.agent_live
                            .insert(invocation, AgentActivityLive { choices });
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
            AgentActivityEvent::ApprovalExpired { invocation } => {
                if let Some(live) = self.agent_live.get_mut(&invocation) {
                    live.choices.clear();
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
                self.publish_tool_activity(row);
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

/// The models an agent lists, in the picker's shape. An agent that lists
/// none answers with an empty list, which the picker shows as no level.
pub(super) fn agent_models(
    agents: &AgentSessions,
    provider: AiProviderId,
) -> ApiResult<AgentModelsResponse> {
    let Some(kind) = agent_kind_of(&provider) else {
        return Err(failure(
            "invalid_request",
            "The selected AI provider is not supported.",
            false,
        ));
    };
    let models = agents.discover_models(&kind).map_err(|_| {
        failure(
            "agent_models_failed",
            "The agent could not list its models.",
            true,
        )
    })?;
    Ok(AgentModelsResponse {
        provider,
        models: models
            .models()
            .iter()
            .map(|model| AgentModelView {
                id: model.id.as_str().to_owned(),
                group: model.group.as_str().to_owned(),
            })
            .collect(),
    })
}

fn history_clearing() -> Failure {
    failure(
        "history_clearing",
        "History is being cleared. Try again in a moment.",
        true,
    )
}
