// SPDX-License-Identifier: MPL-2.0
//! ai22 → ai23 on the actual migrator and linked SQLite. In-memory, plus one
//! temporary file for the real open path. No worker, Store or delegate.
use super::decisions::{ai22, populated_ai22};
use super::library::{foreign_keys_held, snapshot};
use super::*;
use std::collections::BTreeMap;

pub(super) fn ai23() -> Schema {
    Schema {
        database: crate::DATABASE_FILE,
        understood: 23,
        migrations: &MIGRATIONS[..23],
    }
}
fn prepare_ai23(c: &mut Connection) {
    koru_sqlite::prepare_with_preflight(&ai23(), c, refuse_superseded_18).unwrap();
}

/// The rows ai23 has to carry: on top of the ai22 fixture (receipts of the
/// eight ai16 kinds in every stage, `set_knowledge`, `ensure_memory`, one
/// prepared Routine with its draft), the remaining Routine receipt kinds and
/// stages, a Routine in every creation state, and a lifecycle member of every
/// kind, stage and owner. Text that is not canonical JSON and non-zero blobs
/// show the copy does not decode or re-encode anything.
const ROWS: &str = "
 INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,expected_revision,outcome_json,created_at_ms,updated_at_ms) VALUES
  ('scope','routine-create-unconfirmed','bot','retained','create_routine','prepared',2,'{ \"kept\" : [1, 2] }',3,4),
  ('scope','routine-create-bound','bot','retained','create_routine','completed',3,'{ \"kept\" : \"bound\" }',5,6),
  ('scope','routine-create-conflict','bot','retained','create_routine','rejected',4,'{ \"kept\" : \"conflict\" }',7,8);
 INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,outcome_json,created_at_ms,updated_at_ms) VALUES
  ('scope','routine-draft-rejected','bot','retained','routine_draft','rejected','{ \"closed\" : true }',9,10);
 INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,planned_run_id,captured_config_revision,captured_policy_revision,submission_version,submission_json,outcome_json,created_at_ms,updated_at_ms) VALUES
  ('scope','occurrence-prepared','bot','retained','routine_occurrence','prepared','occurrence-run-1',1,1,2,'{ \"version\" : 2 }',NULL,11,12),
  ('scope','occurrence-accepted','bot','retained','routine_occurrence','accepted','occurrence-run-2',1,1,2,'{ \"version\" : 2 }','{ \"kind\" : \"accepted\" }',13,14),
  ('scope','occurrence-rejected','bot','retained','routine_occurrence','rejected','occurrence-run-3',1,1,2,'{ \"version\" : 2 }','{ \"kind\" : \"rejected\" }',15,16),
  ('scope','occurrence-completed','bot','retained','routine_occurrence','completed','occurrence-run-4',1,1,2,'{ \"version\" : 2 }','{ \"kind\" : \"accepted\" }',17,18);
 INSERT INTO bot_routines(schedule_id,user_scope_id,owner_device_id,bot_id,workspace_id,creation_request_id,store_request_id,seed_json,seed_hash,seed_title,source_bot_policy_revision,grant_revision,grant_json,grant_digest,creation_state,origin_edit_revision,refusal,created_at_ms,updated_at_ms) VALUES
  ('schedule-unconfirmed','scope','device','bot','home','routine-create-unconfirmed','store-unconfirmed','{ \"seed\" : 1 }',X'0101010101010101010101010101010101010101010101010101010101010101','Unconfirmed',1,1,'{ \"grant\" : 1 }',X'0202020202020202020202020202020202020202020202020202020202020202','unconfirmed',NULL,NULL,3,4),
  ('schedule-bound','scope','device','bot','home','routine-create-bound','store-bound','{ \"seed\" : 2 }',X'0303030303030303030303030303030303030303030303030303030303030303','Bound',2,1,'{ \"grant\" : 2 }',X'0404040404040404040404040404040404040404040404040404040404040404','bound',X'0A0B0C',NULL,5,6),
  ('schedule-conflict','scope','device','bot','home','routine-create-conflict','store-conflict','{ \"seed\" : 3 }',X'0505050505050505050505050505050505050505050505050505050505050505','Conflict',3,1,'{ \"grant\" : 3 }',X'0606060606060606060606060606060606060606060606060606060606060606','conflict',NULL,'owner_changed',7,8);
 INSERT INTO bot_lifecycle_members(user_scope_id,operation_id,ordinal,kind,owner,chat_id,run_id,workspace_id,document_id,attachment_id,reason,prior_state,intended,stage,retained_reason,child_request_id,updated_at_ms) VALUES
  ('scope','archive',0,'bot','ai',NULL,NULL,NULL,NULL,NULL,'bot','live','hide','pending',NULL,NULL,2),
  ('scope','archive',1,'chat','ai','chat',NULL,NULL,NULL,NULL,'owned_conversation','live','hide','acknowledged',NULL,NULL,3),
  ('scope','archive',2,'run_reference','delegate','chat','run','home',NULL,NULL,'run_of_conversation','trashed','retain','retained','shared_reference','child-run',4),
  ('scope','archive',3,'owned_role','store',NULL,NULL,'home','role',NULL,'role_creation_receipt','absent','remove','unavailable',NULL,'child-role',5),
  ('scope','archive',4,'attachment_copy','ai','chat',NULL,NULL,NULL,'attachment','exclusive_attachment','archived','restore','conflict',NULL,NULL,6);";

pub(super) fn populated() -> Connection {
    let c = populated_ai22();
    assert_eq!(user_version(&c).unwrap(), 22);
    c.execute_batch(ROWS).unwrap();
    c
}

/// Every foreign key of every table, as `PRAGMA foreign_key_list` reports it.
pub(super) fn foreign_keys(c: &Connection) -> BTreeMap<String, Vec<String>> {
    let tables: Vec<String> = c
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    tables
        .into_iter()
        .map(|table| {
            let mut keys: Vec<String> = c
                .prepare("SELECT \"table\",\"from\",\"to\",on_update,on_delete,id,seq FROM pragma_foreign_key_list(?1)")
                .unwrap()
                .query_map([&table], |r| {
                    Ok(format!(
                        "{}|{}|{}|{}|{}|{}|{}",
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            keys.sort();
            (table, keys)
        })
        .collect()
}

fn squeeze(sql: &str) -> String {
    sql.chars().filter(|c| !c.is_whitespace()).collect()
}

#[test]
fn migration_ai23_fixture_holds_every_receipt_kind_creation_state_and_member_kind() {
    let c = populated();
    let distinct = |sql: &str| -> Vec<String> {
        c.prepare(sql)
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        distinct("SELECT DISTINCT command_kind FROM bot_command_receipts ORDER BY 1"),
        [
            "continue",
            "create",
            "create_chat",
            "create_routine",
            "ensure_memory",
            "pause",
            "resume",
            "routine_draft",
            "routine_occurrence",
            "set_knowledge",
            "start",
            "update",
            "update_access"
        ]
    );
    assert_eq!(
        distinct("SELECT DISTINCT creation_state FROM bot_routines ORDER BY 1"),
        ["bound", "conflict", "prepared", "unconfirmed"]
    );
    assert_eq!(
        distinct("SELECT DISTINCT kind FROM bot_lifecycle_members ORDER BY 1"),
        [
            "attachment_copy",
            "bot",
            "chat",
            "owned_role",
            "run_reference"
        ]
    );
}

#[test]
fn migration_ai23_preserves_every_populated_ai22_row_object_index_and_foreign_key() {
    let mut c = populated();
    println!("Rust-linked SQLite {}", rusqlite::version());
    let before = snapshot(&c);
    let keys_before = foreign_keys(&c);
    prepare_ai23(&mut c);
    assert_eq!(user_version(&c).unwrap(), 23);
    let after = snapshot(&c);
    let keys_after = foreign_keys(&c);

    // Every raw row of every table, value for value and type for type.
    for (name, (sql, rows)) in &before {
        let (new_sql, new_rows) = &after[name];
        assert_eq!(rows, new_rows, "raw rows changed: {name}");
        if !matches!(
            name.as_str(),
            "bot_command_receipts" | "bot_lifecycle_members"
        ) {
            // Includes the three receipt indexes and the five member indexes,
            // which the rebuild recreates with their exact predicates.
            assert_eq!(sql, new_sql, "definition changed: {name}");
        }
    }
    // Exactly three objects are new, and the grant table starts empty.
    let added: Vec<&str> = after
        .keys()
        .filter(|name| !before.contains_key(*name))
        .map(String::as_str)
        .collect();
    assert_eq!(
        added,
        [
            "bot_lifecycle_member_routine",
            "bot_pending_routine_access",
            "bot_routine_grants"
        ]
    );
    assert!(after["bot_routine_grants"].1.is_empty());

    // The rebuilt definitions are the old ones plus the stated additions and
    // nothing else: removing the additions gives back the ai22 text.
    let receipts = after["bot_command_receipts"]
        .0
        .replace(
            "'routine_occurrence','update_routine_access'))",
            "'routine_occurrence'))",
        )
        .replace(
            &squeeze(
                ",
 -- New in ai23. expected_revision is the expected Routine grant revision.
 CHECK(command_kind<>'update_routine_access' OR
   (stage IN ('prepared','completed','rejected') AND expected_revision IS NOT NULL
    AND outcome_json IS NOT NULL AND typeof(outcome_json)='text'
    AND length(CAST(outcome_json AS BLOB))<=16384))",
            ),
            "",
        );
    assert_eq!(receipts, before["bot_command_receipts"].0);
    // 018 created this table under its own name; the rebuild renamed it into
    // place, and SQLite quotes a renamed table's name. Nothing else differs.
    let members = after["bot_lifecycle_members"]
        .0
        .replacen(
            "CREATETABLE\"bot_lifecycle_members\"(",
            "CREATETABLEbot_lifecycle_members(",
            1,
        )
        .replace("'attachment_copy','routine'))", "'attachment_copy'))")
        .replace(
            "'exclusive_attachment','owned_routine'))",
            "'exclusive_attachment'))",
        )
        .replace(
            &squeeze(
                "
    -- New in ai23. document_id is the Routine's Schedule document.
    OR (kind = 'routine' AND chat_id IS NULL AND run_id IS NULL AND workspace_id IS NOT NULL AND document_id IS NOT NULL AND attachment_id IS NULL)",
            ),
            "",
        );
    assert_eq!(members, before["bot_lifecycle_members"].0);

    // Foreign keys in both directions: every old table keeps exactly the
    // keys it had (children of the rebuilt receipts table still name it, and
    // the rebuilt member table still names its intent).
    for (table, keys) in &keys_before {
        assert_eq!(&keys_after[table], keys, "foreign keys changed: {table}");
    }
    for table in [
        "bots",
        "bot_chat_runs",
        "bot_lifecycle_intents",
        "bot_memory_creation_intents",
        "bot_library_captures",
        "bot_routines",
        "bot_routine_preparations",
        "bot_routine_grants",
    ] {
        assert!(
            keys_after[table]
                .iter()
                .any(|key| key.starts_with("bot_command_receipts|")),
            "lost receipt parent in {table}"
        );
    }
    assert!(
        keys_after["bot_lifecycle_members"]
            .iter()
            .any(|key| key.starts_with("bot_lifecycle_intents|"))
    );
    assert!(
        keys_after["bot_routine_grants"]
            .iter()
            .any(|key| key.starts_with("bot_routines|"))
    );
    foreign_keys_held(&c);
    for sql in [
        "DELETE FROM bot_command_receipts WHERE request_id='routine-create-bound'",
        "DELETE FROM bot_command_receipts WHERE request_id='routine-draft'",
        "UPDATE bot_routines SET creation_request_id='missing' WHERE schedule_id='schedule-bound'",
        "UPDATE bot_lifecycle_members SET operation_id='missing' WHERE ordinal=0",
        "DELETE FROM bot_lifecycle_intents WHERE operation_id='archive'",
    ] {
        assert!(c.execute(sql, []).is_err(), "accepted: {sql}");
    }
    assert_eq!(snapshot(&c), after);

    // The three old partial indexes and the new one still serve their scans.
    for (index, predicate) in [
        (
            "bot_unresolved_coordination",
            "stage IN ('prepared','accepted') AND command_kind IN ('start','continue','pause','resume','routine_occurrence')",
        ),
        (
            "bot_pending_access",
            "command_kind='update_access' AND stage='prepared'",
        ),
        (
            "bot_pending_routine_access",
            "command_kind='update_routine_access' AND stage='prepared'",
        ),
    ] {
        c.prepare(&format!(
            "SELECT rowid FROM bot_command_receipts INDEXED BY {index} WHERE {predicate}"
        ))
        .unwrap_or_else(|error| panic!("{index}: {error}"));
    }

    // Reopened, it is already current and nothing runs again.
    prepare_ai23(&mut c);
    assert_eq!(snapshot(&c), after);
}

#[test]
fn migration_ai23_failure_in_any_step_rolls_back_rows_schema_and_stamp() {
    let original = MIGRATIONS[22].sql;
    // A dropped receipt leaves its children dangling: the guard refuses.
    let omitted = original.replace(
        "created_at_ms,updated_at_ms FROM bot_command_receipts;",
        "created_at_ms,updated_at_ms FROM bot_command_receipts WHERE request_id<>'routine-create-bound';",
    );
    assert_ne!(omitted, original);
    let after_receipts = original.replace(
        "DROP TABLE bot_command_receipts;",
        "DROP TABLE bot_command_receipts; SELECT * FROM injected_ai23_receipt_failure;",
    );
    assert_ne!(after_receipts, original);
    let after_members = original.replace(
        "DROP TABLE bot_lifecycle_members;",
        "DROP TABLE bot_lifecycle_members; SELECT * FROM injected_ai23_member_failure;",
    );
    assert_ne!(after_members, original);
    for (sql, marker) in [
        (omitted, "CHECK constraint failed"),
        (after_receipts, "injected_ai23_receipt_failure"),
        (after_members, "injected_ai23_member_failure"),
        (
            format!("{original}\nSELECT * FROM injected_ai23_late_failure;"),
            "injected_ai23_late_failure",
        ),
    ] {
        let mut c = populated();
        let before = snapshot(&c);
        let keys = foreign_keys(&c);
        let steps = Box::leak(
            vec![Migration {
                to: 23,
                name: "023_failure_fixture.sql",
                sql: Box::leak(sql.into_boxed_str()),
                columns: &[],
            }]
            .into_boxed_slice(),
        );
        let schema = Schema {
            database: crate::DATABASE_FILE,
            understood: 23,
            migrations: steps,
        };
        let error =
            koru_sqlite::prepare_with_preflight(&schema, &mut c, refuse_superseded_18).unwrap_err();
        assert!(error.to_string().contains(marker), "{error}");
        assert_eq!(user_version(&c).unwrap(), 22);
        assert_eq!(snapshot(&c), before);
        assert_eq!(foreign_keys(&c), keys);
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM sqlite_temp_master WHERE name='bot_ai23_fk_guard'",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
            0
        );
        foreign_keys_held(&c);
        // The real step still applies to what the failure left.
        prepare_ai23(&mut c);
        assert_eq!(user_version(&c).unwrap(), 23);
    }
}

#[test]
fn migration_ai22_build_refuses_ai23_before_preflight_or_configuration() {
    let mut c = populated();
    prepare_ai23(&mut c);
    let before = snapshot(&c);
    c.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA synchronous=OFF")
        .unwrap();
    let error =
        koru_sqlite::prepare_with_preflight(&ai22(), &mut c, |_| panic!("old preflight ran"))
            .unwrap_err();
    assert!(matches!(
        error,
        OpenError::SchemaTooNew {
            discovered: 23,
            supported: 22,
            ..
        }
    ));
    assert_eq!(user_version(&c).unwrap(), 23);
    assert_eq!(snapshot(&c), before);
    for pragma in ["PRAGMA foreign_keys", "PRAGMA synchronous"] {
        assert_eq!(c.query_row(pragma, [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
}

#[test]
fn migration_fresh_database_reaches_ai23_with_an_empty_grant_table() {
    // Stopped at ai23: this build goes on to ai24 (`avatar.rs`).
    let mut c = Connection::open_in_memory().unwrap();
    prepare_ai23(&mut c);
    assert_eq!(user_version(&c).unwrap(), 23);
    assert_eq!(
        c.query_row("SELECT count(*) FROM bot_routine_grants", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        0
    );
    for index in ["bot_pending_routine_access", "bot_lifecycle_member_routine"] {
        let exists: bool = c
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1)",
                [index],
                |r| r.get(0),
            )
            .unwrap();
        assert!(exists, "missing {index}");
    }
    assert_eq!(
        c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, u32>(
            0
        ))
        .unwrap(),
        0
    );
}

/// The real open path, on a temporary file: a populated ai22 file opened by
/// this build is migrated once, keeps its rows, and reopens unchanged.
#[test]
fn migration_ai23_through_the_real_open_of_a_populated_ai22_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ai.db");
    let before = {
        let mut c = Connection::open(&path).unwrap();
        koru_sqlite::prepare_with_preflight(&ai22(), &mut c, refuse_superseded_18).unwrap();
        c.execute_batch(
            "INSERT INTO chats(id,title,current_provider,current_model,created_at_ms,updated_at_ms) VALUES('chat','kept','agent-codex','default',1,2);
             INSERT INTO bots(bot_id,user_scope_id,home_workspace_id,owner_device_id,name,role_workspace_id,role_document_id,working_folder,provider,model,reasoning,revision,policy_revision,access,created_at_ms,updated_at_ms) VALUES('bot','scope','home','device','kept','home','role','folder','agent-codex','default','auto',1,1,'default',1,2);
             INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,expected_revision,outcome_json,created_at_ms,updated_at_ms) VALUES('scope','routine-create','bot','retained','create_routine','prepared',1,'{ \"kept\" : true }',1,2);
             INSERT INTO bot_routines(schedule_id,user_scope_id,owner_device_id,bot_id,workspace_id,creation_request_id,store_request_id,seed_json,seed_hash,seed_title,source_bot_policy_revision,grant_revision,grant_json,grant_digest,creation_state,created_at_ms,updated_at_ms) VALUES('schedule','scope','device','bot','home','routine-create','store-request','{}',zeroblob(32),'Routine',1,1,'{ \"grant\" : 1 }',zeroblob(32),'prepared',1,2);",
        )
        .unwrap();
        assert_eq!(user_version(&c).unwrap(), 22);
        snapshot(&c)
    };
    for _ in 0..2 {
        let c = open(&path).unwrap();
        // The real open runs every later step too.
        assert_eq!(user_version(&c).unwrap(), SCHEMA_VERSION);
        let after = snapshot(&c);
        for (name, (_, rows)) in &before {
            if name == "bots" {
                // ai24 appends one nullable column, NULL in every old row,
                // and ai26 one more, on in every old row.
                let kept: Vec<String> = after[name]
                    .1
                    .iter()
                    .map(|row| format!("{}]", row.strip_suffix(", Null, Integer(1)]").unwrap()))
                    .collect();
                assert_eq!(rows, &kept, "raw rows changed: {name}");
                continue;
            }
            assert_eq!(rows, &after[name].1, "raw rows changed: {name}");
        }
        assert!(after["bot_routine_grants"].1.is_empty());
        foreign_keys_held_without_fixture_rows(&c);
    }
}

fn foreign_keys_held_without_fixture_rows(c: &Connection) {
    assert_eq!(
        c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, u32>(
            0
        ))
        .unwrap(),
        0
    );
    assert!(
        c.execute(
            "DELETE FROM bot_command_receipts WHERE request_id='routine-create'",
            []
        )
        .is_err()
    );
}

#[test]
fn migration_ai23_new_kind_grant_table_and_routine_member_enforce_their_checks() {
    let mut c = populated();
    prepare_ai23(&mut c);
    let case = |label: &str, accepts: bool, sql: &str| {
        c.execute_batch("SAVEPOINT ai23_case").unwrap();
        let result = c.execute_batch(sql);
        c.execute_batch("ROLLBACK TO ai23_case; RELEASE ai23_case")
            .unwrap();
        assert_eq!(result.is_ok(), accepts, "{label}: {result:?}");
    };
    let receipt = |request: &str, stage: &str, expected: &str, outcome: &str, extra: &str| {
        format!(
            "INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,expected_revision,outcome_json,created_at_ms,updated_at_ms{}) VALUES('scope','{request}','bot','retained','update_routine_access','{stage}',{expected},{outcome},1,2{})",
            if extra.is_empty() {
                ""
            } else {
                ",planned_run_id"
            },
            extra
        )
    };
    for stage in ["prepared", "completed", "rejected"] {
        case(stage, true, &receipt("edit", stage, "1", "'{}'", ""));
    }
    case(
        "accepted stage",
        false,
        &receipt("edit", "accepted", "1", "'{}'", ""),
    );
    case(
        "no expected revision",
        false,
        &receipt("edit", "prepared", "NULL", "'{}'", ""),
    );
    case(
        "no outcome",
        false,
        &receipt("edit", "prepared", "1", "NULL", ""),
    );
    case(
        "blob outcome",
        false,
        &receipt("edit", "prepared", "1", "CAST('{}' AS BLOB)", ""),
    );
    case(
        "planned run",
        false,
        &receipt("edit", "prepared", "1", "'{}'", ",'edit-run'"),
    );
    case(
        "oversized outcome",
        false,
        &receipt(
            "edit",
            "prepared",
            "1",
            &format!("'{}'", serde_json::to_string(&"x".repeat(16384)).unwrap()),
            "",
        ),
    );

    c.execute_batch(&receipt("edit", "prepared", "1", "'{}'", ""))
        .unwrap();
    let grant = |schedule: &str, request: &str, revision: &str, json: &str, digest: &str| {
        format!(
            "INSERT INTO bot_routine_grants(schedule_id,user_scope_id,owner_device_id,bot_id,request_id,source_bot_policy_revision,grant_revision,grant_json,grant_digest,updated_at_ms) VALUES('{schedule}','scope','device','bot','{request}',1,{revision},{json},{digest},2)"
        )
    };
    case(
        "edited grant",
        true,
        &grant("schedule-bound", "edit", "2", "'{}'", "zeroblob(32)"),
    );
    case(
        "revision one is the creation grant's",
        false,
        &grant("schedule-bound", "edit", "1", "'{}'", "zeroblob(32)"),
    );
    case(
        "unknown Routine",
        false,
        &grant("missing", "edit", "2", "'{}'", "zeroblob(32)"),
    );
    case(
        "unknown receipt",
        false,
        &grant("schedule-bound", "missing", "2", "'{}'", "zeroblob(32)"),
    );
    case(
        "blob grant",
        false,
        &grant(
            "schedule-bound",
            "edit",
            "2",
            "CAST('{}' AS BLOB)",
            "zeroblob(32)",
        ),
    );
    case(
        "short digest",
        false,
        &grant("schedule-bound", "edit", "2", "'{}'", "zeroblob(31)"),
    );
    c.execute_batch(&grant(
        "schedule-bound",
        "edit",
        "2",
        "'{}'",
        "zeroblob(32)",
    ))
    .unwrap();
    case(
        "one row per Routine",
        false,
        &grant("schedule-bound", "edit", "3", "'{}'", "zeroblob(32)"),
    );
    case(
        "receipt of a grant is retained",
        false,
        "DELETE FROM bot_command_receipts WHERE request_id='edit'",
    );
    case(
        "Routine of a grant is retained",
        false,
        "DELETE FROM bot_routines WHERE schedule_id='schedule-bound'",
    );

    let member = |ordinal: u32, columns: &str| {
        format!(
            "INSERT INTO bot_lifecycle_members(user_scope_id,operation_id,ordinal,kind,owner,chat_id,run_id,workspace_id,document_id,attachment_id,reason,prior_state,intended,stage,retained_reason,child_request_id,updated_at_ms) VALUES('scope','archive',{ordinal},'routine',{columns},2)"
        )
    };
    let routine = "'delegate',NULL,NULL,'home','schedule-bound',NULL,'owned_routine','live','hide','pending',NULL,'child-routine'";
    case("Routine member", true, &member(10, routine));
    case(
        "retained Routine member",
        true,
        &member(
            10,
            "'delegate',NULL,NULL,'home','schedule-bound',NULL,'owned_routine','live','retain','retained','ownership_unproven','child-routine'",
        ),
    );
    case(
        "Routine member with a conversation",
        false,
        &member(
            10,
            "'delegate','chat',NULL,'home','schedule-bound',NULL,'owned_routine','live','hide','pending',NULL,'child-routine'",
        ),
    );
    case(
        "Routine member without its document",
        false,
        &member(
            10,
            "'delegate',NULL,NULL,'home',NULL,NULL,'owned_routine','live','hide','pending',NULL,'child-routine'",
        ),
    );
    case(
        "Routine member without its workspace",
        false,
        &member(
            10,
            "'delegate',NULL,NULL,NULL,'schedule-bound',NULL,'owned_routine','live','hide','pending',NULL,'child-routine'",
        ),
    );
    case(
        "delegate member without a child request",
        false,
        &member(
            10,
            "'delegate',NULL,NULL,'home','schedule-bound',NULL,'owned_routine','live','hide','pending',NULL,NULL",
        ),
    );
    c.execute_batch(&member(10, routine)).unwrap();
    case(
        "one member per Routine and operation",
        false,
        &member(11, routine),
    );
    case(
        "another Routine of the same operation",
        true,
        &member(
            11,
            "'delegate',NULL,NULL,'home','schedule-conflict',NULL,'owned_routine','live','hide','pending',NULL,'child-other'",
        ),
    );
    foreign_keys_held(&c);
}
