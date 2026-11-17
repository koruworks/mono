// SPDX-License-Identifier: MPL-2.0
//! The Routine controls in the Bot's pane (P4.3 piece 4): the real Bot link,
//! the context frame and the window's host, all in memory. The owner is the
//! test: it answers each command, or does not.
use super::routines::{
    answer, detail_answer, detail_page, detail_request, held, item, lines, page, request,
    select_detail, setup,
};
use super::*;
use crate::bots::routines::control::Refusal;
use crate::bots::view::context_library::ROUTINES_SECTION;
use crate::bots::view::context_routines::{
    decide_key,
    detail::{
        ACCESS_CANCEL, ACCESS_SAVE, ACCESS_WORKSPACE, CHANGE_ACCESS, EDIT_TIMING, OLDER_HISTORY,
        history_key,
    },
    details_key, test_key, toggle_key,
};
use koru_ai_core::bots::routines::*;
use koru_ai_core::bots::{BotAccess, BotCommand, BotResponse};
use koru_delegate_core::{DeviceId, RoutineControlOutcome, RunId, ScheduleId, SchedulePauseReason};
use koru_delegate_schedule::{ScheduleName, ScheduleState};

const MINUTE: i64 = 60_000;
/// A time well inside this year, so its date is said without the year.
fn soon() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        / MINUTE
        * MINUTE
}
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
fn routine(name: &str, on: bool, next: RoutineNextObservation) -> BotRoutineListItem {
    let mut row = item();
    row.live = BotRoutineLiveObservation::Current {
        name: ScheduleName::new(name).unwrap(),
        definition_revision: 2,
        state: state(on),
        next,
    };
    row
}
fn paused(name: &str) -> BotRoutineListItem {
    routine(name, false, RoutineNextObservation::Inactive {})
}
fn listing(bot: &Bot, items: Vec<BotRoutineListItem>) -> BotRoutineListPage {
    let mut listed = page(bot);
    listed.items = items;
    listed
}
/// The lines the pane shows, one per element.
fn said(v: &MainView) -> Vec<String> {
    lines(v).split('\n').map(str::to_owned).collect()
}
fn says(v: &MainView, line: &str) -> bool {
    said(v).iter().any(|shown| shown == line)
}
fn disabled(v: &MainView, key: Key) -> Option<bool> {
    v.bot_context
        .with_frame(|frame| frame.node(key).map(|node| node.disabled))
        .flatten()
}
fn label(v: &MainView, key: Key) -> Option<String> {
    v.bot_context
        .with_frame(|frame| {
            frame.node(key).and_then(|node| match node.label.as_ref()? {
                koru_draw::el::Name::Text(value) => Some(value.clone()),
                koru_draw::el::Name::Key(key) => {
                    Some(koru_i18n::t(koru_i18n::Lang::En, *key).to_owned())
                }
            })
        })
        .flatten()
}
/// The one control the pane sent, if it sent one.
fn control(commands: &Receiver<Command>) -> Option<(u64, ScheduleId, BotCommand)> {
    let mut sent: Vec<_> = commands
        .try_iter()
        .filter_map(|command| match command {
            Command::RoutineControl {
                seq,
                schedule,
                command,
                ..
            } => Some((seq, schedule, *command)),
            _ => None,
        })
        .collect();
    assert!(sent.len() <= 1, "one press is one command");
    sent.pop()
}
fn reply(
    v: &mut MainView,
    tx: &SyncSender<Answer>,
    bot: &Bot,
    seq: u64,
    schedule: ScheduleId,
    result: Result<BotResponse, Refusal>,
) {
    tx.send(Answer::RoutineControl {
        seq,
        bot: bot.id,
        schedule,
        result,
    })
    .unwrap();
    held(v).shown_pass(None);
    v.sync_bot_context();
}
fn confirmed(schedule: ScheduleId, action: BotRoutineControlAction, on: bool) -> BotResponse {
    BotResponse::RoutineControl {
        control: BotRoutineControl {
            schedule_id: schedule,
            action,
            outcome: RoutineControlOutcome::Applied,
            definition_revision: 3,
            state: state(on),
            next: if on {
                RoutineNextObservation::Exact {
                    at_ms: soon() + 5 * MINUTE,
                }
            } else {
                RoutineNextObservation::Inactive {}
            },
        },
    }
}
/// Answer the list read the pane asked for by itself.
fn reread(
    v: &mut MainView,
    commands: &Receiver<Command>,
    tx: &SyncSender<Answer>,
    bot: &Bot,
    listed: BotRoutineListPage,
) {
    let seq = request(v, commands);
    answer(v, tx, bot, seq, Ok(listed));
}
fn next_line(at_ms: i64) -> String {
    format!(
        "Next: {}",
        crate::bots::view::context_routines::date(koru_i18n::Lang::En, at_ms)
    )
}

#[test]
fn routine_control_list_shows_each_routines_next_time_and_true_state() {
    let (mut v, _w) = on_document();
    let bot = bot();
    let at = soon() + 90 * MINUTE;
    let on = routine(
        "Morning reading",
        true,
        RoutineNextObservation::Exact { at_ms: at },
    );
    let off = paused("Weekly digest");
    let elsewhere = routine(
        "Owned elsewhere",
        true,
        RoutineNextObservation::OwnerLocal {},
    );
    let (on_id, off_id) = (on.schedule_id, off.schedule_id);
    setup(&mut v, &bot, listing(&bot, vec![on, off, elsewhere]));
    // The heading is Routines, and no row calls a Routine a schedule.
    assert_eq!(label(&v, ROUTINES_SECTION).as_deref(), Some("Routines"));
    let shown = lines(&v);
    assert!(
        !shown.contains("Upcoming") && !shown.contains("Schedule"),
        "{shown}"
    );
    for name in ["Morning reading", "Weekly digest", "Owned elsewhere"] {
        assert!(says(&v, name), "{name}");
    }
    // The state in words, and the next time from the list read.
    assert!(says(&v, "On") && says(&v, "Paused"));
    assert!(says(&v, &next_line(at)), "{shown}");
    assert!(says(&v, "Next: Owner local"));
    // A paused Routine says no next time.
    assert_eq!(
        said(&v)
            .iter()
            .filter(|line| line.starts_with("Next: "))
            .count(),
        2
    );
    // The switch is the state, for the eye and for assistive technology.
    let data = v
        .bot_context
        .data
        .as_ref()
        .unwrap()
        .routines
        .as_ref()
        .unwrap();
    assert_eq!(data.switch_state(toggle_key(on_id)), Some(true));
    assert_eq!(data.switch_state(toggle_key(off_id)), Some(false));
    assert_eq!(data.switch_state(details_key(on_id)), None);
    assert_eq!(
        label(&v, toggle_key(on_id)).as_deref(),
        Some("Pause Morning reading")
    );
    assert_eq!(
        label(&v, toggle_key(off_id)).as_deref(),
        Some("Turn on Weekly digest")
    );
    // Every control of a row is reachable from the keyboard, in row order.
    let keys = v.bot_context.keys();
    let at = |key| keys.iter().position(|found| *found == key).unwrap();
    assert!(at(toggle_key(on_id)) < at(details_key(on_id)));
    assert!(at(details_key(on_id)) < at(toggle_key(off_id)));
    assert!(at(toggle_key(off_id)) < at(test_key(off_id)));
    // Test is offered only while paused.
    assert!(disabled(&v, test_key(off_id)) == Some(false));
    assert_eq!(disabled(&v, test_key(on_id)), None);
}

#[test]
fn routine_control_enable_and_pause_show_pending_until_the_owner_answers() {
    let (mut v, _w) = on_document();
    let bot = bot();
    let off = paused("Weekly digest");
    let id = off.schedule_id;
    let (commands, tx) = setup(&mut v, &bot, listing(&bot, vec![off]));
    assert!(v.bot_context_action(toggle_key(id)));
    let (seq, schedule, command) = control(&commands).expect("Enable was sent");
    assert_eq!(schedule, id);
    assert!(matches!(
        command,
        BotCommand::SetRoutineLifecycle {
            expected_definition_revision: 2,
            action: BotRoutineLifecycleAction::Enable,
            ..
        }
    ));
    // Pending: said, not pressable, and never shown as on.
    assert!(says(&v, "Turning on…"));
    assert!(!says(&v, "On") && !says(&v, "Paused"));
    assert_eq!(disabled(&v, toggle_key(id)), Some(true));
    assert!(!v.bot_context_action(toggle_key(id)));
    assert!(!v.bot_context_action(test_key(id)));
    assert!(control(&commands).is_none());
    let data = v
        .bot_context
        .data
        .as_ref()
        .unwrap()
        .routines
        .as_ref()
        .unwrap();
    assert_eq!(data.switch_state(toggle_key(id)), Some(false));
    // The owner's answer is what is shown.
    reply(
        &mut v,
        &tx,
        &bot,
        seq,
        id,
        Ok(confirmed(id, BotRoutineControlAction::Enable, true)),
    );
    assert!(says(&v, "On") && !says(&v, "Turning on…"));
    assert!(said(&v).iter().any(|line| line.starts_with("Next: ")));
    assert_eq!(disabled(&v, toggle_key(id)), Some(false));
    assert_eq!(
        disabled(&v, test_key(id)),
        None,
        "no Test for a Routine that is on"
    );
    // Pause, the same way, against the revision the answer gave.
    assert!(v.bot_context_action(toggle_key(id)));
    let (seq, _, command) = control(&commands).expect("Pause was sent");
    assert!(matches!(
        command,
        BotCommand::SetRoutineLifecycle {
            expected_definition_revision: 3,
            action: BotRoutineLifecycleAction::Pause,
            ..
        }
    ));
    assert!(says(&v, "Pausing…") && !says(&v, "Paused"));
    reply(
        &mut v,
        &tx,
        &bot,
        seq,
        id,
        Ok(confirmed(id, BotRoutineControlAction::Pause, false)),
    );
    assert!(says(&v, "Paused") && !says(&v, "Pausing…"));
}

#[test]
fn routine_control_refusal_is_a_sentence_and_a_lost_answer_is_checked() {
    let (mut v, _w) = on_document();
    let bot = bot();
    let off = paused("Weekly digest");
    let id = off.schedule_id;
    let (commands, tx) = setup(&mut v, &bot, listing(&bot, vec![off.clone()]));
    assert!(v.bot_context_action(toggle_key(id)));
    let (seq, ..) = control(&commands).unwrap();
    reply(
        &mut v,
        &tx,
        &bot,
        seq,
        id,
        Err(Refusal::not_ready(&["bot_paused", "access_pending"])),
    );
    assert!(says(&v, "The Bot is paused. Resume the Bot first."));
    assert!(says(&v, "An access change is still being applied."));
    assert!(says(&v, "Paused"), "the state nobody changed");
    assert!(!lines(&v).contains("routine_"), "no code is shown");
    // Lost: Checking…, the list read again, and then what it read.
    assert!(v.bot_context_action(toggle_key(id)));
    let (seq, ..) = control(&commands).unwrap();
    reply(&mut v, &tx, &bot, seq, id, Err(Refusal::unanswered()));
    assert!(says(&v, "Checking…"));
    assert_eq!(disabled(&v, toggle_key(id)), Some(true));
    let mut now_on = off;
    now_on.live = BotRoutineLiveObservation::Current {
        name: ScheduleName::new("Weekly digest").unwrap(),
        definition_revision: 3,
        state: state(true),
        next: RoutineNextObservation::OwnerLocal {},
    };
    reread(&mut v, &commands, &tx, &bot, listing(&bot, vec![now_on]));
    assert!(says(&v, "On") && !says(&v, "Checking…"));
}

#[test]
fn routine_control_test_wait_is_a_disabled_button_with_a_time_not_an_error() {
    let (mut v, _w) = on_document();
    let bot = bot();
    let off = paused("Weekly digest");
    let id = off.schedule_id;
    let (commands, tx) = setup(&mut v, &bot, listing(&bot, vec![off]));
    assert_eq!(
        label(&v, test_key(id)).as_deref(),
        Some("Test: Weekly digest")
    );
    assert!(v.bot_context_action(test_key(id)));
    let (seq, _, command) = control(&commands).unwrap();
    assert!(matches!(
        command,
        BotCommand::TestRoutine {
            expected_definition_revision: 2,
            ..
        }
    ));
    assert!(
        label(&v, test_key(id))
            .unwrap()
            .starts_with("Starting test…")
    );
    assert_eq!(disabled(&v, test_key(id)), Some(true));
    let before = said(&v).len();
    reply(
        &mut v,
        &tx,
        &bot,
        seq,
        id,
        Err(Refusal::code("routine_test_too_soon")),
    );
    let waiting = label(&v, test_key(id)).unwrap();
    assert!(waiting.starts_with("Test available at "), "{waiting}");
    assert_eq!(disabled(&v, test_key(id)), Some(true));
    assert!(!v.bot_context_action(test_key(id)));
    // No error line was added, and the Routine is still paused.
    assert_eq!(said(&v).len(), before);
    assert!(says(&v, "Paused"));
    // The tab asks for the clock once, for the end of the wait.
    assert!(held(&mut v).routines.control_deadline().is_some());
    assert!(
        control(&commands).is_none(),
        "the pane starts no Test by itself"
    );
}

#[test]
fn routine_control_test_result_appears_in_the_routines_history() {
    let (mut v, _w) = on_document();
    let bot = bot();
    let off = paused("Weekly digest");
    let id = off.schedule_id;
    let listed = listing(&bot, vec![off]);
    let (commands, tx) = setup(&mut v, &bot, listed.clone());
    assert!(v.bot_context_action(test_key(id)));
    let (seq, ..) = control(&commands).unwrap();
    let minute = soon();
    reply(
        &mut v,
        &tx,
        &bot,
        seq,
        id,
        Ok(BotResponse::RoutineTest {
            test: BotRoutineTest {
                schedule_id: id,
                occurrence_ms: minute,
            },
        }),
    );
    // Accepted: its history is opened and read, and it is still paused.
    let (seq, schedule, _) = detail_request(&mut v, &commands);
    assert_eq!(schedule, id);
    let mut detail = detail_page(&bot, &listed, id);
    let chat = uuid::Uuid::new_v4();
    detail.occurrences = RoutineOccurrenceHistoryObservation::Available {
        items: vec![RoutineOccurrenceLine {
            occurrence_ms: minute,
            outcome: RoutineOccurrenceOutcome::Ran,
            test: true,
            recorded_at_ms: minute + 1_000,
            conversation: Some(BotRoutineLocalConversation {
                occurrence_ms: minute,
                run_id: RunId::mint(),
                chat_id: chat,
                attempt_id: uuid::Uuid::new_v4(),
                conversation: BotRoutineConversationVisibility::Available,
            }),
        }],
        next_before_occurrence_ms: None,
    };
    detail_answer(&mut v, &tx, &bot, seq, id, Ok(detail));
    let date = crate::bots::view::context_routines::date(koru_i18n::Lang::En, minute);
    assert_eq!(
        label(&v, history_key(minute)),
        Some(format!("Open conversation · {date} · Test"))
    );
    assert!(says(&v, "Paused"));
    assert!(!says(&v, "The test didn't start. Try again."));
    // The line opens its conversation through the checked open.
    assert!(v.bot_context_action(history_key(minute)));
    assert!(commands.try_iter().any(|command| matches!(
        command,
        Command::RoutineLabel { chat: asked, .. } if asked.as_uuid() == chat
    )));
}

#[test]
fn routine_control_history_shows_ran_missed_skipped_and_test_lines() {
    let (mut v, _w) = on_document();
    let bot = bot();
    let on = routine(
        "Morning reading",
        true,
        RoutineNextObservation::OwnerLocal {},
    );
    let id = on.schedule_id;
    let listed = listing(&bot, vec![on]);
    let (commands, tx) = setup(&mut v, &bot, listed.clone());
    assert!(v.bot_context_action(details_key(id)));
    let (seq, ..) = detail_request(&mut v, &commands);
    let base = soon() - 10 * MINUTE;
    let line = |n: i64, outcome, test| RoutineOccurrenceLine {
        occurrence_ms: base - n * MINUTE,
        outcome,
        test,
        recorded_at_ms: base,
        conversation: None,
    };
    let mut ran = line(0, RoutineOccurrenceOutcome::Ran, false);
    ran.conversation = Some(BotRoutineLocalConversation {
        occurrence_ms: ran.occurrence_ms,
        run_id: RunId::mint(),
        chat_id: uuid::Uuid::new_v4(),
        attempt_id: uuid::Uuid::new_v4(),
        conversation: BotRoutineConversationVisibility::Available,
    });
    let mut detail = detail_page(&bot, &listed, id);
    detail.occurrences = RoutineOccurrenceHistoryObservation::Available {
        items: vec![
            ran,
            line(1, RoutineOccurrenceOutcome::Missed, false),
            line(2, RoutineOccurrenceOutcome::SkippedBusy, false),
            line(3, RoutineOccurrenceOutcome::Ran, true),
            line(4, RoutineOccurrenceOutcome::SkippedBusy, true),
        ],
        next_before_occurrence_ms: Some(base - 4 * MINUTE),
    };
    detail_answer(&mut v, &tx, &bot, seq, id, Ok(detail));
    let date =
        |n: i64| crate::bots::view::context_routines::date(koru_i18n::Lang::En, base - n * MINUTE);
    assert!(says(&v, "History"));
    // A minute that ran here opens its conversation; the rest are lines.
    assert_eq!(
        label(&v, history_key(base)),
        Some(format!("Open conversation · {} · Ran", date(0)))
    );
    assert!(says(&v, &format!("{} · Missed", date(1))));
    assert!(says(&v, &format!("{} · Skipped, Bot was busy", date(2))));
    // A Test that ran elsewhere has no conversation here: still a line.
    assert!(says(&v, &format!("{} · Test", date(3))));
    assert!(says(
        &v,
        &format!("{} · Test · Skipped, Bot was busy", date(4))
    ));
    assert_eq!(
        disabled(&v, history_key(base - MINUTE)),
        None,
        "a miss opens nothing"
    );
    // Missed work is history only: nothing offers to catch it up.
    assert!(!lines(&v).to_lowercase().contains("catch"));
    // Older lines are one more read, from the cursor the owner gave.
    assert!(v.bot_context_action(OLDER_HISTORY));
    let (_, schedule, before) = detail_request(&mut v, &commands);
    assert_eq!((schedule, before), (id, Some(base - 4 * MINUTE)));

    // Nothing yet.
    let (mut v, _w) = on_document();
    let (commands, tx) = setup(&mut v, &bot, listed.clone());
    assert!(v.bot_context_action(details_key(id)));
    let (seq, ..) = detail_request(&mut v, &commands);
    let mut empty = detail_page(&bot, &listed, id);
    empty.occurrences = RoutineOccurrenceHistoryObservation::Available {
        items: vec![],
        next_before_occurrence_ms: None,
    };
    detail_answer(&mut v, &tx, &bot, seq, id, Ok(empty));
    assert!(says(&v, "Nothing has run yet."));
}

#[test]
fn routine_control_zone_line_for_an_owner_and_a_non_owner() {
    for (zone, line) in [
        (
            RoutineOwnerZoneObservation::ThisComputer {
                iana: "Europe/Zurich".into(),
            },
            "Time zone: Europe/Zurich, this computer",
        ),
        (
            RoutineOwnerZoneObservation::OwnerLocal {},
            "Time zone: Owner local",
        ),
        (
            RoutineOwnerZoneObservation::Fixed {
                iana: "Asia/Tokyo".into(),
            },
            "Time zone: Asia/Tokyo",
        ),
    ] {
        let (mut v, _w) = on_document();
        let bot = bot();
        let on = routine(
            "Morning reading",
            true,
            RoutineNextObservation::OwnerLocal {},
        );
        let id = on.schedule_id;
        let listed = listing(&bot, vec![on]);
        let (commands, tx) = setup(&mut v, &bot, listed.clone());
        assert!(v.bot_context_action(details_key(id)));
        let (seq, ..) = detail_request(&mut v, &commands);
        let mut detail = detail_page(&bot, &listed, id);
        let owned = matches!(zone, RoutineOwnerZoneObservation::ThisComputer { .. });
        let at = soon() + 30 * MINUTE;
        if let RoutineDetailLive::Current {
            owner_zone, next, ..
        } = &mut detail.live
        {
            *owner_zone = zone;
            if owned {
                *next = RoutineNextObservation::Exact { at_ms: at };
            }
        }
        detail_answer(&mut v, &tx, &bot, seq, id, Ok(detail));
        assert!(says(&v, line), "{line}: {}", lines(&v));
        // Only the owning computer knows the exact next time of a floating
        // rule; the viewer's zone is never put in its place.
        assert_eq!(says(&v, &next_line(at)), owned);
    }
}

#[test]
fn routine_control_access_edit_is_unavailable_while_on_and_says_why() {
    let (mut v, _w) = on_document();
    let bot = bot();
    let on = routine(
        "Morning reading",
        true,
        RoutineNextObservation::OwnerLocal {},
    );
    let off = paused("Weekly digest");
    let (on_id, id) = (on.schedule_id, off.schedule_id);
    let listed = listing(&bot, vec![on, off]);
    let (commands, tx) = setup(&mut v, &bot, listed.clone());
    select_detail(&mut v, &commands, &tx, &bot, on_id);
    // On: the control is there, cannot be pressed, and the reason is said.
    assert_eq!(disabled(&v, CHANGE_ACCESS), Some(true));
    assert!(!v.bot_context_action(CHANGE_ACCESS));
    assert!(says(&v, "Pause this Routine to change its access."));
    // Access now, what it means, and the access it was set up with.
    assert!(says(
        &v,
        "Access: create pages; edit created or 2 selected pages."
    ));
    assert!(says(
        &v,
        "Asks before editing pages it didn't create or wasn't given."
    ));
    assert!(lines(&v).contains("Access at setup"));

    // Paused: the choices open on what the Routine has.
    assert!(v.bot_context_action(details_key(id)));
    let (seq, ..) = detail_request(&mut v, &commands);
    detail_answer(
        &mut v,
        &tx,
        &bot,
        seq,
        id,
        Ok(detail_page(&bot, &listed, id)),
    );
    assert_eq!(disabled(&v, CHANGE_ACCESS), Some(false));
    assert!(!says(&v, "Pause this Routine to change its access."));
    assert!(v.bot_context_action(CHANGE_ACCESS));
    assert!(says(&v, "A new Routine starts with the default access."));
    assert!(says(&v, "Saving removes the 2 pages selected at setup."));
    assert_eq!(
        label(&v, ACCESS_WORKSPACE).as_deref(),
        Some("Whole workspace")
    );
    assert!(v.bot_context_action(ACCESS_WORKSPACE));
    assert!(
        says(&v, "Can edit any page in this Bot's home workspace.")
            || lines(&v).contains("Can edit any page in ")
    );
    assert!(v.bot_context_action(ACCESS_SAVE));
    let (seq, schedule, command) = control(&commands).expect("one access edit");
    assert_eq!(schedule, id);
    assert!(matches!(
        &command,
        BotCommand::UpdateRoutineAccess {
            expected_grant_revision: 1,
            access: BotAccess::Workspace,
            selected_pages,
            ..
        } if selected_pages.is_empty()
    ));
    // Pending: Saving…, and neither Save nor Cancel can be pressed.
    assert_eq!(label(&v, ACCESS_SAVE).as_deref(), Some("Saving…"));
    assert_eq!(disabled(&v, ACCESS_SAVE), Some(true));
    assert_eq!(disabled(&v, ACCESS_CANCEL), Some(true));
    reply(
        &mut v,
        &tx,
        &bot,
        seq,
        id,
        Err(Refusal::code("routine_run_in_progress")),
    );
    assert!(says(
        &v,
        "A run of this Routine is still in progress. Change its access when it finishes."
    ));
    // Asked again, and saved: the row shows the access the owner confirmed.
    assert!(v.bot_context_action(ACCESS_SAVE));
    let (seq, ..) = control(&commands).unwrap();
    reply(
        &mut v,
        &tx,
        &bot,
        seq,
        id,
        Ok(BotResponse::RoutineAccess {
            access: BotRoutineAccess {
                schedule_id: id,
                grant_revision: 2,
                access: BotAccess::Workspace,
                selected_page_count: 0,
            },
        }),
    );
    assert!(says(&v, "Access: pages in this Bot's home workspace."));
    assert_eq!(disabled(&v, ACCESS_SAVE), None, "the choices closed");
}

#[test]
fn routine_control_edit_timing_saves_name_and_time_in_one_command() {
    use crate::mainview::delegate::{Hit, Phase};
    let (mut v, _w) = on_document();
    let bot = bot();
    let on = routine(
        "Morning reading",
        true,
        RoutineNextObservation::OwnerLocal {},
    );
    let id = on.schedule_id;
    let listed = listing(&bot, vec![on]);
    let (commands, tx) = setup(&mut v, &bot, listed.clone());
    select_detail(&mut v, &commands, &tx, &bot, id);
    v.bot_context.focus = Some(EDIT_TIMING);
    assert!(v.bot_context_action(EDIT_TIMING));
    // The dialog is the existing recurrence picker, with the name, and
    // nothing of a run: no task, agent, folder or access.
    let modal = v.delegation.as_ref().expect("the timing dialog");
    assert!(modal.routine.as_ref().is_some_and(|routine| routine.on));
    assert_eq!(modal.schedule_name.value(), "Morning reading");
    let ring = modal.controls();
    assert_eq!(ring.first(), Some(&Hit::ScheduleName));
    assert!(ring.contains(&Hit::ScheduleDaily) && ring.contains(&Hit::Submit));
    assert!(!ring.iter().any(|hit| matches!(
        hit,
        Hit::Objective | Hit::Agent(_) | Hit::Scope(_) | Hit::RunOnce | Hit::Repeat
    )));
    let shown = v.delegation_sentences();
    assert!(shown.contains(&"Edit timing".to_owned()), "{shown:?}");
    assert!(shown.contains(&"Save".to_owned()), "{shown:?}");
    assert!(
        shown.contains(&"This Routine is on. The new time applies right away.".to_owned()),
        "{shown:?}"
    );
    assert!(
        shown.contains(&"Time zone: Owner local".to_owned()),
        "{shown:?}"
    );
    // Type a new name and a new time, then Save from the keyboard.
    let modal = v.delegation.as_mut().unwrap();
    modal
        .schedule_name
        .set_text(&mut v.engine, "Evening reading");
    modal.repeat.advanced.set_text(&mut v.engine, "30 18 * * *");
    modal.repeat.advanced_changed(&mut v.engine);
    let chord = if cfg!(target_os = "macos") {
        ModifiersState::SUPER
    } else {
        ModifiersState::CONTROL
    };
    assert!(v.delegation_key(&InputKey::Named(NamedKey::Enter), None, chord));
    let (seq, schedule, command) = control(&commands).expect("one Save is one command");
    assert_eq!(schedule, id);
    let BotCommand::EditRoutine {
        expected_definition_revision,
        expected_name,
        name,
        recurrence,
        ..
    } = command
    else {
        panic!("an edit of the Routine")
    };
    assert_eq!(expected_definition_revision, 2);
    assert_eq!(expected_name.as_str(), "Morning reading");
    assert_eq!(name.as_str(), "Evening reading");
    assert_eq!(recurrence.cron().as_str(), "30 18 * * *");
    // Saving…, until the owner answers.
    assert_eq!(v.delegation.as_ref().unwrap().phase, Phase::Scheduling);
    let shown = v.delegation_sentences();
    assert!(shown.contains(&"Saving…".to_owned()), "{shown:?}");
    assert!(!v.maintain_routine_timing());
    // Changed meanwhile: the form comes back with what was typed.
    reply(
        &mut v,
        &tx,
        &bot,
        seq,
        id,
        Err(Refusal::code("revision_conflict")),
    );
    assert!(v.maintain_routine_timing());
    let modal = v.delegation.as_ref().unwrap();
    assert_eq!(
        modal.phase,
        Phase::Editing {
            problem: Some(koru_i18n::Key::BotsRoutineErrChanged)
        }
    );
    assert_eq!(modal.schedule_name.value(), "Evening reading");
    assert_eq!(modal.repeat.advanced.value(), "30 18 * * *");
    // The latest is read, and the next Save names it.
    reread(&mut v, &commands, &tx, &bot, listed.clone());
    let (seq2, ..) = detail_request(&mut v, &commands);
    let mut latest = detail_page(&bot, &listed, id);
    if let RoutineDetailLive::Current {
        definition_revision,
        ..
    } = &mut latest.live
    {
        *definition_revision = 5;
    }
    detail_answer(&mut v, &tx, &bot, seq2, id, Ok(latest));
    assert!(v.delegation_key(&InputKey::Named(NamedKey::Enter), None, chord));
    let (seq, _, command) = control(&commands).expect("the second Save");
    assert!(matches!(
        command,
        BotCommand::EditRoutine {
            expected_definition_revision: 5,
            ..
        }
    ));
    reply(
        &mut v,
        &tx,
        &bot,
        seq,
        id,
        Ok(confirmed(id, BotRoutineControlAction::Edit, true)),
    );
    assert!(v.maintain_routine_timing());
    // Applied: the dialog is gone, the keyboard is back where it was, and
    // the pane shows the new name.
    assert!(v.delegation.is_none());
    assert_eq!(v.bot_context.focus, Some(EDIT_TIMING));
    v.sync_bot_context();
    assert!(says(&v, "Evening reading"));
}

#[test]
fn routine_control_a_routine_waiting_for_a_decision_is_marked_and_leads_to_it() {
    let (mut v, _w) = on_document();
    let bot = bot();
    let waiting = paused("Weekly digest");
    let quiet = paused("Morning reading");
    let (id, quiet_id) = (waiting.schedule_id, quiet.schedule_id);
    let BotRoutineLocalConversations::Available { items, .. } = &waiting.local_conversations else {
        panic!()
    };
    let chat = koru_ai_core::ChatId::parse(&items[0].chat_id.to_string()).unwrap();
    let (commands, _tx) = setup(&mut v, &bot, listing(&bot, vec![waiting, quiet]));
    // Nothing waits: nothing is marked.
    assert!(!says(&v, "A Routine of this Bot needs you."));
    assert_eq!(disabled(&v, decide_key(id)), None);
    // The window's activity read says that conversation needs the person.
    v.explore.bots_mut().activity_read_for_test(
        vec![koru_ai_core::bots::BotActivity {
            waiting: None,
            bot_id: bot.id,
            chat_id: chat,
            state: koru_ai_core::bots::BotActivityState::NeedsYou,
        }],
        false,
        std::time::Instant::now(),
    );
    v.sync_bot_context();
    // The Bot's pane and the Routine both say so, in words.
    assert!(says(&v, "A Routine of this Bot needs you."));
    assert!(says(&v, "Needs you: a run is waiting for your decision."));
    assert_eq!(
        label(&v, decide_key(id)).as_deref(),
        Some("Open to decide: Weekly digest")
    );
    assert_eq!(
        disabled(&v, decide_key(quiet_id)),
        None,
        "only that Routine"
    );
    // It leads to the conversation, where the approval card is.
    assert!(v.bot_context_action(decide_key(id)));
    assert!(commands.try_iter().any(|command| matches!(
        command,
        Command::RoutineLabel { chat: asked, .. } if asked == chat
    )));
    // A Run still going under a paused Routine is said too.
    v.explore.bots_mut().activity_read_for_test(
        vec![koru_ai_core::bots::BotActivity {
            waiting: None,
            bot_id: bot.id,
            chat_id: chat,
            state: koru_ai_core::bots::BotActivityState::Running,
        }],
        false,
        std::time::Instant::now(),
    );
    v.sync_bot_context();
    assert!(says(&v, "A run in progress will finish."));
    assert!(!says(&v, "A Routine of this Bot needs you."));
}

#[test]
fn routine_control_restore_lists_are_shown_on_the_routines() {
    use crate::bots::routines::control::Restored;
    let (mut v, _w) = on_document();
    let bot = bot();
    let needs = paused("Weekly digest");
    let still = routine(
        "Morning reading",
        true,
        RoutineNextObservation::OwnerLocal {},
    );
    let fine = paused("Inbox sweep");
    let (needs_id, still_id) = (needs.schedule_id, still.schedule_id);
    let listed = listing(&bot, vec![needs, still, fine]);
    let (commands, tx) = setup(&mut v, &bot, listed.clone());
    assert!(!says(&v, "Routines stay paused until you turn them on."));
    // A Restore made elsewhere in the window reaches every tab of the Bot.
    v.bot_routines_restored(
        bot.id,
        &Restored {
            needing_access: vec![needs_id],
            still_on: vec![still_id],
        },
    );
    reread(&mut v, &commands, &tx, &bot, listed);
    assert!(says(&v, "Routines stay paused until you turn them on."));
    assert!(says(&v, "These Routines may still be on: Morning reading"));
    assert_eq!(
        said(&v)
            .iter()
            .filter(|line| *line == "Choose access again")
            .count(),
        1,
        "on the one Routine whose access cannot be read"
    );
}

#[test]
fn routine_control_hiding_the_pane_drops_what_was_pending() {
    let (mut v, _w) = on_document();
    let bot = bot();
    let off = paused("Weekly digest");
    let id = off.schedule_id;
    let (commands, tx) = setup(&mut v, &bot, listing(&bot, vec![off.clone()]));
    assert!(v.bot_context_action(toggle_key(id)));
    let (seq, ..) = control(&commands).unwrap();
    // The pane is closed while the control is out.
    v.context.set_immediately(false);
    v.relayout();
    v.sync_bot_context();
    assert!(!held(&mut v).routines.visible());
    assert!(!held(&mut v).routines.control_busy());
    assert_eq!(held(&mut v).routines.control_deadline(), None);
    // Its answer arrives and changes nothing; nothing is asked while hidden.
    tx.send(Answer::RoutineControl {
        seq,
        bot: bot.id,
        schedule: id,
        result: Ok(confirmed(id, BotRoutineControlAction::Enable, true)),
    })
    .unwrap();
    held(&mut v).shown_pass(None);
    assert!(!commands.try_iter().any(|command| matches!(
        command,
        Command::ReadRoutines { .. } | Command::RoutineControl { .. }
    )));
    // Opened again, the list is read: the owner says what is true.
    v.context.set_immediately(true);
    v.relayout();
    v.sync_bot_context();
    reread(&mut v, &commands, &tx, &bot, listing(&bot, vec![off]));
    assert!(says(&v, "Paused") && !says(&v, "Turning on…"));
}

#[test]
fn routine_control_every_word_the_pane_says_is_in_all_eight_languages() {
    use koru_i18n::Key as W;
    let keys = [
        W::BotsRoutineOn,
        W::BotsRoutinePausedState,
        W::BotsRoutineTurningOn,
        W::BotsRoutinePausing,
        W::BotsRoutineChecking,
        W::BotsRoutineTurnOnName,
        W::BotsRoutinePauseName,
        W::BotsRoutineNextAt,
        W::BotsRoutineNextOwnerLocal,
        W::BotsRoutineRunWillFinish,
        W::BotsRoutineTest,
        W::BotsRoutineTestStarting,
        W::BotsRoutineTestAvailableAt,
        W::BotsRoutineTestNotStarted,
        W::BotsRoutineEditTiming,
        W::BotsRoutineSave,
        W::BotsRoutineSaving,
        W::BotsRoutineInvalidTime,
        W::BotsRoutineOnEditNotice,
        W::BotsRoutineAccessNowDefault,
        W::BotsRoutineAccessNowWorkspace,
        W::BotsRoutineAccessAsks,
        W::BotsRoutineAccessAnyPage,
        W::BotsRoutineAccessAnyPageHome,
        W::BotsRoutineChangeAccess,
        W::BotsRoutineAccessChoiceDefault,
        W::BotsRoutineAccessChoiceWorkspace,
        W::BotsRoutineAccessStartsDefault,
        W::BotsRoutineAccessClearsPages,
        W::BotsRoutineErrGone,
        W::BotsRoutineErrWrongOwner,
        W::BotsRoutineErrWorkspace,
        W::BotsRoutineErrBotTrashed,
        W::BotsRoutineErrLifecyclePending,
        W::BotsRoutineErrBotArchived,
        W::BotsRoutineErrOperationPending,
        W::BotsRoutineErrChanged,
        W::BotsRoutineErrNotBound,
        W::BotsRoutineErrDeleted,
        W::BotsRoutineErrElsewhere,
        W::BotsRoutineErrPending,
        W::BotsRoutineErrUnavailable,
        W::BotsRoutineErrOther,
        W::BotsRoutineErrNotReady,
        W::BotsRoutineErrBotPaused,
        W::BotsRoutineErrBotIsArchived,
        W::BotsRoutineErrAccessUnreadable,
        W::BotsRoutineErrAccessApplying,
        W::BotsRoutineErrPauseToTest,
        W::BotsRoutineErrMinuteTaken,
        W::BotsRoutineErrTestBusy,
        W::BotsRoutineErrPauseForAccess,
        W::BotsRoutineErrRunInProgress,
        W::BotsRoutineErrAccessPending,
        W::BotsRoutineErrPageRights,
        W::BotsRoutineHistoryTitle,
        W::BotsRoutineRan,
        W::BotsRoutineMissed,
        W::BotsRoutineSkippedBusy,
        W::BotsRoutineTestRan,
        W::BotsRoutineTestMissed,
        W::BotsRoutineTestSkippedBusy,
        W::BotsRoutineNothingRan,
        W::BotsRoutineHistoryOlder,
        W::BotsRoutineZone,
        W::BotsRoutineZoneThisComputer,
        W::BotsRoutineZoneOwnerLocal,
        W::BotsRoutineNeedsYou,
        W::BotsRoutineOpenDecision,
        W::BotsRoutinesNeedYou,
        W::BotsRoutinesWillPauseOne,
        W::BotsRoutinesWillPauseMany,
        W::BotsRoutinesWillPauseAny,
        W::BotsRoutinesPausingForLifecycle,
        W::BotsRoutinesStayPaused,
        W::BotsRoutineChooseAccessAgain,
        W::BotsRoutinesMayStillBeOn,
        W::BotsRestoredCheckRoutines,
    ];
    assert_eq!(keys.len(), 78, "every string this piece added");
    for key in keys {
        let english = koru_i18n::t(koru_i18n::Lang::En, key);
        // A Routine is never called a schedule to a person.
        assert!(!english.to_lowercase().contains("schedule"), "{english}");
        let mut seen = std::collections::HashSet::new();
        for lang in koru_i18n::Lang::ALL {
            let words = koru_i18n::t(lang, key);
            assert!(!words.trim().is_empty(), "{lang:?} {key:?}");
            assert_eq!(
                words.matches("{}").count(),
                english.matches("{}").count(),
                "{lang:?} {key:?}"
            );
            seen.insert(words);
        }
        // Written for each language, not English copied eight times. A few
        // short words are the same in two or three languages.
        assert!(seen.len() >= 5, "{key:?}: {seen:?}");
    }
}
