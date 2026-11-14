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
        read_current_request(&mut frame::encode(15, &request).unwrap().as_slice()).unwrap(),
        Some(request.clone())
    );
    assert!(
        read_current_request(&mut frame::encode(14, &request).unwrap().as_slice())
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
        for stale in [1, 3, 4, 6, 7, 10, 13, 14] {
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
    let stale = frame::encode(14, &request).unwrap();
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
            read_current_request(&mut frame::encode(14, &request).unwrap().as_slice())
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
