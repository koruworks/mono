// SPDX-License-Identifier: MPL-2.0
//! ai26 → ai27 (the team pause, M8 P8.1) on the actual migrator and linked
//! SQLite. In-memory, plus one temporary file for the real open path. No
//! worker, Store, delegate or provider, and never a real database.
//!
//! Cohort `m8_ai_team_migration`.
use super::children::{ai26, populated_ai25};
use super::library::{foreign_keys_held, snapshot};
use super::routine_controls::foreign_keys;
use super::*;

const TABLE: &str = "bot_team_pause";

/// The populated fixture every earlier step is tested with, migrated to ai26
/// and no further, with a value in the one column ai26 added so the step
/// under test meets data only an ai26 build could have written.
fn populated_ai26() -> Connection {
    let mut c = populated_ai25();
    koru_sqlite::prepare_with_preflight(&ai26(), &mut c, refuse_superseded_18).unwrap();
    assert_eq!(user_version(&c).unwrap(), 26);
    c.execute("UPDATE bots SET may_ask=0 WHERE bot_id='bot-two'", [])
        .unwrap();
    c
}

/// This step and no later one: ai.db 28 exists now, so the real opener goes
/// further than the step under test.
pub(super) fn ai27() -> Schema {
    Schema {
        database: crate::DATABASE_FILE,
        understood: 27,
        migrations: &MIGRATIONS[..27],
    }
}
fn prepare_ai27(c: &mut Connection) -> Result<(), OpenError> {
    koru_sqlite::prepare_with_preflight(&ai27(), c, refuse_superseded_18)?;
    refuse_superseded_18(c)
}

fn columns(c: &Connection, table: &str) -> Vec<String> {
    c.prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
        .unwrap()
        .query_map([table], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn table_exists(c: &Connection) -> bool {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [TABLE],
        |r| r.get(0),
    )
    .unwrap()
}

fn refused(c: &Connection, sql: &str, why: &str) {
    let error = c.execute_batch(sql).expect_err(&format!("accepted: {sql}"));
    assert!(error.to_string().contains(why), "{why}: {error}\n{sql}");
}

fn row(scope: &str, paused: i64, revision: i64, stage: &str, acknowledged: &str) -> String {
    format!(
        "INSERT INTO bot_team_pause(user_scope_id,owner_device_id,paused,revision,request_id,fingerprint,stage,changed_at_ms,acknowledged_at_ms)
         VALUES('{scope}','device',{paused},{revision},'{}','{}','{stage}',5,{acknowledged});",
        "r".repeat(36),
        "f".repeat(64)
    )
}

#[test]
fn m8_ai_team_migration_keeps_every_populated_ai26_row_and_changes_no_bot() {
    let mut c = populated_ai26();
    println!("Rust-linked SQLite {}", rusqlite::version());
    let before = snapshot(&c);
    let keys_before = foreign_keys(&c);
    assert!(
        before.values().filter(|(_, rows)| !rows.is_empty()).count() > 10,
        "the fixture is not populated"
    );
    assert!(
        before["bots"].1.len() >= 2,
        "the fixture holds several Bots"
    );
    assert!(!before.contains_key(TABLE));

    prepare_ai27(&mut c).unwrap();
    assert_eq!(user_version(&c).unwrap(), 27);
    let after = snapshot(&c);

    // No table was rebuilt, rewritten or re-encoded, and no column was
    // added: every definition and every raw row is as it was. `bots` first.
    assert_eq!(after["bots"], before["bots"], "a Bot row changed");
    for (name, (sql, rows)) in &before {
        let (new_sql, new_rows) = &after[name];
        assert_eq!(sql, new_sql, "definition changed: {name}");
        assert_eq!(rows, new_rows, "raw rows changed: {name}");
    }
    // What is new is exactly the one table, and it is empty: a migrated
    // team was never paused.
    let added: Vec<&str> = after
        .keys()
        .filter(|name| !before.contains_key(*name))
        .map(String::as_str)
        .collect();
    assert_eq!(added, [TABLE]);
    assert!(after[TABLE].1.is_empty());
    assert!(table_exists(&c));
    // No foreign key moved, none is violated.
    let keys_after = foreign_keys(&c);
    for (table, keys) in &keys_before {
        assert_eq!(&keys_after[table], keys, "{table}");
    }
    foreign_keys_held(&c);

    // Reopened, it is already current and nothing runs again.
    prepare_ai27(&mut c).unwrap();
    assert_eq!(snapshot(&c), after);
}

#[test]
fn m8_ai_team_migration_empty_and_fresh_databases_reach_27_with_no_content_column() {
    // An empty ai26 database, as a build that never made a Bot left it.
    let mut empty = koru_sqlite::open_in_memory(&ai26()).unwrap();
    assert_eq!(user_version(&empty).unwrap(), 26);
    assert!(!table_exists(&empty));
    prepare_ai27(&mut empty).unwrap();
    assert_eq!(user_version(&empty).unwrap(), 27);
    // A fresh database, at this step.
    let fresh = koru_sqlite::open_in_memory(&ai27()).unwrap();
    assert!(MIGRATIONS.len() >= 27);
    assert_eq!(MIGRATIONS[26].to, 27);
    assert_eq!(MIGRATIONS[26].name, "027_ai_bot_team_pause.sql");
    assert_eq!(user_version(&fresh).unwrap(), 27);
    assert_eq!(snapshot(&empty), snapshot(&fresh));
    for c in [&empty, &fresh] {
        assert_eq!(
            columns(c, TABLE),
            [
                "user_scope_id",
                "owner_device_id",
                "paused",
                "revision",
                "request_id",
                "fingerprint",
                "stage",
                "changed_at_ms",
                "acknowledged_at_ms"
            ]
        );
        // No column can hold a task, a title, an answer or a credential.
        for column in columns(c, TABLE) {
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
                assert!(!column.contains(word), "{column}");
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
fn m8_ai_team_migration_failure_at_any_point_leaves_26_and_every_row() {
    let original = MIGRATIONS[26].sql;
    let before_table = original.replace(
        "CREATE TABLE bot_team_pause",
        "SELECT * FROM injected_ai27_early_failure;\nCREATE TABLE bot_team_pause",
    );
    assert_ne!(before_table, original);
    for (sql, marker) in [
        (before_table, "injected_ai27_early_failure"),
        (
            format!("{original}\nSELECT * FROM injected_ai27_late_failure;"),
            "injected_ai27_late_failure",
        ),
    ] {
        let mut c = populated_ai26();
        let before = snapshot(&c);
        let steps = Box::leak(
            vec![Migration {
                to: 27,
                name: "027_failure_fixture.sql",
                sql: Box::leak(sql.into_boxed_str()),
                columns: &[],
            }]
            .into_boxed_slice(),
        );
        let schema = Schema {
            database: crate::DATABASE_FILE,
            understood: 27,
            migrations: steps,
        };
        let error =
            koru_sqlite::prepare_with_preflight(&schema, &mut c, refuse_superseded_18).unwrap_err();
        assert!(error.to_string().contains(marker), "{error}");
        // The stamp, the new table and every row are as they were: the step
        // is one transaction.
        assert_eq!(user_version(&c).unwrap(), 26);
        assert_eq!(snapshot(&c), before);
        assert!(!table_exists(&c));
        foreign_keys_held(&c);
        // The real step still applies to what the failure left.
        prepare_ai27(&mut c).unwrap();
        assert_eq!(user_version(&c).unwrap(), 27);
        assert!(table_exists(&c));
    }
}

#[test]
fn m8_ai_team_migration_an_ai26_build_refuses_27_before_preflight_or_configuration() {
    let mut c = populated_ai26();
    prepare_ai27(&mut c).unwrap();
    let before = snapshot(&c);
    c.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA synchronous=OFF")
        .unwrap();
    let error =
        koru_sqlite::prepare_with_preflight(&ai26(), &mut c, |_| panic!("old preflight ran"))
            .unwrap_err();
    assert!(matches!(
        error,
        OpenError::SchemaTooNew {
            discovered: 27,
            supported: 26,
            ..
        }
    ));
    assert_eq!(user_version(&c).unwrap(), 27);
    assert_eq!(snapshot(&c), before);
    // An ai27 build refuses a later one the same way.
    c.execute_batch("PRAGMA user_version=28").unwrap();
    assert!(matches!(
        prepare_ai27(&mut c).unwrap_err(),
        OpenError::SchemaTooNew {
            discovered: 28,
            supported: 27,
            ..
        }
    ));
}

/// The real open path, on a temporary file: a populated ai26 file opened by
/// this build is migrated once, keeps every row, and reopens unchanged.
#[test]
fn m8_ai_team_migration_through_the_real_open_of_a_populated_ai26_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ai.db");
    let before = {
        let mut c = Connection::open(&path).unwrap();
        koru_sqlite::prepare_with_preflight(&ai26(), &mut c, refuse_superseded_18).unwrap();
        c.execute_batch(
            "INSERT INTO chats(id,title,current_provider,current_model,created_at_ms,updated_at_ms) VALUES('chat','kept','agent-codex','default',1,2);
             INSERT INTO bots(bot_id,user_scope_id,home_workspace_id,owner_device_id,name,role_workspace_id,role_document_id,working_folder,provider,model,reasoning,revision,policy_revision,access,paused,created_at_ms,updated_at_ms,avatar,may_ask) VALUES('bot','scope','home','device','kept','home','role','folder','agent-codex','default','auto',4,2,'default',1,1,2,11,0);
             INSERT INTO bot_chats(chat_id,bot_id,purpose,created_at_ms) VALUES('chat','bot','main',1);",
        )
        .unwrap();
        assert_eq!(user_version(&c).unwrap(), 26);
        snapshot(&c)
    };
    let mut first = None;
    for _ in 0..2 {
        // The real opener runs every later step too.
        let c = open(&path).unwrap();
        assert_eq!(user_version(&c).unwrap(), SCHEMA_VERSION);
        let after = snapshot(&c);
        for (name, (sql, rows)) in &before {
            assert_eq!(sql, &after[name].0, "definition changed: {name}");
            assert_eq!(rows, &after[name].1, "raw rows changed: {name}");
        }
        assert!(after[TABLE].1.is_empty());
        // The second open finds exactly what the first one left.
        assert_eq!(*first.get_or_insert_with(|| after.clone()), after);
    }
    // A pause written after the migration is kept across a reopen: that is
    // what makes it durable.
    {
        let c = open(&path).unwrap();
        c.execute_batch(&row("scope", 1, 1, "pending", "NULL"))
            .unwrap();
    }
    let c = open(&path).unwrap();
    assert_eq!(
        c.query_row(
            "SELECT paused,revision,stage FROM bot_team_pause WHERE user_scope_id='scope' AND owner_device_id='device'",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))
        )
        .unwrap(),
        (1, 1, "pending".to_owned())
    );
}

/// The table's own key and checks.
#[test]
fn m8_ai_team_migration_the_table_accepts_only_well_formed_rows() {
    let c = open_in_memory().unwrap();
    c.execute_batch(&row("scope", 1, 1, "pending", "NULL"))
        .unwrap();
    c.execute_batch(&row("other", 0, 9, "acknowledged", "7"))
        .unwrap();
    // One row per person and computer.
    refused(
        &c,
        &row("scope", 0, 2, "pending", "NULL"),
        "UNIQUE constraint failed",
    );
    for (sql, why) in [
        (row("a", 2, 1, "pending", "NULL"), "CHECK constraint failed"),
        (row("b", 1, 0, "pending", "NULL"), "CHECK constraint failed"),
        (row("c", 1, 1, "done", "NULL"), "CHECK constraint failed"),
        // The stage and its time agree.
        (row("d", 1, 1, "pending", "7"), "CHECK constraint failed"),
        (
            row("e", 1, 1, "acknowledged", "NULL"),
            "CHECK constraint failed",
        ),
        (
            row("f", 1, 1, "pending", "NULL").replace(&"r".repeat(36), "short"),
            "CHECK constraint failed",
        ),
        (
            row("g", 1, 1, "pending", "NULL").replace(&"f".repeat(64), "short"),
            "CHECK constraint failed",
        ),
    ] {
        refused(&c, &sql, why);
    }
    assert_eq!(
        c.query_row("SELECT count(*) FROM bot_team_pause", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
}
