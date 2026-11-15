// SPDX-License-Identifier: MPL-2.0

//! The bounded open decision-wait enumeration behind the runtime's per-turn
//! resume driver. Identity rows only; no evidence body is read.
use super::*;

fn insert_wait(h: &Harness, run: RunId, reason: &str, phase: &str, device: &str) {
    let f = h.origin();
    let decision = (reason == "decision").then(|| DecisionId::mint().to_storage());
    let settled = (phase == "settled").then_some(5i64);
    h.db.conn().execute("INSERT INTO bot_wait_runtime(wait_id,run_id,decision_id,user_scope,device_id,bot_id,attempt_id,generation,session_id,turn_id,reason,phase,revision,created_at_ms,updated_at_ms,settled_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8,?9,?10,?11,1,0,0,?12)",
        params![BotWaitId::mint().to_storage(),run.to_storage(),decision,f.owner.user_scope.to_storage(),device,f.bot.to_storage(),RunAttemptId::mint().to_storage(),SessionId::mint().to_string(),AgentTurnId::mint().to_string(),reason,phase,settled]).unwrap();
}

fn runs(count: usize) -> Vec<RunId> {
    let mut runs: Vec<RunId> = (0..count).map(|_| RunId::mint()).collect();
    runs.sort();
    runs
}

#[test]
fn decision_resume_runs_pages_in_run_order_and_excludes_settled_and_nondecision_waits() {
    let h = Harness::new();
    let f = h.origin();
    let device = f.owner.device.to_storage();
    h.db.conn()
        .execute_batch("PRAGMA foreign_keys=OFF")
        .unwrap();
    let open = runs(18);
    for run in &open {
        insert_wait(&h, *run, "decision", "fenced", &device);
    }
    let others = runs(2);
    insert_wait(&h, others[0], "decision", "settled", &device);
    insert_wait(&h, others[1], "capacity", "fenced", &device);
    h.db.conn().execute_batch("PRAGMA foreign_keys=ON").unwrap();
    // A page is bounded and ordered; owner comes from the row.
    let first = BotLedger::decision_resume_runs(&h.db, None, DECISION_RESUME_RUN_PAGE).unwrap();
    assert_eq!(first.len(), DECISION_RESUME_RUN_PAGE);
    assert!(first.iter().all(|(owner, _)| *owner == f.owner));
    assert!(first.windows(2).all(|pair| pair[0].1 < pair[1].1));
    // Continuing after the last returned Run reaches the rest; nothing else.
    let rest =
        BotLedger::decision_resume_runs(&h.db, first.last().map(|(_, run)| *run), 16).unwrap();
    let mut seen: Vec<RunId> = first.iter().chain(&rest).map(|(_, run)| *run).collect();
    seen.sort();
    assert_eq!(seen, open);
    assert!(!seen.contains(&others[0]) && !seen.contains(&others[1]));
    // Out-of-range limits are refused rather than scanned.
    assert_eq!(
        BotLedger::decision_resume_runs(&h.db, None, 17).unwrap_err(),
        BotLedgerError::Invalid
    );
    assert_eq!(
        BotLedger::decision_resume_runs(&h.db, None, 0).unwrap_err(),
        BotLedgerError::Invalid
    );
}

#[test]
fn decision_resume_runs_skips_a_row_with_a_malformed_owner() {
    let h = Harness::new();
    let f = h.origin();
    let good = runs(2);
    h.db.conn()
        .execute_batch("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON")
        .unwrap();
    insert_wait(&h, good[0], "decision", "fenced", "not-a-device");
    insert_wait(
        &h,
        good[1],
        "decision",
        "fenced",
        &f.owner.device.to_storage(),
    );
    h.db.conn()
        .execute_batch("PRAGMA ignore_check_constraints=OFF; PRAGMA foreign_keys=ON")
        .unwrap();
    assert_eq!(
        BotLedger::decision_resume_runs(&h.db, None, 16).unwrap(),
        vec![(f.owner, good[1])]
    );
}
