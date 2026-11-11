// SPDX-License-Identifier: MPL-2.0

//! The Trash's Bot rows, driven only by what the owner's two reads answer —
//! `list_lifecycle` (trashed) with each Bot's holder, as
//! `WorkerState::list_bot_lifecycle` fills it in through
//! `with_lifecycle_holders`, and `list_trashed_bot_chats` with what hid each
//! conversation (`apps/koru-ai-server/src/worker/bots/{lifecycle,reads}.rs`).
//! Every feed here is made fresh, as after a restart: nothing of this
//! window's own is remembered.

use super::*;
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
                    result: Ok(chats.take().expect("once")),
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
            result: Ok(vec![entry]),
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
        "“Notes” wasn't restored. This bot is in the Trash. Restore the bot first."
    );
}
