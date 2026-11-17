// SPDX-License-Identifier: MPL-2.0
//! ai25 → ai26 (one Bot asking another, M6) on the actual migrator and linked
//! SQLite. In-memory, plus one temporary file for the real open path. No
//! worker, Store, delegate or provider, and never a real database.
//!
//! Cohort `m6_ai26_migration`, and the table-level half of
//! `m6_child_chat_rows` (the repository half is beside the repository).
use super::connections::{ai25, connection_row, populated_ai24};
use super::library::{foreign_keys_held, snapshot};
use super::routine_controls::foreign_keys;
use super::*;

/// The whole of what ai26 adds to the stored definition of `bots`, with the
/// whitespace `snapshot` removes.
const ADDED_COLUMN: &str = ",may_askINTEGERNOTNULLDEFAULT1CHECK(may_askIN(0,1))";

/// Every object ai26 creates, by type.
const ADDED: &[(&str, &str)] = &[
    ("table", "bot_child_chats"),
    ("index", "bot_child_chats_parent"),
    ("table", "bot_child_wait_enrollments"),
    ("index", "bot_child_wait_enrollments_run"),
    ("trigger", "bot_child_wait_enrollments_immutable"),
    ("trigger", "bot_child_wait_enrollments_retain"),
    ("table", "bot_child_replay_rollovers"),
    ("trigger", "bot_child_replay_rollovers_immutable"),
    ("trigger", "bot_child_replay_rollovers_retain"),
];

const NEW_TABLES: [&str; 3] = [
    "bot_child_chats",
    "bot_child_wait_enrollments",
    "bot_child_replay_rollovers",
];

/// The populated fixture every earlier step is tested with, migrated to ai25
/// and no further, with rows in the ai25 tables and a second Bot, so `bots`
/// holds more than one row and the M5 tables are not empty.
fn populated_ai25() -> Connection {
    let mut c = populated_ai24();
    koru_sqlite::prepare_with_preflight(&ai25(), &mut c, refuse_superseded_18).unwrap();
    assert_eq!(user_version(&c).unwrap(), 25);
    c.execute_batch(
        "INSERT INTO chats(id,title,current_provider,current_model,created_at_ms,updated_at_ms) VALUES('chat-two','second','agent-codex','default',3,4);
         INSERT INTO bots(bot_id,user_scope_id,home_workspace_id,owner_device_id,name,role_workspace_id,role_document_id,working_folder,provider,model,reasoning,revision,policy_revision,access,paused,created_at_ms,updated_at_ms,avatar)
           VALUES('bot-two','scope','home','device','second','home','role-two','folder-two','agent-codex','default','auto',3,2,'workspace',1,3,4,NULL);
         INSERT INTO bot_chats(chat_id,bot_id,purpose,created_at_ms) VALUES('chat-two','bot-two','main',3);",
    )
    .unwrap();
    c.execute_batch(&connection_row("c1", "connected", "NULL", "NULL"))
        .unwrap();
    c.execute(
        "INSERT INTO bot_connection_grants(bot_id,connection_id,operation,granted_at_ms) VALUES('bot-two',?1,'read_item',5)",
        [pad("c1")],
    )
    .unwrap();
    c
}

fn columns(c: &Connection, table: &str) -> Vec<String> {
    c.prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
        .unwrap()
        .query_map([table], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// A row as `snapshot` prints it, without the column ai26 appended.
fn without_may_ask(row: &str) -> String {
    format!(
        "{}]",
        row.strip_suffix(", Integer(1)]")
            .unwrap_or_else(|| panic!("no may_ask=1 at the end of {row}"))
    )
}

fn refused(c: &Connection, sql: &str, why: &str) {
    let error = c.execute_batch(sql).expect_err(&format!("accepted: {sql}"));
    assert!(error.to_string().contains(why), "{why}: {error}\n{sql}");
}

fn pad(id: &str) -> String {
    format!("{id:0>36}")
}

#[test]
fn m6_ai26_migration_keeps_every_populated_ai25_row_and_gives_every_bot_may_ask() {
    let mut c = populated_ai25();
    println!("Rust-linked SQLite {}", rusqlite::version());
    let before = snapshot(&c);
    let keys_before = foreign_keys(&c);
    assert!(
        before.values().filter(|(_, rows)| !rows.is_empty()).count() > 10,
        "the fixture is not populated"
    );
    let bots_before = before["bots"].1.len() as i64;
    assert!(bots_before >= 2, "the fixture holds several Bots");
    assert!(!before["connections"].1.is_empty());
    assert!(!before["bot_connection_grants"].1.is_empty());

    prepare(&mut c).unwrap();
    assert_eq!(SCHEMA_VERSION, 26);
    assert_eq!(user_version(&c).unwrap(), 26);
    let after = snapshot(&c);

    for (name, (sql, rows)) in &before {
        let (new_sql, new_rows) = &after[name];
        if name == "bots" {
            // The definition is the old one plus the one column, and every
            // old row is there value for value with the default, on.
            assert_eq!(new_sql.replacen(ADDED_COLUMN, "", 1), *sql);
            assert_eq!(new_sql.matches(ADDED_COLUMN).count(), 1, "{new_sql}");
            assert_eq!(
                new_rows
                    .iter()
                    .map(|row| without_may_ask(row))
                    .collect::<Vec<_>>(),
                *rows
            );
            continue;
        }
        // No other table was rebuilt, rewritten or re-encoded.
        assert_eq!(sql, new_sql, "definition changed: {name}");
        assert_eq!(rows, new_rows, "raw rows changed: {name}");
    }
    let (bots, asking): (i64, i64) = c
        .query_row("SELECT count(*),sum(may_ask) FROM bots", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(
        (bots, asking),
        (bots_before, bots_before),
        "every Bot has may_ask = 1"
    );

    // What is new is exactly the listed objects, and the new tables are empty.
    let added: std::collections::BTreeSet<&str> = after
        .keys()
        .filter(|name| !before.contains_key(*name))
        .map(String::as_str)
        .collect();
    assert_eq!(added, ADDED.iter().map(|(_, name)| *name).collect());
    for (kind, name) in ADDED {
        let found: String = c
            .query_row(
                "SELECT type FROM sqlite_master WHERE name=?1",
                [name],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, *kind, "{name}");
    }
    for table in NEW_TABLES {
        assert!(after[table].1.is_empty(), "{table}");
    }
    // No existing foreign key moved, none is violated, and no temporary
    // object is left behind.
    let keys_after = foreign_keys(&c);
    for (table, keys) in &keys_before {
        assert_eq!(&keys_after[table], keys, "{table}");
    }
    foreign_keys_held(&c);
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_temp_master WHERE name LIKE 'bot_ai26%'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );

    // Reopened, it is already current and nothing runs again.
    prepare(&mut c).unwrap();
    assert_eq!(snapshot(&c), after);
}

#[test]
fn m6_ai26_migration_empty_and_fresh_databases_reach_26_with_no_content_column() {
    // An empty ai25 database, as a build that never made a Bot left it.
    let mut empty = koru_sqlite::open_in_memory(&ai25()).unwrap();
    assert_eq!(user_version(&empty).unwrap(), 25);
    prepare(&mut empty).unwrap();
    assert_eq!(user_version(&empty).unwrap(), 26);
    // A fresh database.
    let fresh = open_in_memory().unwrap();
    assert_eq!(MIGRATIONS.len(), 26);
    assert_eq!(MIGRATIONS[25].to, 26);
    assert_eq!(MIGRATIONS[25].name, "026_ai_bot_children.sql");
    assert_eq!(user_version(&fresh).unwrap(), 26);
    assert_eq!(snapshot(&empty), snapshot(&fresh));
    for c in [&empty, &fresh] {
        assert_eq!(columns(c, "bots").last().unwrap(), "may_ask");
        assert_eq!(
            columns(c, "bot_child_chats"),
            [
                "user_scope_id",
                "child_request_id",
                "parent_bot_id",
                "parent_chat_id",
                "parent_run_id",
                "parent_assistant_message_id",
                "ask_key",
                "ordinal",
                "ask_fingerprint",
                "wait_id",
                "chat_id",
                "created_at_ms",
                "refusal",
                "refusal_ordinal"
            ]
        );
        assert_eq!(
            columns(c, "bot_child_wait_enrollments"),
            [
                "wait_id",
                "run_id",
                "ai_attempt_id",
                "wait_revision",
                "origin_attempt_id",
                "origin_generation",
                "origin_session_id",
                "origin_turn_id",
                "receipt_digest",
                "receipt_json",
                "created_at_ms"
            ]
        );
        assert_eq!(
            columns(c, "bot_child_replay_rollovers"),
            [
                "wait_id",
                "ai_attempt_id",
                "old_attempt_id",
                "old_generation",
                "old_session_id",
                "old_turn_id",
                "old_last_sequence",
                "consumer_attempt_id",
                "consumer_generation",
                "consumer_session_id",
                "consumer_turn_id",
                "installed_after_sequence",
                "resume_receipt_json",
                "created_at_ms"
            ]
        );
        // No column can hold a task, a title, an answer or a credential.
        for table in NEW_TABLES {
            for column in columns(c, table) {
                for word in [
                    "task",
                    "title",
                    "text",
                    "content",
                    "objective",
                    "token",
                    "secret",
                    "name",
                ] {
                    assert!(!column.contains(word), "{table}.{column}");
                }
            }
        }
        for (pragma, expected) in [
            ("PRAGMA foreign_keys", 1),
            ("SELECT count(*) FROM pragma_foreign_key_check", 0),
        ] {
            assert_eq!(
                c.query_row(pragma, [], |r| r.get::<_, i64>(0)).unwrap(),
                expected,
                "{pragma}"
            );
        }
    }
}

#[test]
fn m6_ai26_migration_failure_at_any_point_leaves_25_and_every_row() {
    let original = MIGRATIONS[25].sql;
    let after_column = original.replace(
        "CREATE TABLE bot_child_chats",
        "SELECT * FROM injected_ai26_early_failure;\nCREATE TABLE bot_child_chats",
    );
    let after_tables = original.replace(
        "CREATE TEMP TABLE bot_ai26_fk_guard",
        "SELECT * FROM injected_ai26_middle_failure;\nCREATE TEMP TABLE bot_ai26_fk_guard",
    );
    assert_ne!(after_column, original);
    assert_ne!(after_tables, original);
    for (sql, marker) in [
        (after_column, "injected_ai26_early_failure"),
        (after_tables, "injected_ai26_middle_failure"),
        (
            format!("{original}\nSELECT * FROM injected_ai26_late_failure;"),
            "injected_ai26_late_failure",
        ),
    ] {
        let mut c = populated_ai25();
        let before = snapshot(&c);
        let steps = Box::leak(
            vec![Migration {
                to: 26,
                name: "026_failure_fixture.sql",
                sql: Box::leak(sql.into_boxed_str()),
                columns: &[],
            }]
            .into_boxed_slice(),
        );
        let schema = Schema {
            database: crate::DATABASE_FILE,
            understood: 26,
            migrations: steps,
        };
        let error =
            koru_sqlite::prepare_with_preflight(&schema, &mut c, refuse_superseded_18).unwrap_err();
        assert!(error.to_string().contains(marker), "{error}");
        // The stamp, the added column, the new tables and every row are as
        // they were: the step is one transaction.
        assert_eq!(user_version(&c).unwrap(), 25);
        assert_eq!(snapshot(&c), before);
        assert!(!columns(&c, "bots").contains(&"may_ask".to_owned()));
        foreign_keys_held(&c);
        // The real step still applies to what the failure left.
        prepare(&mut c).unwrap();
        assert_eq!(user_version(&c).unwrap(), 26);
    }
}

#[test]
fn m6_ai26_migration_rollback_on_a_failing_guard() {
    // A database that already holds a broken reference is not upgraded.
    // Nothing is changed and the version stays 25.
    let mut c = populated_ai25();
    c.execute_batch(
        "PRAGMA foreign_keys=OFF;
         INSERT INTO bot_chats(chat_id,bot_id,purpose,created_at_ms) VALUES('orphan-chat','no-such-bot','additional',1);",
    )
    .unwrap();
    let before = snapshot(&c);
    let error = prepare(&mut c).unwrap_err();
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "{error}"
    );
    assert_eq!(user_version(&c).unwrap(), 25);
    assert_eq!(snapshot(&c), before);
    assert!(!columns(&c, "bots").contains(&"may_ask".to_owned()));
    // With the broken row removed the same database upgrades.
    c.execute_batch(
        "PRAGMA foreign_keys=OFF; DELETE FROM bot_chats WHERE chat_id='orphan-chat'; PRAGMA foreign_keys=ON;",
    )
    .unwrap();
    prepare(&mut c).unwrap();
    assert_eq!(user_version(&c).unwrap(), 26);
}

#[test]
fn m6_ai26_migration_an_ai25_build_refuses_26_before_preflight_or_configuration() {
    let mut c = populated_ai25();
    prepare(&mut c).unwrap();
    let before = snapshot(&c);
    c.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA synchronous=OFF")
        .unwrap();
    let error =
        koru_sqlite::prepare_with_preflight(&ai25(), &mut c, |_| panic!("old preflight ran"))
            .unwrap_err();
    assert!(matches!(
        error,
        OpenError::SchemaTooNew {
            discovered: 26,
            supported: 25,
            ..
        }
    ));
    assert_eq!(user_version(&c).unwrap(), 26);
    assert_eq!(snapshot(&c), before);
    // This build refuses a later one the same way.
    c.execute_batch("PRAGMA user_version=27").unwrap();
    assert!(matches!(
        prepare(&mut c).unwrap_err(),
        OpenError::SchemaTooNew {
            discovered: 27,
            supported: 26,
            ..
        }
    ));
}

/// The real open path, on a temporary file: a populated ai25 file opened by
/// this build is migrated once, keeps every row, and reopens unchanged.
#[test]
fn m6_ai26_migration_through_the_real_open_of_a_populated_ai25_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ai.db");
    let before = {
        let mut c = Connection::open(&path).unwrap();
        koru_sqlite::prepare_with_preflight(&ai25(), &mut c, refuse_superseded_18).unwrap();
        c.execute_batch(
            "INSERT INTO chats(id,title,current_provider,current_model,created_at_ms,updated_at_ms) VALUES('chat','kept','agent-codex','default',1,2);
             INSERT INTO bots(bot_id,user_scope_id,home_workspace_id,owner_device_id,name,role_workspace_id,role_document_id,working_folder,provider,model,reasoning,revision,policy_revision,access,created_at_ms,updated_at_ms,avatar) VALUES('bot','scope','home','device','kept','home','role','folder','agent-codex','default','auto',1,1,'default',1,2,11);
             INSERT INTO bot_chats(chat_id,bot_id,purpose,created_at_ms) VALUES('chat','bot','main',1);",
        )
        .unwrap();
        assert_eq!(user_version(&c).unwrap(), 25);
        snapshot(&c)
    };
    let mut first = None;
    for _ in 0..2 {
        let c = open(&path).unwrap();
        assert_eq!(user_version(&c).unwrap(), 26);
        let after = snapshot(&c);
        for (name, (sql, rows)) in &before {
            if name == "bots" {
                assert_eq!(
                    after[name]
                        .1
                        .iter()
                        .map(|row| without_may_ask(row))
                        .collect::<Vec<_>>(),
                    *rows
                );
                continue;
            }
            assert_eq!(sql, &after[name].0, "definition changed: {name}");
            assert_eq!(rows, &after[name].1, "raw rows changed: {name}");
        }
        assert!(after.contains_key("bot_child_chats"));
        // The second open finds exactly what the first one left.
        assert_eq!(*first.get_or_insert_with(|| after.clone()), after);
    }
    // The switch turned off after the migration is kept across a reopen.
    {
        let c = open(&path).unwrap();
        c.execute("UPDATE bots SET may_ask=0 WHERE bot_id='bot'", [])
            .unwrap();
    }
    let c = open(&path).unwrap();
    assert_eq!(
        c.query_row("SELECT may_ask FROM bots WHERE bot_id='bot'", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

// ---- m6_child_chat_rows: the tables' own keys, checks and triggers ----------

/// A migrated populated database with a second conversation of the recipient
/// and a `start` receipt whose id is a full-length request id.
fn with_child_fixture() -> Connection {
    let mut c = populated_ai25();
    prepare(&mut c).unwrap();
    for n in ["1", "2", "3", "4"] {
        c.execute_batch(&format!(
            "INSERT INTO chats(id,title,current_provider,current_model,created_at_ms,updated_at_ms) VALUES('child-chat-{n}','child','agent-codex','default',5,5);
             INSERT INTO bot_chats(chat_id,bot_id,purpose,created_at_ms) VALUES('child-chat-{n}','bot-two','additional',5);
             INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,planned_run_id,captured_config_revision,captured_policy_revision,submission_version,submission_json,created_at_ms,updated_at_ms)
               VALUES('scope','{}','bot-two','fingerprint','start','prepared','child-run-{n}',3,2,1,'{{}}',5,5);",
            pad(&format!("request-{n}"))
        ))
        .unwrap();
    }
    c
}

fn child_chat(request: &str, chat: &str, key: &str, ordinal: i64) -> String {
    format!(
        "INSERT INTO bot_child_chats(user_scope_id,child_request_id,parent_bot_id,parent_chat_id,parent_run_id,parent_assistant_message_id,ask_key,ordinal,ask_fingerprint,wait_id,chat_id,created_at_ms)
         VALUES('scope','{}','bot','chat','{}','{}','{}',{ordinal},'fingerprint','{}','{chat}',6);",
        pad(request),
        pad("parent-run"),
        pad("parent-message"),
        pad(key),
        pad("wait")
    )
}

fn enrollment(wait: &str) -> String {
    format!(
        "INSERT INTO bot_child_wait_enrollments(wait_id,run_id,ai_attempt_id,wait_revision,origin_attempt_id,origin_generation,origin_session_id,origin_turn_id,receipt_digest,receipt_json,created_at_ms)
         VALUES('{}','{}','attempt',1,'{}',1,'{}','{}',zeroblob(32),'{{}}',7);",
        pad(wait),
        pad("parent-run"),
        pad("origin-attempt"),
        pad("origin-session"),
        pad("origin-turn")
    )
}

fn rollover(wait: &str, consumer: &str) -> String {
    format!(
        "INSERT INTO bot_child_replay_rollovers(wait_id,ai_attempt_id,old_attempt_id,old_generation,old_session_id,old_turn_id,old_last_sequence,consumer_attempt_id,consumer_generation,consumer_session_id,consumer_turn_id,installed_after_sequence,resume_receipt_json,created_at_ms)
         VALUES('{}','attempt','{}',1,'{}','{}',NULL,'{}',2,'{}','{}',NULL,'{{}}',8);",
        pad(wait),
        pad("origin-attempt"),
        pad("origin-session"),
        pad("origin-turn"),
        pad(consumer),
        pad("consumer-session"),
        pad("consumer-turn")
    )
}

#[test]
fn m6_child_chat_rows_keys_and_foreign_keys_of_the_chat_table() {
    let c = with_child_fixture();
    c.execute_batch(&child_chat("request-1", "child-chat-1", "ask", 0))
        .unwrap();
    c.execute_batch(&child_chat("request-2", "child-chat-2", "ask", 1))
        .unwrap();
    // The same request twice, the same ordinal of one ask twice, and one
    // conversation for two requests.
    refused(
        &c,
        &child_chat("request-1", "child-chat-3", "other", 0),
        "UNIQUE constraint failed",
    );
    refused(
        &c,
        &child_chat("request-3", "child-chat-3", "ask", 1),
        "UNIQUE constraint failed",
    );
    refused(
        &c,
        &child_chat("request-3", "child-chat-2", "ask", 2),
        "UNIQUE constraint failed",
    );
    // At most three items in one ask.
    refused(
        &c,
        &child_chat("request-3", "child-chat-3", "ask", 3),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &child_chat("request-3", "child-chat-3", "ask", -1),
        "CHECK constraint failed",
    );
    // The child conversation is never the parent's.
    refused(
        &c,
        &child_chat("request-3", "chat", "ask", 2),
        "CHECK constraint failed",
    );
    // A request needs its recipient's receipt, a parent Bot, the parent's
    // conversation and its own conversation.
    refused(
        &c,
        &child_chat("no-receipt", "child-chat-3", "ask", 2),
        "FOREIGN KEY constraint failed",
    );
    refused(
        &c,
        &child_chat("request-3", "no-such-chat", "ask", 2),
        "FOREIGN KEY constraint failed",
    );
    refused(
        &c,
        &child_chat("request-3", "child-chat-3", "ask", 2)
            .replace("'bot','chat'", "'ghost','chat'"),
        "FOREIGN KEY constraint failed",
    );
    refused(
        &c,
        &child_chat("request-3", "child-chat-3", "ask", 2).replace("'bot','chat'", "'bot','ghost'"),
        "FOREIGN KEY constraint failed",
    );
    // Identities have their full length.
    for short in ["parent-run", "parent-message", "wait"] {
        refused(
            &c,
            &child_chat("request-3", "child-chat-3", "ask", 2).replace(&pad(short), short),
            "CHECK constraint failed",
        );
    }
    c.execute_batch(&child_chat("request-3", "child-chat-3", "ask", 2))
        .unwrap();
    // The rows keep what they reference in place.
    for held in [
        "DELETE FROM bot_chats WHERE chat_id='child-chat-1'",
        "DELETE FROM bot_chats WHERE chat_id='chat'",
        "DELETE FROM bots WHERE bot_id='bot'",
    ] {
        refused(&c, held, "FOREIGN KEY constraint failed");
    }
    refused(
        &c,
        &format!(
            "DELETE FROM bot_command_receipts WHERE request_id='{}'",
            pad("request-1")
        ),
        "FOREIGN KEY constraint failed",
    );
    // The parent's card reads its rows through the index, in creation order.
    let plan: String = c
        .prepare(
            "EXPLAIN QUERY PLAN SELECT child_request_id FROM bot_child_chats
             WHERE parent_chat_id='chat' ORDER BY created_at_ms,child_request_id",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(3))
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(plan.contains("bot_child_chats_parent"), "{plan}");
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
}

#[test]
fn m6_child_chat_rows_enrollment_and_rollover_are_immutable_and_keyed_by_wait() {
    let c = with_child_fixture();
    c.execute_batch(&enrollment("wait-1")).unwrap();
    // One enrollment per wait.
    refused(&c, &enrollment("wait-1"), "UNIQUE constraint failed");
    // Its checks.
    refused(
        &c,
        &enrollment("wait-2").replace("'attempt',1,", "'attempt',0,"),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &enrollment("wait-2").replace("zeroblob(32)", "zeroblob(31)"),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &enrollment("wait-2").replace("'{}'", "'not json'"),
        "CHECK constraint failed",
    );
    refused(
        &c,
        &enrollment("wait-2").replace("'attempt',1,", "'no-such-attempt',1,"),
        "FOREIGN KEY constraint failed",
    );
    refused(
        &c,
        &enrollment("wait-2").replace(&pad("origin-turn"), "short"),
        "CHECK constraint failed",
    );
    // Immutable and retained.
    refused(
        &c,
        "UPDATE bot_child_wait_enrollments SET wait_revision=2",
        "immutable AI child wait enrollment",
    );
    refused(
        &c,
        "DELETE FROM bot_child_wait_enrollments",
        "retained AI child wait enrollment",
    );

    // A rollover needs its wait's enrollment and is one per wait.
    refused(
        &c,
        &rollover("wait-2", "consumer-1"),
        "FOREIGN KEY constraint failed",
    );
    c.execute_batch(&rollover("wait-1", "consumer-1")).unwrap();
    refused(
        &c,
        &rollover("wait-1", "consumer-2"),
        "UNIQUE constraint failed",
    );
    // Two waits of one Run each have a rollover; the same installed consumer
    // cannot be recorded twice for one AI attempt.
    c.execute_batch(&enrollment("wait-2")).unwrap();
    refused(
        &c,
        &rollover("wait-2", "consumer-1"),
        "UNIQUE constraint failed",
    );
    c.execute_batch(&rollover("wait-2", "consumer-2")).unwrap();
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM bot_child_replay_rollovers r JOIN bot_child_wait_enrollments e USING(wait_id) WHERE e.run_id=?1",
            [pad("parent-run")],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    refused(
        &c,
        &rollover("wait-2", "consumer-3").replace("NULL,'{}'", "-1,'{}'"),
        "CHECK constraint failed",
    );
    refused(
        &c,
        "UPDATE bot_child_replay_rollovers SET consumer_generation=3",
        "immutable child replay rollover",
    );
    refused(
        &c,
        "DELETE FROM bot_child_replay_rollovers",
        "retained child replay rollover",
    );
    // The switch is a 0/1 flag.
    refused(
        &c,
        "UPDATE bots SET may_ask=2 WHERE bot_id='bot'",
        "CHECK constraint failed",
    );
    refused(
        &c,
        "UPDATE bots SET may_ask=NULL WHERE bot_id='bot'",
        "NOT NULL constraint failed",
    );
    c.execute("UPDATE bots SET may_ask=0 WHERE bot_id='bot'", [])
        .unwrap();
}
