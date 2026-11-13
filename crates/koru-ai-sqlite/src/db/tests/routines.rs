// SPDX-License-Identifier: MPL-2.0
//! Real linked SQLite migration and constraint evidence, without disk opens.
use super::library::{foreign_keys_held, snapshot};
use super::memory_identity::{ai20, populated_v19};
use super::*;

fn populated_v20() -> Connection {
    let mut c = populated_v19();
    koru_sqlite::prepare_with_preflight(&ai20(), &mut c, refuse_superseded_18).unwrap();
    c.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,expected_revision,outcome_json,created_at_ms,updated_at_ms) VALUES('scope','knowledge-receipt','bot','retained','set_knowledge','completed',1,'{ \"retained\": true }',1,2)",[]).unwrap();
    c
}

#[test]
fn ai21_preserves_populated_ai20_receipts_children_and_indexes() {
    let mut c = populated_v20();
    println!("Rust-linked SQLite {}", rusqlite::version());
    let before = snapshot(&c);
    prepare(&mut c).unwrap();
    assert_eq!(user_version(&c).unwrap(), 21);
    let after = snapshot(&c);
    for (name, (sql, rows)) in &before {
        let (new_sql, new_rows) = &after[name];
        assert_eq!(rows, new_rows, "raw rows changed: {name}");
        if !matches!(
            name.as_str(),
            "bot_command_receipts" | "bot_unresolved_coordination"
        ) {
            assert_eq!(sql, new_sql, "schema changed: {name}");
        }
    }
    for table in [
        "bots",
        "bot_chat_runs",
        "bot_lifecycle_intents",
        "bot_memory_creation_intents",
        "bot_library_captures",
    ] {
        let target: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_list(?1) WHERE \"table\"='bot_command_receipts')",[table],|r|r.get(0)).unwrap();
        assert!(target, "lost receipt parent in {table}");
    }
    foreign_keys_held(&c);
    assert!(
        c.execute(
            "DELETE FROM bot_command_receipts WHERE command_kind='ensure_memory'",
            []
        )
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE bot_library_captures SET submission_request_id='missing'",
            []
        )
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE bot_memory_bindings SET creation_request_id='missing'",
            []
        )
        .is_err()
    );
    prepare(&mut c).unwrap();
    assert_eq!(snapshot(&c), after);
}

#[test]
fn ai21_rebuild_and_post_structure_faults_roll_back_rows_schema_and_stamp() {
    let original = MIGRATIONS[20].sql;
    let omitted = original.replace("created_at_ms,updated_at_ms FROM bot_command_receipts;", "created_at_ms,updated_at_ms FROM bot_command_receipts WHERE request_id<>'create-completed';");
    assert_ne!(omitted, original);
    let mid = original.replace(
        "DROP TABLE bot_command_receipts;",
        "DROP TABLE bot_command_receipts; SELECT * FROM injected_ai21_mid_failure;",
    );
    assert_ne!(mid, original);
    for (sql, marker) in [
        (omitted, "CHECK constraint failed"),
        (mid, "injected_ai21_mid_failure"),
        (
            format!("{original}\nSELECT * FROM injected_ai21_late_failure;"),
            "injected_ai21_late_failure",
        ),
    ] {
        let mut c = populated_v20();
        let before = snapshot(&c);
        let steps = Box::leak(
            vec![Migration {
                to: 21,
                name: "021_failure_fixture.sql",
                sql: Box::leak(sql.into_boxed_str()),
                columns: &[],
            }]
            .into_boxed_slice(),
        );
        let schema = Schema {
            database: crate::DATABASE_FILE,
            understood: 21,
            migrations: steps,
        };
        let error =
            koru_sqlite::prepare_with_preflight(&schema, &mut c, refuse_superseded_18).unwrap_err();
        assert!(error.to_string().contains(marker), "{error}");
        assert_eq!(user_version(&c).unwrap(), 20);
        assert_eq!(snapshot(&c), before);
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM sqlite_temp_master WHERE name='bot_ai21_fk_guard'",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
            0
        );
        foreign_keys_held(&c);
    }
}

#[test]
fn ai20_refuses_ai21_before_preflight_or_connection_configuration() {
    let mut c = populated_v20();
    prepare(&mut c).unwrap();
    let before = snapshot(&c);
    c.pragma_update(None, "foreign_keys", false).unwrap();
    c.pragma_update(None, "synchronous", 0).unwrap();
    let error =
        koru_sqlite::prepare_with_preflight(&ai20(), &mut c, |_| panic!("old preflight ran"))
            .unwrap_err();
    assert!(matches!(
        error,
        OpenError::SchemaTooNew {
            discovered: 21,
            supported: 20,
            ..
        }
    ));
    assert_eq!(user_version(&c).unwrap(), 21);
    assert_eq!(snapshot(&c), before);
    assert_eq!(
        c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        c.query_row("PRAGMA synchronous", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        0
    );
}

fn routine_rows(c: &Connection) {
    c.execute_batch("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,expected_revision,outcome_json,created_at_ms,updated_at_ms) VALUES('scope','routine-create','bot','retained','create_routine','prepared',1,'{}',1,2);
      INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,outcome_json,created_at_ms,updated_at_ms) VALUES('scope','routine-draft','bot','retained','routine_draft','prepared','{}',1,2);
      INSERT INTO bot_routines(schedule_id,user_scope_id,owner_device_id,bot_id,workspace_id,creation_request_id,store_request_id,seed_json,seed_hash,seed_title,source_bot_policy_revision,grant_revision,grant_json,grant_digest,creation_state,created_at_ms,updated_at_ms) VALUES('schedule','scope','device','bot','home','routine-create','store-request','{}',zeroblob(32),'Routine',1,1,'{}',zeroblob(32),'prepared',1,2);
      INSERT INTO bot_routine_preparations(user_scope_id,owner_device_id,bot_id,schedule_id,occurrence_ms,definition_revision,definition_digest,semantic_hash,preparation_hash,submission_request_id,run_id,chat_id,attempt_id,user_message_id,assistant_message_id,config_revision,policy_revision,grant_revision,submission_json,capture_json,stage,created_at_ms,updated_at_ms) VALUES('scope','device','bot','schedule',10,1,zeroblob(32),zeroblob(32),zeroblob(32),'routine-draft','routine-run','chat','routine-attempt','routine-user','routine-assistant',1,1,1,'{}','{}','prepared',1,2);").unwrap();
}

#[test]
fn ai21_routine_cohorts_enforce_null_groups_storage_types_bounds_and_links() {
    let mut c = populated_v20();
    prepare(&mut c).unwrap();
    routine_rows(&c);
    for sql in [
        "UPDATE bot_command_receipts SET submission_version=2 WHERE command_kind='start'",
        "UPDATE bot_command_receipts SET command_kind='routine_occurrence' WHERE request_id='routine-draft'",
        "UPDATE bot_command_receipts SET stage='accepted' WHERE request_id='routine-draft'",
        "UPDATE bot_command_receipts SET expected_revision=1 WHERE request_id='routine-draft'",
        "UPDATE bot_command_receipts SET expected_revision=NULL WHERE request_id='routine-create'",
        "UPDATE bot_command_receipts SET desired_pause=1 WHERE request_id='routine-create'",
        "UPDATE bot_command_receipts SET outcome_json=CAST('{}' AS BLOB) WHERE request_id='routine-create'",
        "UPDATE bot_routines SET seed_hash=CAST(printf('%032d',0) AS TEXT)",
        "UPDATE bot_routines SET grant_digest=zeroblob(31)",
        "UPDATE bot_routines SET seed_json=CAST('{}' AS BLOB)",
        "UPDATE bot_routines SET creation_state='bound'",
        "UPDATE bot_routines SET creation_state='conflict'",
        "UPDATE bot_routines SET workspace_id='foreign'",
        "UPDATE bot_routine_preparations SET owner_handoff='bad'",
        "UPDATE bot_routine_preparations SET definition_revision=4294967296",
        "UPDATE bot_routine_preparations SET capture_json=CAST('{}' AS BLOB)",
        "UPDATE bot_routine_preparations SET definition_digest=CAST(printf('%032d',0) AS TEXT)",
        "UPDATE bot_routine_preparations SET stage='linked'",
        "UPDATE bot_routine_preparations SET stage='linked',linked_request_id='start-completed',linked_chat_id='chat'",
        "UPDATE bot_routine_preparations SET stage='closed'",
        "DELETE FROM bot_routines",
        "DELETE FROM bot_command_receipts WHERE request_id IN ('routine-create','routine-draft')",
    ] {
        let before = snapshot(&c);
        assert!(c.execute(sql, []).is_err(), "accepted: {sql}");
        assert_eq!(snapshot(&c), before, "partial mutation: {sql}");
    }
    assert!(
        c.execute(
            "UPDATE bot_command_receipts SET outcome_json=?1 WHERE request_id='routine-draft'",
            [serde_json::to_string(&"x".repeat(16384)).unwrap()]
        )
        .is_err()
    );
    c.execute(
        "UPDATE bot_routines SET creation_state='bound',origin_edit_revision=X'0102'",
        [],
    )
    .unwrap();
    c.execute("UPDATE bot_command_receipts SET command_kind='routine_occurrence',planned_run_id='routine-run',captured_config_revision=1,captured_policy_revision=1,submission_version=2,submission_json='{}' WHERE request_id='routine-draft'",[]).unwrap();
    assert!(
        c.execute(
            "UPDATE bot_command_receipts SET submission_version=1 WHERE request_id='routine-draft'",
            []
        )
        .is_err()
    );
    c.execute("UPDATE bot_routine_preparations SET stage='linked',linked_request_id='routine-draft',linked_chat_id='chat'",[]).unwrap();
    foreign_keys_held(&c);
}
