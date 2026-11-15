// SPDX-License-Identifier: MPL-2.0
//! Actual migrator and linked SQLite; no worker, Store or decision API.
use super::library::{foreign_keys_held, snapshot};
use super::routines::{ai21, populated_v20, routine_rows};
use super::*;
use rusqlite::{params_from_iter, types::Value};
use std::collections::BTreeMap;

type Row = BTreeMap<&'static str, Value>;
fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}
fn text(s: impl Into<String>) -> Value {
    Value::Text(s.into())
}
fn integer(n: i64) -> Value {
    Value::Integer(n)
}
fn insert(c: &Connection, table: &str, row: &Row) -> rusqlite::Result<usize> {
    c.execute(
        &format!(
            "INSERT INTO {table}({}) VALUES({})",
            row.keys().copied().collect::<Vec<_>>().join(","),
            vec!["?"; row.len()].join(",")
        ),
        params_from_iter(row.values()),
    )
}
fn check(
    c: &Connection,
    label: &str,
    accepts: bool,
    action: impl FnOnce() -> rusqlite::Result<()>,
) {
    c.execute_batch("SAVEPOINT decision_case").unwrap();
    let result = action();
    c.execute_batch("ROLLBACK TO decision_case; RELEASE decision_case")
        .unwrap();
    assert_eq!(result.is_ok(), accepts, "{label}: {result:?}");
}
fn row_case(
    c: &Connection,
    table: &str,
    row: &Row,
    column: &'static str,
    value: Value,
    accepts: bool,
) {
    let mut row = row.clone();
    row.insert(column, value);
    check(c, &format!("{table}.{column}"), accepts, || {
        insert(c, table, &row).map(|_| ())
    });
}
fn objects(c: &Connection) -> BTreeMap<String, String> {
    c.prepare("SELECT name,coalesce(sql,'') FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
fn populated_ai21() -> Connection {
    let mut c = populated_v20();
    koru_sqlite::prepare_with_preflight(&ai21(), &mut c, refuse_superseded_18).unwrap();
    routine_rows(&c);
    c.execute_batch("INSERT INTO tool_invocations(id,attempt_id,chat_id,tool_name,read_only,status,approval,proposed_at_ms) VALUES('invocation','attempt','chat','fixture',0,'running','approved',-10);").unwrap();
    c
}
fn current() -> Connection {
    let mut c = populated_ai21();
    prepare(&mut c).unwrap();
    c
}
fn invocation() -> Row {
    Row::from([
        ("decision_id", text(id(20))),
        ("run_id", text(id(1))),
        ("ai_attempt_id", text("attempt")),
        ("chat_id", text("chat")),
        ("assistant_message_id", text("message")),
        ("invocation_id", text("invocation")),
        ("decision_revision", integer(1)),
        ("event_id", text(id(21))),
        ("created_at_ms", integer(-100)),
        ("updated_at_ms", integer(-90)),
    ])
}
fn enrollment() -> Row {
    Row::from([
        ("wait_id", text(id(40))),
        ("decision_id", text(id(20))),
        ("run_id", text(id(1))),
        ("ai_attempt_id", text("attempt")),
        ("decision_revision", integer(1)),
        ("wait_revision", integer(1)),
        ("origin_attempt_id", text(id(11))),
        ("origin_generation", integer(1)),
        ("origin_session_id", text(id(12))),
        ("origin_turn_id", text(id(13))),
        ("receipt_digest", Value::Blob(vec![0; 32])),
        ("receipt_json", text("{}")),
        ("created_at_ms", integer(-100)),
    ])
}
fn rollover() -> Row {
    Row::from([
        ("decision_id", text(id(20))),
        ("ai_attempt_id", text("attempt")),
        ("old_attempt_id", text(id(11))),
        ("old_generation", integer(1)),
        ("old_session_id", text(id(12))),
        ("old_turn_id", text(id(13))),
        ("old_last_sequence", integer(42)),
        ("consumer_attempt_id", text(id(22))),
        ("consumer_generation", integer(2)),
        ("consumer_session_id", text(id(23))),
        ("consumer_turn_id", text(id(24))),
        ("installed_after_sequence", integer(0)),
        ("resume_receipt_json", text("{}")),
        ("created_at_ms", integer(-100)),
    ])
}

#[test]
fn decision_migration_ai22_preserves_populated_ai21_receipts_children_raw_rows_objects_and_fks() {
    let mut c = populated_ai21();
    let before = snapshot(&c);
    let old_objects = objects(&c);
    prepare(&mut c).unwrap();
    println!("Rust-linked SQLite {}", rusqlite::version());
    assert_eq!(user_version(&c).unwrap(), 22);
    let after = snapshot(&c);
    for (name, value) in before {
        assert_eq!(after[&name], value, "old rows changed: {name}");
    }
    let now = objects(&c);
    for (name, sql) in old_objects {
        assert_eq!(now[&name], sql, "old SQL changed: {name}");
    }
    for table in [
        "bot_decision_invocations",
        "bot_decision_replay_rollovers",
        "bot_decision_wait_enrollments",
    ] {
        assert!(now.contains_key(table));
        c.prepare(&format!("DELETE FROM {table} WHERE 0")).unwrap();
        let nullable: u32 = c
            .query_row(
                "SELECT count(*) FROM pragma_table_info(?1) WHERE pk<>0 AND \"notnull\"=0",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(nullable, 0, "nullable PK: {table}");
    }
    foreign_keys_held(&c);
    prepare(&mut c).unwrap();
    assert_eq!(snapshot(&c), after);
}

#[test]
fn decision_migration_ai22_precommit_fk_failure_rolls_back_schema_rows_and_stamp() {
    let mut c = populated_ai21();
    let before = snapshot(&c);
    let old_objects = objects(&c);
    let sql = format!(
        "PRAGMA defer_foreign_keys=ON; INSERT INTO messages(id,chat_id,ordinal,role,state,created_at_ms) VALUES('bad','missing',0,'assistant','pending',0);\n{}",
        MIGRATIONS[21].sql
    );
    let steps = [Migration {
        to: 22,
        name: "022_fk_failure_fixture.sql",
        sql: Box::leak(sql.into_boxed_str()),
        columns: &[],
    }];
    let error = koru_sqlite::migrate(crate::DATABASE_FILE, 22, &steps, &mut c).unwrap_err();
    assert!(error.to_string().contains("violations=0"), "{error}");
    assert_eq!(user_version(&c).unwrap(), 21);
    assert_eq!(snapshot(&c), before);
    assert_eq!(objects(&c), old_objects);
    foreign_keys_held(&c);
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_temp_master WHERE name='p42_fk_guard'",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn decision_migration_ai21_refuses22_before_preflight_or_configuration() {
    let mut c = current();
    let before = snapshot(&c);
    c.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA synchronous=OFF")
        .unwrap();
    let called = std::cell::Cell::new(false);
    assert!(matches!(
        koru_sqlite::prepare_with_preflight(&ai21(), &mut c, |_| {
            called.set(true);
            Ok(())
        }),
        Err(OpenError::SchemaTooNew {
            discovered: 22,
            supported: 21,
            ..
        })
    ));
    assert!(!called.get());
    assert_eq!(user_version(&c).unwrap(), 22);
    assert_eq!(snapshot(&c), before);
    for pragma in ["PRAGMA foreign_keys", "PRAGMA synchronous"] {
        assert_eq!(c.query_row(pragma, [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
}

#[test]
fn decision_migration_ai22_enrollment_without_card_is_bounded_immutable_and_fk_retained() {
    let c = current();
    let row = enrollment();
    assert_eq!(
        c.query_row("SELECT count(*) FROM bot_decision_invocations", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
    assert_eq!(c.query_row("SELECT count(*) FROM pragma_table_info('bot_decision_invocations') WHERE name='cleanup_receipt_json'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    for column in row.keys() {
        row_case(
            &c,
            "bot_decision_wait_enrollments",
            &row,
            column,
            Value::Null,
            false,
        );
    }
    for column in ["decision_revision", "wait_revision", "origin_generation"] {
        for value in [integer(0), Value::Real(1.5)] {
            row_case(
                &c,
                "bot_decision_wait_enrollments",
                &row,
                column,
                value,
                false,
            );
        }
    }
    row_case(
        &c,
        "bot_decision_wait_enrollments",
        &row,
        "origin_generation",
        integer(4294967296),
        false,
    );
    row_case(
        &c,
        "bot_decision_wait_enrollments",
        &row,
        "origin_generation",
        integer(4294967295),
        true,
    );
    for column in [
        "wait_id",
        "decision_id",
        "run_id",
        "origin_attempt_id",
        "origin_session_id",
        "origin_turn_id",
    ] {
        for n in [35, 37] {
            row_case(
                &c,
                "bot_decision_wait_enrollments",
                &row,
                column,
                text("x".repeat(n)),
                false,
            );
        }
    }
    for value in [
        Value::Blob(vec![0; 31]),
        Value::Blob(vec![0; 33]),
        text("x".repeat(32)),
    ] {
        row_case(
            &c,
            "bot_decision_wait_enrollments",
            &row,
            "receipt_digest",
            value,
            false,
        );
    }
    for value in [
        text(""),
        text("not json"),
        Value::Blob(b"{}".to_vec()),
        text(format!("\"{}\"", "x".repeat(4095))),
    ] {
        row_case(
            &c,
            "bot_decision_wait_enrollments",
            &row,
            "receipt_json",
            value,
            false,
        );
    }
    row_case(
        &c,
        "bot_decision_wait_enrollments",
        &row,
        "receipt_json",
        text(format!("\"{}\"", "x".repeat(4094))),
        true,
    );
    row_case(
        &c,
        "bot_decision_wait_enrollments",
        &row,
        "created_at_ms",
        integer(i64::MIN),
        true,
    );
    row_case(
        &c,
        "bot_decision_wait_enrollments",
        &row,
        "ai_attempt_id",
        text("missing"),
        false,
    );
    insert(&c, "bot_decision_wait_enrollments", &row).unwrap();
    for sql in [
        "UPDATE bot_decision_wait_enrollments SET wait_revision=2",
        "DELETE FROM bot_decision_wait_enrollments",
        "DELETE FROM bot_chat_runs",
        "DELETE FROM inference_attempts",
        "DELETE FROM chats",
    ] {
        check(&c, sql, false, || c.execute_batch(sql));
    }
    foreign_keys_held(&c);
}

#[test]
fn decision_migration_ai22_invocation_and_rollover_null_full_key_bounds_and_retained_receipts() {
    let c = current();
    let row = invocation();
    for column in row.keys() {
        row_case(
            &c,
            "bot_decision_invocations",
            &row,
            column,
            Value::Null,
            false,
        );
    }
    for column in [
        "ai_attempt_id",
        "chat_id",
        "assistant_message_id",
        "invocation_id",
    ] {
        row_case(
            &c,
            "bot_decision_invocations",
            &row,
            column,
            text("missing"),
            false,
        );
    }
    for column in ["decision_id", "run_id", "event_id"] {
        row_case(
            &c,
            "bot_decision_invocations",
            &row,
            column,
            text("x".repeat(35)),
            false,
        );
    }
    row_case(
        &c,
        "bot_decision_invocations",
        &row,
        "decision_revision",
        integer(0),
        false,
    );
    insert(&c, "bot_decision_invocations", &row).unwrap();
    let row = rollover();
    for column in row
        .keys()
        .filter(|c| !matches!(**c, "old_last_sequence" | "installed_after_sequence"))
    {
        row_case(
            &c,
            "bot_decision_replay_rollovers",
            &row,
            column,
            Value::Null,
            false,
        );
    }
    for column in ["old_last_sequence", "installed_after_sequence"] {
        row_case(
            &c,
            "bot_decision_replay_rollovers",
            &row,
            column,
            Value::Null,
            true,
        );
        row_case(
            &c,
            "bot_decision_replay_rollovers",
            &row,
            column,
            integer(-1),
            false,
        );
        row_case(
            &c,
            "bot_decision_replay_rollovers",
            &row,
            column,
            Value::Real(1.5),
            false,
        );
    }
    for column in ["old_generation", "consumer_generation"] {
        row_case(
            &c,
            "bot_decision_replay_rollovers",
            &row,
            column,
            integer(0),
            false,
        );
        row_case(
            &c,
            "bot_decision_replay_rollovers",
            &row,
            column,
            integer(4294967296),
            false,
        );
    }
    for value in [
        text(""),
        text("bad"),
        Value::Blob(b"{}".to_vec()),
        text(format!("\"{}\"", "x".repeat(65535))),
    ] {
        row_case(
            &c,
            "bot_decision_replay_rollovers",
            &row,
            "resume_receipt_json",
            value,
            false,
        );
    }
    row_case(
        &c,
        "bot_decision_replay_rollovers",
        &row,
        "resume_receipt_json",
        text(format!("\"{}\"", "x".repeat(65534))),
        true,
    );
    insert(&c, "bot_decision_replay_rollovers", &row).unwrap();
    for sql in [
        "UPDATE bot_decision_replay_rollovers SET consumer_turn_id='changed'",
        "DELETE FROM bot_decision_replay_rollovers",
        "UPDATE bot_decision_invocations SET invocation_id='changed'",
        "DELETE FROM bot_decision_invocations",
        "DELETE FROM tool_invocations",
        "DELETE FROM messages",
        "DELETE FROM bot_chat_runs",
    ] {
        check(&c, sql, false, || c.execute_batch(sql));
    }
    c.execute_batch("UPDATE bot_decision_invocations SET decision_revision=2")
        .unwrap();
    foreign_keys_held(&c);
}
