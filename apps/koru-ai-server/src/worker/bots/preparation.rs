// SPDX-License-Identifier: MPL-2.0
//! One-command authority evidence, prepared off the AI writer by its existing bounded pool.
use super::*;
use std::collections::BTreeSet;

#[derive(Clone)]
pub(in crate::worker) struct CommandContext {
    pub(in crate::worker) request: RequestContext,
    pub(in crate::worker) prepared: Option<Arc<PreparedBotCommand>>,
}
impl From<RequestContext> for CommandContext {
    fn from(request: RequestContext) -> Self {
        Self {
            request,
            prepared: None,
        }
    }
}
impl std::ops::Deref for CommandContext {
    type Target = RequestContext;
    fn deref(&self) -> &RequestContext {
        &self.request
    }
}
pub(in crate::worker) struct PreparedBotCommand {
    pub(in crate::worker) authority: Arc<dyn BotAuthority>,
    pub(in crate::worker) chats: Vec<(BotId, ChatId, u64, u64)>,
    pub(in crate::worker) runtime: ApiResult<()>,
    pub(in crate::worker) pins: Vec<koru_delegate_core::RunContextItem>,
    pub(in crate::worker) chat_snapshot: Option<Chat>,
    pub(in crate::worker) settings: Option<chat::SettingsProof>,
    pub(in crate::worker) terminal_question: Option<chat::TerminalQuestionProof>,
}
pub(in crate::worker) struct BotPreparationJob {
    command: WorkerCommand,
    authority: Arc<dyn BotAuthority>,
    workspaces: Vec<WorkspaceId>,
    pages: Vec<BotPage>,
    chats: Vec<(BotId, ChatId, u64, u64)>,
    model: Option<BotConfig>,
    capture: Option<(BotOwner, WorkspaceId, Vec<BotPage>)>,
    chat_snapshot: Option<Chat>,
    terminal_question: Option<chat::TerminalQuestionProof>,
    delegate: Option<Arc<dyn koru_delegate_api::bots::BotDelegatePort>>,
}
impl BotPreparationJob {
    pub(in crate::worker) fn run(
        self,
        agents: Option<&AgentSessions>,
    ) -> (WorkerCommand, ApiResult<PreparedBotCommand>) {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let changing_settings = matches!(
                &self.command,
                WorkerCommand::UpdateChatModel { .. }
                    | WorkerCommand::UpdateChatAgentSettings { .. }
            );
            let require_fast = matches!(&self.command,
                WorkerCommand::UpdateChatAgentSettings { request, .. } if request.fast.is_some());
            let validation = match &self.model {
                Some(config) => {
                    validate_config(config, config.role.workspace).map_err(bot_failure)?;
                    validate_model_evidence(agents, config, require_fast)
                }
                None => Ok(None),
            };
            let settings = changing_settings
                .then(|| {
                    self.model.as_ref().map(|config| {
                        chat::SettingsProof::new(config, validation.clone().ok().flatten())
                    })
                })
                .flatten();
            let runtime = validation.map(|_| ());
            let authority: Arc<dyn BotAuthority> = Arc::new(
                self.authority
                    .inspect(&self.workspaces, &self.pages)
                    .map_err(bot_failure)?,
            );
            let pins = if let Some((owner, workspace, pages)) = &self.capture {
                runtime.clone()?;
                if authority.owner().map_err(bot_failure)? != *owner {
                    return Err(bot_failure(BotError::Forbidden));
                }
                let profile = authority.workspace(*workspace).map_err(bot_failure)?;
                self.authority
                    .capture(*owner, *workspace, profile, pages)
                    .map_err(bot_failure)?
            } else {
                Vec::new()
            };
            if let Some(question) = &self.terminal_question {
                question.verify(self.delegate.as_ref())?;
            }
            Ok(PreparedBotCommand {
                authority,
                chats: self.chats.clone(),
                runtime,
                pins,
                chat_snapshot: self.chat_snapshot.clone(),
                settings,
                terminal_question: self.terminal_question.clone(),
            })
        }))
        .unwrap_or_else(|_| Err(bot_failure(BotError::Unavailable)));
        (self.command, result)
    }
}
impl WorkerState {
    pub(in crate::worker) fn prepare_bot_command(
        &mut self,
        command: WorkerCommand,
    ) -> Option<WorkerCommand> {
        let Some(context) = command.bot_context() else {
            return Some(command);
        };
        if context.prepared.is_some()
            || !context.principal.scopes.contains(&AiScope::ManageBots)
            || self.bot_authority.is_none()
        {
            return Some(command);
        }
        if let Err(error) = authorize(context, AiScope::ManageBots) {
            command.refuse(error);
            return None;
        }
        let targets = match bot_targets(&self.repository, &command) {
            Ok(Some(targets)) => targets,
            Ok(None) => return Some(command),
            Err(error) => {
                command.refuse(error);
                return None;
            }
        };
        let snapshot = (|| -> ApiResult<_> {
            // A change to an archived Bot answers `bot_archived` at dispatch
            // once its owner's workspace authority is proven; nothing is
            // captured for it here, not even Role history or a model check.
            let archived = super::lifecycle::archived_change(&self.repository, &command)?.is_some();
            let mut workspaces = BTreeSet::new();
            let mut pages = BTreeSet::new();
            let mut chats = Vec::new();
            for (id, chat) in targets {
                let bot = match super::lifecycle::provable_bot(&self.repository, id, &command) {
                    Ok(Some(bot)) => bot,
                    Ok(None) | Err(BotError::Forbidden | BotError::NotFound) => continue,
                    Err(error) => return Err(bot_failure(error)),
                };
                workspaces.insert(bot.home_workspace);
                if !archived
                    && matches!(
                        &command,
                        WorkerCommand::Bots {
                            request: BotCommand::Update { .. } | BotCommand::UpdateAccess { .. },
                            ..
                        }
                    )
                {
                    pages.insert(bot.config.role.clone());
                    pages.extend(bot.selected_pages.clone());
                }
                chats.push((id, chat, bot.revision, bot.policy_revision));
            }
            let mut model = None;
            let mut capture = None;
            let mut chat_snapshot = None;
            if let WorkerCommand::Bots { request, .. } = &command
                && !archived
            {
                match request {
                    BotCommand::Create {
                        home_workspace,
                        config,
                        selected_pages,
                        ..
                    } => {
                        workspaces.insert(*home_workspace);
                        pages.insert(config.role.clone());
                        pages.extend(selected_pages.clone());
                        // An unqualified runtime is refused by the writer;
                        // it is not discovered first.
                        if qualified_bot_runtime(&config.model.provider) {
                            model = Some(config.clone());
                        }
                    }
                    BotCommand::Update { config, .. } => {
                        pages.insert(config.role.clone());
                        if qualified_bot_runtime(&config.model.provider) {
                            model = Some(config.clone());
                        }
                    }
                    BotCommand::UpdateAccess { selected_pages, .. } => {
                        pages.extend(selected_pages.clone())
                    }
                    BotCommand::List { workspace } => {
                        workspaces.insert(*workspace);
                    }
                    BotCommand::Start {
                        bot_id,
                        request_id,
                        chat_id,
                        destination,
                        task,
                        ..
                    }
                    | BotCommand::Continue {
                        bot_id,
                        request_id,
                        chat_id,
                        destination,
                        task,
                        ..
                    } => 'start: {
                        // A trashed Bot, one under a whole-Bot operation, or a
                        // held conversation of it, is refused at dispatch by its
                        // hold with the stable code (P1.7). Nothing is captured
                        // for it here, not even Role history.
                        if self
                            .repository
                            .bot_hold(*bot_id)
                            .map_err(bot_failure)?
                            .is_some()
                            || (self
                                .repository
                                .bot_for_chat(*chat_id)
                                .map_err(bot_failure)?
                                == Some(*bot_id)
                                && self
                                    .repository
                                    .bot_chat_hold(*chat_id)
                                    .map_err(bot_failure)?
                                    .is_some())
                        {
                            break 'start;
                        }
                        let bot = self
                            .repository
                            .bot(*bot_id)
                            .map_err(bot_failure)?
                            .ok_or_else(|| bot_failure(BotError::NotFound))?;
                        task.validate(*destination).map_err(bot_failure)?;
                        if *destination != bot.home_workspace {
                            return Err(bot_failure(BotError::Forbidden));
                        }
                        let owner = BotOwner {
                            user_scope: bot.user_scope,
                            device: bot.owner_device,
                        };
                        // Retrying a durable intent never recaptures a different
                        // Role version or requires its original runtime to be online.
                        if self
                            .repository
                            .bot_admission_receipt(owner, *bot_id, *request_id)
                            .map_err(bot_failure)?
                            .is_none()
                        {
                            let mut captured = vec![bot.config.role.clone()];
                            for page in &task.documents {
                                if !captured.contains(page) {
                                    captured.push(page.clone());
                                }
                            }
                            pages.extend(captured.clone());
                            model = Some(bot.config.clone());
                            capture = Some((owner, *destination, captured));
                        }
                    }
                    BotCommand::CreateChat { .. }
                    | BotCommand::Read { .. }
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
                    | BotCommand::ListBotChats { .. } => {}
                    BotCommand::ListLifecycle { workspace, .. } => {
                        workspaces.insert(*workspace);
                    }
                    BotCommand::ListActivity { workspace } => {
                        workspaces.insert(*workspace);
                    }
                }
            }
            // A held conversation is refused by the guard once its owner is
            // known; nothing is captured for it, not even Role history.
            let held = archived || super::refusing_hold(&self.repository, &command)?.is_some();
            if !held && let Some(chat) = chat::chat_capture(&self.repository, &command)? {
                pages.extend(chat.pages.clone());
                model = Some(chat.model);
                capture = Some((chat.owner, chat.destination, chat.pages));
                chat_snapshot = Some(chat.chat);
            }
            if !held
                && let Some((chat, config)) = chat::settings_capture(&self.repository, &command)?
            {
                model = Some(config);
                chat_snapshot = Some(chat);
            }
            let terminal_question = if held {
                None
            } else {
                self.bot_terminal_question_capture(&command)?
            };
            // Every referenced workspace is inspected, including invalid foreign-page requests.
            workspaces.extend(pages.iter().map(|p| p.workspace));
            Ok((
                workspaces.into_iter().collect(),
                pages.into_iter().collect(),
                chats,
                model,
                capture,
                chat_snapshot,
                terminal_question,
            ))
        })();
        let (workspaces, pages, chats, model, capture, chat_snapshot, terminal_question) =
            match snapshot {
                Ok(value) => value,
                Err(error) => {
                    command.refuse(error);
                    return None;
                }
            };
        let job = BotPreparationJob {
            command,
            authority: self.bot_authority.as_ref().unwrap().clone(),
            workspaces,
            pages,
            chats,
            model,
            capture,
            chat_snapshot,
            terminal_question,
            delegate: self.bot_delegate.clone(),
        };
        let Some(pool) = self.provider_admin.as_ref() else {
            job.command.refuse(bot_failure(BotError::Unavailable));
            return None;
        };
        if let Err(error) = pool.try_submit(ProviderAdminJob::BotPrepare(job)) {
            if let ProviderAdminJob::BotPrepare(job) = error.job {
                job.command.refuse(error.failure);
            }
        }
        None
    }
    pub(in crate::worker) fn complete_bot_preparation(
        &mut self,
        mut command: WorkerCommand,
        proof: ApiResult<PreparedBotCommand>,
    ) {
        let proof = match proof {
            Ok(proof) => proof,
            Err(error) => {
                command.refuse(error);
                return;
            }
        };
        if let Some(context) = command.bot_context()
            && let Err(error) = authorize(context, AiScope::ManageBots)
        {
            command.refuse(error);
            return;
        }
        // Authority evidence is bound to the writer snapshot; a queued edit/lifecycle change invalidates it.
        if let Some(expected) = &proof.chat_snapshot {
            match self
                .repository
                .chat_with_access(expected.id, &ChatReadAccess::authorized(vec![expected.id]))
            {
                Ok(Some(chat))
                    if chat.current_model == expected.current_model
                        && chat.agent_mode == expected.agent_mode
                        && chat.agent_fast == expected.agent_fast => {}
                _ => {
                    command.refuse(bot_failure(BotError::RevisionConflict));
                    return;
                }
            }
        }
        let destination = branch_destination(&command);
        for (id, chat, revision, policy) in &proof.chats {
            let owner = self.repository.bot_for_chat(*chat).ok().flatten();
            match super::lifecycle::provable_bot(&self.repository, *id, &command) {
                Ok(Some(bot))
                    if (owner == Some(*id) || (owner.is_none() && destination == Some(*chat)))
                        && bot.revision == *revision
                        && bot.policy_revision == *policy => {}
                _ => {
                    command.refuse(bot_failure(BotError::RevisionConflict));
                    return;
                }
            }
        }
        if let Some(context) = command.bot_context_mut() {
            context.prepared = Some(Arc::new(proof));
        }
        self.handle_command(command);
    }
}

/// A named ordinary Chat never queues behind unrelated Bot authority work.
pub(super) fn bot_targets(
    repository: &Repository,
    command: &WorkerCommand,
) -> ApiResult<Option<Vec<(BotId, ChatId)>>> {
    let aggregate = matches!(
        command,
        WorkerCommand::ListChats { .. }
            | WorkerCommand::Search { .. }
            | WorkerCommand::ToolListChats { .. }
            | WorkerCommand::ToolSearchChats { .. }
            | WorkerCommand::Usage { chat: None, .. }
            | WorkerCommand::Bots {
                request: BotCommand::List { .. } | BotCommand::ListLifecycle { .. },
                ..
            }
    );
    if aggregate {
        let mut rows = repository.visible_bot_chats().map_err(bot_failure)?;
        if let WorkerCommand::Bots {
            request: BotCommand::List { workspace } | BotCommand::ListLifecycle { workspace, .. },
            ..
        } = command
        {
            rows.retain(|(id, _)| {
                repository
                    .bot(*id)
                    .ok()
                    .flatten()
                    .is_some_and(|bot| bot.home_workspace == *workspace)
            });
        }
        return Ok(Some(rows));
    }
    let mut rows = BTreeSet::new();
    if let WorkerCommand::Bots { request, .. } = command {
        let id = match request {
            BotCommand::Create { bot_id, .. }
            | BotCommand::CreateChat { bot_id, .. }
            | BotCommand::Read { bot_id }
            | BotCommand::Update { bot_id, .. }
            | BotCommand::UpdateAccess { bot_id, .. }
            | BotCommand::Start { bot_id, .. }
            | BotCommand::Continue { bot_id, .. }
            | BotCommand::Pause { bot_id, .. }
            | BotCommand::Resume { bot_id, .. }
            | BotCommand::ReadOperation { bot_id, .. }
            | BotCommand::ReconcileOperation { bot_id, .. }
            | BotCommand::Archive { bot_id, .. }
            | BotCommand::Trash { bot_id, .. }
            | BotCommand::Restore { bot_id, .. }
            | BotCommand::TrashChat { bot_id, .. }
            | BotCommand::RestoreChat { bot_id, .. }
            | BotCommand::PurgeBot { bot_id, .. }
            | BotCommand::PurgeChat { bot_id, .. }
            | BotCommand::ReadLifecycleOperation { bot_id, .. }
            | BotCommand::ListBotChats { bot_id, .. } => *bot_id,
            BotCommand::List { .. } | BotCommand::ListLifecycle { .. } => unreachable!(),
            // The workspace alone is proved; the read names no Bot or chat.
            BotCommand::ListActivity { .. } => return Ok(Some(Vec::new())),
        };
        match super::lifecycle::provable_bot(repository, id, command) {
            Ok(Some(bot)) => {
                let chat = match request {
                    // A named conversation must already belong to this Bot; refusing here
                    // keeps a foreign Chat from reading as a retryable revision race.
                    BotCommand::Start { chat_id, .. } | BotCommand::Continue { chat_id, .. } => {
                        if repository.bot_for_chat(*chat_id).map_err(bot_failure)? != Some(id) {
                            return Err(bot_failure(BotError::Forbidden));
                        }
                        *chat_id
                    }
                    _ => bot.main_chat,
                };
                rows.insert((id, chat));
            }
            Ok(None) | Err(BotError::Forbidden | BotError::NotFound) => {}
            Err(error) => return Err(bot_failure(error)),
        }
    }
    if let Some(chat) = command_chat(repository, command)?
        && let Some(id) = repository.bot_for_chat(chat).map_err(bot_failure)?
    {
        rows.insert((id, chat));
        // A Bot branch's destination is proved "absent, or this Bot's"; the
        // guard refuses any other owner before the copy is read.
        if let Some(destination) = branch_destination(command)
            && repository
                .bot_for_chat(destination)
                .map_err(bot_failure)?
                .is_none_or(|owner| owner == id)
        {
            rows.insert((id, destination));
        }
    }
    let refs = match command {
        WorkerCommand::SendTurn { request, .. } => Some(&request.explicit_refs),
        WorkerCommand::RetryTurn { request, .. } => Some(&request.explicit_refs),
        _ => None,
    };
    if let Some(refs) = refs {
        for reference in refs {
            if let ContextRef::MessageBlock { block_id } = reference
                && let Some(pair) = repository
                    .bot_chat_for_block(*block_id)
                    .map_err(bot_failure)?
            {
                rows.insert(pair);
            }
        }
    }
    if rows.is_empty() && !matches!(command, WorkerCommand::Bots { .. }) {
        return Ok(None);
    }
    Ok(Some(rows.into_iter().collect()))
}

/// The only proof row whose association may not exist yet: the Chat a branch creates.
fn branch_destination(command: &WorkerCommand) -> Option<ChatId> {
    match command {
        WorkerCommand::BranchChat { request, .. } => Some(request.branch_chat_id),
        _ => None,
    }
}
