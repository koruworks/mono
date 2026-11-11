// SPDX-License-Identifier: MPL-2.0
//! Bot lifecycle commands (P1.7). The journal and its contract exist; the
//! cross-owner drivers that apply Archive, Trash, Restore and Purge land in
//! later slices, so those commands refuse explicitly until then rather than
//! report a result that did not happen.
use super::*;

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
            BotCommand::ListLifecycle { .. } => Some(Err(lifecycle_unavailable())),
            _ if command.lifecycle_kind().is_some() => Some(Err(lifecycle_unavailable())),
            _ => None,
        }
    }

    fn read_bot_lifecycle(
        &self,
        context: &CommandContext,
        request: BotRequestId,
        bot: BotId,
    ) -> ApiResult<BotResponse> {
        authorize(context, AiScope::ManageBots)?;
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        let owner = proof.authority.owner().map_err(bot_failure)?;
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
}

fn lifecycle_unavailable() -> Failure {
    failure(
        "unsupported_capability",
        "Bot lifecycle changes are not available yet.",
        false,
    )
}
