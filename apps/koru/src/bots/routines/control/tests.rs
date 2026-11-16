// SPDX-License-Identifier: MPL-2.0
//! The controls as a model: no window, no link, no clock but the one given.
use super::*;
use koru_delegate_core::{DeviceId, RoutineControlOutcome, RunId};

const MINUTE: i64 = 60_000;

fn state(on: bool) -> ScheduleState {
    let owner = DeviceId::new();
    if on {
        ScheduleState::Active { owner }
    } else {
        ScheduleState::Paused {
            owner,
            reason: SchedulePauseReason::Manual,
        }
    }
}
fn item(on: bool) -> BotRoutineListItem {
    BotRoutineListItem {
        schedule_id: ScheduleId::mint(),
        creation: BotRoutineCreationObservation {
            request_id: BotRequestId::mint(),
            state: BotRoutineCreationState::Bound,
            refusal: None,
            updated_at_ms: 0,
        },
        grant: BotRoutineGrantSummary {
            source_bot_policy_revision: 1,
            grant_revision: 1,
            access: BotAccess::Default,
            selected_page_count: 0,
        },
        live: BotRoutineLiveObservation::Current {
            name: ScheduleName::new("Morning reading").unwrap(),
            definition_revision: 2,
            state: state(on),
            next: if on {
                RoutineNextObservation::Exact { at_ms: 9 * MINUTE }
            } else {
                RoutineNextObservation::Inactive {}
            },
        },
        local_conversations: BotRoutineLocalConversations::Available {
            items: vec![],
            more: false,
        },
    }
}
fn rule(cron: &str) -> RecurrenceRule {
    RecurrenceRule::floating(koru_recurrence::CronExpression::parse(cron).unwrap())
}
/// A pane shown for one Bot, its list read and answered with `items`.
fn shown(items: Vec<BotRoutineListItem>) -> (Routines, BotId) {
    let bot = BotId::mint();
    let mut routines = Routines::default();
    routines.show(Some(bot), true);
    list(&mut routines, bot, items);
    (routines, bot)
}
fn list(routines: &mut Routines, bot: BotId, items: Vec<BotRoutineListItem>) {
    let Some(Command::ReadRoutines { seq, .. }) = routines.read() else {
        panic!("a list read is owed")
    };
    assert!(routines.answer(
        seq,
        bot,
        Ok(BotRoutineListPage {
            bot_id: bot,
            bot_revision: 1,
            effective_model: BotRoutineModelObservation::Unavailable {},
            items,
            next_schedule: None,
        }),
    ));
}
fn detail_of(
    bot: BotId,
    item: &BotRoutineListItem,
    lines: Vec<RoutineOccurrenceLine>,
) -> BotRoutineDetail {
    let BotRoutineLiveObservation::Current {
        name,
        definition_revision,
        state,
        next,
    } = &item.live
    else {
        panic!("a live row")
    };
    BotRoutineDetail {
        bot_id: bot,
        schedule_id: item.schedule_id,
        bot_revision: 1,
        observed_at_ms: 0,
        creation: item.creation.clone(),
        access: item.grant.clone(),
        access_at_setup: item.grant.clone(),
        configured_model: BotRoutineModelObservation::Unavailable {},
        destination: RoutineDestinationObservation::Unavailable {},
        live: RoutineDetailLive::Current {
            name: name.clone(),
            definition_revision: *definition_revision,
            effective_at_ms: 0,
            recurrence: rule("0 9 * * *"),
            state: state.clone(),
            owner_zone: RoutineOwnerZoneObservation::OwnerLocal {},
            next: next.clone(),
        },
        readiness: RoutineReadinessObservation::NotAssessed {},
        local_history: RoutineLocalHistoryObservation::Available {
            items: vec![],
            next_before_occurrence_ms: None,
        },
        occurrences: RoutineOccurrenceHistoryObservation::Available {
            items: lines,
            next_before_occurrence_ms: None,
        },
    }
}
/// Answer the selected detail's read on its way.
fn detail(
    routines: &mut Routines,
    bot: BotId,
    item: &BotRoutineListItem,
    lines: Vec<RoutineOccurrenceLine>,
) {
    let Some(Command::ReadRoutineDetail { seq, schedule, .. }) = routines.detail.read() else {
        panic!("a detail read is owed")
    };
    assert_eq!(schedule, item.schedule_id);
    assert!(routines.detail_answer(seq, bot, schedule, Ok(detail_of(bot, item, lines))));
}
fn sent(command: Option<Command>) -> (u64, BotCommand) {
    match command {
        Some(Command::RoutineControl { seq, command, .. }) => (seq, *command),
        _ => panic!("a control was sent"),
    }
}
fn confirmed(
    schedule: ScheduleId,
    action: BotRoutineControlAction,
    on: bool,
    revision: u32,
) -> BotResponse {
    BotResponse::RoutineControl {
        control: BotRoutineControl {
            schedule_id: schedule,
            action,
            outcome: RoutineControlOutcome::Applied,
            definition_revision: revision,
            state: state(on),
            next: if on {
                RoutineNextObservation::Exact { at_ms: 42 * MINUTE }
            } else {
                RoutineNextObservation::Inactive {}
            },
        },
    }
}
fn row_state(routines: &Routines, schedule: ScheduleId) -> (u32, bool, RoutineNextObservation) {
    let item = routines.row(schedule).unwrap();
    match &item.live {
        BotRoutineLiveObservation::Current {
            definition_revision,
            state,
            next,
            ..
        } => (
            *definition_revision,
            matches!(state, ScheduleState::Active { .. }),
            next.clone(),
        ),
        _ => panic!("a live row"),
    }
}

#[test]
fn routine_control_enable_is_pending_until_the_owner_confirms_it() {
    let paused = item(false);
    let id = paused.schedule_id;
    let (mut routines, bot) = shown(vec![paused]);
    let (seq, command) = sent(routines.toggle_routine(id));
    assert!(matches!(
        command,
        BotCommand::SetRoutineLifecycle {
            schedule_id,
            expected_definition_revision: 2,
            action: BotRoutineLifecycleAction::Enable,
            ..
        } if schedule_id == id
    ));
    // Pending: said as on its way, and the row still shows what was read.
    assert_eq!(routines.controls_data().doing(id), Some(Doing::TurningOn));
    assert_eq!(row_state(&routines, id).1, false);
    // A pending control is not pressable, and neither is any other.
    assert!(routines.toggle_routine(id).is_none());
    assert!(routines.test_routine(id).is_none());
    // The owner's answer is the state shown, with its revision and next time.
    assert!(routines.control_answer(
        seq,
        bot,
        id,
        Ok(confirmed(id, BotRoutineControlAction::Enable, true, 3)),
        0,
    ));
    assert_eq!(routines.controls_data().pending, None);
    assert_eq!(
        row_state(&routines, id),
        (
            3,
            true,
            RoutineNextObservation::Exact { at_ms: 42 * MINUTE }
        )
    );
}

#[test]
fn routine_control_pause_whose_answer_is_lost_says_checking_and_reads_again() {
    let on = item(true);
    let id = on.schedule_id;
    let (mut routines, bot) = shown(vec![on.clone()]);
    let (seq, command) = sent(routines.toggle_routine(id));
    assert!(matches!(
        command,
        BotCommand::SetRoutineLifecycle {
            action: BotRoutineLifecycleAction::Pause,
            ..
        }
    ));
    assert_eq!(routines.controls_data().doing(id), Some(Doing::Pausing));
    assert!(routines.control_answer(seq, bot, id, Err(Refusal::unanswered()), 0));
    // Never shown as paused: nobody confirmed it.
    assert_eq!(row_state(&routines, id).1, true);
    assert_eq!(routines.controls_data().checking, Some(id));
    // No control is offered on stale detail.
    assert!(routines.toggle_routine(id).is_none());
    // The list is read again, and what it reads is what is shown.
    let mut read = on;
    read.live = BotRoutineLiveObservation::Current {
        name: ScheduleName::new("Morning reading").unwrap(),
        definition_revision: 3,
        state: state(false),
        next: RoutineNextObservation::Inactive {},
    };
    list(&mut routines, bot, vec![read]);
    assert_eq!(routines.controls_data().checking, None);
    assert_eq!(row_state(&routines, id).0, 3);
    assert_eq!(row_state(&routines, id).1, false);
    // `routine_control_unknown` is the same: the owner did not hear back.
    let (seq, _) = sent(routines.toggle_routine(id));
    assert!(routines.control_answer(
        seq,
        bot,
        id,
        Err(Refusal::code("routine_control_unknown")),
        0
    ));
    assert_eq!(routines.controls_data().checking, Some(id));
}

#[test]
fn routine_control_every_refusal_has_a_sentence_and_no_code_is_shown() {
    let shared = [
        "not_found",
        "wrong_owner",
        "forbidden",
        "workspace_unavailable",
        "bot_trashed",
        "bot_lifecycle_pending",
        "bot_archived",
        "bot_operation_pending",
        "revision_conflict",
        "storage_error",
        "ai_worker_saturated",
        "request_conflict",
        "a_code_from_a_newer_build",
    ];
    let reasons = [
        "bot_paused",
        "bot_archived",
        "creation_not_bound",
        "access_unavailable",
        "access_pending",
    ];
    let mut words: Vec<Words> = ROUTINE_CODES
        .iter()
        .chain(shared.iter())
        .map(|code| refusal_words(code))
        .collect();
    for reason in reasons {
        let said = not_ready_words(&[reason.to_owned()]);
        assert_eq!(said.len(), 1);
        assert_ne!(said[0], Words::BotsRoutineErrNotReady, "{reason}");
        words.extend(said);
    }
    words.extend(not_ready_words(&[]));
    words.extend(not_ready_words(&["a_reason_from_a_newer_build".to_owned()]));
    for lang in koru_i18n::Lang::ALL {
        for key in &words {
            let said = koru_i18n::t(lang, *key);
            assert!(!said.is_empty());
            assert!(!said.contains("routine_") && !said.contains('_'), "{said}");
        }
    }
    // Each Routine code has a sentence of its own kind, not the catch-all.
    for code in ROUTINE_CODES {
        assert_ne!(refusal_words(code), Words::BotsRoutineErrOther, "{code}");
    }
    // The reasons of `routine_not_ready` are one line each, in order.
    assert_eq!(
        not_ready_words(&["bot_paused".to_owned(), "access_pending".to_owned()]),
        vec![
            Words::BotsRoutineErrBotPaused,
            Words::BotsRoutineErrAccessApplying
        ]
    );
}

#[test]
fn routine_control_refusals_are_said_on_the_row_and_change_no_state() {
    let paused = item(false);
    let id = paused.schedule_id;
    let (mut routines, bot) = shown(vec![paused]);
    let (seq, _) = sent(routines.toggle_routine(id));
    assert!(routines.control_answer(
        seq,
        bot,
        id,
        Err(Refusal::not_ready(&["bot_paused", "access_unavailable"])),
        0,
    ));
    assert_eq!(
        routines.controls_data().notes(id),
        [
            Words::BotsRoutineErrBotPaused,
            Words::BotsRoutineErrAccessUnreadable
        ]
    );
    assert_eq!(row_state(&routines, id).1, false);
    // Already in the asked state: no sentence, the read state is shown.
    let (seq, _) = sent(routines.toggle_routine(id));
    assert!(routines.control_answer(
        seq,
        bot,
        id,
        Err(Refusal::code("routine_state_conflict")),
        0
    ));
    assert!(routines.controls_data().notes(id).is_empty());
    assert!(routines.loading());
}

#[test]
fn routine_control_stale_answer_after_the_selection_changed_is_dropped() {
    let paused = item(false);
    let id = paused.schedule_id;
    let (mut routines, bot) = shown(vec![paused.clone()]);
    let (seq, _) = sent(routines.toggle_routine(id));
    // An answer for another control, another Routine or another Bot is not
    // this one's.
    let answer = || Ok(confirmed(id, BotRoutineControlAction::Enable, true, 3));
    assert!(!routines.control_answer(seq + 1, bot, id, answer(), 0));
    assert!(!routines.control_answer(seq, BotId::mint(), id, answer(), 0));
    assert!(!routines.control_answer(seq, bot, ScheduleId::mint(), answer(), 0));
    assert_eq!(routines.controls_data().doing(id), Some(Doing::TurningOn));
    // Another Bot shown in the pane: the old answer changes nothing.
    let other = BotId::mint();
    routines.show(Some(other), true);
    assert!(!routines.control_answer(seq, bot, id, answer(), 0));
    assert_eq!(routines.controls_data(), Data::default());
}

#[test]
fn routine_control_hiding_the_pane_cancels_observation_and_adds_no_watch() {
    let paused = item(false);
    let id = paused.schedule_id;
    let (mut routines, bot) = shown(vec![paused]);
    // A Test that waits for the next minute has a deadline while shown.
    let (seq, _) = sent(routines.test_routine(id));
    assert!(routines.control_answer(
        seq,
        bot,
        id,
        Err(Refusal::code("routine_test_minute_ending")),
        55_000,
    ));
    assert_eq!(routines.control_deadline(), Some(MINUTE));
    let (seq, _) = sent(routines.toggle_routine(id));
    // Hidden: nothing pending, no deadline, no tick, and nothing to send.
    routines.show(Some(bot), false);
    assert_eq!(routines.controls_data(), Data::default());
    assert_eq!(routines.control_deadline(), None);
    assert!(!routines.control_tick(10 * MINUTE));
    assert!(routines.read().is_none());
    assert!(routines.toggle_routine(id).is_none());
    // Its answer arrives late and is ignored.
    assert!(!routines.control_answer(
        seq,
        bot,
        id,
        Ok(confirmed(id, BotRoutineControlAction::Enable, true, 3)),
        0,
    ));
    // Shown again, the list is read: that is what says what is true.
    routines.show(Some(bot), true);
    assert!(matches!(
        routines.read(),
        Some(Command::ReadRoutines { .. })
    ));
}

#[test]
fn routine_control_test_is_offered_only_while_paused_and_its_wait_is_not_an_error() {
    let on = item(true);
    let paused = item(false);
    let (on_id, id) = (on.schedule_id, paused.schedule_id);
    let (mut routines, bot) = shown(vec![on, paused]);
    assert!(!routines.testable(on_id));
    assert!(routines.test_routine(on_id).is_none());
    assert!(routines.testable(id));
    let (seq, command) = sent(routines.test_routine(id));
    assert!(matches!(
        command,
        BotCommand::TestRoutine {
            schedule_id,
            expected_definition_revision: 2,
            ..
        } if schedule_id == id
    ));
    assert_eq!(routines.controls_data().doing(id), Some(Doing::Testing));
    // Too late in the minute: a wait with a known end, no error line.
    assert!(routines.control_answer(
        seq,
        bot,
        id,
        Err(Refusal::code("routine_test_minute_ending")),
        3 * MINUTE + 55_000,
    ));
    let data = routines.controls_data();
    assert_eq!(data.test_wait, Some((id, 4 * MINUTE)));
    assert!(data.notes(id).is_empty());
    assert!(routines.test_routine(id).is_none());
    // The wait ends by the clock; no Test is started by it.
    assert!(!routines.control_tick(4 * MINUTE - 1));
    assert!(routines.control_tick(4 * MINUTE));
    assert_eq!(routines.controls_data().test_wait, None);
    assert_eq!(routines.controls_data().pending, None);
    // The other Test refusals are sentences.
    for (code, words) in [
        (
            "routine_test_minute_taken",
            Words::BotsRoutineErrMinuteTaken,
        ),
        ("routine_test_busy", Words::BotsRoutineErrTestBusy),
        ("routine_must_be_paused", Words::BotsRoutineErrPauseToTest),
    ] {
        let (seq, _) = sent(routines.test_routine(id));
        assert!(routines.control_answer(seq, bot, id, Err(Refusal::code(code)), 0));
        assert_eq!(routines.controls_data().notes(id), [words]);
    }
}

#[test]
fn routine_control_test_result_is_read_from_the_history() {
    let paused = item(false);
    let id = paused.schedule_id;
    let minute = 7 * MINUTE;
    let accepted = |routines: &mut Routines, bot| {
        let (seq, _) = sent(routines.test_routine(id));
        assert!(routines.control_answer(
            seq,
            bot,
            id,
            Ok(BotResponse::RoutineTest {
                test: BotRoutineTest {
                    schedule_id: id,
                    occurrence_ms: minute,
                },
            }),
            minute + 1_000,
        ));
    };
    // Accepted: the Routine's history is opened and read. The Routine is
    // still paused: a Test turns nothing on.
    let (mut routines, bot) = shown(vec![paused.clone()]);
    accepted(&mut routines, bot);
    assert_eq!(routines.expanded, Some(id));
    assert_eq!(row_state(&routines, id).1, false);
    let line = RoutineOccurrenceLine {
        occurrence_ms: minute,
        outcome: RoutineOccurrenceOutcome::Ran,
        test: true,
        recorded_at_ms: minute + 2_000,
        conversation: Some(BotRoutineLocalConversation {
            occurrence_ms: minute,
            run_id: RunId::mint(),
            chat_id: uuid::Uuid::new_v4(),
            attempt_id: uuid::Uuid::new_v4(),
            conversation: BotRoutineConversationVisibility::Available,
        }),
    };
    detail(&mut routines, bot, &paused, vec![line.clone()]);
    // Its line is there: nothing more to wait for, and it can be opened.
    assert_eq!(routines.control_deadline(), None);
    assert!(routines.controls_data().notes(id).is_empty());
    let chat = ChatId::parse(&line.conversation.unwrap().chat_id.to_string()).unwrap();
    assert!(routines.can_open(chat));

    // No line by the end of the minute: one more read, then the quiet line.
    let (mut routines, bot) = shown(vec![paused.clone()]);
    accepted(&mut routines, bot);
    detail(&mut routines, bot, &paused, vec![]);
    let due = routines.control_deadline().unwrap();
    assert!(due >= minute + MINUTE);
    assert!(!routines.control_tick(due - 1));
    assert!(routines.control_tick(due));
    detail(&mut routines, bot, &paused, vec![]);
    assert_eq!(
        routines.controls_data().notes(id),
        [Words::BotsRoutineTestNotStarted]
    );
    assert_eq!(routines.control_deadline(), None);
}

#[test]
fn routine_control_edit_sends_one_command_with_timing_and_name() {
    let on = item(true);
    let id = on.schedule_id;
    let (mut routines, bot) = shown(vec![on.clone()]);
    // Nothing to edit until the selected detail is read.
    assert!(routines.timing_seed().is_none());
    assert!(routines.toggle(id));
    detail(&mut routines, bot, &on, vec![]);
    let seed = routines.timing_seed().unwrap();
    assert!(seed.on);
    assert_eq!(seed.name.as_str(), "Morning reading");
    let name = ScheduleName::new("Evening reading").unwrap();
    let command = routines
        .save_timing(id, name.clone(), rule("30 18 * * *"))
        .unwrap();
    let (seq, command) = sent(Some(command));
    assert_eq!(
        command,
        BotCommand::EditRoutine {
            bot_id: bot,
            schedule_id: id,
            expected_definition_revision: 2,
            expected_name: ScheduleName::new("Morning reading").unwrap(),
            name: name.clone(),
            recurrence: rule("30 18 * * *"),
        }
    );
    assert_eq!(
        routines.controls_data().doing(id),
        Some(Doing::SavingTiming)
    );
    // One Save is one command: a second is refused while it is out.
    assert_eq!(
        routines
            .save_timing(id, name.clone(), rule("30 18 * * *"))
            .err(),
        Some(Words::BotsRoutineErrPending)
    );
    assert!(routines.control_answer(
        seq,
        bot,
        id,
        Ok(confirmed(id, BotRoutineControlAction::Edit, true, 3)),
        0,
    ));
    assert_eq!(routines.take_timing_answer(), Some(TimingAnswer::Applied));
    assert_eq!(routines.take_timing_answer(), None);
    let BotRoutineLiveObservation::Current {
        name: shown_name,
        definition_revision,
        ..
    } = &routines.row(id).unwrap().live
    else {
        panic!()
    };
    assert_eq!((shown_name, *definition_revision), (&name, 3));
    // The detail is read again for the new timing.
    assert!(routines.detail.loading());

    // A changed Routine: the dialog keeps what was typed and is told why.
    detail(&mut routines, bot, &on, vec![]);
    let command = routines.save_timing(id, name, rule("0 7 * * *")).unwrap();
    let (seq, _) = sent(Some(command));
    assert!(routines.control_answer(seq, bot, id, Err(Refusal::code("revision_conflict")), 0));
    assert_eq!(
        routines.take_timing_answer(),
        Some(TimingAnswer::Problem(Words::BotsRoutineErrChanged))
    );
    assert!(routines.loading());
}

#[test]
fn routine_control_access_is_changed_only_while_paused_and_each_attempt_is_a_new_request() {
    let on = item(true);
    let on_id = on.schedule_id;
    let (mut routines, bot) = shown(vec![on.clone()]);
    assert!(routines.toggle(on_id));
    detail(&mut routines, bot, &on, vec![]);
    // On: not changeable, and no form opens.
    assert!(!routines.access_changeable());
    assert!(!routines.open_access());
    assert!(routines.save_access().is_none());

    let paused = item(false);
    let id = paused.schedule_id;
    let (mut routines, bot) = shown(vec![paused.clone()]);
    assert!(routines.toggle(id));
    detail(&mut routines, bot, &paused, vec![]);
    assert!(routines.access_changeable());
    assert!(routines.open_access());
    // It opens on the access the Routine has: the default one.
    assert_eq!(
        routines.controls_data().access.as_ref().unwrap().access,
        BotAccess::Default
    );
    assert!(routines.choose_access(BotAccess::Workspace));
    let request_of = |command: &BotCommand| match command {
        BotCommand::UpdateRoutineAccess {
            request_id,
            schedule_id,
            expected_grant_revision: 1,
            access: BotAccess::Workspace,
            selected_pages,
            ..
        } if *schedule_id == id && selected_pages.is_empty() => *request_id,
        _ => panic!("an access edit of this Routine"),
    };
    let (seq, command) = sent(routines.save_access());
    let first = request_of(&command);
    assert_eq!(
        routines.controls_data().doing(id),
        Some(Doing::SavingAccess)
    );
    // Still unresolved at the owner: the same request may be asked again.
    assert!(routines.control_answer(seq, bot, id, Err(Refusal::code("routine_unavailable")), 0));
    assert_eq!(
        routines.controls_data().notes(id),
        [Words::BotsRoutineErrUnavailable]
    );
    let (seq, command) = sent(routines.save_access());
    assert_eq!(request_of(&command), first);
    // A refusal the owner keeps for that request: the next try is a new one.
    assert!(routines.control_answer(
        seq,
        bot,
        id,
        Err(Refusal::code("routine_run_in_progress")),
        0,
    ));
    assert_eq!(
        routines.controls_data().notes(id),
        [Words::BotsRoutineErrRunInProgress]
    );
    let (seq, command) = sent(routines.save_access());
    let second = request_of(&command);
    assert_ne!(second, first);
    for (code, words) in [
        (
            "routine_must_be_paused",
            Words::BotsRoutineErrPauseForAccess,
        ),
        ("routine_access_pending", Words::BotsRoutineErrAccessPending),
        ("wrong_owner", Words::BotsRoutineErrPageRights),
    ] {
        let mut probe = Routines::default();
        probe.show(Some(bot), true);
        list(&mut probe, bot, vec![paused.clone()]);
        assert!(probe.toggle(id));
        detail(&mut probe, bot, &paused, vec![]);
        assert!(probe.open_access());
        let (seq, _) = sent(probe.save_access());
        assert!(probe.control_answer(seq, bot, id, Err(Refusal::code(code)), 0));
        assert_eq!(probe.controls_data().notes(id), [words], "{code}");
    }
    // Saved: the row shows the new access, and the choices close.
    assert!(routines.control_answer(
        seq,
        bot,
        id,
        Ok(BotResponse::RoutineAccess {
            access: BotRoutineAccess {
                schedule_id: id,
                grant_revision: 2,
                access: BotAccess::Workspace,
                selected_page_count: 0,
            },
        }),
        0,
    ));
    let grant = &routines.row(id).unwrap().grant;
    assert_eq!(
        (grant.grant_revision, grant.access),
        (2, BotAccess::Workspace)
    );
    assert_eq!(routines.controls_data().access, None);
}

#[test]
fn routine_control_restore_lists_stay_until_the_list_is_refreshed() {
    let (needs, still) = (item(false), item(true));
    let (needs_id, still_id) = (needs.schedule_id, still.schedule_id);
    let (mut routines, bot) = shown(vec![needs.clone(), still.clone()]);
    routines.note_restored(Restored {
        needing_access: vec![needs_id],
        still_on: vec![still_id],
    });
    // The pane reads the list again by itself, and keeps what Restore said.
    list(&mut routines, bot, vec![needs, still]);
    let restored = routines.controls_data().restored.unwrap();
    assert!(restored.needs_a_look());
    assert_eq!(restored.needing_access, [needs_id]);
    assert_eq!(restored.still_on, [still_id]);
    assert!(routines.refresh(false));
    assert_eq!(routines.controls_data().restored, None);
    assert!(!Restored::default().needs_a_look());
}
