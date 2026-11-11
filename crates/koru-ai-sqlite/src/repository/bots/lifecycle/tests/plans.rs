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
    // Every read of unfinished operations, exactly as the repository runs it.
    // A reversed operation leaves these indexes by their own predicate, so
    // none asks `bot_lifecycle_source` whether a Restore names a row.
    use super::super::{
        RESUMABLE_SQL, bot_hold_sql, bot_open_sql, chat_hold_sql, open_bot_barrier_sql,
        open_chat_sql, open_operations_sql, whole_bot_open_sql,
    };
    let (bot_open, whole_open, barrier, bot_hold, operations) = (
        bot_open_sql(),
        whole_bot_open_sql(),
        open_bot_barrier_sql(),
        bot_hold_sql(),
        open_operations_sql(),
    );
    let (chat_open, chat_hold) = (open_chat_sql(), chat_hold_sql());
    let open_reads: Vec<(&str, &str, Vec<&dyn rusqlite::ToSql>)> = vec![
        ("bot_lifecycle_open", &bot_open, vec![&id]),
        ("bot_lifecycle_open", &whole_open, vec![&id]),
        ("bot_lifecycle_open", &barrier, vec![&id]),
        ("bot_lifecycle_open", &bot_hold, vec![&id]),
        ("bot_lifecycle_open", &operations, vec![&id]),
        ("bot_lifecycle_chat_open", &chat_open, vec![&chat]),
        ("bot_lifecycle_chat_open", &chat_hold, vec![&chat]),
        (
            "bot_lifecycle_resume",
            RESUMABLE_SQL,
            vec![&0i64, &"", &16i64],
        ),
    ];
    for (index, sql, args) in open_reads {
        let found = plan(c, sql, &args);
        assert!(found.contains(index), "{index} not used by {sql}: {found}");
        for walk in [
            "bot_lifecycle_source",
            "SCAN i",
            "SCAN bot_lifecycle_intents",
        ] {
            assert!(!found.contains(walk), "{walk} in {sql}: {found}");
        }
    }
    let cases: Vec<(&str, &str, Vec<&dyn rusqlite::ToSql>)> = vec![
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
        // lifecycle.rs: the reverse-once check at capture.
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
    // From the start and from a cursor: a range of the partial index in its
    // own order, never a sort.
    for (bot_after, row_after) in [("", i64::MIN), ("ffffffff", 7)] {
        let found = plan(
            &repo.connection,
            &super::super::parked_for_owner_sql(),
            &[&bot_after, &row_after, &16i64],
        );
        assert!(found.contains("bot_lifecycle_open"), "{found}");
        assert!(!found.contains("TEMP B-TREE"), "{found}");
    }
    assert!(
        repo.parked_for_owner_bot_lifecycle(None, 16)
            .unwrap()
            .is_empty()
    );
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
    let page = repo.parked_for_owner_bot_lifecycle(None, 16).unwrap();
    assert_eq!(
        page.iter()
            .map(|p| (p.scope, p.operation, p.bot))
            .collect::<Vec<_>>(),
        vec![(owner.user_scope, open.request_id, bot.id)]
    );
    // Past the last one, nothing.
    assert!(
        repo.parked_for_owner_bot_lifecycle(Some(&page[0].cursor), 16)
            .unwrap()
            .is_empty()
    );
}

/// More operations parked for their owner than one page holds: walked page
/// after page by cursor, each is reached exactly once, in a stable order,
/// and a walk that starts again reads the same first page.
#[test]
fn the_operations_parked_for_their_owner_are_walked_in_turn_by_cursor() {
    let (repo, _, owner) = populated();
    let scope = owner.user_scope.to_storage();
    let mut parked = Vec::new();
    for n in 0..20 {
        let operation = BotRequestId::mint();
        repo.connection
            .execute(
                "INSERT INTO bot_lifecycle_intents(user_scope_id,operation_id,bot_id,owner_device_id,kind,fingerprint,expected_revision,prior_lifecycle,stage,refusal,requested_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,'trash',?5,1,'archived','incomplete','owner_unavailable',?6,?6)",
                params![scope, operation.to_storage(), BotId::mint().to_storage(), owner.device.to_storage(), HASH, n as i64],
            )
            .unwrap();
        parked.push(operation);
    }
    let first = repo.parked_for_owner_bot_lifecycle(None, 16).unwrap();
    assert_eq!(first.len(), 16);
    let second = repo
        .parked_for_owner_bot_lifecycle(Some(&first[15].cursor), 16)
        .unwrap();
    assert_eq!(second.len(), 4);
    assert!(
        repo.parked_for_owner_bot_lifecycle(Some(&second[3].cursor), 16)
            .unwrap()
            .is_empty()
    );
    let mut reached: Vec<_> = first.iter().chain(&second).map(|p| p.operation).collect();
    let in_order: Vec<_> = first
        .iter()
        .chain(&second)
        .map(|p| p.bot.to_storage())
        .collect();
    assert!(in_order.windows(2).all(|w| w[0] < w[1]), "{in_order:?}");
    reached.sort_by_key(|id| id.to_storage());
    parked.sort_by_key(|id| id.to_storage());
    assert_eq!(reached, parked);
    assert_eq!(
        repo.parked_for_owner_bot_lifecycle(None, 16).unwrap(),
        first
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
