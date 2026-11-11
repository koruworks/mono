// SPDX-License-Identifier: MPL-2.0
//! Holds and visibility: a conversation in Trash leaves every aggregate and
//! is held still; the designated main survives as an identity (O7).
use super::*;

fn trash_main(repo: &mut Repository, bot: &Bot, owner: BotOwner) -> LifecycleOperation {
    let operation = repo
        .capture_bot_lifecycle(&capture(bot, owner, BotLifecycleKind::TrashChat))
        .unwrap();
    repo.fixture_trash_bot_chat(&operation, bot.main_chat, 11)
        .unwrap();
    operation
}

#[test]
fn a_trashed_main_keeps_its_identity_and_leaves_aggregates() {
    let (mut repo, bot, owner) = setup();
    let additional = repo
        .create_bot_chat(
            &bot,
            ProfileId::new(),
            BotRequestId::mint(),
            HASH,
            koru_ai_core::Timestamp::from_millis(2),
        )
        .unwrap()
        .chat_id;
    assert_eq!(repo.bot_chat_hold(bot.main_chat).unwrap(), None);
    assert_eq!(repo.visible_bot_chats().unwrap().len(), 2);
    trash_main(&mut repo, &bot, owner);
    // O7: the association stays and is still the Bot's designated main; no
    // replacement exists; the Bot itself stays active and readable.
    let read = repo.bot(bot.id).unwrap().unwrap();
    assert_eq!(read.main_chat, bot.main_chat);
    assert_eq!(repo.bot_for_chat(bot.main_chat).unwrap(), Some(bot.id));
    assert_eq!(repo.all_bot_chats().unwrap().len(), 2);
    assert_eq!(
        repo.visible_bot_chats().unwrap(),
        vec![(bot.id, additional)]
    );
    assert_eq!(
        repo.bot_chat_hold(bot.main_chat).unwrap(),
        Some(BotHold::Trashed)
    );
    assert_eq!(repo.bot_chat_hold(additional).unwrap(), None);
    assert_eq!(repo.bot_hold(bot.id).unwrap(), None);
    // A new conversation is additional, never a second main.
    let next = repo
        .create_bot_chat(
            &read,
            ProfileId::new(),
            BotRequestId::mint(),
            HASH,
            koru_ai_core::Timestamp::from_millis(3),
        )
        .unwrap();
    let mains: i64 = repo
        .connection
        .query_row(
            "SELECT count(*) FROM bot_chats WHERE bot_id=?1 AND purpose='main'",
            [bot.id.to_storage()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mains, 1);
    assert_ne!(next.chat_id, bot.main_chat);
}

#[test]
fn open_operations_and_trashed_bots_hold_their_conversations() {
    let (mut repo, bot, owner) = setup();
    // A conversation operation holds only that conversation.
    let chat_op = repo
        .capture_bot_lifecycle(&capture(&bot, owner, BotLifecycleKind::TrashChat))
        .unwrap();
    assert_eq!(
        repo.bot_chat_hold(bot.main_chat).unwrap(),
        Some(BotHold::Pending)
    );
    assert_eq!(repo.bot_hold(bot.id).unwrap(), None);
    repo.connection
        .execute(
            "UPDATE bot_lifecycle_intents SET stage='completed',completed_at_ms=12 WHERE operation_id=?1",
            [chat_op.operation.to_storage()],
        )
        .unwrap();
    assert_eq!(repo.bot_chat_hold(bot.main_chat).unwrap(), None);
    // A whole-Bot operation holds the Bot and every conversation of it.
    let mut archive = capture(&bot, owner, BotLifecycleKind::Archive);
    archive.expected_revision = bot.revision;
    repo.capture_bot_lifecycle(&archive).unwrap();
    assert_eq!(repo.bot_hold(bot.id).unwrap(), Some(BotHold::Pending));
    assert_eq!(
        repo.bot_chat_hold(bot.main_chat).unwrap(),
        Some(BotHold::Pending)
    );
    // A trashed Bot holds them as Trashed, and leaves the aggregates.
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='trashed' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert_eq!(repo.bot_hold(bot.id).unwrap(), Some(BotHold::BotTrashed));
    assert_eq!(
        repo.bot_chat_hold(bot.main_chat).unwrap(),
        Some(BotHold::BotTrashed)
    );
    assert!(repo.visible_bot_chats().unwrap().is_empty());
    // Ordinary reads still refuse it; lifecycle reads see it and its lifecycle.
    assert_eq!(repo.bot(bot.id), Err(BotError::Forbidden));
    let (read, lifecycle) = repo.bot_any_lifecycle(bot.id).unwrap().unwrap();
    assert_eq!(lifecycle, BotLifecycle::Trashed);
    assert_eq!(read.main_chat, bot.main_chat);
    assert_eq!(repo.bot_owner(bot.id).unwrap(), Some(owner));
    assert_eq!(
        repo.bots_in_lifecycle(owner, bot.home_workspace, BotLifecycle::Trashed)
            .unwrap()
            .len(),
        1
    );
    assert!(
        repo.bots_in_lifecycle(owner, bot.home_workspace, BotLifecycle::Archived)
            .unwrap()
            .is_empty()
    );
    let mut other = owner;
    other.device = DeviceId::from_uuid(uuid::Uuid::new_v4());
    assert!(
        repo.bots_in_lifecycle(other, bot.home_workspace, BotLifecycle::Trashed)
            .unwrap()
            .is_empty()
    );
    // An ordinary Chat is never held.
    assert_eq!(repo.bot_chat_hold(ChatId::new()).unwrap(), None);
}

#[test]
fn a_purged_main_resolves_from_its_tombstone_without_a_replacement() {
    let (mut repo, bot, owner) = setup();
    let trash = trash_main(&mut repo, &bot, owner);
    // Stand-in for S8's purge transaction: the live association and Chat go,
    // the content-free main tombstone stays.
    let scope = owner.user_scope.to_storage();
    repo.connection
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .unwrap();
    repo.connection
        .execute(
            "DELETE FROM bot_chat_trash WHERE chat_id=?1",
            [bot.main_chat.to_storage()],
        )
        .unwrap();
    repo.connection
        .execute(
            "DELETE FROM bot_chats WHERE chat_id=?1",
            [bot.main_chat.to_storage()],
        )
        .unwrap();
    repo.connection
        .execute(
            "INSERT INTO bot_purge_tombstones(user_scope_id,subject_kind,subject_id,bot_id,owner_device_id,operation_id,chat_purpose,purged_at_ms) VALUES(?1,'chat',?2,?3,?4,?5,'main',20)",
            params![
                scope,
                bot.main_chat.to_storage(),
                bot.id.to_storage(),
                owner.device.to_storage(),
                trash.operation.to_storage()
            ],
        )
        .unwrap();
    repo.connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    let read = repo.bot(bot.id).unwrap().unwrap();
    assert_eq!(read.main_chat, bot.main_chat);
    assert_eq!(repo.bot_for_chat(bot.main_chat).unwrap(), None);
    assert!(repo.visible_bot_chats().unwrap().is_empty());
    // Without a live main or its tombstone the Bot is damaged, not silently remapped.
    repo.connection
        .execute("DELETE FROM bot_purge_tombstones", [])
        .unwrap();
    assert_eq!(repo.bot(bot.id), Err(BotError::Storage));
}
