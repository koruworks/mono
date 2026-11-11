// SPDX-License-Identifier: MPL-2.0

use koru_ai_core::bots::{BotChatLabel, BotChatReference, BotConversation};
use koru_ai_core::{AiProviderId, Chat, ModelSelection, ReasoningChoice, Timestamp};

use super::*;

fn chat(id: ChatId, title: &str, at: i64) -> Chat {
    Chat {
        id,
        title: title.into(),
        private: false,
        current_model: ModelSelection {
            provider: AiProviderId::parse("agent-codex").expect("a provider id"),
            requested_model: "default".into(),
            default_reasoning: ReasoningChoice::Default,
        },
        branched_from: None,
        agent_working_folder: None,
        agent_mode: None,
        agent_fast: None,
        pinned_at: None,
        profile_id: None,
        bot: None,
        created_at: Timestamp::from_millis(at),
        updated_at: Timestamp::from_millis(at),
    }
}

fn page(bot: BotId, rows: &[(ChatId, BotChatPurpose)], more: bool) -> BotChatPage {
    BotChatPage {
        chats: rows
            .iter()
            .enumerate()
            .map(|(i, (id, purpose))| BotConversation {
                chat: chat(*id, &format!("Conversation {i}"), 1_000 - i as i64),
                label: BotChatLabel {
                    bot_id: bot,
                    bot_name: "Scout".into(),
                    purpose: *purpose,
                },
            })
            .collect(),
        next_cursor: more.then(|| BotChatCursor {
            main: false,
            updated_at_ms: 1,
            chat_id: rows.last().expect("a row").0,
        }),
    }
}

fn read_seq(command: Option<Command>) -> (u64, Option<BotChatCursor>, u16) {
    match command {
        Some(Command::ListChats {
            seq, cursor, limit, ..
        }) => (seq, cursor, limit),
        _ => panic!("a list read"),
    }
}

/// **Loading, then the rows; then *Show more* adds the next page** — and
/// the rows are open-only: no pin, no ⋯, no Delete.
#[test]
fn the_list_loads_pages_and_its_rows_only_open() {
    let mut list = Conversations::default();
    assert_eq!(list.surface.row_actions(), RowActions::OPEN_ONLY);
    assert!(list.read().is_none(), "no Bot, nothing to ask");
    let bot = BotId::mint();
    list.set_bot(bot);
    let (seq, cursor, limit) = read_seq(list.read());
    assert_eq!((cursor, limit), (None, PAGE));
    assert!(list.read().is_none(), "one read at a time");
    assert_eq!(list.notice(), Some(Words::BotsConversationsLoading));
    assert!(!list.shows_rows());
    let (main, other, third) = (ChatId::new(), ChatId::new(), ChatId::new());
    assert!(list.answer(
        seq,
        bot,
        Ok(page(
            bot,
            &[
                (main, BotChatPurpose::Main),
                (other, BotChatPurpose::Additional)
            ],
            true
        ))
    ));
    assert_eq!(list.main(), Some(main));
    assert!(list.has_more());
    assert_eq!(list.notice(), None);
    assert_eq!(list.surface.chats().len(), 2);
    let (seq, cursor, _) = read_seq(list.more());
    assert!(cursor.is_some(), "the page's cursor");
    list.answer(
        seq,
        bot,
        Ok(page(bot, &[(third, BotChatPurpose::Additional)], false)),
    );
    assert_eq!(list.listed().len(), 3, "added to, not replaced");
    assert!(!list.has_more());
    assert!(list.more().is_none());
}

/// **Only the latest read's answer about this Bot is taken**, a failed
/// read keeps what was listed and says so, and a re-read asks from the
/// start for as many rows as were shown.
#[test]
fn a_stale_or_failed_answer_never_replaces_what_is_shown() {
    let mut list = Conversations::default();
    let bot = BotId::mint();
    list.set_bot(bot);
    let (first, _, _) = read_seq(list.read());
    let main = ChatId::new();
    assert!(
        !list.answer(first + 7, bot, Ok(page(bot, &[], false))),
        "stale"
    );
    assert!(
        !list.answer(first, BotId::mint(), Ok(page(bot, &[], false))),
        "another Bot's"
    );
    list.answer(
        first,
        bot,
        Ok(page(bot, &[(main, BotChatPurpose::Main)], false)),
    );
    assert_eq!(
        list.notice(),
        Some(Words::BotsConversationsOnlyMain),
        "only the main one: said"
    );
    list.owe_read();
    let (again, cursor, limit) = read_seq(list.read());
    assert_eq!((cursor, limit), (None, PAGE));
    list.answer(again, bot, Err(Failure::Unreachable));
    assert_eq!(list.listed().len(), 1, "kept");
    assert_eq!(list.notice(), Some(Words::BotsConversationsUnavailable));
}

/// A conversation that went to Trash elsewhere is gone from the next read
/// from the start.
#[test]
fn a_conversation_trashed_elsewhere_leaves_on_the_next_read() {
    let mut list = Conversations::default();
    let bot = BotId::mint();
    list.set_bot(bot);
    let (main, gone) = (ChatId::new(), ChatId::new());
    let (seq, _, _) = read_seq(list.read());
    list.answer(
        seq,
        bot,
        Ok(page(
            bot,
            &[
                (main, BotChatPurpose::Main),
                (gone, BotChatPurpose::Additional),
            ],
            false,
        )),
    );
    list.owe_read();
    let (seq, _, _) = read_seq(list.read());
    list.answer(
        seq,
        bot,
        Ok(page(bot, &[(main, BotChatPurpose::Main)], false)),
    );
    assert!(list.listed().iter().all(|chat| chat.id != gone));
    assert!(list.surface.chats().iter().all(|chat| chat.id != gone));
}

/// ***New conversation* is the owner's `CreateChat`**, once at a time; its
/// answer names the conversation the tab shows next and asks the list
/// again; a refusal is said in words, never as the server's message.
#[test]
fn new_conversation_asks_the_owner_and_names_what_it_made() {
    let mut list = Conversations::default();
    assert!(list.create().is_none(), "no Bot");
    let bot = BotId::mint();
    list.set_bot(bot);
    let (seq, _, _) = read_seq(list.read());
    list.answer(
        seq,
        bot,
        Ok(page(bot, &[(ChatId::new(), BotChatPurpose::Main)], false)),
    );
    let Some(Command::Write {
        request, command, ..
    }) = list.create()
    else {
        panic!("a write");
    };
    assert!(matches!(
        *command,
        BotCommand::CreateChat { request_id, bot_id } if request_id == request && bot_id == bot
    ));
    assert!(list.creating());
    assert!(list.create().is_none(), "one at a time");
    assert!(
        !list.wrote(BotRequestId::mint(), &Err(Failure::Unreachable)),
        "not its"
    );
    let made = ChatId::new();
    assert!(list.wrote(
        request,
        &Ok(BotResponse::Chat {
            reference: BotChatReference {
                bot_id: bot,
                chat_id: made,
            },
        })
    ));
    assert_eq!(list.take_made(), Some(made));
    assert!(list.read().is_some(), "the list asks again");

    let Some(Command::Write { request, .. }) = list.create() else {
        panic!("a write");
    };
    list.wrote(
        request,
        &Err(Failure::Refused("bot_conversation_trashed".into())),
    );
    // The conversation's hold, in its own words (S14).
    assert_eq!(list.create_refused(), Some(Words::BotsErrTrashed));
    assert_eq!(list.take_made(), None);
}
