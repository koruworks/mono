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

    /// The owner's view of one active Bot's conversations. A Bot of another
    /// owner or device, or one out of the caller's current reach, reads as
    /// refused before any conversation is looked at.
    fn list_bot_chats(
        &self,
        context: &CommandContext,
        bot_id: BotId,
        cursor: Option<&BotChatCursor>,
        limit: u16,
    ) -> ApiResult<BotResponse> {
        authorize(context, AiScope::ManageBots)?;
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
        let page = self
            .repository
            .bot_chat_page(bot.id, cursor, limit)
            .map_err(bot_failure)?;
        Ok(BotResponse::Chats { page })
    }
}
