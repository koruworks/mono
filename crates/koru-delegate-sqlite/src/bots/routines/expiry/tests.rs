// SPDX-License-Identifier: MPL-2.0
//! SQLite-only transaction evidence; actual live authority is tested by runtime.
use super::*;
use crate::{BotLedger, BotSettlement, CrashPoint, Moment, fixtures};
use std::sync::{
    Arc,
    atomic::{AtomicI64, AtomicUsize, Ordering},
};

struct Fixture {
    db: RunsDb,
    route: RoutineRouteIdentity,
    clock: Arc<AtomicI64>,
}
impl Fixture {
    fn new() -> Self {
        let clock = Arc::new(AtomicI64::new(60_900));
        let mut db = RunsDb::in_memory(Box::new(fixtures::FakeClock(clock.clone())));
        let route = super::super::tests::route(&crate::bots::tests::submission(1));
        db.register_routine_route(&route).unwrap();
        Self { db, route, clock }
    }
    fn slot(&self, at: i64) -> RoutineSlot {
        RoutineSlot {
            owner: self.route.owner,
            bot: self.route.bot,
            schedule: self.route.schedule,
            occurrence_ms: at,
        }
    }
    fn facts(
        &self,
        at: i64,
        run: RunId,
        miss: Option<RoutineRepresentativeMiss>,
    ) -> RoutineExpiredClaimFacts {
        RoutineExpiredClaimFacts::from_consumed_authority(
            self.slot(at),
            RoutineDefinitionIdentity {
                revision: 1,
                digest: RoutineDigest32::from_bytes([5; 32]),
                owner_handoff: None,
            },
            self.route.workspace,
            self.route.creation_request,
            run,
            miss,
        )
        .unwrap()
    }
    fn count(&self, table: &str) -> i64 {
        self.db
            .conn()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn no_execution(&self) {
        for table in ["run_intents", "bot_admissions", "bot_routine_links"] {
            assert_eq!(self.count(table), 0, "{table}");
        }
    }
}
fn miss() -> RoutineRepresentativeMiss {
    RoutineRepresentativeMiss {
        occurrence_ms: 100,
        planned_run: RunId::from_uuid(fixtures::v4(0xd8)),
    }
}

#[test]
fn routine_expiry_clock_boundary_rollback_and_overflow_never_create_execution() {
    for (at, now, accept) in [
        (900, 899, false),
        (900, 60_899, false),
        (900, 60_900, true),
        (i64::MIN, i64::MAX, false),
    ] {
        let mut f = Fixture::new();
        f.clock.store(now, Ordering::SeqCst);
        let facts = f.facts(at, RunId::mint(), None);
        let result = f.db.record_expired_routine(facts);
        assert_eq!(result.is_ok(), accept);
        assert_eq!(f.count("schedule_claims"), i64::from(accept));
        f.no_execution();
    }
}

#[test]
fn routine_expiry_retained_skip_wins_before_new_clock_ids_or_representative() {
    let mut f = Fixture::new();
    let run = RunId::mint();
    let facts = f.facts(900, run, None);
    let first = f.db.record_expired_routine(facts).unwrap();
    f.clock.store(-1, Ordering::SeqCst);
    let facts = f.facts(900, RunId::mint(), Some(miss()));
    assert_eq!(f.db.record_expired_routine(facts).unwrap(), first);
    assert_eq!(f.count("schedule_claims"), 1);
    f.no_execution();
}

#[test]
fn routine_expiry_requires_registered_subject_and_refuses_corrupt_retained_rows() {
    for changed in ["owner", "bot", "workspace", "creation", "corrupt"] {
        let mut f = Fixture::new();
        let run = RunId::mint();
        let facts = f.facts(900, run, None);
        f.db.record_expired_routine(facts).unwrap();
        let mut slot = f.slot(900);
        let mut workspace = f.route.workspace;
        let mut creation = f.route.creation_request;
        match changed {
            "owner" => slot.owner.device = DeviceId::from_uuid(fixtures::v4(90)),
            "bot" => slot.bot = BotId::mint(),
            "workspace" => workspace = WorkspaceId::from_uuid(fixtures::v4(90)),
            "creation" => creation = BotRequestId::mint(),
            _ => {
                f.db.conn()
                    .execute_batch("PRAGMA ignore_check_constraints=ON")
                    .unwrap();
                f.db.conn()
                    .execute(
                        "UPDATE schedule_claims SET busy_holder_run_id=?1 WHERE schedule_id=?2",
                        params![RunId::mint().to_storage(), f.route.schedule.to_storage()],
                    )
                    .unwrap();
            }
        }
        let facts = RoutineExpiredClaimFacts::from_consumed_authority(
            slot,
            RoutineDefinitionIdentity {
                revision: 1,
                digest: RoutineDigest32::from_bytes([5; 32]),
                owner_handoff: None,
            },
            workspace,
            creation,
            RunId::mint(),
            None,
        )
        .unwrap();
        assert!(f.db.record_expired_routine(facts).is_err(), "{changed}");
        assert_eq!(f.count("schedule_claims"), 1);
        f.no_execution();
    }
}

#[test]
fn routine_expiry_clock_is_not_sampled_when_writer_begin_fails() {
    struct CountClock(Arc<AtomicUsize>);
    impl crate::Clock for CountClock {
        fn now_ms(&self) -> i64 {
            self.0.fetch_add(1, Ordering::SeqCst);
            60_900
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let mut f = Fixture::new();
    f.db = RunsDb::in_memory(Box::new(CountClock(calls.clone())));
    f.db.register_routine_route(&f.route).unwrap();
    calls.store(0, Ordering::SeqCst);
    f.db.conn().execute_batch("BEGIN IMMEDIATE").unwrap();
    let facts = f.facts(900, RunId::mint(), None);
    assert!(f.db.record_expired_routine(facts).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    f.db.conn().execute_batch("ROLLBACK").unwrap();
    let facts = f.facts(900, RunId::mint(), None);
    f.db.record_expired_routine(facts).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    f.no_execution();
}

#[test]
fn routine_expiry_crash_points_commit_only_primary_and_representative_together() {
    for at in Moment::ALL {
        let mut f = Fixture::new();
        let facts = f.facts(900, RunId::mint(), Some(miss()));
        f.db.arm_crash(CrashPoint {
            op: WriteOp::RecordExpiredRoutine,
            at,
        });
        assert!(f.db.record_expired_routine(facts).is_err());
        f.db.restart_in_memory();
        assert_eq!(
            f.count("schedule_claims"),
            if at == Moment::AfterCommit { 2 } else { 0 }
        );
        f.no_execution();
        let facts = f.facts(900, RunId::mint(), Some(miss()));
        let (outcome, representative) = f.db.record_expired_routine(facts).unwrap();
        assert!(matches!(outcome, RoutineClaimObservation::Skipped { .. }));
        assert!(representative.is_some());
        assert_eq!(f.count("schedule_claims"), 2);
        f.no_execution();
    }
}

#[test]
fn routine_expiry_planned_id_collision_rolls_back_primary_and_representative() {
    let mut f = Fixture::new();
    let run = RunId::mint();
    let facts = f.facts(100, run, None);
    f.db.record_expired_routine(facts).unwrap();
    let facts = f.facts(900, run, None);
    assert!(f.db.record_expired_routine(facts).is_err());
    assert_eq!(f.count("schedule_claims"), 1);
    f.no_execution();

    let mut f = Fixture::new();
    let ordinary = crate::bots::tests::submission(66);
    f.db.record_intent(&ordinary.request, ordinary.workspace)
        .unwrap();
    let facts = f.facts(
        900,
        RunId::mint(),
        Some(RoutineRepresentativeMiss {
            occurrence_ms: 100,
            planned_run: ordinary.request.run,
        }),
    );
    assert!(f.db.record_expired_routine(facts).is_err());
    assert_eq!(f.count("schedule_claims"), 0);
    assert_eq!(f.count("run_intents"), 1);
    assert_eq!(f.count("bot_admissions"), 0);
    assert_eq!(f.count("bot_routine_links"), 0);
}

#[test]
fn routine_expiry_retains_actual_busy_representative_after_holder_settlement() {
    let mut f = Fixture::new();
    let ordinary = crate::bots::tests::submission(66);
    f.db.admit_bot(&ordinary).unwrap();
    f.db.settle_bot(ordinary.request.run, BotSettlement::Settled)
        .unwrap();
    let m = miss();
    let busy = skip(
        f.db.conn(),
        f.slot(m.occurrence_ms),
        3,
        m.planned_run,
        RoutineSkipOutcome::SkippedBusy {
            holder: ordinary.request.run,
        },
        1000,
    )
    .unwrap();
    let facts = f.facts(900, RunId::mint(), Some(m));
    let (outcome, representative) = f.db.record_expired_routine(facts).unwrap();
    assert!(
        matches!(outcome,RoutineClaimObservation::Skipped{record} if matches!(record.outcome,RoutineSkipOutcome::SkippedMissed{}))
    );
    assert_eq!(representative, Some(busy));
    assert_eq!(f.count("schedule_claims"), 2);
    assert_eq!(f.count("run_intents"), 1);
    assert_eq!(f.count("bot_admissions"), 1);
    assert_eq!(f.count("bot_routine_links"), 0);
}
