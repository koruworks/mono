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
/// schema 9 retains Routine routing, linkage and independent policy defenses;
/// schema 10 adds decision, wait, resume and exact publication receipt storage;
/// schema 11 recreates the provably empty `bot_decisions` with the kinds
/// `private_operation` and `browser_origin`, and adds `bot_private_effects`.
pub const RUNS_SCHEMA_VERSION: i64 = 11;
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
    Migration {
        to: 10,
        name: "029_runs_bot_decisions.sql",
        sql: include_str!("../migrations/029_runs_bot_decisions.sql"),
        columns: &[],
    },
    Migration {
        to: 11,
        name: "030_runs_private_operations.sql",
        sql: include_str!("../migrations/030_runs_private_operations.sql"),
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
    koru_sqlite::open_with_preflight(&SCHEMA, path, refuse_retained_decisions)
}

/// The whole open sequence on a connection already opened, with the same
/// preflight a file open runs.
#[cfg(test)]
pub(crate) fn prepare(connection: &mut Connection) -> Result<(), OpenError> {
    koru_sqlite::prepare_with_preflight(&SCHEMA, connection, refuse_retained_decisions)
}

/// The `operation` of the [`OpenError::Storage`] that refuses a `runs.db`
/// at schema 10 which holds a Bot decision.
pub const RETAINED_DECISIONS_UPGRADE: &str = "upgrade";
/// Its fixed detail. No row, id or path is interpolated.
pub const RETAINED_DECISIONS_DETAIL: &str = "runs.db holds Bot decisions written by a development build. This build cannot upgrade it. Nothing was changed.";

/// Schema 11 recreates `bot_decisions`, which is possible only while the
/// table is empty: its five child tables reference it by name and a decision
/// is retained for ever (`bot_decisions_retain`). No released build writes a
/// decision at schema 10, so a row there comes from a local development build
/// with decisions switched on. This read-only check runs before any pragma or
/// step and refuses that database in words. It changes nothing: the build that
/// wrote the file still opens it, and the delegate does not start for that
/// root. Nothing carries the rows over; the supported answer is a fresh
/// isolated root. The migration's own SQL guard repeats the test, so a path
/// that skips this check still cannot drop a row.
fn refuse_retained_decisions(connection: &Connection) -> Result<(), OpenError> {
    if user_version(connection)? != 10 {
        return Ok(());
    }
    let storage = |detail: String| OpenError::Storage {
        database: DATABASE_NAME,
        operation: RETAINED_DECISIONS_UPGRADE,
        detail,
    };
    let retained: i64 = connection
        .query_row("SELECT count(*) FROM bot_decisions", [], |row| row.get(0))
        .map_err(|error| storage(error.to_string()))?;
    if retained != 0 {
        return Err(storage(RETAINED_DECISIONS_DETAIL.to_owned()));
    }
    Ok(())
}

pub(crate) fn user_version(connection: &Connection) -> Result<i64, OpenError> {
    koru_sqlite::user_version(DATABASE_NAME, connection)
}

#[cfg(test)]
mod bot_tests;
#[cfg(test)]
mod decision_tests;
#[cfg(test)]
mod routine_tests;
#[cfg(test)]
mod tests;
