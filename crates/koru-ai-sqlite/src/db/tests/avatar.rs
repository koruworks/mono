// SPDX-License-Identifier: MPL-2.0
//! ai23 → ai24 (`bots.avatar`) on the actual migrator and linked SQLite.
//! In-memory, plus one temporary file for the real open path. No worker,
//! Store or delegate, and never a real database.
use super::library::{foreign_keys_held, snapshot};
use super::routine_controls::{ai23, foreign_keys, populated};
use super::*;

/// The whole of what ai24 adds to the stored definition of `bots`, with the
/// whitespace `snapshot` removes.
const ADDED: &str =
    ",avatarINTEGERCHECK(avatarISNULLOR(typeof(avatar)='integer'ANDavatarBETWEEN0AND23))";

/// This step and no later one: ai.db 25 exists now, so the real opener goes
/// further than the step under test.
pub(super) fn ai24() -> Schema {
    Schema {
        database: crate::DATABASE_FILE,
        understood: 24,
        migrations: &MIGRATIONS[..24],
    }
}
fn prepare_ai24(c: &mut Connection) {
    koru_sqlite::prepare_with_preflight(&ai24(), c, refuse_superseded_18).unwrap();
}

/// The populated ai22 fixture with the ai23 rows, migrated to ai23 and no
/// further: what the previous build left.
pub(super) fn populated_ai23() -> Connection {
    let mut c = populated();
    koru_sqlite::prepare_with_preflight(&ai23(), &mut c, refuse_superseded_18).unwrap();
    assert_eq!(user_version(&c).unwrap(), 23);
    c
}

/// A row as `snapshot` prints it, without the column ai24 appended.
fn without_avatar(row: &str) -> String {
    format!(
        "{}]",
        row.strip_suffix(", Null]")
            .unwrap_or_else(|| panic!("no NULL avatar at the end of {row}"))
    )
}

#[test]
fn migration_ai24_keeps_every_populated_ai23_row_and_adds_one_nullable_column() {
    let mut c = populated_ai23();
    println!("Rust-linked SQLite {}", rusqlite::version());
    let before = snapshot(&c);
    let keys_before = foreign_keys(&c);
    assert!(!before["bots"].1.is_empty(), "the fixture holds no Bot");
    prepare_ai24(&mut c);
    assert_eq!(user_version(&c).unwrap(), 24);
    let after = snapshot(&c);

    // No object is added or removed, and no foreign key moves.
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
    assert_eq!(foreign_keys(&c), keys_before);
    for (name, (sql, rows)) in &before {
        let (new_sql, new_rows) = &after[name];
        if name == "bots" {
            // The definition is the old one plus the one column.
            assert_eq!(
                new_sql.replacen(ADDED, "", 1),
                *sql,
                "bots changed beyond the new column"
            );
            assert_eq!(new_sql.matches(ADDED).count(), 1, "{new_sql}");
            // Every old row is there value for value, with no face picked.
            assert_eq!(
                new_rows
                    .iter()
                    .map(|row| without_avatar(row))
                    .collect::<Vec<_>>(),
                *rows
            );
            continue;
        }
        assert_eq!(sql, new_sql, "definition changed: {name}");
        assert_eq!(rows, new_rows, "raw rows changed: {name}");
    }
    let picked: i64 = c
        .query_row(
            "SELECT count(*) FROM bots WHERE avatar IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(picked, 0);
    foreign_keys_held(&c);

    // Reopened, it is already current and nothing runs again.
    prepare_ai24(&mut c);
    assert_eq!(snapshot(&c), after);
}

#[test]
fn migration_ai24_avatar_is_null_or_a_whole_number_inside_the_face_table() {
    let mut c = populated_ai23();
    prepare_ai24(&mut c);
    let case = |value: &str, accepts: bool| {
        let result = c.execute(&format!("UPDATE bots SET avatar={value}"), []);
        assert_eq!(result.is_ok(), accepts, "avatar={value}: {result:?}");
    };
    for value in ["0", "7", "23", "NULL"] {
        case(value, true);
    }
    for value in ["24", "-1", "255", "1.5", "'face'", "X'07'"] {
        case(value, false);
    }
    // The last accepted value is the one stored: a refusal changed nothing.
    let stored: Vec<Option<i64>> = c
        .prepare("SELECT avatar FROM bots")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(!stored.is_empty() && stored.iter().all(Option::is_none));
    assert_eq!(koru_ai_core::bots::BOT_AVATAR_COUNT, 24);
}

#[test]
fn migration_ai24_failure_rolls_back_the_column_and_the_stamp() {
    let original = MIGRATIONS[23].sql;
    let mut c = populated_ai23();
    let before = snapshot(&c);
    let steps = Box::leak(
        vec![Migration {
            to: 24,
            name: "024_failure_fixture.sql",
            sql: Box::leak(
                format!("{original}\nSELECT * FROM injected_ai24_failure;").into_boxed_str(),
            ),
            columns: &[],
        }]
        .into_boxed_slice(),
    );
    let schema = Schema {
        database: crate::DATABASE_FILE,
        understood: 24,
        migrations: steps,
    };
    let error =
        koru_sqlite::prepare_with_preflight(&schema, &mut c, refuse_superseded_18).unwrap_err();
    assert!(
        error.to_string().contains("injected_ai24_failure"),
        "{error}"
    );
    assert_eq!(user_version(&c).unwrap(), 23);
    assert_eq!(snapshot(&c), before);
    foreign_keys_held(&c);
    // The real step still applies to what the failure left.
    prepare_ai24(&mut c);
    assert_eq!(user_version(&c).unwrap(), 24);
}

#[test]
fn migration_ai23_build_refuses_ai24_before_preflight_or_configuration() {
    let mut c = populated_ai23();
    prepare_ai24(&mut c);
    let before = snapshot(&c);
    c.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA synchronous=OFF")
        .unwrap();
    let error =
        koru_sqlite::prepare_with_preflight(&ai23(), &mut c, |_| panic!("old preflight ran"))
            .unwrap_err();
    assert!(matches!(
        error,
        OpenError::SchemaTooNew {
            discovered: 24,
            supported: 23,
            ..
        }
    ));
    assert_eq!(user_version(&c).unwrap(), 24);
    assert_eq!(snapshot(&c), before);
    for pragma in ["PRAGMA foreign_keys", "PRAGMA synchronous"] {
        assert_eq!(c.query_row(pragma, [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
}

#[test]
fn migration_fresh_database_reaches_ai24_with_a_nullable_avatar_column() {
    let c = koru_sqlite::open_in_memory(&ai24()).unwrap();
    assert_eq!(MIGRATIONS[23].to, 24);
    assert_eq!(user_version(&c).unwrap(), 24);
    let (kind, not_null, default): (String, bool, Option<String>) = c
        .query_row(
            "SELECT type,\"notnull\",dflt_value FROM pragma_table_info('bots') WHERE name='avatar'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((kind.as_str(), not_null, default), ("INTEGER", false, None));
    assert_eq!(
        c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, u32>(
            0
        ))
        .unwrap(),
        0
    );
}

/// The real open path, on a temporary file: a populated ai23 file opened by
/// this build is migrated once, keeps its Bot, and reopens unchanged.
#[test]
fn migration_ai24_through_the_real_open_of_a_populated_ai23_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ai.db");
    let before = {
        let mut c = Connection::open(&path).unwrap();
        koru_sqlite::prepare_with_preflight(&ai23(), &mut c, refuse_superseded_18).unwrap();
        c.execute_batch(
            "INSERT INTO chats(id,title,current_provider,current_model,created_at_ms,updated_at_ms) VALUES('chat','kept','agent-codex','default',1,2);
             INSERT INTO bots(bot_id,user_scope_id,home_workspace_id,owner_device_id,name,role_workspace_id,role_document_id,working_folder,provider,model,reasoning,revision,policy_revision,access,created_at_ms,updated_at_ms) VALUES('bot','scope','home','device','kept','home','role','folder','agent-codex','default','auto',1,1,'default',1,2);
             INSERT INTO bot_chats(chat_id,bot_id,purpose,created_at_ms) VALUES('chat','bot','main',1);",
        )
        .unwrap();
        assert_eq!(user_version(&c).unwrap(), 23);
        snapshot(&c)
    };
    let mut first = None;
    for _ in 0..2 {
        // The real opener goes on to the current schema; the Bot rows this
        // step carried are compared all the same.
        let c = open(&path).unwrap();
        assert_eq!(user_version(&c).unwrap(), SCHEMA_VERSION);
        let after = snapshot(&c);
        for (name, (_, rows)) in &before {
            if name == "bots" {
                assert_eq!(
                    after[name]
                        .1
                        .iter()
                        .map(|row| without_avatar(row))
                        .collect::<Vec<_>>(),
                    *rows
                );
                continue;
            }
            assert_eq!(rows, &after[name].1, "raw rows changed: {name}");
        }
        // The second open finds exactly what the first one left.
        assert_eq!(*first.get_or_insert_with(|| after.clone()), after);
    }
    // A face picked after the migration is kept across a reopen.
    {
        let c = open(&path).unwrap();
        c.execute("UPDATE bots SET avatar=11 WHERE bot_id='bot'", [])
            .unwrap();
    }
    let c = open(&path).unwrap();
    assert_eq!(
        c.query_row("SELECT avatar FROM bots WHERE bot_id='bot'", [], |r| r
            .get::<_, Option<
            i64,
        >>(
            0
        ))
        .unwrap(),
        Some(11)
    );
}
