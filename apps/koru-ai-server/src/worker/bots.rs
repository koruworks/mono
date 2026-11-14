// SPDX-License-Identifier: MPL-2.0

//! Bot commands share the AI writer, but not ordinary chat launch authority.
use super::removed::Named;
use super::*;
use koru_ai_core::bots::*;

pub(super) fn bot_failure(error: BotError) -> Failure {
    failure(
        error.code(),
        if error == BotError::RoutineRetryUnsupported {
            "Routine retries aren’t available. Send a new manual message or wait for the next occurrence. New messages use Bot access."
        } else {
            "The Bot operation could not be completed."
        },
        matches!(
            error,
            BotError::Unavailable | BotError::Storage | BotError::Settling
        ),
    )
}

fn closed_memory_failure(command: &BotCommand) -> Option<Failure> {
    (!BOT_MEMORY_AVAILABLE && command.is_memory()).then(|| {
        failure(
            "unsupported_capability",
            "Bot Memory is not available yet.",
            false,
        )
    })
}

fn closed_memory_context_failure(
    _context: &CommandContext,
    command: &BotCommand,
) -> Option<Failure> {
    #[cfg(test)]
    if let Some(expected) = _context.memory_for_fixture {
        let actual = match command {
            BotCommand::ReadMemory { bot_id }
            | BotCommand::EnsureMemory { bot_id, .. }
            | BotCommand::RetryMemory { bot_id, .. }
            | BotCommand::AbandonMemory { bot_id, .. }
            | BotCommand::ListMemory { bot_id, .. }
            | BotCommand::ReadMemoryFact { bot_id, .. } => Some(*bot_id),
            _ => None,
        };
        if actual == Some(expected) {
            return None;
        }
    }
    closed_memory_failure(command)
}

mod admission;
mod boot;
pub(super) use boot::{BootRecovery, BotBootJob, BotBootResult};
mod branch;
pub(in crate::worker) mod chat;
mod library;
mod lifecycle;
pub(crate) mod memory;
pub(in crate::worker) mod occurrence;
pub(crate) mod policy;
mod preparation;
mod reads;
pub(crate) mod routine;
mod stop;
pub(super) use admission::{
    AdmissionRetry, BotCoordinationJob, BotCoordinationResult, DeliveryAnswers,
};
pub(super) use branch::{BotBranchCopy, BotBranchJob, BotBranchPrepared, PendingBotBranch};
pub(super) use lifecycle::{LifecycleDriver, LifecycleJob, LifecycleResult};
pub(super) use policy::{BotPolicyJob, BotPolicyResult};
pub(super) use preparation::{BotPreparationJob, CommandContext, PreparedBotCommand};
pub(super) use reads::{BotRunReadJob, RUN_READ_QUEUE_CAPACITY};
pub(super) use stop::{BotTaskStopJob, TASK_STOP_QUEUE_CAPACITY};
mod creation;
mod creation_lane;
pub(super) use creation::{
    BotFolderSystem, CreationDriver, CreationJob, CreationPreparationJob, CreationPrepared,
    CreationReadJob, CreationReadResult, CreationStepResult, NativeFolders,
};
pub(super) use creation_lane::{CreationLane, CreationLaneJob, CreationLaneResult, CreationWork};
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
            // An archived Bot's conversations stay readable (P1.7); a change
            // never carries such a proof past the guard.
            if let Ok(Some(bot)) = lifecycle::readable_bot(&self.repository, *id)
                && bot.revision == *revision
                && bot.policy_revision == *policy
                && self.repository.bot_for_chat(*chat).map_err(bot_failure)? == Some(*id)
                && validate_owner(&proof.authority, &bot).is_ok()
                // A conversation in Trash is out of every read, history and context.
                && !self
                    .repository
                    .bot_chat_hold(*chat)
                    .map_err(bot_failure)?
                    .is_some_and(koru_ai_sqlite::BotHold::hides)
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
        let routine_setup = matches!(
            &request,
            BotCommand::CreateRoutine { .. }
                | BotCommand::ReadRoutineCreation { .. }
                | BotCommand::RetryRoutineCreation { .. }
        );
        let refresh = !routine_setup && !lifecycle::bot_read_command(&request);
        let response = self.bot_command(context, request);
        if refresh && response.is_ok() {
            self.request_bot_boot_refresh();
            self.memory_recovery.request();
            self.routine_recovery.request();
        }
        // Refused because another change of the Bot is still waiting for its
        // owner: offer what is owed now, so the person's retry can succeed.
        if matches!(&response, Err(error) if error.code == BotError::Settling.code()) {
            self.bot_recovery_retry.soon(Instant::now());
        }
        response
    }
    fn bot_command(
        &mut self,
        context: CommandContext,
        request: BotCommand,
    ) -> ApiResult<BotResponse> {
        if let Some(error) = closed_memory_context_failure(&context, &request) {
            authorize(&context, AiScope::ManageBots)?;
            return Err(error);
        }
        if let Some(result) = self.bot_lifecycle_command(&context, &request) {
            return result;
        }
        if let Some(result) = self.bot_read_command(&context, &request) {
            return result;
        }
        if let Some(result) = self.bot_creation_command(&context, &request) {
            return result;
        }
        if let Some(result) = self.bot_library_command(&context, &request) {
            return result;
        }
        if let Some(result) = self.bot_memory_command(&context, &request) {
            return result;
        }
        if let Some(result) = self.bot_routine_creation_command(&context, &request) {
            return result;
        }
        if let BotCommand::CreateChat { request_id, bot_id } = &request {
            let proof = context
                .prepared
                .as_ref()
                .ok_or_else(|| bot_failure(BotError::Unavailable))?;
            return create_bot_chat(
                &mut self.repository,
                &proof.authority,
                &context,
                *request_id,
                *bot_id,
            );
        }
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
        let mut response = execute_bot_command(
            &mut self.repository,
            proof.authority.clone(),
            move |_| validation.clone(),
            context.request,
            request,
        );
        // A parked operation leaves its Bot active, and so in the roster:
        // the roster names it.
        if let Ok(BotResponse::List { bots }) = &mut response {
            self.with_open_operations(bots)?;
        }
        response
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

fn create_bot_chat(
    repository: &mut Repository,
    authority: &Arc<dyn BotAuthority>,
    context: &RequestContext,
    request_id: BotRequestId,
    bot_id: BotId,
) -> ApiResult<BotResponse> {
    authorize(context, AiScope::ManageBots)?;
    let bot = repository
        .bot(bot_id)
        .map_err(bot_failure)?
        .ok_or_else(not_found)?;
    validate_owner(authority, &bot)?;
    let profile = authority
        .workspace(bot.home_workspace)
        .map_err(bot_failure)?;
    let request = BotCommand::CreateChat { request_id, bot_id };
    let fingerprint =
        blake3::hash(&serde_json::to_vec(&request).map_err(|_| bot_failure(BotError::Invalid))?)
            .to_hex()
            .to_string();
    let reference = repository
        .create_bot_chat(&bot, profile, request_id, &fingerprint, Timestamp::now())
        .map_err(bot_failure)?;
    Ok(BotResponse::Chat { reference })
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

fn validate_model_evidence(
    agents: Option<&AgentSessions>,
    config: &BotConfig,
    require_fast: bool,
) -> ApiResult<Option<ReasoningControl>> {
    use koru_ai_wire::agent_readiness_reasons as reason;
    let agent = agents.ok_or_else(|| {
        agents::with_reason(
            failure(
                "runtime_unavailable",
                "The selected runtime is unavailable.",
                true,
            ),
            Some(reason::SERVICE_UNAVAILABLE),
        )
    })?;
    let catalog = agents::agent_models(agent, config.model.provider.clone(), false)
        .map_err(|refusal| agents::explain_agent_refusal(agent, &config.model.provider, refusal))?;
    validate_model_catalog(config, &catalog, require_fast)
}

fn validate_model_catalog(
    config: &BotConfig,
    catalog: &koru_ai_wire::AgentModelsResponse,
    require_fast: bool,
) -> ApiResult<Option<ReasoningControl>> {
    if catalog.provider != config.model.provider {
        return Err(bot_failure(BotError::Unavailable));
    }
    let model = catalog
        .models
        .iter()
        .find(|model| model.id == config.model.requested_model)
        .ok_or_else(|| {
            agents::with_reason(
                failure(
                    "runtime_unavailable",
                    "The selected model is unavailable.",
                    false,
                ),
                Some(koru_ai_wire::agent_readiness_reasons::MODEL_NOT_OFFERED),
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
        || ((require_fast || config.fast == Some(true)) && model.fast.is_none())
    {
        return Err(failure(
            "unsupported_capability",
            "The selected runtime settings are unavailable.",
            false,
        ));
    }
    Ok(model.reasoning.clone())
}

fn execute_bot_command(
    repository: &mut impl BotRepository,
    authority: Arc<dyn BotAuthority>,
    validate_runtime: impl Fn(&BotConfig) -> ApiResult<()>,
    context: RequestContext,
    mut request: BotCommand,
) -> ApiResult<BotResponse> {
    authorize(&context, AiScope::ManageBots)?;
    if let Some(error) = closed_memory_failure(&request) {
        return Err(error);
    }
    let owner = authority.owner().map_err(bot_failure)?;
    let now = Timestamp::now().millis();
    match &mut request {
        BotCommand::CreateChat { .. }
        | BotCommand::ReadRoutines { .. }
        | BotCommand::CreateRoutine { .. }
        | BotCommand::ReadRoutineCreation { .. }
        | BotCommand::RetryRoutineCreation { .. }
        | BotCommand::Start { .. }
        | BotCommand::Continue { .. }
        | BotCommand::Pause { .. }
        | BotCommand::Resume { .. }
        | BotCommand::ReadOperation { .. }
        | BotCommand::ReconcileOperation { .. }
        | BotCommand::Archive { .. }
        | BotCommand::Trash { .. }
        | BotCommand::Restore { .. }
        | BotCommand::TrashChat { .. }
        | BotCommand::RestoreChat { .. }
        | BotCommand::PurgeBot { .. }
        | BotCommand::PurgeChat { .. }
        | BotCommand::ReadLifecycleOperation { .. }
        | BotCommand::ListLifecycle { .. }
        | BotCommand::ListBotChats { .. }
        | BotCommand::ListActivity { .. }
        | BotCommand::ListTrashedBotChats { .. }
        | BotCommand::CreateBotWithRole { .. }
        | BotCommand::ReadBotCreation { .. }
        | BotCommand::ListBotCreations { .. }
        | BotCommand::RetryBotCreation { .. }
        | BotCommand::AbandonBotCreation { .. }
        | BotCommand::ReadRun { .. }
        | BotCommand::ReadTaskAnswer { .. }
        | BotCommand::ReadLibrary { .. }
        | BotCommand::SetKnowledge { .. }
        | BotCommand::ReadMemory { .. }
        | BotCommand::EnsureMemory { .. }
        | BotCommand::RetryMemory { .. }
        | BotCommand::AbandonMemory { .. }
        | BotCommand::ListMemory { .. }
        | BotCommand::ReadMemoryFact { .. }
        | BotCommand::ListTaskBots { .. }
        | BotCommand::StopTask { .. } => return Err(bot_failure(BotError::Unavailable)),
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
                    lifecycle: Default::default(),
                    open_operation: None,
                    held_by: None,
                    pending_change: None,
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
            require_held_owner(repository, authority, &command)?;
            let bot = lifecycle::provable_bot(repository, id, &command)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            validate_owner(
                authority.ok_or_else(|| bot_failure(BotError::Unavailable))?,
                &bot,
            )?;
            // An archived Bot's conversations read; nothing changes them.
            if bot.lifecycle == BotLifecycle::Archived && !command.bot_read_only() {
                return Err(lifecycle::archived_failure());
            }
            let implemented = matches!(
                command,
                WorkerCommand::SendTurn { .. }
                    | WorkerCommand::RetryTurn { .. }
                    | WorkerCommand::ApproveToolCall { .. }
                    | WorkerCommand::DenyToolCall { .. }
                    | WorkerCommand::SteerAttempt { .. }
                    | WorkerCommand::UpdateChatModel { .. }
                    | WorkerCommand::UpdateChatAgentSettings { .. }
                    // Its title alone, as on an AI chat (the owner's rule
                    // for Bot conversations); never the Bot's name.
                    | WorkerCommand::RenameChat { .. }
                    | WorkerCommand::BranchChat { .. }
                    // The ordinary chat's own attachment staging (P1.5);
                    // what a Bot Send may deliver is decided by its `admit`.
                    | WorkerCommand::AttachmentAdmission { .. }
                    | WorkerCommand::WorkspaceCopyAdmission { .. }
                    | WorkerCommand::StageAttachment { .. }
                    | WorkerCommand::RemoveStagedAttachment { .. }
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
        // After the Bot owner check, so a hidden Bot source still reads as missing.
        if let WorkerCommand::BranchChat { chat, request, .. } = &command {
            branch::require_branch_destination(repository, *chat, request.branch_chat_id)?;
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

/// The lifecycle hold that refuses this command, if any: its own Bot
/// conversation's, or a context block's taken from a conversation in Trash.
pub(super) fn refusing_hold(
    repository: &Repository,
    command: &WorkerCommand,
) -> ApiResult<Option<(BotId, koru_ai_sqlite::BotHold)>> {
    let mut holds = Vec::new();
    if let Some(chat) = command_chat(repository, command)?
        && let Some(bot) = repository.bot_for_chat(chat).map_err(bot_failure)?
        && let Some(hold) = repository.bot_chat_hold(chat).map_err(bot_failure)?
    {
        holds.push((bot, hold));
    }
    let refs = match command {
        WorkerCommand::SendTurn { request, .. } => Some(&request.explicit_refs),
        WorkerCommand::RetryTurn { request, .. } => Some(&request.explicit_refs),
        _ => None,
    };
    for reference in refs.into_iter().flatten() {
        if let ContextRef::MessageBlock { block_id } = reference
            && let Some((bot, source)) = repository
                .bot_chat_for_block(*block_id)
                .map_err(bot_failure)?
            && let Some(hold) = repository
                .bot_chat_hold(source)
                .map_err(bot_failure)?
                .filter(|hold| hold.hides())
        {
            holds.push((bot, hold));
        }
    }
    Ok(holds
        .into_iter()
        .find(|(_, hold)| lifecycle::hold_refuses(command, *hold)))
}

/// A held conversation refuses here, before the active-Bot read (which would
/// call a trashed Bot someone else's) and before any mutation. Ownership is
/// checked first, so only the Bot's owner learns it is held.
fn require_held_owner(
    repository: &Repository,
    authority: Option<&Arc<dyn BotAuthority>>,
    command: &WorkerCommand,
) -> ApiResult<()> {
    let Some((bot, hold)) = refusing_hold(repository, command)? else {
        return Ok(());
    };
    let owner = authority
        .ok_or_else(|| bot_failure(BotError::Unavailable))?
        .owner()
        .map_err(bot_failure)?;
    if repository.bot_owner(bot).map_err(bot_failure)? != Some(owner) {
        return Err(bot_failure(BotError::Forbidden));
    }
    Err(lifecycle::hold_failure(hold))
}

pub(super) fn command_chat(
    repository: &Repository,
    command: &WorkerCommand,
) -> ApiResult<Option<ChatId>> {
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
