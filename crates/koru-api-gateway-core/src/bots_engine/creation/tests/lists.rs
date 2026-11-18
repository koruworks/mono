// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::bots::*;

fn new_chat(f: &Fixture, bot_id: BotId) -> koru_ai_core::ChatId {
    let BotResponse::Chat { reference } = f
        .gateway
        .window_command(BotCommand::CreateChat {
            bot_id,
            request_id: BotRequestId::from_uuid(Uuid::new_v4()),
        })
        .unwrap()
    else {
        panic!()
    };
    reference.chat_id
}

#[test]
fn conversation_list_preserves_main_and_byte_limited_cursor_without_losing_rows() {
    let f = Fixture::new();
    let path = if cfg!(windows) {
        format!(r"C:\projects\{}", "x/".repeat(12000))
    } else {
        format!("/projects/{}", "x/".repeat(12000))
    };
    f.fs.entries
        .lock()
        .unwrap()
        .insert(PathBuf::from(&path), true);
    let mut create = form(&f);
    if let BotCommand::CreateBotWithRole { folder, .. } = &mut create {
        *folder = BotFolderChoice::Selected { path };
    }
    let bot = creation(f.gateway.window_command(create).unwrap())
        .bot
        .unwrap();
    let mut expected = std::collections::BTreeSet::from([bot.main_chat]);
    for _ in 0..32 {
        expected.insert(new_chat(&f, bot.id));
    }
    let mut cursor = None;
    let mut actual = vec![];
    let mut pages = 0;
    loop {
        let BotResponse::Chats { page } = f
            .gateway
            .window_command(BotCommand::ListBotChats {
                bot_id: bot.id,
                cursor,
                limit: 100,
            })
            .unwrap()
        else {
            panic!()
        };
        assert!(!page.chats.is_empty());
        for conversation in &page.chats {
            assert_eq!(conversation.chat.profile_id, Some(f.store.profile));
            assert_eq!(conversation.chat.bot.as_ref(), Some(&conversation.label));
            assert_eq!(conversation.chat.current_model, bot.config.model);
        }
        actual.extend(page.chats.into_iter().map(|c| c.chat.id));
        pages += 1;
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
        assert!(pages < 4);
    }
    assert!(pages > 1);
    assert_eq!(actual[0], bot.main_chat);
    assert_eq!(actual.len(), expected.len());
    assert_eq!(
        actual
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        expected
    );
    assert_eq!(
        f.gateway
            .window_command(BotCommand::ListBotChats {
                bot_id: bot.id,
                cursor: None,
                limit: 0
            })
            .err(),
        Some(Error::InvalidRequest)
    );
}

#[test]
fn trash_list_keeps_conversation_and_bot_restore_identities_distinct() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let own = new_chat(&f, bot.id);
    let live = new_chat(&f, bot.id);
    let own_request = BotRequestId::from_uuid(Uuid::new_v4());
    f.gateway
        .window_command(BotCommand::TrashChat {
            request_id: own_request,
            bot_id: bot.id,
            chat_id: own,
            expected_revision: 1,
        })
        .unwrap();
    let request = BotRequestId::from_uuid(Uuid::new_v4());
    f.gateway
        .window_command(BotCommand::Trash {
            request_id: request,
            bot_id: bot.id,
            expected_revision: 1,
        })
        .unwrap();
    let mut cursor = None;
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let BotResponse::TrashedChats { page } = f
            .gateway
            .window_command(BotCommand::ListTrashedBotChats {
                workspace: f.store.home,
                cursor,
                limit: 1,
            })
            .unwrap()
        else {
            panic!()
        };
        for item in page.chats {
            assert_eq!(item.restore, BotChatRestore::Bot);
            assert_eq!(item.bot_held_by.unwrap().request_id, request);
            let (expected, kind) = if item.conversation.chat.id == own {
                (own_request, BotLifecycleKind::TrashChat)
            } else {
                (request, BotLifecycleKind::Trash)
            };
            assert_eq!(item.trashed_by.request_id, expected);
            assert_eq!(item.trashed_by.kind, kind);
            assert!(seen.insert(item.conversation.chat.id));
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
        assert!(seen.len() < 4);
    }
    assert_eq!(
        seen,
        std::collections::BTreeSet::from([bot.main_chat, own, live])
    );
    f.gateway
        .window_command(BotCommand::Restore {
            request_id: BotRequestId::from_uuid(Uuid::new_v4()),
            bot_id: bot.id,
            expected_revision: 2,
            source_request_id: request,
        })
        .unwrap();
    let BotResponse::TrashedChats { page } = f
        .gateway
        .window_command(BotCommand::ListTrashedBotChats {
            workspace: f.store.home,
            cursor: None,
            limit: 100,
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(page.chats.len(), 1);
    assert_eq!(page.chats[0].conversation.chat.id, own);
    assert_eq!(page.chats[0].restore, BotChatRestore::Conversation);
    assert!(page.chats[0].bot_held_by.is_none());
    let BotResponse::Chats { page } = f
        .gateway
        .window_command(BotCommand::ListBotChats {
            bot_id: bot.id,
            cursor: None,
            limit: 100,
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(page.chats.len(), 2);
}
