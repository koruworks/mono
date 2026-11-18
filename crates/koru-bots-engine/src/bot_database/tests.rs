// SPDX-License-Identifier: MPL-2.0
use super::*;
#[test]
fn resume_state_is_isolated_by_thread_and_session_and_tools_are_named() {
    let db = BotDatabase::in_memory().unwrap();
    let state = AgentState {
        thread: Uuid::new_v4(),
        session: Uuid::new_v4(),
        provider: "scripted".into(),
        resume_state: b"opaque".to_vec(),
        last_delivery: Some(Uuid::new_v4()),
        saved_at_ms: 3,
    };
    db.save(&state).unwrap();
    assert!(db.load(state.thread, state.session).unwrap().as_ref() == Some(&state));
    assert!(db.load(Uuid::new_v4(), state.session).unwrap().is_none());
    assert!(db.load(state.thread, Uuid::new_v4()).unwrap().is_none());
    db.journal(
        Uuid::new_v4(),
        JournalEntry::ToolCall {
            name: "page.read",
            payload: b"{}",
        },
        4,
    )
    .unwrap();
    let name: String = db
        .connection
        .query_row("SELECT tool_name FROM turn_journal", [], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "page.read");
}
#[test]
fn newer_bot_schema_is_refused_before_configuration() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    connection.execute_batch("PRAGMA user_version=2;").unwrap();
    assert!(matches!(
        koru_sqlite::prepare(&SCHEMA, &mut connection),
        Err(koru_sqlite::OpenError::SchemaTooNew { .. })
    ));
}
