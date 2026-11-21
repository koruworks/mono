// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::bots::*;
use koru_bots_engine::routines as native;

#[test]
fn modal_creation_is_owner_scoped_narrow_and_retries_one_enabled_routine() {
    use koru_ai_wire::bot_routines::{Command as C, Response as R};
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let principal = crate::GatewayPrincipal {
        label: "synthetic".into(),
        scopes: vec![koru_ai_api::AiScope::ManageBots.as_str().into()],
    };
    let command = C::Create {
        id: Uuid::new_v4(),
        bot: bot.id,
        expected_bot_revision: bot.revision,
        name: "Watch changes".into(),
        objective: "Report meaningful changes".into(),
        recurrence: RoutineRecurrenceRule::zoned("0 9 * * *".parse().unwrap(), "UTC").unwrap(),
    };
    let denied = crate::GatewayPrincipal {
        label: "synthetic".into(),
        scopes: vec![],
    };
    assert!(matches!(
        f.gateway.routine_command(&denied, command.clone()),
        Err(Error::NotAllowed)
    ));
    let mut stale = command.clone();
    if let C::Create {
        expected_bot_revision,
        ..
    } = &mut stale
    {
        *expected_bot_revision += 1;
    }
    assert!(f.gateway.routine_command(&principal, stale).is_err());
    let R::Created { item, replayed, .. } = f
        .gateway
        .routine_command(&principal, command.clone())
        .unwrap()
    else {
        panic!()
    };
    assert!(!replayed);
    assert_eq!(item.grant.access, BotAccess::Default);
    assert_eq!(item.grant.selected_page_count, 0);
    assert!(matches!(
        item.live,
        BotRoutineLiveObservation::Current {
            state: RoutineScheduleState::Active { .. },
            ..
        }
    ));
    let R::Created {
        item: again,
        replayed,
        ..
    } = f.gateway.routine_command(&principal, command).unwrap()
    else {
        panic!()
    };
    assert!(replayed);
    assert_eq!(item.schedule_id, again.schedule_id);
    let R::Startup { first_enabled, .. } = f
        .gateway
        .routine_command(
            &principal,
            C::ClaimStartup {
                id: Uuid::new_v4(),
                bot: bot.id,
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert!(first_enabled);
    let R::Started { occurrence, .. } = f
        .gateway
        .routine_command(
            &principal,
            C::RunNow {
                id: Uuid::new_v4(),
                bot: bot.id,
                routine: item.schedule_id.as_uuid(),
                revision: 1,
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(occurrence.state, "queued");
    let R::History { snapshot } = f
        .gateway
        .routine_command(
            &principal,
            C::History {
                bot: bot.id,
                routine: item.schedule_id.as_uuid(),
                before: None,
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(snapshot.occurrences.len(), 1);
    assert_eq!(snapshot.occurrences[0].id, occurrence.id);
}

#[test]
fn native_carrier_preserves_enable_marker_and_manual_occurrence_identity() {
    use koru_ai_wire::bot_routines::{Command as C, Response as R};
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let BotResponse::RoutineCreated { receipt } = f.gateway.window_command(create(&bot)).unwrap()
    else {
        panic!()
    };
    let principal = crate::GatewayPrincipal {
        label: "synthetic".into(),
        scopes: vec![koru_ai_api::AiScope::ManageBots.as_str().into()],
    };
    let enable = C::Enable {
        id: Uuid::new_v4(),
        bot: bot.id,
        routine: receipt.schedule_id.as_uuid(),
        revision: 1,
        enabled: true,
    };
    let R::Enabled {
        first_enabled,
        control,
        ..
    } = f
        .gateway
        .routine_command(&principal, enable.clone())
        .unwrap()
    else {
        panic!()
    };
    assert!(!first_enabled);
    assert_eq!(control.definition_revision, 1);
    let R::Enabled {
        first_enabled,
        replayed,
        ..
    } = f.gateway.routine_command(&principal, enable).unwrap()
    else {
        panic!()
    };
    assert!(!first_enabled && replayed);
    let claim = C::ClaimStartup {
        id: Uuid::new_v4(),
        bot: bot.id,
    };
    assert!(matches!(
        f.gateway
            .routine_command(&principal, claim.clone())
            .unwrap(),
        R::Startup {
            first_enabled: true,
            replayed: false,
            ..
        }
    ));
    assert!(matches!(
        f.gateway.routine_command(&principal, claim).unwrap(),
        R::Startup {
            first_enabled: true,
            replayed: true,
            ..
        }
    ));
    assert!(matches!(
        f.gateway
            .routine_command(
                &principal,
                C::ClaimStartup {
                    id: Uuid::new_v4(),
                    bot: bot.id
                }
            )
            .unwrap(),
        R::Startup {
            first_enabled: false,
            ..
        }
    ));
    let run = C::RunNow {
        id: Uuid::new_v4(),
        bot: bot.id,
        routine: receipt.schedule_id.as_uuid(),
        revision: 1,
    };
    let R::Started { occurrence, .. } = f.gateway.routine_command(&principal, run.clone()).unwrap()
    else {
        panic!()
    };
    let R::Started {
        occurrence: again,
        replayed,
        ..
    } = f.gateway.routine_command(&principal, run).unwrap()
    else {
        panic!()
    };
    assert_eq!(occurrence.id, again.id);
    assert!(replayed);
    let R::History { snapshot } = f
        .gateway
        .routine_command(
            &principal,
            C::History {
                bot: bot.id,
                routine: receipt.schedule_id.as_uuid(),
                before: None,
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(snapshot.occurrences[0].id, occurrence.id);
    assert!(snapshot.occurrences[0].manual);
    assert!(!snapshot.thread.as_uuid().is_nil());
    let denied = crate::GatewayPrincipal {
        label: "synthetic".into(),
        scopes: vec![],
    };
    assert!(matches!(
        f.gateway.routine_command(
            &denied,
            C::History {
                bot: bot.id,
                routine: receipt.schedule_id.as_uuid(),
                before: None
            }
        ),
        Err(Error::NotAllowed)
    ));
}

fn create(bot: &Bot) -> BotCommand {
    BotCommand::CreateRoutine {
        request_id: BotRequestId::from_uuid(Uuid::new_v4()),
        bot_id: bot.id,
        expected_bot_revision: bot.revision,
        name: RoutineScheduleName::new("Daily briefing").unwrap(),
        objective: koru_delegate_core::Objective::new("Summarize this page").unwrap(),
        documents: vec![bot.config.role.document],
        recurrence: RoutineRecurrenceRule::zoned("0 9 * * *".parse().unwrap(), "America/New_York")
            .unwrap(),
    }
}
#[test]
fn existing_routine_screens_read_the_engine_and_retry_creation_exactly() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let command = create(&bot);
    let BotResponse::RoutineCreated { receipt } =
        f.gateway.window_command(command.clone()).unwrap()
    else {
        panic!("routine receipt")
    };
    f.store.deleted.store(true, Ordering::Relaxed);
    let BotResponse::RoutineCreated { receipt: again } = f.gateway.window_command(command).unwrap()
    else {
        panic!("routine retry")
    };
    assert_eq!(receipt, again);
    let BotResponse::Routines { page } = f
        .gateway
        .window_command(BotCommand::ReadRoutines {
            bot_id: bot.id,
            after_schedule: None,
            limit: 16,
        })
        .unwrap()
    else {
        panic!("list")
    };
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].schedule_id, receipt.schedule_id);
    assert_eq!(page.items[0].grant.access, BotAccess::Default);
    let BotResponse::RoutineDetail { reply } = f
        .gateway
        .window_command(BotCommand::ReadRoutineDetail {
            bot_id: bot.id,
            schedule_id: receipt.schedule_id,
            before_occurrence_ms: None,
            history_limit: 16,
        })
        .unwrap()
    else {
        panic!("detail")
    };
    assert!(matches!(
        reply.detail.live,
        RoutineDetailLive::Current {
            state: RoutineScheduleState::Active { .. },
            ..
        }
    ));
    assert_eq!(
        f.store.writes.load(Ordering::Relaxed),
        1,
        "only the original Bot role was written, never a Schedule or Run page"
    );
}
#[test]
fn native_run_now_has_a_stable_request_and_real_thread_history() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let BotResponse::RoutineCreated { receipt } = f.gateway.window_command(create(&bot)).unwrap()
    else {
        panic!()
    };
    let command = Command::Routine {
        id: Uuid::new_v4(),
        action: native::Change::RunNow {
            bot: bot.id.as_uuid(),
            routine: receipt.schedule_id.as_uuid(),
            revision: 1,
        },
    };
    let first = f
        .gateway
        .execute_owner(f.store.owner, command.clone())
        .unwrap();
    let again = f.gateway.execute_owner(f.store.owner, command).unwrap();
    assert_eq!(first["answer"], again["answer"]);
    assert_eq!(again["replayed"], true);
    let detail = f
        .gateway
        .execute_owner(
            f.store.owner,
            Command::Read {
                query: Query::Routine {
                    bot: bot.id.as_uuid(),
                    routine: receipt.schedule_id.as_uuid(),
                    before: None,
                    limit: 16,
                },
            },
        )
        .unwrap();
    assert_eq!(detail["routine"]["enabled"], true);
    assert_eq!(detail["occurrences"].as_array().unwrap().len(), 1);
    assert_eq!(detail["occurrences"][0]["state"], "queued");
    assert!(
        detail["routine"]["thread"]
            .as_str()
            .unwrap()
            .parse::<Uuid>()
            .is_ok()
    );
    let json = detail.to_string();
    assert!(!json.contains("run_id"));
    assert!(!json.contains("attempt_id"));
}
#[test]
fn routine_creation_checks_selected_pages_before_mutation() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    f.store.deleted.store(true, Ordering::Relaxed);
    assert!(f.gateway.window_command(create(&bot)).is_err());
    let BotResponse::Routines { page } = f
        .gateway
        .window_command(BotCommand::ReadRoutines {
            bot_id: bot.id,
            after_schedule: None,
            limit: 16,
        })
        .unwrap()
    else {
        panic!()
    };
    assert!(page.items.is_empty());
}
#[test]
fn enable_retries_use_one_definition_revision() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let BotResponse::RoutineCreated { receipt } = f.gateway.window_command(create(&bot)).unwrap()
    else {
        panic!()
    };
    let BotResponse::RoutineControl { control: paused } = f
        .gateway
        .window_command(BotCommand::SetRoutineLifecycle {
            bot_id: bot.id,
            schedule_id: receipt.schedule_id,
            expected_definition_revision: 1,
            action: BotRoutineLifecycleAction::Pause,
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(paused.definition_revision, 2);
    let command = BotCommand::SetRoutineLifecycle {
        bot_id: bot.id,
        schedule_id: receipt.schedule_id,
        expected_definition_revision: paused.definition_revision,
        action: BotRoutineLifecycleAction::Enable,
    };
    let BotResponse::RoutineControl { control } =
        f.gateway.window_command(command.clone()).unwrap()
    else {
        panic!()
    };
    let BotResponse::RoutineControl { control: again } = f.gateway.window_command(command).unwrap()
    else {
        panic!()
    };
    assert_eq!(control.definition_revision, 3);
    assert_eq!(again.definition_revision, 3);
    assert_eq!(
        again.outcome,
        koru_delegate_core::RoutineControlOutcome::AlreadyApplied
    );
}

#[test]
fn timing_retry_uses_original_intent_after_another_edit_changes_objective() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let BotResponse::RoutineCreated { receipt } = f.gateway.window_command(create(&bot)).unwrap()
    else {
        panic!()
    };
    let rule = RoutineRecurrenceRule::zoned("0 18 * * *".parse().unwrap(), "UTC").unwrap();
    let command = BotCommand::EditRoutine {
        bot_id: bot.id,
        schedule_id: receipt.schedule_id,
        expected_definition_revision: 1,
        expected_name: RoutineScheduleName::new("Daily briefing").unwrap(),
        name: RoutineScheduleName::new("Evening briefing").unwrap(),
        recurrence: rule.clone(),
    };
    let first = f.gateway.window_command(command.clone()).unwrap();
    f.gateway
        .execute_owner(
            f.store.owner,
            Command::Routine {
                id: Uuid::new_v4(),
                action: native::Change::Edit {
                    bot: bot.id.as_uuid(),
                    routine: receipt.schedule_id.as_uuid(),
                    revision: 2,
                    name: "Evening briefing".into(),
                    objective: "New task requested later".into(),
                    recurrence: rule,
                },
            },
        )
        .unwrap();
    let again = f.gateway.window_command(command).unwrap();
    let (
        BotResponse::RoutineControl { control: first },
        BotResponse::RoutineControl { control: again },
    ) = (first, again)
    else {
        panic!()
    };
    assert_eq!(first.definition_revision, again.definition_revision);
    assert_eq!(
        again.outcome,
        koru_delegate_core::RoutineControlOutcome::AlreadyApplied
    );
    let detail = f
        .gateway
        .execute_owner(
            f.store.owner,
            Command::Read {
                query: Query::Routine {
                    bot: bot.id.as_uuid(),
                    routine: receipt.schedule_id.as_uuid(),
                    before: None,
                    limit: 1,
                },
            },
        )
        .unwrap();
    assert_eq!(detail["routine"]["objective"], "New task requested later");
    assert_eq!(detail["routine"]["revision"], 3);
}
