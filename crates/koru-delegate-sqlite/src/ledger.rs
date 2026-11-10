// SPDX-License-Identifier: MPL-2.0

//! The open ledger: one connection, one clock, one transaction per write.
//!
//! [`RunsDb`] is the handle every repository method hangs off; the methods
//! themselves live beside their tables (`intents`, `attempts`,
//! `publications`, `cursors`, `claims`). What this file owns is the one
//! rule they share: **a write is one `BEGIN IMMEDIATE` transaction**, timed
//! by the ledger's clock, committed before the caller is answered, and
//! interruptible at the three [`crate::Moment`]s the crash seam names.

use std::{fmt, path::Path};

use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::{
    Clock, LedgerError, OpenError, RUNS_SCHEMA_VERSION, SystemClock, WriteOp, db, runs_db_path,
};

/// What an idempotent write did. A retry after a lost acknowledgement gets
/// `AlreadyApplied` and the same durable fact, never a second one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum WriteOutcome {
    Applied,
    AlreadyApplied,
}

/// The device-local execution ledger, open. `Send`, not `Sync`: one worker
/// opens it, on one thread that never answers a person (§13.3).
pub struct RunsDb {
    conn: Connection,
    clock: Box<dyn Clock>,
    #[cfg(any(test, feature = "crash-points"))]
    crash: crate::crash::CrashArm,
}

impl RunsDb {
    /// Open or create the ledger at `path`, migrating forward, refusing a
    /// file a newer build stamped.
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        Self::open_with_clock(path, Box::new(SystemClock))
    }

    /// Open the ledger where the channel keeps it — `<channel-root>/delegate/runs.db`
    /// beside the store root, through [`runs_db_path`].
    pub fn open_beside(store_root: &Path) -> Result<Self, OpenError> {
        Self::open(&runs_db_path(store_root))
    }

    /// [`Self::open`] with the clock every row's time is read from.
    pub fn open_with_clock(path: &Path, clock: Box<dyn Clock>) -> Result<Self, OpenError> {
        let conn = db::open(path)?;
        Ok(Self {
            conn,
            clock,
            #[cfg(any(test, feature = "crash-points"))]
            crash: crate::crash::CrashArm::default(),
        })
    }

    /// The schema stamped on the file — [`RUNS_SCHEMA_VERSION`] once open.
    pub fn schema_version(&self) -> Result<i64, LedgerError> {
        db::user_version(&self.conn).map_err(|error| LedgerError::Storage {
            operation: "read the schema version",
            detail: error.to_string(),
        })
    }

    /// Arm one crash point. The next write that reaches it dies there.
    #[cfg(any(test, feature = "crash-points"))]
    pub fn arm_crash(&mut self, point: crate::CrashPoint) {
        self.crash.arm(point);
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    pub(crate) fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    /// One consequential write: `BEGIN IMMEDIATE`, the body with the time it
    /// happened at, `COMMIT`, then the answer. Dropping the transaction on
    /// any error rolls it back, so a body that fails half-way leaves
    /// nothing.
    pub(crate) fn write<T>(
        &mut self,
        op: WriteOp,
        body: impl FnOnce(&Transaction<'_>, i64) -> Result<T, LedgerError>,
    ) -> Result<T, LedgerError> {
        self.write_with_error(op, body)
    }

    /// Domain-specific refusals use the same transaction and crash boundary.
    /// In particular, an error from the body must roll back, not become a
    /// successful transaction containing an `Err` result.
    pub(crate) fn write_with_error<T, E: From<LedgerError>>(
        &mut self,
        op: WriteOp,
        body: impl FnOnce(&Transaction<'_>, i64) -> Result<T, E>,
    ) -> Result<T, E> {
        #[cfg(not(any(test, feature = "crash-points")))]
        let _ = op;
        #[cfg(any(test, feature = "crash-points"))]
        self.crash.check(op, crate::Moment::BeforeWrite)?;
        let now = self.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| LedgerError::sql("begin a write", error))?;
        let out = body(&tx, now)?;
        #[cfg(any(test, feature = "crash-points"))]
        self.crash
            .check(op, crate::Moment::AfterWriteBeforeCommit)?;
        tx.commit()
            .map_err(|error| LedgerError::sql("commit a write", error))?;
        #[cfg(any(test, feature = "crash-points"))]
        self.crash.check(op, crate::Moment::AfterCommit)?;
        Ok(out)
    }

    #[cfg(test)]
    pub(crate) fn in_memory(clock: Box<dyn Clock>) -> Self {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        for migration in db::MIGRATIONS {
            conn.execute_batch(migration.sql).unwrap();
        }
        Self {
            conn,
            clock,
            crash: crate::crash::CrashArm::default(),
        }
    }

    /// Exercise the production forward migration opener on a disposable memory fixture.
    #[cfg(test)]
    pub(crate) fn migrate_in_memory(&mut self) {
        koru_sqlite::prepare(
            &koru_sqlite::Schema {
                database: crate::DATABASE_NAME,
                understood: crate::RUNS_SCHEMA_VERSION,
                migrations: db::MIGRATIONS,
            },
            &mut self.conn,
        )
        .unwrap();
    }

    /// Simulate losing the writer's volatile crash state while retaining its
    /// committed database. Tests never open a file or a real Store.
    #[cfg(test)]
    pub(crate) fn restart_in_memory(&mut self) {
        self.crash = crate::crash::CrashArm::default();
    }
}

/// Deliberately says nothing about the file: not its path, not a row.
impl fmt::Debug for RunsDb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunsDb")
            .field("schema", &RUNS_SCHEMA_VERSION)
            .finish()
    }
}
