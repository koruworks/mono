// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::Request;

fn request() -> Request {
    Request::SteerPendingInput(SteerPendingInput {
        session_id: SessionId::mint(),
        controller_lease: ControllerLeaseToken::mint(),
        expected_turn: AgentTurnId::mint(),
        input: PendingInput {
            idempotency_key: IdempotencyKey::mint(),
            text: "amend".into(),
            attachment_ids: vec![],
            references: vec![],
            mode: None,
            fast: None,
            reasoning: None,
            skills: vec![],
            model: None,
        },
    })
}

#[test]
fn conditional_command_roundtrips_and_rejects_unknown_outer_and_nested_fields() {
    let value = serde_json::to_value(request()).unwrap();
    assert_eq!(value["kind"], "steer_pending_input");
    let decoded: Request = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    let mut outer = value.clone();
    outer["extra"] = true.into();
    assert!(serde_json::from_value::<Request>(outer).is_err());
    let mut nested = value.clone();
    nested["input"]["extra"] = true.into();
    assert!(serde_json::from_value::<Request>(nested.clone()).is_err());
    nested["kind"] = "send_pending_input".into();
    nested.as_object_mut().unwrap().remove("expected_turn");
    assert!(
        serde_json::from_value::<Request>(nested).is_ok(),
        "ordinary input retains additive decoding"
    );
    let raw = serde_json::to_string(&value).unwrap().replace(
        "\"text\":\"amend\"",
        "\"text\":\"amend\",\"text\":\"other\"",
    );
    assert!(serde_json::from_str::<Request>(&raw).is_err());
}

#[test]
fn text_only_validation_refuses_every_unsupported_field_without_dropping_it() {
    let Request::SteerPendingInput(command) = request() else {
        unreachable!()
    };
    assert!(command.input.validate_conditional_steering().is_ok());
    for (field, value) in [
        ("attachment_ids", serde_json::json!([AttachmentId::mint()])),
        (
            "references",
            serde_json::json!([{"kind":"file","path":"/fixture"}]),
        ),
        ("mode", serde_json::json!("other")),
        ("fast", serde_json::json!(false)),
        ("reasoning", serde_json::json!("high")),
        ("skills", serde_json::json!(["x"])),
        ("model", serde_json::json!("other")),
    ] {
        let mut input = serde_json::to_value(&command.input).unwrap();
        input[field] = value;
        let input: PendingInput = serde_json::from_value(input).unwrap();
        assert_eq!(
            input.validate_conditional_steering(),
            Err(ErrorCode::DeliveryUnsupported),
            "{field}"
        );
    }
    let mut empty = command.input;
    empty.text.clear();
    assert_eq!(
        empty.validate_conditional_steering(),
        Err(ErrorCode::InvalidRequest)
    );
}
