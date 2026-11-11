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
