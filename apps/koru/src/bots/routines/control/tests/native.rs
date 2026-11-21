// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_api_client::bot_routines as wire;
use uuid::Uuid;

#[test]
fn created_on_startup_claim_is_visible_only_and_retries_exactly_after_uncertainty() {
    let (mut pane, bot) = shown(vec![item(true)]);
    let Some(Command::RoutineStartup { request, .. }) = pane.startup_read() else {
        panic!("visible native claim")
    };
    assert!(pane.startup_read().is_none());
    assert!(pane.startup_answer(bot, request, Err(Refusal::unanswered())));
    pane.show(Some(bot), false);
    assert!(pane.startup_retry().is_none());
    pane.show(Some(bot), true);
    let Some(Command::RoutineStartup { request: retry, .. }) = pane.startup_retry() else {
        panic!("exact retry")
    };
    assert_eq!(request, retry);
    pane.startup_unsent();
    assert!(
        matches!(pane.startup_retry(),Some(Command::RoutineStartup {request:id,..}) if id==request)
    );
    assert!(pane.startup_answer(
        bot,
        request,
        Ok(wire::Response::Startup {
            request,
            first_enabled: true,
            replayed: true
        })
    ));
    assert!(std::mem::take(&mut pane.startup_offer));
    assert!(!pane.startup_answer(
        bot,
        request,
        Ok(wire::Response::Startup {
            request,
            first_enabled: true,
            replayed: true
        })
    ));
    assert!(!pane.startup_offer);
}

fn sent_native(command: Option<Command>) -> (u64, wire::Command) {
    let Some(Command::NativeRoutineControl { seq, command, .. }) = command else {
        panic!("native control")
    };
    (seq, command)
}
fn occurrence(schedule: ScheduleId, state: &str) -> wire::Occurrence {
    wire::Occurrence {
        id: Uuid::new_v4(),
        routine: schedule.as_uuid(),
        definition_revision: 2,
        occurrence_ms: 120_000,
        manual: true,
        state: state.into(),
        reason: (state == "skipped").then(|| "busy".into()),
        message: None,
        reply: None,
        card: None,
        recorded_at_ms: 120_000,
        updated_at_ms: 120_000,
        preview: None,
        review: None,
    }
}
#[test]
fn native_run_now_is_available_on_and_off_and_keeps_unknown_request_identity() {
    let row = item(true);
    let schedule = row.schedule_id;
    let (mut pane, bot) = shown(vec![row]);
    let (seq, command) = sent_native(pane.test_routine(schedule));
    let wire::Command::RunNow { id, .. } = command.clone() else {
        panic!("Run now")
    };
    assert!(pane.native_control_answer(seq, bot, schedule, Err(Refusal::unanswered())));
    assert!(pane.toggle_routine(schedule).is_none());
    let (_, retry) = sent_native(pane.retry_native());
    assert!(serde_json::to_value(&command).unwrap() == serde_json::to_value(&retry).unwrap());
    pane.control_unsent();
    assert!(pane.toggle_routine(schedule).is_none());
    let (_, retry_again) = sent_native(pane.retry_native());
    assert!(serde_json::to_value(&command).unwrap() == serde_json::to_value(&retry_again).unwrap());
    assert!(pane.native_control_answer(
        seq,
        bot,
        schedule,
        Ok(wire::Response::Started {
            request: id,
            occurrence: occurrence(schedule, "skipped"),
            replayed: true
        })
    ));
    assert!(matches!(
        pane.confirmed(schedule).unwrap().1,
        ScheduleState::Active { .. }
    ));
    assert!(!pane.control_busy());
    assert!(pane.detail.read().is_some());
    assert!(!pane.native_control_answer(
        seq,
        bot,
        schedule,
        Ok(wire::Response::Started {
            request: id,
            occurrence: occurrence(schedule, "queued"),
            replayed: true
        })
    ));
}
#[test]
fn first_enable_offer_is_bound_to_matching_command_and_consumed_once() {
    let row = item(false);
    let schedule = row.schedule_id;
    let (mut pane, bot) = shown(vec![row]);
    let (seq, command) = sent_native(pane.toggle_routine(schedule));
    let wire::Command::Enable { id, .. } = command else {
        panic!("Enable")
    };
    let control = BotRoutineControl {
        schedule_id: schedule,
        action: BotRoutineControlAction::Enable,
        outcome: RoutineControlOutcome::Applied,
        definition_revision: 3,
        state: state(true),
        next: RoutineNextObservation::Inactive {},
    };
    assert!(pane.native_control_answer(
        seq,
        bot,
        schedule,
        Ok(wire::Response::Enabled {
            request: Uuid::new_v4(),
            control: control.clone(),
            first_enabled: true,
            replayed: false
        })
    ));
    assert!(!pane.startup_offer);
    assert!(pane.control_busy());
    let mut wrong_action = control.clone();
    wrong_action.action = BotRoutineControlAction::Pause;
    wrong_action.state = state(false);
    assert!(pane.native_control_answer(
        seq,
        bot,
        schedule,
        Ok(wire::Response::Enabled {
            request: id,
            control: wrong_action,
            first_enabled: true,
            replayed: false
        })
    ));
    assert!(!pane.startup_offer);
    assert!(pane.control_busy());
    assert!(pane.native_control_answer(
        seq,
        bot,
        schedule,
        Ok(wire::Response::Enabled {
            request: id,
            control: control.clone(),
            first_enabled: true,
            replayed: true
        })
    ));
    assert!(std::mem::take(&mut pane.startup_offer));
    assert!(!pane.native_control_answer(
        seq,
        bot,
        schedule,
        Ok(wire::Response::Enabled {
            request: id,
            control,
            first_enabled: true,
            replayed: true
        })
    ));
    assert!(!pane.startup_offer);
}
#[test]
fn native_history_keeps_exact_cursor_and_refuses_navigation_after_hidden_reply() {
    let row = item(false);
    let schedule = row.schedule_id;
    let (mut pane, bot) = shown(vec![row.clone()]);
    pane.toggle(schedule);
    let Some(Command::ReadRoutineDetail { seq, .. }) = pane.detail.read() else {
        panic!("read")
    };
    let cursor = wire::Cursor {
        at_ms: 120_000,
        id: Uuid::new_v4(),
    };
    let snapshot = wire::Snapshot {
        detail: detail_of(bot, &row, vec![]),
        thread: ChatId::new(),
        selected_pages: vec![],
        occurrences: vec![occurrence(schedule, "answered")],
        next: Some(cursor.clone()),
    };
    assert!(pane.native_detail_answer(seq, bot, schedule, Ok(snapshot.clone())));
    assert!(pane.can_open(snapshot.thread));
    let data = crate::bots::view::context_routines::Data::from_state(&pane);
    let keys = data.keys();
    assert!(keys.iter().enumerate().all(|(i, k)| !keys[..i].contains(k)));
    assert!(pane.history_older());
    let Some(Command::ReadRoutineDetail {
        seq, native_before, ..
    }) = pane.detail.read()
    else {
        panic!("older")
    };
    assert_eq!(native_before, Some(cursor));
    pane.show(Some(bot), false);
    assert!(!pane.native_detail_answer(seq, bot, schedule, Ok(snapshot.clone())));
    assert!(!pane.can_open(snapshot.thread));
}

#[test]
fn native_access_form_does_not_drop_pages_after_a_failed_detail_refresh() {
    let row = item(false);
    let schedule = row.schedule_id;
    let (mut pane, bot) = shown(vec![row.clone()]);
    pane.toggle(schedule);
    let Some(Command::ReadRoutineDetail { seq, .. }) = pane.detail.read() else {
        panic!()
    };
    let page = koru_ai_core::bots::BotPage {
        workspace: koru_ai_core::WorkspaceId::new(),
        document: koru_ai_core::DocumentId::new(),
    };
    let mut snapshot = wire::Snapshot {
        detail: detail_of(bot, &row, vec![]),
        thread: ChatId::new(),
        selected_pages: vec![page.clone()],
        occurrences: vec![],
        next: None,
    };
    snapshot.detail.access.selected_page_count = 1;
    pane.native_detail_answer(seq, bot, schedule, Ok(snapshot.clone()));
    assert!(pane.open_access());
    assert!(pane.detail.refresh(false));
    let Some(Command::ReadRoutineDetail { seq, .. }) = pane.detail.read() else {
        panic!()
    };
    pane.native_detail_answer(seq, bot, schedule, Err(ReadFailure::TransportOnly));
    assert!(pane.save_access().is_none());
    assert!(pane.detail.refresh(false));
    let Some(Command::ReadRoutineDetail { seq, .. }) = pane.detail.read() else {
        panic!()
    };
    pane.native_detail_answer(seq, bot, schedule, Ok(snapshot));
    let (_, command) = sent(pane.save_access());
    let BotCommand::UpdateRoutineAccess { selected_pages, .. } = command else {
        panic!()
    };
    assert_eq!(selected_pages, vec![page]);
}

#[test]
fn old_enable_receipt_cannot_overwrite_a_newer_pause_observation() {
    let row = item(false);
    let schedule = row.schedule_id;
    let (mut pane, bot) = shown(vec![row.clone()]);
    let (seq, command) = sent_native(pane.toggle_routine(schedule));
    let wire::Command::Enable { id, .. } = command else {
        panic!()
    };
    pane.native_control_answer(seq, bot, schedule, Err(Refusal::unanswered()));
    assert!(pane.refresh(false));
    let mut newer = row;
    if let BotRoutineLiveObservation::Current {
        definition_revision,
        ..
    } = &mut newer.live
    {
        *definition_revision = 4;
    }
    list(&mut pane, bot, vec![newer]);
    let control = BotRoutineControl {
        schedule_id: schedule,
        action: BotRoutineControlAction::Enable,
        outcome: RoutineControlOutcome::AlreadyApplied,
        definition_revision: 3,
        state: state(true),
        next: RoutineNextObservation::Inactive {},
    };
    assert!(pane.native_control_answer(
        seq,
        bot,
        schedule,
        Ok(wire::Response::Enabled {
            request: id,
            control,
            first_enabled: true,
            replayed: true
        })
    ));
    assert_eq!(pane.confirmed(schedule).unwrap().0, 4);
    assert!(matches!(
        pane.confirmed(schedule).unwrap().1,
        ScheduleState::Paused { .. }
    ));
}

#[test]
fn native_work_history_follows_only_while_visible_and_current() {
    let row = item(false);
    let schedule = row.schedule_id;
    let (mut pane, bot) = shown(vec![row.clone()]);
    pane.toggle(schedule);
    let Some(Command::ReadRoutineDetail { seq, .. }) = pane.detail.read() else {
        panic!()
    };
    let snapshot = wire::Snapshot {
        detail: detail_of(bot, &row, vec![]),
        thread: ChatId::new(),
        selected_pages: vec![],
        occurrences: vec![occurrence(schedule, "queued")],
        next: None,
    };
    pane.native_detail_answer(seq, bot, schedule, Ok(snapshot));
    assert_eq!(pane.control_deadline(), Some(2_000));
    assert!(!pane.control_tick(1_999));
    assert!(pane.control_tick(2_000));
    assert_eq!(pane.control_deadline(), None);
    assert!(pane.detail.read().is_some());
    pane.show(Some(bot), false);
    assert_eq!(pane.control_deadline(), None);
    assert!(!pane.control_tick(10_000));
}
