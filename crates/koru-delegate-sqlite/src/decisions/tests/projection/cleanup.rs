// SPDX-License-Identifier: MPL-2.0

use super::*;

fn started(native: bool) -> (Harness, DecisionRecord, DecisionProjectionRecord) {
    let mut h = Harness::new();
    if native {
        h.native();
    }
    let base = base(&h);
    let record =
        h.db.prepare_bot_decision(&h.binding, &h.proof, h.boot, &base)
            .unwrap();
    let f = h.origin();
    let initial =
        h.db.next_decision_projection(f.owner, f.run)
            .unwrap()
            .unwrap();
    apply(&mut h, &initial);
    let record = h.read(record.receipt.decision).unwrap();
    (h, record, initial)
}

#[test]
fn decision_cleanup_request_retains_exact_cause_revision_and_no_terminal_effects() {
    let (mut h, record, initial) = started(false);
    let f = h.origin();
    let expected =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap()
            .expected;
    let transition = DecisionTransitionId::mint();
    let target =
        h.db.request_decision_cleanup(&expected, transition, DecisionClosureReason::Canceled)
            .unwrap();
    assert_eq!(
        target.target.effects().status,
        initial.target.effects().status
    );
    assert!(target.target.publications().is_empty());
    assert!(target.target.effects().steps.as_slice().is_empty());
    assert_eq!(
        h.read(record.receipt.decision).unwrap().receipt.state.phase,
        DecisionPhase::StopPending
    );
    assert!(
        !h.db.current_attempt(f.run).unwrap().unwrap().retired,
        "request is not cleanup proof"
    );
    h.revoke();
    assert_eq!(
        h.db.request_decision_cleanup(&expected, transition, DecisionClosureReason::Canceled)
            .unwrap(),
        target
    );
    assert!(
        h.db.request_decision_cleanup(
            &expected,
            transition,
            DecisionClosureReason::AuthorityChanged
        )
        .is_err()
    );
    let mut changed = expected.clone();
    changed.revision = rev(expected.revision.get() + 1);
    assert!(
        h.db.request_decision_cleanup(&changed, transition, DecisionClosureReason::Canceled)
            .is_err()
    );
}

#[test]
fn decision_cleanup_request_crash_never_commits_phase_without_control() {
    for moment in [
        crate::Moment::AfterWriteBeforeCommit,
        crate::Moment::AfterCommit,
    ] {
        let (mut h, record, _) = started(false);
        let f = h.origin();
        let expected =
            h.db.decision_wait_execution(f.owner, f.run)
                .unwrap()
                .unwrap()
                .expected;
        let transition = DecisionTransitionId::mint();
        h.db.arm_crash(crate::CrashPoint {
            op: crate::WriteOp::RecordDecisionProjection,
            at: moment,
        });
        assert!(
            h.db.request_decision_cleanup(&expected, transition, DecisionClosureReason::Canceled)
                .is_err()
        );
        h.db.restart_in_memory();
        let committed = moment == crate::Moment::AfterCommit;
        assert_eq!(
            h.db.decision_projection(f.owner, f.run, transition)
                .unwrap()
                .is_some(),
            committed
        );
        assert_eq!(
            h.read(record.receipt.decision).unwrap().receipt.state.phase,
            if committed {
                DecisionPhase::StopPending
            } else {
                DecisionPhase::Holding
            }
        );
        h.db.request_decision_cleanup(&expected, transition, DecisionClosureReason::Canceled)
            .unwrap();
        assert_eq!(h.count("bot_decision_publications"), 2);
    }
}

#[test]
fn decision_cleanup_of_settled_native_answer_preserves_answer_and_rejects_deadline_reuse() {
    for allow in [false, true] {
        let (mut h, record, _) = started(true);
        let input = h.answer_input(&record, allow);
        let answer = h.answer(&input, 1001).unwrap().record;
        let Some(DecisionNativeResponse::Prepared { key }) = answer.native_response else {
            panic!("prepared answer")
        };
        let f = h.origin();
        let tx = h.db.conn().unchecked_transaction().unwrap();
        let settled = record_native_answer_on(
            &tx,
            f.owner,
            f.run,
            &DecisionNativeResponse::Acknowledged {
                acknowledgment: key.clone(),
                key,
                observed_at_ms: 1002,
            },
            1002,
        )
        .unwrap()
        .record;
        tx.commit().unwrap();
        let expected =
            h.db.decision_wait_execution(f.owner, f.run)
                .unwrap()
                .unwrap()
                .expected;
        for cause in [
            DecisionClosureReason::Expired,
            DecisionClosureReason::ClockUncertain,
        ] {
            assert!(
                h.db.request_decision_cleanup(&expected, DecisionTransitionId::mint(), cause)
                    .is_err()
            );
            assert_eq!(h.read(record.receipt.decision).unwrap(), settled);
        }
        let control =
            h.db.request_decision_cleanup(
                &expected,
                DecisionTransitionId::mint(),
                DecisionClosureReason::Canceled,
            )
            .unwrap();
        let current = h.read(record.receipt.decision).unwrap();
        assert_eq!(current.receipt, settled.receipt);
        assert_eq!(current.native_response, settled.native_response);
        assert_eq!(current.wait.phase, BotWaitPhase::CleanupPending);
        assert!(matches!(
            control.target,
            RunProjectionTargetV1::Control {
                disposition: Some(DecisionDisposition::Settled { .. }),
                ..
            }
        ));
        assert!(!h.db.current_attempt(f.run).unwrap().unwrap().retired);
    }
}

#[test]
fn decision_cleanup_overflow_rolls_back_and_clock_uncertainty_never_rewinds_observation() {
    let (mut h, record, _) = started(false);
    let f = h.origin();
    h.db.conn()
        .execute(
            "UPDATE bot_wait_runtime SET revision=?2 WHERE wait_id=?1",
            params![record.wait.wait.to_storage(), i64::MAX],
        )
        .unwrap();
    let before = h.read(record.receipt.decision).unwrap();
    let expected =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap()
            .expected;
    assert!(
        h.db.request_decision_cleanup(
            &expected,
            DecisionTransitionId::mint(),
            DecisionClosureReason::Canceled
        )
        .is_err()
    );
    assert_eq!(h.read(record.receipt.decision).unwrap(), before);
    assert_eq!(h.count("bot_decision_publications"), 1);

    let (mut h, record, _) = started(false);
    let f = h.origin();
    h.db.conn().execute("UPDATE bot_decisions SET last_observed_at_ms=1005,updated_at_ms=1005 WHERE decision_id=?1",[record.receipt.decision.to_storage()]).unwrap();
    let expected =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap()
            .expected;
    h.db.request_decision_cleanup(
        &expected,
        DecisionTransitionId::mint(),
        DecisionClosureReason::ClockUncertain,
    )
    .unwrap();
    let closed = h.read(record.receipt.decision).unwrap();
    assert_eq!(
        closed.receipt.state.authorization,
        DecisionAuthorization::Expired
    );
    assert_eq!(closed.receipt.state.phase, DecisionPhase::StopPending);
    assert_eq!(closed.receipt.last_observed_at_ms, 1005);
    assert_eq!(closed.receipt.updated_at_ms, 1005);
    assert_eq!(closed.receipt.deadline, record.receipt.deadline);
}
