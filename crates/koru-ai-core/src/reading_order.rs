// SPDX-License-Identifier: MPL-2.0

//! **The order a person reads a chat in** (koru-ai D99).
//!
//! A chat's messages are stored in the order they were written. A message
//! steered into a running reply (D57) is written once the agent took it,
//! after the reply's own row, which already existed — so in stored order it
//! lands under an answer that was written knowing it. A person reads a steer
//! as part of what they asked: the question, what they added while the agent
//! worked, then the work and the answer.
//!
//! Nothing stored says "steer"; the shape does, and no row changes. A turn's
//! question is always written together with its reply, the reply the very
//! next row ([`AiPersistence::begin_turn`]); a steer is written alone
//! ([`AiPersistence::append_user_message`]). So a person's message that
//! follows a reply and has no fresh reply of its own was sent into that
//! reply. A retry — its `origin_message_id` names a message of the same chat
//! — is another reply to an earlier question, never the reply to the message
//! just before it.
//!
//! [`AiPersistence::begin_turn`]: crate::AiPersistence::begin_turn
//! [`AiPersistence::append_user_message`]: crate::AiPersistence::append_user_message

use std::collections::HashSet;

use crate::{ChatRole, Message, MessageId};

/// `messages`, in stored (ordinal) order, as a person reads them: each
/// message steered into a reply moved in front of that reply, after the
/// question and any earlier steer of it, in the order they were sent.
/// Everything else keeps its place.
pub fn reading_order(messages: &[Message]) -> Vec<&Message> {
    let ids: HashSet<MessageId> = messages.iter().map(|message| message.id).collect();
    let mut order: Vec<&Message> = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate() {
        // The reply it went into is the last message placed: a steer before
        // it was put in front of it, not after.
        let steer = message.role == ChatRole::User
            && !has_own_reply(messages, index, &ids)
            && order
                .last()
                .is_some_and(|last| last.role == ChatRole::Assistant);
        if steer {
            order.insert(order.len() - 1, message);
        } else {
            order.push(message);
        }
    }
    order
}

/// The messages up to and including `through`, in reading order — a reply
/// brings the messages steered into it. `None` when `through` is not one of
/// them.
pub fn read_through(messages: &[Message], through: MessageId) -> Option<Vec<&Message>> {
    let mut order = reading_order(messages);
    let end = order.iter().position(|message| message.id == through)?;
    order.truncate(end + 1);
    Some(order)
}

/// The next message is a reply written for the one at `index`: a reply that
/// is not a retry of an earlier one.
fn has_own_reply(messages: &[Message], index: usize, ids: &HashSet<MessageId>) -> bool {
    messages.get(index + 1).is_some_and(|next| {
        next.role == ChatRole::Assistant
            && !next
                .origin_message_id
                .is_some_and(|origin| ids.contains(&origin))
    })
}

#[cfg(test)]
mod tests;
