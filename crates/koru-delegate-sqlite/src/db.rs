// SPDX-License-Identifier: MPL-2.0

//! The ledger's schema, opened through [`koru_sqlite`] — the one policy every
//! Koru database shares (stamp first, refuse a newer one, WAL and `FULL`,
//! forward-only steps).
//!
//! **A step per version, and a shipped step is never edited again.** The
//! journal's own history records why (`journal.rs`): a table added to an
//! already-stamped step reached only new databases, and every existing one
//! failed silently. Version 2 of this file is a second `Migration` entry
//! with the next numbered file under `crates/koru-delegate-sqlite/migrations/`, and the tests
//! beside this module hold the shape of that rule before it is needed.

use std::path::{Path, PathBuf};

use koru_sqlite::{Migration, Schema};
use rusqlite::Connection;

pub use koru_sqlite::OpenError;

/// The schema this build understands. A file stamped higher is refused.
///
/// Schema 1 is the ledger of §7.1; schema 2 adds `run_deletions`; schema 3
/// retains the holding RunId needed to replay a `skipped_busy` schedule fact;
/// schema 4 adds the durable schedule-event outbox; schema 5 adds Bot
/// admission, reservation accounting, delivery correlation and pause barriers;
/// schema 6 adds native-result intents and ordered effect-policy barriers;
/// schema 7 retains exact Store drain proofs separately from result certainty;
/// schema 8 separates Memory effects and their stable allocation identities;
/// schema 9 retains Routine routing, linkage and independent policy defenses.
pub const RUNS_SCHEMA_VERSION: i64 = 9;
/// The file's name inside the delegate root.
pub const DATABASE_NAME: &str = "runs.db";

pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        to: 1,
        name: "017_runs_initial_migration.sql",
        sql: include_str!("../migrations/017_runs_initial_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 2,
        name: "020_runs_deletions_migration.sql",
        sql: include_str!("../migrations/020_runs_deletions_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 3,
        name: "022_runs_schedule_busy_holder_migration.sql",
        sql: include_str!("../migrations/022_runs_schedule_busy_holder_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 4,
        name: "023_runs_schedule_publications_migration.sql",
        sql: include_str!("../migrations/023_runs_schedule_publications_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 5,
        name: "024_runs_bot_admission_migration.sql",
        sql: include_str!("../migrations/024_runs_bot_admission_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 6,
        name: "025_runs_native_results_migration.sql",
        sql: include_str!("../migrations/025_runs_native_results_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 7,
        name: "026_runs_native_drain_migration.sql",
        sql: include_str!("../migrations/026_runs_native_drain_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 8,
        name: "027_runs_memory_effects_migration.sql",
        sql: include_str!("../migrations/027_runs_memory_effects_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 9,
        name: "028_runs_bot_routines.sql",
        sql: include_str!("../migrations/028_runs_bot_routines.sql"),
        columns: &[],
    },
];

const SCHEMA: Schema = Schema {
    database: DATABASE_NAME,
    understood: RUNS_SCHEMA_VERSION,
    migrations: MIGRATIONS,
};

/// `<channel-root>/delegate/runs.db`, from a store root, through the one
/// rule `koru_channel::delegate_root_beside` states.
pub fn runs_db_path(store_root: &Path) -> PathBuf {
    koru_channel::delegate_root_beside(store_root).join(DATABASE_NAME)
}

pub(crate) fn open(path: &Path) -> Result<Connection, OpenError> {
    koru_sqlite::open(&SCHEMA, path)
}

pub(crate) fn user_version(connection: &Connection) -> Result<i64, OpenError> {
    koru_sqlite::user_version(DATABASE_NAME, connection)
}

#[cfg(test)]
mod bot_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod routine_tests;
