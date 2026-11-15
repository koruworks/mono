// SPDX-License-Identifier: MPL-2.0

//! The two read-only `BotLedger` forwards the runtime uses: the retained
//! decision record and the durably acknowledged successor continuation.
use super::*;

fn stranger(owner: BotOwner) -> BotOwner {
    BotOwner {
        user_scope: owner.user_scope,
        device: DeviceId::from_uuid(uuid::Uuid::new_v4()),
    }
}

#[test]
fn bot_decision_read_is_owner_fenced_and_redacted() {
    let (h, record, _) = lifecycle::koru_enrollment_ready();
    let f = h.origin();
    let read = BotLedger::bot_decision(&h.db, f.owner, f.run, record.receipt.decision)
        .unwrap()
        .unwrap();
    assert_eq!(read, record);
    let DecisionBindingV1::KoruEdit(edit) = &read.receipt.binding else {
        panic!("edit binding")
    };
    assert_eq!(edit.payload.as_slice(), b"PRIVATE-EDIT");
    assert!(
        !format!("{read:?}").contains("PRIVATE"),
        "content is redacted"
    );
    assert_eq!(
        BotLedger::bot_decision(&h.db, stranger(f.owner), f.run, record.receipt.decision)
            .unwrap_err(),
        BotLedgerError::Fenced
    );
    assert!(
        BotLedger::bot_decision(&h.db, f.owner, f.run, DecisionId::mint())
            .unwrap()
            .is_none()
    );
}

#[test]
fn acknowledged_continuation_names_only_an_acknowledged_recorded_successor() {
    let (mut h, record, clock) = lifecycle::koru_enrollment_ready();
    let f = h.origin();
    let read = |h: &Harness, owner: BotOwner| {
        BotLedger::decision_acknowledged_continuation(&h.db, owner, f.run)
    };
    // No successor: the current attempt is the original.
    assert_eq!(read(&h, f.owner).unwrap(), None);
    let record = lifecycle::enrolled(&mut h, &record);
    clock.store(1001, std::sync::atomic::Ordering::Relaxed);
    let record = crate::test_support::stop_origin(&mut h.db, &record, 1001).unwrap();
    assert_eq!(
        read(&h, f.owner).unwrap(),
        None,
        "a retired origin is not a successor"
    );
    let host = HostInstanceId::from_uuid(uuid::Uuid::new_v4());
    let session = SessionId::mint();
    let record =
        crate::test_support::bind_successor(&mut h.db, &record, host, session, &h.proof, h.boot)
            .unwrap();
    assert_eq!(
        read(&h, f.owner).unwrap(),
        None,
        "a bound but unacknowledged successor has no continuation"
    );
    let turn = AgentTurnId::mint();
    let (record, fence) =
        crate::test_support::acknowledge_successor(&mut h.db, &record, turn).unwrap();
    assert_eq!(
        read(&h, f.owner).unwrap(),
        Some(DecisionAcknowledgedContinuation {
            decision: record.receipt.decision,
            attempt: fence.attempt,
            generation: fence.generation,
            session,
            turn,
        })
    );
    assert_eq!(
        read(&h, stranger(f.owner)).unwrap_err(),
        BotLedgerError::Fenced
    );
    // The original InitialSend is untouched by the read.
    let initial =
        h.db.bot_admission(f.run)
            .unwrap()
            .unwrap()
            .initial_send
            .unwrap();
    assert_eq!(initial.attempt, f.attempt);
}
