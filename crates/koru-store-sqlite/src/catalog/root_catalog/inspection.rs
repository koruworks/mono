// SPDX-License-Identifier: MPL-2.0

//! A read-only inventory of the root catalog for one device
//! (`docs/features/root-catalog/VERIFICATION.md`): what the acceptance
//! procedures inspect between steps, and the one diagnostic Plan 4.1 added.
//!
//! **Ids and counts, never a name.** Every string here is a UUID or a fixed
//! vocabulary word. Names, colors and icons live in the identity and
//! Workspace manifests and are not read; a private subject's name must never
//! travel in a report that is pasted into a bug thread. The report is for
//! the local scope only — rows under another scope are a number.
//!
//! The file form opens `catalog.db` **read-only and without migrating**: a
//! diagnostic that moved a 12 to 13 as a side effect would be the opposite
//! of what procedure 1 needs. A catalog below 13 is reported as such.

use std::path::Path;

use koru_store_core::errors::{Code, Error, Result};
use koru_store_core::root_catalog::{CatalogUnitRef, ProfileCatalogUnit, WorkspaceCatalogUnit};
use koru_store_core::service::{
    K_ACCOUNT_SCOPE, K_JOIN_RECEIPT_PREFIX, K_PEER_SCOPE_PREFIX, K_ROOT_CATALOG_BATCH_PREFIX,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::{Value, json};

use super::{TO, peer_from_i64};
use crate::db::sql_failed;

/// One root unit row as the inventory reads it. `decodable` is false when
/// the snapshot did not open as the unit it is filed as; the other facts
/// are then zero and `live` is false.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitReport {
    pub kind: String,
    pub subject: String,
    pub revision: i64,
    pub decodable: bool,
    /// The unit opened read-only because a newer build wrote it.
    pub format_read_only: bool,
    pub live: bool,
    /// A Workspace unit's current placement: a Profile id, `null` for an
    /// explicitly unassigned register, absent for a Profile unit or a unit
    /// with no register yet.
    pub placement: Option<String>,
    pub placement_present: bool,
    pub deletions: usize,
    pub active_deletions: usize,
    pub restores: usize,
    pub purged: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayReport {
    pub kind: String,
    pub subject: String,
    pub writer_peer_id: u64,
    pub base_vv_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonalUnitReport {
    pub workspace: String,
    pub writer_peer_id: u64,
    /// Whether the due index is stamped for the current bytes.
    pub indexed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReceiptCounts {
    pub root_seed: usize,
    pub personal_v2: usize,
}

/// The inventory of one device's root catalog under its local scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootCatalogInventoryReport {
    pub schema_version: i64,
    /// The local user scope id.
    pub scope: String,
    pub profile_units: Vec<UnitReport>,
    pub workspace_units: Vec<UnitReport>,
    pub overlays: Vec<OverlayReport>,
    pub personal_units: Vec<PersonalUnitReport>,
    pub due_rows: usize,
    pub receipts: ReceiptCounts,
    /// `store_state` rows: join receipts, Profile deletion batch jobs,
    /// bound peer relationships, and whether an account scope is bound.
    pub join_receipts: usize,
    pub batch_jobs: usize,
    pub peer_scopes: usize,
    pub account_bound: bool,
    /// Rows of every root table filed under a scope other than this one:
    /// another user's data on a shared installation, a purge hold's cause.
    pub foreign_scope_rows: usize,
}

impl UnitReport {
    fn to_value(&self) -> Value {
        json!({
            "kind": self.kind,
            "subject": self.subject,
            "revision": self.revision,
            "decodable": self.decodable,
            "format_read_only": self.format_read_only,
            "live": self.live,
            "placement": self.placement,
            "placement_present": self.placement_present,
            "deletions": self.deletions,
            "active_deletions": self.active_deletions,
            "restores": self.restores,
            "purged": self.purged,
        })
    }
}

impl RootCatalogInventoryReport {
    /// The report as JSON text. Built by hand rather than derived: this
    /// crate links `serde_json` and not `serde`, and one report is not a
    /// reason to add a dependency.
    pub fn to_value(&self) -> Value {
        json!({
            "schema_version": self.schema_version,
            "scope": self.scope,
            "profile_units": self.profile_units.iter().map(UnitReport::to_value).collect::<Vec<_>>(),
            "workspace_units": self.workspace_units.iter().map(UnitReport::to_value).collect::<Vec<_>>(),
            "overlays": self.overlays.iter().map(|overlay| json!({
                "kind": overlay.kind,
                "subject": overlay.subject,
                "writer_peer_id": overlay.writer_peer_id,
                "base_vv_bytes": overlay.base_vv_bytes,
            })).collect::<Vec<_>>(),
            "personal_units": self.personal_units.iter().map(|unit| json!({
                "workspace": unit.workspace,
                "writer_peer_id": unit.writer_peer_id,
                "indexed": unit.indexed,
            })).collect::<Vec<_>>(),
            "due_rows": self.due_rows,
            "receipts": {
                "root_seed": self.receipts.root_seed,
                "personal_v2": self.receipts.personal_v2,
            },
            "join_receipts": self.join_receipts,
            "batch_jobs": self.batch_jobs,
            "peer_scopes": self.peer_scopes,
            "account_bound": self.account_bound,
            "foreign_scope_rows": self.foreign_scope_rows,
        })
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(&self.to_value())
            .map_err(|e| Error::new(Code::Internal, format!("report is not serializable: {e}")))
    }
}

/// Inspect `catalog.db` at `path` read-only. Never migrates; a file below
/// 13 answers `VersionUnavailable`, one above it `SchemaTooNew`.
pub fn inspect_file(path: &Path) -> Result<RootCatalogInventoryReport> {
    if !path.is_file() {
        return Err(Error::new(
            Code::StorageFailed,
            format!("no catalog at {}", path.display()),
        ));
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sql_failed)?;
    connection
        .execute_batch("PRAGMA query_only = ON;")
        .map_err(sql_failed)?;
    inspect(&connection)
}

/// Inspect the catalog behind `connection`. Reads only; the connection's
/// own mode decides whether it could write, which is why the file form
/// opens read-only.
pub fn inspect(connection: &Connection) -> Result<RootCatalogInventoryReport> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(sql_failed)?;
    if version < TO {
        return Err(Error::new(
            Code::VersionUnavailable,
            format!("catalog is at {version}; the root catalog arrives with {TO}"),
        ));
    }
    if version > TO {
        return Err(Error::new(
            Code::SchemaTooNew,
            format!("catalog is at {version}; this build understands {TO}"),
        ));
    }
    let scope = super::local_user_scope(connection)?;
    let scope_text = scope.to_storage();

    let mut profile_units = Vec::new();
    let mut workspace_units = Vec::new();
    {
        let mut statement = connection
            .prepare(
                "SELECT unit_kind, subject_id, snapshot, revision FROM root_catalog_units
                 WHERE user_scope_id = ?1 ORDER BY unit_kind, subject_id",
            )
            .map_err(sql_failed)?;
        let rows = statement
            .query_map(params![scope_text], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(sql_failed)?;
        for row in rows {
            let (kind, subject, snapshot, revision) = row.map_err(sql_failed)?;
            let report = unit_report(&kind, &subject, &snapshot, revision);
            match CatalogUnitRef::parse(&kind, &subject) {
                Some(CatalogUnitRef::Profile { .. }) => profile_units.push(report),
                _ => workspace_units.push(report),
            }
        }
    }

    let overlays = {
        let mut statement = connection
            .prepare(
                "SELECT unit_kind, subject_id, writer_peer_id, length(base_vv)
                 FROM root_catalog_overlays WHERE user_scope_id = ?1
                 ORDER BY unit_kind, subject_id",
            )
            .map_err(sql_failed)?;
        let rows = statement
            .query_map(params![scope_text], |row| {
                Ok(OverlayReport {
                    kind: row.get(0)?,
                    subject: row.get(1)?,
                    writer_peer_id: peer_from_i64(row.get(2)?),
                    base_vv_bytes: row.get::<_, i64>(3)?.max(0) as usize,
                })
            })
            .map_err(sql_failed)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sql_failed)?
    };

    let personal_units = {
        let mut statement = connection
            .prepare(
                "SELECT workspace_id, writer_peer_id, indexed_version IS NOT NULL
                 FROM workspace_personal_crdts WHERE user_scope_id = ?1 ORDER BY workspace_id",
            )
            .map_err(sql_failed)?;
        let rows = statement
            .query_map(params![scope_text], |row| {
                Ok(PersonalUnitReport {
                    workspace: row.get(0)?,
                    writer_peer_id: peer_from_i64(row.get(1)?),
                    indexed: row.get(2)?,
                })
            })
            .map_err(sql_failed)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sql_failed)?
    };

    let scoped_count = |sql: &str| -> Result<usize> {
        connection
            .query_row(sql, params![scope_text], |row| row.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(sql_failed)
    };
    let prefix_count = |prefix: &str| -> Result<usize> {
        connection
            .query_row(
                "SELECT COUNT(*) FROM store_state WHERE key LIKE ?1 || '%'",
                params![prefix],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n.max(0) as usize)
            .map_err(sql_failed)
    };
    let account_bound: Option<String> = connection
        .query_row(
            "SELECT value FROM store_state WHERE key = ?1",
            params![K_ACCOUNT_SCOPE],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_failed)?;
    let foreign_scope_rows = scoped_count(
        "SELECT (SELECT COUNT(*) FROM root_catalog_units WHERE user_scope_id != ?1)
              + (SELECT COUNT(*) FROM root_catalog_overlays WHERE user_scope_id != ?1)
              + (SELECT COUNT(*) FROM workspace_personal_crdts WHERE user_scope_id != ?1)",
    )?;

    Ok(RootCatalogInventoryReport {
        schema_version: version,
        scope: scope_text.clone(),
        profile_units,
        workspace_units,
        overlays,
        personal_units,
        due_rows: scoped_count(
            "SELECT COUNT(*) FROM workspace_reminder_due WHERE user_scope_id = ?1",
        )?,
        receipts: ReceiptCounts {
            root_seed: scoped_count(
                "SELECT COUNT(*) FROM root_catalog_migration_receipts
                 WHERE user_scope_id = ?1 AND kind = 'root_seed'",
            )?,
            personal_v2: scoped_count(
                "SELECT COUNT(*) FROM root_catalog_migration_receipts
                 WHERE user_scope_id = ?1 AND kind = 'personal_v2'",
            )?,
        },
        join_receipts: prefix_count(K_JOIN_RECEIPT_PREFIX)?,
        batch_jobs: prefix_count(K_ROOT_CATALOG_BATCH_PREFIX)?,
        peer_scopes: prefix_count(K_PEER_SCOPE_PREFIX)?,
        account_bound: account_bound.is_some(),
        foreign_scope_rows,
    })
}

fn unit_report(kind: &str, subject: &str, snapshot: &[u8], revision: i64) -> UnitReport {
    let mut report = UnitReport {
        kind: kind.to_owned(),
        subject: subject.to_owned(),
        revision,
        decodable: false,
        format_read_only: false,
        live: false,
        placement: None,
        placement_present: false,
        deletions: 0,
        active_deletions: 0,
        restores: 0,
        purged: 0,
    };
    let Some(unit) = CatalogUnitRef::parse(kind, subject) else {
        return report;
    };
    match unit {
        CatalogUnitRef::Profile { profile } => {
            if let Ok(unit) = ProfileCatalogUnit::from_snapshot(snapshot, profile, READER) {
                report.decodable = true;
                report.format_read_only = unit.is_read_only();
                report.live = unit.is_live();
                report.deletions = unit.deletions().len();
                report.active_deletions = unit.active_deletions().len();
                report.restores = unit.restores().len();
                report.purged = unit.purged().len();
            }
        }
        CatalogUnitRef::Workspace { workspace } => {
            if let Ok(unit) = WorkspaceCatalogUnit::from_snapshot(snapshot, workspace, READER) {
                report.decodable = true;
                report.format_read_only = unit.is_read_only();
                report.live = unit.is_live();
                if let Some(placement) = unit.placement() {
                    report.placement_present = true;
                    report.placement = placement.profile.map(|p| p.to_storage());
                }
                report.deletions = unit.deletions().len();
                report.active_deletions = unit.active_deletions().len();
                report.restores = unit.restores().len();
                report.purged = unit.purged().len();
            }
        }
    }
    report
}

/// The writer a read-only decode is opened under. Nothing is written, so
/// the value never reaches a document; it is named so a reader can see it
/// is not the Store's peer.
const READER: u64 = 1;

#[cfg(all(test, feature = "disk-tests"))]
mod disk_tests;
