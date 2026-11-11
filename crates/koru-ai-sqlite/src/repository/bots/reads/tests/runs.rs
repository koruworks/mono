// SPDX-License-Identifier: MPL-2.0
//! Binding a public Run read to the reply it names: the reply's own Run, a
//! branch's copy of it, and a copy whose source was purged.
use super::super::super::tests::branch::copy;
use super::*;
use koru_ai_core::MessageId;
use koru_delegate_core::RunId;

/// A Bot whose main conversation has one finished reply written by `run`.
fn replied() -> (Repository, Bot, ProfileId, MessageId, RunId) {
    let (mut repo, bot, profile) = setup();
    let turn = super::super::super::super::tests::complete_turn(
        &mut repo,
        bot.main_chat,
        0,
        "Bot reply",
        10,
    );
    let run = RunId::mint();
    repo.record_bot_chat_run_for_fixture(
        &bot,
        turn.attempt.id,
        bot.main_chat,
        turn.assistant_message.id,
        run,
        bot.home_workspace,
    )
    .unwrap();
    (repo, bot, profile, turn.assistant_message.id, run)
}

#[test]
fn a_reply_binds_its_own_run_and_nothing_else() {
    let (mut repo, bot, profile, reply, run) = replied();
    assert_eq!(
        repo.bound_bot_run(bot.id, bot.main_chat, reply, run)
            .unwrap(),
        Some(bot.home_workspace)
    );
    // Another Run, another Bot, another conversation of the same Bot, a
    // message that is not the reply: all simply unbound.
    let other_chat = additional(&mut repo, &bot, profile, 20);
    let question = repo.messages(bot.main_chat).unwrap()[0].id;
    for (b, c, m, r) in [
        (bot.id, bot.main_chat, reply, RunId::mint()),
        (BotId::mint(), bot.main_chat, reply, run),
        (bot.id, other_chat, reply, run),
        (bot.id, bot.main_chat, question, run),
        (bot.id, bot.main_chat, MessageId::new(), run),
    ] {
        assert_eq!(repo.bound_bot_run(b, c, m, r).unwrap(), None);
    }
    // An ordinary chat's reply is never a Bot Run's, whatever is named.
    let ordinary = ChatId::new();
    repo.create_chat(&super::super::super::super::tests::chat(ordinary, 1))
        .unwrap();
    let plain = super::super::super::super::tests::complete_turn(&mut repo, ordinary, 0, "x", 30);
    assert_eq!(
        repo.bound_bot_run(bot.id, ordinary, plain.assistant_message.id, run)
            .unwrap(),
        None
    );
}

#[test]
fn a_branch_copy_binds_the_source_run_until_the_source_is_purged_then_its_reference_does() {
    let (mut repo, bot, _, reply, run) = replied();
    let first = copy(&repo, &bot, bot.main_chat, reply);
    repo.create_bot_branch(&bot, &first).unwrap();
    let copied = first.messages[1].id;
    let second = copy(&repo, &bot, first.chat.id, copied);
    repo.create_bot_branch(&bot, &second).unwrap();
    let twice = second.messages[1].id;
    assert_eq!(
        repo.bound_bot_run(bot.id, first.chat.id, copied, run)
            .unwrap(),
        Some(bot.home_workspace)
    );
    assert_eq!(
        repo.bound_bot_run(bot.id, second.chat.id, twice, run)
            .unwrap(),
        Some(bot.home_workspace)
    );
    // The copy is named in its own conversation, not in the source's.
    assert_eq!(
        repo.bound_bot_run(bot.id, bot.main_chat, copied, run)
            .unwrap(),
        None
    );
    assert_eq!(
        repo.bound_bot_run(bot.id, first.chat.id, first.messages[0].id, run)
            .unwrap(),
        None
    );

    // The source is purged: its binding goes, the first copy keeps an inert
    // reference, and the copy of the copy (no reference of its own) ends.
    repo.purge_bot_chat_run_source_for_fixture(run, first.chat.id, copied)
        .unwrap();
    assert_eq!(repo.historical_bot_run(copied).unwrap(), None);
    assert_eq!(
        repo.bound_bot_run(bot.id, first.chat.id, copied, run)
            .unwrap(),
        Some(bot.home_workspace)
    );
    assert_eq!(
        repo.bound_bot_run(bot.id, bot.main_chat, reply, run)
            .unwrap(),
        None
    );
    // The reference is keyed by its own conversation, message and Run.
    for (b, c, m, r) in [
        (BotId::mint(), first.chat.id, copied, run),
        (bot.id, first.chat.id, copied, RunId::mint()),
        (bot.id, second.chat.id, copied, run),
        (bot.id, second.chat.id, twice, run),
    ] {
        assert_eq!(repo.bound_bot_run(b, c, m, r).unwrap(), None);
    }
}

#[test]
fn binding_seeks_the_run_and_the_reference_by_their_keys() {
    let (repo, bot, _, reply, run) = replied();
    let (run_id, bot_id, message) = (run.to_storage(), bot.id.to_storage(), reply.to_storage());
    let found = plan(
        &repo.connection,
        BOT_RUN_RECORD_SQL,
        &[&run_id, &bot_id, &message],
    );
    assert!(
        found.contains("USING INDEX sqlite_autoindex_bot_chat_runs") && found.contains("run_id=?"),
        "{found}"
    );
    let chat = bot.main_chat.to_storage();
    let found = plan(
        &repo.connection,
        BOT_HISTORICAL_RUN_SQL,
        &[&chat, &message, &run_id, &bot_id],
    );
    assert!(
        found.contains("sqlite_autoindex_bot_historical_run_refs_1")
            && found.contains("chat_id=? AND message_id=? AND run_id=?"),
        "{found}"
    );
}

/// What the transcript reports for `chat`'s messages, each pair checked
/// against the binding `read_run` uses: the round trip at this layer.
fn reported(repo: &Repository, bot: &Bot, chat: ChatId) -> Vec<(MessageId, RunId)> {
    let messages: Vec<MessageId> = repo.messages(chat).unwrap().iter().map(|m| m.id).collect();
    let runs = repo.bot_reply_runs(bot.id, chat, &messages).unwrap();
    for (message, run) in &runs {
        assert!(
            repo.bound_bot_run(bot.id, chat, *message, *run)
                .unwrap()
                .is_some(),
            "a reported Run is one read_run binds"
        );
    }
    runs
}

#[test]
fn each_reply_reports_the_run_read_run_accepts_and_nothing_else() {
    let (mut repo, bot, profile, reply, run) = replied();
    assert_eq!(reported(&repo, &bot, bot.main_chat), vec![(reply, run)]);
    // A Run still being submitted, or refused before admission, is not
    // reported; once admitted it is.
    for stage in ["prepared", "rejected"] {
        repo.set_bot_run_receipt_stage_for_fixture(run, stage)
            .unwrap();
        assert!(reported(&repo, &bot, bot.main_chat).is_empty(), "{stage}");
    }
    repo.set_bot_run_receipt_stage_for_fixture(run, "completed")
        .unwrap();
    // A reply with no Run, another Bot, another conversation: nothing.
    let other_chat = additional(&mut repo, &bot, profile, 20);
    super::super::super::super::tests::complete_turn(&mut repo, other_chat, 0, "x", 30);
    assert!(reported(&repo, &bot, other_chat).is_empty());
    assert!(
        repo.bot_reply_runs(BotId::mint(), bot.main_chat, &[reply])
            .unwrap()
            .is_empty()
    );
    assert!(
        repo.bot_reply_runs(bot.id, other_chat, &[])
            .unwrap()
            .is_empty()
    );

    // Copies report the source's Run; after the purge only the copy that
    // keeps a reference does.
    let first = copy(&repo, &bot, bot.main_chat, reply);
    repo.create_bot_branch(&bot, &first).unwrap();
    let copied = first.messages[1].id;
    let second = copy(&repo, &bot, first.chat.id, copied);
    repo.create_bot_branch(&bot, &second).unwrap();
    let twice = second.messages[1].id;
    assert_eq!(reported(&repo, &bot, first.chat.id), vec![(copied, run)]);
    assert_eq!(reported(&repo, &bot, second.chat.id), vec![(twice, run)]);
    repo.purge_bot_chat_run_source_for_fixture(run, first.chat.id, copied)
        .unwrap();
    assert_eq!(reported(&repo, &bot, first.chat.id), vec![(copied, run)]);
    assert!(reported(&repo, &bot, second.chat.id).is_empty());
    assert!(reported(&repo, &bot, bot.main_chat).is_empty());
}

#[test]
fn reply_runs_seek_by_keys_only() {
    let (repo, bot, _, reply, run) = replied();
    let (run_id, message, chat, bot_id) = (
        run.to_storage(),
        reply.to_storage(),
        bot.main_chat.to_storage(),
        bot.id.to_storage(),
    );
    let found = plan(&repo.connection, BOT_RUN_ADMITTED_SQL, &[&run_id]);
    assert!(
        found.contains("SEARCH r USING INDEX sqlite_autoindex_bot_chat_runs")
            && found.contains("(run_id=?)")
            && found.contains("SEARCH c USING INDEX sqlite_autoindex_bot_command_receipts")
            && found.contains("(user_scope_id=? AND request_id=?)")
            && !found.contains("SCAN r")
            && !found.contains("SCAN c"),
        "{found}"
    );
    let found = plan(
        &repo.connection,
        BOT_REPLY_HISTORICAL_RUN_SQL,
        &[&chat, &message, &bot_id],
    );
    assert!(
        found.contains("sqlite_autoindex_bot_historical_run_refs_1")
            && found.contains("chat_id=? AND message_id=?"),
        "{found}"
    );
    let hops = 32i64;
    let found = plan(
        &repo.connection,
        super::super::super::branch::HISTORICAL_BOT_RUN_SQL,
        &[&message, &hops],
    );
    assert!(
        found.contains("sqlite_autoindex_messages_1")
            || found.contains("USING INTEGER PRIMARY KEY")
            || found.contains("SEARCH m USING"),
        "{found}"
    );
    assert!(
        found.contains("SEARCH r USING INDEX sqlite_autoindex_bot_chat_runs"),
        "{found}"
    );
}

/// A reference is consulted only when the chain names no Run of this Bot:
/// with the source alive, a stray reference to another Run binds nothing.
#[test]
fn a_reference_never_overrides_the_run_the_chain_names() {
    let (mut repo, bot, _, reply, run) = replied();
    let first = copy(&repo, &bot, bot.main_chat, reply);
    repo.create_bot_branch(&bot, &first).unwrap();
    let copied = first.messages[1].id;
    let stray = RunId::mint();
    repo.connection
        .execute(
            "INSERT INTO bot_historical_run_refs(chat_id,message_id,source_chat_id,source_message_id,bot_id,run_id,record_workspace_id,recorded_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,1)",
            params![
                first.chat.id.to_storage(),
                copied.to_storage(),
                bot.main_chat.to_storage(),
                reply.to_storage(),
                bot.id.to_storage(),
                stray.to_storage(),
                bot.home_workspace.to_storage()
            ],
        )
        .unwrap();
    assert_eq!(
        repo.bound_bot_run(bot.id, first.chat.id, copied, stray)
            .unwrap(),
        None
    );
    assert_eq!(
        repo.bound_bot_run(bot.id, first.chat.id, copied, run)
            .unwrap(),
        Some(bot.home_workspace)
    );
    assert_eq!(reported(&repo, &bot, first.chat.id), vec![(copied, run)]);
}
