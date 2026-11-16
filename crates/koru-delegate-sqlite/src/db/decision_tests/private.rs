// SPDX-License-Identifier: MPL-2.0

//! Cohort `m5_runs11_migration`: runs.db 10 to 11 on in-memory databases built
//! by the real migration order. No file, Profile or Store is opened.
use super::*;

/// Schema 11 exactly, with the production preflight: what `crate::db::prepare`
/// did when this cohort was written, before schema 12 existed.
fn eleven_schema() -> Schema {
    Schema {
        database: DATABASE_NAME,
        understood: 11,
        migrations: &MIGRATIONS[..11],
    }
}
fn prepare_eleven(connection: &mut Connection) -> Result<(), OpenError> {
    koru_sqlite::prepare_with_preflight(&eleven_schema(), connection, refuse_retained_decisions)
}
fn ten_schema() -> Schema {
    Schema {
        database: DATABASE_NAME,
        understood: 10,
        migrations: &MIGRATIONS[..10],
    }
}
/// A populated runs.db at schema 10 whose `bot_decisions` is empty: the rows
/// of `populated_nine`, plus a wait and a publication that name no decision
/// (which 029 allows and the recreate must not disturb).
fn populated_ten() -> Connection {
    let mut c = populated_nine();
    koru_sqlite::prepare(&ten_schema(), &mut c).unwrap();
    assert_eq!(user_version(&c).unwrap(), 10);
    let mut waiting = wait();
    waiting.remove("decision_id");
    waiting.insert("reason", text("capacity"));
    insert(&c, "bot_wait_runtime", &waiting).unwrap();
    insert(
        &c,
        "bot_decision_publications",
        &Row::from([
            ("transition_id", text(id(50))),
            ("run_id", text(id(1))),
            ("wait_id", text(id(40))),
            ("run_sequence", integer(1)),
            ("workspace_id", text(id(2))),
            ("document_id", text(id(3))),
            ("source_schema", integer(5)),
            ("subject_digest", blob(32)),
            ("projection_kind", text("control")),
            ("target_json", text("{}")),
            ("target_digest", blob(32)),
            ("state", text("pending")),
            ("created_at_ms", integer(-100)),
        ]),
    )
    .unwrap();
    c
}
fn objects_of(snapshot: &Snapshot, names: &[&str]) -> Vec<(String, String)> {
    names
        .iter()
        .map(|name| (name.to_string(), snapshot[*name].0.clone()))
        .collect()
}
const DECISION_INDEXES: [&str; 7] = [
    "bot_decisions_operation",
    "bot_decisions_prompt",
    "bot_decisions_outstanding",
    "bot_decisions_answer",
    "bot_decisions_deadline",
    "bot_decisions_owner_open",
    "bot_decisions_history",
];
const DECISION_TRIGGERS: [&str; 2] = ["bot_decisions_immutable", "bot_decisions_retain"];

#[test]
fn m5_runs11_migration_fresh_database_reaches_11() {
    let c = koru_sqlite::open_in_memory(&eleven_schema()).unwrap();
    assert_eq!(user_version(&c).unwrap(), 11);
    assert_eq!(MIGRATIONS[10].to, 11);
    assert_eq!(MIGRATIONS[10].name, "030_runs_private_operations.sql");
    let s = snapshot(&c);
    for name in ["bot_decisions", "bot_private_effects"]
        .into_iter()
        .chain(DECISION_INDEXES)
        .chain(DECISION_TRIGGERS)
        .chain([
            "bot_private_effects_decision",
            "bot_private_effects_open_run",
            "bot_private_effects_open",
            "bot_private_effects_history",
            "bot_private_effects_guard",
            "bot_private_effects_retain",
        ])
    {
        assert!(s.contains_key(name), "missing {name}");
    }
    assert_fk(&c);
    // Through the production opener too, which runs the preflight.
    let mut again = Connection::open_in_memory().unwrap();
    prepare_eleven(&mut again).unwrap();
    assert_eq!(user_version(&again).unwrap(), 11);
    assert_eq!(
        snapshot(&again).keys().collect::<Vec<_>>(),
        s.keys().collect::<Vec<_>>()
    );
}

#[test]
fn m5_runs11_migration_populated_10_with_no_decision_keeps_every_other_row_and_object() {
    let mut c = populated_ten();
    let before = snapshot(&c);
    prepare_eleven(&mut c).unwrap();
    assert_eq!(user_version(&c).unwrap(), 11);
    let after = snapshot(&c);
    let recreated: Vec<&str> = ["bot_decisions"]
        .into_iter()
        .chain(DECISION_INDEXES)
        .collect();
    for (name, value) in &before {
        if recreated.contains(&name.as_str()) {
            continue;
        }
        // Every other table's rows and every other object's stored SQL are
        // byte-identical, the two recreated triggers included.
        assert_eq!(&after[name], value, "changed object or rows: {name}");
    }
    // The recreated triggers carry 029's exact text.
    assert_eq!(
        objects_of(&after, &DECISION_TRIGGERS),
        objects_of(&before, &DECISION_TRIGGERS)
    );
    // The table is still empty and differs from 029's only in the kind set.
    assert!(after["bot_decisions"].1.is_empty());
    let narrowed = after["bot_decisions"]
        .0
        .replace(
            "kind IN ('koru_edit','native_approval','private_operation','browser_origin')",
            "kind IN ('koru_edit','native_approval')",
        )
        .replace(
            "kind IN ('koru_edit','private_operation','browser_origin') AND operation_id",
            "kind='koru_edit' AND operation_id",
        )
        .replace(
            "kind IN ('koru_edit','private_operation','browser_origin') OR phase",
            "kind='koru_edit' OR phase",
        )
        .replace("  -- M5, O158", "")
        .replace("  -- M5", "");
    assert_eq!(narrowed, before["bot_decisions"].0);
    // Six indexes are byte-identical; the seventh covers every operation key.
    for index in DECISION_INDEXES {
        if index == "bot_decisions_operation" {
            assert!(after[index].0.ends_with("WHERE operation_id IS NOT NULL"));
        } else {
            assert_eq!(after[index].0, before[index].0, "{index}");
        }
    }
    // The decision-less wait and publication survived, and nothing else was
    // added but the one new table and its own objects.
    assert_eq!(after["bot_wait_runtime"].1.len(), 1);
    assert_eq!(after["bot_decision_publications"].1.len(), 1);
    let added: Vec<&String> = after.keys().filter(|k| !before.contains_key(*k)).collect();
    assert_eq!(
        added,
        [
            "bot_private_effects",
            "bot_private_effects_decision",
            "bot_private_effects_guard",
            "bot_private_effects_history",
            "bot_private_effects_open",
            "bot_private_effects_open_run",
            "bot_private_effects_retain",
        ]
    );
    assert_fk(&c);
    for guard in ["m5_decisions_empty", "m5_fk_guard"] {
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM sqlite_temp_master WHERE name=?1",
                [guard],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    // A second open runs no step and changes nothing.
    prepare_eleven(&mut c).unwrap();
    assert_eq!(snapshot(&c), after);
}

#[test]
fn m5_runs11_migration_children_still_require_their_recreated_parent() {
    // A 10 database with no wait at all, migrated by the production opener.
    let c2 = {
        let mut c = populated_nine();
        koru_sqlite::prepare(&ten_schema(), &mut c).unwrap();
        prepare_eleven(&mut c).unwrap();
        c
    };
    let mut w = wait();
    w.insert("phase", text("resume_reserved"));
    // A child row needs its parent...
    check(&c2, "wait without decision row", false, || {
        insert(&c2, "bot_wait_runtime", &w).map(|_| ())
    });
    check(&c2, "resume without decision row", false, || {
        insert(&c2, "bot_decision_resumes", &resume(true)).map(|_| ())
    });
    // ...and finds the recreated table by name once the parent row exists.
    insert(
        &c2,
        "bot_decisions",
        &decision("koru_edit", "granted", "held"),
    )
    .unwrap();
    insert(&c2, "bot_wait_runtime", &w).unwrap();
    insert(&c2, "bot_decision_resumes", &resume(true)).unwrap();
    assert_fk(&c2);
    // The recreated retain and immutable triggers fire.
    let error = c2.execute("DELETE FROM bot_decisions", []).unwrap_err();
    assert!(error.to_string().contains("retained Bot decision"));
    let error = c2
        .execute("UPDATE bot_decisions SET kind='private_operation'", [])
        .unwrap_err();
    assert!(error.to_string().contains("immutable Bot decision"));
}

#[test]
fn m5_runs11_migration_preflight_refuses_a_retained_decision_and_changes_nothing() {
    let mut c = populated_ten();
    insert(
        &c,
        "bot_decisions",
        &decision("koru_edit", "pending", "publishing"),
    )
    .unwrap();
    c.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA synchronous=OFF;")
        .unwrap();
    let before = snapshot(&c);
    let error = prepare_eleven(&mut c).unwrap_err();
    let OpenError::Storage {
        database,
        operation,
        detail,
    } = &error
    else {
        panic!("typed refusal expected: {error}")
    };
    assert_eq!(*database, DATABASE_NAME);
    assert_eq!(*operation, crate::RETAINED_DECISIONS_UPGRADE);
    assert_eq!(detail, crate::RETAINED_DECISIONS_DETAIL);
    assert_eq!(
        error.to_string(),
        "could not upgrade runs.db: runs.db holds Bot decisions written by a development build. This build cannot upgrade it. Nothing was changed."
    );
    // No row, id or temporary table name reaches the person.
    for leaked in [id(20), id(1), "m5_decisions_empty".into(), "CHECK".into()] {
        assert!(!error.to_string().contains(&leaked));
    }
    // Nothing was changed: not a row, not an object, not the stamp, not even
    // a pragma (the preflight runs before configuration).
    assert_eq!(user_version(&c).unwrap(), 10);
    assert_eq!(snapshot(&c), before);
    for pragma in ["PRAGMA foreign_keys", "PRAGMA synchronous"] {
        assert_eq!(c.query_row(pragma, [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
    // The build that wrote it still opens it; nothing newer does.
    koru_sqlite::prepare(&ten_schema(), &mut c).unwrap();
    assert_eq!(user_version(&c).unwrap(), 10);
    assert_eq!(
        c.query_row("SELECT count(*) FROM bot_decisions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(prepare_eleven(&mut c).is_err());
    assert_eq!(user_version(&c).unwrap(), 10);
}

#[test]
fn m5_runs11_migration_sql_guard_alone_refuses_and_rolls_back_without_dropping_a_row() {
    // The preflight bypassed: the plain migrator meets the SQL guard.
    let mut c = populated_ten();
    insert(
        &c,
        "bot_decisions",
        &decision("koru_edit", "granted", "held"),
    )
    .unwrap();
    let before = snapshot(&c);
    let error = koru_sqlite::prepare(&SCHEMA, &mut c).unwrap_err();
    assert!(error.to_string().contains("found=0"), "{error}");
    assert_eq!(user_version(&c).unwrap(), 10);
    assert_eq!(snapshot(&c), before);
    assert_eq!(
        c.query_row("SELECT count(*) FROM bot_decisions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    for guard in ["m5_decisions_empty", "m5_fk_guard"] {
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM sqlite_temp_master WHERE name=?1",
                [guard],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    assert_fk(&c);
}

#[test]
fn m5_runs11_migration_closing_fk_guard_rolls_back_the_recreate() {
    let mut c = populated_ten();
    let before = snapshot(&c);
    let sql = format!(
        "PRAGMA defer_foreign_keys=ON; INSERT INTO run_attempts(run_id,generation,attempt_id,state,updated_at_ms) VALUES('missing',1,'bad','created',0);\n{}",
        MIGRATIONS[10].sql
    );
    let steps = [Migration {
        to: 11,
        name: "030_fk_failure_fixture.sql",
        sql: Box::leak(sql.into_boxed_str()),
        columns: &[],
    }];
    let error = koru_sqlite::migrate(DATABASE_NAME, 11, &steps, &mut c).unwrap_err();
    assert!(error.to_string().contains("violations=0"), "{error}");
    assert_eq!(user_version(&c).unwrap(), 10);
    assert_eq!(snapshot(&c), before);
    assert_fk(&c);
}

#[test]
fn m5_runs11_migration_a_10_build_refuses_11() {
    let mut c = koru_sqlite::open_in_memory(&eleven_schema()).unwrap();
    let before = snapshot(&c);
    assert!(matches!(
        koru_sqlite::prepare(&ten_schema(), &mut c),
        Err(OpenError::SchemaTooNew {
            discovered: 11,
            supported: 10,
            ..
        })
    ));
    assert_eq!(snapshot(&c), before);
}

#[test]
fn m5_runs11_migration_admits_private_and_browser_kinds_at_the_sql_level() {
    let c = current();
    for kind in ["private_operation", "browser_origin"] {
        // Accepted in pending/publishing and in consumed/settled.
        for (authorization, phase) in [("pending", "publishing"), ("consumed", "settled")] {
            check(&c, &format!("{kind}/{authorization}/{phase}"), true, || {
                insert(&c, "bot_decisions", &decision(kind, authorization, phase)).map(|_| ())
            });
        }
        // Operation-keyed: an operation id, no native prompt or deadline.
        let row = decision(kind, "pending", "publishing");
        let mut no_operation = row.clone();
        no_operation.remove("operation_id");
        check(&c, &format!("{kind} without operation"), false, || {
            insert(&c, "bot_decisions", &no_operation).map(|_| ())
        });
        row_case(
            &c,
            "bot_decisions",
            &row,
            "native_prompt_id",
            text("p"),
            false,
        );
        row_case(
            &c,
            "bot_decisions",
            &row,
            "native_deadline_at_ms",
            integer(599900),
            false,
        );
    }
    row_case(
        &c,
        "bot_decisions",
        &decision("koru_edit", "pending", "publishing"),
        "kind",
        text("other"),
        false,
    );
    // One operation id per Run across every operation-keyed kind.
    c.execute_batch("SAVEPOINT m5_operation").unwrap();
    insert(
        &c,
        "bot_decisions",
        &decision("koru_edit", "consumed", "settled"),
    )
    .unwrap();
    let mut second = decision("private_operation", "pending", "publishing");
    second.insert("decision_id", text(id(22)));
    assert!(insert(&c, "bot_decisions", &second).is_err());
    second.insert("operation_id", text(id(23)));
    insert(&c, "bot_decisions", &second).unwrap();
    c.execute_batch("ROLLBACK TO m5_operation; RELEASE m5_operation")
        .unwrap();
}

/// The Rust vocabulary mirrors the table's `kind` list, tolerating exactly
/// the one token admitted ahead of its variant.
#[test]
fn m5_runs11_migration_rust_kind_vocabulary_mirrors_the_check_list() {
    use koru_delegate_core::decisions::DecisionKind;
    let c = koru_sqlite::open_in_memory(&SCHEMA).unwrap();
    let sql: String = c
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='bot_decisions'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let list = sql
        .split("CHECK(kind IN (")
        .nth(1)
        .unwrap()
        .split("))")
        .next()
        .unwrap();
    let tokens: Vec<&str> = list.split(',').map(|t| t.trim_matches('\'')).collect();
    assert_eq!(
        tokens,
        [
            "koru_edit",
            "native_approval",
            "private_operation",
            "browser_origin"
        ]
    );
    let mut undecoded = Vec::new();
    for token in &tokens {
        match serde_json::from_value::<DecisionKind>(serde_json::Value::String(token.to_string())) {
            Ok(kind) => assert_eq!(serde_json::to_value(kind).unwrap(), *token),
            Err(_) => undecoded.push(*token),
        }
    }
    // Exactly `browser_origin`, not "any token Rust does not know": the row
    // decoder refuses it until M7 adds the variant.
    assert_eq!(undecoded, ["browser_origin"]);
    for kind in [
        DecisionKind::KoruEdit,
        DecisionKind::NativeApproval,
        DecisionKind::PrivateOperation,
    ] {
        let token = serde_json::to_value(kind).unwrap();
        assert!(tokens.contains(&token.as_str().unwrap()));
    }
}

fn effect() -> Row {
    Row::from([
        ("run_id", text(id(1))),
        ("operation_id", text(id(60))),
        ("user_scope", text(id(4))),
        ("device_id", text(id(5))),
        ("bot_id", text(id(6))),
        ("attempt_id", text(id(11))),
        ("generation", integer(1)),
        ("turn_id", text(id(13))),
        ("dispatcher_instance", text(id(61))),
        ("connection_id", text(id(62))),
        ("connection_revision", integer(1)),
        ("account_digest", blob(32)),
        ("item_id", text(id(63))),
        ("item_ref_digest", blob(32)),
        ("operation", text("read_item")),
        ("request_fingerprint", blob(32)),
        ("barrier_request_id", text(id(64))),
        ("config_revision", integer(1)),
        ("policy_revision", integer(1)),
        ("policy_digest", blob(32)),
        ("stage", text("prepared")),
        ("created_at_ms", integer(-100)),
        ("updated_at_ms", integer(-100)),
    ])
}
fn with_barrier() -> Connection {
    let c = current();
    c.execute(
        "INSERT INTO bot_policy_barriers(user_scope,device_id,bot_id,request_id,config_revision,policy_revision,payload_digest,effects_allowed,state,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,1,1,zeroblob(32),1,'effective',0,0)",
        [id(4), id(5), id(6), id(64)],
    )
    .unwrap();
    c
}

#[test]
fn m5_runs11_migration_private_effect_checks_stages_and_companions() {
    let c = with_barrier();
    check(&c, "prepared", true, || {
        insert(&c, "bot_private_effects", &effect()).map(|_| ())
    });
    let base = effect();
    for (column, value) in [
        ("stage", text("other")),
        ("operation", text("write_item")),
        ("refusal", text("not_dispatched")),
        ("outcome_bytes", integer(1)),
        ("withheld", integer(0)),
        ("dispatched_at_ms", integer(1)),
        ("resolved_at_ms", integer(1)),
        ("generation", integer(0)),
        ("connection_revision", integer(0)),
        ("config_revision", integer(0)),
        ("account_digest", blob(31)),
        ("request_fingerprint", text("x")),
        ("dispatcher_instance", text("short")),
        ("barrier_request_id", text(id(99))),
        ("run_id", text(id(98))),
        ("decision_id", text(id(20))),
        ("binding_digest", blob(32)),
        ("schedule_id", text(id(70))),
        ("grant_revision", integer(1)),
    ] {
        row_case(&c, "bot_private_effects", &base, column, value, false);
    }
    let staged = |stage: &str, extra: &[(&'static str, Value)]| {
        let mut row = effect();
        row.insert("stage", text(stage));
        for (column, value) in extra {
            row.insert(column, value.clone());
        }
        row
    };
    let cases: [(Row, bool); 12] = [
        (
            staged("dispatched", &[("dispatched_at_ms", integer(1))]),
            true,
        ),
        (staged("dispatched", &[]), false),
        (
            staged(
                "confirmed",
                &[
                    ("dispatched_at_ms", integer(1)),
                    ("resolved_at_ms", integer(2)),
                    ("outcome_bytes", integer(0)),
                    ("withheld", integer(1)),
                ],
            ),
            true,
        ),
        (
            staged(
                "confirmed",
                &[
                    ("dispatched_at_ms", integer(1)),
                    ("resolved_at_ms", integer(2)),
                    ("outcome_bytes", integer(4294967296)),
                    ("withheld", integer(0)),
                ],
            ),
            false,
        ),
        (
            staged(
                "confirmed",
                &[
                    ("dispatched_at_ms", integer(1)),
                    ("resolved_at_ms", integer(2)),
                ],
            ),
            false,
        ),
        (
            staged(
                "refused",
                &[
                    ("refusal", text("not_dispatched")),
                    ("resolved_at_ms", integer(2)),
                ],
            ),
            true,
        ),
        (
            staged(
                "refused",
                &[
                    ("refusal", text("vendor_rejected")),
                    ("dispatched_at_ms", integer(1)),
                    ("resolved_at_ms", integer(2)),
                ],
            ),
            true,
        ),
        (staged("refused", &[("resolved_at_ms", integer(2))]), false),
        (
            staged(
                "refused",
                &[
                    ("refusal", text("vendor said no")),
                    ("resolved_at_ms", integer(2)),
                ],
            ),
            false,
        ),
        (
            staged(
                "unknown",
                &[
                    ("dispatched_at_ms", integer(1)),
                    ("resolved_at_ms", integer(2)),
                ],
            ),
            true,
        ),
        (staged("unknown", &[("resolved_at_ms", integer(2))]), false),
        (
            staged("unknown", &[("dispatched_at_ms", integer(1))]),
            false,
        ),
    ];
    for (index, (row, accepts)) in cases.iter().enumerate() {
        check(&c, &format!("stage case {index}"), *accepts, || {
            insert(&c, "bot_private_effects", row).map(|_| ())
        });
    }
}

#[test]
fn m5_runs11_migration_private_effect_decision_needs_a_resume_row() {
    let c = with_barrier();
    // A consume without an acknowledged resume is refused by the foreign key,
    // as `native_decision_leases` requires.
    insert(
        &c,
        "bot_decisions",
        &decision("private_operation", "granted", "held"),
    )
    .unwrap();
    let mut consumed = effect();
    consumed.insert("operation_id", text(id(14)));
    consumed.insert("decision_id", text(id(20)));
    consumed.insert("binding_digest", blob(32));
    check(&c, "decision effect without resume", false, || {
        insert(&c, "bot_private_effects", &consumed).map(|_| ())
    });
    let mut w = wait();
    w.insert("phase", text("resume_reserved"));
    insert(&c, "bot_wait_runtime", &w).unwrap();
    insert(&c, "bot_decision_resumes", &resume(true)).unwrap();
    insert(&c, "bot_private_effects", &consumed).unwrap();
    // One effect per decision, and one open effect per Run.
    let mut second = consumed.clone();
    second.insert("operation_id", text(id(65)));
    assert!(insert(&c, "bot_private_effects", &second).is_err());
    let mut other = effect();
    other.insert("operation_id", text(id(66)));
    assert!(insert(&c, "bot_private_effects", &other).is_err());
    // A decision of another Run is not this row's decision.
    let mut foreign = effect();
    foreign.insert("operation_id", text(id(67)));
    foreign.insert("decision_id", text(id(77)));
    foreign.insert("binding_digest", blob(32));
    assert!(insert(&c, "bot_private_effects", &foreign).is_err());
    assert_fk(&c);
}
