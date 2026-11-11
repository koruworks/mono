// SPDX-License-Identifier: MPL-2.0
//! Bounded owner reads for the Bots view (P1.8). Each runs on the AI writer
//! against ai.db only — no Store, delegate or model call — after the same
//! prepared owner proof every other Bot read uses.
//!
//! The one exception is `read_run` (B1): the writer binds the Run to the
//! conversation from ai.db, and a bounded job on the provider-admin pool
//! asks the execution owner for it, so the writer never waits on the
//! delegate. Nothing it reads is stored.
use super::*;
use koru_delegate_api::bots::{BotCommand as DelegateCommand, BotDelegatePort, BotReply};
use koru_delegate_api::wire::TimelineQuery;
use koru_delegate_core::BotRunKey;

/// A `read_run` the writer has checked and bound: what the job needs to
/// check authority again and ask the execution owner.
struct BotRunRead {
    authority: Arc<dyn BotAuthority>,
    owner: BotOwner,
    record_workspace: WorkspaceId,
    key: BotRunKey,
    window: TimelineQuery,
    delegate: Arc<dyn BotDelegatePort>,
}

/// One `read_run`, off the writer: authority again for the Run's record
/// workspace, then the execution owner's `Read`, answered to the caller
/// directly. Holds no writer state; nothing is persisted.
pub(in crate::worker) struct BotRunReadJob {
    read: BotRunRead,
    reply: SyncSender<ApiResult<BotResponse>>,
}

impl BotRunReadJob {
    pub(in crate::worker) fn run(self) {
        let answer = catch_unwind(AssertUnwindSafe(|| self.read.read()))
            .unwrap_or_else(|_| Err(bot_failure(BotError::Unavailable)));
        let _ = self.reply.send(answer);
    }

    /// Onto the bounded pool, or answered at once when it is full
    /// (`ai_worker_saturated`, retryable) or gone. The writer never waits.
    pub(in crate::worker) fn submit(self, pool: Option<&ProviderAdminPool>) {
        let Some(pool) = pool else {
            let _ = self.reply.send(Err(bot_failure(BotError::Unavailable)));
            return;
        };
        if let Err(error) = pool.try_submit(ProviderAdminJob::BotRunRead(self))
            && let ProviderAdminJob::BotRunRead(job) = error.job
        {
            let _ = job.reply.send(Err(error.failure));
        }
    }

    /// A read of `run` by `owner` for the pool tests, its authority and
    /// workspace given.
    #[cfg(test)]
    pub(in crate::worker) fn fixture(
        authority: Arc<dyn BotAuthority>,
        owner: BotOwner,
        workspace: WorkspaceId,
        key: BotRunKey,
        delegate: Arc<dyn BotDelegatePort>,
        reply: SyncSender<ApiResult<BotResponse>>,
    ) -> Self {
        Self {
            read: BotRunRead {
                authority,
                owner,
                record_workspace: workspace,
                key,
                window: TimelineQuery {
                    from: 0,
                    len: 10,
                    workspace: Some(workspace),
                },
                delegate,
            },
            reply,
        }
    }
}

impl BotRunRead {
    fn read(&self) -> ApiResult<BotResponse> {
        let checked = self
            .authority
            .inspect(&[self.record_workspace], &[])
            .map_err(bot_failure)?;
        if checked.owner != self.owner {
            return Err(bot_failure(BotError::Forbidden));
        }
        checked
            .workspace(self.record_workspace)
            .map_err(bot_failure)?;
        match self.delegate.execute(DelegateCommand::Read {
            key: self.key.clone(),
            window: self.window,
        }) {
            Ok(BotReply::Run { mut view }) if view.header.run == self.key.run => {
                // A read hands out no takeover or lease handle.
                view.execution.session = None;
                Ok(BotResponse::Run { view })
            }
            Ok(_) => Err(failure(
                "bot_unavailable",
                "The Bot's execution owner gave an unexpected answer.",
                false,
            )),
            Err(refused) => Err(run_read_failure(&refused)),
        }
    }
}

/// The execution owner's refusal of a `Read`, in the Bots API's codes: a Run
/// it does not hold is `not_found`; the immutable owner, device or Bot
/// binding (`bot_read_allowed`) is `wrong_owner`; anything else, such as the
/// owner not answering, is `bot_unavailable`, retryable as the owner said.
fn run_read_failure(refused: &koru_delegate_api::Failure) -> Failure {
    match refused.code.as_str() {
        "unknown_run" => not_found(),
        "bot_owner_mismatch" => bot_failure(BotError::Forbidden),
        _ => failure(
            "bot_unavailable",
            "The Bot's execution owner could not read the Run.",
            refused.retryable,
        ),
    }
}

impl WorkerState {
    /// `read_run`: checked and bound here, read off the writer. Every refusal
    /// is answered here; an accepted read is answered by its job.
    pub(in crate::worker) fn read_bot_run(
        &mut self,
        context: CommandContext,
        request: BotCommand,
        reply: SyncSender<ApiResult<BotResponse>>,
    ) {
        let read = match self.bot_run_read(&context, &request) {
            Ok(read) => read,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        BotRunReadJob { read, reply }.submit(self.provider_admin.as_ref());
    }

    /// The owner first, then the conversation's Trash, then the Bot's
    /// workspace, then the binding, as a named conversation read is checked
    /// (`guard_command`, `require_held_owner`): only the owner learns a
    /// conversation is held, and a Run that is not this reply's is
    /// `not_found`, never someone else's.
    fn bot_run_read(
        &self,
        context: &CommandContext,
        request: &BotCommand,
    ) -> ApiResult<BotRunRead> {
        let BotCommand::ReadRun {
            bot_id,
            chat_id,
            message_id,
            run_id,
            from,
            len,
        } = *request
        else {
            return Err(bot_failure(BotError::Invalid));
        };
        authorize(context, AiScope::ManageBots)?;
        if len == 0 || len > BOT_RUN_WINDOW_LIMIT {
            return Err(bot_failure(BotError::Invalid));
        }
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        let owner = proof.authority.owner().map_err(bot_failure)?;
        let (bot, standing) = self
            .repository
            .bot_any_lifecycle(bot_id)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        if (BotOwner {
            user_scope: bot.user_scope,
            device: bot.owner_device,
        }) != owner
        {
            return Err(bot_failure(BotError::Forbidden));
        }
        if self.repository.bot_for_chat(chat_id).map_err(bot_failure)? != Some(bot_id) {
            return Err(not_found());
        }
        // A conversation in Trash, on its own or with its Bot, is out of
        // every read; an archived Bot's history reads, and an unfinished
        // operation holds no read.
        let hold = match standing {
            BotLifecycle::Trashed => Some(koru_ai_sqlite::BotHold::BotTrashed),
            _ => self
                .repository
                .bot_chat_hold(chat_id)
                .map_err(bot_failure)?
                .filter(|hold| hold.hides()),
        };
        if let Some(hold) = hold {
            return Err(lifecycle::hold_failure(hold));
        }
        proof
            .authority
            .workspace(bot.home_workspace)
            .map_err(bot_failure)?;
        let record_workspace = self
            .repository
            .bound_bot_run(bot_id, chat_id, message_id, run_id)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        let unavailable = || {
            failure(
                "bot_unavailable",
                "The Bot's execution owner is not available.",
                true,
            )
        };
        let authority = self.bot_authority.clone().ok_or_else(unavailable)?;
        let delegate = self.bot_delegate.clone().ok_or_else(unavailable)?;
        Ok(BotRunRead {
            authority,
            owner,
            record_workspace,
            key: BotRunKey {
                owner: koru_delegate_core::BotOwner {
                    user_scope: owner.user_scope,
                    device: owner.device,
                },
                bot: bot_id,
                run: run_id,
            },
            window: TimelineQuery {
                from,
                len,
                workspace: Some(record_workspace),
            },
            delegate,
        })
    }

    pub(in crate::worker) fn bot_read_command(
        &self,
        context: &CommandContext,
        command: &BotCommand,
    ) -> Option<ApiResult<BotResponse>> {
        match command {
            BotCommand::ListBotChats {
                bot_id,
                cursor,
                limit,
            } => Some(self.list_bot_chats(context, *bot_id, cursor.as_ref(), *limit)),
            BotCommand::ListActivity { workspace } => {
                Some(self.list_bot_activity(context, *workspace))
            }
            BotCommand::ListTrashedBotChats {
                workspace,
                cursor,
                limit,
            } => Some(self.list_trashed_bot_chats(context, *workspace, cursor.as_ref(), *limit)),
            _ => None,
        }
    }

    /// Running and Needs-you conversations of the owner's active Bots whose
    /// home is `workspace`, which the owner must currently reach. Durable AI
    /// state only: nothing here asks the delegate, so a slow or absent
    /// delegate cannot hold the writer.
    fn list_bot_activity(
        &self,
        context: &CommandContext,
        workspace: WorkspaceId,
    ) -> ApiResult<BotResponse> {
        authorize(context, AiScope::ManageBots)?;
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        let owner = proof.authority.owner().map_err(bot_failure)?;
        proof.authority.workspace(workspace).map_err(bot_failure)?;
        let activity = self
            .repository
            .bot_activity(owner, workspace, BOT_ACTIVITY_LIMIT)
            .map_err(bot_failure)?;
        Ok(BotResponse::Activity { activity })
    }

    /// The owner's Bot conversations in Trash whose Bot lives in `workspace`,
    /// which the owner must currently reach, whatever each Bot's lifecycle,
    /// so a client that knows only the workspace can find each one and the
    /// operation to name in its Restore. ai.db only, like every read here.
    fn list_trashed_bot_chats(
        &self,
        context: &CommandContext,
        workspace: WorkspaceId,
        cursor: Option<&BotTrashedChatCursor>,
        limit: u16,
    ) -> ApiResult<BotResponse> {
        authorize(context, AiScope::ManageBots)?;
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        let owner = proof.authority.owner().map_err(bot_failure)?;
        proof.authority.workspace(workspace).map_err(bot_failure)?;
        let page = self
            .repository
            .trashed_bot_chat_page(owner, workspace, cursor, limit)
            .map_err(bot_failure)?;
        Ok(BotResponse::TrashedChats { page })
    }

    /// Fill in, for each archived or trashed Bot of a read the owner has
    /// already been authorized for, the completed operation that holds it
    /// there (`held_by`), so a client that knows only the workspace can name
    /// it in the Restore after a restart. One journal read serves the whole
    /// list; an active Bot is left without one.
    pub(super) fn with_lifecycle_holders(
        &self,
        owner: BotOwner,
        bots: &mut [Bot],
    ) -> ApiResult<()> {
        let inactive: Vec<_> = bots
            .iter()
            .filter(|bot| !bot.lifecycle.is_active())
            .map(|bot| (bot.id, bot.lifecycle))
            .collect();
        if inactive.is_empty() {
            return Ok(());
        }
        let mut holders = self
            .repository
            .bot_lifecycle_holders(owner, &inactive)
            .map_err(bot_failure)?;
        for bot in bots {
            bot.held_by = holders.remove(&bot.id);
        }
        Ok(())
    }

    /// The owner's view of one Bot's conversations. An archived Bot's stay
    /// listable by name (its history is kept in the Archived filter, as
    /// `bot_read_command` admits); a trashed Bot is `not_found`, as `read`
    /// answers it. A Bot of another owner or device, or one out of the
    /// caller's current reach, reads as refused before any conversation is
    /// looked at.
    fn list_bot_chats(
        &self,
        context: &CommandContext,
        bot_id: BotId,
        cursor: Option<&BotChatCursor>,
        limit: u16,
    ) -> ApiResult<BotResponse> {
        authorize(context, AiScope::ManageBots)?;
        let bot = self.readable_owned_bot(context, bot_id)?;
        let page = self
            .repository
            .bot_chat_page(bot.id, cursor, limit)
            .map_err(bot_failure)?;
        Ok(BotResponse::Chats { page })
    }
}
