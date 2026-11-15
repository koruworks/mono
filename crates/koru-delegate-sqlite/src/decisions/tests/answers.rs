// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn decision_answer_native_allow_and_reject_retain_one_exact_dispatch_key() {
    for allow in [true, false] {
        let mut h = Harness::new();
        h.native();
        let id = h.prepare().receipt.decision;
        h.published(id, true);
        let old = h.read(id).unwrap();
        let input = h.answer_input(&old, allow);
        let first = h.answer(&input, 1001).unwrap();
        assert_eq!(
            first.record.receipt.state.phase,
            DecisionPhase::NativeDispatch
        );
        assert_eq!(
            first.record.receipt.state.authorization,
            if allow {
                DecisionAuthorization::Granted
            } else {
                DecisionAuthorization::Denied
            }
        );
        assert!(matches!(
            first.record.native_response,
            Some(DecisionNativeResponse::Prepared { .. })
        ));
        assert_eq!(first.record.wait.phase, BotWaitPhase::Fenced);
        assert!(first.record.wait.cleanup.is_none());
        let replay = h.answer(&input, 1002).unwrap();
        assert_eq!(replay.outcome, WriteOutcome::AlreadyApplied);
        assert_eq!(replay.record, first.record);
        assert_eq!(h.count("bot_decision_native_answers"), 1);
    }
}
#[test]
fn decision_answer_koru_allow_and_deny_need_retired_origin_and_preserve_proof() {
    for allow in [true, false] {
        let mut h = Harness::new();
        let old = enrollment::retired(&mut h);
        let input = h.answer_input(&old, allow);
        let result = h.answer(&input, 1005).unwrap();
        assert_eq!(
            result.record.receipt.state.authorization,
            if allow {
                DecisionAuthorization::Granted
            } else {
                DecisionAuthorization::Denied
            }
        );
        assert_eq!(result.record.wait.release, old.wait.release);
        assert!(result.record.native_response.is_none());
        assert_eq!(
            result.record.wait.phase,
            if allow {
                BotWaitPhase::Retired
            } else {
                BotWaitPhase::CleanupPending
            }
        );
    }
}
#[test]
fn decision_answer_exact_replay_survives_new_policy_boot_and_deadline() {
    let mut h = Harness::new();
    h.native();
    let id = h.prepare().receipt.decision;
    h.published(id, true);
    let old = h.read(id).unwrap();
    let input = h.answer_input(&old, true);
    let first = h.answer(&input, 1001).unwrap();
    h.revoke();
    h.boot = DelegateBootId::mint();
    assert_eq!(h.answer(&input, i64::MAX).unwrap().record, first.record);
    let mut changed = input;
    changed.authority_digest = DecisionDigest::from_bytes([1; 32]);
    assert!(matches!(
        h.answer(&changed, 1002),
        Err(BotLedgerError::Conflict)
    ));
}
#[test]
fn decision_answer_early_or_stale_never_commits_and_can_reuse_uncommitted_request() {
    let mut h = Harness::new();
    h.native();
    let first = h.prepare();
    let mut input = h.answer_input(&first, true);
    assert!(matches!(h.answer(&input, 1001), Err(BotLedgerError::Stale)));
    assert!(
        h.read(first.receipt.decision)
            .unwrap()
            .receipt
            .answer
            .is_none()
    );
    h.published(first.receipt.decision, true);
    assert!(matches!(h.answer(&input, 1001), Err(BotLedgerError::Stale)));
    input.expected_revision = rev(2);
    assert!(h.answer(&input, 1001).is_ok());
}
#[test]
fn decision_answer_boot_rollback_deadline_and_profile_mismatch_refuse() {
    for case in 0..4 {
        let mut h = Harness::new();
        h.native();
        let id = h.prepare().receipt.decision;
        h.published(id, true);
        let r = h.read(id).unwrap();
        let mut input = h.answer_input(&r, true);
        let mut now = 1001;
        match case {
            0 => h.boot = DelegateBootId::mint(),
            1 => now = 999,
            2 => now = r.receipt.deadline.deadline_at_ms,
            _ => input.human = ProfileId::from_uuid(fixtures::v4(240)),
        };
        assert!(h.answer(&input, now).is_err());
        assert!(h.read(id).unwrap().receipt.answer.is_none());
        assert_eq!(h.count("bot_decision_native_answers"), 0);
    }
}
#[test]
fn decision_answer_unoffered_choice_never_creates_native_journal() {
    let mut h = Harness::new();
    h.native();
    let id = h.prepare().receipt.decision;
    h.published(id, true);
    let r = h.read(id).unwrap();
    let mut input = h.answer_input(&r, true);
    if let DecisionAnswerChoice::Native { offered } = &mut input.choice {
        offered.id = ApprovalChoiceId::mint();
    }
    assert!(h.answer(&input, 1001).is_err());
    assert_eq!(h.count("bot_decision_native_answers"), 0);
}
#[test]
fn decision_answer_rollback_keeps_receipt_and_native_key_unaccepted() {
    let mut h = Harness::new();
    h.native();
    let id = h.prepare().receipt.decision;
    h.published(id, true);
    let r = h.read(id).unwrap();
    let input = h.answer_input(&r, true);
    let tx = h.db.conn().unchecked_transaction().unwrap();
    answer_on(&tx, &input, &h.proof, h.boot, 1001).unwrap();
    drop(tx);
    assert_eq!(h.read(id).unwrap(), r);
    assert_eq!(h.count("bot_decision_native_answers"), 0);
}
