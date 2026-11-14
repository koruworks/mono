// SPDX-License-Identifier: MPL-2.0

use super::*;
mod claim;

fn association() -> RoutineAssociation {
    RoutineAssociation::new(
        BotOwner {
            user_scope: koru_store_core::UserScopeId::from_uuid(v4(9)),
            device: device(5),
        },
        BotId::from_uuid(v4(10)),
        WorkspaceId::from_uuid(v4(2)),
        BotRequestId::from_uuid(v4(11)),
    )
    .unwrap()
}

fn routine_header() -> ScheduleHeader {
    let mut header = schedule().header().unwrap();
    header.definition.routine = Some(association());
    header.definition.lifecycle = ScheduleLifecycle::Paused {
        reason: SchedulePauseReason::Manual,
    };
    header
}

fn write_definition(doc: &ScheduleDocument, definition: &ScheduleDefinition) {
    doc.doc()
        .get_map("schedule")
        .insert(
            "definition",
            LoroValue::from(serde_json::to_value(definition).unwrap()),
        )
        .unwrap();
    doc.doc().commit();
}

#[test]
fn routine_definition_rejects_unknown_top_level_fields_without_changing_legacy_decode() {
    for doc in [
        schedule(),
        ScheduleDocument::create(&routine_header()).unwrap(),
    ] {
        let mut definition = serde_json::to_value(doc.header().unwrap().definition).unwrap();
        definition["future_execution"] = serde_json::json!(true);
        doc.doc()
            .get_map("schedule")
            .insert("definition", LoroValue::from(definition))
            .unwrap();
        doc.doc().commit();
        let before = doc.version();
        if matches!(
            doc.doc().get_map("schedule").get("schema"),
            Some(ValueOrContainer::Value(LoroValue::I64(1)))
        ) {
            assert!(doc.header().is_ok());
            assert!(doc.is_writable());
        } else {
            assert!(matches!(
                doc.header(),
                Err(ScheduleDocumentError::Malformed("definition"))
            ));
            assert!(!doc.is_writable());
            let name = ScheduleName::new("Changed").unwrap();
            assert!(doc.rename(device(5), &name, &name).is_err());
        }
        assert_eq!(doc.version(), before);
    }
}

#[test]
fn routine_initialization_is_paused_schema_two_and_ordinary_remains_one() {
    let ordinary = schedule();
    assert!(matches!(
        ordinary.doc().get_map("schedule").get("schema"),
        Some(ValueOrContainer::Value(LoroValue::I64(1)))
    ));
    assert_eq!(SCHEDULE_DOCUMENT_SCHEMA_VERSION, 2);
    let header = routine_header();
    let routine = ScheduleDocument::create(&header).unwrap();
    assert!(matches!(
        routine.doc().get_map("schedule").get("schema"),
        Some(ValueOrContainer::Value(LoroValue::I64(2)))
    ));
    let reopened = ScheduleDocument::from_snapshot(&routine.export_snapshot().unwrap()).unwrap();
    assert_eq!(reopened.header().unwrap(), header);
    assert!(reopened.is_writable());

    let mut active = header.clone();
    active.definition.lifecycle = ScheduleLifecycle::Active;
    let blank = loro::LoroDoc::new();
    let before = blank.oplog_vv();
    assert!(matches!(
        ScheduleDocument::initialize(blank.clone(), &active),
        Err(ScheduleDocumentError::LifecycleConflict)
    ));
    assert_eq!(blank.oplog_vv(), before);
    let mut wrong_owner = header;
    wrong_owner.initial_owner = device(6);
    assert!(matches!(
        ScheduleDocument::create(&wrong_owner),
        Err(ScheduleDocumentError::Malformed("routine"))
    ));
}

#[test]
fn routine_schema_and_association_must_agree_before_any_write() {
    for (schema, routine) in [(1, Some(association())), (2, None), (0, None)] {
        let doc = schedule();
        let mut definition = doc.header().unwrap().definition;
        definition.routine = routine;
        write_definition(&doc, &definition);
        doc.doc()
            .get_map("schedule")
            .insert("schema", schema)
            .unwrap();
        doc.doc().commit();
        let reopened = ScheduleDocument::from_snapshot(&doc.export_snapshot().unwrap()).unwrap();
        let before = reopened.version();
        assert!(matches!(
            reopened.header(),
            Err(ScheduleDocumentError::Malformed("routine_schema"))
        ));
        assert!(!reopened.is_writable());
        assert!(
            reopened
                .rename(
                    device(5),
                    &ScheduleName::new("old").unwrap(),
                    &ScheduleName::new("new").unwrap()
                )
                .is_err()
        );
        assert_eq!(reopened.version(), before);
    }
}

#[test]
fn routine_association_cannot_be_added_removed_or_substituted() {
    let routine = ScheduleDocument::create(&routine_header()).unwrap();
    let ordinary = schedule();
    let mut removed = routine.header().unwrap().definition;
    removed.routine = None;
    let mut substituted = routine.header().unwrap().definition;
    substituted.routine.as_mut().unwrap().bot = BotId::from_uuid(v4(12));
    let mut added = ordinary.header().unwrap().definition;
    added.routine = Some(association());
    for (doc, mut offered) in [
        (&routine, removed),
        (&routine, substituted),
        (&ordinary, added),
    ] {
        offered.revision += 1;
        offered.effective_at_ms += 1;
        let before = doc.version();
        assert_eq!(
            doc.update_definition(device(5), &offered),
            Err(ScheduleDocumentError::RoutineAssociationChanged)
        );
        let name = doc.name().unwrap();
        assert_eq!(
            doc.edit(device(5), 1, &name, &name, &offered),
            Err(ScheduleDocumentError::RoutineAssociationChanged)
        );
        assert_eq!(doc.version(), before);
    }
}

#[test]
fn routine_template_authority_is_immutable_but_task_content_can_change() {
    let routine = ScheduleDocument::create(&routine_header()).unwrap();
    let original = routine.header().unwrap().definition;
    for field in 0..5 {
        let mut offered = original.clone();
        offered.revision = 2;
        offered.effective_at_ms += 1;
        match field {
            0 => {
                offered.template.requested_by = ActorRef::Person {
                    profile: ProfileId::from_uuid(v4(13)),
                }
            }
            1 => offered.template.agent = SessionProfileKey::BuiltIn("claude".parse().unwrap()),
            2 => offered.template.requested_model = None,
            3 => offered.template.scope = WorkspaceScope::None,
            _ => offered.template.grants.clear(),
        }
        let before = routine.version();
        assert_eq!(
            routine.update_definition(device(5), &offered),
            Err(ScheduleDocumentError::RoutineTemplateChanged)
        );
        let name = routine.name().unwrap();
        assert_eq!(
            routine.edit(device(5), 1, &name, &name, &offered),
            Err(ScheduleDocumentError::RoutineTemplateChanged)
        );
        assert_eq!(routine.version(), before);
    }
    let mut offered = original;
    offered.revision = 2;
    offered.effective_at_ms += 1;
    offered.template.objective = Objective::new("A changed task").unwrap();
    offered.template.context.clear();
    routine.update_definition(device(5), &offered).unwrap();
    assert_eq!(routine.header().unwrap().definition, offered);
}

#[test]
fn routine_lifecycle_preserves_association_and_ordinary_claims_refuse() {
    let routine = ScheduleDocument::create(&routine_header()).unwrap();
    for plan in [
        run_plan(60_000, RunId::from_uuid(v4(20))),
        skip_plan(60_000, RunId::from_uuid(v4(21))),
    ] {
        assert!(matches!(
            routine.claim_authority(association().home, device(5), plan),
            Err(ScheduleDocumentError::RoutineRequiresBotClaim)
        ));
    }
    routine
        .set_lifecycle(device(5), 1, ScheduleLifecycle::Active, 2_000)
        .unwrap();
    assert_eq!(
        routine.header().unwrap().definition.routine,
        Some(association())
    );
    assert!(matches!(
        routine.claim_authority(
            association().home,
            device(5),
            run_plan(60_000, RunId::from_uuid(v4(20)))
        ),
        Err(ScheduleDocumentError::RoutineRequiresBotClaim)
    ));
    routine
        .set_lifecycle(
            device(5),
            2,
            ScheduleLifecycle::Paused {
                reason: SchedulePauseReason::Manual,
            },
            3_000,
        )
        .unwrap();
    assert_eq!(
        routine.header().unwrap().definition.routine,
        Some(association())
    );
}

#[test]
fn routine_import_invalidates_a_previously_minted_ordinary_run_authority() {
    let ordinary = schedule();
    let authority = ordinary
        .claim_authority(
            association().home,
            device(5),
            run_plan(60_000, RunId::from_uuid(v4(20))),
        )
        .unwrap()
        .unwrap();
    let mut changed = ordinary.header().unwrap().definition;
    changed.routine = Some(association());
    write_definition(&ordinary, &changed);
    ordinary
        .doc()
        .get_map("schedule")
        .insert("schema", 2)
        .unwrap();
    ordinary.doc().commit();
    assert!(matches!(
        authority.into_run(),
        Err(ScheduleDocumentError::StaleClaimAuthority)
    ));
}
