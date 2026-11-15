// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn ipc15_routine_policy_sync_is_strict_owner_bound_and_rejects_ipc14() {
    let workspace = Uuid::new_v4();
    let barrier: RoutinePolicyBarrier = serde_json::from_value(serde_json::json!({
        "owner":{"user_scope":Uuid::new_v4(),"device":Uuid::new_v4()},
        "bot":Uuid::new_v4(),"schedule":Uuid::new_v4(),"request":Uuid::new_v4(),
        "grant_revision":1,"effects_allowed":true,
        "policy":{"workspace":workspace,"destination":workspace,"access":"default","selected_pages":[]}
    })).unwrap();
    let command = BotCommand::SynchronizeRoutinePolicy {
        barrier: barrier.clone(),
    };
    assert_eq!(command.owner(), Some(&barrier.owner));
    assert_eq!(crate::ipc::WIRE_VERSION, 16);
    let bytes = koru_ipc::frame::encode(16, &command).unwrap();
    let (version, decoded): (u8, BotCommand) =
        koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
    assert_eq!(version, 16);
    assert_eq!(decoded, command);
    for field in [
        "owner",
        "bot",
        "schedule",
        "request",
        "grant_revision",
        "policy",
        "effects_allowed",
    ] {
        let mut value = serde_json::to_value(&command).unwrap();
        value["barrier"].as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<BotCommand>(value).is_err(),
            "{field}"
        );
    }
    for nested in [false, true] {
        let mut value = serde_json::to_value(&command).unwrap();
        if nested {
            value["barrier"]["global_pages"] = true.into();
        } else {
            value["caller_authority"] = true.into();
        }
        assert!(serde_json::from_value::<BotCommand>(value).is_err());
    }
    for state in [
        BotPolicyBarrierState::Pending,
        BotPolicyBarrierState::Effective,
    ] {
        let reply = BotReply::RoutinePolicyBarrier {
            barrier: barrier.clone(),
            state,
        };
        let mut value = serde_json::to_value(&reply).unwrap();
        assert_eq!(
            serde_json::from_value::<BotReply>(value.clone()).unwrap(),
            reply
        );
        value["execute"] = true.into();
        assert!(serde_json::from_value::<BotReply>(value).is_err());
    }
    let bytes = koru_ipc::frame::encode(15, &serde_json::json!({"old":true})).unwrap();
    assert_eq!(
        crate::local::read_current_response(&mut bytes.as_slice())
            .unwrap_err()
            .code,
        "delegate_incompatible"
    );
}
