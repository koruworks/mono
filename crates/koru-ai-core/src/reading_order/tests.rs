// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{ChatId, MessageState, Timestamp};

/// A chat built row by row, as the store writes it.
struct Chat {
    id: ChatId,
    messages: Vec<Message>,
}

impl Chat {
    fn new() -> Self {
        Self {
            id: ChatId::new(),
            messages: Vec::new(),
        }
    }

    fn row(&mut self, role: ChatRole, markdown: &str, origin: Option<MessageId>) -> MessageId {
        let ordinal = self.messages.len() as u32;
        let at = Timestamp::from_millis(i64::from(ordinal) + 1);
        let message = Message {
            id: MessageId::new(),
            chat_id: self.id,
            ordinal,
            role,
            state: MessageState::Complete,
            markdown: markdown.to_owned(),
            origin_message_id: origin,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: at,
            completed_at: Some(at),
        };
        let id = message.id;
        self.messages.push(message);
        id
    }

    /// `begin_turn`: the question and its reply, together.
    fn turn(&mut self, question: &str, reply: &str) -> MessageId {
        self.row(ChatRole::User, question, None);
        self.row(ChatRole::Assistant, reply, None)
    }

    /// `append_user_message`: a message the running reply took.
    fn steer(&mut self, markdown: &str) {
        self.row(ChatRole::User, markdown, None);
    }

    fn read(&self) -> Vec<&str> {
        reading_order(&self.messages)
            .into_iter()
            .map(|message| message.markdown.as_str())
            .collect()
    }
}

/// **A steer reads under the question it steered**, before the reply's work
/// and answer — though it is stored after the reply.
#[test]
fn a_steer_reads_between_its_question_and_the_reply() {
    let mut chat = Chat::new();
    chat.turn("Today's news?", "Here are the headlines");
    chat.steer("In the US");
    assert_eq!(
        chat.read(),
        ["Today's news?", "In the US", "Here are the headlines"]
    );
}

/// Two steers keep the order they were sent in, and the next turn reads
/// after the reply as it always did.
#[test]
fn two_steers_keep_their_order_and_the_next_turn_follows() {
    let mut chat = Chat::new();
    chat.turn("Plan the launch", "A plan");
    chat.steer("first");
    chat.steer("second");
    chat.turn("And the budget?", "A budget");
    assert_eq!(
        chat.read(),
        [
            "Plan the launch",
            "first",
            "second",
            "A plan",
            "And the budget?",
            "A budget"
        ]
    );
}

/// A message that was not taken by the reply — Steer that fell back to
/// stop-and-send, or a queued message sent after the reply — is a turn of
/// its own and stays where it is.
#[test]
fn a_new_turn_after_a_reply_is_not_moved() {
    let mut chat = Chat::new();
    chat.turn("Question", "Stopped reply");
    chat.turn("Sent after the stop", "Its own reply");
    assert_eq!(
        chat.read(),
        [
            "Question",
            "Stopped reply",
            "Sent after the stop",
            "Its own reply"
        ]
    );
}

/// A retry is another reply to an earlier question, so the steer before it
/// still belongs to the reply it went into.
#[test]
fn a_retry_after_a_steer_does_not_make_it_a_question() {
    let mut chat = Chat::new();
    let failed = chat.turn("Question", "Failed reply");
    chat.steer("steered");
    chat.row(ChatRole::Assistant, "Retried reply", Some(failed));
    assert_eq!(
        chat.read(),
        ["Question", "steered", "Failed reply", "Retried reply"]
    );
}

/// A branch copies every row with `origin_message_id` naming the source
/// chat's message, which is not in the branch: its turns still read as
/// turns, and its steers as steers.
#[test]
fn a_branch_reads_the_same_way() {
    let mut chat = Chat::new();
    chat.turn("Question", "Reply");
    chat.steer("steered");
    chat.turn("Next", "Next reply");
    for message in &mut chat.messages {
        message.origin_message_id = Some(MessageId::new());
    }
    assert_eq!(
        chat.read(),
        ["Question", "steered", "Reply", "Next", "Next reply"]
    );
}

/// Reading through a reply brings the messages steered into it, and stops
/// before the next turn.
#[test]
fn reading_through_a_reply_brings_its_steers() {
    let mut chat = Chat::new();
    let reply = chat.turn("Question", "Reply");
    chat.steer("steered");
    chat.turn("Next", "Next reply");
    let through: Vec<_> = read_through(&chat.messages, reply)
        .unwrap()
        .into_iter()
        .map(|message| message.markdown.as_str())
        .collect();
    assert_eq!(through, ["Question", "steered", "Reply"]);
    assert!(read_through(&chat.messages, MessageId::new()).is_none());
}

/// A person's message with nothing before it, or after another of theirs
/// with no reply between, is never moved.
#[test]
fn a_message_with_no_reply_before_it_stays() {
    let mut chat = Chat::new();
    chat.steer("alone");
    chat.steer("again");
    assert_eq!(chat.read(), ["alone", "again"]);
}
