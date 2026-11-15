// SPDX-License-Identifier: MPL-2.0

use super::*;

fn expected(record: &DecisionRecord) -> DecisionExpected {
    let f = record.receipt.binding.origin();
    DecisionExpected {
        owner: f.owner,
        run: f.run,
        decision: record.receipt.decision,
        revision: record.receipt.state.revision,
        binding_digest: record.receipt.binding_digest,
    }
}

fn bind(
    h: &mut Harness,
    record: &DecisionRecord,
    session: SessionId,
) -> Result<DecisionMutation, BotLedgerError> {
    let resume = record.resume.as_ref().unwrap();
    let (_, generation, _, key) = resume.consumer_identity();
    let host = crate::attempts::retained_bounded(h.db.conn(), h.origin().run, generation)
        .unwrap()
        .unwrap()
        .host_instance
        .unwrap();
    let tx = h.db.conn().unchecked_transaction().unwrap();
    let value = bind_resume_session_on(&tx, &expected(record), key, host, session, 1006)?;
    tx.commit().unwrap();
    Ok(value)
}

fn input(
    h: &mut Harness,
    record: &DecisionRecord,
    ack: Option<&DecisionInputAcknowledgment>,
) -> Result<DecisionMutation, BotLedgerError> {
    let resume = record.resume.as_ref().unwrap();
    let tx = h.db.conn().unchecked_transaction().unwrap();
    let value = record_resume_input_on(
        &tx,
        &expected(record),
        resume.input_key,
        resume.input_fingerprint,
        ack,
        1007,
    )?;
    tx.commit().unwrap();
    Ok(value)
}

#[test]
fn decision_resume_reconciles_unknown_creation_before_consumer_cleanup() {
    let mut h = Harness::new();
    let record = observation::successor(&mut h);
    let session = SessionId::mint();
    let bound = bind(&mut h, &record, session).unwrap().record;
    assert_eq!(
        bound.resume.as_ref().unwrap().consumer_identity().2,
        Some(session)
    );
    assert_eq!(
        bind(&mut h, &record, session).unwrap().outcome,
        WriteOutcome::AlreadyApplied
    );
    assert!(bind(&mut h, &record, SessionId::mint()).is_err());
    let f = h.origin();
    let current =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap();
    assert_eq!(current.cleanup_target.session(), Some(session));
    assert!(current.wait.release.is_some());
    assert!(current.wait.cleanup.is_none());
}

#[test]
fn decision_resume_input_unknown_keeps_identity_and_exact_ack_is_immutable() {
    let mut h = Harness::new();
    let record = observation::successor(&mut h);
    assert!(
        input(&mut h, &record, None).is_err(),
        "no fabricated session before send"
    );
    let record = bind(&mut h, &record, SessionId::mint()).unwrap().record;
    let unknown = input(&mut h, &record, None).unwrap().record;
    assert!(matches!(
        unknown.resume.as_ref().unwrap().state,
        DecisionResumeState::InputUnconfirmed {}
    ));
    let ack = DecisionInputAcknowledgment {
        turn: AgentTurnId::mint(),
        receipt_digest: DecisionDigest::from_bytes([8; 32]),
    };
    let accepted = input(&mut h, &record, Some(&ack)).unwrap().record;
    assert_eq!(accepted.receipt.state.phase, DecisionPhase::AwaitRetry);
    let original =
        h.db.bot_admission(h.origin().run)
            .unwrap()
            .unwrap()
            .initial_send
            .unwrap();
    assert_eq!(
        original.turn,
        Some(h.origin().turn),
        "original InitialSend is never rewritten"
    );
    h.revoke();
    assert_eq!(input(&mut h, &record, Some(&ack)).unwrap().record, accepted);
    assert_eq!(input(&mut h, &record, None).unwrap().record, accepted);
    let changed = DecisionInputAcknowledgment {
        turn: AgentTurnId::mint(),
        ..ack
    };
    assert!(input(&mut h, &record, Some(&changed)).is_err());
}

#[test]
fn decision_resume_late_input_ack_records_history_without_reopening_stop() {
    let mut h = Harness::new();
    let record = observation::successor(&mut h);
    let record = bind(&mut h, &record, SessionId::mint()).unwrap().record;
    let f = h.origin();
    let tx = h.db.conn().unchecked_transaction().unwrap();
    phase_on(
        &tx,
        f.owner,
        f.run,
        record.receipt.decision,
        record.receipt.state.revision,
        DecisionStopCause::Canceled,
        1007,
    )
    .unwrap();
    tx.commit().unwrap();
    h.revoke();
    let ack = DecisionInputAcknowledgment {
        turn: AgentTurnId::mint(),
        receipt_digest: DecisionDigest::from_bytes([8; 32]),
    };
    let late = input(&mut h, &record, Some(&ack)).unwrap().record;
    assert_eq!(late.receipt.state.phase, DecisionPhase::StopPending);
    assert_eq!(late.wait.phase, BotWaitPhase::CleanupPending);
    // Stop closed the retained input identity in the same writer. The late
    // exact acknowledgment is kept as history on the closed row; it never
    // reopens the resume into an acknowledged, consumable state.
    assert_eq!(
        late.resume.unwrap().state,
        DecisionResumeState::Closed {
            acknowledgment: Some(ack.clone())
        }
    );
    assert_eq!(
        input(&mut h, &record, Some(&ack)).unwrap().outcome,
        WriteOutcome::AlreadyApplied
    );
}
