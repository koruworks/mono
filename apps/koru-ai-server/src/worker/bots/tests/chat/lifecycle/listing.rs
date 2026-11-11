// SPDX-License-Identifier: MPL-2.0
//! What a client that knows only the workspace can find after a restart:
//! the Bots in the Trash filter with the revision a Restore needs, and the
//! conversations in Trash with the operation that put each there.
use super::trash::{
    another_chat, no_owner_effects, restore_at, restore_chat_at, trash_at, trash_chat_at,
};
use super::*;

fn trashed_chats(client: &AiWorkerClient, workspace: WorkspaceId) -> Vec<BotTrashedChat> {
    let mut found = Vec::new();
    let mut cursor = None;
    loop {
        let BotResponse::TrashedChats { page } = client
            .bots(
                context(true),
                BotCommand::ListTrashedBotChats {
                    workspace,
                    cursor: cursor.clone(),
                    limit: 1,
                },
            )
            .unwrap()
        else {
            panic!("Trash page")
        };
        found.extend(page.chats);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return found,
        }
    }
}

fn trashed_bots(client: &AiWorkerClient, workspace: WorkspaceId) -> Vec<Bot> {
    let Ok(BotResponse::List { bots }) = client.bots(
        context(true),
        BotCommand::ListLifecycle {
            workspace,
            lifecycle: BotLifecycle::Trashed,
        },
    ) else {
        panic!("Trash filter")
    };
    bots
}

#[test]
fn after_a_restart_the_workspace_alone_finds_and_restores_a_trashed_bot_and_its_conversations() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let alone = another_chat(&mut repository, &authority, &bot);
    let other = another_chat(&mut repository, &authority, &bot);
    let port = Arc::new(Archiving::default());
    let (worker, returned) = spawn(repository, authority.clone(), port.clone());
    let client = worker.client();
    // A conversation trashed on its own, then the whole Bot.
    let chat_trash = BotRequestId::mint();
    trash_chat_at(&client, &bot, alone, chat_trash, bot.revision).unwrap();
    assert_eq!(
        settled(&client, bot.id, chat_trash).stage,
        BotLifecycleStage::Completed
    );
    let bot_trash = BotRequestId::mint();
    trash_at(&client, &bot, bot_trash, bot.revision).unwrap();
    assert_eq!(
        settled(&client, bot.id, bot_trash).stage,
        BotLifecycleStage::Completed
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);

    // A fresh worker over the same database; the client remembers nothing.
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    let (worker, returned) = spawn(repository, authority.clone(), port.clone());
    let client = worker.client();
    let workspace = bot.home_workspace;
    let bots = trashed_bots(&client, workspace);
    assert_eq!(bots.iter().map(|b| b.id).collect::<Vec<_>>(), vec![bot.id]);
    // The revision a Restore needs, since the trashed Bot's read is
    // not_found.
    let revision = bots[0].revision;
    assert_eq!(
        client
            .bots(context(true), BotCommand::Read { bot_id: bot.id })
            .unwrap_err()
            .code,
        "not_found"
    );
    let listed = trashed_chats(&client, workspace);
    assert_eq!(listed.len(), 3, "{listed:?}");
    let entry = |chat: ChatId| {
        listed
            .iter()
            .find(|e| e.conversation.chat.id == chat)
            .unwrap()
            .clone()
    };
    // The conversations the Bot's Trash hid name it, and are restored with
    // the Bot; the one trashed before names its own operation.
    for chat in [bot.main_chat, other] {
        let found = entry(chat);
        assert_eq!(found.trashed_by.request_id, bot_trash);
        assert_eq!(found.trashed_by.kind, BotLifecycleKind::Trash);
        assert_eq!(found.restore, BotChatRestore::Bot);
        assert_eq!(found.conversation.label.bot_id, bot.id);
    }
    let own = entry(alone);
    assert_eq!(own.trashed_by.request_id, chat_trash);
    assert_eq!(own.trashed_by.kind, BotLifecycleKind::TrashChat);
    assert_eq!(own.restore, BotChatRestore::Bot, "its Bot is in Trash");
    // Restore, naming only what was read.
    let source = entry(bot.main_chat).trashed_by.request_id;
    restore_at(&client, &bot, source, revision).unwrap();
    assert!(trashed_bots(&client, workspace).is_empty());
    // The reversed operation is no longer named; the conversation trashed
    // on its own survives the Bot's Restore and is now restored on its own.
    let listed = trashed_chats(&client, workspace);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].conversation.chat.id, alone);
    assert_eq!(listed[0].restore, BotChatRestore::Conversation);
    let current = read_bot(&client, bot.id);
    restore_chat_at(
        &client,
        &bot,
        alone,
        listed[0].trashed_by.request_id,
        current.revision,
    )
    .unwrap();
    assert!(trashed_chats(&client, workspace).is_empty());
    // A workspace the caller cannot reach is refused, never listed.
    assert!(
        client
            .bots(
                context(true),
                BotCommand::ListTrashedBotChats {
                    workspace: WorkspaceId::new(),
                    cursor: None,
                    limit: 10,
                },
            )
            .is_err()
    );
    no_owner_effects(&port);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    drop(returned);
}

#[test]
fn another_owner_learns_nothing_of_a_workspaces_trash() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let alone = another_chat(&mut repository, &authority, &bot);
    let port = Arc::new(Archiving::default());
    let (worker, _returned) = spawn(repository, authority.clone(), port.clone());
    let client = worker.client();
    let request = BotRequestId::mint();
    trash_chat_at(&client, &bot, alone, request, bot.revision).unwrap();
    assert_eq!(
        settled(&client, bot.id, request).stage,
        BotLifecycleStage::Completed
    );
    // Without Bot management the read is refused before anything is read.
    assert!(
        client
            .bots(
                context(false),
                BotCommand::ListTrashedBotChats {
                    workspace: bot.home_workspace,
                    cursor: None,
                    limit: 10,
                },
            )
            .is_err()
    );
    // A workspace the owner cannot reach answers its refusal.
    authority.allowed.store(false, Ordering::SeqCst);
    assert!(
        client
            .bots(
                context(true),
                BotCommand::ListTrashedBotChats {
                    workspace: bot.home_workspace,
                    cursor: None,
                    limit: 10,
                },
            )
            .is_err()
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
