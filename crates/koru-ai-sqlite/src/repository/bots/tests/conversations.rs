// SPDX-License-Identifier: MPL-2.0
use super::*;

fn setup() -> (Repository, Bot, ProfileId) {
    let mut repo = Repository::in_memory().unwrap();
    let bot = fixture_bot();
    let profile = ProfileId::new();
    repo.save_bot(&bot, profile, BotRequestId::mint(), "create", None)
        .unwrap();
    (repo, bot, profile)
}

#[test]
fn additional_chat_is_atomic_idempotent_hidden_and_has_no_execution() {
    let (mut repo, bot, profile) = setup();
    let request = BotRequestId::mint();
    let now = Timestamp::from_millis(2);
    let first = repo
        .create_bot_chat(&bot, profile, request, "same", now)
        .unwrap();
    assert_ne!(first.chat_id, bot.main_chat);
    assert_eq!(
        repo.create_bot_chat(&bot, profile, request, "same", now)
            .unwrap(),
        first
    );
    assert_eq!(
        repo.create_bot_chat(&bot, profile, request, "changed", now),
        Err(BotError::Conflict)
    );
    assert!(repo.chat(first.chat_id).unwrap().is_none());
    let chat = repo
        .chat_with_access(
            first.chat_id,
            &ChatReadAccess::authorized(vec![first.chat_id]),
        )
        .unwrap()
        .unwrap();
    // Titled as an ordinary new chat is (window run 2: both rows read the
    // Bot's name); the main conversation keeps the name it was made with.
    assert_eq!(chat.title, koru_ai_core::NEW_CHAT_TITLE);
    let main = repo
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    assert_eq!(main.title, bot.config.name);
    assert_eq!(chat.current_model, bot.config.model);
    assert_eq!(
        chat.agent_working_folder.as_deref(),
        Some(bot.config.working_folder.as_str())
    );
    assert_eq!(chat.agent_mode, bot.config.mode);
    assert_eq!(chat.agent_fast, bot.config.fast);
    assert_eq!(chat.profile_id, Some(profile));
    assert_eq!(repo.bot(bot.id).unwrap(), Some(bot.clone()));
    assert_eq!(repo.all_bot_chats().unwrap().len(), 2);
    for table in ["inference_attempts", "bot_chat_runs", "bot_run_replay"] {
        let count: u64 = repo
            .connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
}

#[test]
fn late_receipt_failure_rolls_back_chat_and_association() {
    let (mut repo, bot, profile) = setup();
    repo.connection.execute_batch("CREATE TRIGGER refuse_additional BEFORE INSERT ON bot_command_receipts WHEN NEW.command_kind='create_chat' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert_eq!(
        repo.create_bot_chat(
            &bot,
            profile,
            BotRequestId::mint(),
            "new",
            Timestamp::from_millis(2)
        ),
        Err(BotError::Storage)
    );
    assert_eq!(repo.all_bot_chats().unwrap(), vec![(bot.id, bot.main_chat)]);
    assert_eq!(
        repo.connection
            .query_row("SELECT count(*) FROM chats", [], |row| row.get::<_, u64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn stale_snapshot_inactive_bot_and_changed_association_cannot_replay() {
    let (mut repo, bot, profile) = setup();
    let now = Timestamp::from_millis(2);
    let request = BotRequestId::mint();
    let reference = repo
        .create_bot_chat(&bot, profile, request, "same", now)
        .unwrap();
    let mut stale = bot.clone();
    stale.revision += 1;
    assert_eq!(
        repo.create_bot_chat(&stale, profile, BotRequestId::mint(), "new", now),
        Err(BotError::RevisionConflict)
    );
    let mut foreign = bot.clone();
    foreign.owner_device = DeviceId::from_uuid(Uuid::new_v4());
    assert_eq!(
        repo.create_bot_chat(&foreign, profile, BotRequestId::mint(), "new", now),
        Err(BotError::Forbidden)
    );
    assert_eq!(repo.all_bot_chats().unwrap().len(), 2);
    assert_eq!(
        repo.create_bot_chat(&bot, ProfileId::new(), request, "same", now),
        Err(BotError::NotFound)
    );
    repo.connection
        .execute(
            "DELETE FROM bot_chats WHERE chat_id=?1",
            [reference.chat_id.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.create_bot_chat(&bot, profile, request, "same", now),
        Err(BotError::NotFound)
    );
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='trashed' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.create_bot_chat(&bot, profile, request, "same", now),
        Err(BotError::Forbidden)
    );
    assert_eq!(
        repo.create_bot_chat(&bot, profile, BotRequestId::mint(), "new", now),
        Err(BotError::Forbidden)
    );
    assert_eq!(repo.all_bot_chats().unwrap().len(), 1);
}
