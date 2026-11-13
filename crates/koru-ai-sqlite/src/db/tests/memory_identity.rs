// SPDX-License-Identifier: MPL-2.0
//! Real linked SQLite compatibility gate over populated ai19, without disk opens.
use super::library::{foreign_keys_held, populated_v18, snapshot};
use super::*;
use rusqlite::params;

fn ai19() -> Schema {
    Schema {
        database: crate::DATABASE_FILE,
        understood: 19,
        migrations: &MIGRATIONS[..19],
    }
}
fn populated_v19() -> Connection {
    let mut c = populated_v18();
    koru_sqlite::prepare_with_preflight(&ai19(), &mut c, refuse_superseded_18).unwrap();
    for state in ["prepared", "unconfirmed", "conflict", "abandoned", "bound"] {
        let bot = format!("memory-{state}");
        let request = uuid::Uuid::new_v4().to_string();
        let candidate = uuid::Uuid::new_v4().to_string();
        let stage = match state {
            "bound" => "completed",
            "conflict" | "abandoned" => "rejected",
            _ => "prepared",
        };
        c.execute("INSERT INTO bots(bot_id,user_scope_id,home_workspace_id,owner_device_id,name,role_workspace_id,role_document_id,working_folder,provider,model,reasoning,revision,policy_revision,access,created_at_ms,updated_at_ms) SELECT ?1,user_scope_id,home_workspace_id,owner_device_id,name,role_workspace_id,role_document_id,working_folder,provider,model,reasoning,revision,policy_revision,access,created_at_ms,updated_at_ms FROM bots WHERE bot_id='bot'",[&bot]).unwrap();
        let outcome=serde_json::json!({"kind":"ensure_memory","version":1,"creation_request":request,"candidate":{"workspace":"home","document":candidate}}).to_string();
        for receipt in [request.clone(), uuid::Uuid::new_v4().to_string()] {
            c.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,expected_revision,outcome_json,created_at_ms,updated_at_ms) VALUES('scope',?1,?2,'retained','ensure_memory',?3,1,?4,1,2)",params![receipt,bot,stage,outcome]).unwrap();
        }
        c.execute("INSERT INTO bot_memory_creation_intents(user_scope_id,request_id,bot_id,owner_device_id,workspace_id,candidate_document_id,store_request_id,seed_format,seed_title,seed_hash,state,origin_edit_revision,refusal,created_at_ms,updated_at_ms) VALUES('scope',?1,?2,'device','home',?3,?4,1,'Memory',zeroblob(32),?5,?6,?7,1,2)",params![request,bot,candidate,uuid::Uuid::new_v4().to_string(),state,(state=="bound").then_some(vec![1u8,2]),(state=="conflict").then_some("document_changed")]).unwrap();
        if state == "bound" {
            c.execute(
                "INSERT INTO bot_memory_bindings VALUES(?1,'scope','home',?2,?3,2)",
                params![bot, candidate, request],
            )
            .unwrap();
            c.execute(
                "INSERT INTO bot_knowledge_pages VALUES(?1,'home','knowledge',0,2)",
                [&bot],
            )
            .unwrap();
            c.execute("INSERT INTO bot_library_captures(user_scope_id,submission_request_id,bot_id,capture_version,config_revision,knowledge_count,memory_state,memory_workspace_id,memory_document_id,memory_history_version_id,memory_binding_request_id,index_digest,indexed_count,omitted_count,invalid_count,created_at_ms) VALUES('scope','start-completed',?1,1,1,1,'captured','home',?2,?3,?4,zeroblob(32),1,2,3,2)",params![bot,candidate,uuid::Uuid::new_v4().to_string(),request]).unwrap();
            c.execute("INSERT INTO bot_knowledge_captures VALUES('scope','start-completed',0,'home','knowledge',?1)",[uuid::Uuid::new_v4().to_string()]).unwrap();
            c.execute(
                "INSERT INTO bot_memory_index_nodes VALUES('scope','start-completed',0,?1)",
                [uuid::Uuid::new_v4().to_string()],
            )
            .unwrap();
        }
    }
    c
}

#[test]
fn ai20_preserves_populated_ai19() {
    let mut c = populated_v19();
    println!("Rust-linked SQLite {}", rusqlite::version());
    let before = snapshot(&c);
    prepare(&mut c).unwrap();
    assert_eq!(user_version(&c).unwrap(), 20);
    assert_eq!(snapshot(&c), before);
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
            "UPDATE bot_memory_bindings SET creation_request_id='missing'",
            []
        )
        .is_err()
    );
}

#[test]
fn ai19_refuses_ai20_before_callbacks_or_writes() {
    let mut c = populated_v19();
    prepare(&mut c).unwrap();
    let before = snapshot(&c);
    c.pragma_update(None, "foreign_keys", false).unwrap();
    c.pragma_update(None, "synchronous", 0).unwrap();
    let error = koru_sqlite::prepare_with_preflight(&ai19(), &mut c, |_| {
        panic!("older preflight must not run")
    })
    .unwrap_err();
    assert!(matches!(
        error,
        OpenError::SchemaTooNew {
            discovered: 20,
            supported: 19,
            ..
        }
    ));
    assert_eq!(user_version(&c).unwrap(), 20);
    assert_eq!(snapshot(&c), before);
    assert_eq!(
        c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        c.query_row("PRAGMA synchronous", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn ai20_gate_failure_rolls_back() {
    let mut c = populated_v19();
    let before = snapshot(&c);
    let sql = Box::leak(
        format!(
            "{}\nSELECT * FROM injected_ai20_failure;",
            MIGRATIONS[19].sql
        )
        .into_boxed_str(),
    );
    let steps = Box::leak(
        vec![Migration {
            to: 20,
            name: "020_fixture.sql",
            sql,
            columns: &[],
        }]
        .into_boxed_slice(),
    );
    let schema = Schema {
        database: crate::DATABASE_FILE,
        understood: 20,
        migrations: steps,
    };
    assert!(
        koru_sqlite::prepare(&schema, &mut c)
            .unwrap_err()
            .to_string()
            .contains("injected_ai20_failure")
    );
    assert_eq!(user_version(&c).unwrap(), 19);
    assert_eq!(snapshot(&c), before);
    foreign_keys_held(&c);
}
