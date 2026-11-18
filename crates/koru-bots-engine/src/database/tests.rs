// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn fresh_database_has_no_work_and_uses_shared_durable_configuration() {
    let db = Database::in_memory().unwrap();
    assert_eq!(db.status().unwrap(), Status::default());
    assert_eq!(diagnose(&db.connection).unwrap().schema_version, 1);
    for (pragma, expected) in [("synchronous", 2), ("foreign_keys", 1)] {
        let actual: i64 = db
            .connection
            .pragma_query_value(None, pragma, |r| r.get(0))
            .unwrap();
        assert_eq!(actual, expected);
    }
}

#[test]
fn newer_database_is_refused_before_pragmas_but_diagnosis_still_answers() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "PRAGMA user_version = 2; PRAGMA synchronous = OFF; CREATE TABLE future (value TEXT);",
        )
        .unwrap();
    assert_eq!(
        Error::from(koru_sqlite::prepare(&SCHEMA, &mut connection).unwrap_err()),
        Error::SchemaTooNew
    );
    let synchronous: i64 = connection
        .pragma_query_value(None, "synchronous", |r| r.get(0))
        .unwrap();
    assert_eq!(synchronous, 0);
    assert_eq!(
        diagnose(&connection).unwrap(),
        Diagnostic {
            schema_version: 2,
            supported_version: 1,
            status: None,
            status_error: Some(Error::SchemaTooNew),
        }
    );
    assert!(connection.prepare("SELECT * FROM bots").is_err());
}

#[test]
fn unsupported_domain_and_orphan_work_are_rejected() {
    let db = Database::in_memory().unwrap();
    assert!(
        db.connection
            .execute("INSERT INTO team(owner,paused) VALUES ('owner', 2)", [])
            .is_err()
    );
    assert!(
        db.connection
            .execute(
                "INSERT INTO workers VALUES ('missing', 'incarnation', 1, 1)",
                []
            )
            .is_err()
    );
}

#[test]
fn diagnostic_never_contains_content() {
    let db = Database::in_memory().unwrap();
    db.connection.execute("INSERT INTO bots(id,owner,name,settings,access,role_page,knowledge_pages,memory_page,visibility,paused,lifecycle) VALUES ('bot', 'owner', 'private name', X'00', X'00', NULL, X'00', NULL, 'private', 0, 'active')", []).unwrap();
    db.connection
        .execute(
            "INSERT INTO workers VALUES ('bot', 'incarnation', 1, 2)",
            [],
        )
        .unwrap();
    let report = diagnose(&db.connection).unwrap();
    assert_eq!(report.status.as_ref().unwrap().awake_bots, 1);
    assert_eq!(
        report.status.as_ref().unwrap().workers[0].last_heard_at_ms,
        2
    );
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("private name")
    );
}
