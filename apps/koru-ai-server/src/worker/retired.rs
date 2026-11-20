// SPDX-License-Identifier: MPL-2.0

//! Retired Bot conversations remain stored but cannot enter ordinary AI routes.
//! Only the ownership index is consulted; no Bot payload or recovery is read.

use super::removed::Named;
use super::*;

pub(super) fn guard(repository: &Repository, command: WorkerCommand) -> Option<WorkerCommand> {
    let checked = (|| -> ApiResult<()> {
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
        let chat = match command.named() {
            Some(Named::Chat(chat)) | Some(Named::NewChat { chat, .. }) => Some(chat),
            Some(Named::Attempt(id)) => repository
                .attempt(id)
                .map_err(ai_failure)?
                .and_then(|row| row.chat_id),
            Some(Named::Message(id)) => repository
                .message(id)
                .map_err(ai_failure)?
                .map(|row| row.chat_id),
            Some(Named::Tool(id)) => repository
                .tool_activity(id)
                .map_err(ai_failure)?
                .map(|row| row.chat_id),
            Some(Named::Export(id)) => repository
                .export(id)
                .map_err(ai_failure)?
                .map(|row| row.chat_id),
            None => None,
        };
        if let Some(chat) = chat
            && repository.is_retired_bot_chat(chat).map_err(ai_failure)?
        {
            return Err(not_found());
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
