// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::bots::BotCommand;

#[test]
fn routine_owner_wire_is_closed_without_changing_old_command_bytes() {
    let bot = "11111111-1111-4111-8111-111111111111";
    let request = "22222222-2222-4222-8222-222222222222";
    let old = format!(r#"{{"command":"read","bot_id":"{bot}"}}"#);
    let decoded: BotCommand = serde_json::from_str(&old).unwrap();
    assert_eq!(serde_json::to_string(&decoded).unwrap(), old);
    let value = serde_json::json!({"command":"create_routine","request_id":request,"bot_id":bot,"expected_bot_revision":1,"name":"Daily","objective":"private task","documents":[],"recurrence":{"type":"floating","cron":"0 9 * * *"}});
    let command: BotCommand = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        serde_json::from_slice::<BotCommand>(&serde_json::to_vec(&command).unwrap()).unwrap(),
        command
    );
    for key in [
        "model",
        "grant",
        "owner",
        "workspace",
        "schedule_id",
        "enabled",
        "scope",
    ] {
        let mut bad = value.clone();
        bad[key] = serde_json::json!("forged");
        assert!(serde_json::from_value::<BotCommand>(bad).is_err(), "{key}");
    }
    let mut bad = value.clone();
    bad["recurrence"]["unknown"] = serde_json::json!(true);
    assert!(serde_json::from_value::<BotCommand>(bad).is_err());
    let mut bad = value;
    bad["command"] = serde_json::json!("read_routines");
    assert!(serde_json::from_value::<BotCommand>(bad).is_err());
    let retry = serde_json::json!({"command":"retry_routine_creation","bot_id":bot,"original_request_id":request,"observed_token":{"state":"unconfirmed","updated_at_ms":1,"extra":true}});
    assert!(serde_json::from_value::<BotCommand>(retry).is_err());
}

#[test]
fn routine_creation_status_contains_only_creation_evidence() {
    let status = BotRoutineCreationStatus {
        request_id: BotRequestId::mint(),
        bot_id: BotId::mint(),
        schedule_id: ScheduleId::mint(),
        observed_token: BotRoutineCreationObserved {
            state: BotRoutineCreationState::Unconfirmed,
            updated_at_ms: 8,
        },
        refusal: None,
        can_retry: true,
        source_bot_policy_revision: 7,
        grant_revision: 1,
    };
    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(json.as_object().unwrap().len(), 8);
    for key in [
        "seed",
        "grant",
        "name",
        "lifecycle",
        "occurrences",
        "origin_edit_revision",
        "store_request",
        "model",
    ] {
        assert!(json.get(key).is_none(), "{key}");
    }
    let mut bad = json;
    bad["lifecycle"] = serde_json::json!("active");
    assert!(serde_json::from_value::<BotRoutineCreationStatus>(bad).is_err());
}
