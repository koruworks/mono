// SPDX-License-Identifier: MPL-2.0
use super::*;
use rusqlite::params;

fn upgraded() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    for step in MIGRATIONS.iter().filter(|m| m.to <= 4) {
        db.execute_batch(step.sql).unwrap();
    }
    db.execute("INSERT INTO run_intents(run_id,workspace_id,payload_json,state,created_at_ms,updated_at_ms) VALUES('old','workspace','{}','pending',1,1)",[]).unwrap();
    db.execute_batch(MIGRATIONS.iter().find(|m| m.to == 5).unwrap().sql)
        .unwrap();
    db
}
fn insert(db: &Connection, run: &str, sequence: i64, state: &str) -> rusqlite::Result<usize> {
    db.execute("INSERT OR IGNORE INTO run_intents(run_id,workspace_id,payload_json,state,created_at_ms,updated_at_ms) VALUES(?1,'workspace','{}','pending',1,1)",[run])?;
    db.execute("INSERT INTO bot_admissions(run_id,queue_sequence,user_scope,device_id,bot_id,request_id,config_revision,policy_revision,payload_json,state,created_at_ms,updated_at_ms) VALUES(?1,?2,'owner','device','bot',?1,1,1,'{}',?3,1,1)",params![run,sequence,state])
}
#[test]
fn bot_migration_preserves_old_intents_and_guards_execution_uniqueness() {
    let db = upgraded();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM run_intents WHERE run_id='old'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    insert(&db, "one", 1, "reserved").unwrap();
    assert!(insert(&db, "two", 2, "executing").is_err());
    insert(&db, "three", 3, "queued").unwrap();
    assert!(insert(&db, "four", 3, "queued").is_err());
    assert!(
        db.execute(
            "UPDATE bot_admissions SET payload_json='not-json' WHERE run_id='one'",
            []
        )
        .is_err()
    );
    assert!(
        db.execute(
            "UPDATE bot_admissions SET state='invented' WHERE run_id='one'",
            []
        )
        .is_err()
    );
}
#[test]
fn bot_migration_refuses_partial_delivery_and_send_correlation() {
    let db = upgraded();
    insert(&db, "one", 1, "queued").unwrap();
    for sql in [
        "UPDATE bot_admissions SET delivery_outcome='confirmed'",
        "UPDATE bot_admissions SET delivery_chat_id='partial'",
        "UPDATE bot_admissions SET initial_send_key='partial'",
        "UPDATE bot_admissions SET initial_send_turn_id='orphan'",
    ] {
        assert!(db.execute(sql, []).is_err());
    }
    db.execute("UPDATE bot_admissions SET delivery_outcome='confirmed',delivery_attempt_id='a',delivery_generation=1,delivery_chat_id='c',delivery_ai_attempt_id='i',delivery_assistant_message_id='m',delivery_confirmed_at_ms=2",[]).unwrap();
    db.execute("UPDATE bot_admissions SET initial_send_key='key',initial_send_fingerprint=zeroblob(32),initial_send_attempt_id='a',initial_send_generation=1",[]).unwrap();
}
#[test]
fn bot_migration_barrier_needs_no_placeholder_run() {
    let db = upgraded();
    db.execute(
        "INSERT INTO bot_admission_barriers VALUES('owner','device','bot',2,1,'request',1,1)",
        [],
    )
    .unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM bot_admissions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(
        db.execute("UPDATE bot_admission_barriers SET epoch=0", [])
            .is_err()
    );
    assert!(
        db.execute("UPDATE bot_admission_barriers SET paused=2", [])
            .is_err()
    );
}
