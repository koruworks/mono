// SPDX-License-Identifier: MPL-2.0

//! `ai.db`, opened through [`koru_sqlite`] — the one policy every Koru
//! database shares (stamp first, refuse a newer one, WAL and `FULL`,
//! forward-only steps).

use std::path::Path;

use koru_sqlite::{Migration, Schema};
use rusqlite::Connection;

pub use koru_sqlite::OpenError;

pub const SCHEMA_VERSION: i64 = 25;

pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        to: 1,
        name: "001_ai_initial_migration.sql",
        sql: include_str!("../migrations/001_ai_initial_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 2,
        name: "002_ai_context_refs_migration.sql",
        sql: include_str!("../migrations/002_ai_context_refs_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 3,
        name: "003_ai_workspace_tools_migration.sql",
        sql: include_str!("../migrations/003_ai_workspace_tools_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 4,
        name: "004_ai_chat_privacy_migration.sql",
        sql: include_str!("../migrations/004_ai_chat_privacy_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 5,
        name: "005_ai_web_search_migration.sql",
        sql: include_str!("../migrations/005_ai_web_search_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 6,
        name: "006_ai_tool_activity_context_migration.sql",
        sql: include_str!("../migrations/006_ai_tool_activity_context_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 7,
        name: "007_ai_agent_working_folder_migration.sql",
        sql: include_str!("../migrations/007_ai_agent_working_folder_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 8,
        name: "008_ai_agent_tool_content_migration.sql",
        sql: include_str!("../migrations/008_ai_agent_tool_content_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 9,
        name: "009_ai_message_attachments_migration.sql",
        sql: include_str!("../migrations/009_ai_message_attachments_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 10,
        name: "010_ai_tool_text_offset_migration.sql",
        sql: include_str!("../migrations/010_ai_tool_text_offset_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 11,
        name: "011_ai_agent_mode_migration.sql",
        sql: include_str!("../migrations/011_ai_agent_mode_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 12,
        name: "012_ai_agent_notices_and_pins_migration.sql",
        sql: include_str!("../migrations/012_ai_agent_notices_and_pins_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 13,
        name: "013_ai_chat_profile_migration.sql",
        sql: include_str!("../migrations/013_ai_chat_profile_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 14,
        name: "014_ai_attachment_references.sql",
        sql: include_str!("../migrations/014_ai_attachment_references.sql"),
        columns: &[],
    },
    Migration {
        to: 15,
        name: "015_ai_reported_context_usage.sql",
        sql: include_str!("../migrations/015_ai_reported_context_usage.sql"),
        columns: &[],
    },
    Migration {
        to: 16,
        name: "016_ai_bots.sql",
        sql: include_str!("../migrations/016_ai_bots.sql"),
        columns: &[],
    },
    Migration {
        to: 17,
        name: "017_ai_bot_run_replay.sql",
        sql: include_str!("../migrations/017_ai_bot_run_replay.sql"),
        columns: &[],
    },
    Migration {
        to: 18,
        name: "018_ai_bot_lifecycle.sql",
        sql: include_str!("../migrations/018_ai_bot_lifecycle.sql"),
        columns: &[],
    },
    Migration {
        to: 19,
        name: "019_ai_bot_library.sql",
        sql: include_str!("../migrations/019_ai_bot_library.sql"),
        columns: &[],
    },
    Migration {
        to: 20,
        name: "020_ai_bot_memory_identity.sql",
        sql: include_str!("../migrations/020_ai_bot_memory_identity.sql"),
        columns: &[],
    },
    Migration {
        to: 21,
        name: "021_ai_bot_routines.sql",
        sql: include_str!("../migrations/021_ai_bot_routines.sql"),
        columns: &[],
    },
    Migration {
        to: 22,
        name: "022_ai_bot_decisions.sql",
        sql: include_str!("../migrations/022_ai_bot_decisions.sql"),
        columns: &[],
    },
    Migration {
        to: 23,
        name: "023_ai_bot_routine_controls.sql",
        sql: include_str!("../migrations/023_ai_bot_routine_controls.sql"),
        columns: &[],
    },
    Migration {
        to: 24,
        name: "024_ai_bot_avatar.sql",
        sql: include_str!("../migrations/024_ai_bot_avatar.sql"),
        columns: &[],
    },
    Migration {
        to: 25,
        name: "025_ai_bot_connections.sql",
        sql: include_str!("../migrations/025_ai_bot_connections.sql"),
        columns: &[],
    },
];

const SCHEMA: Schema = Schema {
    database: crate::DATABASE_FILE,
    understood: SCHEMA_VERSION,
    migrations: MIGRATIONS,
};

pub(crate) fn open(path: &Path) -> Result<Connection, OpenError> {
    let connection = koru_sqlite::open_with_preflight(&SCHEMA, path, refuse_superseded_18)?;
    refuse_superseded_18(&connection)?;
    Ok(connection)
}

#[cfg(any(test, feature = "fixtures"))]
pub(crate) fn open_in_memory() -> Result<Connection, OpenError> {
    let connection = koru_sqlite::open_in_memory(&SCHEMA)?;
    refuse_superseded_18(&connection)?;
    Ok(connection)
}

/// The whole open sequence on a connection already opened.
#[cfg(test)]
pub(crate) fn prepare(connection: &mut Connection) -> Result<(), OpenError> {
    koru_sqlite::prepare_with_preflight(&SCHEMA, connection, refuse_superseded_18)?;
    refuse_superseded_18(connection)
}

/// The `operation` of the [`OpenError::Storage`] that refuses an `ai.db` an
/// unreleased development build left at schema 18 before that migration was
/// amended. The migrator tracks versions alone; the read-only preflight keeps
/// this unsupported shape from advancing to schema 19 before refusal. Without
/// it a Restore could later fail its CHECK when marking a source `reversed`.
/// No released build ever wrote schema 18, so this is never a user's
/// database: it is refused by this name, untouched, and recreated (delete
/// the file with its `-wal` and `-shm` while Koru is stopped). Nothing
/// repairs it in place: that would be a migration under a version that
/// already claims to be applied.
pub const SUPERSEDED_DEVELOPMENT_SCHEMA: &str = "open the superseded development schema of";

/// What the amended `018_ai_bot_lifecycle.sql` has and the version that
/// unreleased builds ran before it lacks: `(object type, name, a text its
/// stored definition contains)`.
const AMENDED_18: [(&str, &str, &str); 3] = [
    ("index", "bot_lifecycle_held", ""),
    ("index", "bot_chat_trash_owner", ""),
    ("table", "bot_lifecycle_intents", "'reversed'"),
];

/// Refuse, by [`SUPERSEDED_DEVELOPMENT_SCHEMA`], a database stamped 18 or
/// later that lacks the amended ai18's objects. Three `sqlite_master`
/// lookups at open; nothing is written. A later migration that drops or
/// rebuilds one of these objects must update [`AMENDED_18`]. Migration 023
/// rebuilds `bot_command_receipts` and `bot_lifecycle_members`, neither of
/// which is one of them.
fn refuse_superseded_18(connection: &Connection) -> Result<(), OpenError> {
    if user_version(connection)? < 18 {
        return Ok(());
    }
    let mut missing = Vec::new();
    for (kind, name, contains) in AMENDED_18 {
        let present: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type=?1 AND name=?2 AND instr(sql, ?3) > 0)",
                rusqlite::params![kind, name, contains],
                |row| row.get(0),
            )
            .map_err(|error| OpenError::Storage {
                database: crate::DATABASE_FILE,
                operation: "check the schema of",
                detail: error.to_string(),
            })?;
        if !present {
            missing.push(if contains.is_empty() {
                name.to_owned()
            } else {
                format!("{name} {contains}")
            });
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    Err(OpenError::Storage {
        database: crate::DATABASE_FILE,
        operation: SUPERSEDED_DEVELOPMENT_SCHEMA,
        detail: format!(
            "schema 18 was written by an unreleased development build before that migration was amended (missing: {}); no released build wrote it. Nothing was changed. Stop Koru and recreate this development database by deleting ai.db, ai.db-wal and ai.db-shm from the ai folder of the root that opened it",
            missing.join(", ")
        ),
    })
}

/// **An `ai.db` as an older build left it**, for a test above this crate
/// that opens it with this build: the file at `path`, migrated to `version`
/// and no further, with `sql` run in it. Behind the `fixtures` feature,
/// which only dev-dependencies turn on.
#[cfg(feature = "fixtures")]
pub fn write_at_schema(path: &Path, version: i64, sql: &str) -> Result<(), OpenError> {
    let steps = usize::try_from(version).unwrap_or(0).min(MIGRATIONS.len());
    let older = Schema {
        database: crate::DATABASE_FILE,
        understood: version,
        migrations: &MIGRATIONS[..steps],
    };
    let connection = koru_sqlite::open(&older, path)?;
    connection
        .execute_batch(sql)
        .map_err(|error| OpenError::Storage {
            database: crate::DATABASE_FILE,
            operation: "write a fixture",
            detail: error.to_string(),
        })
}

pub(crate) fn user_version(connection: &Connection) -> Result<i64, OpenError> {
    koru_sqlite::user_version(crate::DATABASE_FILE, connection)
}

#[cfg(test)]
mod tests;
