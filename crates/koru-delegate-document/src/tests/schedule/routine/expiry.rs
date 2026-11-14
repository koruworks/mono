// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::{RoutineExpiryAuthority, RoutineExpiryPlan};

fn active() -> ScheduleDocument {
    let doc = ScheduleDocument::create(&routine_header()).unwrap();
    doc.set_lifecycle(device(5), 1, ScheduleLifecycle::Active, 2000)
        .unwrap();
    doc
}
fn plan() -> RoutineExpiryPlan {
    RoutineExpiryPlan {
        occurrence_ms: 60_000,
        planned_run: RunId::mint(),
        representative_miss: None,
    }
}
fn token(doc: &ScheduleDocument, plan: RoutineExpiryPlan) -> RoutineExpiryAuthority {
    doc.expired_routine_authority(association().home, device(5), plan)
        .unwrap()
        .unwrap()
}

#[test]
fn routine_expiry_document_derives_full_subject_without_preparation_and_preserves_ordinary_fence() {
    let doc = active();
    let p = plan();
    let facts = token(&doc, p).into_expired_bot_routine().unwrap();
    let header = doc.header().unwrap();
    assert_eq!(facts.slot().schedule, header.schedule);
    assert_eq!(facts.slot().owner, association().owner);
    assert_eq!(facts.slot().bot, association().bot);
    assert_eq!(facts.workspace(), association().home);
    assert_eq!(facts.creation_request(), association().creation_request);
    assert_eq!(
        facts.definition().digest,
        routine_definition_digest(&header.definition, association().home, None).unwrap()
    );
    assert_eq!(facts.planned_run(), p.planned_run);
    assert!(
        doc.claim_authority(
            association().home,
            device(5),
            skip_plan(60_000, RunId::mint())
        )
        .is_err()
    );
    assert!(
        schedule()
            .expired_routine_authority(association().home, device(5), plan())
            .is_err()
    );
}

#[test]
fn routine_expiry_document_revalidates_live_import_owner_definition_retirement_and_schema() {
    for field in 0..9 {
        let doc = active();
        let authority = token(&doc, plan());
        let since = doc.version();
        let peer = ScheduleDocument::from_snapshot(&doc.export_snapshot().unwrap()).unwrap();
        match field {
            6 => {
                peer.retire(device(5), 2100).unwrap();
            }
            7 => {
                peer.doc()
                    .get_map("schedule")
                    .insert("schema", 3i64)
                    .unwrap();
                peer.doc().commit();
            }
            8 => {
                let id = HandoffId::mint();
                peer.request_handoff(id, device(6), device(6), 2100)
                    .unwrap();
                peer.confirm_handoff(id, device(5), 2200).unwrap();
            }
            _ => {
                let mut definition = peer.header().unwrap().definition;
                match field {
                    0 => definition.template.objective = Objective::new("changed").unwrap(),
                    1 => definition.routine.as_mut().unwrap().bot = BotId::mint(),
                    2 => definition.routine.as_mut().unwrap().home = WorkspaceId::from_uuid(v4(90)),
                    3 => definition.routine.as_mut().unwrap().owner.device = device(6),
                    4 => {
                        definition.routine.as_mut().unwrap().creation_request = BotRequestId::mint()
                    }
                    _ => {
                        definition.lifecycle = ScheduleLifecycle::Paused {
                            reason: SchedulePauseReason::Manual,
                        }
                    }
                }
                write_definition(&peer, &definition);
            }
        }
        doc.import(&peer.export_updates_since(&since).unwrap())
            .unwrap();
        assert!(
            authority.into_expired_bot_routine().is_err(),
            "field{field}"
        );
    }
}

#[test]
fn routine_expiry_document_binds_effective_time_ids_home_and_owner_but_not_name_version() {
    let doc = active();
    for change in ["early", "nil", "same_id", "late_miss", "early_miss"] {
        let mut p = plan();
        match change {
            "early" => p.occurrence_ms = 1999,
            "nil" => p.planned_run = RunId::from_uuid(uuid::Uuid::nil()),
            "same_id" => {
                p.representative_miss = Some(RoutineRepresentativeMiss {
                    occurrence_ms: 3000,
                    planned_run: p.planned_run,
                })
            }
            "late_miss" => {
                p.representative_miss = Some(RoutineRepresentativeMiss {
                    occurrence_ms: p.occurrence_ms,
                    planned_run: RunId::mint(),
                })
            }
            _ => {
                p.representative_miss = Some(RoutineRepresentativeMiss {
                    occurrence_ms: 1999,
                    planned_run: RunId::mint(),
                })
            }
        }
        assert!(
            doc.expired_routine_authority(association().home, device(5), p)
                .is_err(),
            "{change}"
        );
    }
    assert!(
        doc.expired_routine_authority(WorkspaceId::from_uuid(v4(90)), device(5), plan())
            .is_err()
    );
    assert!(
        doc.expired_routine_authority(association().home, device(6), plan())
            .is_err()
    );
    let authority = token(&doc, plan());
    doc.rename(
        device(5),
        &doc.name().unwrap(),
        &ScheduleName::new("renamed").unwrap(),
    )
    .unwrap();
    assert!(authority.into_expired_bot_routine().is_ok());
}
