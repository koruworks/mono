// SPDX-License-Identifier: MPL-2.0
//! Renaming an owned chat uses the same native thread authority as its history.
use super::*;

pub(super) fn run(
    thread: ChatId,
    title: &str,
    mut call: impl FnMut(C) -> Result<R, koru_ai_wire::Failure>,
) -> Result<koru_ai_core::Chat, koru_ai_wire::Failure> {
    let R::Read { snapshot } = call(C::Read { thread })? else {
        return Err(malformed());
    };
    if snapshot.conversation.chat.id != thread {
        return Err(malformed());
    }
    let id = uuid::Uuid::new_v4();
    let R::Changed { request, .. } = call(C::Rename {
        id,
        thread,
        revision: snapshot.revision,
        title: title.into(),
    })?
    else {
        return Err(malformed());
    };
    if request != id {
        return Err(malformed());
    }
    // Return the authoritative title and timestamp, including a newer rename
    // that may have won after this receipt. Never synthesize an AI chat row.
    let R::Read { snapshot } = call(C::Read { thread })? else {
        return Err(malformed());
    };
    if snapshot.conversation.chat.id != thread {
        return Err(malformed());
    }
    Ok(snapshot.conversation.chat)
}

#[cfg(test)]
mod tests;
