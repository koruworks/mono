// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_ai_core::bots::{BotLifecycleRefusal, BotOpenLifecycle};
use std::time::{Duration, Instant};

fn fixture() -> (Lifecycle, Bot) {
    let bot = crate::bots::tests::bot(BotId::mint(), WorkspaceId::new(), "Scout", false);
    let mut model = Lifecycle::default();
    model.set_bot(bot.id);
    model.know(&bot);
    (model, bot)
}

/// The command a `Command::Write` carries.
fn sent(command: Command) -> (BotRequestId, BotCommand) {
    match command {
        Command::Write {
            request, command, ..
        } => (request, *command),
        _ => panic!("a write"),
    }
}

fn progress(
    bot: &Bot,
    request: BotRequestId,
    kind: BotLifecycleKind,
    stage: BotLifecycleStage,
) -> Result<BotResponse, Failure> {
    Ok(BotResponse::Lifecycle {
        operation: BotLifecycleProgress {
            request_id: request,
            bot_id: bot.id,
            kind,
            chat_id: None,
            source_request_id: None,
            stage,
            refusal: (stage == BotLifecycleStage::Incomplete)
                .then_some(BotLifecycleRefusal::WorkRunning),
            members: Vec::new(),
            left_in_place: Vec::new(),
        },
    })
}

/// **Archive carries the revision it read**, and nothing about it is
/// claimed until the owner answers.
#[test]
fn archive_and_trash_carry_the_revision_they_read() {
    let (mut model, mut bot) = fixture();
    bot.revision = 7;
    let (request, command) = sent(model.begin(Ask::Archive, &bot).expect("archive"));
    assert_eq!(
        command,
        BotCommand::Archive {
            request_id: request,
            bot_id: bot.id,
            expected_revision: 7
        }
    );
    assert!(model.busy());
    assert_eq!(model.outcome(), None, "nothing is claimed before an answer");
    // One at a time: a second ask while it moves sends nothing.
    assert!(model.begin(Ask::Trash, &bot).is_none());
    assert!(model.begin_chat_trash(&bot, ChatId::new(), "x").is_none());
}

/// **Progress, read until the owner says completed** — at the settings'
/// pace while it moves, and at once when the tab is shown.
#[test]
fn a_moving_operation_is_read_until_it_completes() {
    let (mut model, bot) = fixture();
    let (request, _) = sent(model.begin(Ask::Archive, &bot).unwrap());
    let now = Instant::now();
    assert!(
        model.read(now).is_none(),
        "the command is still with the link"
    );
    assert!(model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Captured
        ),
        now
    ));
    assert_eq!(model.op().map(|op| op.stage), Some(Stage::Moving));
    assert!(model.read(now).is_none(), "not before its time");
    let due = model.deadline(now).expect("a read is owed");
    assert!(due > now && due <= now + Duration::from_millis(250));
    model.read_now(now);
    let (asked, read) = sent(model.read(now).expect("shown: read now"));
    assert_eq!(asked, request);
    assert_eq!(
        read,
        BotCommand::ReadLifecycleOperation {
            request_id: request,
            bot_id: bot.id
        }
    );
    assert!(model.read(now).is_none(), "one read on its way at a time");
    model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Completed,
        ),
        now,
    );
    assert_eq!(model.outcome(), Some(&Outcome::Archived));
    assert!(!model.busy());
    assert_eq!(model.archive_source(), Some(request));
    assert!(model.take_read_bot_owed(), "the header reads the Bot again");
    let finished = model.take_finished();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].request, request);
    assert_eq!(finished[0].kind, BotLifecycleKind::Archive);
    assert!(model.deadline(now).is_none(), "nothing more to read");
}

/// **A parked operation waits for the person**: no read, no retry of its
/// own; Continue sends the same command by its request, Cancel the Restore
/// naming it.
#[test]
fn a_parked_operation_waits_then_continues_or_cancels() {
    let (mut model, mut bot) = fixture();
    let (request, original) = sent(model.begin(Ask::Trash, &bot).unwrap());
    let now = Instant::now();
    model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Trash,
            BotLifecycleStage::Incomplete,
        ),
        now,
    );
    assert_eq!(model.op().map(|op| op.stage), Some(Stage::Parked));
    model.read_now(now);
    assert!(model.read(now + Duration::from_secs(60)).is_none());
    assert!(model.deadline(now).is_none(), "no clock while parked");
    let (again, command) = sent(model.continue_parked(&bot).expect("continue"));
    assert_eq!((again, command), (request, original), "the same command");
    model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Trash,
            BotLifecycleStage::Incomplete,
        ),
        now,
    );
    bot.revision = 9;
    let (undo, restore) = sent(model.cancel_parked(&bot).expect("cancel"));
    assert_eq!(
        restore,
        BotCommand::Restore {
            request_id: undo,
            bot_id: bot.id,
            expected_revision: 9,
            source_request_id: request
        }
    );
    model.answer(
        undo,
        progress(
            &bot,
            undo,
            BotLifecycleKind::Restore,
            BotLifecycleStage::Completed,
        ),
        now,
    );
    assert_eq!(model.outcome(), Some(&Outcome::Restored));
}

/// A conversation's parked Trash is undone by its own `RestoreChat`.
#[test]
fn a_parked_conversation_trash_is_cancelled_by_its_restore_chat() {
    let (mut model, bot) = fixture();
    let chat = ChatId::new();
    let (request, command) = sent(model.begin_chat_trash(&bot, chat, "Notes").unwrap());
    assert_eq!(
        command,
        BotCommand::TrashChat {
            request_id: request,
            bot_id: bot.id,
            chat_id: chat,
            expected_revision: bot.revision
        }
    );
    let now = Instant::now();
    model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::TrashChat,
            BotLifecycleStage::Incomplete,
        ),
        now,
    );
    let (undo, restore) = sent(model.cancel_parked(&bot).unwrap());
    assert_eq!(
        restore,
        BotCommand::RestoreChat {
            request_id: undo,
            bot_id: bot.id,
            chat_id: chat,
            expected_revision: bot.revision,
            source_request_id: request
        }
    );
    model.answer(
        undo,
        progress(
            &bot,
            undo,
            BotLifecycleKind::RestoreChat,
            BotLifecycleStage::Completed,
        ),
        now,
    );
    assert_eq!(model.outcome(), Some(&Outcome::ChatKept { chat }));
}

/// **A finished conversation Trash is reported with its title**, for the
/// Trash's row.
#[test]
fn a_finished_conversation_trash_reports_its_title() {
    let (mut model, bot) = fixture();
    let chat = ChatId::new();
    let (request, _) = sent(model.begin_chat_trash(&bot, chat, "Notes").unwrap());
    model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::TrashChat,
            BotLifecycleStage::Completed,
        ),
        Instant::now(),
    );
    assert_eq!(model.outcome(), Some(&Outcome::ChatTrashed { chat }));
    let finished = model.take_finished();
    assert_eq!(finished[0].chat, Some((chat, "Notes".to_owned())));
    assert_eq!(finished[0].request, request);
}

/// The holder the owner's reads name (`held_by`, filled in by
/// `with_lifecycle_holders` for `read` of an archived Bot and for
/// `list_lifecycle`, `apps/koru-ai-server/src/worker/bots/reads.rs`).
fn held(bot: &mut Bot, kind: BotLifecycleKind) -> BotRequestId {
    let request = BotRequestId::mint();
    bot.held_by = Some(koru_ai_core::bots::BotLifecycleHolder {
        request_id: request,
        kind,
        completed_at_ms: 5,
    });
    request
}

/// **Unarchive and Restore need the operation they reverse**: without it
/// nothing is sent, and the control is absent. **A fresh model — the tab
/// after a restart — learns it from the owner's read alone** (`held_by`),
/// with nothing this window remembered.
#[test]
fn unarchive_and_restore_name_the_holder_the_owners_read_names() {
    let (mut model, mut bot) = fixture();
    bot.lifecycle = BotLifecycle::Archived;
    model.know(&bot);
    assert!(
        model.begin(Ask::Unarchive, &bot).is_none(),
        "no holder named"
    );
    let archive = held(&mut bot, BotLifecycleKind::Archive);
    let mut model = Lifecycle::default();
    model.set_bot(bot.id);
    model.know(&bot);
    assert_eq!(model.archive_source(), Some(archive));
    let (request, command) = sent(model.begin(Ask::Unarchive, &bot).unwrap());
    assert_eq!(
        command,
        BotCommand::Restore {
            request_id: request,
            bot_id: bot.id,
            expected_revision: bot.revision,
            source_request_id: archive
        }
    );
    model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Restore,
            BotLifecycleStage::Completed,
        ),
        Instant::now(),
    );
    assert_eq!(model.outcome(), Some(&Outcome::Unarchived));
    assert_eq!(model.archive_source(), None);

    // Trashed: the Trash filter's entry names its Trash; Restore names it.
    let (_, mut trashed) = fixture();
    trashed.lifecycle = BotLifecycle::Trashed;
    let trash = held(&mut trashed, BotLifecycleKind::Trash);
    let mut model = Lifecycle::default();
    model.set_bot(trashed.id);
    model.know(&trashed);
    let (request, command) = sent(model.begin(Ask::Restore, &trashed).unwrap());
    assert_eq!(
        command,
        BotCommand::Restore {
            request_id: request,
            bot_id: trashed.id,
            expected_revision: trashed.revision,
            source_request_id: trash
        }
    );
}

/// **An operation begun before Koru last started** is shown from the Bot's
/// read and continued with the same request.
#[test]
fn an_open_operation_from_the_bots_read_is_followed() {
    let (mut model, mut bot) = fixture();
    let request = BotRequestId::mint();
    bot.revision = 4;
    bot.open_operation = Some(BotOpenLifecycle {
        request_id: request,
        kind: BotLifecycleKind::Archive,
        chat_id: None,
        stage: BotLifecycleStage::Incomplete,
        refusal: Some(BotLifecycleRefusal::OwnerUnavailable),
    });
    model.know(&bot);
    assert_eq!(model.op().map(|op| op.stage), Some(Stage::Parked));
    let (again, command) = sent(model.continue_parked(&bot).unwrap());
    assert_eq!(again, request);
    assert_eq!(
        command,
        BotCommand::Archive {
            request_id: request,
            bot_id: bot.id,
            expected_revision: 4
        }
    );
}

/// **Every refusal is a sentence by its code**, and a competing operation
/// — `request_conflict` today, `bot_lifecycle_pending` after the engine's
/// fix — says another change is still finishing and reads the Bot, whose
/// open operation is then shown.
#[test]
fn refusals_read_as_sentences_and_a_competing_operation_is_shown() {
    for (code, words) in [
        ("request_conflict", Words::BotsErrPending),
        ("bot_lifecycle_pending", Words::BotsErrPending),
        ("bot_conversation_trashed", Words::BotsErrTrashed),
        ("bot_trashed", Words::BotsErrBotTrashed),
        ("bot_archived", Words::BotsErrArchived),
        ("revision_conflict", Words::BotsChangedElsewhere),
        ("something_new", Words::BotsErrNotApplied),
    ] {
        let (mut model, mut bot) = fixture();
        let (request, _) = sent(model.begin(Ask::Trash, &bot).unwrap());
        model.answer(
            request,
            Err(Failure::Refused(code.to_owned())),
            Instant::now(),
        );
        assert_eq!(model.outcome(), Some(&Outcome::Refused(words)), "{code}");
        assert!(!model.busy(), "{code}");
        assert!(model.take_read_bot_owed(), "{code}");
        // The read shows what holds the Bot.
        let other = BotRequestId::mint();
        bot.open_operation = Some(BotOpenLifecycle {
            request_id: other,
            kind: BotLifecycleKind::TrashChat,
            chat_id: Some(ChatId::new()),
            stage: BotLifecycleStage::Settling,
            refusal: None,
        });
        model.know(&bot);
        assert_eq!(model.op().map(|op| op.request), Some(other), "{code}");
        assert!(model.busy(), "{code}: a second Trash waits");
    }
}

/// **A command that did not reach the owner** is neither done nor failed:
/// it is asked about, and a read that says the owner never had it says the
/// change was not answered.
#[test]
fn an_unanswered_command_is_asked_about_not_claimed() {
    let (mut model, bot) = fixture();
    let (request, _) = sent(model.begin(Ask::Archive, &bot).unwrap());
    let now = Instant::now();
    model.answer(request, Err(Failure::Unreachable), now);
    assert!(model.busy(), "still on its way, as far as anyone knows");
    model.read_now(now);
    model.read(now).expect("asked about");
    model.answer(request, Err(Failure::Refused("not_found".into())), now);
    assert_eq!(
        model.outcome(),
        Some(&Outcome::Refused(Words::BotsErrUnreachable))
    );
    // A command the link would not take was never sent.
    let (request, _) = sent(model.begin(Ask::Archive, &bot).unwrap());
    model.unsent(request, now);
    assert!(!model.busy());
}

/// **An Archive this model just finished offers Unarchive at once**, before
/// the read that follows names its holder; that read's holder then wins.
#[test]
fn an_archive_just_finished_is_undone_before_the_read_names_it() {
    let (mut model, bot) = fixture();
    let (request, _) = sent(model.begin(Ask::Archive, &bot).unwrap());
    model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Completed,
        ),
        Instant::now(),
    );
    assert_eq!(model.archive_source(), Some(request));
    let mut read = bot.clone();
    read.lifecycle = BotLifecycle::Archived;
    model.know(&read);
    assert_eq!(model.archive_source(), Some(request), "no holder named yet");
    let named = held(&mut read, BotLifecycleKind::Archive);
    model.know(&read);
    assert_eq!(model.archive_source(), Some(named), "the owner's word");
}
