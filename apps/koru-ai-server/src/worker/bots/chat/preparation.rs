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
    if original_user.is_some_and(|user| {
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
) -> ApiResult<Option<&'a Message>> {
    let by_id: HashMap<_, _> = messages
        .iter()
        .map(|message| (message.id, message))
        .collect();
    let mut origin = failed;
    while let Some(id) = origin.origin_message_id {
        let Some(previous) = by_id.get(&id).copied() else {
            break;
        };
        if previous.role != ChatRole::Assistant || previous.ordinal >= origin.ordinal {
            return Err(invariant_failure());
        }
        origin = previous;
    }
    // begin_turn creates adjacent User/Assistant rows. Assistant-only answer
    // tasks have no user bubble; their immutable receipt remains the authority.
    Ok(messages.iter().find(|message| {
        message.role == ChatRole::User
            && message.state == MessageState::Complete
            && message.ordinal.checked_add(1) == Some(origin.ordinal)
            && message.markdown == task
    }))
}

pub(in crate::worker) struct ChatCapture {
    pub model: BotConfig,
    pub owner: BotOwner,
    pub destination: WorkspaceId,
    pub pages: Vec<BotPage>,
    pub chat: Chat,
}

pub(in crate::worker) fn chat_capture(
    repository: &Repository,
    command: &WorkerCommand,
) -> ApiResult<Option<ChatCapture>> {
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
    let bot = repository
        .bot(id)
        .map_err(bot_failure)?
        .ok_or_else(not_found)?;
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
    let mut pages = vec![bot.config.role.clone()];
    for reference in refs {
        if let ContextRef::Document { document_id } = reference {
            let page = BotPage {
                workspace: destination,
                document: *document_id,
            };
            if !pages.contains(&page) {
                pages.push(page);
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
                if !pages.contains(&page) {
                    pages.push(page);
                }
            }
        }
    }
    if pages.len() > 17 {
        return Err(bot_failure(BotError::Invalid));
    }
    Ok(Some(ChatCapture {
        model,
        owner: BotOwner {
            user_scope: bot.user_scope,
            device: bot.owner_device,
        },
        destination,
        pages,
        chat,
    }))
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
        let documents = proof
            .pins
            .iter()
            .filter_map(|pin| match pin {
                koru_delegate_core::RunContextItem::Document {
                    workspace,
                    document,
                    ..
                } => Some(BotPage {
                    workspace: *workspace,
                    document: *document,
                }),
                _ => None,
            })
            .filter(|page| page != &bot.config.role)
            .collect();
        let task = BotTask { text, documents };
        task.validate(destination).map_err(bot_failure)?;
        let request_id = BotRequestId::mint();
        let mut intent = super::super::admission::make_intent(
            &bot,
            request_id,
            chat.id,
            destination,
            &task,
            previous,
            &proof.pins,
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
        self.repository
            .prepare_bot_chat(
                &preparation,
                &intent,
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
