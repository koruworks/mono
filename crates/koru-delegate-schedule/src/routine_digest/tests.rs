// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::*;
use koru_delegate_core::*;
use uuid::Uuid;

const GOLDEN: &str = r#"{"version":1,"workspace":"00000000-0000-4000-8000-000000000002","owner_handoff":null,"definition":{"revision":1,"effective_at_ms":1000,"template":{"requested_by":{"kind":"person","profile":"00000000-0000-4000-8000-000000000001"},"objective":"task","agent":{"kind":"built_in","value":"codex"},"requested_model":null,"scope":{"kind":"temporary"},"grants":["00000000-0000-4000-8000-000000000002"],"context":[]},"recurrence":{"type":"floating","cron":"* * * * *"},"lifecycle":"active","routine":{"owner":{"user_scope":"00000000-0000-4000-8000-000000000004","device":"00000000-0000-4000-8000-000000000005"},"bot":"00000000-0000-4000-8000-000000000006","home":"00000000-0000-4000-8000-000000000002","creation_request":"00000000-0000-4000-8000-000000000007"}}}"#;
fn fixture() -> (ScheduleDefinition, WorkspaceId) {
    let json: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    (
        serde_json::from_value(json["definition"].clone()).unwrap(),
        serde_json::from_value(json["workspace"].clone()).unwrap(),
    )
}

#[test]
fn routine_definition_digest_has_complete_frozen_versioned_json_and_hash() {
    let (definition, workspace) = fixture();
    assert_eq!(
        serde_json::to_string(&DefinitionDigestV1 {
            version: 1,
            workspace,
            owner_handoff: None,
            definition: &definition
        })
        .unwrap(),
        GOLDEN
    );
    let digest = routine_definition_digest(&definition, workspace, None).unwrap();
    eprintln!("definition_fixture_digest={}", String::from(digest));
}

#[test]
fn routine_definition_digest_binds_inert_fields_association_context_and_epoch() {
    let (base, workspace) = fixture();
    let digest = routine_definition_digest(&base, workspace, None).unwrap();
    let mut cases = vec![];
    let mut d = base.clone();
    d.revision += 1;
    cases.push(d);
    let mut d = base.clone();
    d.effective_at_ms += 1;
    cases.push(d);
    let mut d = base.clone();
    d.template.requested_by = ActorRef::Person {
        profile: ProfileId::new(),
    };
    cases.push(d);
    let mut d = base.clone();
    d.template.objective = Objective::new("changed task").unwrap();
    cases.push(d);
    let mut d = base.clone();
    d.template.agent = SessionProfileKey::BuiltIn("claude".parse().unwrap());
    cases.push(d);
    let mut d = base.clone();
    d.template.requested_model = Some(ModelName::new("changed").unwrap());
    cases.push(d);
    let mut d = base.clone();
    d.template.scope = WorkspaceScope::None;
    cases.push(d);
    let mut d = base.clone();
    d.template.grants.insert(WorkspaceId::new());
    cases.push(d);
    let mut d = base.clone();
    d.template.context.push(ScheduleContextPointer::Document {
        workspace,
        document: DocumentId::new(),
    });
    cases.push(d);
    let mut d = base.clone();
    d.recurrence = RecurrenceRule::floating("0 9 * * *".parse().unwrap());
    cases.push(d);
    let mut d = base.clone();
    d.lifecycle = ScheduleLifecycle::Paused {
        reason: SchedulePauseReason::Manual,
    };
    cases.push(d);
    let mut d = base.clone();
    d.routine.as_mut().unwrap().bot = BotId::mint();
    cases.push(d);
    let mut d = base.clone();
    d.routine.as_mut().unwrap().creation_request = BotRequestId::mint();
    cases.push(d);
    let mut d = base.clone();
    d.routine.as_mut().unwrap().owner.device = DeviceId::new();
    cases.push(d);
    for d in cases {
        assert_ne!(
            routine_definition_digest(&d, workspace, None).unwrap(),
            digest
        )
    }
    let handoff = HandoffId::mint();
    assert_ne!(
        routine_definition_digest(&base, workspace, Some(handoff)).unwrap(),
        digest
    );
    let mut d = base.clone();
    let other = WorkspaceId::new();
    d.routine.as_mut().unwrap().home = other;
    assert_ne!(routine_definition_digest(&d, other, None).unwrap(), digest);
    let a = WorkspaceId::new();
    let z = WorkspaceId::new();
    let mut d = base.clone();
    d.template.grants = [a, z].into();
    let mut e = d.clone();
    e.template.grants = [z, a].into();
    assert_eq!(
        routine_definition_digest(&d, workspace, None),
        routine_definition_digest(&e, workspace, None)
    );
    d.template.context = vec![
        ScheduleContextPointer::Document {
            workspace,
            document: DocumentId::new(),
        },
        ScheduleContextPointer::Document {
            workspace,
            document: DocumentId::new(),
        },
    ];
    e = d.clone();
    e.template.context.reverse();
    assert_ne!(
        routine_definition_digest(&d, workspace, None),
        routine_definition_digest(&e, workspace, None)
    );
}

#[test]
fn routine_definition_digest_refuses_invalid_identity_and_large_json_without_new_uuid_rules() {
    let (base, workspace) = fixture();
    let mut d = base.clone();
    d.routine = None;
    assert_eq!(
        routine_definition_digest(&d, workspace, None),
        Err(RoutineDigestError::Invalid)
    );
    d = base.clone();
    d.revision = 0;
    assert!(routine_definition_digest(&d, workspace, None).is_err());
    assert!(routine_definition_digest(&base, WorkspaceId::new(), None).is_err());
    assert!(
        routine_definition_digest(&base, workspace, Some(HandoffId::from_uuid(Uuid::nil())))
            .is_err()
    );
    d = base.clone();
    d.routine.as_mut().unwrap().owner.user_scope =
        serde_json::from_value(serde_json::json!(Uuid::nil())).unwrap();
    d.routine.as_mut().unwrap().owner.device = DeviceId::from_uuid(Uuid::nil());
    assert!(routine_definition_digest(&d, workspace, None).is_ok());
    d = base;
    d.template.context = vec![
        ScheduleContextPointer::Document {
            workspace,
            document: DocumentId::new()
        };
        40_000
    ];
    assert_eq!(
        routine_definition_digest(&d, workspace, None),
        Err(RoutineDigestError::TooLarge)
    );
}
