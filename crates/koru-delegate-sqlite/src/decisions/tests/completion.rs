// SPDX-License-Identifier: MPL-2.0

//! Normal completion, wait settlement, no-decision cleanup, successor delivery
//! and the fresh-work fence. Real in-memory owners; no Store, provider or disk.
use super::*;
use crate::AttemptPhase;
use lifecycle::expected;

/// The returned-handle acknowledgment for a projection applied through
/// `projection::apply`, which always prepares the same delta and observes [3].
pub(super) fn ack(record: &DecisionProjectionRecord) -> DecisionProjectionAcknowledgment {
    DecisionProjectionAcknowledgment {
        identity: record.identity.clone(),
        delta_digest: projection::prepared(record).delta_digest,
        observed_version: DecisionVersionVector::new(vec![3]).unwrap(),
    }
}

/// A native prompt answered and acknowledged: the decision settles while the
/// native runtime continues. The returned Control is still pending.
pub(super) fn native_settled(allow: bool) -> (Harness, DecisionRecord, DecisionProjectionRecord) {
    let (mut h, record) = lifecycle::native_ready();
    let f = h.origin();
    let input = h.answer_input(&record, allow);
    let answered =
        h.db.answer_bot_decision(&input, DecisionTransitionId::mint(), &h.proof, h.boot)
            .unwrap();
    let Some(DecisionNativeResponse::Prepared { key }) = answered.native_response else {
        panic!("prepared response")
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
    assert_eq!(settled.receipt.state.phase, DecisionPhase::Settled);
    let control =
        h.db.decision_projection(f.owner, f.run, transition)
            .unwrap()
            .unwrap();
    (h, settled, control)
}

/// One accepted provider stream event carrying the given Run transition.
fn provider_source(
    h: &Harness,
    base: &DecisionPublicationBase,
    sequence: u64,
    event: RunEvent,
) -> RunProjectionTargetV1 {
    let mut target = projection::source(h, base, sequence);
    let RunProjectionTargetV1::Source {
        transitions,
        effects,
        ..
    } = &mut target
    else {
        unreachable!()
    };
    effects.status = transition_bot(&base.predecessor.status, &event).unwrap();
    *transitions = DecisionList::new(vec![event]).unwrap();
    target
}

pub(super) fn ticket(h: &Harness) -> DecisionWaitExpected {
    let f = h.origin();
    h.db.decision_wait_execution(f.owner, f.run)
        .unwrap()
        .unwrap()
        .expected
}

fn completed() -> RunEvent {
    RunEvent::TurnCompleted {
        originator: TurnOriginator::Orchestrator,
        artifacts_registered: 1,
    }
}

#[test]
fn decision_native_ack_then_provider_ended_source_settles_wait_only_with_final_ack() {
    let cases = [
        (AttemptPhase::Interrupted, completed(), false),
        (AttemptPhase::Unknown, completed(), false),
        // The Needed-you status is a cleanup outcome, never a normal end.
        (
            AttemptPhase::Ended,
            RunEvent::Interrupt {
                prior_attempt: PriorAttempt::Ended,
            },
            false,
        ),
        (AttemptPhase::Ended, completed(), true),
    ];
    for (phase, event, accepted) in cases {
        let (mut h, settled, control) = native_settled(true);
        let f = h.origin();
        projection::apply(&mut h, &control);
        let mut base = projection::base(&h);
        projection::advance_base(&mut base, &control);
        let target = provider_source(&h, &base, 1, event);
        let source =
            h.db.record_decision_source(
                f.owner,
                DecisionTransitionId::mint(),
                settled.wait.wait,
                &base,
                &target,
            )
            .unwrap();
        // An applied Source marker may be final: it retains the settled
        // decision revision, so no synthetic Preserve Control is needed.
        assert_eq!(
            source.identity.decision.as_ref().unwrap().revision,
            settled.receipt.state.revision
        );
        let expected_wait = ticket(&h);
        assert!(
            h.db.settle_decision_wait(&expected_wait, &ack(&source))
                .is_err(),
            "an owed Source has no returned final acknowledgment"
        );
        projection::apply(&mut h, &source);
        h.db.advance(f.run, f.generation, phase).unwrap();
        h.db.retire(f.run, f.generation).unwrap();
        assert!(
            h.db.settle_decision_wait(&expected_wait, &ack(&control))
                .is_err(),
            "an older applied target is not the final acknowledgment"
        );
        let mut changed = ack(&source);
        changed.observed_version = DecisionVersionVector::new(vec![9]).unwrap();
        assert!(h.db.settle_decision_wait(&expected_wait, &changed).is_err());
        let result = h.db.settle_decision_wait(&expected_wait, &ack(&source));
        if accepted {
            let wait = result.unwrap();
            assert_eq!(wait.phase, BotWaitPhase::Settled);
            assert!(
                h.db.decision_wait_execution(f.owner, f.run)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                h.db.bot_admission(f.run).unwrap().unwrap().state,
                BotAdmissionState::Executing,
                "admission settlement stays with its own owner"
            );
            assert_eq!(
                h.db.settle_decision_wait(&expected_wait, &ack(&source))
                    .unwrap(),
                wait
            );
        } else {
            assert!(result.is_err());
            assert!(
                h.db.decision_wait_execution(f.owner, f.run)
                    .unwrap()
                    .is_some()
            );
        }
    }
}

#[test]
fn decision_wait_settlement_retry_replays_history_after_a_later_wait() {
    let (mut h, settled, control) = native_settled(false);
    let f = h.origin();
    let applied = projection::apply(&mut h, &control);
    let first = ticket(&h);
    // Native Reject settles only the answer; the live runtime continues.
    let wait = h.db.settle_decision_wait(&first, &ack(&control)).unwrap();
    assert_eq!(wait.wait, settled.wait.wait);
    assert!(!h.db.current_attempt(f.run).unwrap().unwrap().retired);
    // A later native prompt opens the next wait on the same Run.
    let DecisionBindingV1::NativeApproval(mut next) = h.binding.clone() else {
        panic!("native")
    };
    next.prompt = ApprovalId::mint();
    h.binding = DecisionBindingV1::NativeApproval(next);
    let mut base = projection::base(&h);
    projection::advance_base(&mut base, &applied);
    let later =
        h.db.prepare_bot_decision(&h.binding, &h.proof, h.boot, &base)
            .unwrap();
    assert_ne!(later.wait.wait, wait.wait);
    let initial =
        h.db.next_decision_projection(f.owner, f.run)
            .unwrap()
            .unwrap();
    assert_eq!(
        initial.identity.sequence.get(),
        control.identity.sequence.get() + 1
    );
    // Exact historical retry reads its own retained settlement, never the
    // later wait or today's latest projection.
    assert_eq!(
        h.db.settle_decision_wait(&first, &ack(&control)).unwrap(),
        wait
    );
    let mut changed = ack(&control);
    changed.observed_version = DecisionVersionVector::new(vec![9]).unwrap();
    assert!(h.db.settle_decision_wait(&first, &changed).is_err());
    let earlier = control.target.predecessor().projection.as_ref().unwrap();
    let earlier =
        h.db.decision_projection(f.owner, f.run, earlier.transition)
            .unwrap()
            .unwrap();
    assert!(
        h.db.settle_decision_wait(&first, &ack(&earlier)).is_err(),
        "an earlier target of the settled wait is not its final acknowledgment"
    );
    let mut stale = first.clone();
    stale.revision = rev(stale.revision.get() + 1);
    assert!(h.db.settle_decision_wait(&stale, &ack(&control)).is_err());
    assert_eq!(
        ticket(&h).wait,
        later.wait.wait,
        "the later wait is untouched"
    );
}

#[test]
fn decision_confirmed_cleanup_settles_wait_with_needed_you_interrupt_or_stop() {
    for deny in [true, false] {
        let now = if deny { 1001 } else { 1000 };
        let (mut h, owner_projection) = if deny {
            let (mut h, record, clock) = lifecycle::koru_enrollment_ready();
            let f = h.origin();
            let record = lifecycle::enrolled(&mut h, &record);
            clock.store(1001, std::sync::atomic::Ordering::Relaxed);
            let release = lifecycle::origin_release(&mut h, &record, 1001);
            let held =
                h.db.record_decision_origin_stop(
                    &expected(&record),
                    DecisionTransitionId::mint(),
                    &release,
                )
                .unwrap();
            let input = h.answer_input(&held, false);
            let transition = DecisionTransitionId::mint();
            h.db.answer_bot_decision(&input, transition, &h.proof, h.boot)
                .unwrap();
            let denied =
                h.db.decision_projection(f.owner, f.run, transition)
                    .unwrap()
                    .unwrap();
            projection::apply(&mut h, &denied);
            (h, denied)
        } else {
            let (mut h, _) = lifecycle::native_ready();
            let request =
                h.db.request_decision_cleanup(
                    &ticket(&h),
                    DecisionTransitionId::mint(),
                    DecisionClosureReason::Canceled,
                )
                .unwrap();
            projection::apply(&mut h, &request);
            (h, request)
        };
        let f = h.origin();
        let owner =
            h.db.decision_cleanup_ownership(f.owner, f.run)
                .unwrap()
                .unwrap();
        assert_eq!(owner.projection, owner_projection.identity.transition);
        let (cleanup_ticket, proof) = lifecycle::confirmed_cleanup(&mut h, now);
        // The physical end alone is not the normal positive end: an open
        // cleanup obligation needs its own Confirmed proof first.
        assert!(
            h.db.settle_decision_wait(&cleanup_ticket, &ack(&owner_projection))
                .is_err()
        );
        let final_id = DecisionTransitionId::mint();
        h.db.record_decision_consumer_cleanup(&cleanup_ticket, owner.projection, final_id, &proof)
            .unwrap();
        let last =
            h.db.decision_projection(f.owner, f.run, final_id)
                .unwrap()
                .unwrap();
        let status = last.target.effects().status.clone();
        if deny {
            assert!(matches!(status, RunStatus::Interrupted { .. }));
            assert!(!status.is_terminal());
        } else {
            assert_eq!(status, RunStatus::Cancelled);
        }
        let after = ticket(&h);
        assert!(
            h.db.settle_decision_wait(&after, &ack(&last)).is_err(),
            "the final target must be applied and its handle returned"
        );
        projection::apply(&mut h, &last);
        assert!(
            h.db.settle_decision_wait(&cleanup_ticket, &ack(&last))
                .is_err(),
            "the cleanup advanced the wait revision"
        );
        let wait = h.db.settle_decision_wait(&after, &ack(&last)).unwrap();
        assert_eq!(wait.phase, BotWaitPhase::Settled);
        assert!(matches!(
            wait.cleanup,
            Some(ConsumerCleanup::Confirmed { .. })
        ));
        assert_eq!(
            h.db.settle_decision_wait(&after, &ack(&last)).unwrap(),
            wait
        );
    }
}

/// D's trusted base after one accepted Session event at `observed`.
pub(super) fn fallback_base(h: &Harness, observed: StreamSequence) -> DecisionPublicationBase {
    let mut base = projection::base(h);
    base.bookkeeping = Some(DecisionSourceBookkeeping {
        boundary: DecisionSourceBoundary {
            fence: h.origin(),
            producer_epoch: uuid::Uuid::new_v4(),
            stream: DecisionSourceStream::Session,
            sequence: observed,
            resulting: DecisionSourceFrontiers {
                agent_after: None,
                session_after: Some(observed),
            },
        },
        unflushed: DecisionList::default(),
        usage: None,
        allocated_steps: DecisionList::default(),
    });
    base
}

pub(super) fn sequence(n: u64) -> StreamSequence {
    StreamSequence::new(n).unwrap()
}

#[test]
fn decision_nondecision_fallback_retains_one_occupied_obligation_for_every_reason() {
    let reasons = [
        BotWaitReason::Capacity {},
        BotWaitReason::NativeExpired {},
        BotWaitReason::OverlappingPrompt {},
    ];
    for (index, reason) in reasons.into_iter().enumerate() {
        let other = reasons[(index + 1) % reasons.len()];
        let mut h = Harness::new();
        let f = h.origin();
        let observed = sequence(7);
        let base = fallback_base(&h, observed);
        assert!(
            h.db.begin_bot_wait_cleanup(
                &f,
                reason,
                sequence(8),
                DecisionTransitionId::mint(),
                &base
            )
            .is_err(),
            "the observed cursor must be the accepted session frontier"
        );
        assert!(
            h.db.begin_bot_wait_cleanup(
                &f,
                BotWaitReason::Decision {
                    decision: DecisionId::mint()
                },
                observed,
                DecisionTransitionId::mint(),
                &base
            )
            .is_err()
        );
        let mut foreign = f;
        foreign.session = SessionId::mint();
        assert!(
            h.db.begin_bot_wait_cleanup(
                &foreign,
                reason,
                observed,
                DecisionTransitionId::mint(),
                &base
            )
            .is_err()
        );
        assert_eq!(h.count("bot_wait_runtime"), 0);
        let transition = DecisionTransitionId::mint();
        let wait =
            h.db.begin_bot_wait_cleanup(&f, reason, observed, transition, &base)
                .unwrap();
        assert_eq!(wait.reason, reason);
        assert_eq!(wait.origin, f);
        assert_eq!(wait.phase, BotWaitPhase::CleanupPending);
        assert_eq!(wait.observed_sequence, Some(observed.get()));
        assert!(wait.enrollment_request.is_none() && wait.enrollment.is_none());
        assert_eq!(h.count("bot_decisions"), 0, "no decision is minted");
        let preserve =
            h.db.decision_projection(f.owner, f.run, transition)
                .unwrap()
                .unwrap();
        assert!(preserve.identity.decision.is_none());
        assert!(matches!(
            preserve.target,
            RunProjectionTargetV1::Control {
                action: DecisionControlAction::Preserve {},
                disposition: None,
                ..
            }
        ));
        let inventory = h.db.bot_execution_inventory(f.owner.device).unwrap();
        assert_eq!(inventory.occupied, 1, "the slot is never released");
        assert!(inventory.owned.iter().any(
            |o| o.execution.run == f.run && o.capacity == crate::BotExecutionCapacity::Occupied
        ));
        assert_eq!(
            h.db.begin_bot_wait_cleanup(&f, reason, observed, transition, &base)
                .unwrap(),
            wait
        );
        assert!(
            h.db.begin_bot_wait_cleanup(&f, other, observed, transition, &base)
                .is_err()
        );
        assert!(
            h.db.begin_bot_wait_cleanup(&f, reason, sequence(9), transition, &base)
                .is_err()
        );
        // A repeated overflow coalesces into the same retained obligation.
        let later = fallback_base(&h, sequence(9));
        assert_eq!(
            h.db.begin_bot_wait_cleanup(
                &f,
                other,
                sequence(9),
                DecisionTransitionId::mint(),
                &later
            )
            .unwrap(),
            wait
        );
        assert!(
            h.db.begin_bot_wait_cleanup(
                &foreign,
                reason,
                observed,
                DecisionTransitionId::mint(),
                &base
            )
            .is_err()
        );
        assert_eq!(h.count("bot_wait_runtime"), 1);
        assert_eq!(h.count("bot_decision_publications"), 1);
        // The obligation fences fresh work, including a new decision.
        assert!(
            h.db.prepare_bot_decision(&h.binding, &h.proof, h.boot, &projection::base(&h))
                .is_err()
        );
        assert_eq!(h.count("bot_decisions"), 0);
        let owner =
            h.db.decision_cleanup_ownership(f.owner, f.run)
                .unwrap()
                .unwrap();
        assert!(matches!(
            owner.origin,
            DecisionCleanupOrigin::NoDecision { reason: r, origin, .. } if r == reason && origin == f
        ));
        assert!(owner.expected.decision.is_none());
    }
}

/// The exact physical evidence for the current attempt, without retiring it.
pub(super) fn observed_cleanup(h: &Harness, now: i64) -> (DecisionWaitExpected, ConsumerCleanup) {
    let f = h.origin();
    let attempt = h.db.current_attempt(f.run).unwrap().unwrap();
    let observed =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap();
    let cleanup = ConsumerCleanup::Confirmed {
        cleanup_revision: observed.wait.revision,
        creation_retirement: ObservedCreationRetirement {
            creation_key: observed.cleanup_target.creation_key(),
            host: attempt.host_instance.unwrap(),
            result: ObservedRetirementResult::KnownEnded { session: f.session },
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

pub(super) fn execution_fence(f: &DecisionFence) -> BotExecutionFence {
    BotExecutionFence {
        owner: f.owner,
        bot: f.bot,
        run: f.run,
        attempt: f.attempt,
        generation: f.generation,
        session: f.session,
        turn: Some(f.turn),
    }
}

pub(super) fn result_request(h: &Harness) -> PrepareNativeResult {
    let f = h.origin();
    let admission = h.db.bot_admission(f.run).unwrap().unwrap();
    PrepareNativeResult {
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
        request_fingerprint: [5; 32],
        expected_revision: None,
        config_revision: h.proof.global.config_revision,
        policy_revision: h.proof.global.policy_revision,
        title: "PRIVATE-RESULT".into(),
    }
}

#[test]
fn decision_nondecision_cleanup_needs_drain_retirement_and_final_publication() {
    let mut h = Harness::new();
    let f = h.origin();
    let fence = execution_fence(&f);
    // A Result effect accepted and leased before the fallback.
    let request = result_request(&h);
    h.db.prepare_native_result(&request).unwrap();
    let lease =
        h.db.lease_native_result(&request.key, request.request_fingerprint, &fence, &h.proof)
            .unwrap();
    assert!(lease.execute.is_some());
    let observed = sequence(3);
    let begin = DecisionTransitionId::mint();
    h.db.begin_bot_wait_cleanup(
        &f,
        BotWaitReason::Capacity {},
        observed,
        begin,
        &fallback_base(&h, observed),
    )
    .unwrap();
    let preserve =
        h.db.decision_projection(f.owner, f.run, begin)
            .unwrap()
            .unwrap();
    projection::apply(&mut h, &preserve);
    let owner =
        h.db.decision_cleanup_ownership(f.owner, f.run)
            .unwrap()
            .unwrap();
    assert_eq!(owner.projection, begin);
    // Fresh work is fenced; the retained effect stays reconcilable.
    let mut fresh = request.clone();
    fresh.key.operation_id = uuid::Uuid::new_v4();
    assert_eq!(
        h.db.prepare_native_result(&fresh).unwrap_err(),
        BotLedgerError::Fenced
    );
    assert_eq!(
        h.db.prepare_native_result(&request).unwrap().key,
        request.key
    );
    assert!(
        h.db.lease_native_result(&request.key, request.request_fingerprint, &fence, &h.proof)
            .unwrap()
            .execute
            .is_none()
    );
    let final_id = DecisionTransitionId::mint();
    let (live_ticket, live) = observed_cleanup(&h, 1000);
    assert!(
        h.db.record_nondecision_cleanup(&live_ticket, begin, final_id, &live)
            .is_err(),
        "a live attempt is not retired"
    );
    h.db.advance(f.run, f.generation, AttemptPhase::Ended)
        .unwrap();
    h.db.retire(f.run, f.generation).unwrap();
    let (cleanup_ticket, proof) = observed_cleanup(&h, 1000);
    assert_eq!(
        h.db.record_nondecision_cleanup(&cleanup_ticket, begin, final_id, &proof)
            .unwrap_err(),
        BotLedgerError::Fenced,
        "a held native lease is not drained"
    );
    h.db.confirm_native_result(&ConfirmNativeResult {
        key: request.key.clone(),
        request_fingerprint: request.request_fingerprint,
        observation: NativeResultObservation::NotApplied {
            code: NativeResultRefusal::Conflict,
        },
    })
    .unwrap();
    h.db.arm_crash(crate::CrashPoint {
        op: crate::WriteOp::RecordDecisionProjection,
        at: crate::Moment::AfterWriteBeforeCommit,
    });
    assert!(
        h.db.record_nondecision_cleanup(&cleanup_ticket, begin, final_id, &proof)
            .is_err()
    );
    h.db.restart_in_memory();
    assert!(
        h.db.decision_projection(f.owner, f.run, final_id)
            .unwrap()
            .is_none()
    );
    assert!(
        ticket(&h) == cleanup_ticket,
        "evidence and Control roll back together"
    );
    let wait =
        h.db.record_nondecision_cleanup(&cleanup_ticket, begin, final_id, &proof)
            .unwrap();
    assert_eq!(wait.cleanup.as_ref(), Some(&proof));
    assert_eq!(
        h.db.record_nondecision_cleanup(&cleanup_ticket, begin, final_id, &proof)
            .unwrap(),
        wait
    );
    let (_, mut changed) = observed_cleanup(&h, 1000);
    if let ConsumerCleanup::Confirmed {
        cleanup_revision, ..
    } = &mut changed
    {
        *cleanup_revision = cleanup_ticket.revision;
    }
    if let ConsumerCleanup::Confirmed { native_drain, .. } = &mut changed {
        native_drain.observed_at_ms = 999;
    }
    assert!(
        h.db.record_nondecision_cleanup(&cleanup_ticket, begin, final_id, &changed)
            .is_err()
    );
    let last =
        h.db.decision_projection(f.owner, f.run, final_id)
            .unwrap()
            .unwrap();
    assert!(matches!(
        last.target,
        RunProjectionTargetV1::Control {
            action: DecisionControlAction::Interrupt {
                prior_attempt: PriorAttempt::Ended
            },
            ..
        }
    ));
    let after = ticket(&h);
    assert!(h.db.settle_decision_wait(&after, &ack(&last)).is_err());
    assert_eq!(
        h.db.settle_bot(f.run, crate::BotSettlement::Settled)
            .unwrap_err(),
        BotLedgerError::Fenced,
        "the occupied wait still owns admission"
    );
    projection::apply(&mut h, &last);
    let settled = h.db.settle_decision_wait(&after, &ack(&last)).unwrap();
    assert_eq!(settled.phase, BotWaitPhase::Settled);
    h.db.settle_bot(f.run, crate::BotSettlement::Settled)
        .unwrap();
    assert_eq!(
        h.db.bot_admission(f.run).unwrap().unwrap().state,
        BotAdmissionState::Settled
    );
}

#[test]
fn decision_fallback_on_existing_decision_keeps_owner_and_replays_its_exact_cause() {
    let (mut h, record) = lifecycle::native_ready();
    let f = h.origin();
    let observed = sequence(4);
    let base = fallback_base(&h, observed);
    let transition = DecisionTransitionId::mint();
    let wait =
        h.db.begin_bot_wait_cleanup(
            &f,
            BotWaitReason::OverlappingPrompt {},
            observed,
            transition,
            &base,
        )
        .unwrap();
    assert_eq!(wait.wait, record.wait.wait);
    assert_eq!(
        wait.reason,
        BotWaitReason::Decision {
            decision: record.receipt.decision
        }
    );
    assert_eq!(wait.phase, BotWaitPhase::CleanupPending);
    assert_eq!(h.count("bot_wait_runtime"), 1);
    let request =
        h.db.decision_projection(f.owner, f.run, transition)
            .unwrap()
            .unwrap();
    assert!(matches!(
        request.target,
        RunProjectionTargetV1::Control {
            action: DecisionControlAction::CleanupRequested {
                cause: DecisionClosureReason::AuthorityChanged,
                ..
            },
            ..
        }
    ));
    let state = h.read(record.receipt.decision).unwrap().receipt.state;
    assert_eq!(state.phase, DecisionPhase::StopPending);
    assert_eq!(state.authorization, DecisionAuthorization::Canceled);
    let owner =
        h.db.decision_cleanup_ownership(f.owner, f.run)
            .unwrap()
            .unwrap();
    assert!(matches!(
        owner.origin,
        DecisionCleanupOrigin::Requested {
            cause: DecisionClosureReason::AuthorityChanged,
            ..
        }
    ));
    // The exact retry follows the retained request, not a no-decision branch.
    assert_eq!(
        h.db.begin_bot_wait_cleanup(
            &f,
            BotWaitReason::OverlappingPrompt {},
            observed,
            transition,
            &base
        )
        .unwrap(),
        wait
    );
    assert!(
        h.db.begin_bot_wait_cleanup(
            &f,
            BotWaitReason::NativeExpired {},
            observed,
            transition,
            &base
        )
        .is_err(),
        "an open decision's expiry would have been a different cause"
    );
    let mut foreign = f;
    foreign.session = SessionId::mint();
    assert!(
        h.db.begin_bot_wait_cleanup(
            &foreign,
            BotWaitReason::OverlappingPrompt {},
            observed,
            transition,
            &base
        )
        .is_err()
    );
    // Another prompt strengthens the same obligation without a new row.
    let publications = h.count("bot_decision_publications");
    assert_eq!(
        h.db.begin_bot_wait_cleanup(
            &f,
            BotWaitReason::Capacity {},
            sequence(5),
            DecisionTransitionId::mint(),
            &base
        )
        .unwrap(),
        wait
    );
    assert_eq!(h.count("bot_decision_publications"), publications);
    // After cleanup and settlement the retained request still replays truthfully.
    projection::apply(&mut h, &request);
    let (cleanup_ticket, proof) = lifecycle::confirmed_cleanup(&mut h, 1000);
    let settled =
        h.db.record_decision_consumer_cleanup(
            &cleanup_ticket,
            owner.projection,
            DecisionTransitionId::mint(),
            &proof,
        )
        .unwrap();
    assert_eq!(settled.receipt.state.phase, DecisionPhase::Settled);
    assert_eq!(
        settled.receipt.closure.unwrap().reason,
        DecisionClosureReason::AuthorityChanged
    );
    let replay =
        h.db.begin_bot_wait_cleanup(
            &f,
            BotWaitReason::OverlappingPrompt {},
            observed,
            transition,
            &base,
        )
        .unwrap();
    assert_eq!(replay.wait, wait.wait);
    assert_eq!(replay.cleanup.as_ref(), Some(&proof));
    assert_eq!(h.count("bot_wait_runtime"), 1);
}

#[test]
fn decision_successor_delivery_requires_the_exact_acknowledged_continuation() {
    let (mut h, record, clock) = lifecycle::koru_enrollment_ready();
    let f = h.origin();
    let record = lifecycle::enrolled(&mut h, &record);
    clock.store(1001, std::sync::atomic::Ordering::Relaxed);
    let record = crate::test_support::stop_origin(&mut h.db, &record, 1001).unwrap();
    let host = HostInstanceId::from_uuid(uuid::Uuid::new_v4());
    let record = crate::test_support::bind_successor(
        &mut h.db,
        &record,
        host,
        SessionId::mint(),
        &h.proof,
        h.boot,
    )
    .unwrap();
    let (attempt, generation, _, _) = record.resume.as_ref().unwrap().consumer_identity();
    let admission = h.db.bot_admission(f.run).unwrap().unwrap();
    let provenance = &admission.submission.provenance;
    let receipt = BotDeliveryReceipt {
        run: f.run,
        attempt,
        generation,
        chat_id: provenance.chat_id,
        ai_attempt_id: provenance.ai_attempt_id.unwrap(),
        assistant_message_id: provenance.assistant_message_id.unwrap(),
        confirmed_at_ms: 1001,
    };
    assert_eq!(
        h.db.record_bot_delivery(&receipt).unwrap_err(),
        BotLedgerError::Fenced,
        "a bound session with unconfirmed input is not a continuation owner"
    );
    let mut original = receipt.clone();
    original.attempt = f.attempt;
    original.generation = f.generation;
    assert_eq!(
        h.db.record_bot_delivery(&original).unwrap_err(),
        BotLedgerError::Fenced,
        "the retired original cannot deliver the successor's response"
    );
    crate::test_support::acknowledge_successor(&mut h.db, &record, AgentTurnId::mint()).unwrap();
    let event = h.db.record_bot_delivery(&receipt).unwrap();
    assert_eq!(h.db.record_bot_delivery(&receipt).unwrap(), event);
    let mut changed = receipt.clone();
    changed.confirmed_at_ms = 1002;
    assert_eq!(
        h.db.record_bot_delivery(&changed).unwrap_err(),
        BotLedgerError::Conflict
    );
    let after = h.db.bot_admission(f.run).unwrap().unwrap();
    assert_eq!(
        after.initial_send, admission.initial_send,
        "the original InitialSend is never rewritten"
    );
    assert_eq!(after.delivery, Some(receipt));
    assert_eq!(after.delivery_publication, Some(event));
}

#[test]
fn decision_open_wait_fences_fresh_result_and_memory_but_keeps_retained_reconciliation() {
    let mut h = Harness::new();
    let f = h.origin();
    let fence = execution_fence(&f);
    let held = result_request(&h);
    h.db.prepare_native_result(&held).unwrap();
    h.db.lease_native_result(&held.key, held.request_fingerprint, &fence, &h.proof)
        .unwrap();
    let unleased = result_request(&h);
    h.db.prepare_native_result(&unleased).unwrap();
    let provenance =
        h.db.bot_admission(f.run)
            .unwrap()
            .unwrap()
            .submission
            .provenance;
    let memory = |operation: uuid::Uuid| AllocateMemoryEditRequest {
        key: NativeResultKey {
            run: f.run,
            attempt: f.attempt,
            generation: f.generation,
            turn: f.turn,
            operation_id: operation,
        },
        caller_fingerprint: [40; 32],
        subject: MemoryEditSubjectV1 {
            version: 1,
            owner: provenance.owner,
            bot: provenance.bot,
            submission: provenance.submission,
            captured_config_revision: provenance.config_revision,
            action: MemoryEditAction::Remember,
        },
        base: MemoryBaseSelector::OriginalSeed {},
        current_config_revision: h.proof.global.config_revision,
        current_policy_revision: h.proof.global.policy_revision,
        existing_fact_id: None,
    };
    let allocation = memory(uuid::Uuid::new_v4());
    let allocated =
        h.db.allocate_memory_edit(&allocation, fixtures::document(0xd0), &fence, &h.proof)
            .unwrap();
    h.db.prepare_bot_decision(&h.binding, &h.proof, h.boot, &projection::base(&h))
        .unwrap();
    let intents = h.count("native_result_intents");
    // Fresh Result and Memory work is fenced by the open wait.
    let fresh = result_request(&h);
    assert_eq!(
        h.db.prepare_native_result(&fresh).unwrap_err(),
        BotLedgerError::Fenced
    );
    assert_eq!(
        h.db.lease_native_result(
            &unleased.key,
            unleased.request_fingerprint,
            &fence,
            &h.proof
        )
        .unwrap_err(),
        BotLedgerError::Fenced
    );
    assert_eq!(
        h.db.allocate_memory_edit(
            &memory(uuid::Uuid::new_v4()),
            fixtures::document(0xd0),
            &fence,
            &h.proof
        )
        .unwrap_err(),
        BotLedgerError::Fenced
    );
    let resolution = MemoryBaseResolution::Resolved {
        authority: MemoryEditAuthorityV1 {
            version: 1,
            owner: provenance.owner,
            bot: provenance.bot,
            submission: provenance.submission,
            captured_config_revision: provenance.config_revision,
            creation_request: BotRequestId::mint(),
            workspace: fixtures::workspace(0xa2),
            memory_document: fixtures::document(0xd8),
            action: MemoryEditAction::Remember,
        },
        version: DocumentVersionId::from_uuid(uuid::Uuid::new_v4()),
        expected_revision: vec![2, 1, 2, 2, 4],
    };
    assert_eq!(
        h.db.resolve_memory_edit_base(
            &allocation.key,
            allocation.caller_fingerprint,
            &resolution,
            &fence,
            &h.proof
        )
        .unwrap_err(),
        BotLedgerError::Fenced
    );
    // Retained replay and reconciliation remain available.
    assert_eq!(h.db.prepare_native_result(&held).unwrap().key, held.key);
    assert_eq!(
        h.db.prepare_native_result(&unleased).unwrap().key,
        unleased.key
    );
    assert!(
        h.db.lease_native_result(&held.key, held.request_fingerprint, &fence, &h.proof)
            .unwrap()
            .execute
            .is_none()
    );
    assert_eq!(
        h.db.allocate_memory_edit(&allocation, fixtures::document(0xd0), &fence, &h.proof)
            .unwrap(),
        allocated
    );
    assert_eq!(
        h.db.memory_edit_allocation(
            f.run,
            allocation.key.operation_id,
            allocation.caller_fingerprint
        )
        .unwrap(),
        Some(allocated)
    );
    let confirmed =
        h.db.confirm_native_result(&ConfirmNativeResult {
            key: held.key.clone(),
            request_fingerprint: held.request_fingerprint,
            observation: NativeResultObservation::NotApplied {
                code: NativeResultRefusal::Conflict,
            },
        })
        .unwrap();
    assert_eq!(confirmed.stage, NativeResultStage::Refused);
    assert_eq!(h.count("native_result_intents"), intents);
    assert_eq!(h.count("bot_memory_edit_allocations"), 1);
}

#[test]
fn decision_consumed_companion_corruption_refuses_reads_replay_and_settlement() {
    let corruptions = [
        "UPDATE native_decision_leases SET consumer_turn_id='00000000-0000-4000-8000-0000000000fe'",
        "UPDATE native_decision_leases SET binding_digest=zeroblob(32)",
        "UPDATE native_decision_leases SET consumed_at_ms=consumed_at_ms+1",
        "DELETE FROM native_decision_leases",
    ];
    for corruption in corruptions {
        let (mut h, record, request, fence, payload) = consume::ready();
        let transition = DecisionTransitionId::mint();
        let first =
            h.db.consume_decision_edit(
                &expected(&record),
                transition,
                &request,
                &payload,
                &fence,
                &h.proof,
                h.boot,
            )
            .unwrap();
        assert!(first.execute.is_some());
        let refused = ConfirmNativeResult {
            key: request.key.clone(),
            request_fingerprint: request.request_fingerprint,
            observation: NativeResultObservation::NotApplied {
                code: NativeResultRefusal::Conflict,
            },
        };
        h.db.confirm_native_result(&refused).unwrap();
        assert!(h.db.native_result(&request.key).unwrap().is_some());
        let consumed = h.read(record.receipt.decision).unwrap();
        // Simulate damaged storage beneath the immutable-companion triggers.
        h.db.conn()
            .execute_batch(
                "DROP TRIGGER native_decision_leases_immutable;
                 DROP TRIGGER native_decision_leases_retain;",
            )
            .unwrap();
        h.db.conn().execute(corruption, []).unwrap();
        assert!(h.db.native_result(&request.key).is_err(), "{corruption}");
        assert!(
            h.db.consume_decision_edit(
                &expected(&record),
                transition,
                &request,
                &payload,
                &fence,
                &h.proof,
                h.boot
            )
            .is_err(),
            "{corruption}"
        );
        assert!(
            h.db.confirm_native_result(&refused).is_err(),
            "{corruption}"
        );
        assert!(
            h.db.settle_decision_effect(
                &expected(&consumed),
                DecisionTransitionId::mint(),
                &DecisionRetainedEffect::NotApplied {
                    key: request.key.clone(),
                    code: NativeResultRefusal::Conflict
                }
            )
            .is_err(),
            "{corruption}"
        );
    }
}
