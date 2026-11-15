// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn decision_prepare_replays_before_policy_clock_and_id_allocation() {
    let mut h = Harness::new();
    let first = h.prepare();
    h.revoke();
    h.boot = DelegateBootId::mint();
    let again = h.prepare_at(i64::MAX).unwrap();
    assert_eq!(again.outcome, WriteOutcome::AlreadyApplied);
    assert_eq!(again.record, first);
    assert_eq!(h.count("bot_decisions"), 1);
    assert_eq!(h.count("bot_wait_runtime"), 1);
    assert_eq!(h.count("native_result_intents"), 0);
}
#[test]
fn decision_prepare_changed_operation_body_conflicts_without_replacement() {
    let mut h = Harness::new();
    let first = h.prepare();
    h.edit().payload = DecisionBytes::new(b"CHANGED".to_vec()).unwrap();
    assert!(matches!(h.prepare_at(1001), Err(BotLedgerError::Conflict)));
    assert_eq!(h.read(first.receipt.decision).unwrap(), first);
}
#[test]
fn decision_prepare_one_open_run_keeps_original_question() {
    let mut h = Harness::new();
    let first = h.prepare();
    h.edit().operation_id = uuid::Uuid::new_v4();
    assert!(matches!(h.prepare_at(1001), Err(BotLedgerError::Conflict)));
    assert_eq!(h.count("bot_decisions"), 1);
    assert_eq!(h.read(first.receipt.decision).unwrap(), first);
}
#[test]
fn decision_prepare_never_adopts_unleased_legacy_native_intent() {
    let mut h = Harness::new();
    let DecisionBindingV1::KoruEdit(v) = &h.binding else {
        unreachable!()
    };
    h.db.prepare_native_result(&PrepareNativeResult {
        key: NativeResultKey {
            run: v.run,
            attempt: v.origin_attempt,
            generation: v.origin_generation,
            turn: v.origin_turn,
            operation_id: v.operation_id,
        },
        kind: NativeResultOperation::EditPage,
        destination: v.workspace,
        target: Some(v.target),
        request_fingerprint: v.edit_fingerprint,
        expected_revision: Some(v.expected_revision.as_slice().to_vec()),
        config_revision: 1,
        policy_revision: 1,
        title: "Fixture".into(),
    })
    .unwrap();
    assert!(matches!(h.prepare_at(1000), Err(BotLedgerError::Conflict)));
    assert_eq!(h.count("bot_decisions"), 0);
    assert_eq!(h.count("native_result_intents"), 1);
}
#[test]
fn decision_prepare_refuses_foreign_profile_home_and_noncurrent_attempt() {
    for field in 0..3 {
        let mut h = Harness::new();
        match field {
            0 => h.edit().human_profile = ProfileId::from_uuid(fixtures::v4(240)),
            1 => h.edit().workspace = fixtures::workspace(240),
            _ => h.edit().origin_turn = AgentTurnId::mint(),
        };
        assert!(h.prepare_at(1000).is_err());
        assert_eq!(h.count("bot_decisions"), 0);
    }
}
#[test]
fn decision_prepare_requires_real_selected_write_denial() {
    let mut h = Harness::new();
    let f = h.origin();
    let mut admitted = h.db.bot_admission(f.run).unwrap().unwrap().submission;
    admitted.policy.access = BotAccess::Workspace;
    h.db.conn()
        .execute(
            "UPDATE bot_admissions SET payload_json=?2 WHERE run_id=?1",
            params![
                f.run.to_storage(),
                serde_json::to_string(&admitted).unwrap()
            ],
        )
        .unwrap();
    h.edit().captured_policy = admitted.policy.clone();
    h.proof.global.policy = admitted.policy;
    h.proof.global.key.request = BotRequestId::mint().into();
    h.proof.global.config_revision = 2;
    h.db.install_native_policy(&h.proof.global).unwrap();
    let global = DecisionGlobalPolicy {
        key: h.proof.global.key,
        config_revision: rev(2),
        policy_revision: rev(1),
        digest: DecisionDigest::from_bytes(
            crate::native_results::policy_digest(&h.proof.global).unwrap(),
        ),
    };
    h.edit().global_policy = global;
    assert!(matches!(h.prepare_at(1000), Err(BotLedgerError::Fenced)));
    assert_eq!(h.count("bot_decisions"), 0);
}
#[test]
fn decision_prepare_refuses_routine_edit_grant_bypass_but_keeps_native_subject() {
    let mut h = Harness::new();
    h.routine(true);
    assert!(matches!(h.prepare_at(1000), Err(BotLedgerError::Fenced)));
    assert_eq!(h.count("bot_decisions"), 0);
    h.native();
    let saved = h.prepare();
    assert_eq!(saved.receipt.state.kind, DecisionKind::NativeApproval);
    assert!(
        matches!(saved.receipt.binding,DecisionBindingV1::NativeApproval(ref v) if v.routine.is_some())
    );
}
#[test]
fn decision_prepare_routine_native_refuses_unlinked_or_downgraded_identity() {
    let mut h = Harness::new();
    h.native();
    h.routine(false);
    assert!(h.prepare_at(1000).is_err());
    h.db.conn()
        .execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    // Even if a damaged version is accepted by raw storage, the sole subject
    // decoder must not reinterpret the retained Routine as an ordinary Run.
    h.db.conn()
        .execute("UPDATE bot_admissions SET submission_version=1", [])
        .unwrap();
    assert!(h.prepare_at(1000).is_err());
}
#[test]
fn decision_prepare_native_past_expiry_and_clock_overflow_leave_no_rows() {
    let mut h = Harness::new();
    h.native();
    if let DecisionBindingV1::NativeApproval(v) = &mut h.binding {
        v.native_deadline_at_ms = Some(999);
    }
    assert!(h.prepare_at(1000).is_err());
    assert_eq!(h.count("bot_decisions"), 0);
    let mut h = Harness::new();
    assert!(h.prepare_at(i64::MAX).is_err());
    assert_eq!(h.count("bot_wait_runtime"), 0);
}
#[test]
fn decision_prepare_transaction_rollback_removes_both_owned_rows() {
    let h = Harness::new();
    let tx = h.db.conn().unchecked_transaction().unwrap();
    let record = prepare_on(&tx, &h.binding, &h.proof, h.boot, 1000).unwrap();
    assert_eq!(record.outcome, WriteOutcome::Applied);
    drop(tx);
    assert_eq!(h.count("bot_decisions"), 0);
    assert_eq!(h.count("bot_wait_runtime"), 0);
}

/// Scalar capacity fixtures intentionally have no decision bodies: this tests
/// the bounded admission projection, not full retained history qualification.
fn fill_waits(h: &Harness, count: usize, foreign_owner: bool) {
    let f = h.origin();
    let c = h.db.conn();
    c.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    for n in 0..count {
        let run = RunId::mint();
        let id = DecisionId::mint();
        c.execute("INSERT INTO bot_wait_runtime(wait_id,run_id,decision_id,user_scope,device_id,bot_id,attempt_id,generation,session_id,turn_id,reason,phase,revision,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8,?9,'decision','fenced',1,0,?10)",params![BotWaitId::mint().to_storage(),run.to_storage(),id.to_storage(),if foreign_owner{fixtures::v4(240).to_string()}else{f.owner.user_scope.to_storage()},f.owner.device.to_storage(),f.bot.to_storage(),RunAttemptId::mint().to_storage(),SessionId::mint().to_string(),AgentTurnId::mint().to_string(),n as i64]).unwrap();
    }
    c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
}
#[test]
fn decision_prepare_cap_is_device_wide_and_exact_replay_precedes_cap() {
    for foreign in [false, true] {
        let mut h = Harness::new();
        let first = h.prepare();
        fill_waits(&h, 49, foreign);
        assert_eq!(h.prepare_at(1001).unwrap().record, first);
        let tx = h.db.conn().unchecked_transaction().unwrap();
        assert!(matches!(
            write::cap(&tx, h.origin().owner),
            Err(BotLedgerError::Capacity)
        ));
    }
}
#[test]
fn decision_prepare_cap_probe_detects_overflow_without_loading_bodies() {
    let h = Harness::new();
    fill_waits(&h, 53, true);
    let tx = h.db.conn().unchecked_transaction().unwrap();
    assert!(matches!(
        write::cap(&tx, h.origin().owner),
        Err(BotLedgerError::Capacity)
    ));
}

#[test]
fn decision_prepare_cap_refuses_impossible_nondecision_cleanup_population() {
    let h = Harness::new();
    fill_waits(&h, 52, true);
    h.db.conn()
        .execute_batch("DROP TRIGGER bot_wait_runtime_immutable")
        .unwrap();
    h.db.conn().execute("UPDATE bot_wait_runtime SET decision_id=NULL,reason='capacity' WHERE wait_id IN (SELECT wait_id FROM bot_wait_runtime ORDER BY updated_at_ms DESC LIMIT 7)",[]).unwrap();
    let tx = h.db.conn().unchecked_transaction().unwrap();
    assert!(matches!(
        write::cap(&tx, h.origin().owner),
        Err(BotLedgerError::Capacity)
    ));
}
