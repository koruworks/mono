// SPDX-License-Identifier: MPL-2.0

use std::path::Path;

use koru_sqlite::{Migration, Schema};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::Error;

pub const DATABASE_NAME: &str = "engine.db";
const SCHEMA: Schema = Schema {
    database: DATABASE_NAME,
    understood: 3,
    migrations: &[
        Migration {
            to: 1,
            name: "001_engine.sql",
            sql: include_str!("../migrations/001_engine.sql"),
            columns: &[],
        },
        Migration {
            to: 2,
            name: "002_routines.sql",
            sql: include_str!("../migrations/002_routines.sql"),
            columns: &[],
        },
        Migration {
            to: 3,
            name: "003_routine_review.sql",
            sql: include_str!("../migrations/003_routine_review.sql"),
            columns: &[],
        },
    ],
};

/// Owned by the engine actor. No connection is shared with a Bot worker.
pub struct Database {
    pub(crate) connection: Connection,
}

impl Database {
    pub fn open(root: &Path) -> Result<Self, Error> {
        Ok(Self {
            connection: koru_sqlite::open(&SCHEMA, &root.join("bots").join(DATABASE_NAME))?,
        })
    }

    /// Same schema and durability configuration, with no filesystem effects.
    pub fn in_memory() -> Result<Self, Error> {
        Ok(Self {
            connection: koru_sqlite::open_in_memory(&SCHEMA)?,
        })
    }

    pub fn status(&self) -> Result<Status, Error> {
        status(&self.connection)
    }
}

/// Counts and timestamps only. Names, credentials and conversation content
/// have no representation in a diagnostic response.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub awake_bots: u64,
    pub running_turns: u64,
    pub queued_turns: u64,
    pub inbox_depth: u64,
    pub workers: Vec<WorkerStatus>,
    /// The next scheduled occurrence; None when every Routine is off.
    pub next_routine_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerStatus {
    pub bot: String,
    pub incarnation: String,
    pub started_at_ms: i64,
    pub last_heard_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub schema_version: i64,
    pub supported_version: i64,
    /// A newer or otherwise unreadable schema still reports its version.
    pub status: Option<Status>,
    pub status_error: Option<Error>,
}

/// Opens read-only, never creates a directory, migrates or contacts a worker.
/// The diagnostic remains useful when the worker refuses a newer schema.
pub fn inspect(root: &Path) -> Result<Diagnostic, Error> {
    let connection = Connection::open_with_flags(
        root.join("bots").join(DATABASE_NAME),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    diagnose(&connection)
}

fn diagnose(connection: &Connection) -> Result<Diagnostic, Error> {
    let version = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let result = if version > SCHEMA.understood {
        Err(Error::SchemaTooNew)
    } else {
        status(connection)
    };
    Ok(Diagnostic {
        schema_version: version,
        supported_version: SCHEMA.understood,
        status: result.as_ref().ok().cloned(),
        status_error: result.err(),
    })
}

fn status(connection: &Connection) -> Result<Status, Error> {
    // One SQLite read transaction gives counts and worker rows one snapshot.
    let transaction = connection.unchecked_transaction()?;
    let count = |sql| transaction.query_row(sql, [], |row| row.get::<_, u64>(0));
    let awake_bots = count("SELECT count(*) FROM workers")?;
    let running_turns = count("SELECT count(*) FROM turns WHERE state = 'running'")?;
    let queued_turns = count("SELECT count(*) FROM turns WHERE state = 'queued'")?;
    let inbox_depth = count("SELECT count(*) FROM inbox")?;
    let workers = transaction
        .prepare("SELECT bot, incarnation, started_at_ms, last_heard_at_ms FROM workers ORDER BY bot LIMIT 1024")?
        .query_map([], |row| {
            Ok(WorkerStatus {
                bot: row.get(0)?,
                incarnation: row.get(1)?,
                started_at_ms: row.get(2)?,
                last_heard_at_ms: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let next_routine_at_ms = transaction.query_row(
        "SELECT MIN(next_due_ms) FROM routines WHERE enabled=1",
        [],
        |r| r.get(0),
    )?;
    transaction.commit()?;
    Ok(Status {
        awake_bots,
        running_turns,
        queued_turns,
        inbox_depth,
        workers,
        next_routine_at_ms,
    })
}

#[cfg(test)]
mod tests;
