// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::{
    RoutineDefinitionIdentity, RoutineDigest32, RoutineProvenance, RunOrigin, ScheduleId,
};

// The unchanged v1 bytes are the input to existing durable fingerprint owners.
// Do not produce this golden with the serializer whose compatibility it tests.
const LEGACY: &str = r#"{"version":1,"submission":{"provenance":{"owner":{"user_scope":"00000000-0000-4000-8000-00000000000a","device":"00000000-0000-4000-8000-00000000000b"},"bot":"00000000-0000-4000-8000-00000000000c","submission":"00000000-0000-4000-8000-000000000001","chat_id":"00000000-0000-4000-8000-000000000003","ai_attempt_id":"00000000-0000-4000-8000-000000000004","assistant_message_id":"00000000-0000-4000-8000-000000000006","config_revision":1,"policy_revision":1,"continued_from":null},"policy":{"workspace":"00000000-0000-4000-8000-00000000000e","access":"default","selected_pages":[],"destination":"00000000-0000-4000-8000-00000000000e"},"request":{"run":"00000000-0000-4000-8000-000000000002","requested_by":{"kind":"person","profile":"00000000-0000-4000-8000-00000000000f"},"objective":"task","agent":{"kind":"built_in","value":"codex"},"scope":{"kind":"bot_folder","folder":"/bot"},"context":[],"grants":["00000000-0000-4000-8000-00000000000e"],"executing_device":"00000000-0000-4000-8000-00000000000b","origin":{"origin":"manual"}},"workspace":"00000000-0000-4000-8000-00000000000e","input":{"idempotency_key":"00000000-0000-4000-8000-000000000010","text":"task","attachment_ids":[]}}}"#;

#[test]
fn routine_envelope_addition_preserves_exact_v1_submission_bytes() {
    let intent: BotSubmissionIntent = serde_json::from_str(LEGACY).unwrap();
    assert!(intent.validate().is_ok());
    assert!(intent.validate_for_kind(BotOperationKind::Start).is_ok());
    assert_eq!(serde_json::to_string(&intent).unwrap(), LEGACY);
}

#[test]
fn routine_receipt_kind_and_version_cohorts_are_explicit_and_drafts_never_execute() {
    let mut intent: BotSubmissionIntent = serde_json::from_str(LEGACY).unwrap();
    for kind in [
        BotOperationKind::RoutineDraft,
        BotOperationKind::RoutineOccurrence,
        BotOperationKind::Continue,
    ] {
        assert!(intent.validate_for_kind(kind).is_err());
    }
    let provenance = RoutineProvenance {
        schedule: ScheduleId::from_uuid(
            uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000011").unwrap(),
        ),
        occurrence_ms: 10,
        definition: RoutineDefinitionIdentity {
            revision: 1,
            digest: RoutineDigest32::from_bytes([1; 32]),
            owner_handoff: None,
        },
        grant_revision: 1,
    };
    intent.submission.provenance.routine = Some(provenance);
    intent.submission.request.origin = RunOrigin::Scheduled {
        schedule: provenance.schedule,
        occurrence_ms: provenance.occurrence_ms,
        revision: provenance.definition.revision,
    };
    assert!(
        intent
            .validate_for_kind(BotOperationKind::RoutineOccurrence)
            .is_err()
    );
    intent.version = 2;
    assert!(
        intent
            .validate_for_kind(BotOperationKind::RoutineOccurrence)
            .is_ok()
    );
    assert!(intent.validate().is_err());
    for kind in [
        BotOperationKind::RoutineDraft,
        BotOperationKind::Start,
        BotOperationKind::Continue,
        BotOperationKind::Pause,
        BotOperationKind::Resume,
        BotOperationKind::UpdateAccess,
    ] {
        assert!(intent.validate_for_kind(kind).is_err());
    }
    for kind in [
        BotOperationKind::RoutineDraft,
        BotOperationKind::RoutineOccurrence,
    ] {
        assert_eq!(BotOperationKind::parse(kind.token()), Some(kind));
        assert_eq!(serde_json::to_value(kind).unwrap(), kind.token());
    }
    intent.version = 3;
    assert!(
        intent
            .validate_for_kind(BotOperationKind::RoutineOccurrence)
            .is_err()
    );
}
