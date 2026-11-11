// SPDX-License-Identifier: MPL-2.0
//! The owner's Bot conversations in Trash, per workspace.
use super::*;

fn owner_of(bot: &Bot) -> BotOwner {
    BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    }
}

fn page(repo: &Repository, bot: &Bot) -> BotTrashedChatPage {
    repo.trashed_bot_chat_page(owner_of(bot), bot.home_workspace, None, 100)
        .unwrap()
}

#[test]
fn trashed_conversations_page_by_id_without_gaps_and_say_which_restore_brings_them_back() {
    let (mut repo, bot, profile) = setup();
    let mut trashed: Vec<ChatId> = (0..5)
        .map(|n| additional(&mut repo, &bot, profile, 10 + n))
        .collect();
    let kept = additional(&mut repo, &bot, profile, 20);
    for chat in &trashed {
        trash(&repo, &bot, *chat);
    }
    trashed.sort_by_key(|chat| chat.to_storage());
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let found = repo
            .trashed_bot_chat_page(owner_of(&bot), bot.home_workspace, cursor.as_ref(), 2)
            .unwrap();
        assert!(found.chats.len() <= 2);
        for entry in &found.chats {
            assert_eq!(entry.trashed_by.kind, BotLifecycleKind::TrashChat);
            assert_eq!(entry.restore, BotChatRestore::Conversation);
            assert_eq!(entry.conversation.label.bot_id, bot.id);
            assert_eq!(
                entry.conversation.chat.bot,
                Some(entry.conversation.label.clone())
            );
            seen.push(entry.conversation.chat.id);
        }
        match found.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen, trashed);
    assert!(!seen.contains(&kept) && !seen.contains(&bot.main_chat));
    // Once its Bot is not active, its own Restore is refused: the Bot first.
    repo.fixture_set_bot_lifecycle(bot.id, BotLifecycle::Archived)
        .unwrap();
    assert!(
        page(&repo, &bot)
            .chats
            .iter()
            .all(|entry| entry.restore == BotChatRestore::Bot)
    );
    repo.fixture_set_bot_lifecycle(bot.id, BotLifecycle::Trashed)
        .unwrap();
    assert_eq!(page(&repo, &bot).chats.len(), 5);
}

#[test]
fn another_owner_or_workspace_never_appears_and_the_limit_is_bounded() {
    let (mut repo, bot, profile) = setup();
    let mine = additional(&mut repo, &bot, profile, 2);
    trash(&repo, &bot, mine);
    // Another owner's Bot, and another workspace of the same owner.
    let other = fixture_bot();
    repo.save_bot(&other, profile, BotRequestId::mint(), "create", None)
        .unwrap();
    trash(&repo, &other, other.main_chat);
    let mut elsewhere = fixture_bot();
    elsewhere.user_scope = bot.user_scope;
    elsewhere.owner_device = bot.owner_device;
    repo.save_bot(&elsewhere, profile, BotRequestId::mint(), "create", None)
        .unwrap();
    trash(&repo, &elsewhere, elsewhere.main_chat);
    let found = page(&repo, &bot);
    assert_eq!(
        found
            .chats
            .iter()
            .map(|c| c.conversation.chat.id)
            .collect::<Vec<_>>(),
        vec![mine]
    );
    // The same workspace under another device of the same scope: not mine.
    let stranger = BotOwner {
        user_scope: bot.user_scope,
        device: DeviceId::from_uuid(Uuid::new_v4()),
    };
    assert!(
        repo.trashed_bot_chat_page(stranger, bot.home_workspace, None, 100)
            .unwrap()
            .chats
            .is_empty()
    );
    for limit in [0, BOT_CHAT_PAGE_LIMIT + 1] {
        assert_eq!(
            repo.trashed_bot_chat_page(owner_of(&bot), bot.home_workspace, None, limit),
            Err(BotError::Invalid)
        );
    }
}

/// Thousands of conversations and lifecycle operations that are not in
/// Trash: the page walks only what is in Trash now, in its primary-key
/// order, and reaches everything else by primary key.
#[test]
fn a_trash_page_reads_only_what_is_in_trash_now_never_history() {
    let (mut repo, bot, profile) = setup();
    for n in 0..300 {
        let chat = additional(&mut repo, &bot, profile, 100 + n);
        // Trashed and restored: only its operation remains, as history.
        trash(&repo, &bot, chat);
        repo.connection
            .execute(
                "DELETE FROM bot_chat_trash WHERE chat_id=?1",
                [chat.to_storage()],
            )
            .unwrap();
    }
    let chat = additional(&mut repo, &bot, profile, 5);
    trash(&repo, &bot, chat);
    let (scope, device, workspace) = (
        bot.user_scope.to_storage(),
        bot.owner_device.to_storage(),
        bot.home_workspace.to_storage(),
    );
    let cursor: Option<String> = None;
    let found = plan(
        &repo.connection,
        TRASHED_BOT_CHAT_PAGE_SQL,
        &[&scope, &device, &workspace, &cursor, &101i64],
    );
    assert!(
        found.contains("sqlite_autoindex_bot_chat_trash_1"),
        "{found}"
    );
    for primary in [
        "SEARCH bc USING",
        "SEARCH b USING",
        "SEARCH c USING",
        "SEARCH i USING",
    ] {
        assert!(found.contains(primary), "{primary} missing: {found}");
    }
    for walk in ["SCAN bc", "SCAN b ", "SCAN c", "SCAN i", "TEMP B-TREE"] {
        assert!(!found.contains(walk), "{walk}: {found}");
    }
    assert_eq!(page(&repo, &bot).chats.len(), 1);
}
