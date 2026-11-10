// SPDX-License-Identifier: MPL-2.0
//! Shared steering acknowledgment semantics, with an explicit live-only retry binding.
use super::*;
use koru_ai_wire::{SteerAttemptRequest, SteerAttemptResponse};

const MAX_LIVE_STEERS: usize = 128;
pub(in crate::worker) struct SteerBinding {
    attempt: InferenceAttemptId,
    chat: ChatId,
    fence: BotExecutionFence,
    digest: blake3::Hash,
    busy: bool,
}
pub(in crate::worker) struct SteerJob {
    context: CommandContext,
    workspace: WorkspaceId,
    attempt: InferenceAttemptId,
    chat: ChatId,
    fence: BotExecutionFence,
    request: SteerAttemptRequest,
    input: PendingInput,
    port: Arc<dyn BotDelegatePort>,
    reply: SyncSender<ApiResult<SteerAttemptResponse>>,
}
pub(in crate::worker) struct SteerResult {
    job: SteerJob,
    outcome: Result<BotReply, koru_delegate_api::Failure>,
}
impl SteerJob {
    pub(in crate::worker) fn run(self) -> SteerResult {
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            self.port.execute(DelegateCommand::Steer {
                fence: self.fence.clone(),
                input: self.input.clone(),
            })
        }))
        .unwrap_or_else(|_| {
            Err(koru_delegate_api::Failure {
                code: "bot_unavailable".into(),
                message: "Bot steering unavailable.".into(),
                retryable: true,
                details: Default::default(),
            })
        });
        SteerResult { job: self, outcome }
    }
}
pub(in crate::worker) struct SteerReadJob {
    result: SteerResult,
    authority: Arc<dyn BotAuthority>,
}
pub(in crate::worker) struct SteerReadResult {
    result: SteerResult,
    authority: ApiResult<Arc<dyn BotAuthority>>,
}
impl SteerReadJob {
    pub(in crate::worker) fn run(self) -> SteerReadResult {
        let authority = catch_unwind(AssertUnwindSafe(|| {
            self.authority
                .inspect(&[self.result.job.workspace], &[])
                .map(|proof| Arc::new(proof) as Arc<dyn BotAuthority>)
                .map_err(bot_failure)
        }))
        .unwrap_or_else(|_| Err(bot_failure(BotError::Unavailable)));
        SteerReadResult {
            result: self.result,
            authority,
        }
    }
}
fn uncertain() -> Failure {
    failure(
        "steer_failed",
        "This message's delivery cannot be confirmed for the current reply. Review the conversation before sending a new message.",
        false,
    )
}
fn conflict() -> Failure {
    failure(
        "idempotency_conflict",
        "This message identity is already bound to another input or reply.",
        false,
    )
}

impl WorkerState {
    pub(in crate::worker) fn require_bot_steering_settled(&self, chat: ChatId) -> ApiResult<()> {
        if self
            .bot_steers
            .values()
            .any(|binding| binding.chat == chat && binding.busy)
        {
            return Err(failure(
                "chat_busy",
                "Wait for the in-flight message acknowledgment before starting another task.",
                true,
            ));
        }
        Ok(())
    }

    pub(in crate::worker) fn handle_bot_steering(
        &mut self,
        command: WorkerCommand,
    ) -> Option<WorkerCommand> {
        if !matches!(&command, WorkerCommand::SteerAttempt { .. }) {
            return Some(command);
        }
        let bot = super::super::command_chat(&self.repository, &command).and_then(|chat| {
            chat.map(|id| self.repository.bot_for_chat(id).map_err(bot_failure))
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
        let WorkerCommand::SteerAttempt {
            context,
            attempt,
            request,
            reply,
        } = command
        else {
            unreachable!()
        };
        self.begin_bot_steer(context, attempt, request, reply);
        None
    }

    pub(in crate::worker) fn prune_bot_steers(&mut self) {
        self.bot_steers
            .retain(|_, binding| binding.busy || self.bot_chats.contains_key(&binding.attempt));
    }

    fn begin_bot_steer(
        &mut self,
        context: CommandContext,
        attempt_id: InferenceAttemptId,
        request: SteerAttemptRequest,
        reply: SyncSender<ApiResult<SteerAttemptResponse>>,
    ) {
        self.prune_bot_steers();
        let prepared = (|| -> ApiResult<Option<SteerJob>> {
            authorize(&context, AiScope::Chat)?;
            if request.markdown.trim().is_empty() {
                return Err(failure("invalid_request", "A message is required.", false));
            }
            super::preparation::encoded_size(&request).map_err(ai_failure)?;
            let digest = blake3::hash(request.markdown.as_bytes());
            let access = self.bot_read_access(&context)?;
            let attempt = self
                .repository
                .attempt(attempt_id)
                .map_err(ai_failure)?
                .ok_or_else(not_found)?;
            let chat_id = attempt.chat_id.ok_or_else(not_found)?;
            self.repository
                .chat_with_access(chat_id, &access)
                .map_err(ai_failure)?
                .ok_or_else(not_found)?;
            let id = self
                .repository
                .bot_for_chat(chat_id)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            let bot = self
                .repository
                .bot(id)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            let saved = self
                .repository
                .message(MessageId::from_uuid(request.operation_id))
                .map_err(ai_failure)?;
            if let Some(binding) = self.bot_steers.get(&request.operation_id) {
                if binding.attempt != attempt_id || binding.digest != digest {
                    return Err(conflict());
                }
                if binding.busy {
                    return Err(failure(
                        "idempotency_conflict",
                        "This message is already being sent.",
                        true,
                    ));
                }
                if let Some(saved) = saved {
                    if self
                        .repository
                        .bot_replay_state(attempt_id)
                        .map_err(ai_failure)?
                        .fence
                        .as_ref()
                        != Some(&binding.fence)
                    {
                        return Err(uncertain());
                    }
                    if saved.chat_id != chat_id
                        || saved.role != ChatRole::User
                        || saved.state != MessageState::Complete
                        || saved.markdown != request.markdown
                    {
                        return Err(conflict());
                    }
                    let _ = reply.send(Ok(SteerAttemptResponse {
                        attempt_id,
                        user_message_id: saved.id,
                        replayed: true,
                    }));
                    return Ok(None);
                }
            } else if saved.is_some() {
                return Err(uncertain());
            }
            // Different callers share the same conversational ordering. A
            // second acknowledgment must not overtake the first history row.
            self.require_bot_steering_settled(chat_id)?;
            if attempt.status.is_terminal()
                || !koru_ai_provider_agent::agent_kind_of(&attempt.provider)
                    .is_some_and(|kind| koru_ai_provider_agent::steers_natively(&kind))
            {
                return Err(uncertain());
            }
            let fence = self
                .bot_chats
                .get(&attempt_id)
                .and_then(LiveChat::control_fence)
                .ok_or_else(uncertain)?;
            let replay = self
                .repository
                .bot_replay_state(attempt_id)
                .map_err(ai_failure)?;
            if replay.stop_reason.is_some() || replay.fence.as_ref() != Some(&fence) {
                return Err(uncertain());
            }
            if let Some(binding) = self.bot_steers.get(&request.operation_id) {
                if binding.fence != fence {
                    return Err(uncertain());
                }
            } else if self.bot_steers.len() >= MAX_LIVE_STEERS {
                return Err(failure(
                    "ai_worker_saturated",
                    "This reply has reached its steering limit. Wait for it to finish before sending another message.",
                    true,
                ));
            }
            let input = PendingInput {
                idempotency_key: request
                    .operation_id
                    .to_string()
                    .parse()
                    .map_err(|_| conflict())?,
                text: koru_ai_core::render_prompt_text(&request.markdown),
                attachment_ids: vec![],
                references: vec![],
                mode: None,
                fast: None,
                reasoning: None,
                skills: vec![],
                model: None,
            };
            super::preparation::encoded_size(&input).map_err(ai_failure)?;
            Ok(Some(SteerJob {
                context: context.clone(),
                workspace: bot.home_workspace,
                attempt: attempt_id,
                chat: chat_id,
                fence,
                request: request.clone(),
                input,
                port: self.bot_delegate.clone().ok_or_else(uncertain)?,
                reply: reply.clone(),
            }))
        })();
        match prepared {
            Ok(Some(job)) => {
                let binding = SteerBinding {
                    attempt: attempt_id,
                    chat: job.chat,
                    fence: job.fence.clone(),
                    digest: blake3::hash(request.markdown.as_bytes()),
                    busy: true,
                };
                match self
                    .provider_admin
                    .as_ref()
                    .ok_or_else(unavailable_failure)
                    .and_then(|pool| {
                        pool.try_submit(ProviderAdminJob::BotSteer(job))
                            .map_err(|error| error.failure)
                    }) {
                    Ok(()) => {
                        self.bot_steers.insert(request.operation_id, binding);
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    pub(in crate::worker) fn complete_bot_steer(&mut self, result: SteerResult) {
        let operation = result.job.request.operation_id;
        let reply = result.job.reply.clone();
        let accepted = matches!(&result.outcome,Ok(BotReply::Turn {turn}) if Some(*turn)==result.job.fence.turn);
        let outcome = if accepted {
            self.bot_authority
                .clone()
                .ok_or_else(unavailable_failure)
                .and_then(|authority| {
                    self.provider_admin
                        .as_ref()
                        .ok_or_else(unavailable_failure)?
                        .try_submit(ProviderAdminJob::BotSteerRead(SteerReadJob {
                            result,
                            authority,
                        }))
                        .map_err(|error| error.failure)
                })
        } else {
            Err(match result.outcome {
                Err(_) => failure(
                    "steer_failed",
                    "The message's delivery was not confirmed. Retry only while this reply remains active.",
                    false,
                ),
                _ => uncertain(),
            })
        };
        if outcome.is_err() {
            if let Some(binding) = self.bot_steers.get_mut(&operation) {
                binding.busy = false;
            }
            let _ = reply.send(Err(uncertain()));
        }
    }

    pub(in crate::worker) fn complete_bot_steer_read(&mut self, checked: SteerReadResult) {
        let job = checked.result.job;
        let outcome = (|| {
            authorize(&job.context, AiScope::Chat)?;
            let binding = self
                .bot_steers
                .get(&job.request.operation_id)
                .ok_or_else(uncertain)?;
            if binding.attempt != job.attempt
                || binding.fence != job.fence
                || binding.digest != blake3::hash(job.request.markdown.as_bytes())
            {
                return Err(conflict());
            }
            let old = job.context.prepared.as_ref().ok_or_else(not_found)?;
            let context = CommandContext {
                request: job.context.request.clone(),
                prepared: Some(Arc::new(PreparedBotCommand {
                    authority: checked.authority?,
                    chats: old.chats.clone(),
                    runtime: Ok(()),
                    pins: vec![],
                    chat_snapshot: None,
                    settings: None,
                    terminal_question: None,
                })),
            };
            let access = self.bot_read_access(&context)?;
            self.repository
                .chat_with_access(job.chat, &access)
                .map_err(ai_failure)?
                .ok_or_else(not_found)?;
            let attempt = self
                .repository
                .attempt(job.attempt)
                .map_err(ai_failure)?
                .ok_or_else(not_found)?;
            if attempt.chat_id != Some(job.chat)
                || self
                    .repository
                    .bot_replay_state(job.attempt)
                    .map_err(ai_failure)?
                    .fence
                    .as_ref()
                    != Some(&job.fence)
            {
                return Err(uncertain());
            }
            let now = Timestamp::now();
            let message = Message {
                id: MessageId::from_uuid(job.request.operation_id),
                chat_id: job.chat,
                ordinal: 0,
                role: ChatRole::User,
                state: MessageState::Complete,
                markdown: job.request.markdown,
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: now,
                completed_at: Some(now),
            };
            if let Some(old) = self.repository.message(message.id).map_err(ai_failure)? {
                if old.chat_id != message.chat_id
                    || old.markdown != message.markdown
                    || old.role != message.role
                {
                    return Err(conflict());
                }
            }
            let stored = self
                .repository
                .append_user_message(&message)
                .map_err(ai_failure)?;
            Ok(SteerAttemptResponse {
                attempt_id: job.attempt,
                user_message_id: stored.id,
                replayed: false,
            })
        })();
        if let Some(binding) = self.bot_steers.get_mut(&job.request.operation_id) {
            binding.busy = false;
        }
        self.prune_bot_steers();
        let _ = job.reply.send(outcome.map_err(|_| uncertain()));
    }
}
