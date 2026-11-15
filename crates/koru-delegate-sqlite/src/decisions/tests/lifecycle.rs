// SPDX-License-Identifier: MPL-2.0
use super::*;

pub(super) fn expected(record: &DecisionRecord) -> DecisionExpected {
    DecisionExpected {
        owner: record.wait.origin.owner,
        run: record.wait.origin.run,
        decision: record.receipt.decision,
        revision: record.receipt.state.revision,
        binding_digest: record.receipt.binding_digest,
    }
}

pub(super) fn confirmed_cleanup(
    h: &mut Harness,
    now: i64,
) -> (DecisionWaitExpected, ConsumerCleanup) {
    let f = h.origin();
    let mut attempt = h.db.current_attempt(f.run).unwrap().unwrap();
    if !attempt.retired {
        h.db.advance(f.run, attempt.generation, crate::AttemptPhase::Ended)
            .unwrap();
        attempt = h.db.retire(f.run, attempt.generation).unwrap();
    }
    let observed =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap();
    let cleanup = ConsumerCleanup::Confirmed {
        cleanup_revision: observed.wait.revision,
        creation_retirement: ObservedCreationRetirement {
            creation_key: observed.cleanup_target.creation_key(),
            host: attempt.host_instance.unwrap(),
            result: match observed.cleanup_target.session() {
                Some(session) => ObservedRetirementResult::KnownEnded { session },
                None => ObservedRetirementResult::NoEvidence {},
            },
            observed_at_ms: now,
        },
        target: observed.cleanup_target,
        attempt_retirement: DecisionAttemptRetirement {
            run: f.run,
            attempt: attempt.attempt,
            generation: attempt.generation,
            retired_at_ms: attempt.updated_at_ms,
        },
        native_drain: DecisionNativeDrain {
            run: f.run,
            attempt: attempt.attempt,
            generation: attempt.generation,
            observed_at_ms: now,
            effects: DecisionList::default(),
        },
        retained_effect_outcome: None,
    };
    (observed.expected, cleanup)
}

pub(super) fn koru_enrollment_ready() -> (
    Harness,
    DecisionRecord,
    std::sync::Arc<std::sync::atomic::AtomicI64>,
) {
    use std::sync::{Arc, atomic::AtomicI64};
    let clock = Arc::new(AtomicI64::new(1000));
    let mut h = Harness::with_db(RunsDb::in_memory(Box::new(fixtures::FakeClock(
        clock.clone(),
    ))));
    let prepared =
        h.db.prepare_bot_decision(&h.binding, &h.proof, h.boot, &projection::base(&h))
            .unwrap();
    let f = h.origin();
    let initial =
        h.db.next_decision_projection(f.owner, f.run)
            .unwrap()
            .unwrap();
    projection::apply(&mut h, &initial);
    let ticket =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap()
            .expected;
    let record =
        h.db.enroll_decision_cleanup(&ticket, DecisionTransitionId::mint())
            .unwrap();
    assert_eq!(record.receipt.decision, prepared.receipt.decision);
    (h, record, clock)
}

pub(super) fn enrolled(h: &mut Harness, record: &DecisionRecord) -> DecisionRecord {
    let key = record.wait.enrollment_request.as_ref().unwrap().key.clone();
    h.db.record_decision_enrollment(
        &expected(record),
        DecisionTransitionId::mint(),
        &BotWaitEnrollmentReply::Enrolled {
            receipt_digest: key.receipt_digest,
            key,
            ai_attempt: fixtures::v4(210),
        },
    )
    .unwrap()
}

pub(super) fn origin_release(h: &mut Harness, record: &DecisionRecord, now: i64) -> OriginWaitStop {
    let f = h.origin();
    let mut attempt = h.db.current_attempt(f.run).unwrap().unwrap();
    if !attempt.retired {
        h.db.advance(f.run, attempt.generation, crate::AttemptPhase::Ended)
            .unwrap();
        attempt = h.db.retire(f.run, attempt.generation).unwrap();
    }
    let key = &record.wait.enrollment_request.as_ref().unwrap().key;
    OriginWaitStop::Confirmed {
        origin_fence: f,
        wait_id: record.wait.wait,
        intent_digest: key.receipt_digest,
        enrollment_digest: key.receipt_digest,
        cause: BotWaitCleanupCause::DecisionWaitCleanup,
        creation_retirement: ObservedCreationRetirement {
            creation_key: attempt.provider_session.unwrap().idempotency,
            host: attempt.host_instance.unwrap(),
            result: ObservedRetirementResult::KnownEnded { session: f.session },
            observed_at_ms: now,
        },
        native_drain: DecisionNativeDrain {
            run: f.run,
            attempt: f.attempt,
            generation: f.generation,
            observed_at_ms: now,
            effects: DecisionList::default(),
        },
    }
}

pub(super) fn native_ready() -> (Harness, DecisionRecord) {
    let mut h = Harness::new();
    h.native();
    let record =
        h.db.prepare_bot_decision(&h.binding, &h.proof, h.boot, &projection::base(&h))
            .unwrap();
    let f = h.origin();
    let initial =
        h.db.next_decision_projection(f.owner, f.run)
            .unwrap()
            .unwrap();
    projection::apply(&mut h, &initial);
    let record = h.read(record.receipt.decision).unwrap();
    (h, record)
}

#[test]
fn decision_native_public_answer_and_ack_commit_stable_resolution_without_ending_run() {
    for allow in [false, true] {
        let (mut h, record) = native_ready();
        let f = h.origin();
        let input = h.answer_input(&record, allow);
        let answer_transition = DecisionTransitionId::mint();
        let answered =
            h.db.answer_bot_decision(&input, answer_transition, &h.proof, h.boot)
                .unwrap();
        let Some(DecisionNativeResponse::Prepared { key }) = answered.native_response else {
            panic!("prepared response")
        };
        let response = DecisionNativeResponse::Acknowledged {
            key: key.clone(),
            acknowledgment: key,
            observed_at_ms: 1000,
        };
        let ack_transition = DecisionTransitionId::mint();
        h.revoke();
        let settled =
            h.db.record_decision_native_answer(f.owner, f.run, ack_transition, &response)
                .unwrap();
        assert_eq!(settled.receipt.state.phase, DecisionPhase::Settled);
        let projected =
            h.db.decision_projection(f.owner, f.run, ack_transition)
                .unwrap()
                .unwrap();
        assert_eq!(projected.target.publications().len(), 1);
        assert!(
            matches!(projected.target.publications()[0].fact,RunFact::ApprovalResolved {outcome,..} if outcome==if allow {ApprovalOutcome::Approved}else{ApprovalOutcome::Rejected})
        );
        assert!(!projected.target.effects().status.is_terminal());
        assert_eq!(
            h.db.bot_admission(f.run).unwrap().unwrap().state,
            BotAdmissionState::Executing
        );
        assert!(!h.db.current_attempt(f.run).unwrap().unwrap().retired);
        assert_eq!(
            h.db.record_decision_native_answer(f.owner, f.run, ack_transition, &response)
                .unwrap(),
            settled
        );
        assert_eq!(
            h.db.decision_projection(f.owner, f.run, ack_transition)
                .unwrap()
                .unwrap(),
            projected
        );
        assert_eq!(
            h.db.answer_bot_decision(&input, answer_transition, &h.proof, DelegateBootId::mint())
                .unwrap()
                .receipt
                .answer,
            settled.receipt.answer
        );
    }
}

#[test]
fn decision_native_public_answer_crash_rolls_back_receipt_and_control_together() {
    for moment in crate::Moment::ALL {
        let (mut h, record) = native_ready();
        let f = h.origin();
        let input = h.answer_input(&record, true);
        let transition = DecisionTransitionId::mint();
        h.db.arm_crash(crate::CrashPoint {
            op: crate::WriteOp::RecordDecisionProjection,
            at: moment,
        });
        assert!(
            h.db.answer_bot_decision(&input, transition, &h.proof, h.boot)
                .is_err()
        );
        let committed = moment == crate::Moment::AfterCommit;
        assert_eq!(
            h.read(record.receipt.decision)
                .unwrap()
                .receipt
                .answer
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
        let accepted =
            h.db.answer_bot_decision(&input, transition, &h.proof, h.boot)
                .unwrap();
        assert!(accepted.receipt.answer.is_some());
        assert!(
            h.db.decision_projection(f.owner, f.run, transition)
                .unwrap()
                .is_some()
        );
    }
}

#[test]
fn decision_successor_reservation_replays_before_revocation_and_keeps_initial_send() {
    let mut h = Harness::new();
    let f = h.origin();
    let original = h.db.bot_admission(f.run).unwrap().unwrap().initial_send;
    let reserved = observation::successor(&mut h);
    let expected = DecisionExpected {
        owner: f.owner,
        run: f.run,
        decision: reserved.receipt.decision,
        revision: rev(1),
        binding_digest: reserved.receipt.binding_digest,
    };
    let resume = reserved.resume.as_ref().unwrap();
    let (attempt, generation, _, _) = resume.consumer_identity();
    let host =
        h.db.current_attempt(f.run)
            .unwrap()
            .unwrap()
            .host_instance
            .unwrap();
    h.revoke();
    let tx = h.db.conn().unchecked_transaction().unwrap();
    let replay = reserve_successor_on(
        &tx,
        &expected,
        host,
        resume.input_fingerprint,
        &h.proof,
        DelegateBootId::mint(),
        i64::MAX,
    )
    .unwrap();
    assert_eq!(replay.outcome, WriteOutcome::AlreadyApplied);
    assert_eq!(replay.record, reserved);
    assert!(reserve_successor_on(&tx, &expected, host, [99; 32], &h.proof, h.boot, 1005).is_err());
    drop(tx);
    assert_ne!(attempt, f.attempt);
    assert_eq!(generation.get(), f.generation.get() + 1);
    assert_eq!(
        h.db.bot_admission(f.run).unwrap().unwrap().initial_send,
        original
    );
}

#[test]
fn decision_cleanup_source_carries_owner_and_replays_pair_after_terminal_supersession() {
    let (mut h, _) = native_ready();
    let f = h.origin();
    let expected =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap()
            .expected;
    let request =
        h.db.request_decision_cleanup(
            &expected,
            DecisionTransitionId::mint(),
            DecisionClosureReason::Canceled,
        )
        .unwrap();
    projection::apply(&mut h, &request);
    let before =
        h.db.decision_cleanup_ownership(f.owner, f.run)
            .unwrap()
            .unwrap();
    let mut base = projection::base(&h);
    projection::advance_base(&mut base, &request);
    let target = projection::source(&h, &base, 1);
    let transition = DecisionTransitionId::mint();
    let source =
        h.db.record_decision_source(f.owner, transition, expected.wait, &base, &target)
            .unwrap();
    let prefix = h.db.decision_projection_prefix(f.owner, f.run).unwrap();
    assert_eq!(prefix.len(), 2);
    assert_eq!(prefix[0], source);
    assert_eq!(
        prefix[1].identity.sequence.get(),
        source.identity.sequence.get() + 1
    );
    assert_eq!(
        h.db.decision_cleanup_ownership(f.owner, f.run)
            .unwrap()
            .unwrap()
            .origin,
        before.origin
    );
    assert_eq!(
        h.db.record_decision_source(f.owner, transition, expected.wait, &base, &target)
            .unwrap(),
        source
    );
    projection::apply(&mut h, &source);
    h.db.advance(f.run, f.generation, crate::AttemptPhase::Ended)
        .unwrap();
    let attempt = h.db.retire(f.run, f.generation).unwrap();
    let observed =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap();
    let cleanup = ConsumerCleanup::Confirmed {
        cleanup_revision: observed.wait.revision,
        target: observed.cleanup_target,
        creation_retirement: ObservedCreationRetirement {
            creation_key: attempt.provider_session.unwrap().idempotency,
            host: attempt.host_instance.unwrap(),
            result: ObservedRetirementResult::KnownEnded { session: f.session },
            observed_at_ms: 1000,
        },
        attempt_retirement: DecisionAttemptRetirement {
            run: f.run,
            attempt: f.attempt,
            generation: f.generation,
            retired_at_ms: attempt.updated_at_ms,
        },
        native_drain: DecisionNativeDrain {
            run: f.run,
            attempt: f.attempt,
            generation: f.generation,
            observed_at_ms: 1000,
            effects: DecisionList::default(),
        },
        retained_effect_outcome: None,
    };
    let final_transition = DecisionTransitionId::mint();
    let settled =
        h.db.record_decision_consumer_cleanup(
            &observed.expected,
            prefix[1].identity.transition,
            final_transition,
            &cleanup,
        )
        .unwrap();
    assert_eq!(settled.receipt.state.phase, DecisionPhase::Settled);
    assert_eq!(settled.wait.phase, BotWaitPhase::CleanupPending);
    assert!(
        h.db.decision_cleanup_ownership(f.owner, f.run)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        h.db.decision_projection(f.owner, f.run, prefix[1].identity.transition)
            .unwrap()
            .unwrap()
            .state,
        DecisionProjectionState::Superseded { .. }
    ));
    assert_eq!(
        h.db.record_decision_source(f.owner, transition, expected.wait, &base, &target)
            .unwrap()
            .identity,
        source.identity
    );
    assert_eq!(
        h.db.record_decision_consumer_cleanup(
            &observed.expected,
            prefix[1].identity.transition,
            final_transition,
            &cleanup
        )
        .unwrap(),
        settled
    );
    assert_eq!(
        h.db.bot_admission(f.run).unwrap().unwrap().state,
        BotAdmissionState::Executing,
        "publication and handle return still own admission"
    );
}

#[test]
fn decision_deny_origin_reconciliation_and_later_revision_keep_owner_through_source_cleanup() {
    let (mut h, record, clock) = koru_enrollment_ready();
    let f = h.origin();
    let record = enrolled(&mut h, &record);
    clock.store(1001, std::sync::atomic::Ordering::Relaxed);
    let release = origin_release(&mut h, &record, 1001);
    let release_transition = DecisionTransitionId::mint();
    let held =
        h.db.record_decision_origin_stop(&expected(&record), release_transition, &release)
            .unwrap();
    let input = h.answer_input(&held, false);
    let deny_transition = DecisionTransitionId::mint();
    let denied =
        h.db.answer_bot_decision(&input, deny_transition, &h.proof, h.boot)
            .unwrap();
    let replay =
        h.db.record_decision_origin_stop(&expected(&denied), release_transition, &release)
            .unwrap();
    assert_eq!(replay.receipt.answer, denied.receipt.answer);
    // Isolate the reader contract for a later retained revision: answer identity
    // remains immutable even when an independent observation advances revision.
    h.db.conn()
        .execute(
            "UPDATE bot_decisions SET revision=revision+1 WHERE decision_id=?1",
            [denied.receipt.decision.to_storage()],
        )
        .unwrap();
    let deny =
        h.db.decision_projection(f.owner, f.run, deny_transition)
            .unwrap()
            .unwrap();
    projection::apply(&mut h, &deny);
    let first =
        h.db.decision_cleanup_ownership(f.owner, f.run)
            .unwrap()
            .unwrap();
    assert!(
        matches!(first.origin,DecisionCleanupOrigin::Denied {answer,..} if answer==input.request)
    );
    let mut base = projection::base(&h);
    projection::advance_base(&mut base, &deny);
    let source =
        h.db.record_decision_source(
            f.owner,
            DecisionTransitionId::mint(),
            denied.wait.wait,
            &base,
            &projection::source(&h, &base, 1),
        )
        .unwrap();
    let carried =
        h.db.decision_cleanup_ownership(f.owner, f.run)
            .unwrap()
            .unwrap();
    assert_eq!(carried.origin, first.origin);
    projection::apply(&mut h, &source);
    let (ticket, proof) = confirmed_cleanup(&mut h, 1001);
    let final_id = DecisionTransitionId::mint();
    let settled =
        h.db.record_decision_consumer_cleanup(&ticket, carried.projection, final_id, &proof)
            .unwrap();
    assert_eq!(settled.receipt.answer, denied.receipt.answer);
    assert_eq!(
        settled.receipt.closure.unwrap().reason,
        DecisionClosureReason::Denied
    );
    let final_projection =
        h.db.decision_projection(f.owner, f.run, final_id)
            .unwrap()
            .unwrap();
    assert!(matches!(
        final_projection.target,
        RunProjectionTargetV1::Control {
            action: DecisionControlAction::Interrupt {
                prior_attempt: PriorAttempt::Ended
            },
            ..
        }
    ));
}

#[test]
fn decision_requested_cleanup_keeps_original_cas_through_enrollment_release_and_source() {
    let (mut h, record, clock) = koru_enrollment_ready();
    let f = h.origin();
    let ticket =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap()
            .expected;
    let request =
        h.db.request_decision_cleanup(
            &ticket,
            DecisionTransitionId::mint(),
            DecisionClosureReason::Canceled,
        )
        .unwrap();
    let first =
        h.db.decision_cleanup_ownership(f.owner, f.run)
            .unwrap()
            .unwrap();
    let record = enrolled(&mut h, &record);
    clock.store(1001, std::sync::atomic::Ordering::Relaxed);
    let release = origin_release(&mut h, &record, 1001);
    let stopped = h
        .db
        .record_decision_origin_stop(&expected(&record), DecisionTransitionId::mint(), &release)
        .unwrap();
    assert!(stopped.wait.revision.get() > ticket.revision.get() + 1);
    let owner =
        h.db.decision_cleanup_ownership(f.owner, f.run)
            .unwrap()
            .unwrap();
    assert_eq!(owner.origin, first.origin);
    let carrier =
        h.db.decision_projection(f.owner, f.run, owner.projection)
            .unwrap()
            .unwrap();
    projection::apply(&mut h, &carrier);
    let mut base = projection::base(&h);
    projection::advance_base(&mut base, &carrier);
    let source =
        h.db.record_decision_source(
            f.owner,
            DecisionTransitionId::mint(),
            stopped.wait.wait,
            &base,
            &projection::source(&h, &base, 1),
        )
        .unwrap();
    projection::apply(&mut h, &source);
    let owner =
        h.db.decision_cleanup_ownership(f.owner, f.run)
            .unwrap()
            .unwrap();
    assert_eq!(owner.origin, first.origin);
    let (ticket, proof) = confirmed_cleanup(&mut h, 1001);
    let final_id = DecisionTransitionId::mint();
    assert!(
        h.db.record_decision_consumer_cleanup(
            &ticket,
            request.identity.transition,
            final_id,
            &proof
        )
        .is_err(),
        "fresh finalization requires current carrier"
    );
    h.db.record_decision_consumer_cleanup(&ticket, owner.projection, final_id, &proof)
        .unwrap();
}

#[test]
fn decision_final_cleanup_refuses_old_origin_after_newer_settled_native_stop() {
    let (mut h, record) = native_ready();
    let f = h.origin();
    let input = h.answer_input(&record, true);
    let answered =
        h.db.answer_bot_decision(&input, DecisionTransitionId::mint(), &h.proof, h.boot)
            .unwrap();
    let Some(DecisionNativeResponse::Prepared { key }) = answered.native_response else {
        panic!("response")
    };
    h.db.record_decision_native_answer(
        f.owner,
        f.run,
        DecisionTransitionId::mint(),
        &DecisionNativeResponse::Acknowledged {
            key: key.clone(),
            acknowledgment: key,
            observed_at_ms: 1000,
        },
    )
    .unwrap();
    let ticket =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap()
            .expected;
    let old =
        h.db.request_decision_cleanup(
            &ticket,
            DecisionTransitionId::mint(),
            DecisionClosureReason::AuthorityChanged,
        )
        .unwrap();
    let ticket =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap()
            .expected;
    let current =
        h.db.request_decision_cleanup(
            &ticket,
            DecisionTransitionId::mint(),
            DecisionClosureReason::Canceled,
        )
        .unwrap();
    let (ticket, proof) = confirmed_cleanup(&mut h, 1000);
    let final_id = DecisionTransitionId::mint();
    assert!(
        h.db.record_decision_consumer_cleanup(&ticket, old.identity.transition, final_id, &proof)
            .is_err()
    );
    assert!(
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap()
            .wait
            .cleanup
            .is_none()
    );
    let settled = h
        .db
        .record_decision_consumer_cleanup(&ticket, current.identity.transition, final_id, &proof)
        .unwrap();
    assert_eq!(
        settled.receipt.closure.unwrap().reason,
        DecisionClosureReason::Completed,
        "native answer unchanged"
    );
    assert!(matches!(
        h.db.decision_projection(f.owner, f.run, final_id)
            .unwrap()
            .unwrap()
            .target,
        RunProjectionTargetV1::Control {
            action: DecisionControlAction::Stop {},
            ..
        }
    ));
    assert!(
        h.db.record_decision_consumer_cleanup(&ticket, old.identity.transition, final_id, &proof)
            .is_err(),
        "changed replay cannot select an earlier origin"
    );
}

#[test]
fn decision_pending_native_ack_supersedes_truthfully_but_prepared_ack_publishes_first() {
    for prepared in [false, true] {
        let (mut h, record) = native_ready();
        let f = h.origin();
        let input = h.answer_input(&record, true);
        let answered =
            h.db.answer_bot_decision(&input, DecisionTransitionId::mint(), &h.proof, h.boot)
                .unwrap();
        let Some(DecisionNativeResponse::Prepared { key }) = answered.native_response else {
            panic!("response")
        };
        let transition = DecisionTransitionId::mint();
        let settled =
            h.db.record_decision_native_answer(
                f.owner,
                f.run,
                transition,
                &DecisionNativeResponse::Acknowledged {
                    key: key.clone(),
                    acknowledgment: key,
                    observed_at_ms: 1000,
                },
            )
            .unwrap();
        let acknowledgment =
            h.db.decision_projection(f.owner, f.run, transition)
                .unwrap()
                .unwrap();
        let event = acknowledgment.target.publications()[0].event;
        if prepared {
            h.db.record_decision_preparation(f.owner, &projection::prepared(&acknowledgment))
                .unwrap();
        }
        let ticket =
            h.db.decision_wait_execution(f.owner, f.run)
                .unwrap()
                .unwrap()
                .expected;
        let cleanup =
            h.db.request_decision_cleanup(
                &ticket,
                DecisionTransitionId::mint(),
                DecisionClosureReason::Canceled,
            )
            .unwrap();
        assert_eq!(
            h.read(settled.receipt.decision).unwrap().receipt,
            settled.receipt
        );
        assert_eq!(
            h.read(settled.receipt.decision).unwrap().native_response,
            settled.native_response
        );
        assert!(h.db.publication(event).unwrap().is_none());
        let prefix = h.db.decision_projection_prefix(f.owner, f.run).unwrap();
        if prepared {
            assert_eq!(prefix.len(), 2);
            assert_eq!(prefix[0].identity.transition, transition);
            assert!(matches!(
                prefix[0].state,
                DecisionProjectionState::Prepared { .. }
            ));
            projection::apply(&mut h, &acknowledgment);
            assert!(h.db.publication(event).unwrap().is_some());
        } else {
            assert_eq!(prefix.len(), 1);
            assert_eq!(prefix[0].identity, cleanup.identity);
            assert!(matches!(
                h.db.decision_projection(f.owner, f.run, transition)
                    .unwrap()
                    .unwrap()
                    .state,
                DecisionProjectionState::Superseded { .. }
            ));
        }
        projection::apply(&mut h, &cleanup);
        assert_eq!(
            h.db.publication(event).unwrap().is_some(),
            prepared,
            "unapplied step is never claimed delivered"
        );
    }
}
