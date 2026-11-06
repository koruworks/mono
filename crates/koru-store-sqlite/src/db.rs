// SPDX-License-Identifier: MPL-2.0

//! Opening a database, and moving it forward a version at a time — the
//! Store's face on [`koru_sqlite`], which holds the policy (`synchronous =
//! FULL`, forward-only steps, a newer stamp refused) and the reasons for it.
//! What stays here is the Store's own error type and the order its callers
//! rely on: `open` configures, and `migrate` is asked for separately by the
//! catalog and the workspace with their own tables.

use koru_store_core::errors::{Code, Error, Result};
use rusqlite::Connection;

pub use koru_sqlite::Migration;

fn store_error(error: koru_sqlite::OpenError) -> Error {
    match error {
        koru_sqlite::OpenError::SchemaTooNew { .. } => {
            Error::new(Code::SchemaTooNew, error.to_string())
        }
        koru_sqlite::OpenError::Storage { .. } => {
            Error::new(Code::StorageFailed, error.to_string())
        }
    }
}

/// Whether `table` already has `column`.
pub(crate) fn has_column(tx: &Connection, table: &str, column: &str) -> Result<bool> {
    koru_sqlite::has_column("store", tx, table, column).map_err(store_error)
}

/// Open a connection configured for durable, concurrent use.
pub fn open(path: &std::path::Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::new(
                Code::StorageFailed,
                format!("could not create the store directory: {e}"),
            )
        })?;
    }
    let connection = Connection::open(path)
        .map_err(|e| Error::new(Code::StorageFailed, format!("open: {e}")))?;
    koru_sqlite::configure("store", &connection).map_err(store_error)?;
    Ok(connection)
}

/// An in-memory database, for tests. Same pragmas, so a test exercises the
/// same code path as a real open.
pub fn open_in_memory() -> Result<Connection> {
    let connection = Connection::open_in_memory()
        .map_err(|e| Error::new(Code::StorageFailed, format!("open: {e}")))?;
    koru_sqlite::configure("store", &connection).map_err(store_error)?;
    Ok(connection)
}

/// A named database in this process's memory, shared by every connection that
/// opens the same name and gone with the last of them. Same pragmas as every
/// other open; no file is ever written for it, journal and temporary storage
/// included ([`koru_sqlite::open_memory`]).
pub fn open_named_memory(name: &koru_sqlite::MemoryName) -> Result<Connection> {
    let connection = koru_sqlite::open_memory(name)
        .map_err(|e| Error::new(Code::StorageFailed, format!("open: {e}")))?;
    koru_sqlite::configure("store", &connection).map_err(store_error)?;
    Ok(connection)
}

/// A **named** in-memory database, for tests that have to reopen the bytes a
/// previous `Store` wrote.
///
/// [`open_in_memory`] gives every connection its own private database, so
/// dropping a `Store` destroys everything it persisted and a restart test
/// silently proves nothing. A shared-cache URI database instead survives a
/// connection being dropped for as long as *some* connection to that name
/// stays open, so a test holds one keeper connection and opens and drops
/// `Store`s over it. Same pragmas as every other open, and still no disk.
#[cfg(test)]
pub fn open_shared_in_memory(name: &str) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        format!("file:{name}?mode=memory&cache=shared"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
            | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| Error::new(Code::StorageFailed, format!("open: {e}")))?;
    koru_sqlite::configure("store", &connection).map_err(store_error)?;
    Ok(connection)
}

/// Apply every migration above the database's current version and answer
/// the version it ended on. A database **newer** than `understood` is
/// refused rather than touched — [`Code::SchemaTooNew`], and it is visible.
pub fn migrate(
    connection: &mut Connection,
    database: &'static str,
    migrations: &[Migration],
    understood: i64,
) -> Result<i64> {
    koru_sqlite::migrate(database, understood, migrations, connection).map_err(store_error)
}

pub fn user_version(connection: &Connection) -> Result<i64> {
    koru_sqlite::user_version("store", connection).map_err(store_error)
}

/// Bytes this database occupies, for the debug routes. Metadata, never content.
pub fn size_bytes(connection: &Connection) -> Result<u64> {
    koru_sqlite::size_bytes("store", connection).map_err(store_error)
}

/// Whether the database reads back intact.
pub fn quick_check(connection: &Connection) -> Result<bool> {
    koru_sqlite::quick_check("store", connection).map_err(store_error)
}

pub(crate) fn sql_failed(e: rusqlite::Error) -> Error {
    Error::new(Code::StorageFailed, e.to_string())
}

#[cfg(test)]
mod tests;
