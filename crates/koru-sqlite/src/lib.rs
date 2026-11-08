// SPDX-License-Identifier: MPL-2.0

//! How Koru opens and versions a SQLite file.
//!
//! Four crates own a database — the Store's `catalog.db` and `workspace.db`,
//! the delegate ledger `runs.db`, `artifacts.db` and `ai.db` — and each had
//! written this file again: read `PRAGMA user_version`, refuse a file a newer
//! build stamped, set the journal mode and the durability, apply the
//! forward-only steps each in its own transaction, stamp the version. Four
//! copies is four places the next fix is not applied. This is the one.
//!
//! **Refuse before touching.** `journal_mode` persists in the file, so the
//! stamp is read before any pragma is set: an older build meeting a newer
//! database changes nothing about it, not even the journal mode
//! ([`schema_too_new`](../../../docs/contributing/operations/RUNBOOK.md)).
//!
//! **`synchronous = FULL`, not `NORMAL`.** Under WAL, `NORMAL` syncs at
//! checkpoint time, so a power loss can discard commits SQLite has already
//! reported — and every one of these files answers something with "durable"
//! the moment a commit returns. The search index is the one Koru database on
//! `NORMAL`, and is right to be: it is a rebuildable cache, and it does not
//! open through here.
//!
//! **`fullfsync` too, or `FULL` means less on Apple.** SQLite's sync on macOS
//! and iOS is a plain `fsync()`, which hands the data to the drive and does not
//! flush the drive's own cache — Apple documents `F_FULLFSYNC` as the call
//! that does, and Windows' `FlushFileBuffers` already is one. Without
//! `fullfsync` (and `checkpoint_fullfsync` for the WAL's checkpoints) a power
//! cut on a Mac can lose a commit this crate has reported durable. Both
//! pragmas do nothing on other platforms. Measured on an M-series Mac's
//! internal SSD on 2026-09-24: 0.23 ms a commit without, 1.17 ms with.
//!
//! **Migrations are forward-only, and a shipped step is never edited.** A
//! table added to a step that has already run reaches only new databases,
//! and every existing one fails much later, as a query naming a column the
//! table has not got. The next change is the next numbered step. The one
//! tolerated exception is [`Migration::columns`]: adding a column that is
//! already there is the one step where "already done" is its whole meaning.

use std::{fmt, path::Path, time::Duration};

use rusqlite::Connection;

mod memory;
pub use memory::{MemoryName, is_memory_only, open_memory};

/// One forward step. `sql` runs inside a transaction; if it fails, the
/// version does not move and nothing is left half-applied.
pub struct Migration {
    pub to: i64,
    /// What the step is called in the log — the schema file's name where
    /// there is one, a short label otherwise.
    pub name: &'static str,
    pub sql: &'static str,
    /// Columns to add **if they are not already there**: `(table, column,
    /// declaration)`. Declared here rather than written into `sql`, a later
    /// migration can add the column unconditionally without failing on the
    /// databases that already received it early.
    pub columns: &'static [(&'static str, &'static str, &'static str)],
}

/// A database this build knows how to open: its name, the schema version it
/// understands, and the steps that reach it.
pub struct Schema {
    /// The file's name, for messages and the log — never a path.
    pub database: &'static str,
    /// The version this build understands. A file stamped higher is refused.
    pub understood: i64,
    pub migrations: &'static [Migration],
}

/// Why a database could not be opened. Numbers and a stage — never a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// A newer build stamped this file. Nothing was changed, not even the
    /// journal mode.
    SchemaTooNew {
        database: &'static str,
        discovered: i64,
        supported: i64,
    },
    Storage {
        database: &'static str,
        operation: &'static str,
        detail: String,
    },
}

impl OpenError {
    fn sql(database: &'static str, operation: &'static str, error: rusqlite::Error) -> Self {
        Self::Storage {
            database,
            operation,
            detail: error.to_string(),
        }
    }
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaTooNew {
                database,
                discovered,
                supported,
            } => write!(
                f,
                "{database} schema {discovered} is newer than supported schema {supported}"
            ),
            Self::Storage {
                database,
                operation,
                detail,
            } => write!(f, "could not {operation} {database}: {detail}"),
        }
    }
}

impl std::error::Error for OpenError {}

/// A second writer waits this long before failing. Long enough for any
/// commit these databases make; short enough that a wedged one is news.
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

/// Open the file — creating its directory — and bring it to the schema:
/// refuse a newer stamp, then configure, then migrate.
pub fn open(schema: &Schema, path: &Path) -> Result<Connection, OpenError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| OpenError::Storage {
            database: schema.database,
            operation: "create the directory of",
            detail: error.to_string(),
        })?;
    }
    let mut connection =
        Connection::open(path).map_err(|error| OpenError::sql(schema.database, "open", error))?;
    prepare(schema, &mut connection)?;
    Ok(connection)
}

/// An in-memory database at the schema, for tests. Same pragmas, so a test
/// exercises the same path as a real open — `synchronous` means nothing
/// without a disk, and everything else behaves identically.
pub fn open_in_memory(schema: &Schema) -> Result<Connection, OpenError> {
    let mut connection = Connection::open_in_memory()
        .map_err(|error| OpenError::sql(schema.database, "open in memory", error))?;
    prepare(schema, &mut connection)?;
    Ok(connection)
}

/// The whole open sequence on a connection already opened: the stamp first,
/// so a refusal is harmless; then the pragmas; then the steps.
pub fn prepare(schema: &Schema, connection: &mut Connection) -> Result<(), OpenError> {
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| OpenError::sql(schema.database, "set the busy timeout of", error))?;
    let discovered = user_version(schema.database, connection)?;
    if discovered > schema.understood {
        return Err(OpenError::SchemaTooNew {
            database: schema.database,
            discovered,
            supported: schema.understood,
        });
    }
    configure(schema.database, connection)?;
    migrate(
        schema.database,
        schema.understood,
        schema.migrations,
        connection,
    )
    .map(|_| ())
}

/// What every Koru database that answers "durable" sets: WAL, `synchronous =
/// FULL`, and on Apple a sync that reaches the platter (see the module
/// documentation). Public for the openers that keep their own schema
/// handling but must not keep their own idea of durability.
pub const DURABILITY: &str = "PRAGMA journal_mode = WAL;
     PRAGMA synchronous = FULL;
     PRAGMA fullfsync = ON;
     PRAGMA checkpoint_fullfsync = ON;";

/// The pragmas alone: [`DURABILITY`], then foreign keys. For an opener that
/// has its own reason to configure before it migrates.
pub fn configure(database: &'static str, connection: &Connection) -> Result<(), OpenError> {
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| OpenError::sql(database, "set the busy timeout of", error))?;
    connection
        .execute_batch(DURABILITY)
        .and_then(|()| connection.execute_batch("PRAGMA foreign_keys = ON;"))
        .map_err(|error| OpenError::sql(database, "configure the durability of", error))?;
    Ok(())
}

/// Apply every step above the database's current version, each in its own
/// transaction, and answer the version it ended on. A database newer than
/// the schema is refused rather than touched.
pub fn migrate(
    database: &'static str,
    understood: i64,
    migrations: &[Migration],
    connection: &mut Connection,
) -> Result<i64, OpenError> {
    let current = user_version(database, connection)?;
    if current > understood {
        return Err(OpenError::SchemaTooNew {
            database,
            discovered: current,
            supported: understood,
        });
    }
    let mut from = current;
    for step in migrations.iter().filter(|step| step.to > current) {
        let started = std::time::Instant::now();
        tracing::info!(
            database,
            from,
            to = step.to,
            migration = step.name,
            "applying SQL migration"
        );
        let tx = connection
            .transaction()
            .map_err(|error| OpenError::sql(database, "begin a migration of", error))?;
        if !step.sql.trim().is_empty() {
            tx.execute_batch(step.sql)
                .map_err(|error| OpenError::sql(database, "apply a migration of", error))?;
        }
        for (table, column, declaration) in step.columns {
            if has_column(database, &tx, table, column)? {
                continue;
            }
            tx.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {declaration};"
            ))
            .map_err(|error| OpenError::sql(database, "add a column in a migration of", error))?;
        }
        // `user_version` takes no parameter binding. `step.to` is an `i64`
        // from a compiled-in table, so there is no injection surface here.
        tx.pragma_update(None, "user_version", step.to)
            .map_err(|error| OpenError::sql(database, "stamp a migration of", error))?;
        tx.commit()
            .map_err(|error| OpenError::sql(database, "commit a migration of", error))?;
        tracing::info!(
            database,
            from,
            to = step.to,
            migration = step.name,
            elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
            "SQL migration applied"
        );
        from = step.to;
    }
    user_version(database, connection)
}

pub fn user_version(database: &'static str, connection: &Connection) -> Result<i64, OpenError> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| OpenError::sql(database, "read the schema version of", error))
}

/// Whether `table` already has `column`. `table` comes from a compiled-in
/// table, so there is no injection surface here.
pub fn has_column(
    database: &'static str,
    connection: &Connection,
    table: &str,
    column: &str,
) -> Result<bool, OpenError> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| OpenError::sql(database, "describe a table of", error))?;
    let mut rows = statement
        .query([])
        .map_err(|error| OpenError::sql(database, "describe a table of", error))?;
    while let Some(row) = rows
        .next()
        .map_err(|error| OpenError::sql(database, "describe a table of", error))?
    {
        let name: String = row
            .get(1)
            .map_err(|error| OpenError::sql(database, "describe a table of", error))?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Bytes the database occupies. Metadata, never content.
pub fn size_bytes(database: &'static str, connection: &Connection) -> Result<u64, OpenError> {
    let pages: i64 = connection
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .map_err(|error| OpenError::sql(database, "measure", error))?;
    let size: i64 = connection
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .map_err(|error| OpenError::sql(database, "measure", error))?;
    Ok((pages.max(0) as u64) * (size.max(0) as u64))
}

/// Whether the database reads back intact. `quick_check` rather than
/// `integrity_check`: it catches the damage that actually happens — a torn
/// page, a broken index — without the full cross-reference pass that would
/// make every start-up wait on a scan of everything ever written.
pub fn quick_check(database: &'static str, connection: &Connection) -> Result<bool, OpenError> {
    let answer: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(|error| OpenError::sql(database, "check", error))?;
    Ok(answer == "ok")
}

#[cfg(test)]
mod tests;
