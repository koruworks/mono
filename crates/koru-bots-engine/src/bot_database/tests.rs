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
fn repeated_resumes_keep_bounded_state_and_never_prune_the_journal_or_other_thread() {
    let db = BotDatabase::in_memory().unwrap();
    let thread = Uuid::new_v4();
    let other = Uuid::new_v4();
    let mut state = AgentState {
        thread: other,
        session: Uuid::new_v4(),
        provider: "scripted".into(),
        resume_state: vec![1; 4096],
        last_delivery: None,
        saved_at_ms: 0,
    };
    db.save(&state).unwrap();
    for i in 1..65 {
        state.thread = thread;
        state.session = Uuid::new_v4();
        state.saved_at_ms = i;
        db.save(&state).unwrap();
        db.journal(Uuid::new_v4(), JournalEntry::Input(b"fixture"), i)
            .unwrap();
    }
    assert_eq!(
        db.connection
            .query_row("SELECT count(*) FROM agent_state", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        db.connection
            .query_row("SELECT count(*) FROM turn_journal", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        64
    );
    assert!(db.latest(thread, "scripted").unwrap().as_ref() == Some(&state));
    assert!(db.latest(other, "scripted").unwrap().is_some());
    db.connection.execute_batch("CREATE TEMP TRIGGER fail_prune BEFORE DELETE ON agent_state BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let mut next = state.clone();
    next.session = Uuid::new_v4();
    next.saved_at_ms = 65;
    assert!(db.save(&next).is_err());
    assert!(db.latest(thread, "scripted").unwrap().as_ref() == Some(&state));
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
