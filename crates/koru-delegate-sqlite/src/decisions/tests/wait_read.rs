// SPDX-License-Identifier: MPL-2.0

//! The delegate-owned `ReadBotDecisionWait` observation. Authored, not yet run.
use super::*;
use lifecycle::expected;

fn read(
    h: &Harness,
    record: &DecisionRecord,
) -> Result<Option<BotDecisionWaitObservation>, BotLedgerError> {
    let f = h.origin();
    h.db.read_bot_decision_wait(
        f.owner,
        f.bot,
        f.run,
        record.wait.wait,
        record.receipt.decision,
    )
}

#[test]
fn decision_wait_read_separates_frozen_intent_history_from_fresh_eligibility() {
    let empty = Harness::new();
    let f = empty.origin();
    assert_eq!(
        empty
            .db
            .read_bot_decision_wait(f.owner, f.bot, f.run, BotWaitId::mint(), DecisionId::mint())
            .unwrap(),
        None
    );
    let mut foreign = f.owner;
    foreign.user_scope = serde_json::from_value(fixtures::v4(240).to_string().into()).unwrap();
    assert!(
        empty
            .db
            .read_bot_decision_wait(foreign, f.bot, f.run, BotWaitId::mint(), DecisionId::mint())
            .is_err()
    );

    let (mut h, record, clock) = lifecycle::koru_enrollment_ready();
    let f = h.origin();
    let request = record.wait.enrollment_request.clone().unwrap();
    let pending = read(&h, &record).unwrap().unwrap();
    assert_eq!(pending.key, request.key);
    assert_eq!(pending.cleanup_intent, request.cleanup_intent);
    assert_eq!(pending.current_revision, request.key.wait_revision);
    assert_eq!(
        pending.fresh_stop_eligibility,
        FreshWaitStopEligibility::EligibleOriginWaitStop
    );
    assert_eq!(pending.origin_stop, OriginWaitStop::Pending {});
    assert_eq!(pending.consumer_cleanup, ConsumerCleanup::NotRequested {});
    assert!(matches!(
        &pending.disposition,
        DecisionDisposition::Open { state } if state.phase == DecisionPhase::Holding
            && state.revision == request.key.decision_revision
    ));
    // The enrollment Control is still owed; it is reported, not assumed applied.
    let latest =
        h.db.latest_decision_projection(f.owner, f.run)
            .unwrap()
            .unwrap();
    assert!(matches!(
        pending.publication,
        DecisionPublication::Pending { latest_sequence, target_digest }
            if latest_sequence == latest.identity.sequence
                && target_digest == latest.identity.target_digest
    ));
    projection::apply(&mut h, &latest);
    let applied = read(&h, &record).unwrap().unwrap();
    assert!(matches!(
        applied.publication,
        DecisionPublication::Applied { transition, .. } if transition == latest.identity.transition
    ));
    assert!(applied.validate().is_ok());
    // An existing decision under another Bot or wait is a mismatch, not absence.
    assert!(
        h.db.read_bot_decision_wait(
            f.owner,
            BotId::mint(),
            f.run,
            record.wait.wait,
            record.receipt.decision
        )
        .is_err()
    );
    assert!(
        h.db.read_bot_decision_wait(
            f.owner,
            f.bot,
            f.run,
            BotWaitId::mint(),
            record.receipt.decision
        )
        .is_err()
    );
    let record = lifecycle::enrolled(&mut h, &record);
    let enrolled = read(&h, &record).unwrap().unwrap();
    assert_eq!(
        enrolled.current_revision.get(),
        request.key.wait_revision.get() + 1
    );
    assert_eq!(
        enrolled.fresh_stop_eligibility,
        FreshWaitStopEligibility::EligibleOriginWaitStop
    );
    clock.store(1001, std::sync::atomic::Ordering::Relaxed);
    let release = lifecycle::origin_release(&mut h, &record, 1001);
    h.db.record_decision_origin_stop(&expected(&record), DecisionTransitionId::mint(), &release)
        .unwrap();
    let stopped = read(&h, &record).unwrap().unwrap();
    assert_eq!(stopped.origin_stop, release);
    assert_eq!(
        stopped.fresh_stop_eligibility,
        FreshWaitStopEligibility::Superseded,
        "a confirmed stop is history, never a second fresh stop"
    );
    assert!(matches!(
        &stopped.disposition,
        DecisionDisposition::Open { state } if state.phase == DecisionPhase::Held
    ));
    // Historical evidence needs no fresh policy; revocation changes nothing.
    h.revoke();
    assert_eq!(read(&h, &record).unwrap().unwrap(), stopped);
}

#[test]
fn decision_wait_read_reports_owed_then_confirmed_cleanup_and_terminal() {
    let (mut h, record, clock) = lifecycle::koru_enrollment_ready();
    let f = h.origin();
    let record = lifecycle::enrolled(&mut h, &record);
    clock.store(1001, std::sync::atomic::Ordering::Relaxed);
    let release = lifecycle::origin_release(&mut h, &record, 1001);
    let held = h
        .db
        .record_decision_origin_stop(&expected(&record), DecisionTransitionId::mint(), &release)
        .unwrap();
    let input = h.answer_input(&held, false);
    let deny = DecisionTransitionId::mint();
    h.db.answer_bot_decision(&input, deny, &h.proof, h.boot)
        .unwrap();
    let denied =
        h.db.decision_projection(f.owner, f.run, deny)
            .unwrap()
            .unwrap();
    projection::apply(&mut h, &denied);
    let owed = read(&h, &record).unwrap().unwrap();
    let current =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap();
    assert_eq!(
        owed.consumer_cleanup,
        ConsumerCleanup::Owed {
            cleanup_revision: current.wait.revision,
            target: current.cleanup_target.clone(),
        }
    );
    assert!(matches!(
        current.cleanup_target,
        DecisionCleanupTarget::Origin { fence, .. } if fence == f
    ));
    assert_eq!(
        owed.fresh_stop_eligibility,
        FreshWaitStopEligibility::Superseded
    );
    assert_eq!(owed.origin_stop, release);
    let owner =
        h.db.decision_cleanup_ownership(f.owner, f.run)
            .unwrap()
            .unwrap();
    let (ticket, proof) = lifecycle::confirmed_cleanup(&mut h, 1001);
    let last = DecisionTransitionId::mint();
    h.db.record_decision_consumer_cleanup(&ticket, owner.projection, last, &proof)
        .unwrap();
    let confirmed = read(&h, &record).unwrap().unwrap();
    assert_eq!(confirmed.consumer_cleanup, proof);
    assert_eq!(
        confirmed.fresh_stop_eligibility,
        FreshWaitStopEligibility::Terminal
    );
    assert!(matches!(
        confirmed.disposition,
        DecisionDisposition::Settled {
            reason: DecisionClosureReason::Denied,
            ..
        }
    ));
    let final_projection =
        h.db.decision_projection(f.owner, f.run, last)
            .unwrap()
            .unwrap();
    assert!(matches!(
        confirmed.publication,
        DecisionPublication::Pending { latest_sequence, .. }
            if latest_sequence == final_projection.identity.sequence
    ));
    projection::apply(&mut h, &final_projection);
    let published = read(&h, &record).unwrap().unwrap();
    assert!(matches!(
        published.publication,
        DecisionPublication::Applied { transition, .. } if transition == last
    ));
    assert!(published.validate().is_ok());
}

#[test]
fn decision_wait_read_has_no_observation_for_a_native_wait() {
    let (h, record) = lifecycle::native_ready();
    assert_eq!(
        read(&h, &record).unwrap(),
        None,
        "native waits never enroll"
    );
    let f = h.origin();
    assert!(
        h.db.read_bot_decision_wait(
            f.owner,
            BotId::mint(),
            f.run,
            record.wait.wait,
            record.receipt.decision
        )
        .is_err(),
        "the subject is checked before the missing intent"
    );
}
