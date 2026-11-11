// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::bots::lifecycle::Finished;
use koru_ai_core::bots::{BotLifecycleProgress, BotLifecycleStage};
use std::sync::mpsc::{Receiver, SyncSender};

fn feed() -> (BotTrash, Receiver<Command>, SyncSender<Answer>, WorkspaceId) {
    let (link, commands, answers) = BotsLink::test_pair();
    let mut trash = BotTrash::with_link(link);
    let home = WorkspaceId::new();
    trash.set_workspace(Some(home));
    (trash, commands, answers, home)
}

fn trashed(home: WorkspaceId, name: &str) -> Bot {
    let mut bot = crate::bots::tests::bot(BotId::mint(), home, name, true);
    bot.lifecycle = BotLifecycle::Trashed;
    bot.revision = 6;
    bot.updated_at_ms = 50;
    bot
}

fn trash_finished(bot: &Bot, request: BotRequestId) -> Finished {
    Finished {
        bot: bot.id,
        name: bot.config.name.clone(),
        home: Some(bot.home_workspace),
        kind: BotLifecycleKind::Trash,
        request,
        reverses: None,
        chat: None,
        at_ms: 70,
    }
}

fn listed(
    trash: &mut BotTrash,
    commands: &Receiver<Command>,
    answers: &SyncSender<Answer>,
    bots: Vec<Bot>,
) {
    trash.maintain();
    let Ok(Command::Lifecycle {
        seq,
        workspace,
        lifecycle,
    }) = commands.try_recv()
    else {
        panic!("the Trash filter is asked");
    };
    assert_eq!(lifecycle, BotLifecycle::Trashed);
    answers
        .send(Answer::Lifecycle {
            seq,
            workspace,
            lifecycle,
            result: Ok(bots),
        })
        .unwrap();
    assert!(trash.maintain());
}

/// **A hidden Trash asks nothing**; shown, it asks the owner's Trash filter
/// once.
#[test]
fn a_hidden_trash_asks_nothing_and_a_shown_one_asks_once() {
    let (mut trash, commands, _answers, _) = feed();
    assert!(!trash.maintain());
    assert!(commands.try_recv().is_err());
    trash.set_shown(true, None);
    trash.maintain();
    assert!(matches!(
        commands.try_recv(),
        Ok(Command::Lifecycle {
            lifecycle: BotLifecycle::Trashed,
            ..
        })
    ));
    trash.maintain();
    assert!(commands.try_recv().is_err(), "once");
    assert_eq!(trash.work().commands, 1);
}

/// **Rows for a trashed Bot and a trashed conversation**, with Restore only
/// where the window knows the Trash it names.
#[test]
fn rows_say_what_is_in_trash_and_which_can_be_restored() {
    let (mut trash, commands, answers, home) = feed();
    trash.set_shown(true, None);
    let known = trashed(home, "Scout");
    let unknown = trashed(home, "Older");
    let mut book = Book::default();
    book.record(&trash_finished(&known, BotRequestId::mint()));
    let live = crate::bots::tests::bot(BotId::mint(), home, "Writer", false);
    let chat = ChatId::new();
    book.record(&Finished {
        bot: live.id,
        name: "Writer".into(),
        home: Some(home),
        kind: BotLifecycleKind::TrashChat,
        request: BotRequestId::mint(),
        reverses: None,
        chat: Some((chat, "Notes".into())),
        at_ms: 90,
    });
    listed(
        &mut trash,
        &commands,
        &answers,
        vec![known.clone(), unknown.clone()],
    );
    let rows = trash.rows(&book);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].subject, BotSubject::Chat { bot: live.id, chat });
    assert_eq!(rows[0].name, "Notes");
    assert_eq!(rows[0].origin, "Conversation with Writer");
    assert!(rows[0].restorable);
    let scout = rows
        .iter()
        .find(|row| row.subject == BotSubject::Bot(known.id))
        .unwrap();
    assert!(scout.restorable);
    assert_eq!(scout.origin, "Bot");
    let older = rows
        .iter()
        .find(|row| row.subject == BotSubject::Bot(unknown.id))
        .unwrap();
    assert!(
        !older.restorable,
        "no Restore for a Trash this window did not do"
    );
}

/// **A Bot's Restore names its Trash at the listed revision**, and its
/// completion is recorded and said; nothing is claimed before the answer.
#[test]
fn a_bots_restore_names_its_trash_and_is_said_when_done() {
    let (mut trash, commands, answers, home) = feed();
    trash.set_shown(true, None);
    let bot = trashed(home, "Scout");
    let source = BotRequestId::mint();
    let mut book = Book::default();
    book.record(&trash_finished(&bot, source));
    listed(&mut trash, &commands, &answers, vec![bot.clone()]);
    assert!(trash.restore(BotSubject::Bot(bot.id), &book));
    let Ok(Command::Write {
        request, command, ..
    }) = commands.try_recv()
    else {
        panic!("a Restore");
    };
    assert_eq!(
        *command,
        BotCommand::Restore {
            request_id: request,
            bot_id: bot.id,
            expected_revision: 6,
            source_request_id: source
        }
    );
    assert!(trash.take_receipts().is_empty(), "nothing claimed yet");
    answers
        .send(Answer::Wrote {
            bot: bot.id,
            request,
            result: Ok(BotResponse::Lifecycle {
                operation: BotLifecycleProgress {
                    request_id: request,
                    bot_id: bot.id,
                    kind: BotLifecycleKind::Restore,
                    chat_id: None,
                    source_request_id: Some(source),
                    stage: BotLifecycleStage::Completed,
                    refusal: None,
                    members: Vec::new(),
                    left_in_place: Vec::new(),
                },
            }),
        })
        .unwrap();
    trash.maintain();
    let finished = trash.take_finished();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].reverses, Some(BotLifecycleKind::Trash));
    book.record(&finished[0]);
    assert_eq!(book.sources(bot.id), (None, None));
    let receipts = trash.take_receipts();
    assert_eq!(
        receipts[0].words,
        "Restored “Scout”. It's paused until you resume it."
    );
}

/// **A conversation's Restore reads its Bot for the revision**, then sends
/// `RestoreChat`; a trashed Bot says the Bot must be restored first.
#[test]
fn a_conversations_restore_reads_its_bot_first() {
    let (mut trash, commands, answers, home) = feed();
    let live = crate::bots::tests::bot(BotId::mint(), home, "Writer", false);
    let chat = ChatId::new();
    let source = BotRequestId::mint();
    let mut book = Book::default();
    book.record(&Finished {
        bot: live.id,
        name: "Writer".into(),
        home: Some(home),
        kind: BotLifecycleKind::TrashChat,
        request: source,
        reverses: None,
        chat: Some((chat, "Notes".into())),
        at_ms: 90,
    });
    // From a link's receipt: the Trash is not shown.
    assert!(trash.restore(BotSubject::Chat { bot: live.id, chat }, &book));
    let Ok(Command::Read { seq, bot }) = commands.try_recv() else {
        panic!("the Bot is read first");
    };
    answers
        .send(Answer::Read {
            seq,
            bot,
            result: Ok(live.clone()),
        })
        .unwrap();
    trash.maintain();
    let Ok(Command::Write {
        command, request, ..
    }) = commands.try_recv()
    else {
        panic!("RestoreChat");
    };
    assert_eq!(
        *command,
        BotCommand::RestoreChat {
            request_id: request,
            bot_id: live.id,
            chat_id: chat,
            expected_revision: live.revision,
            source_request_id: source
        }
    );
    // The Bot itself in Trash: said, never sent.
    assert!(trash.restore(BotSubject::Chat { bot: live.id, chat }, &book));
    let Ok(Command::Read { seq, bot }) = commands.try_recv() else {
        panic!("read");
    };
    answers
        .send(Answer::Read {
            seq,
            bot,
            result: Err(Failure::Refused("not_found".into())),
        })
        .unwrap();
    trash.maintain();
    assert!(commands.try_recv().is_err());
    let receipt = trash.take_receipts().pop().unwrap();
    assert_eq!(
        receipt.words,
        "“Notes” wasn't restored. This bot is in the Trash. Restore the bot first."
    );
}
