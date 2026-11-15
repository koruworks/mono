// SPDX-License-Identifier: MPL-2.0

use super::*;

fn request(h: &mut Harness, id: DecisionId) -> DecisionRecord {
    let old = h.read(id).unwrap();
    let f = h.origin();
    let tx = h.db.conn().unchecked_transaction().unwrap();
    let result = enroll_on(
        &tx,
        f.owner,
        f.run,
        id,
        old.receipt.state.revision,
        old.wait.revision,
        1001,
    )
    .unwrap();
    tx.commit().unwrap();
    result.record
}
fn ack(h: &mut Harness, id: DecisionId) -> DecisionRecord {
    let old = h.read(id).unwrap();
    let key = old.wait.enrollment_request.as_ref().unwrap().key.clone();
    let reply = BotWaitEnrollmentReply::Enrolled {
        receipt_digest: key.receipt_digest,
        key,
        ai_attempt: fixtures::v4(210),
    };
    let f = h.origin();
    let tx = h.db.conn().unchecked_transaction().unwrap();
    let result = record_enrollment_on(&tx, f.owner, f.run, id, &reply, 1002).unwrap();
    tx.commit().unwrap();
    result.record
}
pub(super) fn retired(h: &mut Harness) -> DecisionRecord {
    let first = h.prepare();
    let id = first.receipt.decision;
    h.published(id, false);
    request(h, id);
    let prior = ack(h, id);
    let f = h.origin();
    h.db.advance(f.run, f.generation, crate::AttemptPhase::Ended)
        .unwrap();
    let a = h.db.retire(f.run, f.generation).unwrap();
    let key = &prior.wait.enrollment_request.as_ref().unwrap().key;
    let release = OriginWaitStop::Confirmed {
        origin_fence: f,
        wait_id: prior.wait.wait,
        intent_digest: key.receipt_digest,
        enrollment_digest: key.receipt_digest,
        cause: BotWaitCleanupCause::DecisionWaitCleanup,
        creation_retirement: ObservedCreationRetirement {
            creation_key: a.provider_session.unwrap().idempotency,
            host: a.host_instance.unwrap(),
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
    // Handwritten C2 evidence fixture, not a claim that a real runtime stopped.
    h.db.conn().execute("UPDATE bot_wait_runtime SET phase='retired',revision=revision+1,release_evidence_json=?2,updated_at_ms=1003 WHERE wait_id=?1",params![prior.wait.wait.to_storage(),encoded(&release,DECISION_EVIDENCE_BYTES).unwrap()]).unwrap();
    h.db.conn().execute("UPDATE bot_decisions SET phase='held',revision=revision+1,updated_at_ms=1003 WHERE decision_id=?1",[id.to_storage()]).unwrap();
    h.read(id).unwrap()
}

#[test]
fn decision_enrollment_freezes_post_transition_revisions_and_exact_retry() {
    let mut h = Harness::new();
    let id = h.prepare().receipt.decision;
    h.published(id, false);
    let old = h.read(id).unwrap();
    let first = request(&mut h, id);
    let key = &first.wait.enrollment_request.as_ref().unwrap().key;
    assert_eq!(key.decision_revision, first.receipt.state.revision);
    assert_eq!(key.wait_revision, first.wait.revision);
    let tx = h.db.conn().unchecked_transaction().unwrap();
    let f = h.origin();
    let replay = enroll_on(
        &tx,
        f.owner,
        f.run,
        id,
        old.receipt.state.revision,
        old.wait.revision,
        9999,
    )
    .unwrap();
    assert_eq!(replay.outcome, WriteOutcome::AlreadyApplied);
    assert_eq!(replay.record, first);
    assert!(enroll_on(&tx, f.owner, f.run, id, rev(99), old.wait.revision, 9999).is_err());
}
#[test]
fn decision_enrollment_requires_published_holding_and_never_native_wait() {
    for native in [false, true] {
        let mut h = Harness::new();
        if native {
            h.native();
        }
        let r = h.prepare();
        let f = h.origin();
        let tx = h.db.conn().unchecked_transaction().unwrap();
        assert!(
            enroll_on(
                &tx,
                f.owner,
                f.run,
                r.receipt.decision,
                r.receipt.state.revision,
                r.wait.revision,
                1001
            )
            .is_err()
        );
    }
}
#[test]
fn decision_enrollment_rejects_missing_and_foreign_reply_without_receipt() {
    let mut h = Harness::new();
    let id = h.prepare().receipt.decision;
    h.published(id, false);
    let r = request(&mut h, id);
    let key = r.wait.enrollment_request.unwrap().key;
    let f = h.origin();
    let tx = h.db.conn().unchecked_transaction().unwrap();
    assert!(
        record_enrollment_on(
            &tx,
            f.owner,
            f.run,
            id,
            &BotWaitEnrollmentReply::Missing { key: key.clone() },
            1002
        )
        .is_err()
    );
    let mut foreign = key.clone();
    foreign.origin_turn = AgentTurnId::mint();
    assert!(
        record_enrollment_on(
            &tx,
            f.owner,
            f.run,
            id,
            &BotWaitEnrollmentReply::Enrolled {
                receipt_digest: key.receipt_digest,
                key: foreign,
                ai_attempt: fixtures::v4(210)
            },
            1002
        )
        .is_err()
    );
    drop(tx);
    assert!(h.read(id).unwrap().wait.enrollment.is_none());
}
#[test]
fn decision_enrollment_late_ack_records_history_without_reopening_stop() {
    let mut h = Harness::new();
    let id = h.prepare().receipt.decision;
    h.published(id, false);
    request(&mut h, id);
    let old = h.read(id).unwrap();
    let f = h.origin();
    let tx = h.db.conn().unchecked_transaction().unwrap();
    phase_on(
        &tx,
        f.owner,
        f.run,
        id,
        old.receipt.state.revision,
        DecisionStopCause::Canceled,
        1004,
    )
    .unwrap();
    tx.commit().unwrap();
    let completed = ack(&mut h, id);
    assert_eq!(completed.receipt.state.phase, DecisionPhase::StopPending);
    assert_eq!(completed.wait.phase, BotWaitPhase::CleanupPending);
    assert!(completed.wait.enrollment.is_some());
    assert!(completed.wait.release.is_none());
}

#[test]
fn decision_enrollment_requires_exact_retained_ai_attempt_and_preserves_unrelated_reads() {
    let mut h = Harness::new();
    let id = h.prepare().receipt.decision;
    h.published(id, false);
    let pending = request(&mut h, id);
    let f = h.origin();
    let key = pending
        .wait
        .enrollment_request
        .as_ref()
        .unwrap()
        .key
        .clone();
    let reply = |ai_attempt| BotWaitEnrollmentReply::Enrolled {
        key: key.clone(),
        receipt_digest: key.receipt_digest,
        ai_attempt,
    };
    {
        let tx = h.db.conn().unchecked_transaction().unwrap();
        assert!(matches!(
            record_enrollment_on(&tx, f.owner, f.run, id, &reply(fixtures::v4(220)), 1002),
            Err(BotLedgerError::Conflict)
        ));
    }
    assert!(h.read(id).unwrap().wait.enrollment.is_none());
    let mut submission = h.db.bot_admission(f.run).unwrap().unwrap().submission;
    let original = submission.clone();
    submission.provenance.ai_attempt_id = None;
    submission.provenance.assistant_message_id = None;
    h.db.conn()
        .execute(
            "UPDATE bot_admissions SET payload_json=?1",
            [serde_json::to_string(&submission).unwrap()],
        )
        .unwrap();
    // Pending history still reads without pretending an AI owner exists.
    assert!(h.read(id).unwrap().wait.enrollment.is_none());
    {
        let tx = h.db.conn().unchecked_transaction().unwrap();
        assert!(matches!(
            record_enrollment_on(&tx, f.owner, f.run, id, &reply(fixtures::v4(210)), 1002),
            Err(BotLedgerError::Conflict)
        ));
    }
    h.db.conn()
        .execute(
            "UPDATE bot_admissions SET payload_json=?1",
            [serde_json::to_string(&original).unwrap()],
        )
        .unwrap();
    ack(&mut h, id);
    h.db.conn()
        .execute_batch("DROP TRIGGER bot_wait_runtime_immutable")
        .unwrap();
    h.db.conn()
        .execute(
            "UPDATE bot_wait_runtime SET ai_cleanup_receipt_json=?1",
            [encoded(&reply(fixtures::v4(220)), DECISION_EVIDENCE_BYTES).unwrap()],
        )
        .unwrap();
    assert!(h.read(id).is_err());
    h.db.conn()
        .execute(
            "UPDATE bot_wait_runtime SET ai_cleanup_receipt_json=?1",
            [encoded(&reply(fixtures::v4(210)), DECISION_EVIDENCE_BYTES).unwrap()],
        )
        .unwrap();
    h.db.conn()
        .execute(
            "UPDATE bot_admissions SET payload_json=?1",
            [serde_json::to_string(&submission).unwrap()],
        )
        .unwrap();
    assert!(h.read(id).is_err());
}
#[test]
fn decision_phase_cas_fences_fresh_work_and_keeps_capacity_debt() {
    let mut h = Harness::new();
    let r = h.prepare();
    let f = h.origin();
    let tx = h.db.conn().unchecked_transaction().unwrap();
    assert!(
        phase_on(
            &tx,
            f.owner,
            f.run,
            r.receipt.decision,
            r.receipt.state.revision,
            DecisionStopCause::Expired,
            1001
        )
        .is_err()
    );
    let stopped = phase_on(
        &tx,
        f.owner,
        f.run,
        r.receipt.decision,
        r.receipt.state.revision,
        DecisionStopCause::Canceled,
        1001,
    )
    .unwrap();
    assert_eq!(stopped.record.wait.phase, BotWaitPhase::CleanupPending);
    assert!(stopped.record.receipt.closure.is_none());
    assert!(
        phase_on(
            &tx,
            f.owner,
            f.run,
            r.receipt.decision,
            r.receipt.state.revision,
            DecisionStopCause::AuthorityChanged,
            1002
        )
        .is_err()
    );
    tx.commit().unwrap();
    assert_eq!(h.count("bot_wait_runtime"), 1);
}
#[test]
fn decision_retired_origin_evidence_requires_retained_creation_key_and_host() {
    let mut h = Harness::new();
    let r = retired(&mut h);
    assert!(r.wait.release.is_some());
    let mut release = r.wait.release.unwrap();
    if let OriginWaitStop::Confirmed {
        creation_retirement,
        ..
    } = &mut release
    {
        creation_retirement.creation_key = IdempotencyKey::mint();
    }
    h.db.conn()
        .execute_batch("DROP TRIGGER bot_wait_release_evidence_immutable")
        .unwrap();
    h.db.conn()
        .execute(
            "UPDATE bot_wait_runtime SET release_evidence_json=?1",
            [encoded(&release, DECISION_EVIDENCE_BYTES).unwrap()],
        )
        .unwrap();
    assert!(h.read(r.receipt.decision).is_err());
}
