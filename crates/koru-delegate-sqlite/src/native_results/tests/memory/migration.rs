// SPDX-License-Identifier: MPL-2.0
use super::*;
use rusqlite::{Connection, types::Value};
mod routine;

fn tables(conn: &Connection) -> Vec<String> {
    conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap()
        .query_map([],|r|r.get(0)).unwrap().collect::<Result<_,_>>().unwrap()
}
fn columns(conn: &Connection, table: &str) -> Vec<String> {
    conn.prepare(&format!(
        "SELECT name FROM pragma_table_info('{table}') ORDER BY cid"
    ))
    .unwrap()
    .query_map([], |r| r.get(0))
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}
fn rows(conn: &Connection, table: &str, columns: &[String]) -> Vec<Vec<Value>> {
    conn.prepare(&format!(
        "SELECT {} FROM {table} ORDER BY rowid",
        columns.join(",")
    ))
    .unwrap()
    .query_map([], |r| (0..columns.len()).map(|i| r.get(i)).collect())
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}
fn seven(h: &Harness) -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    koru_sqlite::prepare(
        &koru_sqlite::Schema {
            database: crate::DATABASE_NAME,
            understood: 7,
            migrations: &crate::db::MIGRATIONS[..7],
        },
        &mut conn,
    )
    .unwrap();
    conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    // Copy only the frozen v7 columns into a real v7 schema. All fixture
    // identities/payloads were produced by normal in-memory admission/effects.
    for table in tables(&conn) {
        let columns = columns(&conn, &table);
        for row in rows(h.db.conn(), &table, &columns) {
            let marks = vec!["?"; columns.len()].join(",");
            conn.execute(
                &format!("INSERT INTO {table}({}) VALUES({marks})", columns.join(",")),
                rusqlite::params_from_iter(row),
            )
            .unwrap();
        }
    }
    conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    conn
}
fn schema() -> koru_sqlite::Schema {
    koru_sqlite::Schema {
        database: crate::DATABASE_NAME,
        understood: 8,
        migrations: &crate::db::MIGRATIONS[..8],
    }
}
fn snapshot(conn: &Connection) -> Vec<(String, Vec<String>, Vec<Vec<Value>>)> {
    tables(conn)
        .into_iter()
        .map(|table| {
            let cols = columns(conn, &table);
            let data = rows(conn, &table, &cols);
            (table, cols, data)
        })
        .collect()
}
fn assert_fk(conn: &Connection) {
    assert_eq!(
        conn.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
}

#[test]
fn runs_eight_migration_preserves_populated_seven_result_bytes_and_foreign_keys() {
    let mut h = Harness::new();
    let mut records = Vec::new();
    for cohort in 0..9 {
        h.request.key.operation_id = Uuid::new_v4();
        h.prepare();
        if cohort != 0 {
            h.lease();
        }
        if cohort == 2 {
            h.confirm(NativeResultObservation::Unconfirmed);
        }
        if cohort == 5 {
            h.confirm(NativeResultObservation::NotApplied {
                code: NativeResultRefusal::AccessDenied,
            });
        }
        if cohort >= 6 {
            let record = h.db.native_result(&h.request.key).unwrap().unwrap();
            let receipt = NativeEffectDrainReceipt {
                binding: record.pending_effect().unwrap().binding,
                fenced_at: NativeEffectTimestamp::from_millis(900),
                drained_at: NativeEffectTimestamp::from_millis(800),
            };
            h.confirm(NativeResultObservation::DrainedUnknown { receipt });
        }
        if matches!(cohort, 3 | 4 | 7 | 8) {
            let evidence = h.evidence();
            h.confirm(NativeResultObservation::Durable {
                evidence: evidence.clone(),
            });
            if matches!(cohort, 4 | 8) {
                h.db.register_native_result(&h.request.key, &evidence)
                    .unwrap();
            }
        }
        records.push(h.db.native_result(&h.request.key).unwrap().unwrap());
    }
    let mut conn = seven(&h);
    let before = snapshot(&conn);
    assert_fk(&conn);
    let before_indexes: Vec<String> = conn
        .prepare(
            "SELECT sql FROM sqlite_master WHERE type='index' AND sql IS NOT NULL ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    koru_sqlite::prepare(&schema(), &mut conn).unwrap();
    koru_sqlite::prepare(&schema(), &mut conn).unwrap();
    for (table, cols, data) in before {
        assert_eq!(rows(&conn, &table, &cols), data, "{table}");
    }
    for sql in before_indexes {
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND sql=?1",
                [sql],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        8
    );
    assert_fk(&conn);
    for retained in records {
        let migrated = super::super::super::rows::read(&conn, &retained.key)
            .unwrap()
            .unwrap();
        assert_eq!(migrated, retained);
        assert_eq!(migrated.fingerprint, h.request.request_fingerprint);
    }
    assert!(conn.execute("DELETE FROM bot_admissions", []).is_err());
    assert!(conn.execute("DELETE FROM run_attempts", []).is_err());
}

#[test]
fn runs_seven_reader_refuses_eight_before_preflight_or_durability_changes() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE kept(value); INSERT INTO kept VALUES(7); PRAGMA user_version=8; PRAGMA foreign_keys=OFF; PRAGMA synchronous=OFF;").unwrap();
    let called = std::cell::Cell::new(false);
    let old = koru_sqlite::Schema {
        database: crate::DATABASE_NAME,
        understood: 7,
        migrations: &crate::db::MIGRATIONS[..7],
    };
    assert!(matches!(
        koru_sqlite::prepare_with_preflight(&old, &mut conn, |_| {
            called.set(true);
            Ok(())
        }),
        Err(koru_sqlite::OpenError::SchemaTooNew {
            discovered: 8,
            supported: 7,
            ..
        })
    ));
    assert!(!called.get());
    assert_eq!(
        conn.query_row("PRAGMA synchronous", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT value FROM kept", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
}

#[test]
fn runs_eight_migration_failure_rolls_back_alters_and_stamp_with_all_rows_intact() {
    let mut h = Harness::new();
    h.prepare();
    h.lease();
    let mut conn = seven(&h);
    // Deliberate conflict after both ALTER statements tests the actual runner's
    // whole-step transaction, not a hand-written substitute migration.
    conn.execute_batch("CREATE TABLE bot_memory_edit_allocations(sentinel TEXT); INSERT INTO bot_memory_edit_allocations VALUES('kept');").unwrap();
    let before = snapshot(&conn);
    assert!(koru_sqlite::prepare(&schema(), &mut conn).is_err());
    assert_eq!(snapshot(&conn), before);
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
    assert_fk(&conn);
    assert!(!columns(&conn, "native_result_intents").contains(&"effect_purpose".into()));
}
