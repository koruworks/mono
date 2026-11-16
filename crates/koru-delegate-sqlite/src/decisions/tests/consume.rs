// SPDX-License-Identifier: MPL-2.0
use super::*;
use lifecycle::expected;

pub(super) fn ready() -> (
    Harness,
    DecisionRecord,
    PrepareNativeResult,
    BotExecutionFence,
    Vec<u8>,
) {
    let (h, record, request, fence, payload, _clock) = ready_with_clock();
    (h, record, request, fence, payload)
}

/// [`ready`], keeping the ledger's clock so a test can move it.
fn ready_with_clock() -> (
    Harness,
    DecisionRecord,
    PrepareNativeResult,
    BotExecutionFence,
    Vec<u8>,
    std::sync::Arc<std::sync::atomic::AtomicI64>,
) {
    let (mut h, record, clock) = lifecycle::koru_enrollment_ready();
    let record = crate::test_support::record_enrollment(&mut h.db, &record).unwrap();
    let record = crate::test_support::stop_origin(&mut h.db, &record, 1000).unwrap();
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
    let (record, fence) =
        crate::test_support::acknowledge_successor(&mut h.db, &record, AgentTurnId::mint())
            .unwrap();
    let mut request = crate::test_support::consume_request(&record, &fence).unwrap();
    // Different map order, same semantic revision as the retained binding.
    request.expected_revision = Some(vec![2, 2, 4, 1, 2]);
    let DecisionBindingV1::KoruEdit(edit) = &record.receipt.binding else {
        panic!("edit")
    };
    let payload = edit.payload.as_slice().to_vec();
    (h, record, request, fence, payload, clock)
}

#[test]
fn decision_consume_is_atomic_across_native_lease_companion_receipt_and_control() {
    for moment in crate::Moment::ALL {
        let (mut h, record, request, fence, payload) = ready();
        let transition = DecisionTransitionId::mint();
        h.db.arm_crash(crate::CrashPoint {
            op: crate::WriteOp::RecordDecisionProjection,
            at: moment,
        });
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
            .is_err()
        );
        let committed = moment == crate::Moment::AfterCommit;
        assert_eq!(
            h.read(record.receipt.decision)
                .unwrap()
                .receipt
                .state
                .authorization
                == DecisionAuthorization::Consumed,
            committed
        );
        assert_eq!(
            h.db.native_result(&request.key).unwrap().is_some(),
            committed
        );
        let companions: i64 =
            h.db.conn()
                .query_row("SELECT count(*) FROM native_decision_leases", [], |r| {
                    r.get(0)
                })
                .unwrap();
        assert_eq!(companions, i64::from(committed));
        assert_eq!(
            h.db.decision_projection(fence.owner, fence.run, transition)
                .unwrap()
                .is_some(),
            committed
        );
        h.db.restart_in_memory();
        let retried =
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
        assert_eq!(retried.execute.is_some(), !committed);
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
            .unwrap()
            .execute
            .is_none()
        );
    }
}

#[test]
fn decision_consume_replays_first_key_after_revocation_and_rejects_changed_body() {
    let (mut h, record, request, fence, payload) = ready();
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
    h.revoke();
    let mut later = request.clone();
    later.key.attempt = RunAttemptId::mint();
    later.key.generation = AttemptGeneration::from_raw(99).unwrap();
    later.key.turn = AgentTurnId::mint();
    let replay =
        h.db.consume_decision_edit(
            &expected(&record),
            transition,
            &later,
            &payload,
            &fence,
            &h.proof,
            DelegateBootId::mint(),
        )
        .unwrap();
    assert_eq!(replay.intent, first.intent);
    assert!(replay.execute.is_none());
    assert!(
        h.db.consume_decision_edit(
            &expected(&record),
            transition,
            &later,
            b"changed",
            &fence,
            &h.proof,
            h.boot
        )
        .is_err()
    );
    later.title = "changed title".into();
    assert!(
        h.db.consume_decision_edit(
            &expected(&record),
            transition,
            &later,
            &payload,
            &fence,
            &h.proof,
            h.boot
        )
        .is_err()
    );
    let mut foreign = fence.clone();
    foreign.bot = BotId::mint();
    assert!(
        h.db.consume_decision_edit(
            &expected(&record),
            transition,
            &request,
            &payload,
            &foreign,
            &h.proof,
            h.boot
        )
        .is_err()
    );
}

#[test]
fn decision_consume_refuses_legacy_prepared_and_wrong_current_consumer() {
    let (mut h, record, request, fence, payload) = ready();
    let transition = DecisionTransitionId::mint();
    let mut wrong = request.clone();
    wrong.key.turn = AgentTurnId::mint();
    assert!(
        h.db.consume_decision_edit(
            &expected(&record),
            transition,
            &wrong,
            &payload,
            &fence,
            &h.proof,
            h.boot
        )
        .is_err()
    );
    wrong = request.clone();
    wrong.expected_revision = Some(vec![0xff]);
    assert!(
        h.db.consume_decision_edit(
            &expected(&record),
            transition,
            &wrong,
            &payload,
            &fence,
            &h.proof,
            h.boot
        )
        .is_err()
    );
    // An intentional collision fixture models a legacy intent; production
    // prepare refuses this overlap in either direction.
    crate::native_results::insert_prepared_on(h.db.conn(), &request, 1000).unwrap();
    let store: String =
        h.db.conn()
            .query_row(
                "SELECT store_request_id FROM native_result_intents WHERE run_id=?1",
                [fence.run.to_storage()],
                |r| r.get(0),
            )
            .unwrap();
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
        .is_err()
    );
    assert_eq!(
        h.db.conn()
            .query_row(
                "SELECT store_request_id FROM native_result_intents WHERE run_id=?1",
                [fence.run.to_storage()],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        store
    );
    assert_eq!(
        h.read(record.receipt.decision)
            .unwrap()
            .receipt
            .state
            .authorization,
        DecisionAuthorization::Granted
    );
}

#[test]
fn decision_resolved_effect_requires_publication_then_releases_only_wait_for_continuation() {
    let (mut h, record, request, fence, payload) = ready();
    let initial = h.db.bot_admission(fence.run).unwrap().unwrap().initial_send;
    h.db.set_bot_active(
        fence.run,
        fence.attempt,
        fence.generation,
        true,
        Some(&h.proof),
    )
    .unwrap();
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
    let mut unrelated = request.clone();
    unrelated.key.operation_id = uuid::Uuid::new_v4();
    assert!(h.db.prepare_native_result(&unrelated).is_err());
    h.db.confirm_native_result(&ConfirmNativeResult {
        key: request.key.clone(),
        request_fingerprint: request.request_fingerprint,
        observation: NativeResultObservation::NotApplied {
            code: NativeResultRefusal::Conflict,
        },
    })
    .unwrap();
    let consumed = h.read(record.receipt.decision).unwrap();
    let outcome = DecisionRetainedEffect::NotApplied {
        key: request.key.clone(),
        code: NativeResultRefusal::Conflict,
    };
    let transition = DecisionTransitionId::mint();
    let settled =
        h.db.settle_decision_effect(&expected(&consumed), transition, &outcome)
            .unwrap();
    assert!(h.db.prepare_native_result(&unrelated).is_err());
    let pending =
        h.db.decision_projection(fence.owner, fence.run, transition)
            .unwrap()
            .unwrap();
    let expected_wait =
        h.db.decision_wait_execution(fence.owner, fence.run)
            .unwrap()
            .unwrap()
            .expected;
    let prepared = projection::prepared(&pending);
    let ack = DecisionProjectionAcknowledgment {
        identity: pending.identity.clone(),
        delta_digest: prepared.delta_digest,
        observed_version: DecisionVersionVector::new(vec![3]).unwrap(),
    };
    assert!(h.db.settle_decision_wait(&expected_wait, &ack).is_err());
    projection::apply(&mut h, &pending);
    h.db.settle_decision_wait(&expected_wait, &ack).unwrap();
    assert!(
        h.db.decision_wait_execution(fence.owner, fence.run)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        h.db.bot_admission(fence.run).unwrap().unwrap().state,
        BotAdmissionState::Executing
    );
    assert_eq!(
        h.db.bot_admission(fence.run).unwrap().unwrap().initial_send,
        initial
    );
    assert!(!h.db.current_attempt(fence.run).unwrap().unwrap().retired);
    assert_eq!(
        h.db.bot_execution_inventory(fence.owner.device)
            .unwrap()
            .occupied,
        1
    );
    h.db.prepare_native_result(&unrelated).unwrap();
    let mut wrong = unrelated;
    wrong.key.operation_id = uuid::Uuid::new_v4();
    wrong.key.turn = AgentTurnId::mint();
    assert!(h.db.prepare_native_result(&wrong).is_err());
    assert_eq!(
        h.db.settle_decision_effect(&expected(&consumed), transition, &outcome)
            .unwrap()
            .receipt,
        settled.receipt
    );
}

#[test]
fn decision_consume_replay_under_a_new_transition_licenses_nothing_and_records_no_projection() {
    // A stateless retrying caller mints a new transition each time; the
    // replay must still be read-only, not a refused second Control.
    let (mut h, record, request, fence, payload) = ready();
    let first =
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
    assert!(first.execute.is_some());
    let count = |h: &Harness| -> i64 {
        h.db.conn()
            .query_row(
                "SELECT count(*) FROM bot_decision_publications WHERE run_id=?1",
                [fence.run.to_storage()],
                |r| r.get(0),
            )
            .unwrap()
    };
    let projections = count(&h);
    let replay =
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
    assert_eq!(replay.intent, first.intent);
    assert!(replay.execute.is_none());
    assert_eq!(count(&h), projections, "no new projection");
}

/// O150: a positively recorded save settles the decision once. A lost reply
/// is retried by a stateless caller under a new transition; that replay is a
/// read and records no second projection. The settled decision has no
/// deadline any more: expiry cannot reach it, while a person's Stop still can.
#[test]
fn decision_durable_save_settles_once_replays_under_a_new_transition_and_has_no_deadline() {
    let (mut h, record, request, fence, payload, clock) = ready_with_clock();
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
        observation: NativeResultObservation::Durable {
            evidence: koru_delegate_core::NativeResultReference {
                version_id: None,
                operation_id: request.key.operation_id,
                workspace: request.destination,
                document: request.target.unwrap(),
                version: vec![7, 7, 7],
                title: "Approved page".into(),
                confirmed_at_ms: 1000,
                kind: koru_delegate_core::NativeResultKind::EditedPage,
            },
        },
    })
    .unwrap();
    let outcome =
        h.db.decision_retained_effect(fence.owner, fence.run, record.receipt.decision)
            .unwrap()
            .unwrap();
    assert!(matches!(outcome, DecisionRetainedEffect::Durable { .. }));
    let consumed = h.read(record.receipt.decision).unwrap();
    let count = |h: &Harness| -> i64 {
        h.db.conn()
            .query_row(
                "SELECT count(*) FROM bot_decision_publications WHERE run_id=?1",
                [fence.run.to_storage()],
                |r| r.get(0),
            )
            .unwrap()
    };
    let before = count(&h);
    // The settlement commits; its caller never sees the reply (a lost reply
    // looks the same to the ledger).
    let _lost =
        h.db.settle_decision_effect(&expected(&consumed), DecisionTransitionId::mint(), &outcome)
            .unwrap();
    let settled = h.read(record.receipt.decision).unwrap();
    assert_eq!(settled.receipt.state.phase, DecisionPhase::Settled);
    assert_eq!(
        settled
            .receipt
            .closure
            .as_ref()
            .map(|closure| closure.reason),
        Some(DecisionClosureReason::Completed)
    );
    assert_eq!(count(&h), before + 1, "one DecisionResolved Control");
    // The retry under a new transition reads the retained settlement.
    let replay =
        h.db.settle_decision_effect(&expected(&consumed), DecisionTransitionId::mint(), &outcome)
            .unwrap();
    assert_eq!(replay.receipt, settled.receipt);
    assert_eq!(count(&h), before + 1, "no second projection");
    // Past the original deadline, expiry cannot settle it another way; a
    // person's Stop is still accepted and never reopens the settlement.
    clock.store(
        settled.receipt.deadline.deadline_at_ms + 1,
        std::sync::atomic::Ordering::SeqCst,
    );
    let ticket =
        h.db.decision_wait_execution(fence.owner, fence.run)
            .unwrap()
            .unwrap()
            .expected;
    assert!(
        h.db.request_decision_cleanup(
            &ticket,
            DecisionTransitionId::mint(),
            DecisionClosureReason::Expired
        )
        .is_err()
    );
    h.db.request_decision_cleanup(
        &ticket,
        DecisionTransitionId::mint(),
        DecisionClosureReason::Canceled,
    )
    .unwrap();
    assert_eq!(
        h.read(record.receipt.decision).unwrap().receipt,
        settled.receipt
    );
}

/// O152: final cleanup may pass over exactly one pending native effect, this
/// decision's own consumed save once its retained outcome is positively
/// Durable. Its unresolved lease still blocks, and so does any other held or
/// verified effect of the Run.
#[test]
fn decision_other_effects_pending_spares_only_the_decisions_own_durable_save() {
    let (mut h, record, request, fence, payload) = ready();
    let pending = |h: &Harness| {
        h.db.decision_other_effects_pending(fence.owner, fence.run, record.receipt.decision)
            .unwrap()
    };
    assert!(!pending(&h), "nothing consumed, nothing pending");
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
    assert!(pending(&h), "the consumed edit's lease is held: unresolved");
    h.db.confirm_native_result(&ConfirmNativeResult {
        key: request.key.clone(),
        request_fingerprint: request.request_fingerprint,
        observation: NativeResultObservation::Durable {
            evidence: koru_delegate_core::NativeResultReference {
                version_id: None,
                operation_id: request.key.operation_id,
                workspace: request.destination,
                document: request.target.unwrap(),
                version: vec![7, 7, 7],
                title: "Approved page".into(),
                confirmed_at_ms: 1000,
                kind: koru_delegate_core::NativeResultKind::EditedPage,
            },
        },
    })
    .unwrap();
    // The ordinary reader still reports it: its registration is owed.
    assert!(h.db.native_effects_pending(fence.run).unwrap());
    assert!(!pending(&h), "the decision's own Durable save is spared");
    // Another verified effect of the same Run, owing its registration.
    h.db.conn()
        .execute_batch(&format!(
            "CREATE TEMP TABLE o152 AS SELECT * FROM native_result_intents WHERE run_id='{run}';
             UPDATE o152 SET operation_id='{op}', store_request_id='{store}', lease_id='{lease}';
             INSERT INTO native_result_intents SELECT * FROM o152;
             DROP TABLE o152;",
            run = fence.run.to_storage(),
            op = uuid::Uuid::new_v4(),
            store = uuid::Uuid::new_v4(),
            lease = uuid::Uuid::new_v4(),
        ))
        .unwrap();
    assert!(pending(&h), "any other verified effect still blocks");
    // A held lease on another effect blocks as well.
    h.db.conn()
        .execute(
            "UPDATE native_result_intents SET stage='prepared',lease_state='held',
             verified_version=NULL,verified_title=NULL,confirmed_at_ms=NULL
             WHERE run_id=?1 AND operation_id<>?2",
            [fence.run.to_storage(), request.key.operation_id.to_string()],
        )
        .unwrap();
    assert!(pending(&h), "any other held lease still blocks");
}
