// SPDX-License-Identifier: MPL-2.0
use super::*;
use serde::Serialize;

#[cfg(test)]
mod tests;

/// Reuse the selected task when it is already the final complete input.
/// Earlier exchanges retain their order; a later unrelated user is never the
/// retry's task, so that case appends the rendered immutable task explicitly.
pub(in crate::worker) fn retry_input(
    messages: &[Message],
    failed: &Message,
    task: &str,
    history: &mut Vec<AiRequestMessage>,
) -> ApiResult<MessageId> {
    let original_user = original_task_user(messages, failed, task)?;
    if original_user.as_ref().is_some_and(|user| {
        messages
            .iter()
            .rev()
            .find(|message| message.state == MessageState::Complete)
            .is_some_and(|last| last.id == user.id)
    }) {
        let last = history.last_mut().ok_or_else(invariant_failure)?;
        last.markdown = koru_ai_core::render_prompt_text(task);
        return Ok(original_user.expect("matched original task").id);
    }
    history.push(AiRequestMessage {
        role: ChatRole::User,
        markdown: koru_ai_core::render_prompt_text(task),
        attachments: Vec::new(),
    });
    Ok(original_user.map_or(failed.id, |message| message.id))
}

pub(in crate::worker) fn retry_anchor(
    messages: &[Message],
    failed: &Message,
    task: &str,
) -> ApiResult<MessageId> {
    Ok(original_task_user(messages, failed, task)?.map_or(failed.id, |message| message.id))
}

fn original_task_user<'a>(
    messages: &'a [Message],
    failed: &'a Message,
    task: &str,
) -> ApiResult<Option<Message>> {
    let by_id: HashMap<_, _> = messages
        .iter()
        .map(|message| (message.id, message))
        .collect();
    koru_ai_core::bots::replay::original_bot_task_user(
        failed,
        task,
        |id| Ok(by_id.get(&id).map(|message| (*message).clone())),
        |chat, ordinal| {
            Ok(messages
                .iter()
                .find(|message| message.chat_id == chat && message.ordinal == ordinal)
                .cloned())
        },
    )
    .map_err(ai_failure)
}

pub(in crate::worker) struct ChatCapture {
    pub model: BotConfig,
    pub owner: BotOwner,
    pub destination: WorkspaceId,
    pub pages: Vec<BotPage>,
    pub library_sources: super::super::library::LibrarySources,
    pub chat: Chat,
}

pub(in crate::worker) fn chat_capture(
    repository: &Repository,
    command: &WorkerCommand,
) -> ApiResult<Option<ChatCapture>> {
    require_interactive_chat_action(repository, command)?;
    let (chat_id, destination, refs, replace, reasoning, operation) = match command {
        WorkerCommand::SendTurn { request, .. } => (
            request.chat_id,
            request.destination_workspace,
            &request.explicit_refs,
            request.replace_pinned_documents,
            request.reasoning_override,
            request.operation_id,
        ),
        WorkerCommand::RetryTurn { chat, request, .. } => (
            *chat,
            request.destination_workspace,
            &request.explicit_refs,
            request.replace_pinned_documents,
            request.reasoning_override,
            request.operation_id,
        ),
        _ => return Ok(None),
    };
    let Some(id) = repository.bot_for_chat(chat_id).map_err(bot_failure)? else {
        return Ok(None);
    };
    let bot = match repository.bot(id) {
        Ok(Some(bot)) => bot,
        // An archived or trashed Bot is answered by the guard, which never
        // admits a change to it; nothing is captured for it here (P1.7).
        Ok(None) | Err(BotError::Forbidden) => return Ok(None),
        Err(error) => return Err(bot_failure(error)),
    };
    let destination = destination.ok_or_else(|| {
        failure(
            "invalid_request",
            "Choose a destination workspace for this Bot task.",
            false,
        )
    })?;
    if destination != bot.home_workspace {
        return Err(bot_failure(BotError::Forbidden));
    }
    // Exact replay still gets current owner inspection, but never recaptures a
    // different Role or depends on a currently installed provider/model.
    if repository
        .attempt_by_operation(operation)
        .map_err(ai_failure)?
        .is_some()
    {
        return Ok(None);
    }
    let chat = repository
        .chat_with_access(chat_id, &ChatReadAccess::authorized(vec![chat_id]))
        .map_err(ai_failure)?
        .ok_or_else(not_found)?;
    if chat.current_model.provider != bot.config.model.provider {
        return Err(failure(
            "unsupported_capability",
            "Changing this Bot's runtime requires model handoff.",
            false,
        ));
    }
    let mut model = bot.config.clone();
    model.model = chat.current_model.clone();
    if let Some(reasoning) = reasoning {
        model.model.default_reasoning = reasoning;
    }
    model.mode = chat.agent_mode.clone();
    model.fast = chat.agent_fast;
    let mut task_documents = Vec::new();
    for reference in refs {
        if let ContextRef::Document { document_id } = reference {
            let page = BotPage {
                workspace: destination,
                document: *document_id,
            };
            if !task_documents.contains(&page) {
                task_documents.push(page);
            }
        }
    }
    if !replace {
        for pin in repository.pinned_documents(chat_id).map_err(ai_failure)? {
            if pin.pinned {
                let page = BotPage {
                    workspace: destination,
                    document: pin.document_id,
                };
                if !task_documents.contains(&page) {
                    task_documents.push(page);
                }
            }
        }
    }
    if task_documents.len() > 16 {
        return Err(bot_failure(BotError::Invalid));
    }
    let library_sources =
        super::super::library::LibrarySources::read(repository, &bot, task_documents)?;
    let pages = library_sources.pages();
    Ok(Some(ChatCapture {
        model,
        owner: BotOwner {
            user_scope: bot.user_scope,
            device: bot.owner_device,
        },
        destination,
        pages,
        library_sources,
        chat,
    }))
}

/// Preflight before Library reads, capture jobs or fresh AI/Run identities.
/// A lost response still takes the immutable operation replay path first.
pub(in crate::worker) fn require_interactive_chat_action(
    repository: &Repository,
    command: &WorkerCommand,
) -> ApiResult<()> {
    let (operation, chat, prior) = match command {
        WorkerCommand::RetryTurn { chat, request, .. } => {
            (request.operation_id, *chat, Some(request.message_id))
        }
        WorkerCommand::SendTurn { request, .. } if request.answers.is_some() => {
            (request.operation_id, request.chat_id, None)
        }
        _ => return Ok(()),
    };
    if repository
        .attempt_by_operation(operation)
        .map_err(ai_failure)?
        .is_some()
    {
        return Ok(());
    }
    let attempt = if let Some(message) = prior {
        repository
            .attempt_for_message(message)
            .map_err(ai_failure)?
    } else if let WorkerCommand::SendTurn { request, .. } = command {
        let row = repository
            .tool_activity(request.answers.as_ref().unwrap().invocation)
            .map_err(ai_failure)?;
        let Some(row) = row.filter(|row| row.chat_id == chat) else {
            return Ok(());
        };
        repository
            .attempt(row.attempt_id)
            .map_err(ai_failure)?
            .filter(|attempt| attempt.status.is_terminal())
    } else {
        None
    };
    if let Some(attempt) = attempt.filter(|attempt| attempt.chat_id == Some(chat)) {
        require_interactive_predecessor(repository, attempt.id)?;
    }
    Ok(())
}

pub(in crate::worker) fn require_interactive_predecessor(
    repository: &Repository,
    attempt: InferenceAttemptId,
) -> ApiResult<()> {
    if let Some(receipt) = repository
        .bot_submission_for_attempt(attempt)
        .map_err(ai_failure)?
    {
        receipt
            .intent
            .require_interactive_continuation()
            .map_err(bot_failure)?;
    }
    Ok(())
}

fn fingerprint(kind: &str, request: &impl Serialize) -> ApiResult<String> {
    let mut writer = BoundedHash {
        count: 0,
        hash: blake3::Hasher::new(),
    };
    serde_json::to_writer(&mut writer, &(kind, request))
        .map_err(|_| bot_failure(BotError::Invalid))?;
    Ok(writer.hash.finalize().to_hex().to_string())
}
struct BoundedHash {
    count: usize,
    hash: blake3::Hasher,
}
impl std::io::Write for BoundedHash {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.count = self
            .count
            .checked_add(bytes.len())
            .filter(|count| *count <= MAX_REPLAY_BYTES)
            .ok_or_else(|| std::io::Error::other("Bot payload exceeds bound"))?;
        self.hash.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(super) fn encoded_size(value: &impl Serialize) -> Result<usize, AiError> {
    let mut writer = BoundedHash {
        count: 0,
        hash: blake3::Hasher::new(),
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| context_too_large())?;
    Ok(writer.count)
}

impl WorkerState {
    pub(in crate::worker) fn check_bot_chat_retry(
        &self,
        attempt: &InferenceAttempt,
        destination: Option<WorkspaceId>,
        kind: &str,
        request: &impl Serialize,
    ) -> ApiResult<()> {
        if let Some(receipt) = self
            .repository
            .bot_submission_for_attempt(attempt.id)
            .map_err(ai_failure)?
        {
            if destination.map(|w| w.as_uuid())
                != Some(receipt.intent.submission.policy.destination.as_uuid())
                || receipt.fingerprint != fingerprint(kind, request)?
            {
                return Err(bot_failure(BotError::Conflict));
            }
        } else if let Some(chat) = attempt.chat_id {
            if self
                .repository
                .bot_for_chat(chat)
                .map_err(bot_failure)?
                .is_some()
            {
                return Err(failure(
                    "unsupported_capability",
                    "This Bot response has no execution association.",
                    false,
                ));
            }
        }
        Ok(())
    }

    pub(in crate::worker) fn prepare_bot_chat_turn(
        &mut self,
        context: &CommandContext,
        chat: &Chat,
        preparation: BotChatPreparation,
        destination: Option<WorkspaceId>,
        kind: &str,
        request: &impl Serialize,
        skills: &[String],
        previous: Option<koru_delegate_core::RunId>,
    ) -> ApiResult<bool> {
        let Some(id) = self.repository.bot_for_chat(chat.id).map_err(bot_failure)? else {
            return Ok(false);
        };
        let mut bot = self
            .repository
            .bot(id)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        let destination = destination.ok_or_else(|| bot_failure(BotError::Invalid))?;
        if destination != bot.home_workspace {
            return Err(bot_failure(BotError::Forbidden));
        }
        if self.bot_delegate.is_none() {
            return Err(bot_failure(BotError::Unavailable));
        }
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        validate_owner(&proof.authority, &bot)?;
        // read_through associates a standalone steer with the preceding reply.
        // A newer task must not overtake its acknowledged history insert.
        self.require_bot_steering_settled(chat.id)?;
        proof.runtime.clone()?;
        let Some(snapshot) = &proof.chat_snapshot else {
            return Err(bot_failure(BotError::Unavailable));
        };
        if snapshot.id != chat.id
            || snapshot.current_model != chat.current_model
            || snapshot.agent_mode != chat.agent_mode
            || snapshot.agent_fast != chat.agent_fast
        {
            return Err(bot_failure(BotError::RevisionConflict));
        }
        let attempt = preparation.attempt();
        if self
            .repository
            .attempt_by_operation(attempt.operation_id)
            .map_err(ai_failure)?
            .is_none()
        {
            let prior = match &preparation {
                BotChatPreparation::Turn(_) => None,
                BotChatPreparation::Retry { prior_attempt, .. } => Some(*prior_attempt),
                BotChatPreparation::AnsweredContinuation { question, .. } => {
                    Some(question.prior_attempt)
                }
            };
            if let Some(prior) = prior {
                require_interactive_predecessor(&self.repository, prior)?;
            }
        }
        bot.config.model = chat.current_model.clone();
        bot.config.model.default_reasoning = attempt.requested_reasoning;
        bot.config.mode = chat.agent_mode.clone();
        bot.config.fast = chat.agent_fast;
        let text = match &preparation {
            BotChatPreparation::Turn(turn) => turn.user_message.markdown.clone(),
            BotChatPreparation::Retry { prior_attempt, .. } => {
                self.repository
                    .bot_submission_for_attempt(*prior_attempt)
                    .map_err(ai_failure)?
                    .ok_or_else(not_found)?
                    .intent
                    .submission
                    .input
                    .text
            }
            BotChatPreparation::AnsweredContinuation { question, .. } => question
                .answers
                .encode_lossless()
                .ok_or_else(|| bot_failure(BotError::Invalid))?,
        };
        let sources = proof
            .library_sources
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        let documents = sources.task_documents.clone();
        let task = BotTask { text, documents };
        task.validate(destination).map_err(bot_failure)?;
        let request_id = BotRequestId::mint();
        let mut intent = super::super::admission::make_library_intent(
            &bot,
            request_id,
            chat.id,
            destination,
            &task,
            previous,
            &proof.pins,
            sources,
            proof
                .authority
                .workspace(destination)
                .map_err(bot_failure)?,
        )?;
        intent.submission.provenance.ai_attempt_id = Some(attempt.id.as_uuid());
        intent.submission.provenance.assistant_message_id =
            Some(preparation.assistant().id.as_uuid());
        intent.submission.input.skills = skills.to_vec();
        intent.validate().map_err(bot_failure)?;
        let capture = sources.capture(&intent)?;
        self.repository
            .prepare_bot_chat_with_library(
                &preparation,
                &intent,
                &capture,
                &fingerprint(kind, request)?,
                bot.revision,
                Timestamp::now(),
            )
            .map_err(|error| match error {
                BotPreparationError::Admission(error) => bot_failure(error),
                BotPreparationError::Persistence(error) => ai_failure(error),
            })?;
        Ok(true)
    }
}
