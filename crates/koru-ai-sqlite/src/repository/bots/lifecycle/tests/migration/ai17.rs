// SPDX-License-Identifier: MPL-2.0
//! Frozen fixture write shapes from ai17. Current admission requires ai19
//! Library tables and must never gain an old-schema production bypass.
use super::*;
use koru_ai_core::{PendingTurn, bots::BotReceiptOutcome};

fn insert_submission(conn: &Connection, intent: &BotSubmissionIntent, now: i64) {
    let s = &intent.submission;
    let p = &s.provenance;
    assert!(p.continued_from.is_none());
    assert!(s.request.context.is_empty());
    conn.execute(
        "UPDATE bots SET folder_lock_request_id=?2 WHERE bot_id=?1",
        params![p.bot.to_storage(), p.submission.to_storage()],
    )
    .unwrap();
    conn.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,planned_run_id,captured_config_revision,captured_policy_revision,submission_version,submission_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,'start','prepared',?5,?6,?7,1,?8,?9,?9)",
        params![p.owner.user_scope.to_storage(),p.submission.to_storage(),p.bot.to_storage(),HASH,s.request.run.to_storage(),p.config_revision,p.policy_revision,serde_json::to_string(intent).unwrap(),now]).unwrap();
}

pub(super) fn prepare_submission(repo: &mut Repository, intent: &BotSubmissionIntent, now: i64) {
    let tx = repo.connection.transaction().unwrap();
    insert_submission(&tx, intent, now);
    tx.commit().unwrap();
}

pub(super) fn prepare_chat(
    repo: &mut Repository,
    turn: &PendingTurn,
    intent: &BotSubmissionIntent,
    now: i64,
) {
    let tx = repo.connection.transaction().unwrap();
    insert_submission(&tx, intent, now);
    crate::repository::mutations::begin_turn_on(&tx, turn).unwrap();
    let s = &intent.submission;
    let p = &s.provenance;
    assert_eq!(p.ai_attempt_id, Some(turn.attempt.id.as_uuid()));
    assert_eq!(
        p.assistant_message_id,
        Some(turn.assistant_message.id.as_uuid())
    );
    tx.execute("INSERT INTO bot_chat_runs(ai_attempt_id,user_scope_id,owner_device_id,submission_id,bot_id,chat_id,assistant_message_id,run_id,record_workspace_id,continued_from_run_id,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,NULL,?10)",
        params![turn.attempt.id.to_storage(),p.owner.user_scope.to_storage(),p.owner.device.to_storage(),p.submission.to_storage(),p.bot.to_storage(),p.chat_id.to_string(),turn.assistant_message.id.to_storage(),s.request.run.to_storage(),s.workspace.to_storage(),now]).unwrap();
    tx.execute(
        "INSERT INTO bot_run_replay(ai_attempt_id,updated_at_ms) VALUES(?1,?2)",
        params![turn.attempt.id.to_storage(), now],
    )
    .unwrap();
    tx.commit().unwrap();
}

pub(super) fn prepare_access(repo: &mut Repository, bot: &Bot, barrier: &d::BotPolicyBarrier) {
    assert!(bot.selected_pages.is_empty());
    let outcome = BotReceiptOutcome::AccessPolicy {
        version: 1,
        bot: bot.clone(),
        barrier: barrier.clone(),
    };
    let tx = repo.connection.transaction().unwrap();
    tx.execute("UPDATE bots SET revision=?2,policy_revision=?3,access='workspace',updated_at_ms=?4 WHERE bot_id=?1",
        params![bot.id.to_storage(),bot.revision,bot.policy_revision,bot.updated_at_ms]).unwrap();
    tx.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,outcome_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,'update_access','prepared',?5,?6,?6)",
        params![bot.user_scope.to_storage(),BotRequestId::mint().to_storage(),bot.id.to_storage(),HASH,serde_json::to_string(&outcome).unwrap(),bot.updated_at_ms]).unwrap();
    tx.commit().unwrap();
}
