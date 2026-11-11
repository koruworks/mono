// SPDX-License-Identifier: MPL-2.0
//! Query plans over populated history, on the bundled SQLite: each recovery,
//! barrier and tombstone lookup reads its partial index, never settled rows.
use super::*;

/// Thousands of settled receipts and lifecycle operations, a few open ones.
fn populated() -> (Repository, Bot, BotOwner) {
    let (mut repo, bot, owner) = setup();
    let scope = owner.user_scope.to_storage();
    let tx = repo.connection.transaction().unwrap();
    for n in 0..3000 {
        let kind = ["update", "create_chat", "update_access", "create"][n % 4];
        tx.execute(
            "INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,'completed',?6,?6)",
            params![scope, BotRequestId::mint().to_storage(), BotId::mint().to_storage(), HASH, kind, n as i64],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO bot_lifecycle_intents(user_scope_id,operation_id,bot_id,owner_device_id,kind,fingerprint,expected_revision,prior_lifecycle,stage,requested_at_ms,updated_at_ms,completed_at_ms) VALUES(?1,?2,?3,?4,'trash',?5,1,'archived','completed',?6,?6,?6)",
            params![scope, BotRequestId::mint().to_storage(), BotId::mint().to_storage(), owner.device.to_storage(), HASH, n as i64],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    // Open work through the real paths.
    repo.capture_bot_lifecycle(&capture(&bot, owner, BotLifecycleKind::TrashChat))
        .unwrap();
    (repo, bot, owner)
}

fn plan(conn: &Connection, sql: &str, args: &[&dyn rusqlite::ToSql]) -> String {
    let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    statement
        .query_map(args, |r| r.get::<_, String>(3))
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>()
        .join(" | ")
}

#[test]
fn every_lifecycle_and_recovery_lookup_reads_its_index() {
    let (repo, bot, owner) = populated();
    let c = &repo.connection;
    let scope = owner.user_scope.to_storage();
    let id = bot.id.to_storage();
    let chat = bot.main_chat.to_storage();
    // The Bot read's open operation (bot_open_lifecycle), with the reversal rule.
    let open_read = format!(
        "SELECT user_scope_id,operation_id FROM bot_lifecycle_intents i INDEXED BY bot_lifecycle_open WHERE bot_id=?1 AND {OPEN} ORDER BY requested_at_ms DESC,operation_id DESC LIMIT 1"
    );
    let cases: Vec<(&str, &str, Vec<&dyn rusqlite::ToSql>)> = vec![
        ("bot_lifecycle_open", &open_read, vec![&id]),
        // admission.rs pending_bot_operations
        (
            "bot_unresolved_coordination",
            "SELECT rowid,bot_id,user_scope_id,request_id FROM bot_command_receipts INDEXED BY bot_unresolved_coordination WHERE rowid>?1 AND stage IN ('prepared','accepted') AND command_kind IN ('start','continue','pause','resume') ORDER BY rowid LIMIT ?2",
            vec![&0i64, &64i64],
        ),
        // policy.rs pending_bot_access
        (
            "bot_pending_access",
            "SELECT rowid,bot_id,user_scope_id,request_id FROM bot_command_receipts INDEXED BY bot_pending_access WHERE rowid>?1 AND command_kind='update_access' AND stage='prepared' ORDER BY rowid LIMIT ?2",
            vec![&0i64, &64i64],
        ),
        // admission.rs: the 50-intent bound keeps using ai17's guard index.
        (
            "bot_command_guard",
            "SELECT COUNT(*) FROM bot_command_receipts WHERE user_scope_id=?1 AND bot_id=?2 AND command_kind IN ('start','continue') AND stage='prepared'",
            vec![&scope, &id],
        ),
        (
            "bot_command_guard",
            "SELECT EXISTS(SELECT 1 FROM bot_command_receipts WHERE user_scope_id=?1 AND bot_id=?2 AND command_kind IN ('pause','resume') AND stage='prepared')",
            vec![&scope, &id],
        ),
        // lifecycle.rs
        (
            "bot_lifecycle_resume",
            "SELECT updated_at_ms,user_scope_id,operation_id FROM bot_lifecycle_intents WHERE stage NOT IN ('completed','incomplete') AND (updated_at_ms>?1 OR (updated_at_ms=?1 AND operation_id>?2)) ORDER BY updated_at_ms,operation_id LIMIT ?3",
            vec![&0i64, &"", &16i64],
        ),
        (
            "bot_lifecycle_chat_open",
            "SELECT user_scope_id,operation_id FROM bot_lifecycle_intents WHERE chat_id=?1 AND stage<>'completed'",
            vec![&chat],
        ),
        (
            "bot_lifecycle_open",
            "SELECT user_scope_id,operation_id,stage FROM bot_lifecycle_intents WHERE bot_id=?1 AND stage<>'completed'",
            vec![&id],
        ),
        (
            "bot_lifecycle_source",
            "SELECT EXISTS(SELECT 1 FROM bot_lifecycle_intents WHERE user_scope_id=?1 AND source_operation_id=?2)",
            vec![&scope, &id],
        ),
        // The shapes S3/S8 will read: designated main after purge, ID reuse, retained history.
        (
            "bot_tombstoned_main",
            "SELECT subject_id FROM bot_purge_tombstones WHERE bot_id=?1 AND subject_kind='chat' AND chat_purpose='main'",
            vec![&id],
        ),
        (
            "bot_tombstoned_chat",
            "SELECT EXISTS(SELECT 1 FROM bot_purge_tombstones WHERE subject_id=?1 AND subject_kind='chat')",
            vec![&chat],
        ),
        (
            "bot_historical_run_message",
            "SELECT run_id,record_workspace_id FROM bot_historical_run_refs WHERE message_id=?1",
            vec![&chat],
        ),
    ];
    for (index, sql, args) in cases {
        let found = plan(c, sql, &args);
        assert!(found.contains(index), "{index} not used by {sql}: {found}");
    }
    let open = repo.bot_open_lifecycle(bot.id).unwrap().unwrap();
    assert_eq!(open.kind, BotLifecycleKind::TrashChat);
    assert_eq!(open.chat_id, Some(bot.main_chat));
    assert_eq!(open.stage, BotLifecycleStage::Captured);
}

/// The Bot read's `pending_change` seeks the Bot's own pause, resume and
/// access receipts through ai17's per-Bot guard index, never the 3,000
/// settled receipts of other Bots.
#[test]
fn the_bot_reads_pending_change_seeks_the_bots_guard_index() {
    let (repo, bot, _) = populated();
    let found = plan(
        &repo.connection,
        super::super::super::admission::BOT_PENDING_CHANGE_SQL,
        &[&bot.id.to_storage()],
    );
    assert!(found.contains("bot_command_guard"), "{found}");
    assert!(!found.contains("SCAN bot_command_receipts"), "{found}");
    assert_eq!(repo.bot_pending_change(bot.id).unwrap(), None);
}

/// The recovery pass's page of operations parked for an owner that did not
/// answer walks the unfinished operations' partial index, never the 3,000
/// completed ones, and finds exactly those parked for that reason.
#[test]
fn the_page_of_operations_parked_for_their_owner_reads_only_unfinished_ones() {
    let (mut repo, bot, owner) = populated();
    let found = plan(
        &repo.connection,
        &super::super::parked_for_owner_sql(),
        &[&16i64],
    );
    assert!(found.contains("bot_lifecycle_open"), "{found}");
    assert!(!found.contains("TEMP B-TREE"), "{found}");
    assert!(repo.parked_for_owner_bot_lifecycle(16).unwrap().is_empty());
    let open = repo.bot_open_lifecycle(bot.id).unwrap().unwrap();
    assert!(
        repo.park_bot_lifecycle(
            owner.user_scope,
            open.request_id,
            BotLifecycleRefusal::OwnerUnavailable,
            9
        )
        .unwrap()
    );
    assert_eq!(
        repo.parked_for_owner_bot_lifecycle(16).unwrap(),
        vec![(owner.user_scope, open.request_id, bot.id)]
    );
}

#[test]
fn recovery_pages_reach_open_rows_past_settled_history() {
    let (repo, _, owner) = populated();
    // One page holds the single open lifecycle operation despite 3000 settled ones.
    let page = repo.resumable_bot_lifecycle(None, 1).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].1, owner.user_scope);
    assert!(
        repo.resumable_bot_lifecycle(Some((page[0].0, page[0].2)), 16)
            .unwrap()
            .is_empty()
    );
    assert!(repo.pending_bot_operations(0, 64).unwrap().is_empty());
    assert!(repo.pending_bot_access(0, 64).unwrap().pending.is_empty());
}
