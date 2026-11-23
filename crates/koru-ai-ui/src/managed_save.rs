// SPDX-License-Identifier: MPL-2.0
//! **Save a Bot reply as a document.** The name prompt is the ordinary
//! chat's; the save is the engine's: the gateway reads its own copy of the
//! published reply and makes the page in the Bot's workspace. One request
//! and one page are chosen per save, and an uncertain answer keeps both,
//! so saving again under the same name finds the page instead of making a
//! second one.
use super::*;
use koru_ai_wire::bot_threads::{Command as C, Response as R};
use uuid::Uuid;

impl ChatSurface {
    /// Where a published Bot reply sits in its thread; `None` for anything
    /// that is not one (a person's message, a reply still being written).
    pub(crate) fn bot_reply_sequence(&self, message: MessageId) -> Option<u64> {
        let state = self.managed.as_ref()?;
        state
            .messages
            .values()
            .find(|m| {
                m.id == message.as_uuid() && m.kind == "words" && !m.sender.starts_with("person:")
            })
            .map(|m| m.sequence)
    }

    pub(crate) fn save_bot_reply(&mut self, message: MessageId) -> bool {
        if let Some(handled) = self.save_review_result(message) {
            return handled;
        }
        let title = self.save_name.value().trim().to_owned();
        let Some(sequence) = self.bot_reply_sequence(message) else {
            self.destination_open = None;
            return false;
        };
        let Some(state) = self.managed.as_mut() else {
            return false;
        };
        let thread = state.thread();
        let title = if title.is_empty() {
            state.snapshot.conversation.chat.title.clone()
        } else {
            title
        };
        let command = match &state.save {
            Some(
                kept @ C::SaveReply {
                    message: kept_message,
                    title: kept_title,
                    ..
                },
            ) if *kept_message == message.as_uuid() && *kept_title == title => kept.clone(),
            _ => C::SaveReply {
                id: Uuid::new_v4(),
                thread,
                message: message.as_uuid(),
                sequence,
                title,
                document: Uuid::new_v4(),
            },
        };
        state.save = Some(command.clone());
        self.destination_open = None;
        self.focus = Focus::Composer;
        self.send_command(Command::Engine { thread, command })
    }

    /// The save's answer: told the way an ordinary chat's save is, with
    /// its *Open*. An answer to a save no longer in flight is not shown.
    pub(crate) fn saved_bot_reply(&mut self, thread: ChatId, reply: &R) -> bool {
        let R::Saved {
            request,
            document,
            title,
            ..
        } = reply
        else {
            return false;
        };
        let Some(state) = self.managed.as_mut() else {
            return false;
        };
        if state.thread() != thread
            || !matches!(&state.save, Some(C::SaveReply { id, .. }) if id == request)
        {
            return false;
        }
        state.save = None;
        self.apply_update(Update::Exported(
            title.clone(),
            Some(koru_ai_core::DocumentId::from_uuid(*document)),
        ))
    }

    /// A refused save is forgotten, so the next one is a new request; an
    /// uncertain one is kept for saving again.
    pub(crate) fn failed_bot_save(
        &mut self,
        thread: ChatId,
        command: &C,
        failure: &koru_ai_wire::Failure,
    ) -> bool {
        let C::SaveReply { id, .. } = command else {
            return false;
        };
        let words = self.failure_words(failure);
        let Some(state) = self.managed.as_mut() else {
            return false;
        };
        if state.thread() != thread
            || !matches!(&state.save, Some(C::SaveReply { id: kept, .. }) if kept == id)
        {
            return true;
        }
        if !failure.retryable {
            state.save = None;
        }
        self.status = Some(words);
        true
    }
}

#[cfg(test)]
mod tests;
