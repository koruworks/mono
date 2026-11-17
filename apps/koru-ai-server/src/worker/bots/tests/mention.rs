// SPDX-License-Identifier: MPL-2.0
//! **A Bot mentioned in a message.** The message being sent to a Bot tells
//! its agent, in words, that the person asks it to ask the mentioned Bot;
//! an ordinary chat reads the mention as a name; a retried task says what
//! the first send said. The stored text keeps the link throughout: nothing
//! here writes, and nothing here asks anybody.
use super::*;
use crate::worker::bots::chat::render_sent_message;

const CFO: &str = "0b8f6f0e-3c5e-4d0a-9a51-7d5b7c1f2a11";

fn sent() -> String {
    format!(
        "Ask [[{}]] what the Q3 budget is",
        koru_link::bot_link(CFO.parse().unwrap(), "CFO")
    )
}

fn sentence() -> String {
    format!(
        "[The person mentioned @CFO here: they ask you to ask the Bot named \"CFO\" (bot_id {CFO}) with the ask_bot tool, and to give them the final answer yourself.]"
    )
}

/// A message sent to a Bot's conversation carries the request sentence; the
/// same message sent to an ordinary chat is the name and nothing else.
#[test]
fn bot_mention_is_a_request_only_in_a_message_to_a_bot() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority, command);
    let markdown = sent();

    let to_the_bot = render_sent_message(&repo, bot.main_chat, &markdown);
    assert_eq!(
        to_the_bot,
        format!("Ask @CFO what the Q3 budget is\n\n{}", sentence())
    );

    // A chat that is nobody's Bot: the ordinary AI chat.
    let ordinary = render_sent_message(&repo, ChatId::new(), &markdown);
    assert_eq!(ordinary, "Ask @CFO what the Q3 budget is");
    assert!(!ordinary.contains("ask_bot"));
    assert!(!ordinary.contains(CFO));
}

/// A message with no mention reads the same to a Bot as to anybody, and the
/// same as before a Bot could be mentioned.
#[test]
fn bot_mention_absent_leaves_a_message_to_a_bot_as_it_was() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority, command);
    for markdown in [
        "CURRENT-TASK",
        "Use [[koru://skill/review-pr]] on this",
        "an @name typed by hand, and `code`",
    ] {
        assert_eq!(
            render_sent_message(&repo, bot.main_chat, markdown),
            koru_ai_core::render_prompt_text(markdown)
        );
    }
}

/// Rendering changes nothing that is stored: the message keeps its link,
/// which is what the Run's objective is made from.
#[test]
fn bot_mention_rendering_leaves_the_stored_text_alone() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority, command);
    let markdown = sent();
    let before = markdown.clone();
    let _ = render_sent_message(&repo, bot.main_chat, &markdown);
    assert_eq!(markdown, before);
    assert!(markdown.contains(&format!("[[koru://bot/{CFO}#CFO]]")));
}
