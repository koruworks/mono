// SPDX-License-Identifier: MPL-2.0
//! Delegate IPC 17: the owner control and the owner Test of one Bot Routine.
use super::*;
use koru_delegate_schedule::{RecurrenceRule, ScheduleName};

fn route() -> koru_delegate_core::RoutineRouteIdentity {
    serde_json::from_value(serde_json::json!({
        "owner":{"user_scope":Uuid::new_v4(),"device":Uuid::new_v4()}, "bot":Uuid::new_v4(),
        "schedule":Uuid::new_v4(),"workspace":Uuid::new_v4(),"creation_request":Uuid::new_v4(),
        "seed_hash":"11".repeat(32),"created_at_ms":1
    }))
    .unwrap()
}
fn definition() -> koru_delegate_core::RoutineDefinitionIdentity {
    serde_json::from_value(
        serde_json::json!({"revision":2,"digest":"22".repeat(32),"owner_handoff":null}),
    )
    .unwrap()
}
fn edit(name: &str) -> RoutineControlAction {
    RoutineControlAction::Edit {
        expected_name: ScheduleName::new("Before").unwrap(),
        name: ScheduleName::new(name).unwrap(),
        recurrence: RecurrenceRule::floating("0 9 * * *".parse().unwrap()),
    }
}
fn control(action: RoutineControlAction) -> BotCommand {
    BotCommand::ControlRoutine {
        route: route(),
        expected_revision: 1,
        action,
    }
}
fn test(occurrence_ms: i64) -> BotCommand {
    BotCommand::TestRoutine {
        route: route(),
        expected_revision: 1,
        occurrence_ms,
    }
}
fn frame(command: &BotCommand) -> Vec<u8> {
    koru_ipc::frame::encode(
        crate::ipc::WIRE_VERSION,
        &crate::ipc::Request::Bot {
            authority: BotDelegateAuthority::new("a".repeat(48)).unwrap(),
            command: command.clone(),
        },
    )
    .unwrap()
}

#[test]
fn routine_control_commands_are_ipc17_strict_and_owner_bound_to_the_route() {
    assert_eq!(crate::ipc::WIRE_VERSION, 18);
    for (command, tag, action) in [
        (
            control(RoutineControlAction::Enable {}),
            "control_routine",
            Some("enable"),
        ),
        (
            control(RoutineControlAction::Pause {}),
            "control_routine",
            Some("pause"),
        ),
        (control(edit("After")), "control_routine", Some("edit")),
        (test(120_000), "test_routine", None),
    ] {
        let (BotCommand::ControlRoutine { route, .. } | BotCommand::TestRoutine { route, .. }) =
            &command
        else {
            unreachable!()
        };
        assert_eq!(command.owner(), Some(&route.owner));
        assert_eq!(command.routine_control_valid(), Some(true));
        let bytes = koru_ipc::frame::encode(crate::ipc::WIRE_VERSION, &command).unwrap();
        let (version, decoded): (u8, BotCommand) =
            koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!((version, &decoded), (crate::ipc::WIRE_VERSION, &command));
        let wire = serde_json::to_value(&command).unwrap();
        assert_eq!(wire["command"], tag);
        if let Some(action) = action {
            assert_eq!(wire["action"]["action"], action);
        }
        // Nothing an owner may not choose can be supplied, at any depth.
        for (pointer, extra) in [
            ("", "effective_at_ms"),
            ("", "template"),
            ("", "document"),
            ("", "owner"),
            ("/route", "association"),
            ("/route/owner", "authority_override"),
            ("/action", "lifecycle"),
            ("/action", "template"),
        ] {
            let mut value = wire.clone();
            let Some(target) = value.pointer_mut(pointer) else {
                continue;
            };
            target[extra] = serde_json::json!(true);
            assert!(
                serde_json::from_value::<BotCommand>(value).is_err(),
                "{tag} {pointer} {extra}"
            );
        }
    }
    // The action is a closed vocabulary: no Test, Retire or access here.
    for unknown in ["test", "retire", "resume", "update_access", "Enable", ""] {
        let mut wire = serde_json::to_value(control(RoutineControlAction::Enable {})).unwrap();
        wire["action"]["action"] = unknown.into();
        assert!(
            serde_json::from_value::<BotCommand>(wire).is_err(),
            "{unknown}"
        );
    }
    // An Edit carries both names and the recurrence, or it is not an Edit.
    for missing in ["expected_name", "name", "recurrence"] {
        let mut wire = serde_json::to_value(control(edit("After"))).unwrap();
        wire["action"].as_object_mut().unwrap().remove(missing);
        assert!(
            serde_json::from_value::<BotCommand>(wire).is_err(),
            "{missing}"
        );
    }
    let debug = format!("{:?}", edit("PRIVATE-ROUTINE-NAME"));
    assert!(!debug.contains("PRIVATE-ROUTINE-NAME"));
    assert!(!format!("{:?}", control(edit("PRIVATE-ROUTINE-NAME"))).contains("PRIVATE"));
}

#[test]
fn routine_control_requests_reject_revision_zero_partial_minutes_and_malformed_routes() {
    let BotCommand::ControlRoutine { route, action, .. } = control(RoutineControlAction::Pause {})
    else {
        unreachable!()
    };
    assert_eq!(
        BotCommand::ControlRoutine {
            route: route.clone(),
            expected_revision: 0,
            action: action.clone(),
        }
        .routine_control_valid(),
        Some(false)
    );
    assert_eq!(
        BotCommand::ControlRoutine {
            route: route.clone(),
            expected_revision: u32::MAX,
            action,
        }
        .routine_control_valid(),
        Some(true)
    );
    for partial in [1, 59_999, 60_001, -60_000] {
        assert_eq!(
            test(partial).routine_control_valid(),
            Some(false),
            "{partial}"
        );
    }
    assert_eq!(
        BotCommand::TestRoutine {
            route: route.clone(),
            expected_revision: 0,
            occurrence_ms: 60_000,
        }
        .routine_control_valid(),
        Some(false)
    );
    let mut nil = route;
    nil.creation_request = koru_delegate_core::BotRequestId::from_uuid(Uuid::nil());
    assert_eq!(
        BotCommand::TestRoutine {
            route: nil,
            expected_revision: 1,
            occurrence_ms: 60_000,
        }
        .routine_control_valid(),
        Some(false)
    );
    // Every other command is outside this check.
    assert_eq!(
        BotCommand::RegisterRoutineRoute {
            identity: self::route()
        }
        .routine_control_valid(),
        None
    );
}

#[test]
fn routine_control_replies_roundtrip_with_every_field_always_present() {
    let controlled = BotReply::RoutineControlled {
        route: route(),
        outcome: RoutineControlOutcome::AlreadyApplied,
        name: ScheduleName::new("Morning summary").unwrap(),
        definition: definition(),
        effective_at_ms: 61_000,
        lifecycle: RoutineControlLifecycle::Paused,
    };
    let BotCommand::TestRoutine { route, .. } = test(120_000) else {
        unreachable!()
    };
    let accepted = BotReply::RoutineTestAccepted {
        slot: koru_delegate_core::RoutineSlot {
            owner: route.owner,
            bot: route.bot,
            schedule: route.schedule,
            occurrence_ms: 120_000,
        },
        definition: definition(),
    };
    for (reply, tag, fields) in [
        (
            &controlled,
            "routine_controlled",
            &[
                "route",
                "outcome",
                "name",
                "definition",
                "effective_at_ms",
                "lifecycle",
            ][..],
        ),
        (
            &accepted,
            "routine_test_accepted",
            &["slot", "definition"][..],
        ),
    ] {
        assert_eq!(reply.routine_control_valid(), Some(true));
        let wire = serde_json::to_value(reply).unwrap();
        assert_eq!(wire["result"], tag);
        assert_eq!(
            &serde_json::from_value::<BotReply>(wire.clone()).unwrap(),
            reply
        );
        assert_eq!(wire.as_object().unwrap().len(), fields.len() + 1);
        for field in fields {
            let mut missing = wire.clone();
            missing.as_object_mut().unwrap().remove(*field);
            assert!(
                serde_json::from_value::<BotReply>(missing).is_err(),
                "{tag} {field}"
            );
        }
        let mut extra = wire.clone();
        extra["claimed"] = true.into();
        assert!(serde_json::from_value::<BotReply>(extra).is_err());
    }
    let wire = serde_json::to_value(&controlled).unwrap();
    assert_eq!(wire["outcome"], "already_applied");
    assert_eq!(wire["lifecycle"], "paused");
    assert_eq!(BotReply::Applied.routine_control_valid(), None);
    // A reply naming a partial minute is not a Test acceptance.
    let BotReply::RoutineTestAccepted {
        mut slot,
        definition,
    } = accepted
    else {
        unreachable!()
    };
    slot.occurrence_ms += 1;
    assert_eq!(
        BotReply::RoutineTestAccepted { slot, definition }.routine_control_valid(),
        Some(false)
    );
}

#[test]
fn routine_control_is_bounded_at_4_kib_while_encoding_and_before_decoding() {
    assert_eq!(ROUTINE_CONTROL_WIRE_LIMIT, 4096);
    // The largest name an Edit can carry, in both fields, still fits.
    let widest = "\u{10FFFF}".repeat(koru_delegate_schedule::MAX_SCHEDULE_NAME_CHARS);
    let largest = BotCommand::ControlRoutine {
        route: route(),
        expected_revision: u32::MAX,
        action: RoutineControlAction::Edit {
            expected_name: ScheduleName::new(widest.clone()).unwrap(),
            name: ScheduleName::new(widest.clone()).unwrap(),
            recurrence: RecurrenceRule::floating("0 9 * * *".parse().unwrap()),
        },
    };
    assert_eq!(largest.routine_control_valid(), Some(true));
    assert!(serde_json::to_vec(&largest).unwrap().len() <= ROUTINE_CONTROL_WIRE_LIMIT);
    assert!(!routine_control_frame_oversized(
        &frame(&largest)[koru_ipc::frame::HEADER_LEN..]
    ));
    let reply = BotReply::RoutineControlled {
        route: route(),
        outcome: RoutineControlOutcome::Applied,
        name: ScheduleName::new(widest).unwrap(),
        definition: definition(),
        effective_at_ms: i64::MAX,
        lifecycle: RoutineControlLifecycle::Active,
    };
    assert_eq!(reply.routine_control_valid(), Some(true));

    // An oversized body is recognised as a control by its tag alone, before
    // any of its content is decoded, whichever field carries the bulk.
    for tag in ["control_routine", "test_routine"] {
        for padded in ["/command", "/command/route", ""] {
            let mut wire = serde_json::to_value(crate::ipc::Request::Bot {
                authority: BotDelegateAuthority::new("a".repeat(48)).unwrap(),
                command: test(60_000),
            })
            .unwrap();
            wire["command"]["command"] = tag.into();
            wire.pointer_mut(padded).unwrap()["padding"] = "x".repeat(8 * 1024).into();
            let body = serde_json::to_vec(&wire).unwrap();
            assert!(body.len() > ROUTINE_CONTROL_FRAME_LIMIT);
            assert!(routine_control_frame_oversized(&body), "{tag} {padded}");
        }
    }
    // Other commands keep the unchanged frame bound, however large.
    let mut other = serde_json::to_value(crate::ipc::Request::Bot {
        authority: BotDelegateAuthority::new("a".repeat(48)).unwrap(),
        command: BotCommand::RegisterRoutineRoute { identity: route() },
    })
    .unwrap();
    other["command"]["padding"] = "x".repeat(8 * 1024).into();
    assert!(!routine_control_frame_oversized(
        &serde_json::to_vec(&other).unwrap()
    ));
    assert!(!routine_control_frame_oversized(b"not json"));
    assert!(!routine_control_frame_oversized(&vec![b'['; 8 * 1024]));
}

#[test]
fn routine_control_local_port_refuses_a_malformed_control_before_any_frame_is_written() {
    // `encodable` runs before the transport is touched by either call path.
    assert!(LocalBotDelegate::encodable(&test(60_000)).is_ok());
    assert!(LocalBotDelegate::encodable(&control(edit("After"))).is_ok());
    assert!(
        LocalBotDelegate::encodable(&BotCommand::RegisterRoutineRoute { identity: route() })
            .is_ok()
    );
    for command in [test(60_001), {
        let BotCommand::ControlRoutine { route, action, .. } =
            control(RoutineControlAction::Enable {})
        else {
            unreachable!()
        };
        BotCommand::ControlRoutine {
            route,
            expected_revision: 0,
            action,
        }
    }] {
        let failure = LocalBotDelegate::encodable(&command).unwrap_err();
        assert_eq!(failure.code, "invalid_request");
        assert!(!failure.retryable);
    }
}

#[test]
fn routine_control_local_port_rejects_an_out_of_bound_reply_and_an_ipc16_peer() {
    let BotCommand::TestRoutine { route, .. } = test(120_000) else {
        unreachable!()
    };
    let malformed = BotReply::RoutineTestAccepted {
        slot: koru_delegate_core::RoutineSlot {
            owner: route.owner,
            bot: route.bot,
            schedule: route.schedule,
            occurrence_ms: 120_001,
        },
        definition: definition(),
    };
    assert_eq!(
        LocalBotDelegate::answer(crate::ipc::Response::Bot(malformed))
            .unwrap_err()
            .code,
        "delegate_unexpected_response"
    );
    // A delegate still on IPC 16 that answers is incompatible, never
    // reinterpreted as an acceptance; one that drops the frame is unavailable.
    let old = koru_ipc::frame::encode(
        16,
        &serde_json::json!({"frame":"bot","result":"routine_test_accepted"}),
    )
    .unwrap();
    let failure = crate::local::read_current_response(&mut old.as_slice()).unwrap_err();
    assert_eq!(failure.code, "delegate_incompatible");
    assert!(!failure.retryable);
    assert_eq!(
        crate::local::read_current_response(&mut [].as_slice())
            .unwrap_err()
            .code,
        "delegate_unavailable"
    );
}
