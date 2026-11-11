// SPDX-License-Identifier: MPL-2.0

use koru_ai_core::bots::{BotChatPurpose, BotId};

use super::*;

fn label(bot: BotId) -> BotChatLabel {
    BotChatLabel {
        bot_id: bot,
        bot_name: "Scout".into(),
        purpose: BotChatPurpose::Additional,
    }
}

/// **One read per conversation while it is on its way**, answered off the
/// window thread: a labelled answer is a Bot's, an unlabelled one ordinary.
#[test]
fn a_read_learns_whose_a_conversation_is_and_is_asked_once() {
    let (link, commands, answers) = BotsLink::test_pair();
    let mut labels = ChatLabels::with_link(link);
    let (bot_chat, plain) = (ChatId::new(), ChatId::new());
    let bot = BotId::mint();
    assert!(labels.ask(bot_chat, None));
    assert!(labels.ask(bot_chat, None), "asked already: still owed");
    assert!(labels.ask(plain, None));
    assert_eq!(labels.work().commands, 2);
    assert!(labels.awaiting());
    let sent: Vec<ChatId> = commands
        .try_iter()
        .map(|command| match command {
            Command::Label { chat } => chat,
            _ => panic!("only label reads"),
        })
        .collect();
    assert_eq!(sent, vec![bot_chat, plain]);
    answers
        .send(Answer::Labelled {
            chat: bot_chat,
            result: Ok(Some(label(bot))),
        })
        .unwrap();
    answers
        .send(Answer::Labelled {
            chat: plain,
            result: Ok(None),
        })
        .unwrap();
    let found = labels.poll();
    assert_eq!(
        found,
        vec![
            (
                bot_chat,
                Resolved::Known(Known::Bot {
                    bot,
                    name: "Scout".into()
                })
            ),
            (plain, Resolved::Known(Known::Ordinary)),
        ]
    );
    assert!(!labels.awaiting());
    assert_eq!(labels.known(plain), Some(&Known::Ordinary));
}

/// **Not found is ordinary; anything else is not known** and is not
/// remembered, so it is asked again.
#[test]
fn not_found_reads_as_ordinary_and_a_failure_is_asked_again() {
    let (link, commands, answers) = BotsLink::test_pair();
    let mut labels = ChatLabels::with_link(link);
    let (gone, unreachable) = (ChatId::new(), ChatId::new());
    labels.ask(gone, None);
    labels.ask(unreachable, None);
    answers
        .send(Answer::Labelled {
            chat: gone,
            result: Err(Failure::Refused("not_found".into())),
        })
        .unwrap();
    answers
        .send(Answer::Labelled {
            chat: unreachable,
            result: Err(Failure::lost()),
        })
        .unwrap();
    let found = labels.poll();
    assert_eq!(found[0], (gone, Resolved::Known(Known::Ordinary)));
    assert_eq!(found[1], (unreachable, Resolved::Unavailable));
    assert_eq!(labels.known(unreachable), None);
    let _ = commands.try_iter().count();
    assert!(labels.ask(unreachable, None));
    assert_eq!(commands.try_iter().count(), 1, "asked again");
}

/// **Without a link nothing is asked**, and a label learned from a Bot read
/// is never undone by a reader that cannot see Bot labels.
#[test]
fn no_link_asks_nothing_and_a_learned_label_is_kept() {
    let mut labels = ChatLabels::default();
    let chat = ChatId::new();
    assert!(!labels.ask(chat, None), "no link under test");
    assert!(!labels.awaiting());
    let bot = BotId::mint();
    labels.learn_label(chat, Some(&label(bot)));
    labels.learn_label(chat, None);
    assert_eq!(
        labels.known(chat),
        Some(&Known::Bot {
            bot,
            name: "Scout".into()
        })
    );
}

/// **A conversation in Trash is said, not remembered** (S14): the owner's
/// `bot_conversation_trashed` (and the Bot-level `bot_trashed`) resolve as
/// in Trash, and nothing is learned about whose it is.
#[test]
fn a_conversation_in_trash_resolves_as_trashed_and_is_not_remembered() {
    for code in ["bot_conversation_trashed", "bot_trashed"] {
        let (link, _commands, answers) = BotsLink::test_pair();
        let mut labels = ChatLabels::with_link(link);
        let chat = ChatId::new();
        assert!(labels.ask(chat, None));
        answers
            .send(Answer::Labelled {
                chat,
                result: Err(Failure::Refused(code.into())),
            })
            .unwrap();
        assert_eq!(labels.poll(), vec![(chat, Resolved::Trashed)], "{code}");
        assert!(labels.known(chat).is_none(), "{code}");
        assert!(!labels.awaiting(), "{code}");
    }
}
