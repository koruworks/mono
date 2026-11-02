// SPDX-License-Identifier: MPL-2.0

//! `ai.db`, opened through [`koru_sqlite`] — the one policy every Koru
//! database shares (stamp first, refuse a newer one, WAL and `FULL`,
//! forward-only steps).

use std::path::Path;

use koru_sqlite::{Migration, Schema};
use rusqlite::Connection;

pub use koru_sqlite::OpenError;

pub const SCHEMA_VERSION: i64 = 11;

const MIGRATIONS: &[Migration] = &[
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
];

const SCHEMA: Schema = Schema {
    database: crate::DATABASE_FILE,
    understood: SCHEMA_VERSION,
    migrations: MIGRATIONS,
};

pub(crate) fn open(path: &Path) -> Result<Connection, OpenError> {
    koru_sqlite::open(&SCHEMA, path)
}

#[cfg(test)]
pub(crate) fn open_in_memory() -> Result<Connection, OpenError> {
    koru_sqlite::open_in_memory(&SCHEMA)
}

/// The whole open sequence on a connection already opened.
#[cfg(test)]
pub(crate) fn prepare(connection: &mut Connection) -> Result<(), OpenError> {
    koru_sqlite::prepare(&SCHEMA, connection)
}

pub(crate) fn user_version(connection: &Connection) -> Result<i64, OpenError> {
    koru_sqlite::user_version(crate::DATABASE_FILE, connection)
}

#[cfg(test)]
mod tests;
