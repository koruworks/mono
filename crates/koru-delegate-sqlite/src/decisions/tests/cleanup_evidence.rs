// SPDX-License-Identifier: MPL-2.0

//! The two read-only evidence readers a final cleanup needs: the complete
//! native drain of the exact retired attempt, and a consumed edit's outcome.
use super::*;
use lifecycle::expected;

fn drained_effect(h: &mut Harness) -> (PrepareNativeResult, NativeEffectDrainReceipt) {
    let fence = completion::execution_fence(&h.origin());
    let request = completion::result_request(h);
    h.db.prepare_native_result(&request).unwrap();
    let effect =
        h.db.lease_native_result(&request.key, request.request_fingerprint, &fence, &h.proof)
            .unwrap()
            .intent;
    let receipt = NativeEffectDrainReceipt {
        binding: effect.pending_effect().unwrap().binding,
        fenced_at: NativeEffectTimestamp::from_millis(999),
        drained_at: NativeEffectTimestamp::from_millis(1000),
    };
    h.db.confirm_native_result(&ConfirmNativeResult {
        key: request.key.clone(),
        request_fingerprint: request.request_fingerprint,
        observation: NativeResultObservation::DrainedUnknown {
            receipt: receipt.clone(),
        },
    })
    .unwrap();
    (request, receipt)
}

fn not_applied(request: &PrepareNativeResult) -> ConfirmNativeResult {
    ConfirmNativeResult {
        key: request.key.clone(),
        request_fingerprint: request.request_fingerprint,
        observation: NativeResultObservation::NotApplied {
            code: NativeResultRefusal::Conflict,
        },
    }
}

#[test]
fn decision_native_drain_supplies_the_complete_drain_a_cleanup_needs() {
    let mut h = Harness::new();
    let f = h.origin();
    let fence = completion::execution_fence(&f);
    let (_, receipt) = drained_effect(&mut h);
    let held = completion::result_request(&h);
    h.db.prepare_native_result(&held).unwrap();
    h.db.lease_native_result(&held.key, held.request_fingerprint, &fence, &h.proof)
        .unwrap();
    let observed = completion::sequence(3);
    let begin = DecisionTransitionId::mint();
    h.db.begin_bot_wait_cleanup(
        &f,
        BotWaitReason::Capacity {},
        observed,
        begin,
        &completion::fallback_base(&h, observed),
    )
    .unwrap();
    let preserve =
        h.db.decision_projection(f.owner, f.run, begin)
            .unwrap()
            .unwrap();
    projection::apply(&mut h, &preserve);
    let drain = |h: &Harness, observed_at_ms: i64| {
        h.db.decision_native_drain(f.owner, f.run, f.attempt, f.generation, observed_at_ms)
    };
    assert_eq!(
        drain(&h, 1000).unwrap_err(),
        BotLedgerError::Fenced,
        "a live attempt can still gain effects"
    );
    h.db.advance(f.run, f.generation, crate::AttemptPhase::Ended)
        .unwrap();
    h.db.retire(f.run, f.generation).unwrap();
    assert_eq!(
        drain(&h, 1000).unwrap_err(),
        BotLedgerError::Fenced,
        "a held lease is not drained"
    );
    h.db.confirm_native_result(&not_applied(&held)).unwrap();
    let complete = drain(&h, 1000).unwrap();
    assert_eq!(complete.effects.as_slice(), std::slice::from_ref(&receipt));
    assert_eq!(
        (complete.run, complete.attempt, complete.generation),
        (f.run, f.attempt, f.generation)
    );
    assert!(
        drain(&h, 999).is_err(),
        "observed before the effect drained"
    );
    let mut foreign = f.owner;
    foreign.user_scope = serde_json::from_value(fixtures::v4(240).to_string().into()).unwrap();
    assert!(
        h.db.decision_native_drain(foreign, f.run, f.attempt, f.generation, 1000)
            .is_err()
    );
    assert!(
        h.db.decision_native_drain(f.owner, f.run, RunAttemptId::mint(), f.generation, 1000)
            .is_err()
    );
    // The empty list D could supply before is refused; the complete one settles.
    let (ticket, empty) = completion::observed_cleanup(&h, 1000);
    assert!(
        h.db.record_nondecision_cleanup(&ticket, begin, DecisionTransitionId::mint(), &empty)
            .is_err()
    );
    let ConsumerCleanup::Confirmed {
        cleanup_revision,
        target,
        creation_retirement,
        attempt_retirement,
        retained_effect_outcome,
        ..
    } = empty
    else {
        unreachable!()
    };
    let proof = ConsumerCleanup::Confirmed {
        cleanup_revision,
        target,
        creation_retirement,
        attempt_retirement,
        native_drain: complete,
        retained_effect_outcome,
    };
    let wait =
        h.db.record_nondecision_cleanup(&ticket, begin, DecisionTransitionId::mint(), &proof)
            .unwrap();
    assert_eq!(wait.cleanup, Some(proof));
}

#[test]
fn decision_native_drain_refuses_a_corrupt_retained_effect() {
    for corruption in [
        "UPDATE native_result_intents SET lease_payload_digest=zeroblob(32) WHERE operation_id=?1",
        "UPDATE native_result_intents SET turn_id='not-a-turn' WHERE operation_id=?1",
    ] {
        let mut h = Harness::new();
        let f = h.origin();
        let (request, _) = drained_effect(&mut h);
        h.db.advance(f.run, f.generation, crate::AttemptPhase::Ended)
            .unwrap();
        h.db.retire(f.run, f.generation).unwrap();
        h.db.decision_native_drain(f.owner, f.run, f.attempt, f.generation, 1000)
            .unwrap();
        h.db.conn()
            .execute(corruption, [request.key.operation_id.to_string()])
            .unwrap();
        assert!(
            h.db.decision_native_drain(f.owner, f.run, f.attempt, f.generation, 1000)
                .is_err(),
            "{corruption}"
        );
    }
}

#[test]
fn decision_retained_effect_is_exactly_what_consumed_edit_cleanup_accepts() {
    let (mut h, record, request, fence, payload) = consume::ready();
    let f = h.origin();
    let decision = record.receipt.decision;
    assert_eq!(
        h.db.decision_retained_effect(f.owner, f.run, decision)
            .unwrap(),
        None,
        "granted, not consumed: no outcome is owed"
    );
    h.db.consume_decision_edit(
        &expected(&record),
        DecisionTransitionId::mint(),
        &request,
        &payload,
        &fence,
        &h.proof,
        h.boot,
    )
    .unwrap();
    assert_eq!(
        h.db.decision_retained_effect(f.owner, f.run, decision)
            .unwrap_err(),
        BotLedgerError::Fenced,
        "a held effect has no outcome, never a fabricated NotApplied"
    );
    h.db.confirm_native_result(&not_applied(&request)).unwrap();
    let outcome =
        h.db.decision_retained_effect(f.owner, f.run, decision)
            .unwrap()
            .unwrap();
    assert_eq!(
        outcome,
        DecisionRetainedEffect::NotApplied {
            key: request.key.clone(),
            code: NativeResultRefusal::Conflict,
        }
    );
    let mut foreign = f.owner;
    foreign.user_scope = serde_json::from_value(fixtures::v4(240).to_string().into()).unwrap();
    assert!(
        h.db.decision_retained_effect(foreign, f.run, decision)
            .is_err()
    );
    // Final cleanup of the consumed edit refuses without the outcome and
    // accepts exactly the one this reader returns.
    let requested =
        h.db.request_decision_cleanup(
            &completion::ticket(&h),
            DecisionTransitionId::mint(),
            DecisionClosureReason::Canceled,
        )
        .unwrap();
    projection::apply(&mut h, &requested);
    let owner =
        h.db.decision_cleanup_ownership(f.owner, f.run)
            .unwrap()
            .unwrap();
    let (ticket, without) = lifecycle::confirmed_cleanup(&mut h, 1000);
    assert!(
        h.db.record_decision_consumer_cleanup(
            &ticket,
            owner.projection,
            DecisionTransitionId::mint(),
            &without
        )
        .is_err()
    );
    let ConsumerCleanup::Confirmed {
        cleanup_revision,
        target,
        creation_retirement,
        attempt_retirement,
        native_drain,
        ..
    } = without
    else {
        unreachable!()
    };
    let with = ConsumerCleanup::Confirmed {
        cleanup_revision,
        target,
        creation_retirement,
        attempt_retirement,
        native_drain,
        retained_effect_outcome: Some(outcome),
    };
    let settled =
        h.db.record_decision_consumer_cleanup(
            &ticket,
            owner.projection,
            DecisionTransitionId::mint(),
            &with,
        )
        .unwrap();
    assert_eq!(
        settled.receipt.closure.unwrap().reason,
        DecisionClosureReason::EffectNotApplied
    );
}
