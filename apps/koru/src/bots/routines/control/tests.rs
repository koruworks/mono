// SPDX-License-Identifier: MPL-2.0
//! The controls as a model: no window, no link, no clock but the one given.
use super::*;
use koru_delegate_core::{DeviceId, RoutineControlOutcome};
mod native;

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
    assert!(routines.native_detail_answer(
        seq,
        bot,
        schedule,
        Ok(koru_api_client::bot_routines::Snapshot {
            detail: detail_of(bot, item, lines),
            thread: ChatId::new(),
            selected_pages: vec![],
            occurrences: vec![],
            next: None,
        })
    ));
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
