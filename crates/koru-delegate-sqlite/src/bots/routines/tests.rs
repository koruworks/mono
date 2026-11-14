// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::{BotLedger, BotSettlement, CrashPoint, Moment, SlotOutcome, fixtures};
use koru_delegate_core::{BotSubmission, RoutineDefinitionIdentity, RoutineProvenance};
use std::sync::{Arc, atomic::AtomicI64};

pub(crate) fn memory() -> RunsDb {
    RunsDb::in_memory(Box::new(fixtures::FakeClock(Arc::new(AtomicI64::new(
        1000,
    )))))
}
pub(crate) fn route(s: &BotSubmission) -> RoutineRouteIdentity {
    RoutineRouteIdentity {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        schedule: fixtures::schedule(0x91),
        workspace: s.workspace,
        creation_request: koru_delegate_core::BotRequestId::mint(),
        seed_hash: RoutineDigest32::from_bytes([4; 32]),
        created_at_ms: 800,
    }
}
pub(crate) fn routine(s: &mut BotSubmission, route: &RoutineRouteIdentity) {
    s.request.origin = RunOrigin::Scheduled {
        schedule: route.schedule,
        occurrence_ms: 900,
        revision: 1,
    };
    s.provenance.ai_attempt_id.get_or_insert(fixtures::v4(0xe1));
    s.provenance
        .assistant_message_id
        .get_or_insert(fixtures::v4(0xe2));
    s.provenance.routine = Some(RoutineProvenance {
        schedule: route.schedule,
        occurrence_ms: 900,
        definition: RoutineDefinitionIdentity {
            revision: 1,
            digest: RoutineDigest32::from_bytes([5; 32]),
            owner_handoff: None,
        },
        grant_revision: 1,
    });
    assert!(s.valid_for_version(2));
}
/// Test-only retained facts. There is deliberately no public v2 insertion API.
pub(crate) fn convert(
    db: &mut RunsDb,
    s: &mut BotSubmission,
    linked: bool,
) -> RoutineRouteIdentity {
    let r = route(s);
    db.register_routine_route(&r).unwrap();
    routine(s, &r);
    db.conn()
        .execute(
            "UPDATE bot_admissions SET submission_version=2,payload_json=?2 WHERE run_id=?1",
            params![
                s.request.run.to_storage(),
                serde_json::to_string(s).unwrap()
            ],
        )
        .unwrap();
    db.conn()
        .execute(
            "UPDATE run_intents SET payload_json=?2 WHERE run_id=?1",
            params![
                s.request.run.to_storage(),
                serde_json::to_string(&s.request).unwrap()
            ],
        )
        .unwrap();
    db.conn().execute("INSERT INTO schedule_claims(schedule_id,occurrence_ms,revision,run_id,outcome,claimed_at_ms) VALUES(?1,900,1,?2,'claimed',1000)",params![r.schedule.to_storage(),s.request.run.to_storage()]).unwrap();
    db.conn().execute("INSERT INTO bot_routine_links(run_id,schedule_id,occurrence_ms,user_scope,device_id,bot_id,definition_revision,definition_digest,grant_revision,preparation_hash,submission_request_id,chat_id,attempt_id,user_message_id,assistant_message_id,state,created_at_ms,updated_at_ms) VALUES(?1,?2,900,?3,?4,?5,1,?6,1,?7,?8,?9,?10,?11,?12,?13,1000,1000)", params![s.request.run.to_storage(),r.schedule.to_storage(),r.owner.user_scope.to_storage(),r.owner.device.to_storage(),r.bot.to_storage(),[5u8;32].as_slice(),[6u8;32].as_slice(),s.provenance.submission.to_storage(),s.provenance.chat_id.to_string(),s.provenance.ai_attempt_id.unwrap().to_string(),fixtures::v4(0xe3).to_string(),s.provenance.assistant_message_id.unwrap().to_string(),if linked {"linked"} else {"held"}]).unwrap();
    r
}

#[test]
fn routine_route_is_immutable_exact_and_survives_lost_commit_reply() {
    for at in Moment::ALL {
        let mut db = memory();
        let s = crate::bots::tests::submission(1);
        let r = route(&s);
        db.arm_crash(CrashPoint {
            op: WriteOp::RegisterRoutineRoute,
            at,
        });
        assert!(db.register_routine_route(&r).is_err());
        db.restart_in_memory();
        assert_eq!(
            db.routine_route(r.schedule).unwrap().is_some(),
            at == Moment::AfterCommit
        );
        assert_eq!(db.register_routine_route(&r).unwrap(), r);
        assert_eq!(db.register_routine_route(&r).unwrap(), r);
        for field in 0..7 {
            let mut changed = r.clone();
            match field {
                0 => changed.created_at_ms += 1,
                1 => changed.seed_hash = RoutineDigest32::from_bytes([8; 32]),
                2 => changed.creation_request = koru_delegate_core::BotRequestId::mint(),
                3 => changed.workspace = fixtures::workspace(9),
                4 => changed.bot = koru_delegate_core::BotId::mint(),
                5 => {
                    changed.owner.device = koru_delegate_core::DeviceId::from_uuid(fixtures::v4(8))
                }
                _ => {
                    changed.owner.user_scope =
                        serde_json::from_value(serde_json::json!(fixtures::v4(9))).unwrap()
                }
            }
            assert_eq!(
                db.register_routine_route(&changed),
                Err(BotLedgerError::Conflict)
            );
        }
        assert_eq!(db.routine_route(r.schedule).unwrap(), Some(r));
    }
}

#[test]
fn retained_routine_route_denies_every_ordinary_claim_and_preserves_reads() {
    let mut db = memory();
    let mut s = crate::bots::tests::submission(1);
    let r = route(&s);
    routine(&mut s, &r);
    db.record_intent(&s.request, s.workspace).unwrap();
    db.register_routine_route(&r).unwrap();
    assert!(db.intent(s.request.run).unwrap().is_some());
    assert!(
        db.validate_intent(&s.request, s.workspace)
            .unwrap()
            .is_some()
    );
    assert!(db.pending_intents().unwrap().is_empty());
    db.satisfy_intent(s.request.run, fixtures::document(0xd0))
        .unwrap();
    assert_eq!(
        db.open_attempt(s.request.run),
        Err(LedgerError::InvalidScheduleAuthority)
    );
    let mut fresh = s.request.clone();
    fresh.run = fixtures::run_id(2);
    assert_eq!(
        db.record_intent(&fresh, s.workspace),
        Err(LedgerError::InvalidScheduleAuthority)
    );
    assert!(matches!(
        db.claim_slot(r.schedule, 900, 1),
        Err(LedgerError::InvalidScheduleAuthority)
    ));
    assert!(matches!(
        db.claim_scheduled_run_at(r.schedule, 900, 1, &fresh, s.workspace, 1000),
        Err(LedgerError::InvalidScheduleAuthority)
    ));
    assert!(matches!(
        db.claim_scheduled_run_with_missed_at(
            r.schedule,
            900,
            1,
            &fresh,
            s.workspace,
            800,
            fixtures::run_id(3),
            1000
        ),
        Err(LedgerError::InvalidScheduleAuthority)
    ));
    assert!(matches!(
        db.claim_skip_at(
            r.schedule,
            900,
            1,
            fresh.run,
            SlotOutcome::SkippedMissed,
            None,
            1000
        ),
        Err(LedgerError::InvalidScheduleAuthority)
    ));
    assert!(matches!(
        db.claim_skip_with_missed_at(
            r.schedule,
            900,
            1,
            fresh.run,
            SlotOutcome::SkippedMissed,
            None,
            800,
            fixtures::run_id(3),
            1000
        ),
        Err(LedgerError::InvalidScheduleAuthority)
    ));
    assert_eq!(
        db.conn()
            .query_row("SELECT count(*) FROM schedule_claims", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    db.conn()
        .execute("UPDATE bot_routine_routes SET bot_id='corrupt'", [])
        .unwrap();
    assert!(db.routine_route(r.schedule).is_err());
    assert!(matches!(
        db.claim_slot(r.schedule, 900, 1),
        Err(LedgerError::InvalidScheduleAuthority)
    ));
}

#[test]
fn routine_admission_version_decoder_rejects_mixed_or_unknown_identity() {
    let mut db = memory();
    let mut s = crate::bots::tests::submission(1);
    assert_eq!(db.admit_bot(&s).unwrap().submission_version, 1);
    let r = convert(&mut db, &mut s, false);
    assert_eq!(
        db.bot_admission(s.request.run)
            .unwrap()
            .unwrap()
            .submission_version,
        2
    );
    for case in 0..5 {
        let mut bad = s.clone();
        match case {
            0 => bad.provenance.routine = None,
            1 => bad.request.origin = RunOrigin::Manual,
            2 => bad.provenance.routine.as_mut().unwrap().occurrence_ms += 1,
            3 => bad.provenance.routine.as_mut().unwrap().definition.revision += 1,
            _ => bad.provenance.bot = koru_delegate_core::BotId::mint(),
        }
        db.conn()
            .execute(
                "UPDATE bot_admissions SET payload_json=?1",
                [serde_json::to_string(&bad).unwrap()],
            )
            .unwrap();
        assert!(db.bot_admission(s.request.run).is_err());
    }
    db.conn()
        .execute(
            "UPDATE bot_admissions SET payload_json=?1,submission_version=1",
            [serde_json::to_string(&s).unwrap()],
        )
        .unwrap();
    assert!(db.bot_admission(s.request.run).is_err());
    db.conn()
        .execute_batch(
            "PRAGMA ignore_check_constraints=ON; UPDATE bot_admissions SET submission_version=3;",
        )
        .unwrap();
    assert!(db.bot_admission(s.request.run).is_err());
    assert_eq!(db.routine_route(r.schedule).unwrap(), Some(r));
}

#[test]
fn routine_route_decoder_bounds_malformed_cells_without_erasing_denial() {
    let mut db = memory();
    let s = crate::bots::tests::submission(1);
    let r = route(&s);
    db.register_routine_route(&r).unwrap();
    for column in [
        "user_scope",
        "device_id",
        "bot_id",
        "workspace_id",
        "creation_request_id",
    ] {
        let mut db = memory();
        db.register_routine_route(&r).unwrap();
        db.conn()
            .execute(
                &format!("UPDATE bot_routine_routes SET {column}=hex(zeroblob(1048576))"),
                [],
            )
            .unwrap();
        assert!(db.routine_route(r.schedule).is_err());
        assert!(matches!(
            db.claim_slot(r.schedule, 900, 1),
            Err(LedgerError::InvalidScheduleAuthority)
        ));
    }
    db.conn().execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE bot_routine_routes SET seed_hash=zeroblob(1048576);").unwrap();
    assert!(db.routine_route(r.schedule).is_err());
    assert!(matches!(
        db.claim_slot(r.schedule, 900, 1),
        Err(LedgerError::InvalidScheduleAuthority)
    ));
}

#[test]
fn routine_linked_fixture_without_current_proof_refuses_dispatch_but_history_survives() {
    let mut db = memory();
    let mut s = crate::bots::tests::submission(1);
    // The older ordinary fixture reused its Run UUID for request correlation;
    // a complete Routine winner requires distinct preallocated execution IDs.
    s.provenance.submission = koru_delegate_core::BotRequestId::mint();
    let r = route(&s);
    let mut fresh = s.clone();
    routine(&mut fresh, &r);
    assert_eq!(db.admit_bot(&fresh), Err(BotLedgerError::Fenced));
    assert!(db.intent(s.request.run).unwrap().is_none());
    db.admit_bot(&s).unwrap();
    convert(&mut db, &mut s, true);
    let retained = db.bot_admission(s.request.run).unwrap().unwrap();
    assert_eq!(db.admit_bot(&s).unwrap(), retained);
    assert_eq!(db.queued_bots(0, 10).unwrap().len(), 1);
    assert!(db.pending_intents().unwrap().is_empty());
    assert_eq!(
        db.reserve_bot(s.request.run, None),
        Err(BotLedgerError::Fenced)
    );
    db.satisfy_intent(s.request.run, fixtures::document(0xd0))
        .unwrap();
    assert_eq!(
        db.open_attempt(s.request.run),
        Err(LedgerError::InvalidScheduleAuthority)
    );
    db.conn()
        .execute("UPDATE bot_admissions SET state='reserved'", [])
        .unwrap();
    assert!(
        !db.recover_bot_reservation(&koru_delegate_core::BotRunKey {
            owner: s.provenance.owner,
            bot: s.provenance.bot,
            run: s.request.run
        })
        .unwrap()
    );
    assert_eq!(
        db.reserve_bot(s.request.run, None),
        Err(BotLedgerError::Fenced)
    );
    db.settle_bot(s.request.run, BotSettlement::Refused)
        .unwrap();
    assert!(db.bot_admission(s.request.run).unwrap().is_some());
    assert!(db.routine_route(r.schedule).unwrap().is_some());
}
