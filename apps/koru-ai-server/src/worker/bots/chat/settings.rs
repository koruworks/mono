// SPDX-License-Identifier: MPL-2.0
//! Per-conversation settings for the next task; the current Run retains its capture.
use super::*;
use koru_ai_core::ModelSelection;

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub(in crate::worker) struct SettingsProof {
    pub model: ModelSelection,
    pub mode: Option<String>,
    pub fast: Option<bool>,
    pub reasoning_control: Option<ReasoningControl>,
}
impl SettingsProof {
    pub(in crate::worker) fn new(
        config: &BotConfig,
        reasoning_control: Option<ReasoningControl>,
    ) -> Self {
        Self {
            model: config.model.clone(),
            mode: config.mode.clone(),
            fast: config.fast,
            reasoning_control,
        }
    }
    fn matches(&self, config: &BotConfig) -> bool {
        self.model == config.model && self.mode == config.mode && self.fast == config.fast
    }
}

pub(in crate::worker) fn settings_capture(
    repository: &Repository,
    command: &WorkerCommand,
) -> ApiResult<Option<(Chat, BotConfig)>> {
    let chat_id = match command {
        WorkerCommand::UpdateChatModel { chat, .. }
        | WorkerCommand::UpdateChatAgentSettings { chat, .. } => *chat,
        _ => return Ok(None),
    };
    let Some(id) = repository.bot_for_chat(chat_id).map_err(bot_failure)? else {
        return Ok(None);
    };
    let bot = repository
        .bot(id)
        .map_err(bot_failure)?
        .ok_or_else(not_found)?;
    let chat = repository
        .chat_with_access(chat_id, &ChatReadAccess::authorized(vec![chat_id]))
        .map_err(ai_failure)?
        .ok_or_else(not_found)?;
    if chat.current_model.provider != bot.config.model.provider {
        return Err(failure(
            "unsupported_capability",
            "The conversation runtime no longer matches its Bot.",
            false,
        ));
    }
    let mut config = chat_config(&bot, &chat);
    match command {
        WorkerCommand::UpdateChatModel { request, .. } => {
            apply_model(&mut config, &chat, request)?;
        }
        WorkerCommand::UpdateChatAgentSettings { request, .. } => {
            apply_agent(&mut config, request);
        }
        _ => unreachable!(),
    }
    Ok(Some((chat, config)))
}
fn chat_config(bot: &Bot, chat: &Chat) -> BotConfig {
    let mut config = bot.config.clone();
    config.model = chat.current_model.clone();
    config.mode = chat.agent_mode.clone();
    config.fast = chat.agent_fast;
    config
}
fn apply_model(
    config: &mut BotConfig,
    chat: &Chat,
    request: &UpdateChatModelRequest,
) -> ApiResult<()> {
    if request.model.provider != chat.current_model.provider
        || request.model.provider != config.model.provider
    {
        return Err(failure(
            "unsupported_capability",
            "Changing a Bot conversation to another runtime requires model handoff.",
            false,
        ));
    }
    config.model = request.model.clone();
    config.model.requested_model = config.model.requested_model.trim().to_owned();
    Ok(())
}
fn apply_agent(config: &mut BotConfig, request: &koru_ai_wire::UpdateChatAgentSettingsRequest) {
    if let Some(mode) = &request.mode {
        config.mode = Some(mode.clone());
    }
    if let Some(fast) = request.fast {
        config.fast = Some(fast);
    }
}

impl WorkerState {
    fn checked_bot_settings(
        &self,
        context: &CommandContext,
        chat_id: ChatId,
    ) -> ApiResult<(Chat, BotConfig, SettingsProof)> {
        let access = self.bot_read_access(context)?;
        let chat = self
            .repository
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
        let prepared = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        prepared.runtime.clone()?;
        let captured = prepared
            .chat_snapshot
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        if captured.id != chat.id
            || captured.current_model != chat.current_model
            || captured.agent_mode != chat.agent_mode
            || captured.agent_fast != chat.agent_fast
        {
            return Err(bot_failure(BotError::RevisionConflict));
        }
        if chat.current_model.provider != bot.config.model.provider {
            return Err(failure(
                "unsupported_capability",
                "The conversation runtime no longer matches its Bot.",
                false,
            ));
        }
        let proof = prepared
            .settings
            .clone()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        let config = chat_config(&bot, &chat);
        Ok((chat, config, proof))
    }
    pub(in crate::worker) fn update_bot_chat_model(
        &mut self,
        context: &CommandContext,
        chat_id: ChatId,
        request: &UpdateChatModelRequest,
    ) -> ApiResult<UpdateChatModelResponse> {
        let (chat, mut config, proof) = self.checked_bot_settings(context, chat_id)?;
        apply_model(&mut config, &chat, request)?;
        if !proof.matches(&config) {
            return Err(bot_failure(BotError::RevisionConflict));
        }
        if !self
            .repository
            .update_chat_model(chat_id, &config.model)
            .map_err(ai_failure)?
        {
            return Err(not_found());
        }
        let chat = self
            .repository
            .chat_with_access(chat_id, &self.bot_read_access(context)?)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        Ok(self.chat_model_response(chat, Some(proof.reasoning_control)))
    }
    pub(in crate::worker) fn update_bot_chat_agent_settings(
        &mut self,
        context: &CommandContext,
        chat_id: ChatId,
        request: &koru_ai_wire::UpdateChatAgentSettingsRequest,
    ) -> ApiResult<koru_ai_wire::UpdateChatAgentSettingsResponse> {
        let (_, mut config, proof) = self.checked_bot_settings(context, chat_id)?;
        apply_agent(&mut config, request);
        if !proof.matches(&config) {
            return Err(bot_failure(BotError::RevisionConflict));
        }
        let access = self.bot_read_access(context)?;
        let chat = self
            .repository
            .patch_chat_agent_settings(chat_id, request.mode.as_deref(), request.fast, &access)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        Ok(koru_ai_wire::UpdateChatAgentSettingsResponse { chat })
    }
}
