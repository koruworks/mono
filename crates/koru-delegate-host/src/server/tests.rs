// SPDX-License-Identifier: MPL-2.0

//! The stale-worker guard rejects frames before any actor command exists.
use super::*;
use koru_delegate_api::ipc::Caller;

#[test]
fn admission_read_server_requires_private_capability_device_and_current_frame() {
    use koru_delegate_api::bots::{BotCommand, BotDelegateAuthority};
    let capability =
        BotDelegateAuthority::new("admission-read-private-capability-01234567890123456789".into())
            .unwrap();
    let device = koru_delegate_core::DeviceId::new();
    let mut identity = Identity {
        launch_id: "fixture".into(),
        authority: "ordinary-stop".into(),
        device_id: device.to_storage(),
        bot_authority: Some(capability.clone()),
        run_authorities: Default::default(),
    };
    let owner = serde_json::from_value(
        serde_json::json!({"user_scope":uuid::Uuid::new_v4(),"device":device}),
    )
    .unwrap();
    let command = BotCommand::ReadAdmissionBarrier {
        owner,
        bot: koru_delegate_core::BotId::mint(),
    };
    assert!(admits_bot_command(&identity, &capability, &command));
    let other =
        BotDelegateAuthority::new("ordinary-scope-label-01234567890123456789".into()).unwrap();
    assert!(!admits_bot_command(&identity, &other, &command));
    identity.device_id = koru_delegate_core::DeviceId::new().to_storage();
    assert!(!admits_bot_command(&identity, &capability, &command));
    identity.device_id = uuid::Uuid::nil().to_string();
    assert!(!admits_bot_command(&identity, &capability, &command));
    identity.device_id = device.to_storage();
    identity.bot_authority = None;
    assert!(!admits_bot_command(&identity, &capability, &command));
    let request = Request::Bot {
        authority: capability,
        command,
    };
    assert_eq!(
        read_current_request(&mut frame::encode(WIRE_VERSION, &request).unwrap().as_slice())
            .unwrap(),
        Some(request.clone())
    );
    assert!(
        read_current_request(&mut frame::encode(15, &request).unwrap().as_slice())
            .unwrap()
            .is_none()
    );
}

#[test]
fn stale_wire_versions_cannot_reach_dispatch() {
    let caller = Caller {
        product: "test".into(),
        trace_id: "test".into(),
    };
    let run = koru_delegate_core::RunId::mint();
    let request: koru_delegate_api::CreateRunRequest = serde_json::from_value(serde_json::json!({
        "run": run.to_storage(),
        "workspace": koru_delegate_core::WorkspaceId::from_uuid(uuid::Uuid::new_v4()).to_storage(),
        "requested_by": {"kind": "person", "profile": uuid::Uuid::new_v4().to_string()},
        "objective": "test",
        "agent": {"kind": "built_in", "value": "opencode"},
        "scope": {"kind": "temporary"},
        "requested_model": "deepseek/deepseek-chat"
    }))
    .unwrap();
    for request in [
        Request::Create {
            caller: caller.clone(),
            request,
        },
        Request::Resume {
            caller,
            run,
            confirm: false,
        },
    ] {
        for stale in [1, 3, 4, 6, 7, 10, 13, 14, 15] {
            let legacy = frame::encode(stale, &request).unwrap();
            assert!(
                read_current_request(&mut legacy.as_slice())
                    .unwrap()
                    .is_none()
            );
            let invalid_legacy = frame::encode(stale, &serde_json::json!({"old": true})).unwrap();
            assert!(
                read_current_request(&mut invalid_legacy.as_slice())
                    .unwrap()
                    .is_none(),
                "a stale body is rejected by version before current-shape decoding"
            );
        }
        let current = frame::encode(WIRE_VERSION, &request).unwrap();
        assert_eq!(
            read_current_request(&mut current.as_slice()).unwrap(),
            Some(request)
        );
    }
}

#[test]
fn bot_admission_requires_separate_capability_and_validated_device() {
    use koru_delegate_api::bots::BotDelegateAuthority;
    let capability =
        BotDelegateAuthority::new("private-bot-capability-01234567890123456789".into()).unwrap();
    let mut identity = Identity {
        launch_id: "test".into(),
        authority: "private-stop-authority-01234567890123456789".into(),
        device_id: uuid::Uuid::new_v4().to_string(),
        bot_authority: Some(capability.clone()),
        run_authorities: Default::default(),
    };
    assert!(admits_bot(&identity, &capability));
    assert!(!admits_bot(
        &identity,
        &BotDelegateAuthority::new(identity.authority.clone()).unwrap()
    ));
    identity.device_id = uuid::Uuid::nil().to_string();
    assert!(!admits_bot(&identity, &capability));
    identity.device_id = "invalid".into();
    assert!(!admits_bot(&identity, &capability));
    identity.device_id = uuid::Uuid::new_v4().to_string();
    identity.bot_authority = None;
    assert!(!admits_bot(&identity, &capability));
}

#[test]
fn caller_owner_must_match_the_agents_validated_device() {
    use koru_delegate_api::bots::{BotCommand, BotDelegateAuthority};
    let capability =
        BotDelegateAuthority::new("private-bot-capability-01234567890123456789".into()).unwrap();
    let device = koru_delegate_core::DeviceId::new();
    let identity = Identity {
        launch_id: "test".into(),
        authority: "different".into(),
        device_id: device.to_storage(),
        bot_authority: Some(capability.clone()),
        run_authorities: Default::default(),
    };
    let mut value = serde_json::json!({
        "command":"reconcile",
        "key":{"owner":{"user_scope":uuid::Uuid::new_v4(),"device":device.as_uuid()},
            "bot":uuid::Uuid::new_v4(),"request":uuid::Uuid::new_v4()}
    });
    let command: BotCommand = serde_json::from_value(value.clone()).unwrap();
    assert!(admits_bot_command(&identity, &capability, &command));
    value["key"]["owner"]["device"] = serde_json::json!(uuid::Uuid::new_v4());
    let command: BotCommand = serde_json::from_value(value).unwrap();
    assert!(!admits_bot_command(&identity, &capability, &command));
}

#[test]
fn routine_route_frame_requires_current_protocol_private_capability_and_device() {
    use koru_delegate_api::bots::{BotCommand, BotDelegateAuthority};
    let capability =
        BotDelegateAuthority::new("routine-private-authority-01234567890123456789".into()).unwrap();
    let device = koru_delegate_core::DeviceId::new();
    let identity = Identity {
        launch_id: "fixture".into(),
        authority: "stop-only".into(),
        device_id: device.to_storage(),
        bot_authority: Some(capability.clone()),
        run_authorities: Default::default(),
    };
    let route: koru_delegate_core::RoutineRouteIdentity = serde_json::from_value(serde_json::json!({
        "owner":{"user_scope":uuid::Uuid::new_v4(),"device":device},"bot":uuid::Uuid::new_v4(),
        "schedule":uuid::Uuid::new_v4(),"workspace":uuid::Uuid::new_v4(),"creation_request":uuid::Uuid::new_v4(),"seed_hash":"11".repeat(32),"created_at_ms":1
    })).unwrap();
    let command = BotCommand::RegisterRoutineRoute {
        identity: route.clone(),
    };
    let request = Request::Bot {
        authority: capability.clone(),
        command: command.clone(),
    };
    let stale = frame::encode(15, &request).unwrap();
    assert!(
        read_current_request(&mut stale.as_slice())
            .unwrap()
            .is_none()
    );
    let current = frame::encode(WIRE_VERSION, &request).unwrap();
    assert_eq!(
        read_current_request(&mut current.as_slice()).unwrap(),
        Some(request)
    );
    assert!(admits_bot_command(&identity, &capability, &command));
    let ordinary =
        BotDelegateAuthority::new("ordinary-caller-label-01234567890123456789".into()).unwrap();
    assert!(!admits_bot_command(&identity, &ordinary, &command));
    let slot = koru_delegate_core::RoutineSlot {
        owner: route.owner,
        bot: route.bot,
        schedule: route.schedule,
        occurrence_ms: 60000,
    };
    let acknowledgement:koru_delegate_core::RoutineLinkAcknowledgement=serde_json::from_value(serde_json::json!({
        "claim":{"slot":slot,"definition":{"revision":1,"digest":"11".repeat(32),"owner_handoff":null},
        "ids":{"submission":uuid::Uuid::new_v4(),"run":uuid::Uuid::new_v4(),"chat":uuid::Uuid::new_v4(),"attempt":uuid::Uuid::new_v4(),"user_message":uuid::Uuid::new_v4(),"assistant_message":uuid::Uuid::new_v4()},
        "preparation_hash":"22".repeat(32),"claimed_at_ms":60001},"outcome":{"state":"linked"}
    })).unwrap();
    for command in [
        BotCommand::ReadRoutineClaim { slot },
        BotCommand::AcknowledgeRoutineLink { acknowledgement },
    ] {
        assert!(admits_bot_command(&identity, &capability, &command));
        assert!(!admits_bot_command(&identity, &ordinary, &command));
        let request = Request::Bot {
            authority: capability.clone(),
            command: command.clone(),
        };
        assert!(
            read_current_request(&mut frame::encode(15, &request).unwrap().as_slice())
                .unwrap()
                .is_none()
        );
        assert_eq!(
            read_current_request(&mut frame::encode(WIRE_VERSION, &request).unwrap().as_slice())
                .unwrap(),
            Some(request)
        );
        let mut foreign = command;
        match &mut foreign {
            BotCommand::ReadRoutineClaim { slot } => {
                slot.owner.device = koru_delegate_core::DeviceId::new()
            }
            BotCommand::AcknowledgeRoutineLink { acknowledgement } => {
                acknowledgement.claim.slot.owner.device = koru_delegate_core::DeviceId::new()
            }
            _ => unreachable!(),
        }
        assert!(!admits_bot_command(&identity, &capability, &foreign));
    }
    let mut foreign = route;
    foreign.owner.device = koru_delegate_core::DeviceId::new();
    assert!(!admits_bot_command(
        &identity,
        &capability,
        &BotCommand::RegisterRoutineRoute { identity: foreign }
    ));
}

#[test]
fn decision_commands_need_the_private_capability_device_and_the_current_frame() {
    use koru_delegate_api::bots::{BotCommand, BotDelegateAuthority};
    let capability =
        BotDelegateAuthority::new("decision-read-private-capability-0123456789012345".into())
            .unwrap();
    let device = koru_delegate_core::DeviceId::new();
    let identity = Identity {
        launch_id: "fixture".into(),
        authority: "ordinary-stop".into(),
        device_id: device.to_storage(),
        bot_authority: Some(capability.clone()),
        run_authorities: Default::default(),
    };
    let owner: koru_delegate_core::BotOwner = serde_json::from_value(
        serde_json::json!({"user_scope":uuid::Uuid::new_v4(),"device":device}),
    )
    .unwrap();
    let command = BotCommand::ReadBotDecisionWait {
        owner,
        bot: koru_delegate_core::BotId::mint(),
        run: koru_delegate_core::RunId::mint(),
        wait_id: koru_delegate_core::decisions::BotWaitId::mint(),
        decision_id: koru_delegate_core::decisions::DecisionId::mint(),
    };
    assert!(admits_bot_command(&identity, &capability, &command));
    // A decision-bridge credential is not the Bot channel capability.
    let other =
        BotDelegateAuthority::new("ordinary-scope-label-01234567890123456789".into()).unwrap();
    assert!(!admits_bot_command(&identity, &other, &command));
    let mut foreign = command.clone();
    if let BotCommand::ReadBotDecisionWait { owner, .. } = &mut foreign {
        owner.device = koru_delegate_core::DeviceId::new();
    }
    assert!(!admits_bot_command(&identity, &capability, &foreign));
    let request = Request::Bot {
        authority: capability,
        command,
    };
    // An older handshake is refused before any decision body is decoded.
    for older in 1..WIRE_VERSION {
        assert!(
            read_current_request(&mut frame::encode(older, &request).unwrap().as_slice())
                .unwrap()
                .is_none(),
            "{older}"
        );
    }
    assert_eq!(
        read_current_request(&mut frame::encode(WIRE_VERSION, &request).unwrap().as_slice())
            .unwrap(),
        Some(request)
    );
}

/// IPC16. A version-15 frame, for an ordinary command or a decision command,
/// is refused by its version before its body is decoded (an undecodable body
/// is refused the same way, never surfaced as a decode error). `handle` then
/// returns without writing: the connection closes unanswered, so an IPC15
/// peer sees `delegate_unavailable` (retryable), not `delegate_incompatible`.
/// The client half of that is pinned in the delegate API's own tests.
#[test]
fn ipc15_frames_are_refused_before_decoding_and_dropped_unanswered_so_the_peer_sees_delegate_unavailable()
 {
    use koru_delegate_api::bots::{BotCommand, BotDelegateAuthority};
    assert_eq!(WIRE_VERSION, 20);
    let previous = WIRE_VERSION - 1;
    let owner: koru_delegate_core::BotOwner = serde_json::from_value(
        serde_json::json!({"user_scope":uuid::Uuid::new_v4(),"device":uuid::Uuid::new_v4()}),
    )
    .unwrap();
    let ordinary = Request::Resume {
        caller: Caller {
            product: "test".into(),
            trace_id: "test".into(),
        },
        run: koru_delegate_core::RunId::mint(),
        confirm: false,
    };
    let decision = Request::Bot {
        authority: BotDelegateAuthority::new(
            "decision-ipc15-private-capability-0123456789012345".into(),
        )
        .unwrap(),
        command: BotCommand::ReadBotDecisionWait {
            owner,
            bot: koru_delegate_core::BotId::mint(),
            run: koru_delegate_core::RunId::mint(),
            wait_id: koru_delegate_core::decisions::BotWaitId::mint(),
            decision_id: koru_delegate_core::decisions::DecisionId::mint(),
        },
    };
    for request in [ordinary, decision] {
        let old = frame::encode(previous, &request).unwrap();
        assert!(
            read_current_request(&mut old.as_slice()).unwrap().is_none(),
            "an IPC15 frame never becomes a command"
        );
        let undecodable = frame::encode(previous, &serde_json::json!({"old": true})).unwrap();
        assert!(
            read_current_request(&mut undecodable.as_slice())
                .unwrap()
                .is_none(),
            "the version is checked before any body decoding"
        );
        let current = frame::encode(WIRE_VERSION, &request).unwrap();
        assert_eq!(
            read_current_request(&mut current.as_slice()).unwrap(),
            Some(request)
        );
    }
}

fn routine_control_route(
    device: koru_delegate_core::DeviceId,
) -> koru_delegate_core::RoutineRouteIdentity {
    serde_json::from_value(serde_json::json!({
        "owner":{"user_scope":uuid::Uuid::new_v4(),"device":device},
        "bot":uuid::Uuid::new_v4(),"schedule":uuid::Uuid::new_v4(),
        "workspace":uuid::Uuid::new_v4(),"creation_request":uuid::Uuid::new_v4(),
        "seed_hash":"11".repeat(32),"created_at_ms":1
    }))
    .unwrap()
}

/// IPC 17. The owner control and the owner Test need the private Bot
/// capability and this device, like every Bot command; a peer still on IPC 16
/// is refused by its version before the body is decoded and is dropped
/// unanswered, so it never reaches the scheduler or the actor.
#[test]
fn routine_control_frames_need_the_private_capability_this_device_and_ipc17() {
    use koru_delegate_api::bots::{BotCommand, BotDelegateAuthority, RoutineControlAction};
    assert_eq!(WIRE_VERSION, 20);
    let capability =
        BotDelegateAuthority::new("routine-control-private-capability-0123456789012345".into())
            .unwrap();
    let device = koru_delegate_core::DeviceId::new();
    let mut identity = Identity {
        launch_id: "fixture".into(),
        authority: "ordinary-stop".into(),
        device_id: device.to_storage(),
        bot_authority: Some(capability.clone()),
        run_authorities: Default::default(),
    };
    let route = routine_control_route(device);
    for command in [
        BotCommand::ControlRoutine {
            route: route.clone(),
            expected_revision: 1,
            action: RoutineControlAction::Enable {},
        },
        BotCommand::ControlRoutine {
            route: route.clone(),
            expected_revision: 1,
            action: RoutineControlAction::Pause {},
        },
        BotCommand::TestRoutine {
            route: route.clone(),
            expected_revision: 1,
            occurrence_ms: 60_000,
        },
    ] {
        assert!(admits_bot_command(&identity, &capability, &command));
        let other =
            BotDelegateAuthority::new("ordinary-scope-label-01234567890123456789".into()).unwrap();
        assert!(!admits_bot_command(&identity, &other, &command));
        // A route owned by another device is not this worker's to control.
        identity.device_id = koru_delegate_core::DeviceId::new().to_storage();
        assert!(!admits_bot_command(&identity, &capability, &command));
        identity.device_id = device.to_storage();
        let request = Request::Bot {
            authority: capability.clone(),
            command,
        };
        for older in 1..WIRE_VERSION {
            assert!(
                read_current_request(&mut frame::encode(older, &request).unwrap().as_slice())
                    .unwrap()
                    .is_none(),
                "an IPC {older} frame never becomes a Routine control"
            );
        }
        assert_eq!(
            read_current_request(&mut frame::encode(WIRE_VERSION, &request).unwrap().as_slice())
                .unwrap(),
            Some(request)
        );
    }
    // What an IPC 16 worker would have been sent does not decode there
    // either: the command names did not exist, so nothing is reinterpreted.
    let unknown_to_16 = frame::encode(
        16,
        &serde_json::json!({"frame":"bot","authority":"x".repeat(48),
            "command":{"command":"control_routine"}}),
    )
    .unwrap();
    assert!(
        read_current_request(&mut unknown_to_16.as_slice())
            .unwrap()
            .is_none()
    );
}

#[test]
fn routine_control_oversized_frame_is_dropped_before_its_content_is_decoded() {
    use koru_delegate_api::bots::{BotCommand, BotDelegateAuthority, ROUTINE_CONTROL_FRAME_LIMIT};
    let route = routine_control_route(koru_delegate_core::DeviceId::new());
    let request = Request::Bot {
        authority: BotDelegateAuthority::new("a".repeat(48)).unwrap(),
        command: BotCommand::TestRoutine {
            route,
            expected_revision: 1,
            occurrence_ms: 60_000,
        },
    };
    let mut padded = serde_json::to_value(&request).unwrap();
    padded["command"]["route"]["padding"] = "x".repeat(ROUTINE_CONTROL_FRAME_LIMIT).into();
    let oversized = frame::encode(WIRE_VERSION, &padded).unwrap();
    // Dropped unanswered, exactly like a frame that does not decode: the peer
    // learns nothing, and no command exists.
    assert!(
        read_current_request(&mut oversized.as_slice())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        read_current_request(&mut frame::encode(WIRE_VERSION, &request).unwrap().as_slice())
            .unwrap(),
        Some(request)
    );
}

/// Routine controls go to the scheduler thread, which owns the Schedule
/// document write. They never become an actor command.
#[test]
fn routine_control_is_served_by_the_scheduler_and_never_reaches_the_actor() {
    use koru_delegate_api::bots::{BotCommand, RoutineControlAction};
    let (actor, actor_inbox) = mpsc::channel();
    // This scheduler stand-in drops every command but Stop, so a routed
    // control answers `delegate_unavailable` at once instead of waiting.
    let scheduler = crate::scheduler::test_control(actor);
    let route = routine_control_route(koru_delegate_core::DeviceId::new());
    for command in [
        BotCommand::ControlRoutine {
            route: route.clone(),
            expected_revision: 1,
            action: RoutineControlAction::Enable {},
        },
        BotCommand::TestRoutine {
            route: route.clone(),
            expected_revision: 1,
            occurrence_ms: 60_000,
        },
    ] {
        let Ok(Response::Failure(failure)) = serve_routine_control(&scheduler, command) else {
            panic!("served by the scheduler")
        };
        assert_eq!(failure.code, "delegate_unavailable");
        assert!(failure.retryable);
    }
    // Malformed controls are answered here and are not sent to anyone.
    for malformed in [
        BotCommand::ControlRoutine {
            route: route.clone(),
            expected_revision: 0,
            action: RoutineControlAction::Pause {},
        },
        BotCommand::TestRoutine {
            route: route.clone(),
            expected_revision: 1,
            occurrence_ms: 60_001,
        },
    ] {
        let Ok(Response::Failure(failure)) = serve_routine_control(&scheduler, malformed) else {
            panic!("refused at the server")
        };
        assert_eq!(failure.code, "invalid_request");
        assert!(!failure.retryable);
    }
    // Every other Bot command is handed back for the actor, unchanged.
    let other = BotCommand::RegisterRoutineRoute {
        identity: route.clone(),
    };
    assert_eq!(
        serve_routine_control(&scheduler, other.clone()).unwrap_err(),
        other
    );
    assert!(matches!(
        actor_inbox.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    scheduler.stop();
}

/// IPC 18. A private command needs the private Bot capability, and the
/// commands that assert an owner need this device. A peer still on IPC 17 is
/// refused by its version before its body is decoded and is dropped
/// unanswered (it sees `delegate_unavailable`), so no private command ever
/// reaches the actor from it. A bounded private command over its 16 KiB is
/// dropped the same way, before its content is decoded.
#[test]
fn m5_ipc18_frame_version_an_ipc17_peer_is_refused_cleanly_and_an_oversized_private_frame_is_dropped()
 {
    use koru_delegate_api::bots::{
        BotCommand, BotDelegateAuthority, PRIVATE_FRAME_LIMIT, PrivateDispatcherId,
        PrivateEffectKey, PrivateEffectTransition,
    };
    assert_eq!(WIRE_VERSION, 20);
    let capability =
        BotDelegateAuthority::new("private-operation-capability-01234567890123456".into()).unwrap();
    let device = koru_delegate_core::DeviceId::new();
    let mut identity = Identity {
        launch_id: "fixture".into(),
        authority: "ordinary-stop".into(),
        device_id: device.to_storage(),
        bot_authority: Some(capability.clone()),
        run_authorities: Default::default(),
    };
    let owner: koru_delegate_core::BotOwner = serde_json::from_value(
        serde_json::json!({"user_scope":uuid::Uuid::new_v4(),"device":device}),
    )
    .unwrap();
    let dispatcher = PrivateDispatcherId::mint();
    let run = koru_delegate_core::RunId::mint();
    let commands = [
        BotCommand::AnnouncePrivateDispatcher { owner, dispatcher },
        BotCommand::AdvancePrivateEffect {
            key: PrivateEffectKey {
                run,
                operation_id: uuid::Uuid::new_v4(),
                attempt: koru_delegate_core::RunAttemptId::mint(),
                generation: koru_delegate_core::AttemptGeneration::FIRST,
                turn: koru_delegate_core::AgentTurnId::mint(),
            },
            dispatcher,
            transition: PrivateEffectTransition::Unknown {},
        },
        BotCommand::ReadPrivateEffect {
            run,
            operation_id: uuid::Uuid::new_v4(),
            dispatcher,
        },
    ];
    for command in commands {
        assert!(admits_bot_command(&identity, &capability, &command));
        let other =
            BotDelegateAuthority::new("ordinary-scope-label-01234567890123456789".into()).unwrap();
        assert!(!admits_bot_command(&identity, &other, &command));
        if command.owner().is_some() {
            // An owner on another device is not this worker's.
            identity.device_id = koru_delegate_core::DeviceId::new().to_storage();
            assert!(!admits_bot_command(&identity, &capability, &command));
            identity.device_id = device.to_storage();
        }
        let request = Request::Bot {
            authority: capability.clone(),
            command,
        };
        for older in 1..WIRE_VERSION {
            assert!(
                read_current_request(&mut frame::encode(older, &request).unwrap().as_slice())
                    .unwrap()
                    .is_none(),
                "an IPC {older} frame never becomes a private command"
            );
        }
        // The version is checked before any body decoding.
        let undecodable = frame::encode(17, &serde_json::json!({"old": true})).unwrap();
        assert!(
            read_current_request(&mut undecodable.as_slice())
                .unwrap()
                .is_none()
        );
        // Over its bound: dropped unanswered, and no command exists.
        let mut padded = serde_json::to_value(&request).unwrap();
        padded["command"]["padding"] = "x".repeat(PRIVATE_FRAME_LIMIT).into();
        let oversized = frame::encode(WIRE_VERSION, &padded).unwrap();
        assert!(
            read_current_request(&mut oversized.as_slice())
                .unwrap()
                .is_none()
        );
        assert_eq!(
            read_current_request(&mut frame::encode(WIRE_VERSION, &request).unwrap().as_slice())
                .unwrap(),
            Some(request)
        );
    }
    // What an IPC 17 worker would have been sent does not decode there
    // either: the command names did not exist, so nothing is reinterpreted.
    let unknown_to_17 = frame::encode(
        17,
        &serde_json::json!({"frame":"bot","authority":"x".repeat(48),
            "command":{"command":"announce_private_dispatcher"}}),
    )
    .unwrap();
    assert!(
        read_current_request(&mut unknown_to_17.as_slice())
            .unwrap()
            .is_none()
    );
}

/// IPC 20. The team barrier needs the private Bot capability and this
/// device, like every Bot command. A peer still on IPC 19 is refused by its
/// version before the body is decoded and is dropped unanswered, so it never
/// reaches the actor: it sees `delegate_unavailable`, and nothing of a Bot
/// starts through it.
#[test]
fn m8_ipc_team_frame_version_an_ipc19_peer_is_dropped_before_the_team_barrier_is_decoded() {
    use koru_delegate_api::bots::{BotCommand, BotDelegateAuthority};
    use koru_delegate_core::{BotOwner, BotRequestId, BotTeamBarrier};
    assert_eq!(WIRE_VERSION, 20);
    let capability =
        BotDelegateAuthority::new("team-barrier-private-capability-01234567890123456789".into())
            .unwrap();
    let device = koru_delegate_core::DeviceId::new();
    let mut identity = Identity {
        launch_id: "fixture".into(),
        authority: "ordinary-stop".into(),
        device_id: device.to_storage(),
        bot_authority: Some(capability.clone()),
        run_authorities: Default::default(),
    };
    let owner: BotOwner = serde_json::from_value(
        serde_json::json!({"user_scope":uuid::Uuid::new_v4(),"device":device}),
    )
    .unwrap();
    for barrier in [
        BotTeamBarrier::never_paused(owner),
        BotTeamBarrier {
            owner,
            epoch: 1,
            paused: true,
            request: Some(BotRequestId::mint()),
        },
    ] {
        let command = BotCommand::TeamBarrier { barrier };
        assert!(admits_bot_command(&identity, &capability, &command));
        let other =
            BotDelegateAuthority::new("ordinary-scope-label-01234567890123456789".into()).unwrap();
        assert!(!admits_bot_command(&identity, &other, &command));
        // Another device's team is not this worker's to hold.
        identity.device_id = koru_delegate_core::DeviceId::new().to_storage();
        assert!(!admits_bot_command(&identity, &capability, &command));
        identity.device_id = device.to_storage();
        let request = Request::Bot {
            authority: capability.clone(),
            command,
        };
        for older in 1..WIRE_VERSION {
            assert!(
                read_current_request(&mut frame::encode(older, &request).unwrap().as_slice())
                    .unwrap()
                    .is_none(),
                "an IPC {older} frame never becomes a team barrier"
            );
        }
        // The same for a body an IPC 19 peer could have written: refused by
        // its stamp, never decoded.
        let from_19 = frame::encode(19, &serde_json::json!({"old": true})).unwrap();
        assert!(
            read_current_request(&mut from_19.as_slice())
                .unwrap()
                .is_none()
        );
        assert_eq!(
            read_current_request(&mut frame::encode(WIRE_VERSION, &request).unwrap().as_slice())
                .unwrap(),
            Some(request)
        );
    }
}
