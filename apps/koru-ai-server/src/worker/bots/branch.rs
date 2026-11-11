// SPDX-License-Identifier: MPL-2.0
//! Branch a Bot conversation into another conversation with the same Bot
//! (P1.6). It reuses the shared Branch copy and launches nothing: no attempt,
//! Run, replay position, tool activity or approval is copied or created.
//!
//! A prefix with attached files is committed in three steps, as the ordinary
//! Branch's is, with the destination reserved throughout: the folders are
//! copied on the cleanup thread; the owner's authority is inspected again on
//! the administration pool; and the writer fences the Bot snapshot, the
//! source prefix, steering and the destination before the one transaction.
//! Nothing that waits on disk or on the Store runs on the writer.
use super::*;
use crate::attachments::BranchFolder;

impl WorkerState {
    pub(in crate::worker) fn branch_bot_chat(
        &mut self,
        context: CommandContext,
        bot_id: BotId,
        source_chat: ChatId,
        request: BranchChatRequest,
    ) -> ApiResult<BranchPlan> {
        authorize(&context, AiScope::ManageBots)?;
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        let bot = self
            .repository
            .bot(bot_id)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        validate_owner(&proof.authority, &bot)?;
        Self::require_branch_title(&request)?;
        let destination = request.branch_chat_id;
        let origin = BranchOrigin {
            chat_id: source_chat,
            through_message_id: request.through_message_id,
        };
        require_bot_branch_destination(&self.repository, bot.id, destination)?;
        if self
            .repository
            .bot_for_chat(destination)
            .map_err(bot_failure)?
            == Some(bot.id)
        {
            // An exact retry finds the branch it made; any other reuse conflicts.
            let existing = self
                .repository
                .chat_with_access(destination, &ChatReadAccess::authorized(vec![destination]))
                .map_err(ai_failure)?
                .ok_or_else(not_found)?;
            if existing.branched_from.as_ref() != Some(&origin) || existing.title != request.title {
                return Err(destination_conflict());
            }
            return self
                .read_chat(context, destination)
                .map(|transcript| BranchPlan::Done(BranchChatResponse { transcript }));
        }
        // An acknowledged steer may still be writing its history row (O20).
        self.require_bot_steering_settled(source_chat)?;
        let access = self.bot_read_access(&context)?;
        let source = self
            .repository
            .chat_with_access(source_chat, &access)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        let profile = proof
            .authority
            .workspace(bot.home_workspace)
            .map_err(bot_failure)?;
        if source.profile_id != Some(profile) {
            return Err(bot_failure(BotError::Forbidden));
        }
        let now = Timestamp::now();
        let mut chat = Chat {
            id: destination,
            title: request.title,
            private: source.private,
            current_model: source.current_model,
            branched_from: Some(origin),
            agent_working_folder: source.agent_working_folder,
            agent_mode: None,
            agent_fast: None,
            pinned_at: None,
            profile_id: source.profile_id,
            created_at: now,
            updated_at: now,
        };
        seed_bot_branch_settings(&mut chat, &bot.config);
        let (copy, folders) = self.branch_copy(source_chat, chat)?;
        if !folders.is_empty() {
            // The files are copied off the writer first; the commit is then
            // fenced again under fresh authority (`complete_bot_branch`).
            return Ok(BranchPlan::BotCopy(Box::new(BotBranchCopy {
                bot,
                copy,
                folders,
            })));
        }
        self.repository
            .create_bot_branch(&bot, &copy)
            .map_err(bot_failure)?;
        self.read_chat(context, destination)
            .map(|transcript| BranchPlan::Done(BranchChatResponse { transcript }))
    }

    /// Reserve the destination and copy the folders on the cleanup thread.
    /// The reservation is held until the branch commits or is abandoned.
    pub(in crate::worker) fn begin_bot_branch_copy(
        &mut self,
        context: CommandContext,
        plan: Box<BotBranchCopy>,
        reply: SyncSender<ApiResult<BranchChatResponse>>,
    ) {
        self.branching.insert(plan.copy.chat.id);
        let folders = plan.folders.clone();
        let output = self.output.clone();
        let pending = PendingBotBranch {
            context,
            plan,
            reply,
        };
        self.cleanup.submit(CleanupJob::CopyBranch {
            folders,
            done: Box::new(move |copied| {
                let _ = output.send(Envelope::BotBranchCopied {
                    pending: Box::new(pending),
                    copied: copied.is_ok(),
                });
            }),
        });
    }

    /// The folders are copied: inspect the owner's authority again, off the
    /// writer, before anything is committed.
    pub(in crate::worker) fn on_bot_branch_copied(
        &mut self,
        pending: Box<PendingBotBranch>,
        copied: bool,
    ) {
        if !copied {
            // `copy_for_branch` is all or nothing: it removed what it made.
            self.branching.remove(&pending.plan.copy.chat.id);
            let _ = pending.reply.send(Err(failure(
                koru_ai_wire::attachment_codes::STORAGE,
                "The branch's attachments could not be copied.",
                true,
            )));
            return;
        }
        let (Some(authority), Some(pool)) = (self.bot_authority.clone(), &self.provider_admin)
        else {
            return self.abandon_bot_branch(*pending, bot_failure(BotError::Unavailable));
        };
        let job = BotBranchJob {
            pending: *pending,
            authority,
        };
        if let Err(error) = pool.try_submit(ProviderAdminJob::BotBranch(job))
            && let ProviderAdminJob::BotBranch(job) = error.job
        {
            self.abandon_bot_branch(job.pending, error.failure);
        }
    }

    /// Fence on the writer, then commit the copy and its association in one
    /// transaction, or abandon it and remove only what it copied.
    pub(in crate::worker) fn complete_bot_branch(&mut self, prepared: BotBranchPrepared) {
        let BotBranchPrepared { pending, authority } = prepared;
        let context =
            match authority.and_then(|authority| self.fence_bot_branch(&pending, authority)) {
                Ok(context) => context,
                Err(error) => return self.abandon_bot_branch(pending, error),
            };
        let PendingBotBranch { plan, reply, .. } = pending;
        let destination = plan.copy.chat.id;
        let answer = match self.repository.create_bot_branch(&plan.bot, &plan.copy) {
            Ok(()) => Ok(()),
            // An error does not prove nothing was written: what is stored decides.
            Err(error) => {
                if settle_uncommitted(&self.repository, &self.cleanup, &plan.bot, &plan.copy) {
                    Ok(())
                } else {
                    Err(bot_failure(error))
                }
            }
        };
        self.branching.remove(&destination);
        let _ = reply.send(answer.and_then(|()| {
            self.read_chat(context, destination)
                .map(|transcript| BranchChatResponse { transcript })
        }));
    }

    /// Every check the first step made, made again: the copy may have taken
    /// long enough for any of them to change. The proof returned reads the
    /// committed branch; it names only this Bot's source and destination.
    fn fence_bot_branch(
        &self,
        pending: &PendingBotBranch,
        authority: Arc<dyn BotAuthority>,
    ) -> ApiResult<CommandContext> {
        authorize(&pending.context, AiScope::ManageBots)?;
        let BotBranchCopy { bot, copy, folders } = &*pending.plan;
        validate_owner(&authority, bot)?;
        let profile = authority
            .workspace(bot.home_workspace)
            .map_err(bot_failure)?;
        if copy.chat.profile_id != Some(profile) {
            return Err(bot_failure(BotError::Forbidden));
        }
        if self.is_hidden(copy.chat.profile_id) {
            return Err(not_found());
        }
        // The Bot the copy was seeded from, unedited while its files copied.
        // `create_bot_branch` fences the same snapshot inside its transaction.
        let current = self
            .repository
            .bot(bot.id)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        if (
            current.revision,
            current.policy_revision,
            current.home_workspace,
        ) != (bot.revision, bot.policy_revision, bot.home_workspace)
        {
            return Err(bot_failure(BotError::RevisionConflict));
        }
        let origin = copy.chat.branched_from.as_ref().ok_or_else(not_found)?;
        if self
            .repository
            .bot_for_chat(origin.chat_id)
            .map_err(bot_failure)?
            != Some(bot.id)
        {
            return Err(not_found());
        }
        // An acknowledgement can land while the files copy (O20).
        self.require_bot_steering_settled(origin.chat_id)?;
        self.require_unchanged_prefix(origin, copy, folders)?;
        let destination = copy.chat.id;
        if self
            .repository
            .bot_for_chat(destination)
            .map_err(bot_failure)?
            .is_some()
            || self
                .repository
                .chat(destination)
                .map_err(ai_failure)?
                .is_some()
        {
            return Err(destination_conflict());
        }
        let chats = [origin.chat_id, destination]
            .into_iter()
            .map(|chat| (bot.id, chat, bot.revision, bot.policy_revision))
            .collect();
        Ok(CommandContext {
            request: pending.context.request.clone(),
            prepared: Some(Arc::new(PreparedBotCommand {
                authority,
                chats,
                runtime: Ok(()),
                pins: vec![],
                chat_snapshot: None,
                settings: None,
                terminal_question: None,
            })),
        })
    }

    /// The source still reads through the same messages and attachments the
    /// copy was made from, in the same order.
    fn require_unchanged_prefix(
        &self,
        origin: &BranchOrigin,
        copy: &BranchCopy,
        folders: &[BranchFolder],
    ) -> ApiResult<()> {
        let changed = || bot_failure(BotError::RevisionConflict);
        let messages = self
            .repository
            .messages(origin.chat_id)
            .map_err(ai_failure)?;
        let through = messages
            .iter()
            .find(|message| message.id == origin.through_message_id)
            .filter(|message| message.state.is_terminal())
            .ok_or_else(changed)?;
        let mut prefix = koru_ai_core::read_through(&messages, through.id).unwrap_or_default();
        prefix.sort_by_key(|message| message.ordinal);
        if !prefix.iter().map(|message| Some(message.id)).eq(copy
            .messages
            .iter()
            .map(|message| message.origin_message_id))
        {
            return Err(changed());
        }
        let ids: HashSet<MessageId> = prefix.iter().map(|message| message.id).collect();
        let source: Vec<_> = self
            .repository
            .attachment_records(origin.chat_id)
            .map_err(ai_failure)?
            .into_iter()
            .filter(|record| {
                record
                    .attachment
                    .message_id
                    .is_some_and(|message| ids.contains(&message))
            })
            .collect();
        let mut stored: Vec<_> = source
            .iter()
            .filter(|record| record.attachment.source != koru_ai_core::AttachmentSource::Path)
            .map(|record| record.attachment.id)
            .collect();
        let mut copied: Vec<_> = folders.iter().map(|folder| folder.from.1).collect();
        stored.sort();
        copied.sort();
        if source.len() != copy.attachments.len() || stored != copied {
            return Err(changed());
        }
        Ok(())
    }

    /// Give up on a copy that did not commit: release the destination and
    /// remove only what the copy itself made.
    fn abandon_bot_branch(&mut self, pending: PendingBotBranch, error: Failure) {
        let PendingBotBranch { plan, reply, .. } = pending;
        settle_uncommitted(&self.repository, &self.cleanup, &plan.bot, &plan.copy);
        self.branching.remove(&plan.copy.chat.id);
        let _ = reply.send(Err(error));
    }
}

/// A Bot branch whose attachment folders must be copied before it commits,
/// with the Bot snapshot its copy was seeded from.
pub(in crate::worker) struct BotBranchCopy {
    bot: Bot,
    copy: BranchCopy,
    folders: Vec<BranchFolder>,
}

/// A Bot branch from its folder copy to its commit.
pub(in crate::worker) struct PendingBotBranch {
    context: CommandContext,
    plan: Box<BotBranchCopy>,
    reply: SyncSender<ApiResult<BranchChatResponse>>,
}

/// The owner's authority inspected again, on the administration pool.
pub(in crate::worker) struct BotBranchJob {
    pending: PendingBotBranch,
    authority: Arc<dyn BotAuthority>,
}

pub(in crate::worker) struct BotBranchPrepared {
    pending: PendingBotBranch,
    authority: ApiResult<Arc<dyn BotAuthority>>,
}

impl BotBranchJob {
    pub(in crate::worker) fn run(self) -> BotBranchPrepared {
        let workspace = self.pending.plan.bot.home_workspace;
        let authority = catch_unwind(AssertUnwindSafe(|| {
            self.authority
                .inspect(&[workspace], &[])
                .map(|proof| Arc::new(proof) as Arc<dyn BotAuthority>)
                .map_err(bot_failure)
        }))
        .unwrap_or_else(|_| Err(bot_failure(BotError::Unavailable)));
        BotBranchPrepared {
            pending: self.pending,
            authority,
        }
    }
}

/// After a Bot copy failed to commit, or may have: what is stored decides.
/// The destination is this copy's committed branch when it holds the copy's
/// association, origin, title and every one of its fresh attachment rows;
/// then every folder stays and this answers `true`. Otherwise only the copy's
/// fresh identities that no row names are removed: never a chat's folder,
/// never the source's files, and nothing at all when a read fails (the
/// start-up sweep takes what no row names).
pub(super) fn settle_uncommitted(
    repository: &Repository,
    cleanup: &crate::attachments::Cleanup,
    bot: &Bot,
    copy: &BranchCopy,
) -> bool {
    let destination = copy.chat.id;
    let committed = (|| -> Option<bool> {
        if repository.bot_for_chat(destination).ok()? != Some(bot.id) {
            return Some(false);
        }
        let stored = repository
            .chat_with_access(destination, &ChatReadAccess::authorized(vec![destination]))
            .ok()?;
        if !stored.is_some_and(|chat| {
            chat.branched_from == copy.chat.branched_from && chat.title == copy.chat.title
        }) {
            return Some(false);
        }
        for record in &copy.attachments {
            match repository.attachment(record.attachment.id).ok()? {
                Some(row) if row.chat_id == destination => {}
                _ => return Some(false),
            }
        }
        Some(true)
    })();
    let unnamed = match committed {
        Some(true) => return true,
        Some(false) => copy
            .attachments
            .iter()
            .filter(|record| record.attachment.source != koru_ai_core::AttachmentSource::Path)
            .map(|record| {
                repository.attachment(record.attachment.id).map(|row| {
                    row.is_none()
                        .then_some((record.chat_id, record.attachment.id))
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .ok(),
        None => None,
    };
    match unnamed {
        Some(unnamed) => {
            let unnamed: Vec<_> = unnamed.into_iter().flatten().collect();
            if !unnamed.is_empty() {
                cleanup.submit(CleanupJob::RemoveAttachments(unnamed));
            }
        }
        None => tracing::warn!(
            target: "koru_ai::attachments",
            chat_id = %destination,
            "a Bot branch's copied files were left for the start-up sweep: its rows could not be read"
        ),
    }
    false
}

/// O24: a Bot branch keeps the source's model and reasoning, as every branch
/// does, but starts in the Bot's current mode and Fast setting instead of the
/// provider defaults an ordinary branch resets to. This is the only Bot-specific
/// seeding; removing it restores the shared reset.
fn seed_bot_branch_settings(chat: &mut Chat, config: &BotConfig) {
    chat.agent_mode = config.mode.clone();
    chat.agent_fast = config.fast;
}

/// Ordinary-safe Chat reads hide Bot rows, so a destination is checked by its
/// raw association: an ordinary source keeps the ordinary rule, and a Bot
/// source may only reach an unused identity or its own earlier branch.
pub(super) fn require_branch_destination(
    repository: &Repository,
    source: ChatId,
    destination: ChatId,
) -> ApiResult<()> {
    match repository.bot_for_chat(source).map_err(bot_failure)? {
        None => {
            super::super::attachments::require_ordinary_branch_destination(repository, destination)
        }
        Some(bot) => require_bot_branch_destination(repository, bot, destination),
    }
}

fn require_bot_branch_destination(
    repository: &Repository,
    bot: BotId,
    destination: ChatId,
) -> ApiResult<()> {
    match repository.bot_for_chat(destination).map_err(bot_failure)? {
        // Even an exact earlier branch is held while it is in Trash or moving.
        Some(owner) if owner == bot => {
            match repository.bot_chat_hold(destination).map_err(bot_failure)? {
                Some(hold) => Err(super::lifecycle::hold_failure(hold)),
                None => Ok(()),
            }
        }
        Some(_) => Err(destination_conflict()),
        None if repository.chat(destination).map_err(ai_failure)?.is_some() => {
            Err(destination_conflict())
        }
        None => Ok(()),
    }
}

fn destination_conflict() -> Failure {
    failure(
        "idempotency_conflict",
        "This chat identity was already used for another branch.",
        false,
    )
}
