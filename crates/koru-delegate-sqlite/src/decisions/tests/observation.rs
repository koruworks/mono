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
    foreign.user_scope = serde_json::from_value(fixtures::v4(240).to_string().into()).unwrap();
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

pub(super) fn successor(h: &mut Harness) -> DecisionRecord {
    let f = h.origin();
    h.db.set_bot_active(f.run, f.attempt, f.generation, false, None).unwrap();
    let retired = enrollment::retired(h);
    let input = h.answer_input(&retired, true);
    let allowed = h.answer(&input, 1004).unwrap().record;
    // Real reservation/attempt owner; no provider or public transport is run.
    let tx = h.db.conn().unchecked_transaction().unwrap();
    reserve_successor_on(&tx,&DecisionExpected {
        owner:f.owner,run:f.run,decision:allowed.receipt.decision,revision:allowed.receipt.state.revision,
        binding_digest:allowed.receipt.binding_digest,
    },authority::attempt(&tx,&f).unwrap().host_instance.unwrap(),[3;32],&h.proof,h.boot,1005).unwrap();
    tx.commit().unwrap();
    h.read(allowed.receipt.decision).unwrap()
}

#[test]
fn decision_wait_execution_unknown_successor_never_reuses_retired_origin_cleanup() {
    let mut h = Harness::new();
    let record = successor(&mut h);
    let f = h.origin();
    let resume = record.resume.as_ref().unwrap();
    let (attempt, _, _, creation) = resume.consumer_identity();
    let observed =
        h.db.decision_wait_execution(f.owner, f.run)
            .unwrap()
            .unwrap();
    assert_eq!(observed.current_attempt.attempt, attempt);
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
            [attempt.to_storage()],
        )
        .unwrap();
    assert!(
        h.db.decision_wait_execution(f.owner, f.run).is_err(),
        "missing current attempt is corruption, never process absence"
    );
}
