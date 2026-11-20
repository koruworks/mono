// SPDX-License-Identifier: MPL-2.0

use koru_recurrence::{CronExpression, RecurrenceRule};
use std::collections::BTreeSet;
use std::path::Path;
use uuid::Uuid;

use super::*;
use koru_delegate_core::{
    ActorRef, DeviceId, DocumentId, ModelName, Objective, ProfileId, RunHeader, RunId,
    SessionProfileKey, WorkspaceId, WorkspaceScope,
};

fn v4(n: u8) -> Uuid {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
        .parse()
        .unwrap()
}

fn device_id(n: u8) -> DeviceId {
    DeviceId::from_uuid(v4(n))
}

#[test]
fn dependency_direction_is_domain_only() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let section = manifest
        .split("[dependencies]")
        .nth(1)
        .unwrap()
        .split("[dev-dependencies]")
        .next()
        .unwrap();
    let actual = section
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
        .filter(|name| !name.is_empty() && !name.starts_with('#'))
        .map(|name| name.split('.').next().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual,
        BTreeSet::from(["koru-delegate-core", "koru-recurrence", "schemars", "serde",])
    );
}

fn definition() -> ScheduleDefinition {
    ScheduleDefinition {
        routine: None,
        revision: 1,
        effective_at_ms: 1_000,
        template: DelegationTemplate {
            requested_by: ActorRef::Person {
                profile: ProfileId::from_uuid(v4(1)),
            },
            objective: Objective::new("毎朝 ملخصًا do the work").unwrap(),
            agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
            requested_model: Some(ModelName::new("gpt-5.4").unwrap()),
            scope: WorkspaceScope::Temporary,
            grants: BTreeSet::from([WorkspaceId::from_uuid(v4(2))]),
            context: vec![ScheduleContextPointer::Document {
                workspace: WorkspaceId::from_uuid(v4(2)),
                document: DocumentId::from_uuid(v4(3)),
            }],
        },
        recurrence: RecurrenceRule::floating(CronExpression::parse("0 9 * * MON-FRI").unwrap()),
        lifecycle: ScheduleLifecycle::Active,
    }
}

#[test]
fn template_round_trips_with_multilingual_text_and_selected_model() {
    let value = serde_json::to_string(&definition()).unwrap();
    let round_trip: ScheduleDefinition = serde_json::from_str(&value).unwrap();
    assert_eq!(round_trip, definition());
    assert!(format!("{round_trip:?}").contains("Objective(<redacted>)"));
    assert!(!format!("{round_trip:?}").contains("ملخصًا"));
}

#[test]
fn routine_addition_preserves_ordinary_definition_json_bytes() {
    let legacy = concat!(
        r#"{"revision":1,"effective_at_ms":1000,"template":{"requested_by":{"kind":"person","profile":"00000000-0000-4000-8000-000000000001"},"objective":"毎朝 ملخصًا do the work","agent":{"kind":"built_in","value":"codex"},"requested_model":"gpt-5.4","scope":{"kind":"temporary"},"grants":["00000000-0000-4000-8000-000000000002"],"context":[{"kind":"document","workspace":"00000000-0000-4000-8000-000000000002","document":"00000000-0000-4000-8000-000000000003"}]},"recurrence":{"type":"floating","cron":"0 9 * * Mon,Tue,Wed,Thu,Fri"},"lifecycle":"active"}"#
    );
    assert_eq!(serde_json::to_string(&definition()).unwrap(), legacy);
    let decoded: ScheduleDefinition = serde_json::from_str(legacy).unwrap();
    assert_eq!(decoded, definition());
    assert!(decoded.routine.is_none());
}

#[test]
fn routine_association_has_strict_identity_and_no_authority_fields() {
    use koru_delegate_core::{BotId, BotOwner, BotRequestId};
    let association = RoutineAssociation {
        owner: BotOwner {
            user_scope: serde_json::from_value(serde_json::json!(v4(4).to_string())).unwrap(),
            device: device_id(5),
        },
        bot: BotId::from_uuid(v4(6)),
        home: WorkspaceId::from_uuid(v4(2)),
        creation_request: BotRequestId::from_uuid(v4(7)),
    };
    assert!(association.valid());
    let encoded = serde_json::to_value(association).unwrap();
    assert_eq!(encoded.as_object().unwrap().len(), 4);
    assert_eq!(
        serde_json::from_value::<RoutineAssociation>(encoded.clone()).unwrap(),
        association
    );
    let mut extra = encoded;
    extra["grant_revision"] = serde_json::json!(1);
    assert!(serde_json::from_value::<RoutineAssociation>(extra).is_err());
    let invalid = RoutineAssociation {
        bot: BotId::from_uuid(Uuid::nil()),
        ..association
    };
    assert!(!invalid.valid());
}

#[test]
fn schedule_names_are_trimmed_bounded_multilingual_and_redacted() {
    let longest = "界".repeat(MAX_SCHEDULE_NAME_CHARS);
    let name = ScheduleName::new(format!("  {longest}  ")).unwrap();
    assert_eq!(name.as_str(), longest);
    assert_eq!(name.char_count(), MAX_SCHEDULE_NAME_CHARS);
    assert_eq!(format!("{name:?}"), "ScheduleName(<redacted>)");
    assert!(matches!(
        ScheduleName::new("界".repeat(MAX_SCHEDULE_NAME_CHARS + 1)),
        Err(TextError::TooLong { .. })
    ));
    assert!(matches!(
        ScheduleName::new("one\ntwo"),
        Err(TextError::ControlCharacter)
    ));
    assert!(matches!(ScheduleName::new("   "), Err(TextError::Empty)));
}

#[test]
fn compatibility_name_uses_the_first_nonempty_objective_line_once() {
    let objective = Objective::new(format!(
        "\n  {} trailing words\nsecond line",
        "界".repeat(MAX_SCHEDULE_NAME_CHARS)
    ))
    .unwrap();
    let name = ScheduleName::from_objective(&objective);
    assert_eq!(name.as_str(), "界".repeat(MAX_SCHEDULE_NAME_CHARS));
}

#[test]
fn selected_model_survives_template_request_and_run_header() {
    let definition = definition();
    let selected = definition.template.requested_model.clone();
    let run = RunId::from_uuid(v4(20));
    let request = definition
        .template
        .into_request(run, device_id(7), Vec::new());
    assert_eq!(request.requested_model, selected);
    let header = RunHeader::from_request(request, 2_000);
    assert_eq!(header.run, run);
    assert_eq!(header.requested_model, selected);
}

#[test]
fn pause_reasons_are_closed_content_free_values() {
    for reason in [
        SchedulePauseReason::Manual,
        SchedulePauseReason::ProviderNotDelegatable,
        SchedulePauseReason::ScopeUnavailable,
        SchedulePauseReason::ContextUnavailable {
            item: 3,
            stage: ScheduleContextFailureStage::Retention,
        },
    ] {
        let json = serde_json::to_string(&reason).unwrap();
        assert_eq!(
            serde_json::from_str::<SchedulePauseReason>(&json).unwrap(),
            reason
        );
        assert!(!format!("{reason:?}").contains('/'));
    }
}

#[test]
fn firing_serde_uses_the_ratified_required_run_and_at_fields() {
    let firing = ScheduleFiring {
        occurrence_ms: 60_000,
        revision: 1,
        run: RunId::from_uuid(v4(30)),
        outcome: ScheduleFiringOutcome::SkippedMissed,
        busy_holder: None,
        at: 3_600_000,
    };
    let value = serde_json::to_value(&firing).unwrap();
    assert!(value.get("run").is_some());
    assert_eq!(value["at"], 3_600_000);
    assert!(value.get("at_ms").is_none());
    assert_eq!(
        serde_json::from_value::<ScheduleFiring>(value).unwrap(),
        firing
    );
}

#[test]
fn request_does_not_transfer_and_only_the_effective_owner_confirms() {
    let handoff = HandoffId::from_uuid(v4(10));
    let request = HandoffEvent::Requested {
        handoff,
        previous: None,
        requested_by: device_id(2),
        from: device_id(1),
        to: device_id(2),
        at_ms: 10,
    };
    let pending = HandoffState::reduce(device_id(1), std::slice::from_ref(&request));
    assert_eq!(pending.owner, device_id(1));
    assert_eq!(pending.pending, vec![handoff]);

    let wrong = HandoffEvent::Confirmed {
        handoff,
        confirmed_by: device_id(2),
        at_ms: 11,
    };
    let still_pending = HandoffState::reduce(device_id(1), &[request.clone(), wrong]);
    assert_eq!(still_pending.owner, device_id(1));
    assert_eq!(still_pending.pending, vec![handoff]);
    let confirmed = HandoffEvent::Confirmed {
        handoff,
        confirmed_by: device_id(1),
        at_ms: 12,
    };
    let moved = HandoffState::reduce(device_id(1), &[confirmed, request]);
    assert_eq!(moved.owner, device_id(2));
    assert_eq!(moved.epoch, Some(handoff));
}

#[cfg(feature = "schema")]
#[test]
fn recurrence_schema_matches_its_tagged_serde_shape() {
    let schedule = serde_json::to_value(schemars::schema_for!(ScheduleDefinition)).unwrap();
    let recurrence = &schedule["$defs"]["RecurrenceRule"];
    assert!(recurrence.to_string().contains("floating"));
    assert!(recurrence.to_string().contains("zoned"));
    assert_ne!(schedule["properties"]["recurrence"]["type"], "string");
}
