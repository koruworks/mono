// SPDX-License-Identifier: MPL-2.0
//! Finite native commands, with live identity checked before queueing and by
//! the delegate owner at execution. Only observations settle permission rows.
use super::*;
mod decision;
#[cfg(test)]
mod tests;

pub(super) fn validate_bot_answers(
    questions: &[koru_session_protocol::AgentQuestion],
    answers: Vec<koru_ai_core::AgentAnswer>,
) -> ApiResult<Vec<koru_session_protocol::QuestionAnswer>> {
    super::preparation::encoded_size(&answers).map_err(ai_failure)?;
    let answers: Vec<_> = answers
        .into_iter()
        .map(koru_ai_provider_agent::protocol_answer)
        .collect();
    let valid =
        koru_session_protocol::validate_answers(questions, answers.clone()).map_err(|_| {
            failure(
                "invalid_request",
                "Those answers do not fit the questions.",
                false,
            )
        })?;
    if valid != answers {
        return Err(failure(
            "invalid_request",
            "Those answers exceed the supported format or length. Edit them before sending.",
            false,
        ));
    }
    Ok(valid)
}

pub(in crate::worker) struct ControlJob {
    context: CommandContext,
    workspace: WorkspaceId,
    attempt: InferenceAttemptId,
    invocation: ToolInvocationId,
    command: DelegateCommand,
    port: Arc<dyn BotDelegatePort>,
    reply: SyncSender<ApiResult<ToolDecisionResponse>>,
    /// A decision card's answer: human proof and reread off the writer.
    decision: Option<Box<decision::DecisionHuman>>,
}
pub(in crate::worker) struct ControlResult {
    context: CommandContext,
    workspace: WorkspaceId,
    attempt: InferenceAttemptId,
    invocation: ToolInvocationId,
    result: Result<BotReply, koru_delegate_api::Failure>,
    reply: SyncSender<ApiResult<ToolDecisionResponse>>,
    /// The decision answer as actually sent, when one was.
    decision: Option<Box<decision::DecisionSent>>,
}
enum PreparedDecision {
    Forward(ControlJob),
    Local(ToolDecisionResponse),
}
impl ControlJob {
    #[cfg(test)]
    pub(super) fn fixture(port: Arc<dyn BotDelegatePort>) -> Self {
        use koru_delegate_core as d;
        use koru_session_protocol as s;
        Self {
            context: RequestContext {
                principal: koru_ai_api::AiPrincipal {
                    label: "fixture".into(),
                    scopes: vec![AiScope::Chat, AiScope::ManageBots],
                },
                trace_id: "fixture".into(),
                deadline_millis: None,
            }
            .into(),
            workspace: WorkspaceId::new(),
            attempt: InferenceAttemptId::new(),
            invocation: ToolInvocationId::new(),
            command: DelegateCommand::AnswerApproval {
                fence: BotExecutionFence {
                    owner: d::BotOwner {
                        user_scope: UserScopeId::new(),
                        device: DeviceId::from_uuid(Uuid::new_v4()),
                    },
                    bot: BotId::mint(),
                    run: d::RunId::mint(),
                    attempt: d::RunAttemptId::mint(),
                    generation: d::AttemptGeneration::from_raw(1).unwrap(),
                    session: s::SessionId::mint(),
                    turn: Some(s::AgentTurnId::mint()),
                },
                request: d::IdempotencyKey::mint(),
                approval: s::ApprovalId::mint(),
                decision: s::ApprovalDecision::Reject,
                choice: None,
            },
            port,
            reply: sync_channel(1).0,
            decision: None,
        }
    }
    pub(in crate::worker) fn run(self) -> ControlResult {
        let ControlJob {
            context,
            workspace,
            attempt,
            invocation,
            command,
            port,
            reply,
            decision,
        } = self;
        let (result, sent) = catch_unwind(AssertUnwindSafe(|| match decision {
            Some(human) => decision::answer(&human, command, port.as_ref()),
            None => (port.execute(command), None),
        }))
        .unwrap_or_else(|_| {
            (
                Err(koru_delegate_api::Failure {
                    code: "bot_unavailable".into(),
                    message: "Bot control unavailable.".into(),
                    retryable: true,
                    details: Default::default(),
                }),
                None,
            )
        });
        ControlResult {
            context,
            workspace,
            attempt,
            invocation,
            result,
            reply,
            decision: sent,
        }
    }
}

pub(in crate::worker) struct ControlReadJob {
    result: ControlResult,
    authority: Arc<dyn BotAuthority>,
}
pub(in crate::worker) struct ControlReadResult {
    result: ControlResult,
    authority: ApiResult<Arc<dyn BotAuthority>>,
}
impl ControlReadJob {
    pub(in crate::worker) fn run(self) -> ControlReadResult {
        let authority = catch_unwind(AssertUnwindSafe(|| {
            self.authority
                .inspect(&[self.result.workspace], &[])
                .map(|value| Arc::new(value) as Arc<dyn BotAuthority>)
                .map_err(bot_failure)
        }))
        .unwrap_or_else(|_| Err(bot_failure(BotError::Unavailable)));
        ControlReadResult {
            result: self.result,
            authority,
        }
    }
}

impl WorkerState {
    pub(in crate::worker) fn handle_bot_interaction(
        &mut self,
        command: WorkerCommand,
    ) -> Option<WorkerCommand> {
        if !matches!(
            &command,
            WorkerCommand::ApproveToolCall { .. } | WorkerCommand::DenyToolCall { .. }
        ) {
            return Some(command);
        }
        let bot = super::super::command_chat(&self.repository, &command).and_then(|chat| {
            chat.map(|chat| self.repository.bot_for_chat(chat).map_err(bot_failure))
                .transpose()
        });
        match bot {
            Ok(Some(Some(_))) => {}
            Ok(_) => return Some(command),
            Err(error) => {
                command.refuse(error);
                return None;
            }
        }
        match command {
            WorkerCommand::ApproveToolCall {
                context,
                tool,
                decision,
                reply,
            } => {
                self.begin_bot_decision(context, tool, decision, true, reply);
            }
            WorkerCommand::DenyToolCall {
                context,
                tool,
                choice,
                reply,
            } => {
                self.begin_bot_decision(
                    context,
                    tool,
                    koru_ai_wire::ToolDecisionRequest {
                        choice,
                        answers: None,
                    },
                    false,
                    reply,
                );
            }
            _ => unreachable!("selected interaction"),
        }
        None
    }

    fn begin_bot_decision(
        &mut self,
        context: CommandContext,
        invocation: ToolInvocationId,
        decision: koru_ai_wire::ToolDecisionRequest,
        approve: bool,
        reply: SyncSender<ApiResult<ToolDecisionResponse>>,
    ) {
        let prepared = (|| -> ApiResult<PreparedDecision> {
            authorize(&context, AiScope::Chat)?;
            let access = self.bot_read_access(&context)?;
            let row = self
                .repository
                .tool_activity(invocation)
                .map_err(ai_failure)?
                .ok_or_else(not_found)?;
            let bot = self
                .repository
                .bot_for_chat(row.chat_id)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            let bot = self
                .repository
                .bot(bot)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            self.repository
                .chat_with_access(row.chat_id, &access)
                .map_err(ai_failure)?
                .ok_or_else(not_found)?;
            if self.bot_interactions.contains(&invocation) {
                return Err(failure(
                    "idempotency_conflict",
                    "This answer is already being sent.",
                    true,
                ));
            }
            if row.approval != ToolApproval::Pending
                || !matches!(
                    row.status,
                    ToolActivityStatus::ApprovalRequired | ToolActivityStatus::Running
                )
            {
                return Err(tool_decision_conflict());
            }
            let attempt = self
                .repository
                .attempt(row.attempt_id)
                .map_err(ai_failure)?
                .ok_or_else(not_found)?;
            if attempt.status.is_terminal() {
                if row
                    .agent_input
                    .as_deref()
                    .and_then(koru_ai_core::QuestionsRecord::decode)
                    .is_some_and(|record| record.next_message)
                    && self
                        .repository
                        .bot_source_for_tool(row.attempt_id, row.id)
                        .map_err(ai_failure)?
                        .is_some_and(|source| source.kind == BotToolSourceKind::Questions)
                {
                    if !approve && decision.choice.is_none() && decision.answers.is_none() {
                        return self.skip_next_message(&row).map(PreparedDecision::Local);
                    }
                    return Err(failure(
                        "answer_as_message",
                        "These questions are answered by your next message; send the answers as one.",
                        false,
                    ));
                }
                return Err(tool_not_resumable());
            }
            // A decision's card answers through the decision route, never
            // through the native prompt's legacy answer.
            if let Some(card) = self
                .repository
                .bot_decision_card(invocation)
                .map_err(bot_failure)?
            {
                return self
                    .prepare_decision_answer(&context, &bot, &row, card, &decision, approve, &reply)
                    .map(PreparedDecision::Forward);
            }
            let chat = self
                .bot_chats
                .get(&row.attempt_id)
                .ok_or_else(tool_not_resumable)?;
            let fence = chat.control_fence().ok_or_else(tool_not_resumable)?;
            let stored = self
                .repository
                .bot_replay_state(row.attempt_id)
                .map_err(ai_failure)?;
            if stored.stop_reason.is_some() || stored.fence.as_ref() != Some(&fence) {
                return Err(tool_not_resumable());
            }
            let live = chat.live.iter().find(|action| matches!(action,
                AgentLiveAction::OfferApproval {invocation:id,..} | AgentLiveAction::OfferQuestions {invocation:id,..} if *id==invocation))
                .ok_or_else(tool_not_resumable)?;
            let (source, command) = match live {
                AgentLiveAction::OfferApproval {
                    approval, choices, ..
                } => {
                    if decision.answers.is_some()
                        || (choices.is_empty() && decision.choice.is_some())
                    {
                        return Err(tool_decision_conflict());
                    }
                    let (answer, choice) = koru_ai_provider_agent::select_approval_choice(
                        choices,
                        decision.choice.as_deref(),
                        approve,
                    )
                    .map_err(|_| tool_decision_conflict())?;
                    (
                        AgentSource::Approval(*approval),
                        DelegateCommand::AnswerApproval {
                            fence,
                            // O138: one stable key per human answer, derived
                            // from this prompt, decision and choice, so a
                            // retry resends the same key and a different
                            // answer never shares it.
                            request: decision::native_answer_key(*approval, answer, choice)?,
                            approval: *approval,
                            decision: answer,
                            choice,
                        },
                    )
                }
                AgentLiveAction::OfferQuestions {
                    request, questions, ..
                } => {
                    if decision.choice.is_some() {
                        return Err(tool_decision_conflict());
                    }
                    let command = if approve {
                        let answers = decision.answers.ok_or_else(|| {
                            failure(
                                "answers_required",
                                "These questions take an answer for each item.",
                                false,
                            )
                        })?;
                        let answers = validate_bot_answers(questions, answers)?;
                        DelegateCommand::AnswerQuestions {
                            fence,
                            request: *request,
                            answers,
                        }
                    } else {
                        DelegateCommand::SkipQuestions {
                            fence,
                            request: *request,
                        }
                    };
                    (AgentSource::Questions(*request), command)
                }
                _ => unreachable!("selected live request"),
            };
            if self
                .repository
                .bot_tool_source(row.attempt_id, &super::replay::source(source))
                .map_err(ai_failure)?
                != Some(invocation)
            {
                return Err(tool_decision_conflict());
            }
            super::preparation::encoded_size(&command).map_err(ai_failure)?;
            Ok(PreparedDecision::Forward(ControlJob {
                context: context.clone(),
                workspace: bot.home_workspace,
                attempt: row.attempt_id,
                invocation,
                command,
                port: self.bot_delegate.clone().ok_or_else(tool_not_resumable)?,
                reply: reply.clone(),
                decision: None,
            }))
        })();
        match prepared {
            Ok(PreparedDecision::Local(response)) => {
                let _ = reply.send(Ok(response));
            }
            Ok(PreparedDecision::Forward(job)) => match self
                .provider_admin
                .as_ref()
                .ok_or_else(unavailable_failure)
                .and_then(|pool| {
                    pool.try_submit(ProviderAdminJob::BotInteraction(job))
                        .map_err(|error| error.failure)
                }) {
                Ok(()) => {
                    self.bot_interactions.insert(invocation);
                }
                Err(error) => {
                    let _ = reply.send(Err(error));
                }
            },
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    pub(in crate::worker) fn complete_bot_interaction(&mut self, mut result: ControlResult) {
        if let Some(sent) = result.decision.take() {
            self.settle_decision_answer(&mut result, *sent);
        }
        if matches!(&result.result, Ok(BotReply::Applied)) {
            let reply = result.reply.clone();
            let invocation = result.invocation;
            let submitted = self
                .bot_authority
                .clone()
                .ok_or_else(unavailable_failure)
                .and_then(|authority| {
                    self.provider_admin
                        .as_ref()
                        .ok_or_else(unavailable_failure)?
                        .try_submit(ProviderAdminJob::BotInteractionRead(ControlReadJob {
                            result,
                            authority,
                        }))
                        .map_err(|error| error.failure)
                });
            if let Err(error) = submitted {
                self.bot_interactions.remove(&invocation);
                let _ = reply.send(Err(error));
            }
            return;
        }
        self.bot_interactions.remove(&result.invocation);
        let answer = match result.result {
            Err(error) => Err(decision::person_failure(&error.code, error.retryable)),
            _ => Err(tool_not_resumable()),
        };
        let _ = result.reply.send(answer);
    }

    pub(in crate::worker) fn complete_bot_interaction_read(&mut self, checked: ControlReadResult) {
        let result = checked.result;
        self.bot_interactions.remove(&result.invocation);
        let answer = (|| {
            authorize(&result.context, AiScope::Chat)?;
            let old = result.context.prepared.as_ref().ok_or_else(not_found)?;
            let context = CommandContext {
                #[cfg(test)]
                memory_for_fixture: None,
                request: result.context.request.clone(),
                prepared: Some(Arc::new(PreparedBotCommand {
                    authority: checked.authority?,
                    chats: old.chats.clone(),
                    runtime: Ok(()),
                    pins: Vec::new(),
                    library_sources: None,
                    chat_snapshot: None,
                    settings: None,
                    terminal_question: None,
                })),
            };
            let access = self.bot_read_access(&context)?;
            let mut activity = self
                .repository
                .tool_activity(result.invocation)
                .map_err(ai_failure)?
                .filter(|row| row.attempt_id == result.attempt)
                .ok_or_else(not_found)?;
            self.repository
                .chat_with_access(activity.chat_id, &access)
                .map_err(ai_failure)?
                .ok_or_else(not_found)?;
            activity.live = self.agent_live.get(&activity.id).cloned();
            Ok(ToolDecisionResponse { activity })
        })();
        let _ = result.reply.send(answer);
    }
}
