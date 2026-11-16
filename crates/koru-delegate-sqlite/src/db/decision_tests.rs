// SPDX-License-Identifier: MPL-2.0
//! Linked SQLite migration evidence only; no decision execution owner is enabled.
use super::*;
use rusqlite::{params_from_iter, types::Value};
use std::collections::BTreeMap;

mod children;
mod constraints;
mod private;
mod publication;

type Row = BTreeMap<&'static str, Value>;
type Snapshot = BTreeMap<String, (String, Vec<Vec<Value>>)>;

fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}
fn text(s: impl Into<String>) -> Value {
    Value::Text(s.into())
}
fn integer(n: i64) -> Value {
    Value::Integer(n)
}
fn blob(n: usize) -> Value {
    Value::Blob(vec![0; n])
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
fn snapshot(c: &Connection) -> Snapshot {
    let objects: Vec<(String, String, String)> = c.prepare("SELECT type,name,coalesce(sql,'') FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name").unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().collect::<Result<_, _>>().unwrap();
    objects
        .into_iter()
        .map(|(kind, name, sql)| {
            let mut rows = Vec::new();
            if kind == "table" {
                let mut query = c.prepare(&format!("SELECT * FROM \"{name}\"")).unwrap();
                let count = query.column_count();
                rows = query
                    .query_map([], |r| {
                        (0..count)
                            .map(|i| r.get(i))
                            .collect::<rusqlite::Result<Vec<Value>>>()
                    })
                    .unwrap()
                    .collect::<Result<_, _>>()
                    .unwrap();
                // Rowid order is retained, including exact TEXT/BLOB byte encodings.
            }
            (name, (sql, rows))
        })
        .collect()
}
fn old_schema() -> Schema {
    Schema {
        database: DATABASE_NAME,
        understood: 9,
        migrations: &MIGRATIONS[..9],
    }
}
fn populated_nine() -> Connection {
    let c = koru_sqlite::open_in_memory(&old_schema()).unwrap();
    c.execute("INSERT INTO run_intents(run_id,workspace_id,payload_json,state,run_document_id,created_at_ms,updated_at_ms) VALUES(?1,?2,'{ \"retained\": true }','satisfied',?3,-10,-9)", [id(1),id(2),id(3)]).unwrap();
    c.execute("INSERT INTO bot_admissions(run_id,queue_sequence,user_scope,device_id,bot_id,request_id,config_revision,policy_revision,payload_json,state,created_at_ms,updated_at_ms) VALUES(?1,1,?2,?3,?4,?5,1,1,'{ \"old\": true }','executing',-10,-9)",[id(1),id(4),id(5),id(6),id(7)]).unwrap();
    for generation in [1_i64, 2] {
        c.execute("INSERT INTO run_attempts(run_id,generation,attempt_id,state,retired,updated_at_ms) VALUES(?1,?2,?3,'running',0,-9)", rusqlite::params![id(1),generation,id(10+generation as u32)]).unwrap();
    }
    c.execute("INSERT INTO pending_publications(event_id,run_id,kind,body_json,attempts,queued_at_ms) VALUES(?1,?2,'usage','{ \"opaque\": 42 }',3,-8)",[id(8),id(1)]).unwrap();
    c.execute("INSERT INTO native_result_intents(run_id,operation_id,attempt_id,generation,turn_id,kind,request_fingerprint,destination_workspace_id,target_document_id,store_request_id,requested_title,stage,lease_state,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,1,?4,'create_page',zeroblob(32),?5,?6,?7,'Retained','prepared','none',-10,-9)",[id(1),id(14),id(11),id(13),id(2),id(9),id(30)]).unwrap();
    c
}
fn current() -> Connection {
    let mut c = populated_nine();
    koru_sqlite::prepare(&SCHEMA, &mut c).unwrap();
    c
}
fn decision(kind: &str, authorization: &str, phase: &str) -> Row {
    let mut row = Row::from([
        ("decision_id", text(id(20))),
        ("run_id", text(id(1))),
        ("user_scope", text(id(4))),
        ("device_id", text(id(5))),
        ("bot_id", text(id(6))),
        ("origin_attempt_id", text(id(11))),
        ("origin_generation", integer(1)),
        ("origin_session_id", text(id(12))),
        ("origin_turn_id", text(id(13))),
        ("kind", text(kind)),
        ("human_profile_id", text(id(15))),
        ("binding_version", integer(1)),
        ("binding_digest", blob(32)),
        ("binding_json", text("{}")),
        ("created_boot_id", text(id(16))),
        ("created_at_ms", integer(-100)),
        ("deadline_at_ms", integer(599900)),
        ("last_observed_at_ms", integer(-100)),
        ("revision", integer(1)),
        ("authorization", text(authorization)),
        ("phase", text(phase)),
        ("updated_at_ms", integer(-100)),
    ]);
    if kind == "native_approval" {
        row.insert("native_prompt_id", text("fixture-prompt"));
    } else {
        row.insert("operation_id", text(id(14)));
    }
    if matches!(authorization, "granted" | "denied" | "consumed") {
        row.extend([
            ("answer_request_id", text(id(21))),
            ("answer_profile_id", text(id(15))),
            ("answer_choice_json", text("{}")),
            ("answer_at_ms", integer(-90)),
            ("answer_proof_digest", blob(32)),
            ("answer_receipt_json", text("{}")),
        ]);
    }
    if authorization == "consumed" {
        row.insert("consumed_at_ms", integer(-80));
    }
    if phase == "settled" {
        row.extend([
            ("closure_reason", text("completed")),
            ("settled_at_ms", integer(-70)),
        ]);
    }
    row
}
fn wait() -> Row {
    Row::from([
        ("wait_id", text(id(40))),
        ("run_id", text(id(1))),
        ("decision_id", text(id(20))),
        ("user_scope", text(id(4))),
        ("device_id", text(id(5))),
        ("bot_id", text(id(6))),
        ("attempt_id", text(id(11))),
        ("generation", integer(1)),
        ("session_id", text(id(12))),
        ("turn_id", text(id(13))),
        ("reason", text("decision")),
        ("phase", text("fenced")),
        ("revision", integer(1)),
        ("created_at_ms", integer(-100)),
        ("updated_at_ms", integer(-100)),
    ])
}
fn resume(successor: bool) -> Row {
    Row::from([
        ("decision_id", text(id(20))),
        ("run_id", text(id(1))),
        (
            "kind",
            text(if successor {
                "recorded_successor"
            } else {
                "same_session"
            }),
        ),
        ("origin_attempt_id", text(id(11))),
        ("origin_generation", integer(1)),
        ("origin_session_id", text(id(12))),
        (
            "consumer_attempt_id",
            text(id(if successor { 12 } else { 11 })),
        ),
        (
            "consumer_generation",
            integer(if successor { 2 } else { 1 }),
        ),
        ("consumer_session_id", text(id(12))),
        ("session_creation_key", text(id(31))),
        ("input_key", text(id(32))),
        ("input_fingerprint", blob(32)),
        ("state", text("prepared")),
        ("created_at_ms", integer(-90)),
        ("updated_at_ms", integer(-90)),
    ])
}
fn assert_fk(c: &Connection) {
    assert_eq!(
        c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
}

#[test]
fn decision_migration_runs10_preserves_populated_runs9_objects_rows_and_foreign_keys() {
    // Schema 10 exactly: this is the 9 to 10 step's own evidence. Schema 12
    // adds a column to one of these older tables, which has its own cohort.
    let ten = Schema {
        database: DATABASE_NAME,
        understood: 10,
        migrations: &MIGRATIONS[..10],
    };
    let mut c = populated_nine();
    let before = snapshot(&c);
    koru_sqlite::prepare(&ten, &mut c).unwrap();
    println!("Rust-linked SQLite {}", rusqlite::version());
    assert_eq!(user_version(&c).unwrap(), 10);
    let after = snapshot(&c);
    for (name, value) in before {
        assert_eq!(after[&name], value, "changed old object/bytes: {name}");
    }
    for table in [
        "bot_decisions",
        "bot_wait_runtime",
        "bot_decision_resumes",
        "native_decision_leases",
        "bot_decision_native_answers",
        "bot_decision_publications",
    ] {
        assert!(after.contains_key(table), "missing {table}");
        c.prepare(&format!("DELETE FROM {table} WHERE 0")).unwrap();
        let nullable: u32 = c
            .query_row(
                "SELECT count(*) FROM pragma_table_info(?1) WHERE pk<>0 AND \"notnull\"=0",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(nullable, 0, "nullable text primary key: {table}");
    }
    assert_fk(&c);
    koru_sqlite::prepare(&ten, &mut c).unwrap();
    assert_eq!(snapshot(&c), after);
}

#[test]
fn decision_migration_runs10_precommit_fk_failure_rolls_back_schema_rows_and_stamp() {
    let mut c = populated_nine();
    let before = snapshot(&c);
    let sql = format!(
        "PRAGMA defer_foreign_keys=ON; INSERT INTO run_attempts(run_id,generation,attempt_id,state,updated_at_ms) VALUES('missing',1,'bad','created',0);\n{}",
        MIGRATIONS[9].sql
    );
    let steps = [Migration {
        to: 10,
        name: "029_fk_failure_fixture.sql",
        sql: Box::leak(sql.into_boxed_str()),
        columns: &[],
    }];
    let error = koru_sqlite::migrate(DATABASE_NAME, 10, &steps, &mut c).unwrap_err();
    assert!(error.to_string().contains("violations=0"), "{error}");
    assert_eq!(user_version(&c).unwrap(), 9);
    assert_eq!(snapshot(&c), before);
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_temp_master WHERE name='p42_fk_guard'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_fk(&c);
}

#[test]
fn decision_migration_runs9_refuses10_before_preflight_or_configuration() {
    let mut c = current();
    let before = snapshot(&c);
    c.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA synchronous=OFF;")
        .unwrap();
    let called = std::cell::Cell::new(false);
    assert!(matches!(
        koru_sqlite::prepare_with_preflight(&old_schema(), &mut c, |_| {
            called.set(true);
            Ok(())
        }),
        Err(OpenError::SchemaTooNew {
            discovered: RUNS_SCHEMA_VERSION,
            supported: 9,
            ..
        })
    ));
    assert!(!called.get());
    assert_eq!(user_version(&c).unwrap(), RUNS_SCHEMA_VERSION);
    assert_eq!(snapshot(&c), before);
    for pragma in ["PRAGMA foreign_keys", "PRAGMA synchronous"] {
        assert_eq!(c.query_row(pragma, [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
}
