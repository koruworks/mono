// SPDX-License-Identifier: MPL-2.0
//! Actual linked SQLite and migration owner, without opening any disk root.
use super::*;
use rusqlite::{params, types::Value};
use std::collections::BTreeMap;

pub(super) fn populated_v18() -> Connection {
    let schema = Schema {
        database: crate::DATABASE_FILE,
        understood: 18,
        migrations: &MIGRATIONS[..18],
    };
    let c = koru_sqlite::open_in_memory(&schema).unwrap();
    for kind in [
        "create",
        "update",
        "update_access",
        "create_chat",
        "start",
        "continue",
        "pause",
        "resume",
    ] {
        for stage in ["prepared", "accepted", "rejected", "completed"] {
            let request = format!("{kind}-{stage}");
            let submission = matches!(kind, "start" | "continue");
            let pause = matches!(kind, "pause" | "resume");
            c.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,planned_run_id,captured_config_revision,captured_policy_revision,submission_version,submission_json,desired_pause,expected_revision,pause_ack_stage,outcome_json,created_at_ms,updated_at_ms) VALUES('scope',?1,'bot','fingerprint',?2,?3,?4,?5,?5,?5,?6,?7,?8,?9,?10,1,2)",params![request,kind,stage,submission.then(||request.clone()),submission.then_some(1),submission.then_some("{ \"version\": 1 }"),pause.then_some(kind=="pause"),pause.then_some(1),pause.then_some(if stage=="completed" {"acknowledged"} else {"pending"}),if stage=="prepared" && submission {None} else {Some("{ \"retained\": \"bytes\" }")}]).unwrap();
        }
    }
    c.execute_batch("INSERT INTO bots(bot_id,user_scope_id,home_workspace_id,owner_device_id,name,role_workspace_id,role_document_id,role_owned,role_creation_request_id,working_folder,provider,model,reasoning,revision,policy_revision,access,created_at_ms,updated_at_ms) VALUES('bot','scope','home','device','kept','home','role',1,'create-completed','folder','agent-codex','default','auto',1,1,'default',1,2);
        INSERT INTO chats(id,title,current_provider,current_model,created_at_ms,updated_at_ms) VALUES('chat','kept','agent-codex','default',1,2);
        INSERT INTO bot_chats(chat_id,bot_id,purpose,created_at_ms) VALUES('chat','bot','main',1);
        INSERT INTO messages(id,chat_id,ordinal,role,state,markdown,created_at_ms,completed_at_ms) VALUES('message','chat',0,'assistant','complete','Retained answer',1,2);
        INSERT INTO inference_attempts(id,operation_id,surface,provider,requested_model,prompt_template_version,status,queued_at_ms) VALUES('attempt','operation','standalone','agent-codex','default','v1','pending',1);
        INSERT INTO bot_chat_runs(ai_attempt_id,user_scope_id,owner_device_id,submission_id,bot_id,chat_id,assistant_message_id,run_id,record_workspace_id,created_at_ms) VALUES('attempt','scope','device','start-completed','bot','chat','message','run','home',1);
        INSERT INTO bot_run_replay(ai_attempt_id,updated_at_ms) VALUES('attempt',2);
        INSERT INTO bot_lifecycle_intents(user_scope_id,operation_id,bot_id,owner_device_id,kind,fingerprint,expected_revision,prior_lifecycle,pause_request_id,policy_request_id,policy_barrier_state,stage,requested_at_ms,updated_at_ms) VALUES('scope','archive','bot','device','archive','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',1,'active','pause-prepared','policy','pending','captured',1,2);").unwrap();
    c
}

pub(super) type Snapshot = BTreeMap<String, (String, Vec<String>)>;
pub(super) fn snapshot(c: &Connection) -> Snapshot {
    let objects:Vec<(String,String,String)> = c.prepare("SELECT type,name,coalesce(sql,'') FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name").unwrap().query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap().map(Result::unwrap).collect();
    let mut result = Snapshot::new();
    for (kind, name, sql) in objects {
        let mut rows = Vec::new();
        if kind == "table" {
            let mut q = c.prepare(&format!("SELECT * FROM \"{name}\"")).unwrap();
            let columns = q.column_count();
            let mut cursor = q.query([]).unwrap();
            while let Some(row) = cursor.next().unwrap() {
                rows.push(format!(
                    "{:?}",
                    (0..columns)
                        .map(|i| row.get::<_, Value>(i).unwrap())
                        .collect::<Vec<_>>()
                ));
            }
            rows.sort();
        }
        result.insert(
            name,
            (sql.chars().filter(|c| !c.is_whitespace()).collect(), rows),
        );
    }
    result
}

pub(super) fn foreign_keys_held(c: &Connection) {
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
            "UPDATE bots SET role_creation_request_id='missing' WHERE bot_id='bot'",
            []
        )
        .is_err()
    );
    assert!(
        c.execute("UPDATE bot_chat_runs SET submission_id='missing'", [])
            .is_err()
    );
    assert!(
        c.execute(
            "UPDATE bot_lifecycle_intents SET pause_request_id='missing'",
            []
        )
        .is_err()
    );
}

#[test]
fn ai19_preserves_populated_parent_and_children() {
    let mut c = populated_v18();
    println!("Rust-linked SQLite {}", rusqlite::version());
    let before = snapshot(&c);
    let ai19 = Schema {
        database: crate::DATABASE_FILE,
        understood: 19,
        migrations: &MIGRATIONS[..19],
    };
    koru_sqlite::prepare_with_preflight(&ai19, &mut c, refuse_superseded_18).unwrap();
    assert_eq!(user_version(&c).unwrap(), 19);
    let after = snapshot(&c);
    for (name, (sql, rows)) in before {
        let (new_sql, new_rows) = after.get(&name).unwrap();
        assert_eq!(&rows, new_rows, "rows changed: {name}");
        if name != "bot_command_receipts" {
            assert_eq!(&sql, new_sql, "definition changed: {name}");
        }
    }
    for table in ["bots", "bot_chat_runs", "bot_lifecycle_intents"] {
        let target:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_list(?1) WHERE \"table\"='bot_command_receipts')",[table],|r|r.get(0)).unwrap();
        assert!(target, "lost parent target in {table}");
    }
    foreign_keys_held(&c);
    let old = Schema {
        database: crate::DATABASE_FILE,
        understood: 18,
        migrations: &MIGRATIONS[..18],
    };
    assert!(matches!(
        koru_sqlite::prepare(&old, &mut c),
        Err(OpenError::SchemaTooNew {
            discovered: 19,
            supported: 18,
            ..
        })
    ));
}

#[test]
fn ai19_parent_rebuild_failure_rolls_back() {
    let original = MIGRATIONS[18].sql;
    let omitted=original.replace("created_at_ms,updated_at_ms FROM bot_command_receipts;","created_at_ms,updated_at_ms FROM bot_command_receipts WHERE request_id <> 'create-completed';");
    assert_ne!(omitted, original);
    for (sql, marker) in [
        (omitted, "CHECK constraint failed"),
        (
            format!("{original}\nSELECT * FROM injected_late_failure;"),
            "injected_late_failure",
        ),
    ] {
        let mut c = populated_v18();
        let before = snapshot(&c);
        let migration = Migration {
            to: 19,
            name: "019_failure_fixture.sql",
            sql: Box::leak(sql.into_boxed_str()),
            columns: &[],
        };
        let steps = Box::leak(vec![migration].into_boxed_slice());
        let schema = Schema {
            database: crate::DATABASE_FILE,
            understood: 19,
            migrations: steps,
        };
        let error = koru_sqlite::prepare(&schema, &mut c).unwrap_err();
        assert!(error.to_string().contains(marker), "{error}");
        assert_eq!(user_version(&c).unwrap(), 18);
        assert_eq!(snapshot(&c), before);
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM sqlite_temp_master WHERE name='bot_ai19_fk_guard'",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
            0
        );
        foreign_keys_held(&c);
    }
}
