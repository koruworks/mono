// SPDX-License-Identifier: MPL-2.0

//! The Trash's Bot rows, driven only by what the owner's two reads answer —
//! `list_lifecycle` (trashed) with each Bot's holder, as
//! `WorkerState::list_bot_lifecycle` fills it in through
//! `with_lifecycle_holders`, and `list_trashed_bot_chats` with what hid each
//! conversation (`apps/koru-ai-server/src/worker/bots/{lifecycle,reads}.rs`).
//! Every feed here is made fresh, as after a restart: nothing of this
//! window's own is remembered.

use super::*;
use crate::bots::link::Listing;
use crate::bots::tests::trashed_chat;
use koru_ai_core::bots::{BotLifecycleHolder, BotLifecycleProgress, BotLifecycleStage};
use std::sync::mpsc::{Receiver, SyncSender};

fn feed() -> (BotTrash, Receiver<Command>, SyncSender<Answer>, WorkspaceId) {
    let (link, commands, answers) = BotsLink::test_pair();
    let mut trash = BotTrash::with_link(link);
    let home = WorkspaceId::new();
    trash.set_workspace(Some(home));
    (trash, commands, answers, home)
}

/// A trashed Bot as the Trash filter lists it: `held_by` names its Trash
/// when `held` is given, as `with_lifecycle_holders` does; none otherwise.
fn trashed(home: WorkspaceId, name: &str, held: Option<BotRequestId>) -> Bot {
    let mut bot = crate::bots::tests::bot(BotId::mint(), home, name, true);
    bot.lifecycle = BotLifecycle::Trashed;
    bot.revision = 6;
    bot.updated_at_ms = 50;
    bot.held_by = held.map(|request_id| BotLifecycleHolder {
        request_id,
        kind: BotLifecycleKind::Trash,
        completed_at_ms: 70,
    });
    bot
}

/// Answer the feed's two reads with `bots` and `chats`.
fn listed(
    trash: &mut BotTrash,
    commands: &Receiver<Command>,
    answers: &SyncSender<Answer>,
    bots: Vec<Bot>,
    chats: Vec<koru_ai_core::bots::BotTrashedChat>,
) {
    trash.maintain();
    let mut bots = Some(bots);
    let mut chats = Some(chats);
    while let Ok(command) = commands.try_recv() {
        match command {
            Command::Lifecycle {
                seq,
                workspace,
                lifecycle,
            } => {
                assert_eq!(lifecycle, BotLifecycle::Trashed);
                answers
                    .send(Answer::Lifecycle {
                        seq,
                        workspace,
                        lifecycle,
                        result: Ok(bots.take().expect("once")),
                    })
                    .unwrap();
            }
            Command::TrashedChats { seq, workspace } => answers
                .send(Answer::TrashedChats {
                    seq,
                    workspace,
                    result: Ok(Listing::all(chats.take().expect("once"))),
                })
                .unwrap(),
            _ => panic!("the Trash's two reads"),
        }
    }
    assert!(trash.maintain());
}

/// **A hidden Trash asks nothing**; shown, it asks the owner's Trash filter
/// and its conversations in Trash, once.
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
    assert!(matches!(
        commands.try_recv(),
        Ok(Command::TrashedChats { .. })
    ));
    trash.maintain();
    assert!(commands.try_recv().is_err(), "once");
    assert_eq!(trash.work().commands, 2);
}

/// **Rows for a trashed Bot and a conversation trashed on its own**, with
/// Restore exactly where the owner names what to undo: a Bot whose holder
/// is named; a conversation the owner says its own Restore brings back. A
/// conversation that went with its Bot's Trash is the Bot's row; one whose
/// Bot is archived or in Trash waits for its Bot.
#[test]
fn rows_say_what_is_in_trash_and_which_can_be_restored() {
    let (mut trash, commands, answers, home) = feed();
    trash.set_shown(true, None);
    let known = trashed(home, "Scout", Some(BotRequestId::mint()));
    let unknown = trashed(home, "Older", None);
    let live = crate::bots::tests::bot(BotId::mint(), home, "Writer", false);
    let chat = ChatId::new();
    let waiting = ChatId::new();
    listed(
        &mut trash,
        &commands,
        &answers,
        vec![known.clone(), unknown.clone()],
        vec![
            trashed_chat(
                live.id,
                "Writer",
                chat,
                "Notes",
                (BotRequestId::mint(), BotLifecycleKind::TrashChat),
                BotChatRestore::Conversation,
            ),
            // Went with Scout's own Trash: Scout's row brings it back.
            trashed_chat(
                known.id,
                "Scout",
                ChatId::new(),
                "Scout main",
                (BotRequestId::mint(), BotLifecycleKind::Trash),
                BotChatRestore::Bot,
            ),
            // Trashed on its own, while its Bot is in Trash too.
            trashed_chat(
                unknown.id,
                "Older",
                waiting,
                "Draft",
                (BotRequestId::mint(), BotLifecycleKind::TrashChat),
                BotChatRestore::Bot,
            ),
        ],
    );
    let rows = trash.rows();
    assert_eq!(rows.len(), 4, "two Bots, two conversations on their own");
    let notes = rows
        .iter()
        .find(|row| row.subject == BotSubject::Chat { bot: live.id, chat })
        .unwrap();
    assert_eq!(notes.name, "Notes");
    assert_eq!(notes.origin, "Conversation with Writer");
    assert!(notes.restorable);
    let draft = rows
        .iter()
        .find(|row| {
            row.subject
                == BotSubject::Chat {
                    bot: unknown.id,
                    chat: waiting,
                }
        })
        .unwrap();
    assert!(!draft.restorable, "its Bot comes back first");
    let scout = rows
        .iter()
        .find(|row| row.subject == BotSubject::Bot(known.id))
        .unwrap();
    assert!(scout.restorable);
    assert_eq!(scout.origin, "Bot");
    assert_eq!(scout.trashed_at_ms, Some(70), "when its Trash completed");
    let older = rows
        .iter()
        .find(|row| row.subject == BotSubject::Bot(unknown.id))
        .unwrap();
    assert!(
        !older.restorable,
        "no Restore where the owner names no Trash"
    );
}

/// **A Bot's Restore names the Trash the owner says holds it**, at the
/// listed revision, from a fresh feed; its completion is said; nothing is
/// claimed before the answer.
#[test]
fn a_bots_restore_names_its_trash_and_is_said_when_done() {
    let (mut trash, commands, answers, home) = feed();
    trash.set_shown(true, None);
    let source = BotRequestId::mint();
    let bot = trashed(home, "Scout", Some(source));
    listed(
        &mut trash,
        &commands,
        &answers,
        vec![bot.clone()],
        Vec::new(),
    );
    assert!(trash.restore(BotSubject::Bot(bot.id)));
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
    let receipts = trash.take_receipts();
    assert_eq!(
        receipts[0].words,
        "Restored “Scout”. It's paused until you resume it."
    );
}

/// **A conversation's Restore names what hid it**, read from the owner's
/// listing — asked for first when the feed had not listed it, as from a
/// link's receipt with no Trash shown — then reads its Bot for the
/// revision and sends `RestoreChat`; a Bot read as not found (in Trash, as
/// `readable_owned_bot` answers it) says the Bot must be restored first.
#[test]
fn a_conversations_restore_names_what_hid_it_after_a_restart() {
    let (mut trash, commands, answers, home) = feed();
    let live = crate::bots::tests::bot(BotId::mint(), home, "Writer", false);
    let chat = ChatId::new();
    let source = BotRequestId::mint();
    let entry = trashed_chat(
        live.id,
        "Writer",
        chat,
        "Notes",
        (source, BotLifecycleKind::TrashChat),
        BotChatRestore::Conversation,
    );
    // From a link's receipt: the Trash is not shown, nothing is listed.
    assert!(trash.restore(BotSubject::Chat { bot: live.id, chat }));
    let Ok(Command::TrashedChats { seq, workspace }) = commands.try_recv() else {
        panic!("what hid it is read first");
    };
    answers
        .send(Answer::TrashedChats {
            seq,
            workspace,
            result: Ok(Listing::all(vec![entry])),
        })
        .unwrap();
    trash.maintain();
    let Ok(Command::Read { seq, bot }) = commands.try_recv() else {
        panic!("then the Bot, for its revision");
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
    // While it is on its way a second press sends nothing.
    assert!(!trash.restore(BotSubject::Chat { bot: live.id, chat }));
    answers
        .send(Answer::Wrote {
            bot: live.id,
            request,
            result: Err(Failure::lost()),
        })
        .unwrap();
    trash.maintain();
    let _ = trash.take_receipts();
    // The Bot itself in Trash: said, never sent.
    assert!(trash.restore(BotSubject::Chat { bot: live.id, chat }));
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
        "“Notes” can be restored once its bot is restored from the Trash."
    );
}

/// **One Restore per row at a time**: a second press while the first is
/// unanswered sends nothing, so the owner's refusal of a used source
/// (`require_source` in `worker/bots/lifecycle.rs` answers a second Restore
/// of one Trash `request_conflict`) can never become the receipt that
/// follows a Restore that succeeded.
#[test]
fn a_second_restore_press_sends_nothing_while_the_first_is_on_its_way() {
    let (mut trash, commands, answers, home) = feed();
    trash.set_shown(true, None);
    let source = BotRequestId::mint();
    let bot = trashed(home, "Scout", Some(source));
    listed(
        &mut trash,
        &commands,
        &answers,
        vec![bot.clone()],
        Vec::new(),
    );
    assert!(trash.restore(BotSubject::Bot(bot.id)));
    assert!(!trash.restore(BotSubject::Bot(bot.id)), "nothing more");
    let writes: Vec<_> = commands
        .try_iter()
        .filter_map(|command| match command {
            Command::Write { request, .. } => Some(request),
            _ => None,
        })
        .collect();
    assert_eq!(writes.len(), 1, "one Restore went");
    answers
        .send(Answer::Wrote {
            bot: bot.id,
            request: writes[0],
            result: Ok(BotResponse::Lifecycle {
                operation: BotLifecycleProgress {
                    request_id: writes[0],
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
    let receipts = trash.take_receipts();
    assert_eq!(receipts.len(), 1);
    assert!(
        receipts[0].words.starts_with("Restored"),
        "{}",
        receipts[0].words
    );
}

/// **A link's Restore of a conversation that went with its Bot's Trash
/// says the Bot comes back first** — never nothing.
#[test]
fn a_link_restore_of_a_conversation_hidden_by_its_bots_trash_says_why() {
    let (mut trash, commands, answers, _home) = feed();
    let bot = BotId::mint();
    let chat = ChatId::new();
    let entry = trashed_chat(
        bot,
        "Writer",
        chat,
        "Notes",
        (BotRequestId::mint(), BotLifecycleKind::Trash),
        BotChatRestore::Bot,
    );
    assert!(trash.restore(BotSubject::Chat { bot, chat }));
    let Ok(Command::TrashedChats { seq, workspace }) = commands.try_recv() else {
        panic!("listing read first");
    };
    answers
        .send(Answer::TrashedChats {
            seq,
            workspace,
            result: Ok(Listing::all(vec![entry])),
        })
        .unwrap();
    trash.maintain();
    assert!(
        commands.try_recv().is_err(),
        "nothing the owner would refuse"
    );
    let receipt = trash.take_receipts().pop().expect("said");
    assert_eq!(
        receipt.words,
        "“Notes” can be restored once its bot is restored from the Trash."
    );
}

/// **A Restore of a Trash another surface restored first, or of a Bot
/// another tab changed since the Trash listed it,** is refused as the
/// owner refuses it — `require_source`
/// (`crates/koru-ai-sqlite/src/repository/bots/lifecycle.rs`) answers
/// `request_conflict` for a source an intent already names, and
/// `capture_on` answers `revision_conflict` when the revision moved — and
/// the receipt never says it was not restored: it was restored already or
/// it changed elsewhere, in one whole template per language; the Trash
/// reads again and says which.
#[test]
fn a_restore_of_a_used_source_says_it_was_restored_already() {
    let (mut trash, commands, answers, home) = feed();
    trash.set_shown(true, None);
    let source = BotRequestId::mint();
    let bot = trashed(home, "Scout", Some(source));
    listed(
        &mut trash,
        &commands,
        &answers,
        vec![bot.clone()],
        Vec::new(),
    );
    for (lang, code, said) in [
        (
            Lang::En,
            "request_conflict",
            "“Scout” was already restored, or changed somewhere else. The Trash shows it as it is now.",
        ),
        (
            Lang::Ja,
            "request_conflict",
            "「Scout」はすでに復元されたか、別の場所で変更されました。ゴミ箱には現在の状態が表示されます。",
        ),
        (
            Lang::En,
            "revision_conflict",
            "“Scout” was already restored, or changed somewhere else. The Trash shows it as it is now.",
        ),
    ] {
        trash.set_lang(lang);
        assert!(trash.restore(BotSubject::Bot(bot.id)));
        let request = commands
            .try_iter()
            .find_map(|command| match command {
                Command::Write { request, .. } => Some(request),
                _ => None,
            })
            .expect("a Restore");
        answers
            .send(Answer::Wrote {
                bot: bot.id,
                request,
                result: Err(Failure::Refused(code.into())),
            })
            .unwrap();
        trash.maintain();
        let receipt = trash.take_receipts().pop().unwrap().words;
        assert_eq!(receipt, said, "{code}");
        assert!(!receipt.contains("wasn't restored"), "{code}: {receipt}");
        // It reads again, to show what the owner holds now.
        trash.maintain();
        assert!(
            commands
                .try_iter()
                .any(|command| matches!(command, Command::Lifecycle { .. })),
            "read again"
        );
    }
}

/// The one Restore the feed just sent, by its request.
fn restore_sent(commands: &Receiver<Command>) -> BotRequestId {
    commands
        .try_iter()
        .find_map(|command| match command {
            Command::Write { request, .. } => Some(request),
            _ => None,
        })
        .expect("a Restore went")
}

/// A Restore's answer as `restore_trashed_bot` gives it: completed in one
/// transaction.
fn restore_completed(bot: &Bot, request: BotRequestId, source: BotRequestId) -> Answer {
    Answer::Wrote {
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
    }
}

/// **A Restore that answered takes its row with it at once** — the second
/// review's proof, turned round. Before, the row stayed listed until the
/// re-list answered, so a late second press sent a second Restore that the
/// owner refused (`require_source`) and the last receipt said the Bot was
/// not restored. Now the row is gone the moment the first answers, and a
/// second press sends nothing.
#[test]
fn a_second_press_after_a_restore_answered_sends_nothing() {
    let (mut trash, commands, answers, home) = feed();
    trash.set_shown(true, None);
    let source = BotRequestId::mint();
    let bot = trashed(home, "Scout", Some(source));
    listed(
        &mut trash,
        &commands,
        &answers,
        vec![bot.clone()],
        Vec::new(),
    );
    assert!(trash.restore(BotSubject::Bot(bot.id)));
    let first = restore_sent(&commands);
    answers
        .send(restore_completed(&bot, first, source))
        .unwrap();
    trash.maintain();
    let said = trash.take_receipts();
    assert_eq!(said.len(), 1);
    assert!(said[0].words.starts_with("Restored"), "{}", said[0].words);
    assert!(
        trash
            .rows()
            .iter()
            .all(|row| row.subject != BotSubject::Bot(bot.id)),
        "its row is gone before the re-list answers"
    );
    assert!(
        !trash.restore(BotSubject::Bot(bot.id)),
        "a late second press"
    );
    assert!(
        !commands
            .try_iter()
            .any(|command| matches!(command, Command::Write { .. })),
        "no second Restore"
    );
}

/// **A Restore Koru AI did not answer is not said as not restored** — the
/// real link reads a lost connection as unanswered (`Failure::of`): the
/// Restore may well have happened, so the receipt says it is not
/// confirmed, and the Trash reads again to show which.
#[test]
fn an_unanswered_restore_is_said_as_unconfirmed_and_read_again() {
    use crate::bots::link::{Said, wrote};
    let (mut trash, commands, answers, home) = feed();
    trash.set_shown(true, None);
    let source = BotRequestId::mint();
    let bot = trashed(home, "Scout", Some(source));
    listed(
        &mut trash,
        &commands,
        &answers,
        vec![bot.clone()],
        Vec::new(),
    );
    for code in ["gateway_unavailable", "ai_worker_timeout"] {
        assert!(trash.restore(BotSubject::Bot(bot.id)));
        let request = restore_sent(&commands);
        answers
            .send(Answer::Wrote {
                bot: bot.id,
                request,
                result: wrote(Err(Said::new(code, true))),
            })
            .unwrap();
        trash.maintain();
        let receipt = trash.take_receipts().pop().expect("said").words;
        assert_eq!(
            receipt,
            "Koru AI didn't answer, so restoring “Scout” isn't confirmed. The Trash shows whether it came back.",
            "{code}"
        );
        assert!(!receipt.contains("wasn't restored"), "{code}");
        trash.maintain();
        assert!(
            commands
                .try_iter()
                .any(|command| matches!(command, Command::Lifecycle { .. })),
            "{code}: read again"
        );
    }
}

/// **No receipt puts one sentence inside another** (the second review's
/// item 6): every Restore receipt is one whole template with the row's
/// name as its only slot, in every language.
#[test]
fn every_restore_receipt_is_one_template_with_the_name_alone() {
    for key in [
        Words::TrashBotRestoreRefused,
        Words::TrashBotRestoreUnconfirmed,
        Words::TrashBotRestoreChanged,
        Words::TrashBotRestoreBotFirst,
        Words::TrashBotRestoreUnarchiveFirst,
        Words::TrashBotRestoreLater,
        Words::TrashBotRestoreBusy,
        Words::TrashBotRestoreNotSent,
    ] {
        for lang in Lang::ALL {
            let words = t(lang, key);
            assert_eq!(words.matches("{}").count(), 1, "{key:?} {lang:?}: {words}");
        }
    }
}
