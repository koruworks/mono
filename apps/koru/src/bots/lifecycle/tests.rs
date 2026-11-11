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

/// **A parked operation waits for the person**: no retry of its own — only
/// reads, on the slower clock (2 s, doubling to 10 s), because the owner
/// takes a parked Archive or Trash up again by itself; Continue sends the
/// same command by its request, Cancel the Restore naming it.
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
    assert!(model.take_read_bot_owed(), "parked: the Bot is read once");
    assert!(model.read(now).is_none(), "not at once");
    assert_eq!(model.deadline(now), Some(now + Duration::from_secs(2)));
    // Each read that finds it still parked waits longer: 2, 4, 8, 10, 10 s.
    let mut at = now;
    for wait in [2, 4, 8, 10, 10] {
        at += Duration::from_secs(wait);
        assert!(
            model.read(at - Duration::from_millis(1)).is_none(),
            "{wait}"
        );
        let (asked, read) = sent(model.read(at).expect("read on the slower clock"));
        assert_eq!(asked, request);
        assert_eq!(
            read,
            BotCommand::ReadLifecycleOperation {
                request_id: request,
                bot_id: bot.id
            },
            "a read, never the Trash again"
        );
        model.answer(
            request,
            progress(
                &bot,
                request,
                BotLifecycleKind::Trash,
                BotLifecycleStage::Incomplete,
            ),
            at,
        );
        assert_eq!(model.op().map(|op| op.stage), Some(Stage::Parked));
    }
    assert!(
        !model.take_read_bot_owed(),
        "the Bot is read once, when it parked — not again for every read"
    );
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
        // A tab shows no settings to point at: the general sentences.
        ("revision_conflict", Words::BotsErrRevisionConflict),
        ("something_new", Words::BotsErrNotDone),
        // `ai_worker_saturated`: never queued, so a refusal.
        ("ai_worker_saturated", Words::BotsErrBusy),
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
    model.answer(request, Err(Failure::lost()), now);
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

/// **An operation learned from the Bot's read while it moves is read on the
/// bounded clock** — after a restart, or one another tab started — so the
/// tab sees it finish instead of saying *Finishing…* for ever.
#[test]
fn an_adopted_moving_operation_is_read_on_the_bounded_clock() {
    let (mut model, mut bot) = fixture();
    let request = BotRequestId::mint();
    bot.open_operation = Some(BotOpenLifecycle {
        request_id: request,
        kind: BotLifecycleKind::Archive,
        chat_id: None,
        stage: BotLifecycleStage::Settling,
        refusal: None,
    });
    model.know(&bot);
    assert!(model.busy());
    let now = Instant::now() + Duration::from_millis(1);
    assert!(model.deadline(now).is_some(), "a wake is owed");
    let (asked, command) = sent(model.read(now).expect("read now"));
    assert_eq!(asked, request);
    assert_eq!(
        command,
        BotCommand::ReadLifecycleOperation {
            request_id: request,
            bot_id: bot.id
        }
    );
}

/// **An operation the owner no longer names is dropped**: another tab
/// cancelled or continued it, so this tab neither shows it parked nor keeps
/// the ⋯ hidden; the one the owner names instead is followed.
#[test]
fn an_operation_the_owner_no_longer_names_is_dropped() {
    let (mut model, mut bot) = fixture();
    bot.open_operation = Some(BotOpenLifecycle {
        request_id: BotRequestId::mint(),
        kind: BotLifecycleKind::Trash,
        chat_id: None,
        stage: BotLifecycleStage::Incomplete,
        refusal: Some(BotLifecycleRefusal::WorkRunning),
    });
    model.know(&bot);
    assert_eq!(model.op().map(|op| op.stage), Some(Stage::Parked));
    bot.open_operation = None;
    model.moved_elsewhere();
    model.know(&bot);
    assert!(model.op().is_none(), "dropped");
    assert!(!model.busy(), "the ⋯ comes back");
    let other = BotRequestId::mint();
    bot.open_operation = Some(BotOpenLifecycle {
        request_id: other,
        kind: BotLifecycleKind::Archive,
        chat_id: None,
        stage: BotLifecycleStage::Settling,
        refusal: None,
    });
    model.know(&bot);
    assert_eq!(
        model.op().map(|op| op.request),
        Some(other),
        "the one named"
    );
}

/// **A second Restore of a source already used is refused as the owner
/// refuses it** — `require_source` in
/// `crates/koru-ai-sqlite/src/repository/bots/lifecycle.rs` answers
/// `request_conflict` when an intent already names that source — and the
/// tab says it was restored already or the Bot changed elsewhere, never
/// that another change is still finishing. It reads the Bot again.
#[test]
fn a_restore_of_a_source_already_used_says_it_was_restored_already() {
    let (mut model, mut bot) = fixture();
    bot.lifecycle = BotLifecycle::Archived;
    let archive = held(&mut bot, BotLifecycleKind::Archive);
    model.know(&bot);
    let (request, command) = sent(model.begin(Ask::Unarchive, &bot).unwrap());
    assert!(
        matches!(command, BotCommand::Restore { source_request_id, .. } if source_request_id == archive)
    );
    // Another tab restored it first: the source is used.
    model.answer(
        request,
        Err(Failure::Refused("request_conflict".into())),
        Instant::now(),
    );
    assert_eq!(
        model.outcome(),
        Some(&Outcome::Refused(Words::BotsErrRestoreConflict))
    );
    assert!(model.take_read_bot_owed(), "what holds it now is read");
    // An Archive's own `request_conflict` still says another change is open.
    assert_eq!(refusal_words("request_conflict"), Words::BotsErrPending);
    assert_eq!(
        restore_refusal_words("request_conflict"),
        Words::BotsErrRestoreConflict
    );
    // A revision moved elsewhere reads the same: the read speaks.
    assert_eq!(
        restore_refusal_words("revision_conflict"),
        Words::BotsErrRestoreConflict
    );
    // A code no build knows yet is the general sentence — true where no
    // settings are shown — never blank.
    assert_eq!(
        restore_refusal_words("bot_new_in_a_later_build"),
        Words::BotsErrNotDone
    );
}

/// **Continue of a parked operation another tab already reversed** is
/// answered as the owner answers it: `resume_bot_lifecycle` leaves a
/// reversed operation parked, so `repeat_bot_lifecycle`
/// (`apps/koru-ai-server/src/worker/bots/lifecycle.rs`) returns its
/// progress still `incomplete`. The tab shows it parked, reads the Bot, and
/// the read — which no longer names it — drops it.
#[test]
fn continue_of_a_reversed_parked_operation_stays_parked_until_the_read_drops_it() {
    let (mut model, mut bot) = fixture();
    let request = BotRequestId::mint();
    bot.open_operation = Some(BotOpenLifecycle {
        request_id: request,
        kind: BotLifecycleKind::Archive,
        chat_id: None,
        stage: BotLifecycleStage::Incomplete,
        refusal: Some(BotLifecycleRefusal::WorkRunning),
    });
    model.know(&bot);
    let (again, _) = sent(model.continue_parked(&bot).unwrap());
    assert_eq!(again, request);
    model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Incomplete,
        ),
        Instant::now(),
    );
    assert_eq!(model.op().map(|op| op.stage), Some(Stage::Parked));
    assert!(model.take_read_bot_owed(), "the Bot is read again");
    // The reversal finished elsewhere: the Bot names nothing open.
    bot.open_operation = None;
    model.know(&bot);
    assert!(!model.busy(), "dropped, never a Continue the owner ignores");
}

/// **A sent Archive that Koru AI does not answer** is said as that — not
/// as the Bot's work finishing — while it is still asked about on the
/// clock; the first answer that comes back clears it.
#[test]
fn an_unanswered_archive_says_koru_ai_is_not_answering() {
    let (mut model, bot) = fixture();
    let (request, _) = sent(model.begin(Ask::Archive, &bot).unwrap());
    assert!(!model.op().unwrap().unanswered);
    let now = Instant::now();
    model.answer(request, Err(Failure::lost()), now);
    assert!(model.op().unwrap().unanswered);
    assert!(model.busy(), "still on its way, as far as anyone knows");
    let later = now + Duration::from_secs(5);
    model.read(later).expect("asked again on the clock");
    model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Settling,
        ),
        later,
    );
    assert!(!model.op().unwrap().unanswered, "it answered");
}

/// **A moving Archive whose read is lost in transport keeps moving** — the
/// second review's proof, turned round. The answer goes through the real
/// link's reading of what `koru_api_client` reports (`gateway_unavailable`
/// for a lost connection, `ai_worker_timeout` when the AI worker did not
/// answer in time): the operation is still followed on the clock and said
/// as unanswered, never dropped as not applied; and a Trash whose own
/// answer is lost the same way is asked about, not claimed either way.
#[test]
fn a_lost_answer_keeps_an_operation_followed_never_dropped() {
    use crate::bots::link::{Said, wrote};
    let (mut model, bot) = fixture();
    let (request, _) = sent(model.begin(Ask::Archive, &bot).expect("archive"));
    let now = Instant::now();
    model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Settling,
        ),
        now,
    );
    let later = now + Duration::from_secs(1);
    assert!(model.read(later).is_some(), "read on the clock");
    model.answer(
        request,
        wrote(Err(Said::new("gateway_unavailable", true))),
        later,
    );
    assert!(model.busy(), "still followed");
    assert_eq!(model.outcome(), None, "nothing claimed");
    assert!(model.op().unwrap().unanswered, "said as unanswered");
    assert!(model.deadline(later).is_some(), "read again on the clock");
    let again = later + Duration::from_secs(5);
    assert!(model.read(again).is_some(), "and read");
    // The command's own answer lost the same way.
    let (mut model, bot) = fixture();
    let (request, _) = sent(model.begin(Ask::Trash, &bot).expect("trash"));
    model.answer(
        request,
        wrote(Err(Said::new("ai_worker_timeout", true))),
        now,
    );
    assert!(model.busy(), "asked about, not claimed");
    assert_eq!(model.outcome(), None);
    assert!(
        model.read(later).is_some(),
        "the owner is asked what it holds"
    );
}

/// **A parked operation the owner took up again by itself is followed** —
/// the engine resumes a parked Archive or Trash (`owner_unavailable`) once
/// its execution owner answers, so the Bot's next read names the same
/// operation moving: the tab follows it on the clock without the person
/// pressing Continue, and a later read that parks it again is shown parked.
#[test]
fn a_read_that_names_the_same_operation_at_a_new_stage_updates_it() {
    let (mut model, mut bot) = fixture();
    let request = BotRequestId::mint();
    let open = |stage| BotOpenLifecycle {
        request_id: request,
        kind: BotLifecycleKind::Archive,
        chat_id: None,
        stage,
        refusal: (stage == BotLifecycleStage::Incomplete)
            .then_some(BotLifecycleRefusal::OwnerUnavailable),
    };
    bot.open_operation = Some(open(BotLifecycleStage::Incomplete));
    model.know(&bot);
    assert_eq!(model.op().map(|op| op.stage), Some(Stage::Parked));
    let soon = Instant::now() + Duration::from_secs(1);
    assert!(
        model.deadline(soon).is_some_and(|due| due > soon),
        "waits for the person, read on the slower clock"
    );
    assert!(model.read(soon).is_none(), "not yet");
    bot.open_operation = Some(open(BotLifecycleStage::Settling));
    model.know(&bot);
    assert_eq!(model.op().map(|op| op.stage), Some(Stage::Moving));
    let later = Instant::now() + Duration::from_millis(1);
    let (asked, _) = sent(model.read(later).expect("read on the clock"));
    assert_eq!(asked, request, "the same operation");
    model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Completed,
        ),
        later,
    );
    assert_eq!(model.outcome(), Some(&Outcome::Archived));
    // And one parked again by a read is shown parked.
    let (mut model, mut bot) = fixture();
    bot.open_operation = Some(open(BotLifecycleStage::Settling));
    model.know(&bot);
    bot.open_operation = Some(open(BotLifecycleStage::Incomplete));
    model.know(&bot);
    assert_eq!(model.op().map(|op| op.stage), Some(Stage::Parked));
}

/// **A write's answer never drops or replaces an operation** — only a read
/// does. A settings save is answered with the Bot the owner wrote
/// (`execute_bot_command`'s `BotResponse::Bot`), which reports no open
/// operation; a conversation's parked Trash the tab follows is still
/// followed after it, with its Continue and Cancel.
#[test]
fn a_writes_answer_never_drops_an_operation() {
    let (mut model, mut bot) = fixture();
    let parked = BotRequestId::mint();
    bot.open_operation = Some(BotOpenLifecycle {
        request_id: parked,
        kind: BotLifecycleKind::TrashChat,
        chat_id: Some(ChatId::new()),
        stage: BotLifecycleStage::Incomplete,
        refusal: Some(BotLifecycleRefusal::WorkRunning),
    });
    model.know(&bot);
    assert_eq!(model.op().map(|op| op.stage), Some(Stage::Parked));
    let mut saved = bot.clone();
    saved.config.name = "Scout two".into();
    saved.revision += 1;
    saved.open_operation = None;
    model.know_written(&saved);
    assert_eq!(model.op().map(|op| op.request), Some(parked), "kept");
    assert_eq!(model.op().map(|op| op.stage), Some(Stage::Parked));
    // A read that no longer names it is what drops it.
    model.know(&saved);
    assert!(model.op().is_none(), "the read's word");
}

/// A Bot whose Archive the owner parked because its execution owner did not
/// answer, as `read_bot` names it after a restart (`Bot.open_operation`).
fn parked_for_its_owner(bot: &mut Bot) -> BotRequestId {
    let request = BotRequestId::mint();
    bot.open_operation = Some(BotOpenLifecycle {
        request_id: request,
        kind: BotLifecycleKind::Archive,
        chat_id: None,
        stage: BotLifecycleStage::Incomplete,
        refusal: Some(BotLifecycleRefusal::OwnerUnavailable),
    });
    request
}

/// The Bot once the owner completed `request`'s Archive: archived, nothing
/// open, held by that Archive (`with_lifecycle_holders`), its revision moved.
fn archived_by(bot: &Bot, request: BotRequestId) -> Bot {
    let mut archived = bot.clone();
    archived.revision += 1;
    archived.lifecycle = BotLifecycle::Archived;
    archived.open_operation = None;
    archived.held_by = Some(koru_ai_core::bots::BotLifecycleHolder {
        request_id: request,
        kind: BotLifecycleKind::Archive,
        completed_at_ms: 1,
    });
    archived
}

/// **A parked Archive the owner takes up again by itself is seen moving and
/// finishing** — the third review's blocker. A tab restored after a restart
/// adopts the Archive parked for its silent owner; `owner_answered_bot_lifecycle`
/// (`apps/koru-ai-server/src/worker/bots/lifecycle.rs`) resumes it with no
/// command once the owner answers: parked, then moving, then completed. The
/// tab left in front reads it on the slower clock, so it follows it on the
/// bounded clock once it moves and says it is archived — never *This didn't
/// finish… Continue* for ever.
#[test]
fn a_parked_archive_the_owner_takes_up_by_itself_is_read_until_it_finishes() {
    let (mut model, mut bot) = fixture();
    let request = parked_for_its_owner(&mut bot);
    model.know(&bot);
    assert_eq!(model.op().map(|op| op.stage), Some(Stage::Parked));
    let now = Instant::now() + Duration::from_millis(1);
    let due = model
        .deadline(now)
        .expect("a parked operation is read again");
    assert!(due > now && due <= now + Duration::from_secs(2), "{due:?}");
    let (asked, read) = sent(model.read(due).expect("read on the slower clock"));
    assert_eq!(asked, request);
    assert!(matches!(read, BotCommand::ReadLifecycleOperation { .. }));
    // The owner answered meanwhile: it moves again, with nobody pressing.
    model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Settling,
        ),
        due,
    );
    assert_eq!(model.op().map(|op| op.stage), Some(Stage::Moving));
    let next = model.deadline(due).expect("followed");
    assert!(
        next <= due + Duration::from_millis(250),
        "the bounded clock again"
    );
    sent(model.read(next).expect("read"));
    model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Completed,
        ),
        next,
    );
    assert_eq!(model.outcome(), Some(&Outcome::Archived));
    assert_eq!(model.archive_source(), Some(request));
    assert_eq!(model.take_finished().len(), 1, "the window's book");
}

/// **Continue on a parked Archive the owner finished meanwhile says what the
/// read shows** — the third review's probe, turned round. Continue rebuilds
/// the command at the read revision; `repeat_bot_lifecycle` accepts a
/// different fingerprint only while parked, so it answers
/// `request_conflict`. That is not *another change is still finishing*: the
/// operation is read at once, and so is the Bot, and an archived Bot held by
/// that very Archive is said as archived, with Unarchive.
#[test]
fn continue_after_the_owner_finished_a_parked_archive_says_what_the_read_shows() {
    let (mut model, mut bot) = fixture();
    bot.revision = 8;
    let request = parked_for_its_owner(&mut bot);
    model.know(&bot);
    let (_, command) = sent(model.continue_parked(&bot).unwrap());
    assert!(matches!(
        command,
        BotCommand::Archive {
            expected_revision: 8,
            ..
        }
    ));
    let now = Instant::now();
    model.answer(
        request,
        Err(Failure::Refused("request_conflict".into())),
        now,
    );
    assert_eq!(model.outcome(), None, "nothing said before the read");
    assert!(model.busy(), "still followed");
    assert!(model.take_read_bot_owed(), "the Bot is read");
    let (asked, read) = sent(model.read(now).expect("the operation is read at once"));
    assert_eq!(asked, request);
    assert!(matches!(read, BotCommand::ReadLifecycleOperation { .. }));
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
    assert_eq!(model.archive_source(), Some(request));

    // The Bot's read may answer first: it names the Archive as what holds
    // the Bot, which is the same finish.
    let (mut model, mut bot) = fixture();
    let request = parked_for_its_owner(&mut bot);
    model.know(&bot);
    sent(model.continue_parked(&bot).unwrap());
    model.answer(
        request,
        Err(Failure::Refused("request_conflict".into())),
        Instant::now(),
    );
    model.know(&archived_by(&bot, request));
    assert_eq!(model.outcome(), Some(&Outcome::Archived));
    assert!(!model.busy());
    assert_eq!(model.archive_source(), Some(request));
}

/// **Cancel on a parked Archive the owner took up again** is answered
/// `bot_lifecycle_pending` (`restore_bot` holds a Restore of a source that
/// is moving): the parked Archive is followed again and read at once, and
/// the tab says what the read shows — here that it finished — never that
/// another change is still finishing.
#[test]
fn cancel_of_an_archive_moving_again_reads_it_rather_than_refusing() {
    let (mut model, mut bot) = fixture();
    let request = parked_for_its_owner(&mut bot);
    model.know(&bot);
    let (undo, _) = sent(model.cancel_parked(&bot).unwrap());
    let now = Instant::now();
    model.answer(
        undo,
        Err(Failure::Refused("bot_lifecycle_pending".into())),
        now,
    );
    assert_eq!(model.outcome(), None, "nothing said before the read");
    assert_eq!(
        model.op().map(|op| op.request),
        Some(request),
        "the Archive is followed again"
    );
    let (asked, _) = sent(model.read(now).expect("read at once"));
    assert_eq!(asked, request);
    model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Settling,
        ),
        now,
    );
    assert_eq!(model.op().map(|op| op.stage), Some(Stage::Moving));
    assert_eq!(model.outcome(), None);
    model.know(&archived_by(&bot, request));
    assert_eq!(model.outcome(), Some(&Outcome::Archived));
}

/// **A read older than Continue says nothing about it**: a parked
/// operation's read is with the link when the person presses Continue; the
/// link answers in order, so the read's *still parked* comes first and is
/// from before the Continue, whose own answer says it moves.
#[test]
fn a_parked_read_answered_after_continue_does_not_park_it_again() {
    let (mut model, mut bot) = fixture();
    let request = parked_for_its_owner(&mut bot);
    model.know(&bot);
    let later = Instant::now() + Duration::from_secs(3);
    sent(model.read(later).expect("read on the slower clock"));
    sent(model.continue_parked(&bot).expect("continue meanwhile"));
    let parked = progress(
        &bot,
        request,
        BotLifecycleKind::Archive,
        BotLifecycleStage::Incomplete,
    );
    model.answer(request, parked, later);
    assert_eq!(
        model.op().map(|op| op.stage),
        Some(Stage::Moving),
        "the read is older than the Continue"
    );
    model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Settling,
        ),
        later,
    );
    assert_eq!(model.op().map(|op| op.stage), Some(Stage::Moving));
}

/// **A read that names the Bot's state clears a refusal it contradicts**:
/// *another change is still finishing* is not said once the Bot's read
/// names no open operation.
#[test]
fn a_read_with_nothing_open_clears_another_change_is_finishing() {
    let (mut model, bot) = fixture();
    let (request, _) = sent(model.begin(Ask::Trash, &bot).unwrap());
    model.answer(
        request,
        Err(Failure::Refused("bot_lifecycle_pending".into())),
        Instant::now(),
    );
    assert_eq!(
        model.outcome(),
        Some(&Outcome::Refused(Words::BotsErrPending))
    );
    model.know(&bot);
    assert_eq!(model.outcome(), None, "nothing is finishing now");
    // A refusal about the attempt itself is not the read's to undo.
    let (request, _) = sent(model.begin(Ask::Trash, &bot).unwrap());
    model.answer(
        request,
        Err(Failure::Refused("ai_worker_saturated".into())),
        Instant::now(),
    );
    model.know(&bot);
    assert_eq!(model.outcome(), Some(&Outcome::Refused(Words::BotsErrBusy)));
}

/// **A read turned away for now keeps a moving Archive followed** — the
/// third review's probe, made permanent. The Archive is accepted and moving;
/// its progress read is turned away by the AI worker (`ai_worker_saturated`
/// from `queue_failure`: the read was never queued), or by a Store or Bot
/// that does not answer (`workspace_unavailable`, `storage_error` from
/// `bot_failure`), each through the real link's classification. Only the
/// question went unanswered: the Archive is still followed on the clock and
/// nothing is claimed — never *Koru AI is busy, so nothing changed*.
#[test]
fn a_read_turned_away_for_now_keeps_a_moving_archive_followed() {
    use crate::bots::link::Said;
    for refused in [
        Failure::turned_away(),
        Failure::of(Said::new("workspace_unavailable", true)),
        Failure::of(Said::new("storage_error", true)),
    ] {
        let (mut model, bot) = fixture();
        let (request, _) = sent(model.begin(Ask::Archive, &bot).unwrap());
        let now = Instant::now();
        model.answer(
            request,
            progress(
                &bot,
                request,
                BotLifecycleKind::Archive,
                BotLifecycleStage::Captured,
            ),
            now,
        );
        model.read_now(now);
        let (asked, _) = sent(model.read(now).expect("a read"));
        assert_eq!(asked, request);
        model.answer(request, Err(refused.clone()), now);
        assert!(model.busy(), "{refused:?}: still followed");
        assert_eq!(model.outcome(), None, "{refused:?}: nothing claimed");
        assert_eq!(model.op().map(|op| op.stage), Some(Stage::Moving));
        assert!(!model.op().unwrap().unanswered, "{refused:?}");
        let again = model.deadline(now).expect("read again on the clock");
        assert!(again > now, "{refused:?}: not at once");
        let (asked, _) = sent(model.read(again).expect("and read"));
        assert_eq!(asked, request);
    }
    // The command's own refusal for now is still the command's: it was
    // never queued, so nothing changed.
    let (mut model, bot) = fixture();
    let (request, _) = sent(model.begin(Ask::Archive, &bot).unwrap());
    model.answer(request, Err(Failure::turned_away()), Instant::now());
    assert_eq!(model.outcome(), Some(&Outcome::Refused(Words::BotsErrBusy)));
}

/// **A read refused for good after the owner accepted the command** is not
/// the command's refusal: the Archive stays as last said, the Bot is read
/// rather than *that wasn't done* said, and it is read again only when the
/// tab is next shown. `wrong_owner` is what `read_bot_lifecycle` answers
/// once the caller's owner is no longer the operation's.
#[test]
fn a_read_refused_for_good_reads_the_bot_rather_than_saying_the_commands_words() {
    let (mut model, bot) = fixture();
    let (request, _) = sent(model.begin(Ask::Archive, &bot).unwrap());
    let now = Instant::now();
    model.answer(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Settling,
        ),
        now,
    );
    model.take_read_bot_owed();
    model.read_now(now);
    sent(model.read(now).expect("a read"));
    model.answer(request, Err(Failure::Refused("wrong_owner".into())), now);
    assert_eq!(model.outcome(), None, "never the command's words");
    assert!(model.busy(), "as last said");
    assert!(model.take_read_bot_owed(), "the Bot is read");
    assert_eq!(model.deadline(now), None, "no clock: it would be refused");
    assert!(model.read(now + Duration::from_secs(60)).is_none());
    // Shown again: asked once more.
    model.read_now(now);
    assert!(model.read(now).is_some());
}
