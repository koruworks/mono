// SPDX-License-Identifier: MPL-2.0

//! Migration from empty and from each version, refuse-newer before any
//! pragma, two-root isolation, and the step-per-version rule held before
//! schema 4 exists.

use rusqlite::Connection;

use super::*;
use crate::{RunsDb, fixtures::Root};

fn objects(conn: &Connection) -> Vec<String> {
    let mut statement = conn
        .prepare("SELECT name FROM sqlite_master WHERE type IN ('table', 'index') ORDER BY name")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn a_fresh_database_migrates_from_empty_and_reopens_at_the_version() {
    let root = Root::new();
    let db = root.open();
    assert_eq!(db.schema_version().unwrap(), RUNS_SCHEMA_VERSION);
    let names = objects(db.conn());
    for table in [
        "run_intents",
        "run_attempts",
        "pending_publications",
        "pending_publications_open",
        "projection_cursors",
        "schedule_claims",
        "run_deletions",
        "schedule_publications",
        "schedule_publications_definition_fact",
        "schedule_publications_created_root",
        "schedule_publications_occurrence_outcome",
        "schedule_publications_schedule",
        "schedule_publications_ready",
        "schedule_publications_prepared",
    ] {
        assert!(
            names.iter().any(|n| n == table),
            "missing {table}: {names:?}"
        );
    }
    drop(db);
    // Rule 10: the fresh shape is inspected through production code, and a
    // second open finds it and runs no step.
    let again = root.open();
    assert_eq!(again.schema_version().unwrap(), RUNS_SCHEMA_VERSION);
    assert_eq!(objects(again.conn()), names);
}

/// A database stamped at every version below the current one migrates
/// forward by exactly the steps it lacks: empty, v1, v2 and v3 all reach v4.
#[test]
fn a_database_at_each_older_version_migrates_forward() {
    for stamped in 0..RUNS_SCHEMA_VERSION {
        let root = Root::new();
        let path = root.db_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        {
            let older = Connection::open(&path).unwrap();
            for migration in MIGRATIONS.iter().filter(|m| m.to <= stamped) {
                older.execute_batch(migration.sql).unwrap();
            }
            if stamped >= 1 {
                older
                    .execute(
                        "INSERT INTO run_intents
                           (run_id, workspace_id, payload_json, state, created_at_ms, updated_at_ms)
                         VALUES (?1, ?2, '{}', 'pending', 1, 1)",
                        rusqlite::params![
                            crate::fixtures::run_id(0xe1).to_storage(),
                            crate::fixtures::workspace(0xe2).to_storage(),
                        ],
                    )
                    .unwrap();
            }
            if stamped >= 2 {
                older
                    .execute(
                        "INSERT INTO run_deletions (run_id, artifacts, decided_at_ms)
                         VALUES (?1, 'keep', 2)",
                        [crate::fixtures::run_id(0xe3).to_storage()],
                    )
                    .unwrap();
            }
            if stamped >= 3 {
                older
                    .execute(
                        "INSERT INTO schedule_claims
                           (schedule_id, occurrence_ms, revision, run_id, outcome,
                            claimed_at_ms, busy_holder_run_id)
                         VALUES (?1, 3, 1, ?2, 'skipped_busy', 3, ?3)",
                        rusqlite::params![
                            crate::fixtures::schedule(0xe4).to_storage(),
                            crate::fixtures::run_id(0xe5).to_storage(),
                            crate::fixtures::run_id(0xe6).to_storage(),
                        ],
                    )
                    .unwrap();
            }
            older.pragma_update(None, "user_version", stamped).unwrap();
        }
        let db = root.open();
        assert_eq!(
            db.schema_version().unwrap(),
            RUNS_SCHEMA_VERSION,
            "from {stamped}"
        );
        let names = objects(db.conn());
        assert!(names.iter().any(|n| n == "schedule_claims"));
        assert!(
            names.iter().any(|n| n == "schedule_publications"),
            "schema {stamped} did not reach the v4 outbox"
        );
        if stamped >= 1 {
            let count: i64 = db
                .conn()
                .query_row("SELECT count(*) FROM run_intents", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 1, "v{stamped} intent was not preserved");
        }
        if stamped >= 2 {
            let count: i64 = db
                .conn()
                .query_row("SELECT count(*) FROM run_deletions", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 1, "v{stamped} deletion receipt was not preserved");
        }
        if stamped >= 3 {
            let holder: String = db
                .conn()
                .query_row(
                    "SELECT busy_holder_run_id FROM schedule_claims",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(holder, crate::fixtures::run_id(0xe6).to_storage());
        }
    }
}

/// One step per version, strictly ascending, ending at the constant, each
/// named by its numbered file. A step that has shipped is never edited; the
/// next version is the next entry.
#[test]
fn migrations_are_one_step_per_version_ending_at_the_constant() {
    let versions: Vec<i64> = MIGRATIONS.iter().map(|m| m.to).collect();
    assert_eq!(versions.first(), Some(&1));
    assert_eq!(versions.last(), Some(&RUNS_SCHEMA_VERSION));
    assert!(
        versions.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "every version has exactly one step: {versions:?}"
    );
    for migration in MIGRATIONS {
        assert!(
            migration.name.ends_with("_migration.sql")
                || (migration.to == 9 && migration.name == "028_runs_bot_routines.sql")
                || (migration.to == 10 && migration.name == "029_runs_bot_decisions.sql")
                || (migration.to == 11 && migration.name == "030_runs_private_operations.sql"),
            "{}",
            migration.name
        );
        // Statements only: the header comment may say what the code does.
        let statements: String = migration
            .sql
            .lines()
            .filter(|line| !line.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        assert!(
            !statements.contains("user_version"),
            "{}: the code stamps the version, the file never does",
            migration.name
        );
        assert!(
            statements.split(';').all(|statement| {
                // CREATE TRIGGER ... BEGIN ... END is one SQL statement,
                // not an attempt to own the migration transaction.
                !matches!(
                    statement
                        .split_whitespace()
                        .next()
                        .map(str::to_ascii_uppercase)
                        .as_deref(),
                    Some("BEGIN" | "COMMIT" | "ROLLBACK" | "SAVEPOINT" | "RELEASE")
                )
            }),
            "{}: the code owns the transaction",
            migration.name
        );
    }
}

/// A newer build's file is refused before any pragma is touched: the
/// `journal_mode` it chose survives, its stamp survives, and the refusal
/// names both numbers as `schema_too_new` does everywhere else.
#[test]
fn a_newer_database_is_refused_before_anything_is_changed() {
    let root = Root::new();
    let path = root.db_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    {
        let newer = Connection::open(&path).unwrap();
        newer
            .execute_batch("PRAGMA journal_mode = DELETE; PRAGMA user_version = 99;")
            .unwrap();
    }
    assert_eq!(
        RunsDb::open(&path).err(),
        Some(OpenError::SchemaTooNew {
            database: DATABASE_NAME,
            discovered: 99,
            supported: RUNS_SCHEMA_VERSION
        })
    );
    let untouched = Connection::open(&path).unwrap();
    let mode: String = untouched
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "delete", "the refusal changed nothing");
    assert_eq!(user_version(&untouched).unwrap(), 99);
    assert!(objects(&untouched).is_empty(), "no step ran");
}

/// Facts a person is entitled to see are written at FULL, in WAL.
#[test]
fn the_ledger_is_durable_at_full_and_journals_in_wal() {
    let root = Root::new();
    let db = root.open();
    let synchronous: i64 = db
        .conn()
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .unwrap();
    assert_eq!(synchronous, 2, "FULL");
    let mode: String = db
        .conn()
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
    let foreign_keys: i64 = db
        .conn()
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(foreign_keys, 1);
}

/// Two roots are two files that know nothing of each other, each where a
/// `--root R` stack would look — `R/delegate/runs.db` beside `R/store` —
/// and neither grows a directory nothing opened.
#[test]
fn two_roots_hold_two_independent_databases() {
    let a = Root::new();
    let b = Root::new();
    let mut on_a = a.open();
    crate::fixtures::created_run(&mut on_a, 1);
    let on_b = b.open();
    assert_eq!(on_b.pending_intents().unwrap().len(), 0);
    assert!(on_b.intent(crate::fixtures::run_id(1)).unwrap().is_none());
    assert_eq!(a.db_path(), a.path().join("delegate").join(DATABASE_NAME));
    assert!(a.db_path().is_file());
    assert!(b.db_path().is_file());
    assert!(!a.path().join("artifacts").exists(), "nothing opened it");
    assert!(!a.path().join("store").join("delegate").exists());
}

/// `open_beside` and `runs_db_path` are the same rule.
#[test]
fn open_beside_places_the_file_where_the_channel_rule_says() {
    let root = Root::new();
    let db = RunsDb::open_beside(&root.store_root()).unwrap();
    assert_eq!(db.schema_version().unwrap(), RUNS_SCHEMA_VERSION);
    assert!(root.db_path().is_file());
    assert_eq!(
        format!("{db:?}"),
        format!("RunsDb {{ schema: {RUNS_SCHEMA_VERSION} }}")
    );
}
