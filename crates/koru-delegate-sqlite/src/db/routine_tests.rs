// SPDX-License-Identifier: MPL-2.0
use super::*;
use rusqlite::Connection;

#[test]
fn runs_nine_registers_exact_routine_tables_indexes_and_triggers_in_memory() {
    let mut conn = Connection::open_in_memory().unwrap();
    koru_sqlite::prepare(&SCHEMA, &mut conn).unwrap();
    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE name LIKE '%routine%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut expected = vec![
        "bot_routine_routes",
        "bot_routine_links",
        "bot_routine_policy_barriers",
        "native_routine_leases",
        "bot_routine_link_recovery",
        "bot_routine_link_subject",
        "bot_routine_policy_current",
        "native_routine_lease_subject",
        "bot_routine_dispatch_gate",
        "bot_routine_initial_dispatch_gate",
        "native_routine_lease_identity",
        "native_routine_lease_immutable",
        "native_routine_lease_retained",
        "native_routine_acquire_gate",
        "native_routine_initial_lease_gate",
    ];
    // SQLite autoindexes are derived implementation details of the exact keys.
    let names: Vec<_> = names
        .into_iter()
        .filter(|n| !n.starts_with("sqlite_autoindex_"))
        .collect();
    expected.sort();
    assert_eq!(names, expected);
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        9
    );
    assert_eq!(conn.query_row("SELECT dflt_value FROM pragma_table_info('bot_admissions') WHERE name='submission_version'",[],|r|r.get::<_,String>(0)).unwrap(),"1");
    assert_eq!(
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, u32>(
            0
        ))
        .unwrap(),
        0
    );
}

#[test]
fn runs_eight_reader_refuses_nine_before_preflight_or_configuration() {
    let mut conn = Connection::open_in_memory().unwrap();
    koru_sqlite::prepare(&SCHEMA, &mut conn).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA synchronous=OFF;")
        .unwrap();
    let called = std::cell::Cell::new(false);
    let old = koru_sqlite::Schema {
        database: DATABASE_NAME,
        understood: 8,
        migrations: &MIGRATIONS[..8],
    };
    assert!(matches!(
        koru_sqlite::prepare_with_preflight(&old, &mut conn, |_| {
            called.set(true);
            Ok(())
        }),
        Err(koru_sqlite::OpenError::SchemaTooNew {
            discovered: 9,
            supported: 8,
            ..
        })
    ));
    assert!(!called.get());
    for pragma in ["PRAGMA foreign_keys", "PRAGMA synchronous"] {
        assert_eq!(
            conn.query_row(pragma, [], |r| r.get::<_, u32>(0)).unwrap(),
            0
        );
    }
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        9
    );
}
