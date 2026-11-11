// SPDX-License-Identifier: MPL-2.0
//! Bounded owner reads for the Bots view (P1.8). Each runs on the AI writer
//! against ai.db only — no Store, delegate or model call — after the same
//! prepared owner proof every other Bot read uses.
use super::*;

impl WorkerState {
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
