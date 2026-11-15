// SPDX-License-Identifier: MPL-2.0

use super::*;
fn damage(h: &Harness) {
    h.db.conn()
        .execute_batch("DROP TRIGGER bot_decisions_immutable; PRAGMA ignore_check_constraints=ON")
        .unwrap();
}

#[test]
fn decision_reader_bounds_body_before_decode_and_never_turns_corruption_into_missing() {
    let mut h = Harness::new();
    let id = h.prepare().receipt.decision;
    damage(&h);
    h.db.conn()
        .execute(
            "UPDATE bot_decisions SET binding_json=?1",
            [" ".repeat(DECISION_FRAME_BYTES + 1)],
        )
        .unwrap();
    assert!(matches!(
        h.read(id),
        Err(BotLedgerError::Ledger(LedgerError::Corrupt {
            column: "bounded_columns",
            ..
        }))
    ));
    h.db.conn()
        .execute("UPDATE bot_decisions SET binding_json='{}'", [])
        .unwrap();
    assert!(h.read(id).is_err());
}
#[test]
fn decision_reader_foreign_owner_fences_before_corrupt_body() {
    let mut h = Harness::new();
    let id = h.prepare().receipt.decision;
    damage(&h);
    h.db.conn()
        .execute("UPDATE bot_decisions SET binding_json='{}'", [])
        .unwrap();
    let mut owner = h.origin().owner;
    owner.device = DeviceId::from_uuid(fixtures::v4(240));
    assert!(matches!(
        h.db.bot_decision(owner, h.origin().run, id),
        Err(BotLedgerError::Fenced)
    ));
    assert!(
        h.db.bot_decision(h.origin().owner, h.origin().run, DecisionId::mint())
            .unwrap()
            .is_none()
    );
}
#[test]
fn decision_reader_rejects_scalar_type_digest_and_repeated_origin_corruption() {
    for statement in [
        "UPDATE bot_decisions SET revision=1.5",
        "UPDATE bot_decisions SET binding_digest=zeroblob(32)",
        "UPDATE bot_decisions SET origin_turn_id='00000000-0000-4000-8000-000000000099'",
        "UPDATE bot_decisions SET deadline_at_ms=deadline_at_ms-1",
    ] {
        let mut h = Harness::new();
        let id = h.prepare().receipt.decision;
        damage(&h);
        h.db.conn().execute(statement, []).unwrap();
        assert!(h.read(id).is_err(), "{statement}");
    }
}
#[test]
fn decision_reader_revalidates_original_admission_and_retained_policy_identity() {
    let mut h = Harness::new();
    let id = h.prepare().receipt.decision;
    h.db.conn()
        .execute("UPDATE bot_admissions SET payload_json='{}'", [])
        .unwrap();
    assert!(h.read(id).is_err());
    let mut h = Harness::new();
    let id = h.prepare().receipt.decision;
    damage(&h);
    let mut binding = h.binding.clone();
    if let DecisionBindingV1::KoruEdit(v) = &mut binding {
        v.global_policy.config_revision = rev(99);
    }
    h.db.conn()
        .execute(
            "UPDATE bot_decisions SET binding_json=?1,binding_digest=?2",
            params![
                encoded(&binding, DECISION_FRAME_BYTES).unwrap(),
                binding.digest().unwrap().as_bytes().as_slice()
            ],
        )
        .unwrap();
    assert!(h.read(id).is_err());
}

#[test]
fn decision_reader_requires_exact_satisfied_intent_without_equating_document_and_run() {
    let mut h = Harness::new();
    let id = h.prepare().receipt.decision;
    let (intent, request) = crate::intents::retained_bounded(h.db.conn(), h.origin().run).unwrap();
    assert_ne!(
        intent.run_document.unwrap().as_uuid(),
        h.origin().run.as_uuid()
    );
    assert!(h.read(id).is_ok());
    // The old public retry remains on the same request codec.
    assert_eq!(
        h.db.validate_intent(&request, intent.workspace).unwrap(),
        Some(intent)
    );
    for mutation in 0..4 {
        let mut changed = request.clone();
        match mutation {
            0 => changed.run = fixtures::run_id(220),
            1 => {
                changed.requested_by = ActorRef::Person {
                    profile: ProfileId::from_uuid(fixtures::v4(220)),
                }
            }
            2 => changed.context.clear(),
            _ => changed.objective = Objective::new("different retained task").unwrap(),
        }
        h.db.conn()
            .execute(
                "UPDATE run_intents SET payload_json=?1",
                [serde_json::to_string(&changed).unwrap()],
            )
            .unwrap();
        assert!(h.read(id).is_err(), "request mutation {mutation}");
    }
    h.db.conn()
        .execute(
            "UPDATE run_intents SET payload_json=?1,state='pending',run_document_id=NULL",
            [serde_json::to_string(&request).unwrap()],
        )
        .unwrap();
    assert!(h.read(id).is_err());
    h.db.conn()
        .execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM run_intents; PRAGMA foreign_keys=ON")
        .unwrap();
    assert!(h.read(id).is_err());
}

#[test]
fn decision_reader_bounds_intent_and_fences_foreign_owner_before_its_payload() {
    let mut h = Harness::new();
    let id = h.prepare().receipt.decision;
    h.db.conn()
        .execute(
            "UPDATE run_intents SET payload_json=?1",
            [" ".repeat(DECISION_FRAME_BYTES + 1)],
        )
        .unwrap();
    assert!(matches!(
        h.read(id),
        Err(BotLedgerError::Ledger(LedgerError::Corrupt {
            table: "run_intents",
            column: "bounded_identity"
        }))
    ));
    let mut owner = h.origin().owner;
    owner.device = DeviceId::from_uuid(fixtures::v4(220));
    assert!(matches!(
        h.db.bot_decision(owner, h.origin().run, id),
        Err(BotLedgerError::Fenced)
    ));
}

#[test]
fn decision_reader_rejects_missing_phase_companions_and_backward_retained_clock() {
    for statement in [
        "UPDATE bot_decisions SET phase='held'",
        "UPDATE bot_decisions SET updated_at_ms=created_at_ms-1",
        "UPDATE bot_decisions SET last_observed_at_ms=created_at_ms-1",
        "UPDATE bot_wait_runtime SET created_at_ms=created_at_ms-1",
    ] {
        let mut h = Harness::new();
        let id = h.prepare().receipt.decision;
        damage(&h);
        h.db.conn()
            .execute_batch("DROP TRIGGER bot_wait_runtime_immutable")
            .unwrap();
        h.db.conn().execute(statement, []).unwrap();
        assert!(h.read(id).is_err(), "{statement}");
    }
}
#[test]
fn decision_reader_answer_scalar_receipt_mismatch_is_not_hidden() {
    let mut h = Harness::new();
    h.native();
    let id = h.prepare().receipt.decision;
    h.published(id, true);
    let input = h.answer_input(&h.read(id).unwrap(), true);
    h.answer(&input, 1001).unwrap();
    damage(&h);
    h.db.conn()
        .execute(
            "UPDATE bot_decisions SET answer_request_id=?1",
            [DecisionAnswerId::mint().to_storage()],
        )
        .unwrap();
    assert!(h.read(id).is_err());
}
#[test]
fn decision_reader_native_journal_is_exact_choice_key_not_generic_resolved() {
    let mut h = Harness::new();
    h.native();
    let id = h.prepare().receipt.decision;
    h.published(id, true);
    let input = h.answer_input(&h.read(id).unwrap(), true);
    h.answer(&input, 1001).unwrap();
    h.db.conn()
        .execute_batch("DROP TRIGGER bot_decision_native_answers_immutable")
        .unwrap();
    h.db.conn()
        .execute(
            "UPDATE bot_decision_native_answers SET choice_id=?1",
            [ApprovalChoiceId::mint().to_string()],
        )
        .unwrap();
    assert!(h.read(id).is_err());
}
#[test]
fn decision_reader_resume_reuses_owning_attempt_and_requires_exact_ack_group() {
    let mut h = Harness::new();
    let r = enrollment::retired(&mut h);
    let input = h.answer_input(&r, true);
    let r = h.answer(&input, 1005).unwrap().record;
    let f = h.origin();
    let a = authority::attempt(h.db.conn(), &f).unwrap();
    let creation = a.provider_session.unwrap().idempotency;
    h.db.conn()
        .execute("UPDATE bot_wait_runtime SET phase='resume_reserved'", [])
        .unwrap();
    h.db.conn().execute("INSERT INTO bot_decision_resumes(decision_id,run_id,kind,origin_attempt_id,origin_generation,origin_session_id,consumer_attempt_id,consumer_generation,consumer_session_id,session_creation_key,input_key,input_fingerprint,state,created_at_ms,updated_at_ms) VALUES(?1,?2,'same_session',?3,?4,?5,?3,?4,?5,?6,?7,?8,'prepared',1005,1005)",params![r.receipt.decision.to_storage(),f.run.to_storage(),f.attempt.to_storage(),f.generation.get(),f.session.to_string(),creation.to_string(),IdempotencyKey::mint().to_string(),[3u8;32].as_slice()]).unwrap();
    h.db.conn()
        .execute("UPDATE bot_decisions SET phase='resume_prepared'", [])
        .unwrap();
    assert!(h.read(r.receipt.decision).unwrap().resume.is_some());
    h.db.conn()
        .execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    h.db.conn()
        .execute(
            "UPDATE bot_decision_resumes SET state='input_acknowledged'",
            [],
        )
        .unwrap();
    assert!(h.read(r.receipt.decision).is_err());
}
#[test]
fn decision_reader_historical_release_is_not_current_consumer_cleanup() {
    let mut h = Harness::new();
    let r = enrollment::retired(&mut h);
    let f = h.origin();
    let a = authority::attempt(h.db.conn(), &f).unwrap();
    let target = DecisionCleanupTarget::Origin {
        fence: f,
        creation_key: a.provider_session.unwrap().idempotency,
    };
    let cleanup = ConsumerCleanup::Confirmed {
        cleanup_revision: rev(10),
        target,
        creation_retirement: ObservedCreationRetirement {
            creation_key: a.provider_session.unwrap().idempotency,
            host: a.host_instance.unwrap(),
            result: ObservedRetirementResult::KnownEnded { session: f.session },
            observed_at_ms: 1000,
        },
        attempt_retirement: DecisionAttemptRetirement {
            run: f.run,
            attempt: f.attempt,
            generation: f.generation,
            retired_at_ms: 1000,
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
    h.db.conn()
        .execute(
            "UPDATE bot_wait_runtime SET cleanup_evidence_json=?1",
            [encoded(&cleanup, DECISION_EVIDENCE_BYTES).unwrap()],
        )
        .unwrap();
    assert!(h.read(r.receipt.decision).unwrap().wait.cleanup.is_some());
    h.db.conn()
        .execute_batch("DROP TRIGGER bot_wait_runtime_immutable")
        .unwrap();
    let mut changed = cleanup;
    if let ConsumerCleanup::Confirmed { target, .. } = &mut changed {
        *target = DecisionCleanupTarget::Consumer {
            origin: f,
            attempt: RunAttemptId::mint(),
            generation: AttemptGeneration::from_raw(2).unwrap(),
            creation_key: IdempotencyKey::mint(),
            session: None,
            input_key: IdempotencyKey::mint(),
            accepted_turn: None,
        };
    }
    h.db.conn()
        .execute(
            "UPDATE bot_wait_runtime SET cleanup_evidence_json=?1",
            [encoded(&changed, DECISION_EVIDENCE_BYTES).unwrap()],
        )
        .unwrap();
    assert!(h.read(r.receipt.decision).is_err());
}
