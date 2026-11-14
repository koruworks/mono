// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::*;
use koru_delegate_schedule::{DelegationTemplate, RecurrenceRule, RoutineAssociation};

#[test]
fn current_routine_definition_requires_actual_location_identity_active_owner_and_ancestry() {
    let owner = DeviceId::new();
    let home = WorkspaceId::new();
    let schedule = ScheduleId::mint();
    let definition = ScheduleDefinition {
        revision: 1,
        effective_at_ms: 0,
        template: DelegationTemplate {
            requested_by: ActorRef::Person {
                profile: ProfileId::new(),
            },
            objective: Objective::new("task").unwrap(),
            agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
            requested_model: None,
            scope: WorkspaceScope::Temporary,
            grants: [home].into(),
            context: vec![],
        },
        recurrence: RecurrenceRule::floating("0 9 * * *".parse().unwrap()),
        lifecycle: ScheduleLifecycle::Active,
        routine: Some(RoutineAssociation {
            owner: BotOwner {
                user_scope: koru_store_core::UserScopeId::new(),
                device: owner,
            },
            bot: BotId::mint(),
            home,
            creation_request: BotRequestId::mint(),
        }),
    };
    let doc = ScheduleDocument::create(&ScheduleHeader {
        schedule,
        created_at_ms: 0,
        initial_owner: owner,
        definition: definition.clone(),
    })
    .unwrap();
    let snapshot = doc.export_snapshot().unwrap();
    assert!(
        ScheduleDocument::verify_routine_occurrence_snapshot(
            &snapshot,
            schedule,
            &definition,
            owner,
            None
        )
        .is_ok()
    );
    assert!(
        ScheduleDocument::verify_routine_occurrence_snapshot(
            &snapshot,
            ScheduleId::mint(),
            &definition,
            owner,
            None
        )
        .is_err()
    );
    assert!(
        ScheduleDocument::verify_routine_occurrence_snapshot(
            &snapshot,
            schedule,
            &definition,
            DeviceId::new(),
            None
        )
        .is_err()
    );
    let mut changed = definition.clone();
    changed.revision += 1;
    assert!(
        ScheduleDocument::verify_routine_occurrence_snapshot(
            &snapshot, schedule, &changed, owner, None
        )
        .is_err()
    );
    let before = doc.version();
    doc.doc.get_map("extra").insert("later", true).unwrap();
    doc.doc.commit();
    let delta = doc.export_updates_since(&before).unwrap();
    assert!(
        ScheduleDocument::verify_routine_occurrence_snapshot(
            &delta,
            schedule,
            &definition,
            owner,
            None
        )
        .is_err()
    );
    assert!(
        ScheduleDocument::verify_routine_occurrence_snapshot(
            &vec![0; 16 * 1024 * 1024 + 1],
            schedule,
            &definition,
            owner,
            None
        )
        .is_err()
    );
    let newer = ScheduleDocument::from_snapshot(&snapshot).unwrap();
    newer.root().insert(K_SCHEMA, 99).unwrap();
    newer.doc.commit();
    assert!(
        ScheduleDocument::verify_routine_occurrence_snapshot(
            &newer.export_snapshot().unwrap(),
            schedule,
            &definition,
            owner,
            None
        )
        .is_err()
    );
    doc.retire(owner, 3).unwrap();
    assert!(
        ScheduleDocument::verify_routine_occurrence_snapshot(
            &doc.export_snapshot().unwrap(),
            schedule,
            &definition,
            owner,
            None
        )
        .is_err()
    );
}
