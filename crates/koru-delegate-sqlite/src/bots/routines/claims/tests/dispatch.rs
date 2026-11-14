// SPDX-License-Identifier: MPL-2.0

//! Production transaction owners, in-memory SQLite only. No provider or Store.
use super::*;
use crate::{AttemptPhase, BotInitialSend, BotReservation, SlotOutcome, SlotState};
use koru_delegate_core::{AttemptGeneration, ProviderSessionKey};

fn linked(f: &mut Fixture) {
    let record = f.claim();
    f.db.acknowledge_routine_link(&RoutineLinkAcknowledgement {
        claim: record.claim,
        outcome: RoutineLinkOutcome::Linked {},
    })
    .unwrap();
}
fn starting(f: &mut Fixture) -> crate::AttemptRow {
    linked(f);
    let run = f.prepared.ids.run;
    assert_eq!(
        f.db.reserve_bot(run, Some(&f.fresh)).unwrap(),
        BotReservation::Acquired
    );
    f.db.satisfy_intent(run, fixtures::document(0xf1)).unwrap();
    let a = f.db.open_bot_attempt(run, Some(&f.fresh)).unwrap();
    f.db.activate_bot(run, a.attempt, a.generation, Some(&f.fresh))
        .unwrap();
    f.db.advance(run, a.generation, AttemptPhase::Starting)
        .unwrap();
    f.db.bind_session(
        run,
        a.generation,
        ProviderSessionKey {
            idempotency: fixtures::key(0xf2),
            session: Some(fixtures::session(0xf3)),
        },
        fixtures::host(0xf4),
    )
    .unwrap()
}
fn prepared_send(f: &mut Fixture, a: &crate::AttemptRow) -> BotInitialSend {
    let send = BotInitialSend {
        attempt: a.attempt,
        generation: a.generation,
        key: fixtures::key(0xf5),
        fingerprint: [8; 32],
        turn: None,
    };
    f.db.prepare_bot_initial_send(a.run, &send, Some(&f.fresh))
        .unwrap()
}
fn acknowledged(mut send: BotInitialSend) -> BotInitialSend {
    send.turn = Some(fixtures::v4(0xf6).to_string().parse().unwrap());
    send
}
pub(super) fn fire_existing(f: &mut Fixture) {
    let a = starting(f);
    let send = prepared_send(f, &a);
    f.db.acknowledge_bot_initial_send(a.run, &acknowledged(send))
        .unwrap();
}

#[test]
fn routine_dispatch_full_link_current_proof_first_attempt_and_acknowledged_firing() {
    let mut f = Fixture::new();
    let a = starting(&mut f);
    assert_eq!(a.generation, AttemptGeneration::FIRST);
    assert!(f.db.open_attempt(a.run).is_err());
    assert!(f.db.open_bot_attempt(a.run, Some(&f.fresh)).is_err());
    assert_eq!(f.db.routine_firing(&f.prepared.slot).unwrap(), None);
    let send = prepared_send(&mut f, &a);
    assert_eq!(f.db.routine_firing(&f.prepared.slot).unwrap(), None);
    let ack = acknowledged(send);
    f.db.acknowledge_bot_initial_send(a.run, &ack).unwrap();
    let firing = f.db.routine_firing(&f.prepared.slot).unwrap().unwrap();
    assert_eq!(firing.run, a.run);
    assert_eq!(firing.state, SlotState::Settled(SlotOutcome::Fired));
    assert_eq!(f.db.acknowledge_bot_initial_send(a.run, &ack).unwrap(), ack);
}

#[test]
fn routine_dispatch_held_terminal_missing_and_superseded_proofs_never_reserve() {
    for change in 0..7 {
        let mut f = Fixture::new();
        let record = f.claim();
        if change != 0 {
            f.db.acknowledge_routine_link(&RoutineLinkAcknowledgement {
                claim: record.claim,
                outcome: RoutineLinkOutcome::Linked {},
            })
            .unwrap();
        }
        match change {
            2 => {
                f.db.conn()
                    .execute(
                        "UPDATE bot_routine_links SET state='terminal',terminal_reason='stopped'",
                        [],
                    )
                    .unwrap();
            }
            3 => {
                f.db.conn()
                    .execute("DELETE FROM bot_routine_links", [])
                    .unwrap();
            }
            4 => {
                let mut newer = f.fresh.global.clone();
                newer.key.request = BotRequestId::mint().into();
                newer.config_revision += 1;
                f.db.install_native_policy(&newer).unwrap();
            }
            5 => {
                let mut newer = f.fresh.routine.clone().unwrap();
                newer.request = BotRequestId::mint().into();
                newer.grant_revision += 1;
                f.db.install_routine_policy(&newer).unwrap();
            }
            6 => {
                let mut b = f.admission.clone();
                b.epoch += 1;
                b.paused = true;
                b.request = BotRequestId::mint();
                f.db.apply_bot_barrier(&b).unwrap();
            }
            _ => {}
        }
        assert!(
            f.db.reserve_bot(
                f.prepared.ids.run,
                if change == 1 { None } else { Some(&f.fresh) }
            )
            .is_err(),
            "case {change}"
        );
        let state: String =
            f.db.conn()
                .query_row("SELECT state FROM bot_admissions", [], |r| r.get(0))
                .unwrap();
        assert_eq!(state, "queued");
        assert!(f.db.current_attempt(f.prepared.ids.run).unwrap().is_none());
    }
}

#[test]
fn routine_dispatch_each_fresh_writer_rechecks_current_composite() {
    for stage in 0..4 {
        let mut f = Fixture::new();
        linked(&mut f);
        let run = f.prepared.ids.run;
        f.db.satisfy_intent(run, fixtures::document(0xf1)).unwrap();
        f.db.reserve_bot(run, Some(&f.fresh)).unwrap();
        let a = if stage > 0 {
            Some(f.db.open_bot_attempt(run, Some(&f.fresh)).unwrap())
        } else {
            None
        };
        if stage > 1 {
            let a = a.as_ref().unwrap();
            f.db.activate_bot(run, a.attempt, a.generation, Some(&f.fresh))
                .unwrap();
        }
        if stage == 3 {
            let a = a.as_ref().unwrap();
            f.db.set_bot_active(run, a.attempt, a.generation, false, None)
                .unwrap();
        }
        let mut newer = f.fresh.routine.clone().unwrap();
        newer.request = BotRequestId::mint().into();
        newer.grant_revision += 1;
        newer.effects_allowed = false;
        f.db.install_routine_policy(&newer).unwrap();
        let result = match stage {
            0 => f.db.open_bot_attempt(run, Some(&f.fresh)).map(|_| ()),
            1 => {
                let a = a.unwrap();
                f.db.activate_bot(run, a.attempt, a.generation, Some(&f.fresh))
            }
            2 => {
                let a = a.unwrap();
                f.db.prepare_bot_initial_send(
                    run,
                    &BotInitialSend {
                        attempt: a.attempt,
                        generation: a.generation,
                        key: fixtures::key(0xf5),
                        fingerprint: [8; 32],
                        turn: None,
                    },
                    Some(&f.fresh),
                )
                .map(|_| ())
            }
            _ => {
                let a = a.unwrap();
                f.db.set_bot_active(run, a.attempt, a.generation, true, Some(&f.fresh))
            }
        };
        assert!(result.is_err(), "stage {stage}");
    }
}

#[test]
fn routine_dispatch_late_ack_after_stop_is_history_not_fresh_authority() {
    let mut f = Fixture::new();
    let a = starting(&mut f);
    let send = prepared_send(&mut f, &a);
    f.db.advance(a.run, a.generation, AttemptPhase::Interrupted)
        .unwrap();
    f.db.retire(a.run, a.generation).unwrap();
    f.db.settle_bot(a.run, BotSettlement::Settled).unwrap();
    assert_eq!(f.db.routine_firing(&f.prepared.slot).unwrap(), None);
    let mut newer = f.fresh.global.clone();
    newer.key.request = BotRequestId::mint().into();
    newer.config_revision += 1;
    newer.effects_allowed = false;
    f.db.install_native_policy(&newer).unwrap();
    let ack = acknowledged(send.clone());
    f.db.acknowledge_bot_initial_send(a.run, &ack).unwrap();
    assert!(f.db.routine_firing(&f.prepared.slot).unwrap().is_some());
    assert_eq!(
        f.db.prepare_bot_initial_send(a.run, &send, None).unwrap(),
        ack
    );
    assert!(f.db.open_bot_attempt(a.run, Some(&f.fresh)).is_err());
    let retained = f.db.current_attempt(a.run).unwrap().unwrap();
    assert!(retained.retired);
    assert_eq!(retained.provider_session, a.provider_session);
    assert!(f.db.forget_run(a.run).is_err());
    assert!(f.db.routine_firing(&f.prepared.slot).unwrap().is_some());
}

#[test]
fn routine_firing_rejects_corrupt_launch_binding_without_repair() {
    for sql in [
        "UPDATE run_attempts SET session_id=NULL",
        "UPDATE run_attempts SET session_key=NULL",
        "UPDATE run_attempts SET host_instance_id=NULL",
        "UPDATE run_attempts SET host_instance_id='invalid'",
        "UPDATE run_attempts SET host_instance_id='00000000-0000-0000-0000-000000000000'",
        "UPDATE run_intents SET run_document_id=NULL",
        "UPDATE run_intents SET run_document_id='00000000-0000-0000-0000-000000000000'",
    ] {
        let mut f = Fixture::new();
        let a = starting(&mut f);
        let send = prepared_send(&mut f, &a);
        let ack = acknowledged(send);
        f.db.acknowledge_bot_initial_send(a.run, &ack).unwrap();
        f.db.conn()
            .execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        f.db.conn().execute_batch(sql).unwrap();
        assert!(f.db.routine_firing(&f.prepared.slot).is_err(), "{sql}");
    }
}

#[test]
fn routine_firing_ack_failure_rolls_back_turn_and_outcome_together() {
    for at in Moment::ALL {
        let mut f = Fixture::new();
        let a = starting(&mut f);
        let send = prepared_send(&mut f, &a);
        let ack = acknowledged(send);
        f.db.arm_crash(CrashPoint {
            op: WriteOp::AcknowledgeBotInitialSend,
            at,
        });
        assert!(f.db.acknowledge_bot_initial_send(a.run, &ack).is_err());
        f.db.restart_in_memory();
        assert_eq!(
            f.db.routine_firing(&f.prepared.slot).unwrap().is_some(),
            at == Moment::AfterCommit
        );
        assert_eq!(
            f.db.bot_execution(a.run)
                .unwrap()
                .unwrap()
                .initial_send
                .unwrap()
                .turn
                .is_some(),
            at == Moment::AfterCommit
        );
        f.db.acknowledge_bot_initial_send(a.run, &ack).unwrap();
    }
    let mut f = Fixture::new();
    let a = starting(&mut f);
    let send = prepared_send(&mut f, &a);
    let ack = acknowledged(send);
    f.db.conn().execute_batch("CREATE TRIGGER refuse_firing BEFORE UPDATE OF outcome ON schedule_claims BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(f.db.acknowledge_bot_initial_send(a.run, &ack).is_err());
    assert!(
        f.db.bot_execution(a.run)
            .unwrap()
            .unwrap()
            .initial_send
            .unwrap()
            .turn
            .is_none()
    );
    assert_eq!(f.db.routine_firing(&f.prepared.slot).unwrap(), None);
}

#[test]
fn routine_firing_generic_writer_denies_route_even_with_forged_ordinary_run() {
    let mut f = Fixture::new();
    starting(&mut f);
    assert!(
        f.db.record_outcome(
            f.prepared.slot.schedule,
            f.prepared.slot.occurrence_ms,
            SlotOutcome::Fired
        )
        .is_err()
    );
    let ordinary = crate::bots::tests::submission(2);
    f.db.admit_bot(&ordinary).unwrap();
    f.db.conn()
        .execute_batch("PRAGMA foreign_keys=OFF")
        .unwrap();
    f.db.conn()
        .execute(
            "UPDATE schedule_claims SET run_id=?1",
            [ordinary.request.run.to_storage()],
        )
        .unwrap();
    assert!(
        f.db.record_outcome(
            f.prepared.slot.schedule,
            f.prepared.slot.occurrence_ms,
            SlotOutcome::Fired
        )
        .is_err()
    );
}

#[test]
fn routine_dispatch_boot_releases_only_unused_fully_valid_reservation() {
    for attempted in [false, true] {
        let mut f = Fixture::new();
        linked(&mut f);
        let run = f.prepared.ids.run;
        f.db.reserve_bot(run, Some(&f.fresh)).unwrap();
        if attempted {
            f.db.satisfy_intent(run, fixtures::document(0xf1)).unwrap();
            f.db.open_bot_attempt(run, Some(&f.fresh)).unwrap();
        }
        let key = BotRunKey {
            owner: f.route.owner,
            bot: f.route.bot,
            run,
        };
        assert_eq!(f.db.recover_bot_reservation(&key).unwrap(), !attempted);
        if attempted {
            assert_eq!(
                f.db.bot_execution(run).unwrap().unwrap().state,
                BotAdmissionState::Reserved
            );
        }
    }
}

#[test]
fn routine_dispatch_scalar_projection_is_not_json_downgrade_authority() {
    for malformed in [false, true] {
        let mut f = Fixture::new();
        let mut s = crate::bots::tests::submission(2);
        let run = s.request.run;
        f.db.admit_bot(&s).unwrap();
        f.db.satisfy_intent(run, fixtures::document(0xf1)).unwrap();
        f.db.reserve_bot(run, None).unwrap();
        let a = f.db.open_bot_attempt(run, None).unwrap();
        f.db.activate_bot(run, a.attempt, a.generation, None)
            .unwrap();
        let route = super::super::super::tests::route(&s);
        super::super::super::tests::routine(&mut s, &route);
        let payload = if malformed {
            "not JSON".to_owned()
        } else {
            serde_json::to_string(&s).unwrap()
        };
        f.db.conn()
            .execute(
                "UPDATE bot_admissions SET payload_json=?2 WHERE run_id=?1",
                params![run.to_storage(), payload],
            )
            .unwrap();
        // A deliberately unreadable body proves this scalar/no-op path does
        // not parse either JSON body. It grants no new execution transition.
        assert_eq!(
            f.db.bot_execution(run).unwrap().unwrap().submission_version,
            1
        );
        f.db.set_bot_active(run, a.attempt, a.generation, true, None)
            .unwrap();
        assert!(f.db.bot_admission(run).is_err());
        assert!(
            f.db.prepare_bot_initial_send(
                run,
                &BotInitialSend {
                    attempt: a.attempt,
                    generation: a.generation,
                    key: fixtures::key(0xf5),
                    fingerprint: [8; 32],
                    turn: None
                },
                None
            )
            .is_err()
        );
        assert!(
            f.db.set_bot_active(run, a.attempt, a.generation, false, None)
                .is_err()
        );
    }
}

#[test]
fn routine_dispatch_downgrade_and_orphan_link_deny_even_scalar_projection() {
    for orphan in [false, true] {
        let mut f = Fixture::new();
        linked(&mut f);
        let run = f.prepared.ids.run;
        let mut ordinary = f.prepared.submission.clone();
        ordinary.provenance.routine = None;
        ordinary.request.origin = RunOrigin::Manual;
        f.db.conn()
            .execute_batch("PRAGMA foreign_keys=OFF")
            .unwrap();
        f.db.conn()
            .execute(
                "UPDATE bot_admissions SET submission_version=1,payload_json=?1",
                [serde_json::to_string(&ordinary).unwrap()],
            )
            .unwrap();
        if orphan {
            f.db.conn()
                .execute("DELETE FROM schedule_claims", [])
                .unwrap();
            f.db.conn()
                .execute("DELETE FROM bot_routine_routes", [])
                .unwrap();
        }
        assert!(f.db.bot_execution(run).is_err());
        assert!(f.db.reserve_bot(run, None).is_err());
    }
}
