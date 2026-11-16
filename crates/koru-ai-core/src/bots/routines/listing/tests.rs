// SPDX-License-Identifier: MPL-2.0
use super::*;

fn page() -> BotRoutineListPage {
    BotRoutineListPage {
        bot_id: BotId::mint(),
        bot_revision: 1,
        effective_model: BotRoutineModelObservation::Configured {
            provider: AiProviderId::parse("agent-codex").unwrap(),
            requested_model: "PRIVATE-MODEL".into(),
        },
        items: vec![BotRoutineListItem {
            schedule_id: ScheduleId::mint(),
            creation: BotRoutineCreationObservation {
                request_id: BotRequestId::mint(),
                state: BotRoutineCreationState::Bound,
                refusal: None,
                updated_at_ms: 0,
            },
            grant: BotRoutineGrantSummary {
                source_bot_policy_revision: 1,
                grant_revision: 1,
                access: BotAccess::Default,
                selected_page_count: 0,
            },
            live: BotRoutineLiveObservation::Current {
                name: ScheduleName::new("PRIVATE-NAME").unwrap(),
                definition_revision: 1,
                state: ScheduleState::Paused {
                    owner: DeviceId::new(),
                    reason: SchedulePauseReason::Manual,
                },
                next: RoutineNextObservation::Inactive {},
            },
            local_conversations: BotRoutineLocalConversations::Available {
                items: vec![],
                more: false,
            },
        }],
        next_schedule: None,
    }
}

#[test]
fn listing_wire_is_closed_through_nested_state_and_pause_reason() {
    let value = serde_json::to_value(page()).unwrap();
    for path in [
        vec![],
        vec!["effective_model"],
        vec!["items", "0"],
        vec!["items", "0", "live"],
        vec!["items", "0", "live", "state"],
        vec!["items", "0", "live", "state", "reason"],
    ] {
        let diagnostic = format!("{path:?}");
        let mut altered = value.clone();
        let mut target = &mut altered;
        for part in path {
            target = if part == "0" {
                &mut target[0]
            } else {
                &mut target[part]
            };
        }
        target
            .as_object_mut()
            .unwrap()
            .insert("extra".into(), serde_json::json!(true));
        assert!(
            serde_json::from_value::<BotRoutineListPage>(altered).is_err(),
            "{diagnostic}"
        );
    }
    let mut other = value;
    other["items"][0]["live"]["state"]["reason"] =
        serde_json::json!({"reason":"context_unavailable","item":0,"stage":"resolve","extra":true});
    assert!(serde_json::from_value::<BotRoutineListPage>(other).is_err());
    let mut retired = serde_json::to_value(page()).unwrap();
    retired["items"][0]["live"]["state"] = serde_json::json!({"state":"retired","extra":true});
    assert!(serde_json::from_value::<BotRoutineListPage>(retired).is_err());
}

#[test]
fn listing_command_preserves_id_domains_and_rejects_foreign_fields() {
    for limit in [0, 17, 255] {
        assert_eq!(
            validate_routine_list_request(BotId::mint(), None, limit),
            Err(BotError::Invalid)
        );
    }
    assert_eq!(
        validate_routine_list_request(
            BotId::mint(),
            Some(ScheduleId::from_uuid(uuid::Uuid::nil())),
            1
        ),
        Err(BotError::Invalid)
    );
    let request = crate::bots::BotCommand::ReadRoutines {
        bot_id: BotId::mint(),
        after_schedule: Some(ScheduleId::mint()),
        limit: 16,
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serde_json::from_value::<crate::bots::BotCommand>(value.clone()).unwrap(),
        request
    );
    for key in ["owner", "grant", "scope"] {
        let mut bad = value.clone();
        bad[key] = serde_json::json!("caller-selected");
        assert!(serde_json::from_value::<crate::bots::BotCommand>(bad).is_err());
    }
    let mut bad = value;
    bad["after_schedule"] = serde_json::json!("not-an-id");
    assert!(serde_json::from_value::<crate::bots::BotCommand>(bad).is_err());
}

#[test]
fn listing_debug_never_prints_model_or_schedule_name() {
    let p = page();
    for debug in [
        format!("{p:?}"),
        format!("{:?}", p.effective_model),
        format!("{:?}", p.items[0]),
        format!("{:?}", p.items[0].live),
        format!("{:?}", crate::bots::BotResponse::Routines { page: p }),
    ] {
        assert!(!debug.contains("PRIVATE"));
        assert!(debug.contains("REDACTED"));
    }
}

#[test]
fn listing_counted_limit_includes_complete_response_wrapper_and_json_escaping() {
    let mut p = page();
    p.effective_model = BotRoutineModelObservation::Configured {
        provider: AiProviderId::parse("agent-codex").unwrap(),
        requested_model: String::new(),
    };
    let base = serde_json::to_vec(&crate::bots::BotResponse::Routines { page: p.clone() })
        .unwrap()
        .len();
    let BotRoutineModelObservation::Configured {
        requested_model, ..
    } = &mut p.effective_model
    else {
        unreachable!()
    };
    *requested_model = "a".repeat(BOT_ROUTINE_LIST_BYTES - base);
    let mut response = crate::bots::BotResponse::Routines { page: p };
    assert_eq!(validate_routine_list_bytes(&response), Ok(()));
    let crate::bots::BotResponse::Routines { page: p } = &mut response else {
        unreachable!()
    };
    let BotRoutineModelObservation::Configured {
        requested_model, ..
    } = &mut p.effective_model
    else {
        unreachable!()
    };
    requested_model.push('"'); // one character, two encoded bytes
    assert!(validate_routine_list_bytes(p).is_ok()); // the outer wrapper matters
    assert_eq!(
        validate_routine_list_bytes(&response),
        Err(BotError::ContextTooLarge)
    );
}

#[test]
fn listing_maximum_legitimate_page_roundtrips_without_payload_or_firing_fields() {
    let mut p = page();
    p.effective_model = BotRoutineModelObservation::Configured {
        provider: AiProviderId::parse("agent-codex").unwrap(),
        requested_model: "m".repeat(512),
    };
    let mut item = p.items.remove(0);
    item.live = BotRoutineLiveObservation::Current {
        name: ScheduleName::new("界".repeat(128)).unwrap(),
        definition_revision: u32::MAX,
        state: ScheduleState::HandoffPending {
            owner: DeviceId::new(),
            requests: (0..256).map(|_| HandoffId::mint()).collect(),
        },
        next: RoutineNextObservation::Inactive {},
    };
    item.local_conversations = BotRoutineLocalConversations::Available {
        items: (0..4)
            .map(|_| BotRoutineLocalConversation {
                occurrence_ms: i64::MIN,
                run_id: RunId::mint(),
                chat_id: uuid::Uuid::new_v4(),
                attempt_id: uuid::Uuid::new_v4(),
                conversation: BotRoutineConversationVisibility::Unavailable,
            })
            .collect(),
        more: true,
    };
    p.items = vec![item; 16];
    let response = crate::bots::BotResponse::Routines { page: p };
    validate_routine_list_bytes(&response).unwrap();
    let bytes = serde_json::to_vec(&response).unwrap();
    assert!(bytes.len() < BOT_ROUTINE_LIST_BYTES);
    assert_eq!(
        serde_json::from_slice::<crate::bots::BotResponse>(&bytes).unwrap(),
        response
    );
    let text = String::from_utf8(bytes).unwrap();
    for forbidden in [
        "capture_json",
        "submission_json",
        "firings",
        "objective",
        "working_folder",
        "credentials",
    ] {
        assert!(!text.contains(forbidden));
    }
}
