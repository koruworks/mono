// SPDX-License-Identifier: MPL-2.0

//! Databases that live in this process's memory and nowhere else.
//!
//! A plain `:memory:` database belongs to the one connection that opened it,
//! so a component that keeps a writer and its readers — the read model, the
//! search index — or that opens the same database more than once — the Store
//! opens a `workspace.db` per operation — cannot use one. A **named database
//! in the shared cache** can: every connection in this process that opens the
//! name shares it, and it lives while at least one connection holds it. No
//! journal, WAL, shared-memory or temporary file ever exists beside it
//! (`temp_store = MEMORY` covers sorts and temporary tables), so a process
//! with a read-only filesystem runs everything opened through here.
//!
//! **Readers read uncommitted, and that is the decision.** Koru's databases
//! run in WAL on disk, and its code relies on what WAL gives: a reader never
//! waits for a writer and a writer never waits for a reader, so a thread may
//! hold a read on one connection while it reads through another. A database
//! in memory has no WAL. Both other ways of sharing one were measured and
//! refused:
//!
//! - the `memdb` VFS locks the whole database the rollback way — no new
//!   reader once a writer holds its reservation — and the projector, the
//!   indexer and a query deadlocked through it under load until a busy
//!   timeout failed the query;
//! - the shared cache at the default isolation takes table locks that fail
//!   with `SQLITE_LOCKED` at once, never waiting on the busy timeout.
//!
//! With `read_uncommitted` a reader takes no table lock at all, so neither
//! side ever blocks — WAL's property — and the price is that a reader may see
//! a writer's transaction before it commits. Every database this serves has
//! **one writer**, and every one is either serialized above SQLite (the
//! Store's, under its lock) or a rebuildable cache (the read model and the
//! index), where a query that lands mid-transaction is answered from a moment
//! that is about to be true rather than one that was.

use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{Connection, OpenFlags};

/// The name of one database in this process's memory.
///
/// Minted, never chosen: two compositions in one process must never share a
/// database by accident, so every name carries the process id and a counter.
/// The label is only there to make a name readable in a debugger.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MemoryName(String);

impl MemoryName {
    /// A name no other database in this process has.
    pub fn fresh(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let label: String = label
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .take(48)
            .collect();
        Self(format!(
            "/koru-{}-{}-{label}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Open (or join) the named database in memory, reading uncommitted.
///
/// The caller's own pragmas still apply: a durability or journal setting is
/// harmless here, because SQLite keeps this database's journal in memory
/// whatever it is asked for.
pub fn open_memory(name: &MemoryName) -> rusqlite::Result<Connection> {
    let connection = Connection::open_with_flags(
        format!("file:{}?mode=memory&cache=shared", name.as_str()),
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_SHARED_CACHE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    connection.pragma_update(None, "read_uncommitted", true)?;
    Ok(connection)
}

/// Whether every database this connection has attached lives in memory: no
/// file behind it and temporary storage kept in memory. What a composition
/// that promises a read-only filesystem asserts about each of its connections.
pub fn is_memory_only(connection: &Connection) -> rusqlite::Result<bool> {
    let mut statement = connection.prepare("PRAGMA database_list")?;
    let files = statement
        .query_map([], |row| row.get::<_, String>(2))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let temp_store: i64 = connection.query_row("PRAGMA temp_store", [], |row| row.get(0))?;
    let journal: String = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    Ok(files.iter().all(String::is_empty)
        && temp_store == 2
        && journal.eq_ignore_ascii_case("memory"))
}

#[cfg(test)]
mod tests;
