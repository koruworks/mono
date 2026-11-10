// SPDX-License-Identifier: MPL-2.0

//! The stale-worker guard rejects frames before any actor command exists.
use super::*;
use koru_delegate_api::ipc::Caller;

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
        for stale in [1, 3, 4, 6, 7] {
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
