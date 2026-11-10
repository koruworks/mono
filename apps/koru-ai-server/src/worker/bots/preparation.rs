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
}
pub(in crate::worker) struct BotPreparationJob {
    command: WorkerCommand,
    authority: Arc<dyn BotAuthority>,
    workspaces: Vec<WorkspaceId>,
    pages: Vec<BotPage>,
    chats: Vec<(BotId, ChatId, u64, u64)>,
    model: Option<BotConfig>,
}
impl BotPreparationJob {
    pub(in crate::worker) fn run(
        self,
        agents: Option<&AgentSessions>,
    ) -> (WorkerCommand, ApiResult<PreparedBotCommand>) {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let runtime = match &self.model {
                Some(config) => {
                    validate_config(config, config.role.workspace).map_err(bot_failure)?;
                    validate_model(agents, config)
                }
                None => Ok(()),
            };
            let authority: Arc<dyn BotAuthority> = Arc::new(
                self.authority
                    .inspect(&self.workspaces, &self.pages)
                    .map_err(bot_failure)?,
            );
            Ok(PreparedBotCommand {
                authority,
                chats: self.chats.clone(),
                runtime,
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
            let mut workspaces = BTreeSet::new();
            let mut pages = BTreeSet::new();
            let mut chats = Vec::new();
            for (id, chat) in targets {
                let bot = match self.repository.bot(id) {
                    Ok(Some(bot)) => bot,
                    Ok(None) | Err(BotError::Forbidden | BotError::NotFound) => continue,
                    Err(error) => return Err(bot_failure(error)),
                };
                workspaces.insert(bot.home_workspace);
                if matches!(
                    &command,
                    WorkerCommand::Bots {
                        request: BotCommand::Update { .. } | BotCommand::UpdateAccess { .. },
                        ..
                    }
                ) {
                    pages.insert(bot.config.role.clone());
                    pages.extend(bot.selected_pages.clone());
                }
                chats.push((id, chat, bot.revision, bot.policy_revision));
            }
            let mut model = None;
            if let WorkerCommand::Bots { request, .. } = &command {
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
                        model = Some(config.clone());
                    }
                    BotCommand::Update { config, .. } => {
                        pages.insert(config.role.clone());
                        model = Some(config.clone());
                    }
                    BotCommand::UpdateAccess { selected_pages, .. } => {
                        pages.extend(selected_pages.clone())
                    }
                    BotCommand::List { workspace } => {
                        workspaces.insert(*workspace);
                    }
                    BotCommand::Read { .. } => {}
                }
            }
            // Every referenced workspace is inspected, including invalid foreign-page requests.
            workspaces.extend(pages.iter().map(|p| p.workspace));
            Ok((
                workspaces.into_iter().collect(),
                pages.into_iter().collect(),
                chats,
                model,
            ))
        })();
        let (workspaces, pages, chats, model) = match snapshot {
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
        for (id, chat, revision, policy) in &proof.chats {
            match self.repository.bot(*id) {
                Ok(Some(bot))
                    if bot.main_chat == *chat
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
                request: BotCommand::List { .. },
                ..
            }
    );
    if aggregate {
        let mut rows = repository.all_bot_chats().map_err(bot_failure)?;
        if let WorkerCommand::Bots {
            request: BotCommand::List { workspace },
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
    let mut ids = BTreeSet::new();
    if let WorkerCommand::Bots { request, .. } = command {
        match request {
            BotCommand::Create { bot_id, .. }
            | BotCommand::Read { bot_id }
            | BotCommand::Update { bot_id, .. }
            | BotCommand::UpdateAccess { bot_id, .. } => {
                ids.insert(*bot_id);
            }
            BotCommand::List { .. } => unreachable!(),
        }
    }
    if let Some(chat) = command_chat(repository, command)?
        && let Some(id) = repository.bot_for_chat(chat).map_err(bot_failure)?
    {
        ids.insert(id);
    }
    let refs = match command {
        WorkerCommand::SendTurn { request, .. } => Some(&request.explicit_refs),
        WorkerCommand::RetryTurn { request, .. } => Some(&request.explicit_refs),
        _ => None,
    };
    if let Some(refs) = refs {
        for reference in refs {
            if let ContextRef::MessageBlock { block_id } = reference
                && let Some(id) = repository.bot_for_block(*block_id).map_err(bot_failure)?
            {
                ids.insert(id);
            }
        }
    }
    if ids.is_empty() && !matches!(command, WorkerCommand::Bots { .. }) {
        return Ok(None);
    }
    let mut rows = Vec::new();
    for id in ids {
        match repository.bot(id) {
            Ok(Some(bot)) => rows.push((id, bot.main_chat)),
            Ok(None) | Err(BotError::Forbidden | BotError::NotFound) => {}
            Err(error) => return Err(bot_failure(error)),
        }
    }
    Ok(Some(rows))
}
