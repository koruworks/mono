// SPDX-License-Identifier: MPL-2.0
//! One-command authority evidence, prepared off the AI writer on preparation's
//! own lane of its bounded pool (`koru-ai-provider-prepare`).
//!
//! What a preparation may wait on: the Store, through the authority adapter
//! (`inspect`, and `capture` of Role history for a task), each hop bounded by
//! the Store worker's own 30 s; the session host's model catalog, for a
//! change that names a model; the folder system, for a changed folder; and,
//! for a terminal question card answered by the next message, one execution
//! owner read (`Reconcile`), bounded by [`PREPARATION_OWNER_WITHIN`]. It never
//! waits on background work: no lifecycle, recovery, probe, walk, branch,
//! policy or provider job runs on this lane.
use super::*;
use std::collections::BTreeSet;

/// The longest a preparation waits on the execution owner (the terminal
/// question card's `Reconcile`): well inside the caller's own
/// `RESPONSE_TIMEOUT`, so a hung owner answers `chat_busy` (retryable), not a
/// worker timeout, and holds the people queued behind it for no longer.
/// Shorter under test, so a hung owner is proved in seconds.
#[cfg(not(test))]
pub(in crate::worker) const PREPARATION_OWNER_WITHIN: Duration = Duration::from_secs(10);
#[cfg(test)]
pub(in crate::worker) const PREPARATION_OWNER_WITHIN: Duration = Duration::from_secs(2);
const _: () = assert!(PREPARATION_OWNER_WITHIN.as_millis() * 3 <= RESPONSE_TIMEOUT.as_millis());

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
    pub(in crate::worker) library_sources: Option<library::LibrarySources>,
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
    library_sources: Option<library::LibrarySources>,
    chat_snapshot: Option<Chat>,
    terminal_question: Option<chat::TerminalQuestionProof>,
    delegate: Option<Arc<dyn koru_delegate_api::bots::BotDelegatePort>>,
    /// A changed folder to check before the first task (P1.8): it must exist,
    /// be a folder and not be Koru's own data. Update binds its canonical form.
    folder: Option<(Arc<dyn BotFolderSystem>, creation::ReservedFolders, String)>,
    /// Tests only: model discovery answers ready without a session host.
    #[cfg(test)]
    runtime_ready: bool,
}
impl BotPreparationJob {
    #[cfg(test)]
    fn runtime_ready(&self) -> bool {
        self.runtime_ready
    }
    #[cfg(not(test))]
    fn runtime_ready(&self) -> bool {
        false
    }
    pub(in crate::worker) fn run(
        mut self,
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
                    if self.runtime_ready() {
                        Ok(None)
                    } else {
                        validate_model_evidence(agents, config, require_fast)
                    }
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
            let binding = match &self.folder {
                Some((folders, reserved, path)) => Some(creation::require_existing_folder(
                    folders.as_ref(),
                    reserved,
                    path,
                )),
                None => None,
            };
            let runtime = validation
                .map(|_| ())
                .and_then(|()| binding.clone().transpose().map(|_| ()));
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
            Ok((
                PreparedBotCommand {
                    authority,
                    chats: self.chats.clone(),
                    runtime,
                    pins,
                    library_sources: self.library_sources.clone(),
                    chat_snapshot: self.chat_snapshot.clone(),
                    settings,
                    terminal_question: self.terminal_question.clone(),
                },
                binding.and_then(Result::ok),
            ))
        }))
        .unwrap_or_else(|_| Err(bot_failure(BotError::Unavailable)));
        // Update binds the folder where it resolves, as creation does; the
        // request is fingerprinted with that binding.
        let result = result.map(|(proof, binding)| {
            if let (
                Some(binding),
                WorkerCommand::Bots {
                    request: BotCommand::Update { config, .. },
                    ..
                },
            ) = (binding, &mut self.command)
            {
                config.working_folder = binding;
            }
            proof
        });
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
        // Creation and its read run their own owner checks (P1.8 B2).
        let command = self.route_bot_creation(command)?;
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
            let mut library_sources = None;
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
                        model = Some(config.clone());
                    }
                    BotCommand::Update { config, .. } => {
                        pages.insert(config.role.clone());
                        model = Some(config.clone());
                    }
                    BotCommand::UpdateAccess { selected_pages, .. } => {
                        pages.extend(selected_pages.clone())
                    }
                    BotCommand::SetKnowledge {
                        bot_id,
                        request_id,
                        expected_revision,
                        pages: knowledge,
                    } => {
                        let bot = self
                            .repository
                            .bot(*bot_id)
                            .map_err(bot_failure)?
                            .ok_or_else(not_found)?;
                        let input = PreparedBotKnowledge {
                            owner: BotOwner {
                                user_scope: bot.user_scope,
                                device: bot.owner_device,
                            },
                            bot: *bot_id,
                            request: *request_id,
                            expected_revision: *expected_revision,
                            pages: knowledge.clone(),
                        };
                        if self
                            .repository
                            .bot_knowledge_receipt(&input, &library::command_fingerprint(request)?)
                            .map_err(bot_failure)?
                            .is_none()
                        {
                            pages.extend(knowledge.clone());
                        }
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
                            let sources = library::LibrarySources::read(
                                &self.repository,
                                &bot,
                                task.documents.clone(),
                            )?;
                            let captured = sources.pages();
                            pages.extend(captured.clone());
                            model = Some(bot.config.clone());
                            capture = Some((owner, *destination, captured));
                            library_sources = Some(sources);
                        }
                    }
                    BotCommand::CreateChat { .. }
                    | BotCommand::Read { .. }
                    | BotCommand::ReadLibrary { .. }
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
                    | BotCommand::ListBotChats { .. }
                    | BotCommand::ReadRun { .. }
                    | BotCommand::ReadTaskAnswer { .. }
                    | BotCommand::StopTask { .. } => {}
                    BotCommand::ListLifecycle { workspace, .. } => {
                        workspaces.insert(*workspace);
                    }
                    BotCommand::ListActivity { workspace }
                    | BotCommand::ListTaskBots { workspace, .. }
                    | BotCommand::ListTrashedBotChats { workspace, .. } => {
                        workspaces.insert(*workspace);
                    }
                    BotCommand::ListBotCreations { workspace, .. } => {
                        workspaces.insert(*workspace);
                    }
                    // Routed to their own preparation before this.
                    BotCommand::CreateBotWithRole { .. }
                    | BotCommand::ReadBotCreation { .. }
                    | BotCommand::RetryBotCreation { .. }
                    | BotCommand::AbandonBotCreation { .. } => {}
                }
            }
            // A held conversation is refused by the guard once its owner is
            // known; nothing is captured for it, not even Role history.
            let held = archived || super::refusing_hold(&self.repository, &command)?.is_some();
            if !held && let Some(chat) = chat::chat_capture(&self.repository, &command)? {
                pages.extend(chat.pages.clone());
                model = Some(chat.model);
                capture = Some((chat.owner, chat.destination, chat.pages));
                library_sources = Some(chat.library_sources);
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
                library_sources,
                chat_snapshot,
                terminal_question,
            ))
        })();
        let (
            workspaces,
            pages,
            chats,
            model,
            capture,
            library_sources,
            chat_snapshot,
            terminal_question,
        ) = match snapshot {
            Ok(value) => value,
            Err(error) => {
                command.refuse(error);
                return None;
            }
        };
        // A read failure refuses: the folder check is never skipped by an
        // error. An archived Bot's change is refused at dispatch, and a
        // trashed or missing one reads as not found there.
        let folder = match &command {
            WorkerCommand::Bots {
                request: BotCommand::Update { bot_id, config, .. },
                ..
            } => match super::lifecycle::readable_bot(&self.repository, *bot_id) {
                Ok(Some(bot))
                    if bot.lifecycle == BotLifecycle::Active
                        && bot.config.working_folder != config.working_folder =>
                {
                    Some((
                        self.bot_creations.folders(),
                        self.bot_creations.reserved(),
                        config.working_folder.clone(),
                    ))
                }
                Ok(_) => None,
                Err(error) => {
                    command.refuse(bot_failure(error));
                    return None;
                }
            },
            _ => None,
        };
        let job = BotPreparationJob {
            folder,
            command,
            authority: self.bot_authority.as_ref().unwrap().clone(),
            workspaces,
            pages,
            chats,
            model,
            capture,
            library_sources,
            chat_snapshot,
            terminal_question,
            delegate: self.bot_delegate.clone(),
            #[cfg(test)]
            runtime_ready: self.test_bot_runtime_ready,
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
        // A Bot read answers from the row as it is at dispatch, and proves the
        // owner and the current workspace authority against that row there,
        // so its proof is not fenced to the revision it was prepared at: an
        // operation moving the revision (Archive's pause, its completion)
        // must not refuse a client polling its progress.
        // A task's Stop is the same: it proves the owner and the workspace
        // against the row at dispatch, and only reduces work.
        let read = matches!(&command, WorkerCommand::Bots { request, .. }
            if super::lifecycle::bot_read_command(request)
                || matches!(request, BotCommand::StopTask { .. }));
        for (id, chat, revision, policy) in &proof.chats {
            let owner = self.repository.bot_for_chat(*chat).ok().flatten();
            match super::lifecycle::provable_bot(&self.repository, *id, &command) {
                Ok(Some(bot))
                    if (owner == Some(*id) || (owner.is_none() && destination == Some(*chat)))
                        && (read
                            || (bot.revision == *revision && bot.policy_revision == *policy)) => {}
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
            | BotCommand::ReadLibrary { bot_id }
            | BotCommand::SetKnowledge { bot_id, .. }
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
            | BotCommand::ListBotChats { bot_id, .. }
            | BotCommand::ReadRun { bot_id, .. }
            | BotCommand::ReadTaskAnswer { bot_id, .. }
            | BotCommand::StopTask { bot_id, .. } => *bot_id,
            BotCommand::List { .. } | BotCommand::ListLifecycle { .. } => unreachable!(),
            // The workspace alone is proved; the read names no Bot or chat.
            BotCommand::ListActivity { .. }
            | BotCommand::ListTrashedBotChats { .. }
            | BotCommand::ListTaskBots { .. } => {
                return Ok(Some(Vec::new()));
            }
            // A creation names no Bot that exists yet; its owner is proved.
            BotCommand::CreateBotWithRole { .. }
            | BotCommand::ReadBotCreation { .. }
            | BotCommand::ListBotCreations { .. }
            | BotCommand::RetryBotCreation { .. }
            | BotCommand::AbandonBotCreation { .. } => return Ok(Some(Vec::new())),
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
                    // A Run read proves the conversation it names when it is
                    // this Bot's; any other reads as not found at dispatch,
                    // never as someone else's.
                    BotCommand::ReadRun { chat_id, .. }
                        if repository.bot_for_chat(*chat_id).map_err(bot_failure)? == Some(id) =>
                    {
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

#[cfg(test)]
mod tests;
