// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn decision_wait_execution_absence_is_owner_checked_and_retired_origin_is_explicit() {
    let mut h = Harness::new();
    let f = h.origin();
    assert!(
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .is_none()
    );
    let mut foreign = f.owner;
    foreign.user_scope = fixtures::v4(240).to_string().parse().unwrap();
    assert!(h.db.decision_wait_execution(foreign, f.run).is_err());
    let decision = enrollment::retired(&mut h);
    let observed =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap();
    assert_eq!(observed.wait, decision.wait);
    assert!(observed.current_attempt.retired);
    assert_eq!(observed.cleanup_target.attempt(), (f.attempt, f.generation));
    assert_eq!(observed.cleanup_target.session(), Some(f.session));
    assert!(observed.resume.is_none());
}

#[test]
fn decision_wait_execution_unknown_successor_never_reuses_retired_origin_cleanup() {
    let mut h = Harness::new();
    let retired = enrollment::retired(&mut h);
    let input = h.answer_input(&retired, true);
    let allowed = h.answer(&input, 1004).unwrap().record;
    let f = h.origin();
    // Partial owner fixture: use the real attempt insertion/binding owner,
    // then retain the not-yet-public C2 resume row without claiming a host ran.
    let tx = h.db.conn().unchecked_transaction().unwrap();
    let attempt = crate::attempts::open_on(&tx, f.run, 1005).unwrap();
    let creation = IdempotencyKey::mint();
    crate::attempts::bind_on(
        &tx,
        f.run,
        attempt.generation,
        ProviderSessionKey {
            idempotency: creation,
            session: None,
        },
        authority::attempt(&tx, &f).unwrap().host_instance.unwrap(),
        1005,
    )
    .unwrap();
    tx.execute("UPDATE bot_wait_runtime SET phase='resume_reserved',revision=revision+1,updated_at_ms=1005 WHERE wait_id=?1",[allowed.wait.wait.to_storage()]).unwrap();
    tx.execute("UPDATE bot_decisions SET phase='resume_prepared',revision=revision+1,updated_at_ms=1005 WHERE decision_id=?1",[allowed.receipt.decision.to_storage()]).unwrap();
    tx.execute("INSERT INTO bot_decision_resumes(decision_id,run_id,kind,origin_attempt_id,origin_generation,origin_session_id,consumer_attempt_id,consumer_generation,session_creation_key,input_key,input_fingerprint,state,created_at_ms,updated_at_ms) VALUES(?1,?2,'recorded_successor',?3,?4,?5,?6,?7,?8,?9,?10,'prepared',1005,1005)",params![allowed.receipt.decision.to_storage(),f.run.to_storage(),f.attempt.to_storage(),f.generation.get(),f.session.to_string(),attempt.attempt.to_storage(),attempt.generation.get(),creation.to_string(),IdempotencyKey::mint().to_string(),[3u8;32].as_slice()]).unwrap();
    tx.commit().unwrap();
    let observed =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap();
    assert_eq!(observed.current_attempt.attempt, attempt.attempt);
    assert!(!observed.current_attempt.retired);
    assert_eq!(observed.cleanup_target.session(), None);
    assert_eq!(observed.cleanup_target.creation_key(), creation);
    assert_ne!(observed.cleanup_target.attempt().0, f.attempt);
    assert!(
        observed.wait.release.is_some(),
        "historical origin retirement remains visible"
    );
    assert!(
        observed.wait.cleanup.is_none(),
        "origin proof is not successor cleanup"
    );
    h.db.conn()
        .execute_batch("PRAGMA foreign_keys=OFF")
        .unwrap();
    h.db.conn()
        .execute(
            "DELETE FROM run_attempts WHERE attempt_id=?1",
            [attempt.attempt.to_storage()],
        )
        .unwrap();
    assert!(
        h.db.decision_wait_execution(f.owner, f.run).is_err(),
        "missing current attempt is corruption, never process absence"
    );
}
