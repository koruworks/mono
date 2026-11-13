// SPDX-License-Identifier: MPL-2.0
//! Shared task ancestry for the writer's point reads and the worker's message map.
use crate::{AiError, AiErrorClass, ChatId, ChatRole, Message, MessageId, MessageState};

/// Find the original adjacent user, preserving assistant-only tasks as None.
/// The caller's fallback is the selected assistant, never the oldest ancestor.
pub fn original_bot_task_user(
    selected: &Message,
    task: &str,
    mut by_id: impl FnMut(MessageId) -> Result<Option<Message>, AiError>,
    mut by_ordinal: impl FnMut(ChatId, u32) -> Result<Option<Message>, AiError>,
) -> Result<Option<Message>, AiError> {
    let invalid = || {
        AiError::new(
            AiErrorClass::Storage,
            "The Bot task ancestry is inconsistent.",
            false,
        )
    };
    if selected.role != ChatRole::Assistant {
        return Err(invalid());
    }
    let mut origin = selected.clone();
    while let Some(id) = origin.origin_message_id {
        let previous = by_id(id)?.ok_or_else(invalid)?;
        if previous.id != id
            || previous.chat_id != selected.chat_id
            || previous.role != ChatRole::Assistant
            || previous.ordinal >= origin.ordinal
        {
            return Err(invalid());
        }
        origin = previous;
    }
    let Some(ordinal) = origin.ordinal.checked_sub(1) else {
        return Ok(None);
    };
    let Some(user) = by_ordinal(selected.chat_id, ordinal)? else {
        return Ok(None);
    };
    if user.chat_id != selected.chat_id || user.ordinal != ordinal {
        return Err(invalid());
    }
    Ok((user.role == ChatRole::User
        && user.state == MessageState::Complete
        && user.markdown == task)
        .then_some(user))
}

#[cfg(test)]
mod tests;
