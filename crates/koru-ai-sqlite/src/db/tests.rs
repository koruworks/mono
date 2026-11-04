// SPDX-License-Identifier: MPL-2.0

use rusqlite::Connection;

use super::*;

#[test]
fn fresh_database_applies_all_numbered_ai_migrations() {
    let connection = open_in_memory().unwrap();
    assert_eq!(user_version(&connection).unwrap(), SCHEMA_VERSION);

    for object in [
        "ai_settings",
        "chats",
        "messages",
        "chat_documents",
        "message_document_refs",
        "message_block_refs",
        "message_blocks",
        "message_blocks_fts",
        "inference_attempts",
        "feedback_events",
        "document_exports",
        "chat_workspace_grants",
        "tool_invocations",
        "message_citations",
        "current_message_feedback",
        "inference_attempt_durations",
        "usage_by_day",
        "response_quality_by_model",
        "message_attachments",
        "message_attachments_bound_are_immutable",
        "agent_notices",
    ] {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = ?1)",
                [object],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists, "missing schema object {object}");
    }
}

#[test]
fn migration_is_idempotent() {
    let mut connection = open_in_memory().unwrap();
    prepare(&mut connection).unwrap();
    assert_eq!(user_version(&connection).unwrap(), SCHEMA_VERSION);
}

#[test]
fn the_v0_fixture_upgrades_through_production_migration_code() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(include_str!("../fixtures/ai-v0.sql"))
        .unwrap();
    assert_eq!(user_version(&connection).unwrap(), 0);

    prepare(&mut connection).unwrap();
    assert_eq!(user_version(&connection).unwrap(), SCHEMA_VERSION);
    let chats: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'chats')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(chats);
}

#[test]
fn the_v0_fixture_gains_a_null_working_folder_for_every_chat() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(include_str!("../fixtures/ai-v0.sql"))
        .unwrap();
    prepare(&mut connection).unwrap();
    let has_column: bool = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM pragma_table_info('chats') WHERE name = 'agent_working_folder'
             )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(has_column, "schema 7 adds the working folder column");
    let all_null: bool = connection
        .query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM chats WHERE agent_working_folder IS NOT NULL)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(all_null, "every earlier chat keeps the home-folder default");
}

#[test]
fn the_v0_fixture_gains_null_agent_words_on_every_tool_row() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(include_str!("../fixtures/ai-v0.sql"))
        .unwrap();
    prepare(&mut connection).unwrap();
    for column in [
        "agent_label",
        "agent_summary",
        "agent_input",
        "agent_output",
    ] {
        let has_column: bool = connection
            .query_row(
                &format!(
                    "SELECT EXISTS(
                        SELECT 1 FROM pragma_table_info('tool_invocations') WHERE name = '{column}'
                     )"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(has_column, "schema 8 adds {column}");
        let all_null: bool = connection
            .query_row(
                &format!(
                    "SELECT NOT EXISTS(SELECT 1 FROM tool_invocations WHERE {column} IS NOT NULL)"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(all_null, "every earlier row reads by kind and outcome");
    }
    // The ceiling holds: a label past it is refused, not truncated.
    let refused = connection.execute(
        "UPDATE tool_invocations SET agent_label = ?1",
        [&"x".repeat(1_001)],
    );
    assert!(
        refused.is_err() || refused.unwrap() == 0,
        "the column's check is the ceiling"
    );
}

#[test]
fn a_v1_database_preserves_rows_and_adds_message_block_provenance() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(include_str!(
            "../../migrations/001_ai_initial_migration.sql"
        ))
        .unwrap();
    connection.pragma_update(None, "user_version", 1).unwrap();
    connection
        .execute(
            "INSERT INTO chats (
                id, title, current_provider, current_model, created_at_ms, updated_at_ms
             ) VALUES ('chat', 'kept', 'mock', 'model', 1, 1)",
            [],
        )
        .unwrap();

    prepare(&mut connection).unwrap();

    assert_eq!(user_version(&connection).unwrap(), SCHEMA_VERSION);
    let title: String = connection
        .query_row("SELECT title FROM chats WHERE id = 'chat'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(title, "kept");
    let provenance_exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'message_block_refs')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(provenance_exists);
}

#[test]
fn a_v3_database_preserves_chats_and_defaults_them_to_public() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(include_str!(
            "../../migrations/001_ai_initial_migration.sql"
        ))
        .unwrap();
    connection
        .execute_batch(include_str!(
            "../../migrations/002_ai_context_refs_migration.sql"
        ))
        .unwrap();
    connection
        .execute_batch(include_str!(
            "../../migrations/003_ai_workspace_tools_migration.sql"
        ))
        .unwrap();
    connection.pragma_update(None, "user_version", 3).unwrap();
    connection
        .execute(
            "INSERT INTO chats (
                id, title, current_provider, current_model, created_at_ms, updated_at_ms
             ) VALUES ('chat', 'kept', 'mock', 'model', 1, 1)",
            [],
        )
        .unwrap();

    prepare(&mut connection).unwrap();

    assert_eq!(user_version(&connection).unwrap(), SCHEMA_VERSION);
    let (title, private): (String, bool) = connection
        .query_row(
            "SELECT title, private FROM chats WHERE id = 'chat'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(title, "kept");
    assert!(!private);
}

#[test]
fn a_v4_database_adds_search_accounting_and_citations_without_changing_rows() {
    let mut connection = Connection::open_in_memory().unwrap();
    for migration in [
        include_str!("../../migrations/001_ai_initial_migration.sql"),
        include_str!("../../migrations/002_ai_context_refs_migration.sql"),
        include_str!("../../migrations/003_ai_workspace_tools_migration.sql"),
        include_str!("../../migrations/004_ai_chat_privacy_migration.sql"),
    ] {
        connection.execute_batch(migration).unwrap();
    }
    connection.pragma_update(None, "user_version", 4).unwrap();
    connection
        .execute(
            "INSERT INTO chats (
                id, title, current_provider, current_model, created_at_ms, updated_at_ms
             ) VALUES ('chat', 'kept', 'openai', 'gpt-5.4', 1, 1)",
            [],
        )
        .unwrap();

    prepare(&mut connection).unwrap();

    assert_eq!(user_version(&connection).unwrap(), SCHEMA_VERSION);
    let title: String = connection
        .query_row("SELECT title FROM chats WHERE id = 'chat'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(title, "kept");
    for column in ["web_search_requested", "web_search_calls"] {
        let exists = connection
            .prepare("PRAGMA table_info(inference_attempts)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .iter()
            .any(|name| name == column);
        assert!(exists, "missing inference_attempts.{column}");
    }
}

#[test]
fn a_v5_database_adds_minimal_tool_context_columns() {
    let mut connection = Connection::open_in_memory().unwrap();
    for migration in [
        include_str!("../../migrations/001_ai_initial_migration.sql"),
        include_str!("../../migrations/002_ai_context_refs_migration.sql"),
        include_str!("../../migrations/003_ai_workspace_tools_migration.sql"),
        include_str!("../../migrations/004_ai_chat_privacy_migration.sql"),
        include_str!("../../migrations/005_ai_web_search_migration.sql"),
    ] {
        connection.execute_batch(migration).unwrap();
    }
    connection.pragma_update(None, "user_version", 5).unwrap();

    prepare(&mut connection).unwrap();

    assert_eq!(user_version(&connection).unwrap(), SCHEMA_VERSION);
    let columns = connection
        .prepare("PRAGMA table_info(tool_invocations)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for column in ["target_workspace_id", "target_document_id", "error_message"] {
        assert!(columns.iter().any(|candidate| candidate == column));
    }
}

#[test]
fn a_v8_database_with_chats_gains_attachments_and_reads_as_before() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    for migration in &MIGRATIONS[..8] {
        connection.execute_batch(migration.sql).unwrap();
    }
    connection.pragma_update(None, "user_version", 8).unwrap();
    connection
        .execute(
            "INSERT INTO chats (
                id, title, current_provider, current_model, created_at_ms, updated_at_ms
             ) VALUES ('chat', 'kept', 'agent-claude', 'default', 1, 1)",
            [],
        )
        .unwrap();

    prepare(&mut connection).unwrap();
    assert_eq!(user_version(&connection).unwrap(), SCHEMA_VERSION);
    let title: String = connection
        .query_row("SELECT title FROM chats WHERE id = 'chat'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(title, "kept");
    let attachments: i64 = connection
        .query_row("SELECT COUNT(*) FROM message_attachments", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(attachments, 0, "an existing chat has no attachment");

    // Reopened, it is already current and nothing runs again.
    prepare(&mut connection).unwrap();
    assert_eq!(user_version(&connection).unwrap(), SCHEMA_VERSION);
}

#[test]
fn a_build_that_understands_only_schema_8_refuses_schema_9() {
    let mut connection = open_in_memory().unwrap();
    let older = Schema {
        database: crate::DATABASE_FILE,
        understood: 8,
        migrations: &MIGRATIONS[..8],
    };
    assert_eq!(
        koru_sqlite::prepare(&older, &mut connection).unwrap_err(),
        OpenError::SchemaTooNew {
            database: crate::DATABASE_FILE,
            discovered: SCHEMA_VERSION,
            supported: 8,
        }
    );
}

#[test]
fn a_build_that_understands_only_schema_9_refuses_schema_10() {
    let mut connection = open_in_memory().unwrap();
    let older = Schema {
        database: crate::DATABASE_FILE,
        understood: 9,
        migrations: &MIGRATIONS[..9],
    };
    assert_eq!(
        koru_sqlite::prepare(&older, &mut connection).unwrap_err(),
        OpenError::SchemaTooNew {
            database: crate::DATABASE_FILE,
            discovered: SCHEMA_VERSION,
            supported: 9,
        }
    );
}

#[test]
fn a_sent_attachment_row_cannot_change() {
    let connection = open_in_memory().unwrap();
    connection
        .execute_batch(
            "INSERT INTO chats (id, title, current_provider, current_model, created_at_ms, updated_at_ms)
                 VALUES ('chat', 't', 'agent-claude', 'default', 1, 1);
             INSERT INTO messages (id, chat_id, ordinal, role, state, markdown, created_at_ms, completed_at_ms)
                 VALUES ('message', 'chat', 0, 'user', 'complete', 'hi', 1, 1);",
        )
        .unwrap();
    let hash = "0".repeat(64);
    connection
        .execute(
            "INSERT INTO message_attachments (id, chat_id, kind, file_name, media_type,
                byte_count, width, height, source, content_blake3, created_at_ms)
             VALUES ('a', 'chat', 'image', 'shot.png', 'image/png', 10, 4, 3, 'clipboard', ?1, 1)",
            [&hash],
        )
        .unwrap();
    // Staged, it may be bound once.
    connection
        .execute(
            "UPDATE message_attachments SET message_id = 'message', ordinal = 0, bound_at_ms = 2
              WHERE id = 'a'",
            [],
        )
        .unwrap();
    assert!(
        connection
            .execute(
                "UPDATE message_attachments SET file_name = 'other.png' WHERE id = 'a'",
                [],
            )
            .is_err()
    );
    // A picture without dimensions, or a workspace copy without its origin,
    // is not a row.
    assert!(
        connection
            .execute(
                "INSERT INTO message_attachments (id, chat_id, kind, file_name, media_type,
                    byte_count, source, content_blake3, created_at_ms)
                 VALUES ('b', 'chat', 'image', 'x.png', 'image/png', 1, 'clipboard', ?1, 1)",
                [&hash],
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "INSERT INTO message_attachments (id, chat_id, kind, file_name, media_type,
                    byte_count, source, content_blake3, created_at_ms)
                 VALUES ('c', 'chat', 'file', 'x.pdf', 'application/pdf', 1, 'workspace', ?1, 1)",
                [&hash],
            )
            .is_err()
    );
    // Deleting the chat removes its attachments.
    connection
        .execute("DELETE FROM chats WHERE id = 'chat'", [])
        .unwrap();
    let left: i64 = connection
        .query_row("SELECT COUNT(*) FROM message_attachments", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(left, 0);
}

#[test]
fn newer_schema_is_refused_before_persistent_pragmas_or_rows_are_changed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ai.db");
    {
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(&format!(
                "PRAGMA journal_mode = DELETE;
                 CREATE TABLE sentinel (value TEXT NOT NULL);
                 INSERT INTO sentinel VALUES ('untouched');
                 PRAGMA user_version = {};",
                SCHEMA_VERSION + 1
            ))
            .unwrap();
    }

    assert_eq!(
        open(&path).unwrap_err(),
        OpenError::SchemaTooNew {
            database: crate::DATABASE_FILE,
            discovered: SCHEMA_VERSION + 1,
            supported: SCHEMA_VERSION,
        }
    );

    let connection = Connection::open(&path).unwrap();
    let journal: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    let sentinel: String = connection
        .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal, "delete");
    assert_eq!(sentinel, "untouched");
}

#[test]
fn a_real_database_is_wal_full_and_foreign_keyed() {
    let directory = tempfile::tempdir().unwrap();
    let connection = open(&directory.path().join("nested").join("ai.db")).unwrap();

    let journal: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    let synchronous: i64 = connection
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .unwrap();
    let foreign_keys: bool = connection
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();

    assert_eq!(journal, "wal");
    assert_eq!(synchronous, 2, "durable chat commits require FULL");
    assert!(foreign_keys);
}

#[test]
fn an_empty_database_has_the_text_offset_column() {
    let connection = open_in_memory().unwrap();
    assert_eq!(user_version(&connection).unwrap(), SCHEMA_VERSION);
    let has_column: bool = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM pragma_table_info('tool_invocations') WHERE name = 'text_offset'
             )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(has_column, "schema 10 adds text_offset");
}

#[test]
fn a_v9_database_keeps_its_tool_rows_with_no_text_offset() {
    const V9: Schema = Schema {
        database: crate::DATABASE_FILE,
        understood: 9,
        migrations: MIGRATIONS.split_at(9).0,
    };
    let mut connection = koru_sqlite::open_in_memory(&V9).unwrap();
    assert_eq!(user_version(&connection).unwrap(), 9);
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO tool_invocations (
                 id, attempt_id, chat_id, tool_name, read_only, status, approval,
                 proposed_at_ms, started_at_ms, completed_at_ms, agent_label
             ) VALUES ('row', 'attempt', 'chat', 'agent.other', 0, 'complete',
                       'not_required', 1, 1, 2, 'Read a.txt');
             PRAGMA foreign_keys = ON;",
        )
        .unwrap();

    prepare(&mut connection).unwrap();

    assert_eq!(user_version(&connection).unwrap(), SCHEMA_VERSION);
    let (label, offset): (String, Option<i64>) = connection
        .query_row(
            "SELECT agent_label, text_offset FROM tool_invocations WHERE id = 'row'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(label, "Read a.txt");
    assert_eq!(offset, None, "an earlier row folds its steps only");
    let refused = connection.execute("UPDATE tool_invocations SET text_offset = -1", []);
    assert!(refused.is_err(), "an offset is never negative");
}

#[test]
fn a_build_that_understands_only_schema_10_refuses_schema_11() {
    let mut connection = open_in_memory().unwrap();
    let older = Schema {
        database: crate::DATABASE_FILE,
        understood: 10,
        migrations: &MIGRATIONS[..10],
    };
    assert_eq!(
        koru_sqlite::prepare(&older, &mut connection).unwrap_err(),
        OpenError::SchemaTooNew {
            database: crate::DATABASE_FILE,
            discovered: SCHEMA_VERSION,
            supported: 10,
        }
    );
}

/// **Schema 11** (koru-ai D7, D11): an empty database opens with both of a
/// chat's agent settings, null until a chat chooses.
#[test]
fn an_empty_database_opens_at_schema_11_with_the_agent_mode_columns() {
    let connection = open_in_memory().unwrap();
    assert!(user_version(&connection).unwrap() >= 11);
    for column in ["agent_mode", "agent_fast"] {
        let has_column: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM pragma_table_info('chats') WHERE name = ?1
                 )",
                [column],
                |row| row.get(0),
            )
            .unwrap();
        assert!(has_column, "schema 11 adds {column}");
    }
}

/// A schema-10 database keeps its chats, each with no mode and no Fast
/// choice — the agent's own defaults — and a mode is any token.
#[test]
fn a_v10_database_keeps_its_chats_with_no_agent_mode() {
    const V10: Schema = Schema {
        database: crate::DATABASE_FILE,
        understood: 10,
        migrations: MIGRATIONS.split_at(10).0,
    };
    let mut connection = koru_sqlite::open_in_memory(&V10).unwrap();
    assert_eq!(user_version(&connection).unwrap(), 10);
    connection
        .execute(
            "INSERT INTO chats (
                id, title, current_provider, current_model, created_at_ms, updated_at_ms
             ) VALUES ('chat', 'kept', 'agent-claude', 'default', 1, 1)",
            [],
        )
        .unwrap();

    prepare(&mut connection).unwrap();

    assert_eq!(user_version(&connection).unwrap(), SCHEMA_VERSION);
    let (title, mode, fast): (String, Option<String>, Option<bool>) = connection
        .query_row(
            "SELECT title, agent_mode, agent_fast FROM chats WHERE id = 'chat'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(title, "kept");
    assert_eq!((mode, fast), (None, None));
    connection
        .execute(
            "UPDATE chats SET agent_mode = 'acceptEdits', agent_fast = 1",
            [],
        )
        .unwrap();
    let refused = connection.execute("UPDATE chats SET agent_fast = 2", []);
    assert!(refused.is_err(), "Fast is on or off");

    // Reopened, it is already current and nothing runs again.
    prepare(&mut connection).unwrap();
    assert_eq!(user_version(&connection).unwrap(), SCHEMA_VERSION);
}

/// **An older build refuses schema 12 cleanly** (koru-ai D69, pinned
/// chats): a build that understands 11 meets a database this build has
/// moved on and opens nothing.
#[test]
fn a_build_that_understands_only_schema_11_refuses_schema_12() {
    let mut connection = open_in_memory().unwrap();
    let older = Schema {
        database: crate::DATABASE_FILE,
        understood: 11,
        migrations: &MIGRATIONS[..11],
    };
    assert_eq!(
        koru_sqlite::prepare(&older, &mut connection).unwrap_err(),
        OpenError::SchemaTooNew {
            database: crate::DATABASE_FILE,
            discovered: SCHEMA_VERSION,
            supported: 11,
        }
    );
}

/// **Schema 12** (koru-ai D69, pinned chats): a schema-11 database keeps
/// its chats, none of them pinned, and gains an empty notice table whose
/// words and offset are checked.
#[test]
fn a_v11_database_keeps_its_chats_unpinned_and_gains_agent_notices() {
    const V11: Schema = Schema {
        database: crate::DATABASE_FILE,
        understood: 11,
        migrations: MIGRATIONS.split_at(11).0,
    };
    let mut connection = koru_sqlite::open_in_memory(&V11).unwrap();
    assert_eq!(user_version(&connection).unwrap(), 11);
    connection
        .execute(
            "INSERT INTO chats (
                id, title, current_provider, current_model, created_at_ms, updated_at_ms,
                agent_mode
             ) VALUES ('chat', 'kept', 'agent-devin', 'default', 1, 1, 'plan')",
            [],
        )
        .unwrap();

    prepare(&mut connection).unwrap();

    assert_eq!(user_version(&connection).unwrap(), SCHEMA_VERSION);
    let (title, mode, pinned): (String, Option<String>, Option<i64>) = connection
        .query_row(
            "SELECT title, agent_mode, pinned_at_ms FROM chats WHERE id = 'chat'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (title.as_str(), mode.as_deref(), pinned),
        ("kept", Some("plan"), None)
    );
    connection
        .execute("UPDATE chats SET pinned_at_ms = 5", [])
        .unwrap();
    assert!(
        connection
            .execute("UPDATE chats SET pinned_at_ms = -1", [])
            .is_err(),
        "a pin time is never negative"
    );
    let notices: i64 = connection
        .query_row("SELECT count(*) FROM agent_notices", [], |row| row.get(0))
        .unwrap();
    assert_eq!(notices, 0);
    connection
        .execute_batch(
            "INSERT INTO inference_attempts (
                id, operation_id, chat_id, surface, provider, requested_model,
                prompt_template_version, status, queued_at_ms
             ) VALUES ('attempt', 'op', 'chat', 'standalone', 'agent-devin', 'default',
                       'v1', 'pending', 1);",
        )
        .unwrap();
    let insert = |text: &str, offset: i64| {
        connection.execute(
            "INSERT INTO agent_notices (id, attempt_id, chat_id, text, text_offset, created_at_ms)
             VALUES (?1, 'attempt', 'chat', ?2, ?3, 2)",
            rusqlite::params![format!("n{offset}{}", text.len()), text, offset],
        )
    };
    assert!(insert("", 0).is_err(), "a notice has words");
    assert!(
        insert("Hook failed", -1).is_err(),
        "an offset is never negative"
    );
    insert("Hook failed", 0).unwrap();
}

/// **An older build refuses schema 13 cleanly** (koru-ai D120): a build
/// that understands 12 meets a database this build has moved on and opens
/// nothing — its AI service does not start.
#[test]
fn a_build_that_understands_only_schema_12_refuses_schema_13() {
    let mut connection = open_in_memory().unwrap();
    let older = Schema {
        database: crate::DATABASE_FILE,
        understood: 12,
        migrations: &MIGRATIONS[..12],
    };
    assert_eq!(
        koru_sqlite::prepare(&older, &mut connection).unwrap_err(),
        OpenError::SchemaTooNew {
            database: crate::DATABASE_FILE,
            discovered: SCHEMA_VERSION,
            supported: 12,
        }
    );
}

/// **Schema 13** (koru-ai D120): a schema-12 database keeps every chat and
/// all it holds, each with no profile yet — the AI service places them, as
/// the database cannot — and a profile is never an empty string.
#[test]
fn a_v12_database_keeps_its_chats_with_no_profile_yet() {
    const V12: Schema = Schema {
        database: crate::DATABASE_FILE,
        understood: 12,
        migrations: MIGRATIONS.split_at(12).0,
    };
    let mut connection = koru_sqlite::open_in_memory(&V12).unwrap();
    assert_eq!(user_version(&connection).unwrap(), 12);
    connection
        .execute_batch(
            "INSERT INTO chats (
                id, title, current_provider, current_model, created_at_ms, updated_at_ms,
                pinned_at_ms
             ) VALUES ('pinned', 'kept', 'mock', 'default', 1, 3, 2),
                      ('plain', 'also kept', 'mock', 'default', 1, 1, NULL);
             INSERT INTO chat_workspace_grants (chat_id, workspace_id, granted_at_ms)
             VALUES ('plain', 'workspace', 1);",
        )
        .unwrap();

    prepare(&mut connection).unwrap();

    assert_eq!(user_version(&connection).unwrap(), SCHEMA_VERSION);
    let rows: Vec<(String, String, Option<i64>, Option<String>)> = connection
        .prepare("SELECT id, title, pinned_at_ms, profile_id FROM chats ORDER BY id")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("pinned".into(), "kept".into(), Some(2), None),
            ("plain".into(), "also kept".into(), None, None),
        ]
    );
    let grants: i64 = connection
        .query_row("SELECT count(*) FROM chat_workspace_grants", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(grants, 1, "nothing is deleted");
    assert!(
        connection
            .execute("UPDATE chats SET profile_id = '' WHERE id = 'plain'", [])
            .is_err(),
        "a profile is an id, never empty"
    );
    let indexed: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'chats_by_profile')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(indexed);

    // Reopened, it is already current and nothing runs again.
    prepare(&mut connection).unwrap();
    assert_eq!(user_version(&connection).unwrap(), SCHEMA_VERSION);
}
