// SPDX-License-Identifier: MPL-2.0

use super::*;

fn dispatch(h: &mut Harness, allow: bool) -> DecisionNativeResponseKey {
    h.native();
    let id = h.prepare().receipt.decision;
    h.published(id, true);
    let input = h.answer_input(&h.read(id).unwrap(), allow);
    let result = h.answer(&input, 1001).unwrap();
    let Some(DecisionNativeResponse::Prepared { key }) = result.record.native_response else {
        panic!("actual answer prepared one response key");
    };
    key
}

fn observe(
    h: &mut Harness,
    value: &DecisionNativeResponse,
    now: i64,
) -> Result<DecisionMutation, BotLedgerError> {
    let f = h.origin();
    let tx = h.db.conn().unchecked_transaction().unwrap();
    let result = record_native_answer_on(&tx, f.owner, f.run, value, now)?;
    tx.commit().unwrap();
    Ok(result)
}

#[test]
fn decision_native_completion_allow_and_reject_settle_answer_not_run() {
    for allow in [true, false] {
        let mut h = Harness::new();
        let key = dispatch(&mut h, allow);
        observe(
            &mut h,
            &DecisionNativeResponse::Unconfirmed { key: key.clone() },
            1002,
        )
        .unwrap();
        h.revoke(); // Retained completion remains recordable after permission loss.
        let reply = DecisionNativeResponse::Acknowledged {
            key: key.clone(),
            acknowledgment: key.clone(),
            observed_at_ms: 1003,
        };
        let completed = observe(&mut h, &reply, 1004).unwrap();
        assert_eq!(completed.record.receipt.state.phase, DecisionPhase::Settled);
        assert_eq!(
            completed.record.receipt.state.authorization,
            if allow {
                DecisionAuthorization::Consumed
            } else {
                DecisionAuthorization::Denied
            }
        );
        assert_eq!(completed.record.wait.phase, BotWaitPhase::Fenced);
        assert!(completed.record.wait.cleanup.is_none());
        let f = h.origin();
        assert_eq!(
            h.db.bot_admission(f.run).unwrap().unwrap().state,
            BotAdmissionState::Executing
        );
        assert!(!h.db.current_attempt(f.run).unwrap().unwrap().retired);
        let replay = observe(&mut h, &reply, i64::MAX).unwrap();
        assert_eq!(replay.outcome, WriteOutcome::AlreadyApplied);
        assert_eq!(replay.record, completed.record);
    }
}

#[test]
fn decision_native_completion_after_stop_preserves_cleanup_debt() {
    let mut h = Harness::new();
    let key = dispatch(&mut h, true);
    let before = h.read(key.decision).unwrap();
    let f = h.origin();
    let tx = h.db.conn().unchecked_transaction().unwrap();
    phase_on(
        &tx,
        f.owner,
        f.run,
        key.decision,
        before.receipt.state.revision,
        DecisionStopCause::Canceled,
        1002,
    )
    .unwrap();
    tx.commit().unwrap();
    let stopped = h.read(key.decision).unwrap();
    let reply = DecisionNativeResponse::Acknowledged {
        key: key.clone(),
        acknowledgment: key.clone(),
        observed_at_ms: 1003,
    };
    let result = observe(&mut h, &reply, 1004).unwrap().record;
    assert_eq!(result.receipt, stopped.receipt);
    assert_eq!(result.wait, stopped.wait);
    assert_eq!(result.native_response, Some(reply));
    assert_eq!(result.wait.phase, BotWaitPhase::CleanupPending);
}

#[test]
fn decision_native_completion_refuses_changed_identity_and_impossible_time() {
    let mut h = Harness::new();
    let key = dispatch(&mut h, true);
    let retained = h.read(key.decision).unwrap();
    let mut wrong = key.clone();
    wrong.response_key = IdempotencyKey::mint();
    assert!(
        observe(
            &mut h,
            &DecisionNativeResponse::Unconfirmed { key: wrong },
            1002
        )
        .is_err()
    );
    for observed_at_ms in [1000, 1004] {
        let reply = DecisionNativeResponse::Acknowledged {
            key: key.clone(),
            acknowledgment: key.clone(),
            observed_at_ms,
        };
        assert!(observe(&mut h, &reply, 1003).is_err());
    }
    assert_eq!(h.read(key.decision).unwrap(), retained);
}
