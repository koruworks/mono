// SPDX-License-Identifier: MPL-2.0
//! Bot lifecycle commands and the holds they place (P1.7).
//!
//! The journal and its contract exist; the cross-owner drivers that apply
//! Archive, Trash, Restore and Purge land in later slices, so those commands
//! refuse explicitly until then rather than report a result that did not
//! happen. What is enforced already is the hold: a conversation in Trash, or
//! one an unfinished operation captured, refuses changes before any mutation
//! with a stable code, and never falls back to being an ordinary chat.
use super::*;
use koru_ai_sqlite::BotHold;
use koru_ai_wire::bot_lifecycle_codes;

impl WorkerState {
    pub(in crate::worker) fn bot_lifecycle_command(
        &mut self,
        context: &CommandContext,
        command: &BotCommand,
    ) -> Option<ApiResult<BotResponse>> {
        match command {
            BotCommand::ReadLifecycleOperation { request_id, bot_id } => {
                Some(self.read_bot_lifecycle(context, *request_id, *bot_id))
            }
            BotCommand::ListLifecycle {
                workspace,
                lifecycle,
            } => Some(self.list_bot_lifecycle(context, *workspace, *lifecycle)),
            _ if command.lifecycle_kind().is_some() => Some(Err(lifecycle_unavailable())),
            _ => self.bot_command_hold(context, command).err().map(Err),
        }
    }

    fn read_bot_lifecycle(
        &self,
        context: &CommandContext,
        request: BotRequestId,
        bot: BotId,
    ) -> ApiResult<BotResponse> {
        let owner = prepared_owner(context)?;
        // The journal is keyed by the trusted scope; an operation of another
        // Bot or device reads as missing, never as someone else's progress.
        let operation = self
            .repository
            .bot_lifecycle_operation(owner.user_scope, request)
            .map_err(bot_failure)?
            .filter(|operation| operation.bot == bot && operation.owner == owner)
            .ok_or_else(not_found)?;
        Ok(BotResponse::Lifecycle {
            operation: operation.progress(),
        })
    }

    /// The Archived or Trash filter: the owner's Bots of a workspace it may
    /// currently see, in that lifecycle. Active Bots stay with `list`.
    fn list_bot_lifecycle(
        &self,
        context: &CommandContext,
        workspace: WorkspaceId,
        lifecycle: BotLifecycle,
    ) -> ApiResult<BotResponse> {
        let owner = prepared_owner(context)?;
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        proof.authority.workspace(workspace).map_err(bot_failure)?;
        if lifecycle == BotLifecycle::Active {
            return Err(failure(
                "invalid_request",
                "Active Bots are listed by the list command.",
                false,
            ));
        }
        let bots = self
            .repository
            .bots_in_lifecycle(owner, workspace, lifecycle)
            .map_err(bot_failure)?;
        Ok(BotResponse::List { bots })
    }

    /// A Bot-level command on a held Bot, or a task naming a held
    /// conversation, refuses before it reaches its owner path.
    fn bot_command_hold(&self, context: &CommandContext, command: &BotCommand) -> ApiResult<()> {
        let (bot, chat) = match command {
            BotCommand::Start {
                bot_id, chat_id, ..
            }
            | BotCommand::Continue {
                bot_id, chat_id, ..
            } => (*bot_id, Some(*chat_id)),
            BotCommand::CreateChat { bot_id, .. }
            | BotCommand::Update { bot_id, .. }
            | BotCommand::UpdateAccess { bot_id, .. }
            | BotCommand::Pause { bot_id, .. }
            | BotCommand::Resume { bot_id, .. } => (*bot_id, None),
            _ => return Ok(()),
        };
        let hold = match chat {
            Some(chat) if self.repository.bot_for_chat(chat).map_err(bot_failure)? == Some(bot) => {
                self.repository.bot_chat_hold(chat)
            }
            _ => self.repository.bot_hold(bot),
        }
        .map_err(bot_failure)?;
        let Some(hold) = hold else {
            return Ok(());
        };
        // Ownership first: someone else's Bot never learns its lifecycle.
        if self.repository.bot_owner(bot).map_err(bot_failure)? != Some(prepared_owner(context)?) {
            return Err(bot_failure(BotError::Forbidden));
        }
        Err(hold_failure(hold))
    }
}

fn prepared_owner(context: &CommandContext) -> ApiResult<BotOwner> {
    authorize(context, AiScope::ManageBots)?;
    let proof = context
        .prepared
        .as_ref()
        .ok_or_else(|| bot_failure(BotError::Unavailable))?;
    proof.authority.owner().map_err(bot_failure)
}

/// Whether a held conversation refuses this command. A trashed one refuses
/// everything but Stop; a pending one refuses what would add to or change
/// it, while reads, denial and Stop stay available to settle it.
///
/// Attachment staging changes the conversation's own rows, so both holds
/// refuse every staging command (admission, a workspace copy, the row, its
/// removal) before any bytes are prepared. Reading a staged or sent
/// attachment (`ResolveAttachments`) is a read: a pending hold keeps it.
pub(super) fn hold_refuses(command: &WorkerCommand, hold: BotHold) -> bool {
    match hold {
        BotHold::Trashed => !matches!(command, WorkerCommand::CancelAttempt { .. }),
        BotHold::Pending => matches!(
            command,
            WorkerCommand::SendTurn { .. }
                | WorkerCommand::RetryTurn { .. }
                | WorkerCommand::SteerAttempt { .. }
                | WorkerCommand::BranchChat { .. }
                | WorkerCommand::UpdateChatModel { .. }
                | WorkerCommand::UpdateChatAgentSettings { .. }
                | WorkerCommand::ApproveToolCall { .. }
                | WorkerCommand::AttachmentAdmission { .. }
                | WorkerCommand::WorkspaceCopyAdmission { .. }
                | WorkerCommand::StageAttachment { .. }
                | WorkerCommand::RemoveStagedAttachment { .. }
        ),
    }
}

pub(super) fn hold_failure(hold: BotHold) -> Failure {
    match hold {
        BotHold::Trashed => failure(
            bot_lifecycle_codes::TRASHED,
            "This Bot conversation is in Trash. Restore it to use it again.",
            false,
        ),
        BotHold::Pending => failure(
            bot_lifecycle_codes::PENDING,
            "This Bot conversation is being archived, moved to Trash or restored.",
            false,
        ),
    }
}

fn lifecycle_unavailable() -> Failure {
    failure(
        "unsupported_capability",
        "Bot lifecycle changes are not available yet.",
        false,
    )
}
