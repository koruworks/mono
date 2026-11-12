// SPDX-License-Identifier: MPL-2.0
//! Renaming a Bot conversation, as an AI chat is renamed (the owner's rule
//! for Bot conversations, "same as an AI chat"): through the same guards as
//! the conversation's other changes, touching only that conversation's
//! title, never the Bot.
use super::holds::capture;
use super::*;
use koru_ai_wire::bot_lifecycle_codes::{ARCHIVED, BOT_TRASHED, PENDING, TRASHED};

fn worker(repo: Repository, authority: Arc<Authority>) -> (WorkerHandle, AiWorkerClient) {
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-rename"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority);
    config.test_repository = Some(repo);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    (worker, client)
}

fn rename(
    client: &AiWorkerClient,
    context: RequestContext,
    chat: ChatId,
    title: &str,
) -> ApiResult<Chat> {
    client
        .rename_chat(
            context,
            chat,
            RenameChatRequest {
                title: title.into(),
            },
        )
        .map(|response| response.chat)
}

fn title(client: &AiWorkerClient, chat: ChatId) -> String {
    client.read_chat(context(true), chat).unwrap().chat.title
}

fn bot_of(client: &AiWorkerClient, bot: BotId) -> Bot {
    let Ok(BotResponse::Bot { bot }) = client.bots(context(true), BotCommand::Read { bot_id: bot })
    else {
        panic!("the Bot")
    };
    bot
}

/// The additional conversation (made untitled, as an ordinary new chat is)
/// takes the title its first message gives it, as the chat surface sends
/// it; the main conversation, made with the Bot's name, is renamed by the
/// person like any other. Neither touches the Bot, its revision or the other
/// conversation, and an ordinary caller still finds nothing.
#[test]
fn a_bot_conversation_is_renamed_as_an_ai_chat_is_and_the_bot_is_untouched() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let additional = repo
        .create_bot_chat(&bot, authority.profile, BotRequestId::mint(), "x", at(3))
        .unwrap()
        .chat_id;
    let (worker, client) = worker(repo, authority);
    assert_eq!(title(&client, additional), koru_ai_core::NEW_CHAT_TITLE);
    let before = bot_of(&client, bot.id);

    let renamed = rename(&client, context(true), additional, "Plan the launch").unwrap();
    assert_eq!(renamed.id, additional);
    assert_eq!(renamed.title, "Plan the launch");
    assert_eq!(title(&client, bot.main_chat), bot.config.name);

    assert_eq!(
        rename(&client, context(true), bot.main_chat, "Kickoff")
            .unwrap()
            .title,
        "Kickoff"
    );
    assert_eq!(title(&client, additional), "Plan the launch");
    let after = bot_of(&client, bot.id);
    assert_eq!(after.config.name, bot.config.name, "the Bot keeps its name");
    assert_eq!(
        after.revision, before.revision,
        "a rename moves no revision"
    );
    assert_eq!(after.policy_revision, before.policy_revision);

    // Not the owner's Bot capability: nothing to see, nothing renamed.
    assert_eq!(
        rename(&client, context(false), additional, "Taken")
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(title(&client, additional), "Plan the launch");
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// A later save of the Bot's defaults never rewrites a conversation's title
/// (QUESTIONS O23), the main one included.
#[test]
fn a_renamed_conversation_keeps_its_title_when_the_bots_defaults_are_saved() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let additional = repo
        .create_bot_chat(&bot, authority.profile, BotRequestId::mint(), "x", at(3))
        .unwrap()
        .chat_id;
    // What the worker's rename writes (`WorkerState::rename_chat`).
    assert!(repo.rename_chat(additional, "Plan the launch").unwrap());
    assert!(repo.rename_chat(bot.main_chat, "Kickoff").unwrap());
    let mut config = bot.config.clone();
    config.name = "Scout Renamed".into();
    let BotResponse::Bot { bot: saved } = execute_bot_command(
        &mut repo,
        authority.clone(),
        |_| Ok(()),
        context(true),
        BotCommand::Update {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: bot.revision,
            config,
        },
    )
    .unwrap() else {
        panic!("Bot")
    };
    assert_eq!(saved.config.name, "Scout Renamed");
    let title = |chat: ChatId| {
        repo.chat_with_access(chat, &ChatReadAccess::authorized(vec![chat]))
            .unwrap()
            .unwrap()
            .title
    };
    assert_eq!(title(additional), "Plan the launch");
    assert_eq!(title(bot.main_chat), "Kickoff");
}

/// Held as its neighbours (a model or agent-settings change) are: a
/// conversation in Trash, a Bot in Trash, an archived Bot, and an Archive,
/// Trash or Restore moving or parked over it each refuse with their own code,
/// and nothing is renamed.
#[test]
fn a_rename_is_held_as_the_conversations_other_changes_are() {
    // The conversation in Trash.
    {
        let (mut repo, authority, command) = fixture();
        let bot = created(&mut repo, authority.clone(), command);
        let additional = repo
            .create_bot_chat(&bot, authority.profile, BotRequestId::mint(), "x", at(3))
            .unwrap()
            .chat_id;
        let operation = repo
            .capture_bot_lifecycle(&capture(
                &bot,
                BotLifecycleKind::TrashChat,
                Some(additional),
            ))
            .unwrap();
        repo.fixture_trash_bot_chat(&operation, additional, 9)
            .unwrap();
        let (worker, client) = worker(repo, authority);
        assert_eq!(
            rename(&client, context(true), additional, "Taken")
                .unwrap_err()
                .code,
            TRASHED
        );
        worker.shutdown();
        assert_eq!(worker.join(), WorkerExit::Clean);
    }
    // The Bot in Trash, and the Bot archived (its conversations read only).
    for (lifecycle, code) in [
        (BotLifecycle::Trashed, BOT_TRASHED),
        (BotLifecycle::Archived, ARCHIVED),
    ] {
        let (mut repo, authority, command) = fixture();
        let bot = created(&mut repo, authority.clone(), command);
        repo.fixture_set_bot_lifecycle(bot.id, lifecycle).unwrap();
        let (worker, client) = worker(repo, authority);
        assert_eq!(
            rename(&client, context(true), bot.main_chat, "Taken")
                .unwrap_err()
                .code,
            code,
            "{lifecycle:?}"
        );
        worker.shutdown();
        assert_eq!(worker.join(), WorkerExit::Clean);
    }
    // An operation moving over the conversation, then the same parked.
    for parked in [false, true] {
        let (mut repo, authority, command) = fixture();
        let bot = created(&mut repo, authority.clone(), command);
        let operation = repo
            .capture_bot_lifecycle(&capture(
                &bot,
                BotLifecycleKind::TrashChat,
                Some(bot.main_chat),
            ))
            .unwrap();
        if parked {
            assert!(
                repo.park_bot_lifecycle(
                    bot.user_scope,
                    operation.operation,
                    BotLifecycleRefusal::WorkRunning,
                    9,
                )
                .unwrap()
            );
        }
        let (worker, client) = worker(repo, authority);
        assert_eq!(
            rename(&client, context(true), bot.main_chat, "Taken")
                .unwrap_err()
                .code,
            PENDING,
            "parked: {parked}"
        );
        assert_eq!(title(&client, bot.main_chat), bot.config.name);
        worker.shutdown();
        assert_eq!(worker.join(), WorkerExit::Clean);
    }
}
