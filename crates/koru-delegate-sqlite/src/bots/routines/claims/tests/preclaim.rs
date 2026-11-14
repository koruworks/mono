// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn routine_preclaim_sql_requires_exact_present_unpaused_admission_tuple() {
    for change in 0..7 {
        let mut f = Fixture::new();
        let mut offered = f.admission.clone();
        match change {
            0 => {
                f.db.conn()
                    .execute("DELETE FROM bot_admission_barriers", [])
                    .unwrap();
            }
            1 => offered.request = BotRequestId::mint(),
            2 => offered.paused = true,
            3 => offered.epoch += 1,
            4 => offered.owner.device = DeviceId::new(),
            5 => offered.bot = BotId::mint(),
            _ => {
                let mut current = offered.clone();
                current.request = BotRequestId::mint();
                f.db.conn()
                    .execute(
                        "UPDATE bot_admission_barriers SET request_id=?1",
                        [current.request.to_storage()],
                    )
                    .unwrap();
            }
        }
        assert!(
            f.db.claim_bot_routine(f.facts(false), &offered, &f.fresh)
                .is_err(),
            "change{change}"
        );
        assert!(matches!(
            f.db.read_routine_claim(&f.prepared.slot).unwrap(),
            RoutineClaimObservation::Absent { .. }
        ));
        assert!(f.db.intents().unwrap().is_empty());
    }
}

#[test]
fn routine_preclaim_sql_rejects_newer_policy_and_grant_than_captured_draft() {
    for independent in [false, true] {
        let mut f = Fixture::new();
        if independent {
            let r = f.fresh.routine.as_mut().unwrap();
            r.grant_revision += 1;
            r.request = BotRequestId::mint().into();
            f.db.install_routine_policy(r).unwrap();
        } else {
            f.fresh.global.policy_revision += 1;
            f.fresh.global.key.request = BotRequestId::mint().into();
            f.db.install_native_policy(&f.fresh.global).unwrap();
        }
        assert_eq!(
            f.db.claim_bot_routine(f.facts(false), &f.admission, &f.fresh),
            Err(BotLedgerError::Stale)
        );
        assert!(f.db.intents().unwrap().is_empty());
    }
}

#[test]
fn routine_preclaim_sql_retained_winner_precedes_stale_admission_and_proof() {
    let mut f = Fixture::new();
    let winner = f.claim();
    f.admission.request = BotRequestId::mint();
    f.admission.paused = true;
    f.fresh.global.effects_allowed = false;
    f.db.conn()
        .execute("DELETE FROM bot_admission_barriers", [])
        .unwrap();
    f.clock.store(-1, Ordering::SeqCst);
    assert_eq!(
        f.db.claim_bot_routine(f.facts(false), &f.admission, &f.fresh)
            .unwrap()
            .0,
        RoutineClaimObservation::Claimed { record: winner }
    );
    assert_eq!(f.db.intents().unwrap().len(), 1);
}

#[test]
fn routine_preclaim_sql_expired_prepared_path_remains_policy_free_and_nonexecuting() {
    let mut f = Fixture::new();
    f.db.conn()
        .execute("DELETE FROM bot_admission_barriers", [])
        .unwrap();
    f.admission.paused = true;
    f.fresh.global.effects_allowed = false;
    f.clock.store(60_900, Ordering::SeqCst);
    assert!(matches!(
        f.db.claim_bot_routine(f.facts(false), &f.admission, &f.fresh)
            .unwrap()
            .0,
        RoutineClaimObservation::Skipped {
            record: RoutineSkippedClaim {
                outcome: RoutineSkipOutcome::SkippedMissed {},
                ..
            }
        }
    ));
    assert!(f.db.intents().unwrap().is_empty());
    assert!(f.db.bot_admission(f.prepared.ids.run).unwrap().is_none());
}
