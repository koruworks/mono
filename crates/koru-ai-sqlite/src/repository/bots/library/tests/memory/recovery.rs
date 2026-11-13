// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn memory_bounded_projection_refuses_oversize_and_wrong_types() {
    for (column, value) in [
        ("seed_title", rusqlite::types::Value::Text("é".repeat(129))),
        ("seed_title", rusqlite::types::Value::Blob(vec![1])),
        ("seed_hash", rusqlite::types::Value::Blob(vec![1; 33])),
        (
            "origin_edit_revision",
            rusqlite::types::Value::Blob(vec![1; 65537]),
        ),
        ("refusal", rusqlite::types::Value::Text("x".repeat(33))),
    ] {
        let (mut repo, bot, owner) = fixture();
        prepared(&mut repo, &bot, owner);
        repo.connection
            .execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        repo.connection
            .execute(
                &format!("UPDATE bot_memory_creation_intents SET {column}=?1 WHERE bot_id=?2"),
                params![value, bot.id.to_storage()],
            )
            .unwrap();
        assert_eq!(
            repo.bot_library(owner, bot.id),
            Err(BotError::Storage),
            "{column}"
        );
        assert_eq!(
            repo.bot_memory_recovery_page(owner, None)
                .unwrap()
                .entries
                .len(),
            1
        );
    }
}

#[test]
fn memory_receipt_projection_ignores_task_payload_but_preserves_corruption() {
    let (mut repo, bot, owner) = fixture();
    let creation = prepared(&mut repo, &bot, owner);
    repo.connection
        .execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    // The Memory decoder must never materialize this unrelated column.
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET submission_json=?1 WHERE request_id=?2",
            params![
                "x".repeat(1024 * 1024),
                creation.identity.creation_request.to_storage()
            ],
        )
        .unwrap();
    assert_eq!(
        repo.bot_library(owner, bot.id).unwrap().creation,
        Some(creation.clone())
    );
    for value in [
        rusqlite::types::Value::Text("x".repeat(BOT_LIBRARY_RECEIPT_LIMIT + 1)),
        rusqlite::types::Value::Blob(vec![1; 8]),
        rusqlite::types::Value::Null,
    ] {
        repo.connection
            .execute(
                "UPDATE bot_command_receipts SET outcome_json=?1 WHERE request_id=?2",
                params![value, creation.identity.creation_request.to_storage()],
            )
            .unwrap();
        assert_eq!(repo.bot_library(owner, bot.id), Err(BotError::Storage));
    }
}

fn another(repo: &mut Repository, owner: BotOwner) -> Bot {
    let mut bot = super::super::super::super::tests::fixture_bot();
    bot.user_scope = owner.user_scope;
    bot.owner_device = owner.device;
    repo.save_bot(&bot, ProfileId::new(), BotRequestId::mint(), HASH, None)
        .unwrap();
    prepared(repo, &bot, owner);
    bot
}

#[test]
fn memory_recovery_pages_advance_past_bad_rows_and_freeze_highwater() {
    let (mut repo, bot, owner) = fixture();
    prepared(&mut repo, &bot, owner);
    for _ in 0..17 {
        another(&mut repo, owner);
    }
    repo.connection
        .execute(
            "UPDATE bot_memory_creation_intents SET updated_at_ms='broken' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    let first = repo.bot_memory_recovery_page(owner, None).unwrap();
    assert_eq!(first.entries.len(), 16);
    assert!(matches!(first.entries[0], Err(BotError::Storage)));
    assert!(first.entries[1..].iter().all(Result::is_ok));
    let cursor = first.next.unwrap();
    let late = another(&mut repo, owner);
    let second = repo.bot_memory_recovery_page(owner, Some(cursor)).unwrap();
    assert_eq!(second.entries.len(), 2);
    assert!(second.next.is_none());
    assert!(
        second
            .entries
            .iter()
            .all(|entry| entry.as_ref().unwrap().bot != late.id)
    );
    let restart = repo.bot_memory_recovery_page(owner, None).unwrap();
    let tail = repo.bot_memory_recovery_page(owner, restart.next).unwrap();
    assert!(
        tail.entries
            .iter()
            .any(|entry| entry.as_ref().is_ok_and(|entry| entry.bot == late.id))
    );
    assert!(matches!(
        repo.bot_memory_recovery_page(
            BotOwner {
                device: DeviceId::new(),
                ..owner
            },
            Some(cursor)
        ),
        Err(BotError::Invalid)
    ));
}

#[test]
fn memory_recovery_inventory_contains_no_creation_bodies_and_seeks_rowid() {
    let (mut repo, bot, owner) = fixture();
    prepared(&mut repo, &bot, owner);
    repo.connection
        .execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    repo.connection
        .execute(
            "UPDATE bot_memory_creation_intents SET seed_title=?1",
            ["x".repeat(1024 * 1024)],
        )
        .unwrap();
    let page = repo.bot_memory_recovery_page(owner, None).unwrap();
    assert_eq!(page.entries[0].as_ref().unwrap().bot, bot.id);
    assert_eq!(repo.bot_library(owner, bot.id), Err(BotError::Storage));
    let plan: Vec<String> = repo
        .connection
        .prepare(
            "EXPLAIN QUERY PLAN SELECT rowid FROM bot_memory_creation_intents NOT INDEXED
         WHERE rowid>0 AND rowid<=100 AND user_scope_id='scope' AND owner_device_id='device'
         AND state IN ('prepared','unconfirmed') ORDER BY rowid LIMIT 17",
        )
        .unwrap()
        .query_map([], |row| row.get(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        plan.iter().any(|line| line.contains("INTEGER PRIMARY KEY")),
        "{plan:?}"
    );
    assert!(
        !plan.iter().any(|line| line.contains("TEMP B-TREE")),
        "{plan:?}"
    );
}

#[test]
fn memory_recovery_reports_invalid_state_without_hiding_later_work() {
    for state in [
        rusqlite::types::Value::Text("invalid".into()),
        rusqlite::types::Value::Text("x".repeat(1024 * 1024)),
        rusqlite::types::Value::Blob(vec![1; 32]),
    ] {
        let (mut repo, bot, owner) = fixture();
        prepared(&mut repo, &bot, owner);
        let later = another(&mut repo, owner);
        repo.connection
            .execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        repo.connection
            .execute(
                "UPDATE bot_memory_creation_intents SET state=?1 WHERE bot_id=?2",
                params![state, bot.id.to_storage()],
            )
            .unwrap();
        let page = repo.bot_memory_recovery_page(owner, None).unwrap();
        assert_eq!(page.entries.len(), 2);
        assert!(matches!(page.entries[0], Err(BotError::Storage)));
        assert_eq!(page.entries[1].as_ref().unwrap().bot, later.id);
    }
}
