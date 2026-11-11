// SPDX-License-Identifier: MPL-2.0
//! The Bots view's owner reads, through the real worker: the same prepared
//! owner proof as every other Bot read, before any conversation is read.
use super::*;

fn worker(repo: Repository, authority: Arc<Authority>) -> (WorkerHandle, AiWorkerClient) {
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-reads"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority);
    config.test_repository = Some(repo);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    (worker, client)
}

fn list(bot: BotId, cursor: Option<BotChatCursor>, limit: u16) -> BotCommand {
    BotCommand::ListBotChats {
        bot_id: bot,
        cursor,
        limit,
    }
}

#[test]
fn the_owner_pages_a_bots_conversations_and_nobody_else_reads_them() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let additional = repo
        .create_bot_chat(&bot, authority.profile, BotRequestId::mint(), "x", at(5))
        .unwrap()
        .chat_id;
    let (worker, client) = worker(repo, authority.clone());

    let BotResponse::Chats {
        page: BotChatPage { chats, next_cursor },
    } = client.bots(context(true), list(bot.id, None, 1)).unwrap()
    else {
        panic!("a page")
    };
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0].chat.id, bot.main_chat);
    assert_eq!(
        chats[0].label,
        BotChatLabel {
            bot_id: bot.id,
            bot_name: bot.config.name.clone(),
            purpose: BotChatPurpose::Main,
        }
    );
    let BotResponse::Chats {
        page: BotChatPage { chats, next_cursor },
    } = client
        .bots(context(true), list(bot.id, next_cursor, 1))
        .unwrap()
    else {
        panic!("a page")
    };
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0].chat.id, additional);
    assert_eq!(chats[0].label.purpose, BotChatPurpose::Additional);
    assert_eq!(next_cursor, None);

    // Ordinary chat scope: refused before the Bot is looked at.
    assert_eq!(
        client
            .bots(context(false), list(bot.id, None, 10))
            .unwrap_err()
            .code,
        "forbidden"
    );
    // A Bot nobody created reads as missing, not as an empty list.
    assert_eq!(
        client
            .bots(context(true), list(BotId::mint(), None, 10))
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(
        client
            .bots(context(true), list(bot.id, None, BOT_CHAT_PAGE_LIMIT + 1))
            .unwrap_err()
            .code,
        "invalid_request"
    );
    // Revoked current authority: the owner's own Bot is refused too.
    authority.allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        client
            .bots(context(true), list(bot.id, None, 10))
            .unwrap_err()
            .code,
        "wrong_owner"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn workspace_activity_is_the_owners_alone_and_a_restart_leaves_nothing_running() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    // A reply left pending by the previous process: the worker's startup
    // recovery interrupts it, and the durable answer follows the transcript.
    let pending = turn(bot.main_chat, Uuid::new_v4(), 3);
    repo.begin_turn(&pending).unwrap();
    let workspace = authority.workspace;
    let (worker, client) = worker(repo, authority.clone());

    let BotResponse::Activity { activity } = client
        .bots(context(true), BotCommand::ListActivity { workspace })
        .unwrap()
    else {
        panic!("activity")
    };
    assert_eq!(
        activity,
        BotActivityPage {
            entries: vec![],
            truncated: false,
        }
    );
    assert_eq!(
        client
            .bots(context(false), BotCommand::ListActivity { workspace })
            .unwrap_err()
            .code,
        "forbidden"
    );
    // A workspace the owner cannot currently reach is refused, not empty.
    assert_eq!(
        client
            .bots(
                context(true),
                BotCommand::ListActivity {
                    workspace: WorkspaceId::new()
                }
            )
            .unwrap_err()
            .code,
        "wrong_owner"
    );
    authority.allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        client
            .bots(context(true), BotCommand::ListActivity { workspace })
            .unwrap_err()
            .code,
        "wrong_owner"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn an_archived_bots_conversations_stay_listable_but_it_has_no_activity() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    repo.fixture_set_bot_lifecycle(bot.id, BotLifecycle::Archived)
        .unwrap();
    let workspace = authority.workspace;
    let (worker, client) = worker(repo, authority.clone());

    let BotResponse::Chats {
        page: BotChatPage { chats, next_cursor },
    } = client.bots(context(true), list(bot.id, None, 10)).unwrap()
    else {
        panic!("a page")
    };
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0].chat.id, bot.main_chat);
    assert_eq!(next_cursor, None);
    // The roster's activity read stays with active Bots.
    let BotResponse::Activity { activity } = client
        .bots(context(true), BotCommand::ListActivity { workspace })
        .unwrap()
    else {
        panic!("activity")
    };
    assert!(activity.entries.is_empty());
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn a_trashed_bots_conversations_are_refused_as_read_refuses_it() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    repo.fixture_set_bot_lifecycle(bot.id, BotLifecycle::Trashed)
        .unwrap();
    let (worker, client) = worker(repo, authority.clone());
    let read = client
        .bots(context(true), BotCommand::Read { bot_id: bot.id })
        .unwrap_err()
        .code;
    // P1.7 S4b: a trashed Bot reads as missing to its owner (it was
    // `wrong_owner`, because its workspace is not inspected for it).
    assert_eq!(read, "not_found");
    assert_eq!(
        client
            .bots(context(true), list(bot.id, None, 10))
            .unwrap_err()
            .code,
        read
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
