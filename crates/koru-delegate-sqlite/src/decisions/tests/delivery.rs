// SPDX-License-Identifier: MPL-2.0

//! The atomic delivery receipt + Delivery Control writer used while the Run
//! handle is held exclusively.
use super::*;
use lifecycle::expected;

fn receipt(
    h: &Harness,
    attempt: RunAttemptId,
    generation: AttemptGeneration,
) -> BotDeliveryReceipt {
    let f = h.origin();
    let provenance =
        h.db.bot_admission(f.run)
            .unwrap()
            .unwrap()
            .submission
            .provenance;
    BotDeliveryReceipt {
        run: f.run,
        attempt,
        generation,
        chat_id: provenance.chat_id,
        ai_attempt_id: provenance.ai_attempt_id.unwrap(),
        assistant_message_id: provenance.assistant_message_id.unwrap(),
        confirmed_at_ms: 1000,
    }
}

/// A settled native answer on the original attempt, its Control applied.
fn native_delivery_ready() -> (Harness, DecisionWaitExpected) {
    let (mut h, _, control) = completion::native_settled(true);
    projection::apply(&mut h, &control);
    let ticket = completion::ticket(&h);
    (h, ticket)
}

#[test]
fn decision_delivery_commits_receipt_and_owed_completion_together() {
    let (mut h, ticket) = native_delivery_ready();
    let f = h.origin();
    let initial = h.db.bot_admission(f.run).unwrap().unwrap().initial_send;
    let delivered = receipt(&h, f.attempt, f.generation);
    let transition = DecisionTransitionId::mint();
    let step = TimelineEntryId::mint();
    let (event, control) =
        h.db.record_decision_delivery(
            &delivered,
            &ticket,
            transition,
            DecisionDeliveryOutcome::Completed,
            step,
            0,
            1000,
        )
        .unwrap();
    let admission = h.db.bot_admission(f.run).unwrap().unwrap();
    assert_eq!(admission.delivery.as_ref(), Some(&delivered));
    assert_eq!(admission.delivery_publication, Some(event));
    assert_eq!(admission.initial_send, initial, "InitialSend is immutable");
    assert!(matches!(control.state, DecisionProjectionState::Pending));
    let RunProjectionTargetV1::Control {
        action: DecisionControlAction::Delivery { outcome },
        delivery: Some(delivery),
        effects,
        ..
    } = &control.target
    else {
        panic!("delivery control")
    };
    assert_eq!(*outcome, DecisionDeliveryOutcome::Completed);
    assert_eq!(delivery.receipt, delivered);
    assert_eq!(delivery.publication, event);
    assert_eq!(effects.status, RunStatus::Completed);
    assert_eq!(effects.steps.as_slice()[0].id, step);
    assert_eq!(control.target.publications()[0].event, event);
    assert!(
        h.db.publication(event).unwrap().is_none(),
        "the header publication is owed, not yet enqueued"
    );
    // Exact retained replay, independent of fresh policy.
    h.revoke();
    assert_eq!(
        h.db.record_decision_delivery(
            &delivered,
            &ticket,
            transition,
            DecisionDeliveryOutcome::Completed,
            step,
            0,
            1000
        )
        .unwrap(),
        (event, control.clone())
    );
    assert_eq!(
        h.db.record_decision_delivery(
            &delivered,
            &ticket,
            transition,
            DecisionDeliveryOutcome::Retained,
            step,
            0,
            1000
        )
        .unwrap_err(),
        BotLedgerError::Conflict
    );
    let mut changed = delivered.clone();
    changed.confirmed_at_ms = 1001;
    assert!(
        h.db.record_decision_delivery(
            &changed,
            &ticket,
            transition,
            DecisionDeliveryOutcome::Completed,
            step,
            0,
            1000
        )
        .is_err()
    );
    assert!(
        h.db.record_decision_delivery(
            &changed,
            &ticket,
            DecisionTransitionId::mint(),
            DecisionDeliveryOutcome::Retained,
            step,
            0,
            1000
        )
        .is_err(),
        "a changed receipt never replaces the retained one"
    );
    // The non-exclusive owner agrees on the same retained identity.
    assert_eq!(h.db.record_bot_delivery(&delivered).unwrap(), event);
    projection::apply(&mut h, &control);
    assert!(h.db.publication(event).unwrap().is_some());
}

#[test]
fn decision_delivery_rolls_back_receipt_and_control_together() {
    for moment in crate::Moment::ALL {
        let (mut h, ticket) = native_delivery_ready();
        let f = h.origin();
        let delivered = receipt(&h, f.attempt, f.generation);
        let transition = DecisionTransitionId::mint();
        let step = TimelineEntryId::mint();
        h.db.arm_crash(crate::CrashPoint {
            op: crate::WriteOp::RecordDecisionProjection,
            at: moment,
        });
        assert!(
            h.db.record_decision_delivery(
                &delivered,
                &ticket,
                transition,
                DecisionDeliveryOutcome::Completed,
                step,
                0,
                1000
            )
            .is_err()
        );
        let committed = moment == crate::Moment::AfterCommit;
        assert_eq!(
            h.db.bot_admission(f.run)
                .unwrap()
                .unwrap()
                .delivery
                .is_some(),
            committed
        );
        assert_eq!(
            h.db.decision_projection(f.owner, f.run, transition)
                .unwrap()
                .is_some(),
            committed
        );
        h.db.restart_in_memory();
        let first =
            h.db.record_decision_delivery(
                &delivered,
                &ticket,
                transition,
                DecisionDeliveryOutcome::Completed,
                step,
                0,
                1000,
            )
            .unwrap();
        assert_eq!(
            h.db.record_decision_delivery(
                &delivered,
                &ticket,
                transition,
                DecisionDeliveryOutcome::Completed,
                step,
                0,
                1000
            )
            .unwrap(),
            first
        );
    }
}

#[test]
fn decision_delivery_successor_requires_the_acknowledged_continuation() {
    let (mut h, record, clock) = lifecycle::koru_enrollment_ready();
    let f = h.origin();
    let record = lifecycle::enrolled(&mut h, &record);
    clock.store(1001, std::sync::atomic::Ordering::Relaxed);
    let release = lifecycle::origin_release(&mut h, &record, 1001);
    let record = h
        .db
        .record_decision_origin_stop(&expected(&record), DecisionTransitionId::mint(), &release)
        .unwrap();
    let input = h.answer_input(&record, true);
    let record =
        h.db.answer_bot_decision(&input, DecisionTransitionId::mint(), &h.proof, h.boot)
            .unwrap();
    let host = HostInstanceId::from_uuid(uuid::Uuid::new_v4());
    let record =
        h.db.reserve_decision_successor(
            &expected(&record),
            DecisionTransitionId::mint(),
            host,
            [42; 32],
            &h.proof,
            h.boot,
        )
        .unwrap();
    let (attempt, generation, _, creation) = record.resume.as_ref().unwrap().consumer_identity();
    let session = SessionId::mint();
    let record =
        h.db.bind_decision_successor(
            &expected(&record),
            DecisionTransitionId::mint(),
            creation,
            host,
            session,
        )
        .unwrap();
    h.db.advance(f.run, generation, crate::AttemptPhase::Starting)
        .unwrap();
    h.db.advance(f.run, generation, crate::AttemptPhase::Running)
        .unwrap();
    let initial = h.db.bot_admission(f.run).unwrap().unwrap().initial_send;
    let delivered = receipt(&h, attempt, generation);
    let transition = DecisionTransitionId::mint();
    let step = TimelineEntryId::mint();
    assert_eq!(
        h.db.record_decision_delivery(
            &delivered,
            &completion::ticket(&h),
            transition,
            DecisionDeliveryOutcome::Retained,
            step,
            0,
            1001
        )
        .unwrap_err(),
        BotLedgerError::Fenced,
        "an unconfirmed successor input is not a continuation owner"
    );
    assert!(
        h.db.bot_admission(f.run)
            .unwrap()
            .unwrap()
            .delivery
            .is_none()
    );
    assert!(
        h.db.decision_projection(f.owner, f.run, transition)
            .unwrap()
            .is_none()
    );
    let resume = record.resume.as_ref().unwrap();
    let turn = AgentTurnId::mint();
    h.db.record_decision_resume_input(
        &expected(&record),
        DecisionTransitionId::mint(),
        resume.input_key,
        resume.input_fingerprint,
        Some(&DecisionInputAcknowledgment {
            turn,
            receipt_digest: DecisionDigest::from_bytes([43; 32]),
        }),
    )
    .unwrap();
    let ticket = completion::ticket(&h);
    assert!(
        h.db.record_decision_delivery(
            &delivered,
            &ticket,
            DecisionTransitionId::mint(),
            DecisionDeliveryOutcome::Completed,
            step,
            0,
            1001
        )
        .is_err(),
        "completion waits for the decision to settle"
    );
    let (event, control) =
        h.db.record_decision_delivery(
            &delivered,
            &ticket,
            transition,
            DecisionDeliveryOutcome::Retained,
            step,
            0,
            1001,
        )
        .unwrap();
    let consumer = control.target.fence();
    assert_eq!(
        (
            consumer.attempt,
            consumer.generation,
            consumer.session,
            consumer.turn
        ),
        (attempt, generation, session, turn)
    );
    assert!(control.target.publications().is_empty());
    let admission = h.db.bot_admission(f.run).unwrap().unwrap();
    assert_eq!(admission.delivery, Some(delivered));
    assert_eq!(admission.delivery_publication, Some(event));
    assert_eq!(admission.initial_send, initial, "InitialSend is immutable");
}
