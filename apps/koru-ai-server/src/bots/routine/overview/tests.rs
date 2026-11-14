// SPDX-License-Identifier: MPL-2.0
use super::super::fixture::Fixture;
use super::*;
use koru_ai_core::{DocumentId, Timestamp};
use koru_delegate_core::{ActorRef, HandoffId};

fn read(f: &Fixture) -> Result<RoutineOverviewObservation, BotError> {
    f.adapter.read_routine_overview(
        f.owner,
        f.profile,
        &f.creation.identity,
        &Cancellation::default(),
    )
}
fn update(f: &Fixture, change: impl FnOnce(&ScheduleDocument) + Send + 'static) {
    let id = f.creation.identity.clone();
    f.memory
        .with_store(move |store| {
            let candidate = DocumentId::from_uuid(id.schedule.as_uuid());
            let doc =
                ScheduleDocument::adopt(store.fork_document(id.home, candidate).unwrap()).unwrap();
            let before = doc.version();
            change(&doc);
            doc.doc().commit();
            store
                .apply_update(
                    id.home,
                    candidate,
                    doc.export_updates_since(&before).unwrap(),
                    Timestamp::from_millis(8),
                    None,
                )
                .unwrap();
            store
                .flush(id.home, candidate, Timestamp::from_millis(8))
                .unwrap();
        })
        .unwrap();
}
fn state(f: &Fixture) -> String {
    let RoutineOverviewObservation::Current { state, .. } = read(f).unwrap() else {
        panic!("actual current projection");
    };
    serde_json::to_value(state).unwrap()["state"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn routine_overview_store_not_live_precedes_origin_and_never_uses_header_alias() {
    let (f, _) = Fixture::new();
    assert_eq!(read(&f).unwrap(), RoutineOverviewObservation::NotLive);
    let id = f.creation.identity.clone();
    f.memory
        .with_store(move |store| {
            store
                .create_seeded_document_with_origin(
                    id.home,
                    id.seed.name.as_str(),
                    SCHEDULE_KIND,
                    None,
                    None,
                    BOT_ROUTINE_SCOPE,
                    &id.store_request.to_string(),
                    &String::from(id.seed_hash),
                    DocumentId::new(),
                    ScheduleDocument::routine_seed_update(&id.seed.header, &id.seed.name).unwrap(),
                    None,
                    Timestamp::from_millis(3),
                )
                .unwrap();
        })
        .unwrap();
    assert_eq!(read(&f).unwrap(), RoutineOverviewObservation::NotLive);
}

#[test]
fn routine_overview_store_reads_current_name_revision_and_all_lifecycles() {
    let (f, _) = Fixture::new();
    f.prepare().unwrap();
    assert_eq!(state(&f), "paused");
    let owner = f.owner.device;
    let next = DeviceId::new();
    let handoff = HandoffId::mint();
    update(&f, move |doc| {
        let mut definition = doc.header().unwrap().definition;
        definition.revision += 1;
        definition.effective_at_ms += 1;
        definition.lifecycle = serde_json::from_str("\"active\"").unwrap();
        doc.update_definition(owner, &definition).unwrap();
        doc.rename(
            owner,
            &doc.name().unwrap(),
            &serde_json::from_str("\"Edited label\"").unwrap(),
        )
        .unwrap();
    });
    assert_eq!(state(&f), "active");
    assert!(
        matches!(read(&f).unwrap(), RoutineOverviewObservation::Current { name, definition_revision: 2, .. }
        if name.as_str() == "Edited label")
    );
    update(&f, move |doc| {
        doc.request_handoff(handoff, next, next, 4).unwrap();
    });
    assert_eq!(state(&f), "handoff_pending");
    update(&f, move |doc| {
        doc.confirm_handoff(handoff, owner, 5).unwrap();
        doc.retire(next, 6).unwrap();
    });
    assert_eq!(state(&f), "retired");
}

#[test]
fn routine_overview_store_direct_and_inherited_trash_are_not_live() {
    for inherited in [false, true] {
        let (f, _) = Fixture::new();
        let id = f.creation.identity.clone();
        f.memory
            .with_store(move |store| {
                let at = Timestamp::from_millis(3);
                let parent = inherited.then(|| {
                    store
                        .create_document(id.home, "Parent", "folder", None, None, at)
                        .unwrap()
                        .0
                });
                let candidate = DocumentId::from_uuid(id.schedule.as_uuid());
                store
                    .create_seeded_document_with_origin(
                        id.home,
                        id.seed.name.as_str(),
                        SCHEDULE_KIND,
                        parent,
                        None,
                        BOT_ROUTINE_SCOPE,
                        &id.store_request.to_string(),
                        &String::from(id.seed_hash),
                        candidate,
                        ScheduleDocument::routine_seed_update(&id.seed.header, &id.seed.name)
                            .unwrap(),
                        None,
                        at,
                    )
                    .unwrap();
                store
                    .trash_document_entry(id.home, parent.unwrap_or(candidate), at, None)
                    .unwrap();
            })
            .unwrap();
        assert_eq!(read(&f).unwrap(), RoutineOverviewObservation::NotLive);
    }
}

#[test]
fn routine_overview_store_unknown_origin_and_wrong_origin_or_kind_are_unavailable() {
    for mutation in 0..4 {
        let (mut f, _) = Fixture::new();
        let id = f.creation.identity.clone();
        let actual = f
            .memory
            .with_store(move |store| {
                let at = Timestamp::from_millis(3);
                if mutation == 0 {
                    return store
                        .create_document(
                            id.home,
                            "No retained origin",
                            SCHEDULE_KIND,
                            None,
                            None,
                            at,
                        )
                        .unwrap()
                        .0;
                }
                let candidate = DocumentId::from_uuid(id.schedule.as_uuid());
                store
                    .create_seeded_document_with_origin(
                        id.home,
                        id.seed.name.as_str(),
                        if mutation == 1 {
                            "document"
                        } else {
                            SCHEDULE_KIND
                        },
                        None,
                        None,
                        BOT_ROUTINE_SCOPE,
                        &if mutation == 2 {
                            uuid::Uuid::new_v4().to_string()
                        } else {
                            id.store_request.to_string()
                        },
                        &String::from(id.seed_hash),
                        candidate,
                        ScheduleDocument::routine_seed_update(
                            &id.seed.header,
                            &if mutation == 3 {
                                serde_json::from_str("\"Wrong original\"").unwrap()
                            } else {
                                id.seed.name.clone()
                            },
                        )
                        .unwrap(),
                        None,
                        at,
                    )
                    .unwrap();
                candidate
            })
            .unwrap();
        if mutation == 0 {
            f.creation.identity.schedule = actual.as_uuid().to_string().parse().unwrap();
            f.creation.identity.seed.header.schedule = f.creation.identity.schedule;
            f.creation.identity.seed_hash = f.creation.identity.seed.canonical_digest().unwrap();
        }
        assert_eq!(
            read(&f).unwrap(),
            RoutineOverviewObservation::Unavailable,
            "mutation {mutation}"
        );
    }
}

#[test]
fn routine_overview_store_rejects_foreign_current_identity_profile_and_schema() {
    for mutation in 0..6 {
        let (f, _) = Fixture::new();
        f.prepare().unwrap();
        let other = f.other;
        update(&f, move |doc| {
            let mut header = doc.header().unwrap();
            match mutation {
                0 => {
                    doc.doc().get_map("schedule").insert("schema", 3).unwrap();
                }
                1 => {
                    doc.doc()
                        .get_map("schedule")
                        .insert(
                            "id",
                            serde_json::to_value(koru_delegate_core::ScheduleId::mint()).unwrap(),
                        )
                        .unwrap();
                }
                2 => {
                    header.definition.template.requested_by = ActorRef::Person { profile: other };
                    doc.doc()
                        .get_map("schedule")
                        .insert(
                            "definition",
                            loro::LoroValue::from(serde_json::to_value(header.definition).unwrap()),
                        )
                        .unwrap();
                }
                3 => {
                    header.definition.routine.as_mut().unwrap().bot = BotId::mint();
                    doc.doc()
                        .get_map("schedule")
                        .insert(
                            "definition",
                            loro::LoroValue::from(serde_json::to_value(header.definition).unwrap()),
                        )
                        .unwrap();
                }
                4 => {
                    doc.doc()
                        .get_map("schedule")
                        .insert("created_at_ms", 0)
                        .unwrap();
                }
                _ => {
                    doc.doc()
                        .get_map("schedule")
                        .insert("name", "x".repeat(4 * 1024 * 1024 + 1))
                        .unwrap();
                }
            }
        });
        assert_eq!(
            read(&f).unwrap(),
            RoutineOverviewObservation::Unavailable,
            "mutation {mutation}"
        );
    }
}

#[test]
fn routine_overview_store_shared_authority_failure_is_not_an_item_observation() {
    let (f, _) = Fixture::new();
    f.prepare().unwrap();
    assert!(matches!(
        f.adapter.read_routine_overview(
            f.owner,
            f.other,
            &f.creation.identity,
            &Cancellation::default()
        ),
        Err(BotError::Forbidden)
    ));
    let mut foreign = f.owner;
    foreign.device = DeviceId::new();
    assert!(matches!(
        f.adapter.read_routine_overview(
            foreign,
            f.profile,
            &f.creation.identity,
            &Cancellation::default()
        ),
        Err(BotError::Forbidden)
    ));
    let home = f.creation.identity.home;
    let other = f.other;
    f.memory
        .with_store(move |store| store.move_workspace(home, other).unwrap())
        .unwrap();
    assert!(matches!(read(&f), Err(BotError::Forbidden)));
}

#[test]
fn routine_overview_finish_rechecks_profile_even_after_not_live_or_item_refusal() {
    for variant in 0..3 {
        let (f, _) = Fixture::new();
        let projected = match variant {
            0 => RoutineOverviewObservation::NotLive,
            1 => RoutineOverviewObservation::Unavailable,
            _ => {
                f.prepare().unwrap();
                read(&f).unwrap()
            }
        };
        let home = f.creation.identity.home;
        let other = f.other;
        // Deterministic continuation-owner check on the actual Store. This is
        // the production final guard, not a scheduling hook in the adapter.
        f.memory
            .with_store(move |store| store.move_workspace(home, other).unwrap())
            .unwrap();
        assert!(matches!(
            finish(
                &f.adapter,
                f.owner,
                f.profile,
                home,
                &Cancellation::default(),
                Ok(projected)
            ),
            Err(BotError::Forbidden)
        ));
    }
}

#[test]
fn routine_overview_store_cancellation_before_and_after_projection_refuses() {
    let (f, _) = Fixture::new();
    let cancel = Cancellation::default();
    cancel.cancel();
    assert!(matches!(
        f.adapter
            .read_routine_overview(f.owner, f.profile, &f.creation.identity, &cancel),
        Err(BotError::Unavailable)
    ));
    f.prepare().unwrap();
    let actual = read(&f).unwrap();
    assert!(matches!(
        finish(
            &f.adapter,
            f.owner,
            f.profile,
            f.creation.identity.home,
            &cancel,
            Ok(actual)
        ),
        Err(BotError::Unavailable)
    ));
}
