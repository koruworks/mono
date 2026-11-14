// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::*;

const DEFINITION: &str = r#"{"revision":1,"effective_at_ms":1000,"template":{"requested_by":{"kind":"person","profile":"00000000-0000-4000-8000-000000000001"},"objective":"task","agent":{"kind":"built_in","value":"codex"},"requested_model":null,"scope":{"kind":"temporary"},"grants":["00000000-0000-4000-8000-000000000002"],"context":[]},"recurrence":{"type":"floating","cron":"*/1 * * * *"},"lifecycle":"active","routine":{"owner":{"user_scope":"00000000-0000-4000-8000-000000000004","device":"00000000-0000-4000-8000-000000000005"},"bot":"00000000-0000-4000-8000-000000000006","home":"00000000-0000-4000-8000-000000000002","creation_request":"00000000-0000-4000-8000-000000000007"}}"#;

#[test]
fn semantic_hash_is_shared_replay_identity_before_current_policy_or_ids() {
    let mut definition: ScheduleDefinition = serde_json::from_str(DEFINITION).unwrap();
    let association = definition.routine.clone().unwrap();
    let slot = RoutineSlot {
        owner: association.owner,
        bot: association.bot,
        schedule: "00000000-0000-4000-8000-000000000008".parse().unwrap(),
        occurrence_ms: 60_000,
    };
    let old = routine_semantic_digest(&slot, &definition, association.home, None).unwrap();
    assert_eq!(
        old,
        routine_semantic_digest(&slot, &definition, association.home, None).unwrap()
    );
    let mut changed = slot;
    changed.occurrence_ms += 60_000;
    assert_ne!(
        old,
        routine_semantic_digest(&changed, &definition, association.home, None).unwrap()
    );
    definition.template.requested_model = Some(ModelName::new("inert-changed-model").unwrap());
    assert_ne!(
        old,
        routine_semantic_digest(&slot, &definition, association.home, None).unwrap()
    );
    definition.routine.as_mut().unwrap().bot = BotId::mint();
    assert!(routine_semantic_digest(&slot, &definition, association.home, None).is_err());
}

#[test]
fn semantic_encoding_has_frozen_field_order_and_no_allocated_or_current_fields() {
    let definition: ScheduleDefinition = serde_json::from_str(DEFINITION).unwrap();
    let creation = definition.routine.as_ref().unwrap();
    let slot = RoutineSlot {
        owner: creation.owner,
        bot: creation.bot,
        schedule: "00000000-0000-4000-8000-000000000008".parse().unwrap(),
        occurrence_ms: 60_000,
    };
    let identity = RoutineDefinitionIdentity {
        revision: 1,
        digest: routine_definition_digest(&definition, creation.home, None).unwrap(),
        owner_handoff: None,
    };
    let encoded = serde_json::to_string(&SemanticV1 {
        version: 1,
        slot: &slot,
        definition: identity,
        creation,
        objective: &definition.template.objective,
        task_context: &definition.template.context,
    })
    .unwrap();
    assert_eq!(
        encoded,
        r#"{"version":1,"slot":{"owner":{"user_scope":"00000000-0000-4000-8000-000000000004","device":"00000000-0000-4000-8000-000000000005"},"bot":"00000000-0000-4000-8000-000000000006","schedule":"00000000-0000-4000-8000-000000000008","occurrence_ms":60000},"definition":{"revision":1,"digest":"a4d22d3a01e7972e7ff3fb2ec6258ebfc200d1f4cf3091240cd2e76037db7115","owner_handoff":null},"creation":{"owner":{"user_scope":"00000000-0000-4000-8000-000000000004","device":"00000000-0000-4000-8000-000000000005"},"bot":"00000000-0000-4000-8000-000000000006","home":"00000000-0000-4000-8000-000000000002","creation_request":"00000000-0000-4000-8000-000000000007"},"objective":"task","task_context":[]}"#
    );
    assert_eq!(
        String::from(routine_semantic_digest(&slot, &definition, creation.home, None).unwrap()),
        "622de77ed4def4d0455892130ddc2775ecbb83fc5d471e3e4742b67584fd0700"
    );
}
