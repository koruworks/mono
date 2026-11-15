// SPDX-License-Identifier: MPL-2.0

use super::*;

fn retired(h: &mut Harness) -> DecisionRecord {
    let f = h.origin();
    // Use the owning active-clock transition before the scripted physical end.
    h.db.set_bot_active(f.run, f.attempt, f.generation, false, None)
        .unwrap();
    enrollment::retired(h)
}

#[test]
fn decision_inventory_retired_origin_is_owned_but_does_not_use_host_capacity() {
    let mut h = Harness::new();
    let f = h.origin();
    let record = retired(&mut h);
    let inventory = h.db.bot_execution_inventory(f.owner.device).unwrap();
    assert_eq!(inventory.occupied, 0);
    assert_eq!(inventory.owned.len(), 1);
    assert!(
        matches!(inventory.owned[0].capacity,crate::BotExecutionCapacity::QualifiedDecisionRelease{wait,..} if wait==record.wait.wait)
    );
    assert_eq!(h.db.active_bot_executions(f.owner.device).unwrap().len(), 1);
    assert!(h.db.bot_has_unsettled(f.owner, f.bot).unwrap());
    assert!(
        h.db.settle_bot(f.run, crate::BotSettlement::Settled)
            .is_err()
    );
    assert!(
        !h.db
            .recover_bot_reservation(&BotRunKey {
                owner: f.owner,
                bot: f.bot,
                run: f.run
            })
            .unwrap()
    );
}

#[test]
fn decision_inventory_canceled_fifty_retired_origins_keep_capacity_and_ownership() {
    let mut h = Harness::new();
    for n in 0..50 {
        let record = retired(&mut h);
        let f = h.origin();
        // The capacity owner is tested independently of D publication here.
        // This is a real phase body but not a claimed public Stop completion.
        let tx = h.db.conn().unchecked_transaction().unwrap();
        phase_on(
            &tx,
            f.owner,
            f.run,
            record.receipt.decision,
            record.receipt.state.revision,
            DecisionStopCause::Canceled,
            1004,
        )
        .unwrap();
        tx.commit().unwrap();
        let inventory = h.db.bot_execution_inventory(f.owner.device).unwrap();
        assert_eq!(inventory.occupied, 0);
        assert_eq!(inventory.owned.len(), n + 1);
        if n < 49 {
            h = Harness::with_db(h.db);
        }
    }
    let f = h.origin();
    let tx = h.db.conn().unchecked_transaction().unwrap();
    assert!(matches!(
        write::cap(&tx, f.owner),
        Err(BotLedgerError::Capacity)
    ));
    drop(tx);
    let h = Harness::with_db(h.db);
    let inventory =
        h.db.bot_execution_inventory(h.origin().owner.device)
            .unwrap();
    assert_eq!((inventory.owned.len(), inventory.occupied), (51, 1));
}

#[test]
fn decision_inventory_native_wait_and_unknown_successor_remain_occupied() {
    let mut h = Harness::new();
    h.native();
    let record = h.prepare();
    h.published(record.receipt.decision, true);
    assert_eq!(
        h.db.bot_execution_inventory(h.origin().owner.device)
            .unwrap()
            .occupied,
        1
    );
    let mut h = Harness::new();
    let f = h.origin();
    h.db.set_bot_active(f.run, f.attempt, f.generation, false, None)
        .unwrap();
    let record = observation::successor(&mut h);
    assert_eq!(
        h.db.bot_execution_inventory(f.owner.device)
            .unwrap()
            .occupied,
        1
    );
    let tx = h.db.conn().unchecked_transaction().unwrap();
    phase_on(
        &tx,
        f.owner,
        f.run,
        record.receipt.decision,
        record.receipt.state.revision,
        DecisionStopCause::Canceled,
        1006,
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(
        h.db.bot_execution_inventory(f.owner.device)
            .unwrap()
            .occupied,
        1,
        "old release never discounts unknown consumer"
    );
}

#[test]
fn decision_inventory_rejects_changed_origin_release_and_oversized_evidence() {
    for mutation in 0..3 {
        let mut h = Harness::new();
        let record = retired(&mut h);
        let f = h.origin();
        h.db.conn()
            .execute_batch(
                "PRAGMA ignore_check_constraints=ON; DROP TRIGGER bot_wait_runtime_immutable;",
            )
            .unwrap();
        if mutation == 0 {
            h.db.conn()
                .execute(
                    "UPDATE bot_wait_runtime SET release_evidence_json=?2 WHERE wait_id=?1",
                    params![record.wait.wait.to_storage(), "x".repeat(4097)],
                )
                .unwrap();
        } else if mutation == 1 {
            let mut release = record.wait.release.unwrap();
            let OriginWaitStop::Confirmed {
                creation_retirement,
                ..
            } = &mut release
            else {
                panic!("confirmed")
            };
            creation_retirement.creation_key = IdempotencyKey::mint();
            h.db.conn()
                .execute(
                    "UPDATE bot_wait_runtime SET release_evidence_json=?2 WHERE wait_id=?1",
                    params![
                        record.wait.wait.to_storage(),
                        encoded(&release, DECISION_EVIDENCE_BYTES).unwrap()
                    ],
                )
                .unwrap();
        } else {
            h.db.conn()
                .execute(
                    "UPDATE bot_wait_runtime SET session_id=?2 WHERE wait_id=?1",
                    params![record.wait.wait.to_storage(), SessionId::mint().to_string()],
                )
                .unwrap();
        }
        assert!(h.db.bot_execution_inventory(f.owner.device).is_err());
    }
}

#[test]
fn decision_inventory_does_not_decode_large_binding_for_capacity() {
    let mut h = Harness::new();
    let record = retired(&mut h);
    let f = h.origin();
    h.db.conn()
        .execute_batch("PRAGMA ignore_check_constraints=ON; DROP TRIGGER bot_decisions_immutable;")
        .unwrap();
    h.db.conn()
        .execute(
            "UPDATE bot_decisions SET binding_json=?2 WHERE decision_id=?1",
            params![
                record.receipt.decision.to_storage(),
                "x".repeat(4 * 1024 * 1024 + 1)
            ],
        )
        .unwrap();
    assert_eq!(
        h.db.bot_execution_inventory(f.owner.device)
            .unwrap()
            .occupied,
        0,
        "capacity is a narrow evidence projection, not payload authority"
    );
    assert!(
        h.read(record.receipt.decision).is_err(),
        "full authority reader still refuses corruption"
    );
}

#[test]
fn decision_inventory_requires_every_exact_native_drain_receipt() {
    let mut h = Harness::new();
    let f = h.origin();
    let request = PrepareNativeResult {
        key: NativeResultKey {
            run: f.run,
            attempt: f.attempt,
            generation: f.generation,
            turn: f.turn,
            operation_id: uuid::Uuid::new_v4(),
        },
        kind: NativeResultOperation::CreatePage,
        destination: h.proof.global.policy.destination,
        target: None,
        request_fingerprint: [9; 32],
        expected_revision: None,
        config_revision: 1,
        policy_revision: 1,
        title: "drain fixture".into(),
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
        fenced_at: NativeEffectTimestamp::from_millis(1001),
        drained_at: NativeEffectTimestamp::from_millis(1002),
    };
    h.db.confirm_native_result(&ConfirmNativeResult {
        key: request.key,
        request_fingerprint: request.request_fingerprint,
        observation: NativeResultObservation::DrainedUnknown {
            receipt: drain.clone(),
        },
    })
    .unwrap();
    // Preserve the fixture's empty release to prove omitted retained effects
    // are rejected by the owning reader and the capacity projection alike.
    let first = h.prepare();
    let id = first.receipt.decision;
    h.published(id, false);
    enrollment::request(&mut h, id);
    let prior = enrollment::ack(&mut h, id);
    h.db.set_bot_active(f.run, f.attempt, f.generation, false, None)
        .unwrap();
    h.db.advance(f.run, f.generation, crate::AttemptPhase::Ended)
        .unwrap();
    let attempt = h.db.retire(f.run, f.generation).unwrap();
    let key = &prior.wait.enrollment_request.as_ref().unwrap().key;
    let mut release = OriginWaitStop::Confirmed {
        origin_fence: f,
        wait_id: prior.wait.wait,
        intent_digest: key.receipt_digest,
        enrollment_digest: key.receipt_digest,
        cause: BotWaitCleanupCause::DecisionWaitCleanup,
        creation_retirement: ObservedCreationRetirement {
            creation_key: attempt.provider_session.unwrap().idempotency,
            host: attempt.host_instance.unwrap(),
            result: ObservedRetirementResult::KnownExited {
                session: f.session,
                code: Some(0),
            },
            observed_at_ms: 1003,
        },
        native_drain: DecisionNativeDrain {
            run: f.run,
            attempt: f.attempt,
            generation: f.generation,
            observed_at_ms: 1003,
            effects: DecisionList::default(),
        },
    };
    h.db.conn().execute("UPDATE bot_wait_runtime SET phase='retired',revision=revision+1,release_evidence_json=?2,updated_at_ms=1003 WHERE wait_id=?1",params![prior.wait.wait.to_storage(),encoded(&release,DECISION_EVIDENCE_BYTES).unwrap()]).unwrap();
    h.db.conn().execute("UPDATE bot_decisions SET phase='held',revision=revision+1,updated_at_ms=1003 WHERE decision_id=?1",[id.to_storage()]).unwrap();
    assert!(h.db.bot_execution_inventory(f.owner.device).is_err());
    assert!(h.read(id).is_err());
    h.db.conn()
        .execute_batch("DROP TRIGGER bot_wait_runtime_immutable")
        .unwrap();
    let OriginWaitStop::Confirmed { native_drain, .. } = &mut release else {
        unreachable!()
    };
    native_drain.effects = DecisionList::new(vec![drain.clone()]).unwrap();
    h.db.conn()
        .execute(
            "UPDATE bot_wait_runtime SET release_evidence_json=?2 WHERE wait_id=?1",
            params![
                prior.wait.wait.to_storage(),
                encoded(&release, DECISION_EVIDENCE_BYTES).unwrap()
            ],
        )
        .unwrap();
    assert_eq!(
        h.db.bot_execution_inventory(f.owner.device)
            .unwrap()
            .occupied,
        0
    );
    let OriginWaitStop::Confirmed { native_drain, .. } = &mut release else {
        unreachable!()
    };
    let mut wrong = drain;
    wrong.binding.request_fingerprint = [7; 32];
    native_drain.effects = DecisionList::new(vec![wrong]).unwrap();
    h.db.conn()
        .execute(
            "UPDATE bot_wait_runtime SET release_evidence_json=?2 WHERE wait_id=?1",
            params![
                prior.wait.wait.to_storage(),
                encoded(&release, DECISION_EVIDENCE_BYTES).unwrap()
            ],
        )
        .unwrap();
    assert!(h.db.bot_execution_inventory(f.owner.device).is_err());
}
