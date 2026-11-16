// SPDX-License-Identifier: MPL-2.0

//! Cohorts `m6_runs12_migration`, `m6_child_rows` and `m6_version_id_column`
//! (its schema half): runs.db 11 to 12 through the real runner, on in-memory
//! databases built by the real migration order. No file, Profile or Store is
//! opened.
use super::*;

fn eleven_schema() -> Schema {
    Schema {
        database: DATABASE_NAME,
        understood: 11,
        migrations: &MIGRATIONS[..11],
    }
}
const WAIT_TRIGGERS_UNCHANGED: [&str; 4] = [
    "bot_wait_runtime_immutable",
    "bot_wait_runtime_retain",
    "bot_wait_enrollment_request_immutable",
    "bot_wait_release_evidence_immutable",
];
const PUBLICATION_TRIGGERS: [&str; 2] = [
    "bot_decision_publications_immutable",
    "bot_decision_publications_retain",
];
const INDEXES_UNCHANGED: [&str; 5] = [
    "bot_wait_runtime_open_run",
    "bot_wait_runtime_recovery",
    "bot_decision_publications_pending_source",
    "bot_decision_publications_pending_control",
    "bot_decision_publications_prepared",
];

/// One more executing Bot Run with two attempts, so each open wait phase can
/// have a Run of its own (a Run has at most one open wait).
fn add_run(c: &Connection, n: u32) {
    c.execute("INSERT INTO run_intents(run_id,workspace_id,payload_json,state,run_document_id,created_at_ms,updated_at_ms) VALUES(?1,?2,'{}','satisfied',?3,-10,-9)", [id(n), id(2), id(n + 1)]).unwrap();
    c.execute("INSERT INTO bot_admissions(run_id,queue_sequence,user_scope,device_id,bot_id,request_id,config_revision,policy_revision,payload_json,state,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,1,1,'{}','executing',-10,-9)", rusqlite::params![id(n), i64::from(n), id(4), id(5), id(n + 2), id(n + 3)]).unwrap();
    for generation in [1_i64, 2] {
        c.execute("INSERT INTO run_attempts(run_id,generation,attempt_id,state,retired,updated_at_ms) VALUES(?1,?2,?3,'running',0,-9)", rusqlite::params![id(n), generation, id(n + 3 + generation as u32)]).unwrap();
    }
}
fn wait_of(run: u32, wait_id: u32, reason: &str, phase: &str) -> Row {
    let mut row = wait();
    row.remove("decision_id");
    row.insert("wait_id", text(id(wait_id)));
    row.insert("run_id", text(id(run)));
    row.insert("reason", text(reason));
    row.insert("phase", text(phase));
    if run != 1 {
        row.insert("attempt_id", text(id(run + 4)));
    }
    if matches!(phase, "held" | "retired") {
        row.insert(
            "release_evidence_json",
            text(r#"{ "kept": "byte for byte" }"#),
        );
    }
    if phase == "settled" {
        row.insert("settled_at_ms", integer(-50));
    }
    row
}
fn publication(sequence: i64, wait_id: u32, kind: &str, state: &str) -> Row {
    let mut row = Row::from([
        ("transition_id", text(id(500 + sequence as u32))),
        ("run_id", text(id(1))),
        ("wait_id", text(id(wait_id))),
        ("run_sequence", integer(sequence)),
        ("workspace_id", text(id(2))),
        ("document_id", text(id(3))),
        ("source_schema", integer(if sequence == 1 { 3 } else { 5 })),
        ("subject_digest", blob(32)),
        ("projection_kind", text(kind)),
        ("target_json", text(r#"{ "spacing": "is kept" }"#)),
        ("target_digest", blob(32)),
        ("state", text(state)),
        ("created_at_ms", integer(-100)),
    ]);
    if matches!(state, "prepared" | "applied") {
        row.extend([
            ("base_version", Value::Blob(vec![1])),
            ("result_version", Value::Blob(vec![2])),
            ("prepared_delta", Value::Blob(vec![3, 4])),
            ("delta_digest", blob(32)),
        ]);
    }
    if state == "applied" {
        row.extend([
            ("observed_version", Value::Blob(vec![2])),
            ("completed_at_ms", integer(-60)),
        ]);
    }
    row
}

/// A populated runs.db at schema 11: every wait reason, every wait phase,
/// both projection kinds and every publication state, a superseded
/// publication included.
fn populated_eleven() -> Connection {
    let mut c = populated_nine();
    koru_sqlite::prepare(&eleven_schema(), &mut c).unwrap();
    assert_eq!(user_version(&c).unwrap(), 11);
    // The one decision of Run 1, and its open wait with a frozen request.
    insert(
        &c,
        "bot_decisions",
        &decision("koru_edit", "pending", "holding"),
    )
    .unwrap();
    let mut holding = wait();
    holding.insert("phase", text("hold_requested"));
    holding.insert("ai_enrollment_request_json", text(r#"{ "frozen": 1 }"#));
    holding.insert("ai_cleanup_receipt_json", text(r#"{"receipt":true}"#));
    insert(&c, "bot_wait_runtime", &holding).unwrap();
    // Settled waits of Run 1, one per no-decision reason.
    for (n, reason) in [
        (41, "capacity"),
        (42, "native_expired"),
        (43, "overlapping_prompt"),
    ] {
        insert(&c, "bot_wait_runtime", &wait_of(1, n, reason, "settled")).unwrap();
    }
    // One Run per remaining open phase.
    for (index, phase) in [
        "fenced",
        "held",
        "retired",
        "resume_reserved",
        "cleanup_pending",
    ]
    .into_iter()
    .enumerate()
    {
        let run = 100 + 10 * index as u32;
        add_run(&c, run);
        let mut row = wait_of(run, 44 + index as u32, "capacity", phase);
        row.insert("observed_sequence", integer(7));
        if phase == "cleanup_pending" {
            row.insert("cleanup_evidence_json", text(r#"{"cleanup":"kept"}"#));
        }
        insert(&c, "bot_wait_runtime", &row).unwrap();
    }
    // Publications of Run 1: an applied Source, a superseded Control, the
    // prepared Control that superseded it, and a pending Control.
    insert(
        &c,
        "bot_decision_publications",
        &publication(1, 41, "source", "applied"),
    )
    .unwrap();
    let mut superseded = publication(2, 41, "control", "superseded");
    superseded.extend([
        ("superseded_by_sequence", integer(3)),
        ("completed_at_ms", integer(-55)),
    ]);
    c.execute_batch("BEGIN").unwrap();
    insert(&c, "bot_decision_publications", &superseded).unwrap();
    insert(
        &c,
        "bot_decision_publications",
        &publication(3, 41, "control", "prepared"),
    )
    .unwrap();
    c.execute_batch("COMMIT").unwrap();
    let mut pending = publication(4, 40, "control", "pending");
    pending.extend([
        ("decision_id", text(id(20))),
        ("decision_revision", integer(1)),
    ]);
    insert(&c, "bot_decision_publications", &pending).unwrap();
    assert_fk(&c);
    c
}
fn to_twelve(c: &mut Connection) -> Result<(), OpenError> {
    koru_sqlite::migrate(DATABASE_NAME, 12, &MIGRATIONS[11..], c).map(|_| ())
}

#[test]
fn m6_runs12_migration_fresh_database_reaches_12() {
    let c = koru_sqlite::open_in_memory(&SCHEMA).unwrap();
    assert_eq!(RUNS_SCHEMA_VERSION, 12);
    assert_eq!(user_version(&c).unwrap(), 12);
    assert_eq!(MIGRATIONS.len(), 12);
    assert_eq!(MIGRATIONS[11].to, 12);
    assert_eq!(MIGRATIONS[11].name, "031_runs_bot_children.sql");
    let s = snapshot(&c);
    for name in [
        "bot_child_requests",
        "bot_child_resumes",
        "bot_wait_runtime_child_open",
        "bot_wait_runtime_child_requests_guard",
        "bot_child_requests_open_parent",
        "bot_child_requests_wait",
        "bot_child_requests_history",
        "bot_child_requests_limit",
        "bot_child_requests_depth",
        "bot_child_requests_guard",
        "bot_child_requests_retain",
        "bot_child_successor_generation",
        "bot_child_consumer_turn",
        "bot_child_resumes_immutable",
        "bot_child_resumes_retain",
    ] {
        assert!(s.contains_key(name), "missing {name}");
    }
    assert_fk(&c);
    // The production opener reaches the same schema.
    let mut again = Connection::open_in_memory().unwrap();
    crate::db::prepare(&mut again).unwrap();
    assert_eq!(user_version(&again).unwrap(), 12);
    assert_eq!(
        snapshot(&again).keys().collect::<Vec<_>>(),
        s.keys().collect::<Vec<_>>()
    );
}

#[test]
fn m6_runs12_migration_populated_11_keeps_every_row_byte_for_byte() {
    let mut c = populated_eleven();
    let before = snapshot(&c);
    assert_eq!(before["bot_wait_runtime"].1.len(), 9);
    assert_eq!(before["bot_decision_publications"].1.len(), 4);
    to_twelve(&mut c).unwrap();
    println!("Rust-linked SQLite {}", rusqlite::version());
    assert_eq!(user_version(&c).unwrap(), 12);
    let after = snapshot(&c);
    // Every row of both rebuilt tables is equal, in rowid order, including
    // the exact bytes of every JSON and blob cell.
    for table in ["bot_wait_runtime", "bot_decision_publications"] {
        assert_eq!(after[table].1, before[table].1, "rows of {table}");
    }
    // Every other table's rows, and every other object's stored SQL, are
    // byte-identical. `native_result_intents` gains one null column.
    let rebuilt = [
        "bot_wait_runtime",
        "bot_decision_publications",
        "bot_wait_runtime_phase_guard",
        "native_result_intents",
    ];
    for (name, value) in &before {
        if rebuilt.contains(&name.as_str()) {
            continue;
        }
        assert_eq!(&after[name], value, "changed object or rows: {name}");
    }
    let widened: Vec<Vec<Value>> = before["native_result_intents"]
        .1
        .iter()
        .cloned()
        .map(|mut row| {
            row.push(Value::Null);
            row
        })
        .collect();
    assert_eq!(after["native_result_intents"].1, widened);
    // The six unchanged triggers and five unchanged indexes carry 029's text.
    for name in WAIT_TRIGGERS_UNCHANGED
        .into_iter()
        .chain(PUBLICATION_TRIGGERS)
        .chain(INDEXES_UNCHANGED)
    {
        assert_eq!(after[name].0, before[name].0, "{name}");
    }
    // The wait table differs from 029's only in the marked lines.
    let wait_sql = &after["bot_wait_runtime"].0;
    assert!(wait_sql.contains(
        "reason IN ('decision','capacity','native_expired','overlapping_prompt','child')"
    ));
    assert!(wait_sql.contains("CHECK(reason<>'child' OR phase NOT IN ('fenced','held'))"));
    assert!(wait_sql.contains("CHECK(reason<>'child' OR ai_enrollment_request_json IS NOT NULL)"));
    assert!(
        after["bot_decision_publications"]
            .0
            .contains("source_schema IN (3,4,5,6)")
    );
    assert_eq!(
        after["bot_decision_publications"].0.replace(
            "source_schema IN (3,4,5,6)),  -- M6",
            "source_schema IN (3,4,5)),"
        ),
        before["bot_decision_publications"].0
    );
    // The widened guard, the new trigger and the new index.
    let guard = &after["bot_wait_runtime_phase_guard"].0;
    assert!(
        guard.contains("SELECT 1 FROM bot_decision_resumes r WHERE r.decision_id=NEW.decision_id")
    );
    assert!(guard.contains("SELECT 1 FROM bot_child_resumes c WHERE c.wait_id=NEW.wait_id"));
    assert!(
        after["bot_wait_runtime_child_requests_guard"]
            .0
            .contains("RAISE(ABORT,'open child request')")
    );
    assert!(
        after["bot_wait_runtime_child_open"]
            .0
            .ends_with("WHERE reason='child' AND phase<>'settled'")
    );
    let triggers: i64 = c.query_row("SELECT count(*) FROM sqlite_master WHERE type='trigger' AND tbl_name IN ('bot_wait_runtime','bot_decision_publications')", [], |r| r.get(0)).unwrap();
    let indexes: i64 = c.query_row("SELECT count(*) FROM sqlite_master WHERE type='index' AND sql IS NOT NULL AND tbl_name IN ('bot_wait_runtime','bot_decision_publications')", [], |r| r.get(0)).unwrap();
    assert_eq!((triggers, indexes), (8, 6));
    assert_fk(&c);
    for temporary in [
        "m6_wait_hold",
        "m6_publication_hold",
        "m6_rows_guard",
        "m6_fk_guard",
    ] {
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM sqlite_temp_master WHERE name=?1",
                [temporary],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    // A second open runs no step and changes nothing.
    crate::db::prepare(&mut c).unwrap();
    assert_eq!(snapshot(&c), after);
}

#[test]
fn m6_runs12_migration_rebuilt_tables_keep_their_keys_and_guards() {
    let mut c = populated_eleven();
    to_twelve(&mut c).unwrap();
    // A publication still requires its wait, found under the real name.
    check(&c, "publication without its wait", false, || {
        insert(
            &c,
            "bot_decision_publications",
            &publication(9, 99, "source", "pending"),
        )
        .map(|_| ())
    });
    check(&c, "publication of a retained wait", true, || {
        insert(
            &c,
            "bot_decision_publications",
            &publication(9, 41, "source", "pending"),
        )
        .map(|_| ())
    });
    // A wait is still retained, and its identity still immutable.
    check(&c, "wait delete", false, || {
        c.execute("DELETE FROM bot_wait_runtime WHERE wait_id=?1", [id(41)])
            .map(|_| ())
    });
    check(&c, "publication delete", false, || {
        c.execute(
            "DELETE FROM bot_decision_publications WHERE run_sequence=1",
            [],
        )
        .map(|_| ())
    });
    check(&c, "wait identity", false, || {
        c.execute(
            "UPDATE bot_wait_runtime SET turn_id=?2 WHERE wait_id=?1",
            [id(40), id(99)],
        )
        .map(|_| ())
    });
    check(&c, "frozen request", false, || {
        c.execute(
            "UPDATE bot_wait_runtime SET ai_enrollment_request_json='{}' WHERE wait_id=?1",
            [id(40)],
        )
        .map(|_| ())
    });
    // The phase guard still orders phases, and a decision wait still moves.
    check(&c, "phase backwards", false, || {
        c.execute(
            "UPDATE bot_wait_runtime SET phase='fenced' WHERE wait_id=?1",
            [id(40)],
        )
        .map(|_| ())
    });
    check(&c, "hold to cleanup", true, || {
        c.execute(
            "UPDATE bot_wait_runtime SET phase='cleanup_pending' WHERE wait_id=?1",
            [id(40)],
        )
        .map(|_| ())
    });
    // Source schema 6 is admitted, 7 is not.
    let mut six = publication(9, 41, "source", "pending");
    six.insert("source_schema", integer(6));
    check(&c, "source schema 6", true, || {
        insert(&c, "bot_decision_publications", &six).map(|_| ())
    });
    six.insert("source_schema", integer(7));
    check(&c, "source schema 7", false, || {
        insert(&c, "bot_decision_publications", &six).map(|_| ())
    });
}

#[test]
fn m6_runs12_migration_orphan_row_fails_the_closing_guard_and_changes_nothing() {
    // An orphan in a table this migration does not touch, written with
    // foreign keys off: a resume that names no decision. Every step runs,
    // and the closing assertion (H) refuses the result.
    let mut c = populated_eleven();
    c.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    let mut orphan = resume(true);
    orphan.insert("decision_id", text(id(999)));
    insert(&c, "bot_decision_resumes", &orphan).unwrap();
    c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    let before = snapshot(&c);
    let error = to_twelve(&mut c).unwrap_err();
    assert!(error.to_string().contains("violations=0"), "{error}");
    assert_eq!(user_version(&c).unwrap(), 11);
    assert_eq!(snapshot(&c), before);
    assert!(!before.contains_key("bot_child_requests"));
}

#[test]
fn m6_runs12_migration_orphan_in_a_rebuilt_table_fails_the_refill_and_changes_nothing() {
    // An orphan in a rebuilt table: a wait that names no Run. With foreign
    // keys on, putting the row back (C) is refused at once, before H.
    let mut c = populated_eleven();
    c.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    insert(
        &c,
        "bot_wait_runtime",
        &wait_of(900, 60, "capacity", "settled"),
    )
    .unwrap();
    c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    let before = snapshot(&c);
    assert_eq!(before["bot_wait_runtime"].1.len(), 10);
    let error = to_twelve(&mut c).unwrap_err();
    assert!(
        error.to_string().contains("FOREIGN KEY") || error.to_string().contains("violations=0"),
        "{error}"
    );
    assert_eq!(user_version(&c).unwrap(), 11);
    // The orphan itself is still there: nothing was dropped.
    assert_eq!(snapshot(&c), before);
}

#[test]
fn m6_runs12_migration_existing_object_fails_and_changes_nothing() {
    let mut c = populated_eleven();
    c.execute_batch("CREATE TABLE bot_child_requests (squatter TEXT)")
        .unwrap();
    let before = snapshot(&c);
    let error = to_twelve(&mut c).unwrap_err();
    assert!(error.to_string().contains("bot_child_requests"), "{error}");
    assert_eq!(user_version(&c).unwrap(), 11);
    assert_eq!(snapshot(&c), before);
    // Both rebuilt tables are back under their names with every row.
    assert_eq!(before["bot_wait_runtime"].1.len(), 9);
    assert_fk(&c);
}

#[test]
fn m6_runs12_migration_a_schema_11_build_refuses_12_and_12_refuses_a_newer_file() {
    let mut c = populated_eleven();
    to_twelve(&mut c).unwrap();
    let before = snapshot(&c);
    assert!(matches!(
        koru_sqlite::prepare(&eleven_schema(), &mut c),
        Err(OpenError::SchemaTooNew {
            discovered: 12,
            supported: 11,
            ..
        })
    ));
    assert_eq!(snapshot(&c), before);
    c.pragma_update(None, "user_version", 13).unwrap();
    assert!(matches!(
        crate::db::prepare(&mut c),
        Err(OpenError::SchemaTooNew {
            discovered: 13,
            supported: 12,
            ..
        })
    ));
}

#[test]
fn m6_runs12_migration_empty_11_database_migrates() {
    let mut c = koru_sqlite::open_in_memory(&eleven_schema()).unwrap();
    to_twelve(&mut c).unwrap();
    assert_eq!(user_version(&c).unwrap(), 12);
    assert_fk(&c);
}

// ---- m6_child_rows -----------------------------------------------------------

/// A migrated database with a waiting parent Run (1) and three more Runs.
fn twelve() -> Connection {
    let mut c = populated_nine();
    koru_sqlite::prepare(&SCHEMA, &mut c).unwrap();
    for run in [100, 110, 120, 130, 140] {
        add_run(&c, run);
    }
    c
}
fn child_wait(run: u32, wait_id: u32, phase: &str) -> Row {
    let mut row = wait_of(run, wait_id, "child", phase);
    row.insert("ai_enrollment_request_json", text(r#"{"frozen":true}"#));
    row
}
fn request(parent: u32, wait_id: u32, ordinal: i64, child: u32) -> Row {
    Row::from([
        ("child_request_id", text(id(700 + child))),
        ("parent_run_id", text(id(parent))),
        ("wait_id", text(id(wait_id))),
        ("ask_key", text(id(600 + wait_id))),
        ("ordinal", integer(ordinal)),
        ("user_scope", text(id(4))),
        ("device_id", text(id(5))),
        ("parent_bot_id", text(id(6))),
        ("recipient_bot_id", text(id(child + 2))),
        ("human_profile_id", text(id(15))),
        ("child_run_id", text(id(child))),
        ("binding_version", integer(1)),
        ("binding_digest", blob(32)),
        ("binding_json", text("{}")),
        ("state", text("open")),
        ("wait_elapsed_ms", integer(0)),
        ("clock", text("running")),
        ("clock_boot_id", text(id(16))),
        ("clock_since_ms", integer(-100)),
        ("revision", integer(1)),
        ("created_at_ms", integer(-100)),
        ("updated_at_ms", integer(-100)),
    ])
}

#[test]
fn m6_child_rows_wait_checks_and_the_open_request_guard() {
    let c = twelve();
    // A child wait is born hold_requested with its frozen intent.
    for (phase, accepts) in [
        ("hold_requested", true),
        ("resume_reserved", true),
        ("cleanup_pending", true),
        ("fenced", false),
        ("held", false),
    ] {
        check(&c, &format!("child wait {phase}"), accepts, || {
            insert(&c, "bot_wait_runtime", &child_wait(1, 40, phase)).map(|_| ())
        });
    }
    let mut unfrozen = child_wait(1, 40, "hold_requested");
    unfrozen.remove("ai_enrollment_request_json");
    check(&c, "child wait without its intent", false, || {
        insert(&c, "bot_wait_runtime", &unfrozen).map(|_| ())
    });
    let mut decided = child_wait(1, 40, "hold_requested");
    decided.insert("decision_id", text(id(20)));
    check(&c, "child wait with a decision", false, || {
        insert(&c, "bot_wait_runtime", &decided).map(|_| ())
    });
    // A wait cannot leave `retired` (or the hold) while a request is open.
    insert(&c, "bot_wait_runtime", &child_wait(1, 40, "retired")).unwrap();
    insert(&c, "bot_child_requests", &request(1, 40, 0, 100)).unwrap();
    for phase in ["resume_reserved", "cleanup_pending"] {
        check(
            &c,
            &format!("leave retired for {phase} with an open request"),
            false,
            || {
                c.execute(
                    "UPDATE bot_wait_runtime SET phase=?2 WHERE wait_id=?1",
                    [id(40), phase.into()],
                )
                .map(|_| ())
            },
        );
    }
    check(&c, "other columns still move", true, || {
        c.execute(
            "UPDATE bot_wait_runtime SET revision=2 WHERE wait_id=?1",
            [id(40)],
        )
        .map(|_| ())
    });
    c.execute("UPDATE bot_child_requests SET state='settled',settled_by='timed_out',settled_at_ms=1,clock='stopped',clock_boot_id=NULL,clock_since_ms=NULL,revision=2 WHERE wait_id=?1", [id(40)]).unwrap();
    check(
        &c,
        "leave retired once every request is settled",
        true,
        || {
            c.execute(
                "UPDATE bot_wait_runtime SET phase='resume_reserved' WHERE wait_id=?1",
                [id(40)],
            )
            .map(|_| ())
        },
    );
    // A wait with a resume row cannot be held or retired again.
    insert(
        &c,
        "bot_wait_runtime",
        &child_wait(100, 41, "hold_requested"),
    )
    .unwrap();
    let mut resume = child_resume(100, 41);
    resume.insert("origin_attempt_id", text(id(104)));
    resume.insert("consumer_attempt_id", text(id(105)));
    insert(&c, "bot_child_resumes", &resume).unwrap();
    check(&c, "retire a wait that has a resume row", false, || {
        c.execute("UPDATE bot_wait_runtime SET phase='retired',release_evidence_json='{}' WHERE wait_id=?1", [id(41)]).map(|_| ())
    });
}

fn child_resume(run: u32, wait_id: u32) -> Row {
    Row::from([
        ("wait_id", text(id(wait_id))),
        ("run_id", text(id(run))),
        ("origin_attempt_id", text(id(11))),
        ("origin_generation", integer(1)),
        ("origin_session_id", text(id(12))),
        ("consumer_attempt_id", text(id(12))),
        ("consumer_generation", integer(2)),
        ("session_creation_key", text(id(31))),
        ("input_key", text(id(32 + wait_id))),
        ("input_fingerprint", blob(32)),
        ("state", text("prepared")),
        ("created_at_ms", integer(-90)),
        ("updated_at_ms", integer(-90)),
    ])
}

#[test]
fn m6_child_rows_request_checks_limits_and_immutability() {
    let c = twelve();
    insert(&c, "bot_wait_runtime", &child_wait(1, 40, "hold_requested")).unwrap();
    let base = request(1, 40, 0, 100);
    for (column, value, accepts) in [
        ("ordinal", integer(3), false),
        ("ordinal", integer(2), true),
        ("binding_version", integer(2), false),
        ("binding_digest", blob(31), false),
        ("binding_json", text("not json"), false),
        ("state", text("queued"), false),
        ("clock", text("stopped"), false),
        ("clock", text("paused"), false),
        ("clock_boot_id", Value::Null, false),
        ("clock_since_ms", Value::Null, false),
        ("wait_elapsed_ms", integer(-1), false),
        ("settled_by", text("child_ended"), false),
        ("recipient_bot_id", text(id(6)), false),
        ("child_run_id", text(id(1)), false),
        ("child_run_id", text(id(999)), false),
        ("parent_run_id", text(id(999)), false),
        ("wait_id", text(id(41)), false),
        ("revision", integer(0), false),
    ] {
        row_case(&c, "bot_child_requests", &base, column, value, accepts);
    }
    // A paused open request has no running interval; a settled one is stopped.
    let mut paused = base.clone();
    paused.extend([
        ("clock", text("paused")),
        ("clock_boot_id", Value::Null),
        ("clock_since_ms", Value::Null),
    ]);
    check(&c, "paused open request", true, || {
        insert(&c, "bot_child_requests", &paused).map(|_| ())
    });
    let mut settled = paused.clone();
    settled.extend([
        ("state", text("settled")),
        ("settled_by", text("parent_stopped")),
        ("settled_at_ms", integer(1)),
        ("clock", text("stopped")),
    ]);
    check(&c, "settled request", true, || {
        insert(&c, "bot_child_requests", &settled).map(|_| ())
    });
    settled.insert("settled_by", text("gave_up"));
    check(&c, "unknown settler", false, || {
        insert(&c, "bot_child_requests", &settled).map(|_| ())
    });

    // Three requests per parent Run, for good: the trigger refuses a fourth
    // whatever the writer checked, even on another wait and when all settled.
    for (ordinal, child) in [(0, 100), (1, 110), (2, 120)] {
        insert(&c, "bot_child_requests", &request(1, 40, ordinal, child)).unwrap();
    }
    check(&c, "same ask and ordinal twice", false, || {
        insert(&c, "bot_child_requests", &request(1, 40, 0, 130)).map(|_| ())
    });
    c.execute("UPDATE bot_child_requests SET state='settled',settled_by='child_ended',settled_at_ms=1,clock='stopped',clock_boot_id=NULL,clock_since_ms=NULL,revision=2", []).unwrap();
    c.execute(
        "UPDATE bot_wait_runtime SET phase='cleanup_pending' WHERE wait_id=?1",
        [id(40)],
    )
    .unwrap();
    c.execute(
        "UPDATE bot_wait_runtime SET phase='settled',settled_at_ms=1 WHERE wait_id=?1",
        [id(40)],
    )
    .unwrap();
    insert(&c, "bot_wait_runtime", &child_wait(1, 41, "hold_requested")).unwrap();
    let error = insert(&c, "bot_child_requests", &request(1, 41, 0, 130)).unwrap_err();
    assert!(error.to_string().contains("child request limit"), "{error}");

    // A settled row admits no update at all, and no row is ever deleted.
    for statement in [
        "UPDATE bot_child_requests SET revision=revision+1",
        "UPDATE bot_child_requests SET state='open',settled_by=NULL,settled_at_ms=NULL,clock='paused',revision=9",
        "UPDATE bot_child_requests SET updated_at_ms=5,revision=9",
        "DELETE FROM bot_child_requests",
    ] {
        check(&c, statement, false, || {
            c.execute(statement, []).map(|_| ())
        });
    }
}

#[test]
fn m6_child_rows_depth_one_both_ways_and_open_row_transitions() {
    let c = twelve();
    insert(&c, "bot_wait_runtime", &child_wait(1, 40, "hold_requested")).unwrap();
    insert(&c, "bot_child_requests", &request(1, 40, 0, 100)).unwrap();
    // The child Run (100) can never be a parent...
    insert(
        &c,
        "bot_wait_runtime",
        &child_wait(100, 41, "hold_requested"),
    )
    .unwrap();
    let mut nested = request(100, 41, 0, 110);
    nested.insert("parent_bot_id", text(id(102)));
    let error = insert(&c, "bot_child_requests", &nested).unwrap_err();
    assert!(error.to_string().contains("child request depth"), "{error}");
    // ...and a parent Run (1) can never be a child.
    insert(
        &c,
        "bot_wait_runtime",
        &child_wait(110, 42, "hold_requested"),
    )
    .unwrap();
    let mut inverted = request(110, 42, 0, 1);
    inverted.insert("parent_bot_id", text(id(112)));
    inverted.insert("recipient_bot_id", text(id(6)));
    let error = insert(&c, "bot_child_requests", &inverted).unwrap_err();
    assert!(error.to_string().contains("child request depth"), "{error}");
    // One Run is the child of at most one request.
    check(&c, "one child Run, two requests", false, || {
        insert(&c, "bot_child_requests", &request(1, 40, 1, 100)).map(|_| ())
    });

    // An open row: identity is immutable, the revision must rise, and the
    // counted time never goes down.
    for (statement, accepts) in [
        (
            "UPDATE bot_child_requests SET wait_elapsed_ms=10,revision=2",
            true,
        ),
        ("UPDATE bot_child_requests SET wait_elapsed_ms=10", false),
        (
            "UPDATE bot_child_requests SET recipient_bot_id='00000000-0000-4000-8000-000000000999',revision=2",
            false,
        ),
        (
            "UPDATE bot_child_requests SET binding_json='{\"other\":1}',revision=2",
            false,
        ),
        (
            "UPDATE bot_child_requests SET ask_key='00000000-0000-4000-8000-000000000999',revision=2",
            false,
        ),
        (
            "UPDATE bot_child_requests SET clock='paused',clock_boot_id=NULL,clock_since_ms=NULL,revision=2",
            true,
        ),
    ] {
        check(&c, statement, accepts, || {
            c.execute(statement, []).map(|_| ())
        });
    }
    c.execute(
        "UPDATE bot_child_requests SET wait_elapsed_ms=10,revision=2",
        [],
    )
    .unwrap();
    check(&c, "time going down", false, || {
        c.execute(
            "UPDATE bot_child_requests SET wait_elapsed_ms=9,revision=3",
            [],
        )
        .map(|_| ())
    });
    // The open-edge index serves the cycle walk.
    let plan: Vec<String> = c
        .prepare("EXPLAIN QUERY PLAN SELECT DISTINCT recipient_bot_id FROM bot_child_requests INDEXED BY bot_child_requests_open_parent WHERE device_id=?1 AND parent_bot_id=?2 AND state='open'")
        .unwrap()
        .query_map([id(5), id(6)], |r| r.get(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|line| line.contains("bot_child_requests_open_parent")),
        "{plan:?}"
    );
}

#[test]
fn m6_child_rows_resume_checks_and_immutability() {
    let c = twelve();
    insert(&c, "bot_wait_runtime", &child_wait(1, 40, "hold_requested")).unwrap();
    let base = child_resume(1, 40);
    for (column, value, accepts) in [
        ("consumer_generation", integer(3), false),
        ("consumer_attempt_id", text(id(11)), false),
        ("state", text("session_bound"), false),
        ("state", text("held"), false),
        ("wait_id", text(id(99)), false),
        ("accepted_turn_id", text(id(77)), false),
        ("input_fingerprint", blob(31), false),
    ] {
        row_case(&c, "bot_child_resumes", &base, column, value, accepts);
    }
    insert(&c, "bot_child_resumes", &base).unwrap();
    check(&c, "one resume per wait", false, || {
        insert(&c, "bot_child_resumes", &base).map(|_| ())
    });
    for (statement, accepts) in [
        (
            "UPDATE bot_child_resumes SET consumer_session_id='00000000-0000-4000-8000-000000000021',state='session_bound'",
            true,
        ),
        (
            "UPDATE bot_child_resumes SET input_key='00000000-0000-4000-8000-000000000999'",
            false,
        ),
        ("UPDATE bot_child_resumes SET consumer_generation=3", false),
        ("DELETE FROM bot_child_resumes", false),
    ] {
        check(&c, statement, accepts, || {
            c.execute(statement, []).map(|_| ())
        });
    }
    c.execute("UPDATE bot_child_resumes SET consumer_session_id=?1,state='input_acknowledged',accepted_turn_id=?2,receipt_json='{}'", [id(21), id(22)]).unwrap();
    for statement in [
        "UPDATE bot_child_resumes SET accepted_turn_id='00000000-0000-4000-8000-000000000023'",
        "UPDATE bot_child_resumes SET consumer_session_id='00000000-0000-4000-8000-000000000024'",
    ] {
        check(&c, statement, false, || {
            c.execute(statement, []).map(|_| ())
        });
    }
    c.execute("UPDATE bot_child_resumes SET state='closed'", [])
        .unwrap();
    check(&c, "a closed resume stays closed", false, || {
        c.execute(
            "UPDATE bot_child_resumes SET state='input_acknowledged'",
            [],
        )
        .map(|_| ())
    });
}

// ---- m6_version_id_column (schema half) ---------------------------------------

#[test]
fn m6_version_id_column_is_null_on_old_rows_and_bound_to_a_verified_stage() {
    let mut c = populated_eleven();
    to_twelve(&mut c).unwrap();
    let stored: Option<String> = c
        .query_row(
            "SELECT verified_version_id FROM native_result_intents",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, None, "a row written before schema 12 reads as null");
    // The retained row is `prepared`: it cannot carry a version id, of any
    // shape. Only a verified or registered result may (the ledger's own
    // writer is exercised by the behaviour test beside the child cohorts).
    for statement in [
        "UPDATE native_result_intents SET verified_version_id='00000000-0000-4000-8000-000000000077'",
        "UPDATE native_result_intents SET verified_version_id='short'",
        "UPDATE native_result_intents SET verified_version_id=x'00'",
    ] {
        check(&c, statement, false, || {
            c.execute(statement, []).map(|_| ())
        });
    }
    let table: String = c
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='native_result_intents'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(table.contains("verified_version_id TEXT"));
    assert!(table.contains("length(verified_version_id)=36"));
    assert!(table.contains("stage IN ('verified','registered')"));
}
