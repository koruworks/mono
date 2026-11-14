// SPDX-License-Identifier: MPL-2.0

//! Ledger transaction tests use explicit consumed facts. Real live authority is
//! separately exercised by the production runtime adapter tests; neither suite
//! enables an executable Routine, Store, scheduler or provider.
use super::*;
mod preclaim;
mod recovery;
use crate::{BotLedger, BotSettlement, CrashPoint, Moment, NativeResultLedger, fixtures};
use std::sync::{
    Arc,
    atomic::{AtomicI64, AtomicUsize, Ordering},
};

struct Fixture {
    db: RunsDb,
    route: RoutineRouteIdentity,
    prepared: RoutinePrepared,
    fresh: FreshBotPolicyProof,
    admission: BotAdmissionBarrier,
    clock: Arc<AtomicI64>,
}
impl Fixture {
    fn new() -> Self {
        let clock = Arc::new(AtomicI64::new(1000));
        let mut db = RunsDb::in_memory(Box::new(fixtures::FakeClock(clock.clone())));
        let mut s = crate::bots::tests::submission(1);
        let route = super::super::tests::route(&s);
        super::super::tests::routine(&mut s, &route);
        s.request.context.clear();
        let ids = RoutineExecutionIds {
            submission: BotRequestId::mint(),
            run: s.request.run,
            chat: uuid::Uuid::new_v4(),
            attempt: uuid::Uuid::new_v4(),
            user_message: uuid::Uuid::new_v4(),
            assistant_message: uuid::Uuid::new_v4(),
        };
        s.provenance.submission = ids.submission;
        s.provenance.chat_id = ids.chat;
        s.provenance.ai_attempt_id = Some(ids.attempt);
        s.provenance.assistant_message_id = Some(ids.assistant_message);
        let prepared = RoutinePrepared {
            slot: RoutineSlot {
                owner: route.owner,
                bot: route.bot,
                schedule: route.schedule,
                occurrence_ms: 900,
            },
            definition: s.provenance.routine.unwrap().definition,
            ids,
            semantic_hash: RoutineDigest32::from_bytes([2; 32]),
            preparation_hash: RoutineDigest32::from_bytes([3; 32]),
            submission_version: 2,
            submission: s,
        };
        assert!(prepared.valid());
        let fresh = FreshBotPolicyProof {
            global: BotPolicyBarrier {
                key: BotPolicyBarrierKey {
                    owner: route.owner,
                    bot: route.bot,
                    request: BotRequestId::mint().into(),
                },
                config_revision: 1,
                policy_revision: 1,
                policy: prepared.submission.policy.clone(),
                effects_allowed: true,
            },
            routine: Some(RoutinePolicyBarrier {
                owner: route.owner,
                bot: route.bot,
                schedule: route.schedule,
                request: BotRequestId::mint().into(),
                grant_revision: 1,
                policy: prepared.submission.policy.clone(),
                effects_allowed: true,
            }),
        };
        let admission = BotAdmissionBarrier {
            owner: route.owner,
            bot: route.bot,
            request: BotRequestId::mint(),
            epoch: 1,
            paused: false,
        };
        db.apply_bot_barrier(&admission).unwrap();
        db.register_routine_route(&route).unwrap();
        db.install_native_policy(&fresh.global).unwrap();
        db.install_routine_policy(fresh.routine.as_ref().unwrap())
            .unwrap();
        Self {
            db,
            route,
            prepared,
            fresh,
            admission,
            clock,
        }
    }
    fn facts(&self, missed: bool) -> RoutineClaimFacts {
        RoutineClaimFacts::from_consumed_authority(
            self.prepared.clone(),
            self.prepared.slot,
            self.prepared.definition,
            self.route.workspace,
            self.route.creation_request,
            missed.then_some(RoutineRepresentativeMiss {
                occurrence_ms: 100,
                planned_run: RunId::from_uuid(fixtures::v4(0xd8)),
            }),
        )
        .unwrap()
    }
    fn claim(&mut self) -> RoutineClaimRecord {
        let facts = self.facts(false);
        let RoutineClaimObservation::Claimed { record } = self
            .db
            .claim_bot_routine(facts, &self.admission, &self.fresh)
            .unwrap()
            .0
        else {
            panic!("claim")
        };
        record
    }
}

#[test]
fn routine_claim_crash_boundary_keeps_all_four_rows_and_representative_atomic() {
    for at in Moment::ALL {
        let mut f = Fixture::new();
        let facts = f.facts(true);
        f.db.arm_crash(CrashPoint {
            op: WriteOp::ClaimBotRoutine,
            at,
        });
        assert!(
            f.db.claim_bot_routine(facts, &f.admission, &f.fresh)
                .is_err()
        );
        f.db.restart_in_memory();
        for (table, committed) in [
            ("run_intents", 1),
            ("schedule_claims", 2),
            ("bot_admissions", 1),
            ("bot_routine_links", 1),
        ] {
            let count: i64 =
                f.db.conn()
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                    .unwrap();
            assert_eq!(
                count,
                if at == Moment::AfterCommit {
                    committed
                } else {
                    0
                }
            );
        }
        let (outcome, retained_skip) =
            f.db.claim_bot_routine(f.facts(true), &f.admission, &f.fresh)
                .unwrap();
        assert!(matches!(outcome, RoutineClaimObservation::Claimed { .. }));
        assert!(retained_skip.is_some());
    }
}

#[test]
fn routine_representative_preserves_retained_skip_truth_and_refuses_claim_collision() {
    for outcome in [
        RoutineSkipOutcome::SkippedMissed {},
        RoutineSkipOutcome::SkippedBusy {
            holder: RunId::from_uuid(fixtures::v4(0xd6)),
        },
    ] {
        let mut f = Fixture::new();
        if let RoutineSkipOutcome::SkippedBusy { holder } = outcome {
            let mut s = crate::bots::tests::submission(66);
            s.request.run = holder;
            f.db.admit_bot(&s).unwrap();
            f.db.settle_bot(holder, BotSettlement::Settled).unwrap();
        }
        let retained = skip(
            f.db.conn(),
            RoutineSlot {
                occurrence_ms: 100,
                ..f.prepared.slot
            },
            9,
            RunId::from_uuid(fixtures::v4(0xd7)),
            outcome,
            500,
        )
        .unwrap();
        let (_, actual) =
            f.db.claim_bot_routine(f.facts(true), &f.admission, &f.fresh)
                .unwrap();
        assert_eq!(actual, Some(retained));
    }
    for outcome in ["claimed", "fired"] {
        let mut f = Fixture::new();
        // A real older claim, with all linked identities, wins its slot.
        let later = f.prepared.clone();
        f.prepared.slot.occurrence_ms = 100;
        f.prepared
            .submission
            .provenance
            .routine
            .as_mut()
            .unwrap()
            .occurrence_ms = 100;
        f.prepared.submission.request.origin = RunOrigin::Scheduled {
            schedule: f.route.schedule,
            occurrence_ms: 100,
            revision: 1,
        };
        let old = f.claim();
        if outcome == "fired" {
            f.db.conn()
                .execute("UPDATE schedule_claims SET outcome='fired'", [])
                .unwrap();
            f.db.acknowledge_routine_link(&RoutineLinkAcknowledgement {
                claim: old.claim.clone(),
                outcome: RoutineLinkOutcome::Linked {},
            })
            .unwrap();
            f.db.satisfy_intent(old.claim.ids.run, fixtures::document(0xe0))
                .unwrap();
        }
        f.prepared = later;
        f.prepared.ids.run = RunId::from_uuid(uuid::Uuid::new_v4());
        f.prepared.ids.submission = BotRequestId::mint();
        f.prepared.submission.request.run = f.prepared.ids.run;
        f.prepared.submission.provenance.submission = f.prepared.ids.submission;
        assert!(
            f.db.claim_bot_routine(f.facts(true), &f.admission, &f.fresh)
                .is_err()
        );
        assert!(f.db.intent(f.prepared.ids.run).unwrap().is_none());
        assert!(matches!(
            f.db.read_routine_claim(&f.prepared.slot).unwrap(),
            RoutineClaimObservation::Absent { .. }
        ));
        assert!(matches!(
            f.db.read_routine_claim(&old.claim.slot).unwrap(),
            RoutineClaimObservation::Claimed { .. }
        ));
    }
}

#[test]
fn routine_observation_and_ack_reject_corrupt_repeated_identity_and_oversized_cells() {
    for mutation in [
        "UPDATE bot_routine_links SET updated_at_ms=created_at_ms-1",
        "UPDATE bot_routine_links SET definition_revision=definition_revision+1",
        "UPDATE bot_routine_links SET grant_revision=grant_revision+1",
        "UPDATE bot_routine_links SET preparation_hash=zeroblob(1048576)",
        "UPDATE bot_routine_links SET chat_id=hex(zeroblob(1048576))",
        "UPDATE bot_admissions SET initial_send_key=hex(zeroblob(1048576))",
        "UPDATE bot_admissions SET payload_json=hex(zeroblob(3000000))",
        "UPDATE run_intents SET state='abandoned'",
        "UPDATE run_intents SET workspace_id='00000000-0000-4000-8000-000000000009'",
        "UPDATE schedule_claims SET revision=revision+1",
    ] {
        let mut f = Fixture::new();
        let record = f.claim();
        f.db.conn()
            .execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        f.db.conn().execute(mutation, []).unwrap();
        assert!(
            f.db.read_routine_claim(&f.prepared.slot).is_err(),
            "{mutation}"
        );
        assert!(
            f.db.acknowledge_routine_link(&RoutineLinkAcknowledgement {
                claim: record.claim,
                outcome: RoutineLinkOutcome::Linked {}
            })
            .is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn routine_link_ack_exact_identity_and_terminal_monotonicity_survive_lost_reply() {
    let mut f = Fixture::new();
    let record = f.claim();
    for field in 0..7 {
        let mut identity = record.claim.clone();
        match field {
            0 => identity.ids.user_message = uuid::Uuid::new_v4(),
            1 => identity.ids.run = RunId::from_uuid(uuid::Uuid::new_v4()),
            2 => identity.preparation_hash = RoutineDigest32::from_bytes([44; 32]),
            3 => {
                identity.definition.owner_handoff = Some(HandoffId::from_uuid(uuid::Uuid::new_v4()))
            }
            4 => identity.claimed_at_ms += 1,
            5 => identity.slot.bot = BotId::mint(),
            _ => identity.ids.submission = BotRequestId::mint(),
        }
        assert!(
            f.db.acknowledge_routine_link(&RoutineLinkAcknowledgement {
                claim: identity,
                outcome: RoutineLinkOutcome::Linked {}
            })
            .is_err()
        );
    }
    for at in Moment::ALL {
        let mut f = Fixture::new();
        let record = f.claim();
        let ack = RoutineLinkAcknowledgement {
            claim: record.claim,
            outcome: RoutineLinkOutcome::Terminal {
                reason: RoutineTerminalReason::ConfigurationChanged,
            },
        };
        f.clock.store(900, Ordering::SeqCst);
        f.db.arm_crash(CrashPoint {
            op: WriteOp::AcknowledgeRoutineLink,
            at,
        });
        assert!(f.db.acknowledge_routine_link(&ack).is_err());
        f.db.restart_in_memory();
        let terminal = f.db.acknowledge_routine_link(&ack).unwrap();
        assert_eq!(terminal.link_state, ack.outcome.state());
        let repeated = f.db.acknowledge_routine_link(&ack).unwrap();
        assert_eq!(repeated, terminal);
        assert!(
            f.db.acknowledge_routine_link(&RoutineLinkAcknowledgement {
                claim: ack.claim,
                outcome: RoutineLinkOutcome::Linked {}
            })
            .is_err()
        );
    }
}

#[test]
fn routine_policy_is_exact_monotonic_current_and_stale_config_cannot_claim() {
    let mut f = Fixture::new();
    let barrier = f.fresh.routine.clone().unwrap();
    let first = f.db.install_routine_policy(&barrier).unwrap();
    assert_eq!(first.state, BotPolicyBarrierState::Effective);
    let mut changed = barrier.clone();
    changed.effects_allowed = false;
    assert_eq!(
        f.db.install_routine_policy(&changed),
        Err(BotLedgerError::Conflict)
    );
    changed.request = BotRequestId::mint().into();
    assert_eq!(
        f.db.install_routine_policy(&changed),
        Err(BotLedgerError::Conflict)
    );
    changed.grant_revision = 2;
    f.db.install_routine_policy(&changed).unwrap();
    assert_eq!(f.db.install_routine_policy(&barrier).unwrap(), first);
    assert_eq!(
        f.db.current_routine_policy(barrier.owner, barrier.bot, barrier.schedule)
            .unwrap()
            .unwrap()
            .grant_revision,
        2
    );
    assert!(
        f.db.claim_bot_routine(f.facts(false), &f.admission, &f.fresh)
            .is_err()
    );
    let mut f = Fixture::new();
    f.fresh.global.config_revision = 2;
    f.fresh.global.key.request = BotRequestId::mint().into();
    f.db.install_native_policy(&f.fresh.global).unwrap();
    assert_eq!(
        f.db.claim_bot_routine(f.facts(false), &f.admission, &f.fresh),
        Err(BotLedgerError::Stale)
    );
    f.db.conn().execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE bot_routine_policy_barriers SET payload_digest=zeroblob(1048576)").unwrap();
    assert!(
        f.db.current_routine_policy(f.route.owner, f.route.bot, f.route.schedule)
            .is_err()
    );
}

#[test]
fn routine_stop_refuses_reserved_work_and_generic_settlement_terminalizes_exact_link() {
    let mut f = Fixture::new();
    let record = f.claim();
    f.db.acknowledge_routine_link(&RoutineLinkAcknowledgement {
        claim: record.claim.clone(),
        outcome: RoutineLinkOutcome::Linked {},
    })
    .unwrap();
    f.db.conn()
        .execute("UPDATE bot_admissions SET state='reserved'", [])
        .unwrap();
    let key = BotRunKey {
        owner: f.route.owner,
        bot: f.route.bot,
        run: f.prepared.ids.run,
    };
    assert_eq!(
        f.db.stop_unstarted_routine(&key),
        Err(BotLedgerError::Fenced)
    );
    assert_eq!(
        f.db.bot_admission(key.run).unwrap().unwrap().state,
        BotAdmissionState::Reserved
    );
    f.db.settle_bot(key.run, BotSettlement::Refused).unwrap();
    let RoutineClaimObservation::Claimed { record } =
        f.db.read_routine_claim(&f.prepared.slot).unwrap()
    else {
        panic!("claim")
    };
    assert_eq!(
        record.link_state,
        RoutineLinkState::Terminal {
            reason: RoutineTerminalReason::AlreadyTerminal
        }
    );
    f.db.conn()
        .execute(
            "UPDATE bot_routine_links SET state='linked',terminal_reason=NULL",
            [],
        )
        .unwrap();
    assert!(f.db.settle_bot(key.run, BotSettlement::Refused).is_err());
}

#[test]
fn routine_writer_clock_is_sampled_only_after_successful_begin_and_at_stays_explicit() {
    struct CountClock(Arc<AtomicUsize>);
    impl crate::Clock for CountClock {
        fn now_ms(&self) -> i64 {
            self.0.fetch_add(1, Ordering::SeqCst);
            1337
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let mut db = RunsDb::in_memory(Box::new(CountClock(calls.clone())));
    db.conn().execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(db.write(WriteOp::ClaimSlot, |_, now| Ok(now)).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    db.conn().execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        db.write(WriteOp::ClaimSlot, |_, now| Ok(now)).unwrap(),
        1337
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let request = fixtures::request(5);
    let slot = db
        .claim_scheduled_run_at(
            fixtures::schedule(5),
            900,
            1,
            &request,
            fixtures::workspace(0xa2),
            777,
        )
        .unwrap();
    assert_eq!(slot.claimed_at_ms, 777);
}

#[test]
fn routine_busy_reads_use_unsettled_indexes_and_reject_foreign_retained_holder() {
    let mut f = Fixture::new();
    for n in 90..120 {
        let s = crate::bots::tests::submission(n);
        f.db.admit_bot(&s).unwrap();
        f.db.settle_bot(s.request.run, BotSettlement::Settled)
            .unwrap();
    }
    for (query, index) in [
        (BUSY_EXECUTION, "bot_admissions_execution"),
        (BUSY_QUEUE, "bot_admissions_queue"),
    ] {
        let mut statement =
            f.db.conn()
                .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
                .unwrap();
        let plan = statement
            .query_map(
                params![
                    f.route.owner.user_scope.to_storage(),
                    f.route.owner.device.to_storage(),
                    f.route.bot.to_storage()
                ],
                |r| r.get::<_, String>(3),
            )
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(plan.contains(index), "{plan}");
        assert!(!plan.contains("SCAN bot_admissions"), "{plan}");
    }
    let holder = crate::bots::tests::submission(80);
    f.db.admit_bot(&holder).unwrap();
    let result =
        f.db.claim_bot_routine(f.facts(false), &f.admission, &f.fresh)
            .unwrap()
            .0;
    assert!(matches!(result, RoutineClaimObservation::Skipped { .. }));
    f.db.settle_bot(holder.request.run, BotSettlement::Settled)
        .unwrap();
    assert_eq!(f.db.read_routine_claim(&f.prepared.slot).unwrap(), result);
    let foreign = BotId::mint();
    f.db.conn()
        .execute(
            "UPDATE bot_admissions SET bot_id=?2 WHERE run_id=?1",
            params![holder.request.run.to_storage(), foreign.to_storage()],
        )
        .unwrap();
    assert!(f.db.read_routine_claim(&f.prepared.slot).is_err());
}
