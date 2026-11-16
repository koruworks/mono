// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::bots::{BotAccess, BotCommand, BotResponse};
use koru_delegate_schedule::SchedulePauseReason;

fn detail() -> BotRoutineDetail {
    BotRoutineDetail {
        bot_id: BotId::mint(),
        schedule_id: ScheduleId::mint(),
        bot_revision: 1,
        observed_at_ms: -60_000,
        creation: BotRoutineCreationObservation {
            request_id: BotRequestId::mint(),
            state: BotRoutineCreationState::Bound,
            refusal: None,
            updated_at_ms: -100,
        },
        access: BotRoutineGrantSummary {
            source_bot_policy_revision: 2,
            grant_revision: 2,
            access: BotAccess::Workspace,
            selected_page_count: 3,
        },
        access_at_setup: BotRoutineGrantSummary {
            source_bot_policy_revision: 1,
            grant_revision: 1,
            access: BotAccess::Default,
            selected_page_count: 0,
        },
        configured_model: BotRoutineModelObservation::Configured {
            provider: crate::AiProviderId::parse("agent-codex").unwrap(),
            requested_model: "PRIVATE-MODEL".into(),
        },
        destination: RoutineDestinationObservation::Current {
            home: WorkspaceId::new(),
            working_folder: BotWorkingFolder::new("/PRIVATE-FOLDER".into()).unwrap(),
        },
        live: RoutineDetailLive::Current {
            name: ScheduleName::new("PRIVATE-NAME").unwrap(),
            definition_revision: 1,
            effective_at_ms: -60_000,
            recurrence: RecurrenceRule::floating("0 9 * * *".parse().unwrap()),
            state: ScheduleState::Paused {
                owner: DeviceId::new(),
                reason: SchedulePauseReason::Manual,
            },
            owner_zone: RoutineOwnerZoneObservation::OwnerLocal {},
            next: RoutineNextObservation::Inactive {},
        },
        readiness: RoutineReadinessObservation::NotAssessed {},
        local_history: RoutineLocalHistoryObservation::Available {
            items: vec![],
            next_before_occurrence_ms: None,
        },
        occurrences: RoutineOccurrenceHistoryObservation::Available {
            items: vec![],
            next_before_occurrence_ms: None,
        },
    }
}
#[test]
fn routine_detail_wire_is_closed_and_counts_complete_response() {
    let response = BotResponse::RoutineDetail {
        reply: BotRoutineDetailEnvelope { detail: detail() },
    };
    let value = serde_json::to_value(&response).unwrap();
    for path in [
        vec![],
        vec!["detail"],
        vec!["detail", "creation"],
        vec!["detail", "access"],
        vec!["detail", "access_at_setup"],
        vec!["detail", "configured_model"],
        vec!["detail", "destination"],
        vec!["detail", "live"],
        vec!["detail", "live", "state"],
        vec!["detail", "live", "state", "reason"],
        vec!["detail", "live", "recurrence"],
        vec!["detail", "live", "owner_zone"],
        vec!["detail", "live", "next"],
        vec!["detail", "readiness"],
        vec!["detail", "local_history"],
    ] {
        let mut changed = value.clone();
        let mut at = &mut changed;
        for part in path {
            at = &mut at[part];
        }
        at.as_object_mut()
            .unwrap()
            .insert("extra".into(), serde_json::json!(true));
        assert!(serde_json::from_value::<BotResponse>(changed).is_err());
    }
    assert_eq!(
        serde_json::from_value::<BotResponse>(value).unwrap(),
        response
    );
    let debug = format!("{response:?}");
    for secret in ["PRIVATE-NAME", "PRIVATE-MODEL", "PRIVATE-FOLDER"] {
        assert!(!debug.contains(secret));
    }
    let mut large = response;
    let base = serde_json::to_vec(&large).unwrap().len();
    let BotResponse::RoutineDetail { reply } = &mut large else {
        panic!()
    };
    let BotRoutineModelObservation::Configured {
        requested_model, ..
    } = &mut reply.detail.configured_model
    else {
        panic!()
    };
    *requested_model = "x".repeat(BOT_ROUTINE_LIST_BYTES - base + requested_model.len());
    assert_eq!(
        serde_json::to_vec(&large).unwrap().len(),
        BOT_ROUTINE_LIST_BYTES
    );
    validate_routine_detail_bytes(&large).unwrap();
    let BotResponse::RoutineDetail { reply } = &mut large else {
        panic!()
    };
    let BotRoutineModelObservation::Configured {
        requested_model, ..
    } = &mut reply.detail.configured_model
    else {
        panic!()
    };
    requested_model.push('x');
    assert_eq!(
        validate_routine_detail_bytes(&large),
        Err(BotError::ContextTooLarge)
    );
    for cursor in [serde_json::json!("0"), serde_json::json!(0.5)] {
        let mut request = serde_json::to_value(BotCommand::ReadRoutineDetail {
            bot_id: BotId::mint(),
            schedule_id: ScheduleId::mint(),
            before_occurrence_ms: Some(i64::MIN),
            history_limit: 16,
        })
        .unwrap();
        request["before_occurrence_ms"] = cursor;
        assert!(serde_json::from_value::<BotCommand>(request).is_err());
    }
    for limit in [0, 17, 255] {
        assert_eq!(
            validate_routine_detail_request(BotId::mint(), ScheduleId::mint(), limit),
            Err(BotError::Invalid)
        );
    }
}
#[test]
fn routine_detail_readiness_is_ordered_partial_not_ready() {
    let live = detail().live;
    let RoutineReadinessObservation::Blocked { reasons } =
        routine_readiness(true, true, false, &live)
    else {
        panic!()
    };
    assert_eq!(
        reasons,
        vec![
            RoutineReadinessReason::BotPaused,
            RoutineReadinessReason::BotArchived,
            RoutineReadinessReason::CreationNotBound,
            RoutineReadinessReason::SchedulePaused
        ]
    );
    assert_eq!(
        routine_readiness(false, false, true, &RoutineDetailLive::Unavailable {}),
        RoutineReadinessObservation::Unavailable {}
    );
    assert_eq!(
        routine_readiness(false, false, true, &RoutineDetailLive::NotLive {}),
        RoutineReadinessObservation::Blocked {
            reasons: vec![RoutineReadinessReason::ScheduleNotLive]
        }
    );
    let floating = RoutineDetailObservation::Current {
        name: ScheduleName::new("x").unwrap(),
        definition_revision: 1,
        effective_at_ms: 0,
        recurrence: RecurrenceRule::floating("0 9 * * *".parse().unwrap()),
        state: ScheduleState::Active {
            owner: DeviceId::new(),
        },
    }
    .live(0);
    assert!(matches!(
        floating,
        RoutineDetailLive::Current {
            next: RoutineNextObservation::OwnerLocal {},
            owner_zone: RoutineOwnerZoneObservation::OwnerLocal {},
            ..
        }
    ));
    assert_eq!(
        routine_readiness(false, false, true, &floating),
        RoutineReadinessObservation::NotAssessed {}
    );
    assert!(
        serde_json::from_value::<RoutineReadinessObservation>(
            serde_json::json!({"readiness":"blocked","reasons":["schedule_paused","bot_paused"]})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<RoutineReadinessObservation>(
            serde_json::json!({"readiness":"blocked","reasons":["bot_paused","bot_paused"]})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<RoutineLocalHistoryObservation>(
            serde_json::json!({"observation":"unavailable","items":[]})
        )
        .is_err()
    );
}
