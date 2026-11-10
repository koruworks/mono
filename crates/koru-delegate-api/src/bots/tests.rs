// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::{AttemptGeneration, DocumentId, RunAttemptId, RunId};
use uuid::Uuid;

#[test]
fn automatic_interrupt_has_closed_private_ipc_eleven_shape_and_owner() {
    let key: BotRunKey = serde_json::from_value(serde_json::json!({
        "owner":{"user_scope":Uuid::new_v4(),"device":Uuid::new_v4()},
        "bot":Uuid::new_v4(),"run":Uuid::new_v4()
    }))
    .unwrap();
    let command = BotCommand::Interrupt {
        key: key.clone(),
        fence: None,
    };
    assert_eq!(command.owner(), Some(&key.owner));
    let bytes = koru_ipc::frame::encode(11, &command).unwrap();
    let (version, decoded): (u8, BotCommand) =
        koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
    assert_eq!(version, crate::ipc::WIRE_VERSION);
    assert_eq!(decoded, command);
    let mut wire = serde_json::to_value(command).unwrap();
    assert_eq!(wire["command"], "interrupt");
    wire["force"] = serde_json::json!(true);
    assert!(serde_json::from_value::<BotCommand>(wire).is_err());
    let bytes = koru_ipc::frame::encode(10, &serde_json::json!({"frame":"hello"})).unwrap();
    let error = crate::local::read_current_response(&mut bytes.as_slice()).unwrap_err();
    assert_eq!(error.code, "delegate_incompatible");
    assert!(!error.retryable);
}

#[test]
fn drain_commands_use_current_ipc_and_refuse_undeclared_trigger_authority() {
    assert_eq!(crate::ipc::WIRE_VERSION, 11);
    let request = prepare();
    for reason in [
        NativeDrainReason::GatewayStartup,
        NativeDrainReason::RunRecovery,
        NativeDrainReason::OwnerStop,
        NativeDrainReason::TerminalTurn,
        NativeDrainReason::UnconfirmedOperation,
    ] {
        let command = BotCommand::CheckNativeEffectDrain {
            key: request.key.clone(),
            reason,
        };
        let encoded = koru_ipc::frame::encode(crate::ipc::WIRE_VERSION, &command).unwrap();
        let (version, decoded): (u8, BotCommand) =
            koru_ipc::frame::read_frame(&mut encoded.as_slice()).unwrap();
        assert_eq!(version, 11);
        assert_eq!(decoded, command);
        let mut json = serde_json::to_value(command).unwrap();
        json["reason"]["trusted"] = serde_json::json!(true);
        let encoded = koru_ipc::frame::encode(crate::ipc::WIRE_VERSION, &json).unwrap();
        assert!(koru_ipc::frame::read_frame::<_, BotCommand>(&mut encoded.as_slice()).is_err());
    }
    let mut json = serde_json::to_value(BotCommand::CheckNativeEffectDrain {
        key: request.key,
        reason: NativeDrainReason::GatewayStartup,
    })
    .unwrap();
    json["reason"]["reason"] = serde_json::json!("ordinary_read");
    assert!(serde_json::from_value::<BotCommand>(json).is_err());
}

#[test]
fn bootstrap_authority_is_redacted_and_requires_an_exact_match() {
    let secret = "PRIVATE-CAPABILITY-SENTINEL-0123456789";
    let authority = BotDelegateAuthority::new(secret.into()).unwrap();
    assert!(!format!("{authority:?}").contains(secret));
    assert!(authority.matches(&authority.clone()));
    assert!(!authority.matches(
        &BotDelegateAuthority::new("different-capability-01234567890123456789".into()).unwrap()
    ));
    assert!(BotDelegateAuthority::new("short".into()).is_none());
    assert!(serde_json::from_str::<BotDelegateAuthority>("\"short\"").is_err());
}

#[test]
fn begin_turn_uses_the_strict_nested_input_adapter() {
    let mut value = serde_json::json!({
        "command":"begin_turn",
        "fence": {
            "owner":{"user_scope":Uuid::new_v4(),"device":Uuid::new_v4()},
            "bot":Uuid::new_v4(),"run":Uuid::new_v4(),"attempt":Uuid::new_v4(),
            "generation":1,"session":Uuid::new_v4(),"turn":null
        },
        "input":{"idempotency_key":Uuid::new_v4(),"text":"private","attachment_ids":[]}
    });
    assert!(serde_json::from_value::<BotCommand>(value.clone()).is_ok());
    let mut answer = value.clone();
    answer["command"] = serde_json::json!("answer_questions");
    answer.as_object_mut().unwrap().remove("input");
    answer["request"] = serde_json::json!(Uuid::new_v4());
    answer["answers"] =
        serde_json::json!([{"question_id":"one","value":{"kind":"text","text":"answer"}}]);
    assert!(serde_json::from_value::<BotCommand>(answer.clone()).is_ok());
    let mut skip = answer.clone();
    skip["command"] = serde_json::json!("skip_questions");
    skip.as_object_mut().unwrap().remove("answers");
    let decoded: BotCommand = serde_json::from_value(skip.clone()).unwrap();
    assert!(matches!(decoded, BotCommand::SkipQuestions { .. }));
    assert!(decoded.owner().is_some());
    skip["answers"] = serde_json::json!([]);
    assert!(serde_json::from_value::<BotCommand>(skip).is_err());
    for pointer in ["/answers/0", "/answers/0/value"] {
        let mut changed = answer.clone();
        changed.pointer_mut(pointer).unwrap()["undeclared_authority"] = serde_json::json!(true);
        assert!(serde_json::from_value::<BotCommand>(changed).is_err());
    }
    value["input"]["undeclared_authority"] = serde_json::json!(true);
    assert!(serde_json::from_value::<BotCommand>(value).is_err());
}

fn prepare() -> PrepareNativeResult {
    PrepareNativeResult {
        key: NativeResultKey {
            run: RunId::mint(),
            attempt: RunAttemptId::mint(),
            generation: AttemptGeneration::FIRST,
            turn: Uuid::new_v4().to_string().parse().unwrap(),
            operation_id: Uuid::new_v4(),
        },
        kind: NativeResultOperation::CreatePage,
        destination: WorkspaceId::from_uuid(Uuid::new_v4()),
        target: None,
        request_fingerprint: [7; 32],
        expected_revision: None,
        config_revision: 1,
        policy_revision: 1,
        title: "PRIVATE-TITLE-SENTINEL".into(),
    }
}

#[test]
fn native_result_shapes_are_closed_bounded_and_redacted() {
    let mut value = prepare();
    assert!(value.valid());
    assert!(!format!("{value:?}").contains(&value.title));
    let mut wire = serde_json::to_value(&value).unwrap();
    wire["authority"] = serde_json::json!("forged");
    assert!(serde_json::from_value::<PrepareNativeResult>(wire).is_err());
    value.kind = NativeResultOperation::EditPage;
    assert!(!value.valid());
    value.target = Some(DocumentId::from_uuid(Uuid::new_v4()));
    value.expected_revision = Some(vec![1; 65536]);
    assert!(value.valid());
    value.expected_revision.as_mut().unwrap().push(1);
    assert!(!value.valid());
    value = prepare();
    value.title = "x".repeat(257);
    assert!(!value.valid());
    value = prepare();
    value.key.operation_id = Uuid::nil();
    assert!(!value.valid());
}

#[test]
fn reserved_commands_refuse_arbitrary_fields() {
    let command = BotCommand::PrepareNativeResult { request: prepare() };
    let mut wire = serde_json::to_value(command).unwrap();
    wire["extra"] = serde_json::json!(true);
    assert!(serde_json::from_value::<BotCommand>(wire).is_err());
}

#[test]
fn historical_read_window_is_strict_and_lifecycle_is_owner_and_revision_fenced() {
    let key: BotRunKey = serde_json::from_value(serde_json::json!({
        "owner": {"user_scope":Uuid::new_v4(),"device":Uuid::new_v4()},
        "bot":Uuid::new_v4(),"run":Uuid::new_v4()
    }))
    .unwrap();
    let command = BotCommand::Read {
        key: key.clone(),
        window: TimelineQuery {
            from: 0,
            len: 100,
            workspace: None,
        },
    };
    let mut value = serde_json::to_value(&command).unwrap();
    assert_eq!(
        serde_json::from_value::<BotCommand>(value.clone()).unwrap(),
        command
    );
    value["window"]["undeclared_authority"] = serde_json::json!(true);
    assert!(serde_json::from_value::<BotCommand>(value).is_err());
    let mut lifecycle = BotLifecycleFence {
        barrier: BotPolicyBarrierKey {
            owner: key.owner.clone(),
            bot: key.bot,
            request: koru_delegate_core::BotRequestId::mint().into(),
        },
        config_revision: 2,
        policy_revision: 1,
    };
    assert!(lifecycle.valid_for(&key));
    let command = BotCommand::Trash {
        key: key.clone(),
        lifecycle: lifecycle.clone(),
    };
    let value = serde_json::to_value(&command).unwrap();
    assert_eq!(
        serde_json::from_value::<BotCommand>(value).unwrap(),
        command
    );
    lifecycle.barrier.bot = koru_delegate_core::BotId::mint();
    assert!(!lifecycle.valid_for(&key));
    lifecycle.barrier.bot = key.bot;
    lifecycle.barrier.owner.device = koru_delegate_core::DeviceId::new();
    assert!(!lifecycle.valid_for(&key));
    lifecycle.barrier.owner = key.owner;
    lifecycle.policy_revision = 0;
    assert!(!lifecycle.valid_for(&key));
}

#[test]
fn native_core_reexport_preserves_the_frozen_ipc_seven_prepare_shape() {
    let request = prepare();
    let neutral: koru_delegate_core::PrepareNativeResult = request.clone();
    let expected = serde_json::json!({"command":"prepare_native_result","request":{
        "key":{"run":request.key.run,"attempt":request.key.attempt,"generation":request.key.generation,"turn":request.key.turn,"operation_id":request.key.operation_id},
        "kind":"create_page","destination":request.destination,"target":null,"request_fingerprint":vec![7;32],"expected_revision":null,"config_revision":1,"policy_revision":1,"title":"PRIVATE-TITLE-SENTINEL"
    }});
    assert_eq!(
        serde_json::to_value(BotCommand::PrepareNativeResult { request: neutral }).unwrap(),
        expected
    );
    let decoded: BotCommand = serde_json::from_value(expected.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), expected);
    let mut unknown = expected;
    unknown["request"]["extra_authority"] = true.into();
    assert!(serde_json::from_value::<BotCommand>(unknown).is_err());
}

#[test]
fn run_authority_lookup_has_no_caller_owner_and_refuses_unknown_fields() {
    let mut value =
        serde_json::json!({"command":"resolve_run_authority","run":Uuid::new_v4(),"generation":1});
    let command: BotCommand = serde_json::from_value(value.clone()).unwrap();
    assert!(command.owner().is_none());
    value["owner"] = serde_json::json!({"device":Uuid::new_v4()});
    assert!(serde_json::from_value::<BotCommand>(value).is_err());
    let ordinary = BotReply::RunAuthority {
        authority: koru_delegate_core::RunToolAuthority::Ordinary {},
    };
    assert_eq!(
        serde_json::to_value(ordinary).unwrap(),
        serde_json::json!({"result":"run_authority","authority":{"kind":"ordinary"}})
    );
    assert!(
        serde_json::from_value::<koru_delegate_core::RunToolAuthority>(
            serde_json::json!({"kind":"ordinary","owner":"guessed"})
        )
        .is_err()
    );
}
