// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn ipc15_claim_read_and_link_ack_are_closed_owner_bound_commands() {
    let slot: koru_delegate_core::RoutineSlot = serde_json::from_value(serde_json::json!({
        "owner":{"user_scope":Uuid::new_v4(),"device":Uuid::new_v4()},
        "bot":Uuid::new_v4(),"schedule":Uuid::new_v4(),"occurrence_ms":60000
    }))
    .unwrap();
    let acknowledgement=serde_json::from_value(serde_json::json!({
        "claim":{"slot":slot,"definition":{"revision":1,"digest":"11".repeat(32),"owner_handoff":null},
        "ids":{"submission":Uuid::new_v4(),"run":Uuid::new_v4(),"chat":Uuid::new_v4(),"attempt":Uuid::new_v4(),"user_message":Uuid::new_v4(),"assistant_message":Uuid::new_v4()},
        "preparation_hash":"22".repeat(32),"claimed_at_ms":60001},"outcome":{"state":"linked"}
    })).unwrap();
    for (command, nested) in [
        (BotCommand::ReadRoutineClaim { slot }, "slot"),
        (
            BotCommand::AcknowledgeRoutineLink { acknowledgement },
            "acknowledgement",
        ),
    ] {
        assert_eq!(command.owner(), Some(&slot.owner));
        let bytes = koru_ipc::frame::encode(crate::ipc::WIRE_VERSION, &command).unwrap();
        let (_, decoded): (u8, BotCommand) =
            koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded, command);
        let mut extra = serde_json::to_value(&command).unwrap();
        extra[nested]["authority_override"] = true.into();
        assert!(serde_json::from_value::<BotCommand>(extra).is_err());
        let mut extra = serde_json::to_value(&command).unwrap();
        extra["force"] = true.into();
        assert!(serde_json::from_value::<BotCommand>(extra).is_err());
    }
    let reply = BotReply::RoutineClaim {
        observation: koru_delegate_core::RoutineClaimObservation::Absent {
            slot,
            observed_at_ms: 60002,
        },
    };
    assert_eq!(
        serde_json::from_value::<BotReply>(serde_json::to_value(&reply).unwrap()).unwrap(),
        reply
    );
}

#[test]
fn ipc15_routine_route_is_closed_owner_bound_and_rejects_ipc14_reply() {
    let identity: koru_delegate_core::RoutineRouteIdentity =
        serde_json::from_value(serde_json::json!({
            "owner":{"user_scope":Uuid::new_v4(),"device":Uuid::new_v4()}, "bot":Uuid::new_v4(),
            "schedule":Uuid::new_v4(),"workspace":Uuid::new_v4(),"creation_request":Uuid::new_v4(),
            "seed_hash":"11".repeat(32),"created_at_ms":1
        }))
        .unwrap();
    let command = BotCommand::RegisterRoutineRoute {
        identity: identity.clone(),
    };
    assert_eq!(command.owner(), Some(&identity.owner));
    assert_eq!(crate::ipc::WIRE_VERSION, 19);
    let bytes = koru_ipc::frame::encode(crate::ipc::WIRE_VERSION, &command).unwrap();
    let (_, decoded): (u8, BotCommand) =
        koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
    assert_eq!(decoded, command);
    for nested in [false, true] {
        let mut value = serde_json::to_value(&command).unwrap();
        if nested {
            value["identity"]["grant_execution"] = true.into();
        } else {
            value["force"] = true.into();
        }
        assert!(serde_json::from_value::<BotCommand>(value).is_err());
    }
    let reply = BotReply::RoutineRoute { identity };
    assert_eq!(
        serde_json::from_value::<BotReply>(serde_json::to_value(&reply).unwrap()).unwrap(),
        reply
    );
    let bytes = koru_ipc::frame::encode(15, &serde_json::json!({"old":true})).unwrap();
    assert_eq!(
        crate::local::read_current_response(&mut bytes.as_slice())
            .unwrap_err()
            .code,
        "delegate_incompatible"
    );
}
