// SPDX-License-Identifier: MPL-2.0
use super::super::tests::fixture_bot;
use super::*;

const HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn setup() -> (Repository, Bot, ProfileId) {
    let mut repo = Repository::in_memory().unwrap();
    let bot = fixture_bot();
    let profile = ProfileId::new();
    repo.save_bot(&bot, profile, BotRequestId::mint(), "create", None)
        .unwrap();
    (repo, bot, profile)
}

/// An additional conversation last touched at `updated`.
fn additional(repo: &mut Repository, bot: &Bot, profile: ProfileId, updated: i64) -> ChatId {
    let reference = repo
        .create_bot_chat(
            bot,
            profile,
            BotRequestId::mint(),
            "additional",
            Timestamp::from_millis(updated),
        )
        .unwrap();
    reference.chat_id
}

/// A conversation in Trash, through a completed single-conversation operation.
fn trash(repo: &Repository, bot: &Bot, chat: ChatId) {
    let operation = BotRequestId::mint().to_storage();
    let scope = bot.user_scope.to_storage();
    repo.connection
        .execute(
            "INSERT INTO bot_lifecycle_intents(user_scope_id,operation_id,bot_id,owner_device_id,kind,chat_id,fingerprint,expected_revision,prior_lifecycle,stage,requested_at_ms,updated_at_ms,completed_at_ms) VALUES(?1,?2,?3,?4,'trash_chat',?5,?6,1,'active','completed',1,1,1)",
            params![scope, operation, bot.id.to_storage(), bot.owner_device.to_storage(), chat.to_storage(), HASH],
        )
        .unwrap();
    repo.connection
        .execute(
            "INSERT INTO bot_chat_trash(chat_id,user_scope_id,operation_id,trashed_at_ms) VALUES(?1,?2,?3,1)",
            params![chat.to_storage(), scope, operation],
        )
        .unwrap();
}

fn ids(page: &[BotConversation]) -> Vec<ChatId> {
    page.iter().map(|c| c.chat.id).collect()
}

#[test]
fn a_page_lists_main_first_then_newest_and_pages_without_gaps_or_repeats() {
    let (mut repo, bot, profile) = setup();
    // The main conversation is the oldest and still comes first.
    let a = additional(&mut repo, &bot, profile, 10);
    let b = additional(&mut repo, &bot, profile, 30);
    let c = additional(&mut repo, &bot, profile, 20);
    // Two at the same moment are ordered by ID, so the cursor stays exact.
    let d = additional(&mut repo, &bot, profile, 20);
    let (mut tied_low, mut tied_high) = (c, d);
    if tied_low.to_storage() > tied_high.to_storage() {
        std::mem::swap(&mut tied_low, &mut tied_high);
    }
    let expected = vec![bot.main_chat, b, tied_low, tied_high, a];

    let BotChatPage {
        chats: all,
        next_cursor: next,
    } = repo.bot_chat_page(bot.id, None, 100).unwrap();
    assert_eq!(ids(&all), expected);
    assert_eq!(next, None);
    assert_eq!(all[0].label.purpose, BotChatPurpose::Main);
    assert!(
        all[1..]
            .iter()
            .all(|c| c.label.purpose == BotChatPurpose::Additional)
    );
    assert!(
        all.iter()
            .all(|c| c.label.bot_id == bot.id && c.label.bot_name == bot.config.name)
    );

    let mut walked = Vec::new();
    let mut cursor = None;
    loop {
        let BotChatPage {
            chats: page,
            next_cursor: next,
        } = repo.bot_chat_page(bot.id, cursor.as_ref(), 2).unwrap();
        assert!(page.len() <= 2);
        walked.extend(ids(&page));
        match next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(walked, expected);
}

#[test]
fn trash_other_bots_and_ordinary_chats_stay_out_of_a_page() {
    let (mut repo, bot, profile) = setup();
    let kept = additional(&mut repo, &bot, profile, 5);
    let trashed = additional(&mut repo, &bot, profile, 6);
    trash(&repo, &bot, trashed);
    trash(&repo, &bot, bot.main_chat);
    let mut other = fixture_bot();
    other.home_workspace = bot.home_workspace;
    other.config.role.workspace = bot.home_workspace;
    repo.save_bot(&other, profile, BotRequestId::mint(), "create-other", None)
        .unwrap();
    let ordinary = ChatId::new();
    repo.create_chat(&super::super::super::tests::chat(ordinary, 100))
        .unwrap();
    let BotChatPage {
        chats: page,
        next_cursor: next,
    } = repo.bot_chat_page(bot.id, None, 100).unwrap();
    // A trashed main is not listed and nothing stands in for it (O7).
    assert_eq!(ids(&page), vec![kept]);
    assert_eq!(next, None);
    let BotChatPage { chats: others, .. } = repo.bot_chat_page(other.id, None, 100).unwrap();
    assert_eq!(ids(&others), vec![other.main_chat]);
}

#[test]
fn a_page_limit_outside_its_bound_refuses() {
    let (repo, bot, _) = setup();
    assert_eq!(
        repo.bot_chat_page(bot.id, None, 0).unwrap_err(),
        BotError::Invalid
    );
    assert_eq!(
        repo.bot_chat_page(bot.id, None, BOT_CHAT_PAGE_LIMIT + 1)
            .unwrap_err(),
        BotError::Invalid
    );
    assert!(
        repo.bot_chat_page(bot.id, None, BOT_CHAT_PAGE_LIMIT)
            .is_ok()
    );
}

fn plan(conn: &Connection, sql: &str, args: &[&dyn rusqlite::ToSql]) -> String {
    let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    statement
        .query_map(args, |r| r.get::<_, String>(3))
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Thousands of ordinary chats, all newer than the Bot's: a page that drove
/// from `chats` would walk every one of them first.
#[test]
fn a_page_reads_the_association_and_primary_keys_never_the_chat_history() {
    let (mut repo, bot, profile) = setup();
    for n in 0..2000 {
        repo.create_chat(&super::super::super::tests::chat(ChatId::new(), 1000 + n))
            .unwrap();
    }
    additional(&mut repo, &bot, profile, 2);
    let id = bot.id.to_storage();
    let rank: Option<i64> = None;
    let cursor_at: Option<i64> = None;
    let cursor_id: Option<String> = None;
    let found = plan(
        &repo.connection,
        BOT_CHAT_PAGE_SQL,
        &[&id, &rank, &cursor_at, &cursor_id, &101i64],
    );
    assert!(found.contains("SCAN bc"), "{found}");
    for primary in [
        "SEARCH c USING INDEX",
        "SEARCH b USING INDEX",
        "SEARCH t USING",
    ] {
        assert!(found.contains(primary), "{primary} missing: {found}");
    }
    assert!(!found.contains("SCAN c"), "{found}");
    assert!(!found.contains("chats_by_updated"), "{found}");
    let BotChatPage { chats: page, .. } = repo.bot_chat_page(bot.id, None, 100).unwrap();
    assert_eq!(page.len(), 2);
}
