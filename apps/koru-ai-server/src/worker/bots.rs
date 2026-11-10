// SPDX-License-Identifier: MPL-2.0

//! Bot commands share the AI writer, but not ordinary chat launch authority.
use super::removed::Named;
use super::*;
use koru_ai_core::bots::*;

pub(super) fn bot_failure(error: BotError) -> Failure {
    failure(
        error.code(),
        "The Bot operation could not be completed.",
        matches!(error, BotError::Unavailable | BotError::Storage),
    )
}

mod admission;
pub(in crate::worker) mod chat;
pub(crate) mod policy;
mod preparation;
pub(super) use admission::{BotCoordinationJob, BotCoordinationResult};
pub(super) use policy::{BotPolicyJob, BotPolicyResult};
pub(super) use preparation::{BotPreparationJob, CommandContext, PreparedBotCommand};
impl WorkerState {
    pub(super) fn bot_read_access(&self, context: &CommandContext) -> ApiResult<ChatReadAccess> {
        if !context.principal.scopes.contains(&AiScope::ManageBots) {
            return Ok(ChatReadAccess::ordinary());
        }
        let Some(proof) = &context.prepared else {
            return Ok(ChatReadAccess::ordinary());
        };
        let mut allowed = Vec::new();
        for (id, chat, revision, policy) in &proof.chats {
            if let Ok(Some(bot)) = self.repository.bot(*id)
                && bot.revision == *revision
                && bot.policy_revision == *policy
                && bot.main_chat == *chat
                && validate_owner(&proof.authority, &bot).is_ok()
            {
                allowed.push(*chat);
            }
        }
        Ok(ChatReadAccess::authorized(allowed))
    }
    pub(super) fn bots(
        &mut self,
        context: CommandContext,
        request: BotCommand,
    ) -> ApiResult<BotResponse> {
        if let Some(result) = self.bot_policy_command(&context, &request) {
            return result;
        }
        if let Some(result) = self.bot_admission_command(&context, &request) {
            return result;
        }
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        let validation = proof.runtime.clone();
        execute_bot_command(
            &mut self.repository,
            proof.authority.clone(),
            move |_| validation.clone(),
            context.request,
            request,
        )
    }
    pub(super) fn guard_bot_command(&self, command: WorkerCommand) -> Option<WorkerCommand> {
        let authority = command
            .bot_context()
            .and_then(|context| context.prepared.as_ref())
            .map(|proof| proof.authority.clone());
        guard_command(&self.repository, authority.as_ref(), command)
    }
}

fn next_revision(revision: u64) -> ApiResult<u64> {
    if revision == 0 || revision >= i64::MAX as u64 {
        return Err(bot_failure(BotError::RevisionConflict));
    }
    Ok(revision + 1)
}

impl WorkerCommand {
    fn bot_read_only(&self) -> bool {
        matches!(
            self,
            Self::ReadChat { .. }
                | Self::ToolReadChat { .. }
                | Self::ReadChatToolPolicy { .. }
                | Self::ReadAttempt { .. }
                | Self::ReadExport { .. }
                | Self::ExportChat { .. }
                | Self::Usage { .. }
                | Self::ResolveAttachments { .. }
        )
    }
}

fn validate_model(agents: Option<&AgentSessions>, config: &BotConfig) -> ApiResult<()> {
    let agent = agents.ok_or_else(|| {
        failure(
            "runtime_unavailable",
            "The selected runtime is unavailable.",
            true,
        )
    })?;
    let catalog = agents::agent_models(agent, config.model.provider.clone(), false)?;
    let model = catalog
        .models
        .iter()
        .find(|model| model.id == config.model.requested_model)
        .ok_or_else(|| {
            failure(
                "runtime_unavailable",
                "The selected model is unavailable.",
                false,
            )
        })?;
    koru_ai_core::validate_reasoning(
        model
            .reasoning
            .as_ref()
            .unwrap_or(&ReasoningControl::Unsupported),
        config.model.default_reasoning,
    )
    .map_err(ai_failure)?;
    if config
        .mode
        .as_ref()
        .is_some_and(|mode| !catalog.modes.iter().any(|m| &m.id == mode))
        || (config.fast == Some(true) && model.fast.is_none())
    {
        return Err(failure(
            "unsupported_capability",
            "The selected runtime settings are unavailable.",
            false,
        ));
    }
    Ok(())
}

fn execute_bot_command(
    repository: &mut impl BotRepository,
    authority: Arc<dyn BotAuthority>,
    validate_runtime: impl Fn(&BotConfig) -> ApiResult<()>,
    context: RequestContext,
    mut request: BotCommand,
) -> ApiResult<BotResponse> {
    authorize(&context, AiScope::ManageBots)?;
    let owner = authority.owner().map_err(bot_failure)?;
    let now = Timestamp::now().millis();
    match &mut request {
        BotCommand::Start { .. }
        | BotCommand::Continue { .. }
        | BotCommand::Pause { .. }
        | BotCommand::Resume { .. }
        | BotCommand::ReadOperation { .. }
        | BotCommand::ReconcileOperation { .. } => return Err(bot_failure(BotError::Unavailable)),
        BotCommand::Create {
            home_workspace,
            config,
            selected_pages,
            ..
        } => {
            validate_config(config, *home_workspace).map_err(bot_failure)?;
            normalize_pages(selected_pages, *home_workspace).map_err(bot_failure)?;
            authority.workspace(*home_workspace).map_err(bot_failure)?;
            authority.page(&config.role).map_err(bot_failure)?;
            for page in selected_pages {
                authority.page(page).map_err(bot_failure)?;
            }
        }
        BotCommand::List { workspace } => {
            authority.workspace(*workspace).map_err(bot_failure)?;
            let bots = repository
                .bots(owner.user_scope, *workspace)
                .map_err(bot_failure)?;
            return Ok(BotResponse::List {
                bots: bots
                    .into_iter()
                    .filter(|bot| bot.owner_device == owner.device)
                    .collect(),
            });
        }
        BotCommand::Read { bot_id } => {
            let bot = repository
                .bot(*bot_id)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            validate_owner(&authority, &bot)?;
            return Ok(BotResponse::Bot { bot });
        }
        BotCommand::Update { bot_id, config, .. } => {
            let bot = repository
                .bot(*bot_id)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            validate_owner(&authority, &bot)?;
            validate_config(config, bot.home_workspace).map_err(bot_failure)?;
            authority.page(&config.role).map_err(bot_failure)?;
        }
        BotCommand::UpdateAccess {
            bot_id,
            selected_pages,
            ..
        } => {
            let bot = repository
                .bot(*bot_id)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            validate_owner(&authority, &bot)?;
            normalize_pages(selected_pages, bot.home_workspace).map_err(bot_failure)?;
            for page in selected_pages {
                authority.page(page).map_err(bot_failure)?;
            }
        }
    }
    let fingerprint =
        blake3::hash(&serde_json::to_vec(&request).map_err(|_| bot_failure(BotError::Invalid))?)
            .to_hex()
            .to_string();
    let request_id = match &request {
        BotCommand::Create { request_id, .. }
        | BotCommand::Update { request_id, .. }
        | BotCommand::UpdateAccess { request_id, .. } => *request_id,
        _ => unreachable!(),
    };
    // Current authority was checked before replaying a retained success.
    if let Some(bot) = repository
        .bot_receipt(owner.user_scope, request_id, &fingerprint)
        .map_err(bot_failure)?
    {
        let current = repository
            .bot(bot.id)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        validate_owner(&authority, &current)?;
        validate_owner(&authority, &bot)?;
        return Ok(BotResponse::Bot { bot });
    }
    let (bot, expected) = match request {
        BotCommand::Create {
            bot_id,
            home_workspace,
            config,
            access,
            selected_pages,
            ..
        } => {
            validate_runtime(&config)?;
            (
                Bot {
                    id: bot_id,
                    home_workspace,
                    user_scope: owner.user_scope,
                    owner_device: owner.device,
                    config,
                    access,
                    selected_pages,
                    revision: 1,
                    policy_revision: 1,
                    main_chat: ChatId::new(),
                    paused: false,
                    folder_lock_request: None,
                    folder_frozen_at_ms: None,
                    created_at_ms: now,
                    updated_at_ms: now,
                },
                None,
            )
        }
        BotCommand::Update {
            bot_id,
            expected_revision,
            config,
            ..
        } => {
            let mut bot = repository
                .bot(bot_id)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            validate_runtime(&config)?;
            bot.config = config;
            bot.revision = next_revision(expected_revision)?;
            bot.updated_at_ms = now;
            (bot, Some(expected_revision))
        }
        BotCommand::UpdateAccess {
            bot_id,
            expected_revision,
            expected_policy_revision,
            access,
            selected_pages,
            ..
        } => {
            let mut bot = repository
                .bot(bot_id)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            if bot.policy_revision != expected_policy_revision {
                return Err(bot_failure(BotError::RevisionConflict));
            }
            bot.access = access;
            bot.selected_pages = selected_pages;
            bot.revision = next_revision(expected_revision)?;
            bot.policy_revision = next_revision(expected_policy_revision)?;
            bot.updated_at_ms = now;
            (bot, Some(expected_revision))
        }
        _ => unreachable!(),
    };
    let profile = authority
        .workspace(bot.home_workspace)
        .map_err(bot_failure)?;
    let bot = repository
        .save_bot(&bot, profile, request_id, &fingerprint, expected)
        .map_err(bot_failure)?;
    Ok(BotResponse::Bot { bot })
}

fn validate_owner(authority: &Arc<dyn BotAuthority>, bot: &Bot) -> ApiResult<()> {
    let owner = authority.owner().map_err(bot_failure)?;
    if owner.user_scope != bot.user_scope || owner.device != bot.owner_device {
        return Err(bot_failure(BotError::Forbidden));
    }
    authority
        .workspace(bot.home_workspace)
        .map_err(bot_failure)?;
    Ok(())
}

#[cfg(test)]
mod tests;

fn guard_command(
    repository: &Repository,
    authority: Option<&Arc<dyn BotAuthority>>,
    command: WorkerCommand,
) -> Option<WorkerCommand> {
    let checked = (|| -> ApiResult<()> {
        let Some(context) = command.bot_context() else {
            return Ok(());
        };
        if let WorkerCommand::BranchChat { request, .. } = &command {
            super::attachments::require_ordinary_branch_destination(
                repository,
                request.branch_chat_id,
            )?;
        }
        let operation = match &command {
            WorkerCommand::SendTurn { request, .. } => {
                Some((request.operation_id, request.chat_id))
            }
            WorkerCommand::RetryTurn { chat, request, .. } => Some((request.operation_id, *chat)),
            _ => None,
        };
        if let Some((operation, chat)) = operation
            && let Some(attempt) = repository
                .attempt_by_operation(operation)
                .map_err(ai_failure)?
            && attempt.chat_id != Some(chat)
        {
            return Err(failure(
                "request_conflict",
                "This operation belongs to another conversation.",
                false,
            ));
        }
        let chat = command_chat(repository, &command)?;
        if let Some(chat) = chat
            && let Some(id) = repository.bot_for_chat(chat).map_err(bot_failure)?
        {
            if !context.principal.scopes.contains(&AiScope::ManageBots) {
                return Err(not_found());
            }
            let bot = repository
                .bot(id)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            validate_owner(
                authority.ok_or_else(|| bot_failure(BotError::Unavailable))?,
                &bot,
            )?;
            let implemented = matches!(
                command,
                WorkerCommand::SendTurn { .. } | WorkerCommand::RetryTurn { .. }
            ) || matches!(&command,WorkerCommand::CancelAttempt {attempt,..}
                    if repository.bot_submission_for_attempt(*attempt).map_err(ai_failure)?.is_some());
            if !command.bot_read_only() && !implemented {
                return Err(failure(
                    "unsupported_capability",
                    "Bot conversation controls are not available yet.",
                    false,
                ));
            }
        }
        Ok(())
    })();
    match checked {
        Ok(()) => Some(command),
        Err(error) => {
            command.refuse(error);
            None
        }
    }
}

fn command_chat(repository: &Repository, command: &WorkerCommand) -> ApiResult<Option<ChatId>> {
    Ok(match command.named() {
        Some(Named::Chat(chat)) | Some(Named::NewChat { chat, .. }) => Some(chat),
        Some(Named::Attempt(id)) => repository
            .attempt(id)
            .map_err(ai_failure)?
            .and_then(|v| v.chat_id),
        Some(Named::Message(id)) => repository
            .message(id)
            .map_err(ai_failure)?
            .map(|v| v.chat_id),
        Some(Named::Tool(id)) => repository
            .tool_activity(id)
            .map_err(ai_failure)?
            .map(|v| v.chat_id),
        Some(Named::Export(id)) => repository
            .export(id)
            .map_err(ai_failure)?
            .map(|v| v.chat_id),
        None => None,
    })
}
