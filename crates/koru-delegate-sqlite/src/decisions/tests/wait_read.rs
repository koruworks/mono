// SPDX-License-Identifier: MPL-2.0

//! The delegate-owned `ReadBotDecisionWait` observation.
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

fn ack(record: &DecisionProjectionRecord) -> DecisionProjectionAcknowledgment {
    DecisionProjectionAcknowledgment {
        identity: record.identity.clone(),
        delta_digest: projection::prepared(record).delta_digest,
        observed_version: DecisionVersionVector::new(vec![3]).unwrap(),
    }
}

#[test]
fn decision_wait_read_is_stale_once_a_later_wait_owns_the_newest_projection() {
    // An enrolled Koru wait resumes on the recorded successor, consumes, settles
    // its effect and releases only its wait while the successor continues.
    let (mut h, record, request, fence, payload) = consume::ready();
    let f = h.origin();
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
    h.db.confirm_native_result(&ConfirmNativeResult {
        key: request.key.clone(),
        request_fingerprint: request.request_fingerprint,
        observation: NativeResultObservation::NotApplied {
            code: NativeResultRefusal::Conflict,
        },
    })
    .unwrap();
    let consumed = h.read(record.receipt.decision).unwrap();
    let settle = DecisionTransitionId::mint();
    h.db.settle_decision_effect(
        &expected(&consumed),
        settle,
        &DecisionRetainedEffect::NotApplied {
            key: request.key.clone(),
            code: NativeResultRefusal::Conflict,
        },
    )
    .unwrap();
    let last =
        h.db.decision_projection(f.owner, f.run, settle)
            .unwrap()
            .unwrap();
    let ticket =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap()
            .expected;
    let applied = projection::apply(&mut h, &last);
    h.db.settle_decision_wait(&ticket, &ack(&last)).unwrap();
    let settled = read(&h, &record).unwrap().unwrap();
    assert_eq!(
        settled.fresh_stop_eligibility,
        FreshWaitStopEligibility::Terminal
    );
    assert!(matches!(
        settled.publication,
        DecisionPublication::Applied { transition, .. } if transition == settle
    ));
    // A later native prompt on the acknowledged successor opens the next wait.
    h.native();
    let DecisionBindingV1::NativeApproval(mut later) = h.binding.clone() else {
        panic!("native")
    };
    later.origin_attempt = fence.attempt;
    later.origin_generation = fence.generation;
    later.origin_session = fence.session;
    later.origin_turn = fence.turn.unwrap();
    h.binding = DecisionBindingV1::NativeApproval(later);
    let mut base = projection::base(&h);
    projection::advance_base(&mut base, &applied);
    let next =
        h.db.prepare_bot_decision(&h.binding, &h.proof, h.boot, &base)
            .unwrap();
    assert_ne!(next.wait.wait, record.wait.wait);
    // The older wait's final target is no longer reachable by a bounded read:
    // the reader reports that instead of walking history or guessing.
    assert_eq!(
        h.db.read_bot_decision_wait(
            f.owner,
            f.bot,
            f.run,
            record.wait.wait,
            record.receipt.decision
        ),
        Err(BotLedgerError::Stale)
    );
    assert_eq!(
        h.db.read_bot_decision_wait(f.owner, f.bot, f.run, next.wait.wait, next.receipt.decision)
            .unwrap(),
        None,
        "the later native wait has no cleanup intent"
    );
}

#[test]
fn decision_wait_read_refuses_an_origin_drain_that_omits_a_retained_effect() {
    for complete in [true, false] {
        let mut h = Harness::new();
        let f = h.origin();
        // A drained-unknown native effect of the original attempt.
        let admission = h.db.bot_admission(f.run).unwrap().unwrap();
        let request = PrepareNativeResult {
            key: NativeResultKey {
                run: f.run,
                attempt: f.attempt,
                generation: f.generation,
                turn: f.turn,
                operation_id: uuid::Uuid::new_v4(),
            },
            kind: NativeResultOperation::CreatePage,
            destination: admission.submission.policy.destination,
            target: None,
            request_fingerprint: [6; 32],
            expected_revision: None,
            config_revision: h.proof.global.config_revision,
            policy_revision: h.proof.global.policy_revision,
            title: "PRIVATE-DRAIN".into(),
        };
        h.db.prepare_native_result(&request).unwrap();
        let effect =
            h.db.lease_native_result(
                &request.key,
                request.request_fingerprint,
                &BotExecutionFence {
                    owner: f.owner,
                    bot: f.bot,
                    run: f.run,
                    attempt: f.attempt,
                    generation: f.generation,
                    session: f.session,
                    turn: Some(f.turn),
                },
                &h.proof,
            )
            .unwrap()
            .intent;
        let drain = NativeEffectDrainReceipt {
            binding: effect.pending_effect().unwrap().binding,
            fenced_at: NativeEffectTimestamp::from_millis(999),
            drained_at: NativeEffectTimestamp::from_millis(1000),
        };
        h.db.confirm_native_result(&ConfirmNativeResult {
            key: request.key,
            request_fingerprint: request.request_fingerprint,
            observation: NativeResultObservation::DrainedUnknown {
                receipt: drain.clone(),
            },
        })
        .unwrap();
        h.db.prepare_bot_decision(&h.binding, &h.proof, h.boot, &projection::base(&h))
            .unwrap();
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
        let record = lifecycle::enrolled(&mut h, &record);
        let OriginWaitStop::Confirmed {
            origin_fence,
            wait_id,
            intent_digest,
            enrollment_digest,
            cause,
            creation_retirement,
            mut native_drain,
        } = lifecycle::origin_release(&mut h, &record, 1000)
        else {
            unreachable!()
        };
        if complete {
            native_drain.effects = DecisionList::new(vec![drain]).unwrap();
        }
        let release = OriginWaitStop::Confirmed {
            origin_fence,
            wait_id,
            intent_digest,
            enrollment_digest,
            cause,
            creation_retirement,
            native_drain,
        };
        if complete {
            h.db.record_decision_origin_stop(
                &expected(&record),
                DecisionTransitionId::mint(),
                &release,
            )
            .unwrap();
            let observed = read(&h, &record).unwrap().unwrap();
            assert_eq!(observed.origin_stop, release);
        } else {
            assert!(
                h.db.record_decision_origin_stop(
                    &expected(&record),
                    DecisionTransitionId::mint(),
                    &release
                )
                .is_err(),
                "the writer refuses an incomplete drain"
            );
            // Damaged storage: the same incomplete release written directly.
            h.db.conn()
                .execute(
                    "UPDATE bot_wait_runtime SET phase='retired',revision=revision+1,release_evidence_json=?2 WHERE wait_id=?1",
                    params![
                        record.wait.wait.to_storage(),
                        encoded(&release, DECISION_EVIDENCE_BYTES).unwrap()
                    ],
                )
                .unwrap();
            h.db.conn()
                .execute(
                    "UPDATE bot_decisions SET phase='held',revision=revision+1 WHERE decision_id=?1",
                    [record.receipt.decision.to_storage()],
                )
                .unwrap();
            assert!(
                read(&h, &record).is_err(),
                "an origin drain omitting a retained effect is never reported"
            );
        }
    }
}
