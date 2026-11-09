// SPDX-License-Identifier: MPL-2.0

//! Catalog 12 → 13 and the rows behind the root-catalog and personal v2
//! ports (`docs/features/root-catalog/TECHNICAL.final.md` §4.2, §5, §8.3).
//!
//! The step is half SQL and half Rust, so it cannot be a [`Migration`] row:
//! `koru_sqlite::migrate` runs SQL-only steps, and this one has to seed Loro
//! units and union personal histories *in the same transaction* as its DDL,
//! or a crash between the two would leave a 13 with no rows or a 12 with new
//! tables. `migrate_to_root_catalog` is therefore called by
//! `SqliteCatalog::from_connection` after the SQL steps have brought the file
//! to 12, inside one `IMMEDIATE` transaction that ends with the stamp.
//!
//! Policy lives in core: the seeds come from `root_catalog::seed`, the
//! personal union from `profile_units::personal_migration`. This module only
//! reads legacy rows, calls them, and writes what they answer.
//!
//! [`Migration`]: crate::db::Migration

use std::collections::BTreeMap;

use koru_store_core::errors::{Code, Error, Result};
use koru_store_core::ids::{ProfileId, ReminderId, UserScopeId, WorkspaceId};
use koru_store_core::model::{ProfileKind, Timestamp};
use koru_store_core::ports::{
    PurgeHold, RootCatalogOverlay, RootCatalogUnitCommit, RootCatalogUnitRow,
    WorkspacePersonalUnitState, WorkspaceReminderDue,
};
use koru_store_core::profile_units::{LegacyPersonalSource, migrate_personal_sources};
use koru_store_core::root_catalog::seed::{
    RootSeedReceipt, seed_profile_unit, seed_workspace_unit, seeded_deletion_id, seeded_move_id,
};
use koru_store_core::root_catalog::{CatalogUnitKind, CatalogUnitRef, K_LOCAL_USER_SCOPE};
use koru_store_core::service::{K_PEER_ID, mint_peer_id};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::collect;
use crate::db::sql_failed;

/// The version this step moves the catalog from and to.
pub(super) const FROM: i64 = 12;
pub(super) const TO: i64 = 13;

const DDL: &str = include_str!("../../migrations/025_catalog_root_catalog_migration.sql");
const RECEIPT_ROOT_SEED: &str = "root_seed";
const RECEIPT_PERSONAL_V2: &str = "personal_v2";

/// Catalog 12 → 13 in one transaction. Phases, in order: DDL; the Store peer
/// id and the local user scope in `store_state`; one root unit per Profile
/// and Workspace; one personal v2 row per Workspace with legacy personal
/// rows; drop the legacy personal tables; stamp. Any error rolls every
/// phase back and surfaces from the open.
pub(super) fn migrate_to_root_catalog(connection: &mut Connection) -> Result<()> {
    let started = std::time::Instant::now();
    tracing::info!(
        database = "catalog.db",
        from = FROM,
        to = TO,
        migration = "025_catalog_root_catalog_migration",
        "applying the root-catalog migration"
    );
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql_failed)?;
    tx.execute_batch(DDL).map_err(sql_failed)?;

    let peer = ensure_peer_id(&tx)?;
    let scope = UserScopeId::new();
    tx.execute(
        "INSERT INTO store_state (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![K_LOCAL_USER_SCOPE, scope.to_storage()],
    )
    .map_err(sql_failed)?;
    let at_ms = Timestamp::now().millis();

    let seeded = seed_root_units(&tx, scope, peer, at_ms)?;
    tracing::info!(
        profiles = seeded.profiles,
        holders_excluded = seeded.holders_excluded,
        workspaces = seeded.workspaces,
        "seeded root catalog units"
    );
    let migrated = migrate_personal_rows(&tx, scope, at_ms)?;
    tracing::info!(
        workspaces = migrated.workspaces,
        sources = migrated.sources,
        "migrated personal units to v2"
    );
    // The doc: remove successful old rows, no retained legacy live table.
    // The due index references the personal table, so it goes first.
    tx.execute_batch("DROP TABLE profile_reminder_due; DROP TABLE profile_personal_crdts;")
        .map_err(sql_failed)?;
    tx.pragma_update(None, "user_version", TO)
        .map_err(sql_failed)?;
    tx.commit().map_err(sql_failed)?;
    tracing::info!(
        database = "catalog.db",
        from = FROM,
        to = TO,
        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
        "root-catalog migration applied"
    );
    Ok(())
}

/// The Store's peer id, minted here if the Store has never opened this
/// catalog — the same way `Store::open` mints it, so the two never disagree.
fn ensure_peer_id(tx: &Connection) -> Result<u64> {
    let stored: Option<String> = tx
        .query_row(
            "SELECT value FROM store_state WHERE key = ?1",
            params![K_PEER_ID],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_failed)?;
    if let Some(peer) = stored.and_then(|value| value.parse::<u64>().ok()) {
        return Ok(peer);
    }
    let minted = mint_peer_id();
    tx.execute(
        "INSERT INTO store_state (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![K_PEER_ID, minted.to_string()],
    )
    .map_err(sql_failed)?;
    Ok(minted)
}

struct Seeded {
    profiles: usize,
    holders_excluded: usize,
    workspaces: usize,
}

/// §8.3: exact UUIDs, the Store's writer, revision 1, a receipt per subject.
/// Locally removed Profiles are seeded live (their `removed_at` is device
/// suppression, §2.1); Holder rows are excluded (§6.3); deleted Workspaces
/// carry one deterministic deletion fact.
fn seed_root_units(tx: &Connection, scope: UserScopeId, peer: u64, at_ms: i64) -> Result<Seeded> {
    let mut seeded = Seeded {
        profiles: 0,
        holders_excluded: 0,
        workspaces: 0,
    };
    let profiles: Vec<(String, Option<String>, Option<i64>)> = {
        let mut statement = tx
            .prepare("SELECT id, kind, removed_at FROM profiles ORDER BY created_at, id")
            .map_err(sql_failed)?;
        let rows = statement
            .query_map([], |row| Ok(Ok((row.get(0)?, row.get(1)?, row.get(2)?))))
            .map_err(sql_failed)?;
        collect(rows)?
    };
    let mut holders = std::collections::BTreeSet::new();
    for (id, kind, removed_at) in profiles {
        let profile = ProfileId::parse(&id)
            .ok_or_else(|| Error::new(Code::CorruptState, "invalid Profile id in catalog"))?;
        let kind = kind.map(|k| ProfileKind::parse(&k)).unwrap_or_default();
        let receipt = if kind == ProfileKind::Holder {
            holders.insert(profile);
            seeded.holders_excluded += 1;
            RootSeedReceipt::HolderExcluded { profile }
        } else {
            let unit = CatalogUnitRef::Profile { profile };
            insert_unit(tx, scope, unit, &seed_profile_unit(profile, peer)?)?;
            seeded.profiles += 1;
            RootSeedReceipt::Profile {
                profile,
                writer_peer_id: peer,
                legacy_removed_at_ms: removed_at,
            }
        };
        write_receipt(
            tx,
            scope,
            RECEIPT_ROOT_SEED,
            &profile.to_storage(),
            serde_json::to_string(&receipt),
            at_ms,
        )?;
    }
    let workspaces: Vec<(String, String, Option<i64>)> = {
        let mut statement = tx
            .prepare("SELECT id, owner, deleted_at FROM workspaces ORDER BY created_at, id")
            .map_err(sql_failed)?;
        let rows = statement
            .query_map([], |row| Ok(Ok((row.get(0)?, row.get(1)?, row.get(2)?))))
            .map_err(sql_failed)?;
        collect(rows)?
    };
    for (id, owner, deleted_at) in workspaces {
        let workspace = WorkspaceId::parse(&id)
            .ok_or_else(|| Error::new(Code::CorruptState, "invalid Workspace id in catalog"))?;
        let owner = ProfileId::parse(&owner)
            .ok_or_else(|| Error::new(Code::CorruptState, "invalid Workspace owner in catalog"))?;
        let unit = CatalogUnitRef::Workspace { workspace };
        insert_unit(
            tx,
            scope,
            unit,
            &seed_workspace_unit(workspace, owner, deleted_at, peer)?,
        )?;
        seeded.workspaces += 1;
        let receipt = RootSeedReceipt::Workspace {
            workspace,
            writer_peer_id: peer,
            placement_profile: owner,
            move_id: seeded_move_id(workspace),
            deleted_at_ms: deleted_at,
            deletion_id: deleted_at.map(|at| seeded_deletion_id(workspace, at)),
            owner_is_holder: holders.contains(&owner),
        };
        write_receipt(
            tx,
            scope,
            RECEIPT_ROOT_SEED,
            &workspace.to_storage(),
            serde_json::to_string(&receipt),
            at_ms,
        )?;
    }
    Ok(seeded)
}

fn insert_unit(
    tx: &Connection,
    scope: UserScopeId,
    unit: CatalogUnitRef,
    snapshot: &[u8],
) -> Result<()> {
    tx.execute(
        "INSERT INTO root_catalog_units (user_scope_id, unit_kind, subject_id, snapshot, revision)
         VALUES (?1, ?2, ?3, ?4, 1)",
        params![
            scope.to_storage(),
            unit.kind().as_str(),
            unit.subject_id(),
            snapshot
        ],
    )
    .map_err(sql_failed)?;
    Ok(())
}

fn write_receipt(
    tx: &Connection,
    scope: UserScopeId,
    kind: &str,
    subject: &str,
    receipt: serde_json::Result<String>,
    at_ms: i64,
) -> Result<()> {
    let text = receipt
        .map_err(|e| Error::new(Code::Internal, format!("receipt is not serializable: {e}")))?;
    tx.execute(
        "INSERT INTO root_catalog_migration_receipts
           (user_scope_id, kind, subject_id, receipt, at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![scope.to_storage(), kind, subject, text, at_ms],
    )
    .map_err(sql_failed)?;
    Ok(())
}

struct Migrated {
    workspaces: usize,
    sources: usize,
}

/// §5.2 step 5: every `profile_personal_crdts` row, grouped by Workspace,
/// through `migrate_personal_sources`; one `workspace_personal_crdts` row,
/// its due rows and its receipt per Workspace. All Workspaces migrate or
/// the caller's transaction never commits.
fn migrate_personal_rows(tx: &Connection, scope: UserScopeId, at_ms: i64) -> Result<Migrated> {
    let rows: Vec<(String, String, Vec<u8>)> = {
        let mut statement = tx
            .prepare(
                "SELECT owner_profile_id, workspace_id, snapshot FROM profile_personal_crdts
                 ORDER BY workspace_id, owner_profile_id",
            )
            .map_err(sql_failed)?;
        let rows = statement
            .query_map([], |row| Ok(Ok((row.get(0)?, row.get(1)?, row.get(2)?))))
            .map_err(sql_failed)?;
        collect(rows)?
    };
    let mut by_workspace: BTreeMap<WorkspaceId, Vec<LegacyPersonalSource>> = BTreeMap::new();
    for (owner, workspace, snapshot) in rows {
        let owner = ProfileId::parse(&owner).ok_or_else(|| {
            Error::new(Code::CorruptState, "invalid personal owner id in catalog")
        })?;
        let workspace = WorkspaceId::parse(&workspace).ok_or_else(|| {
            Error::new(
                Code::CorruptState,
                "invalid personal workspace id in catalog",
            )
        })?;
        by_workspace
            .entry(workspace)
            .or_default()
            .push(LegacyPersonalSource {
                owner,
                workspace,
                snapshot,
            });
    }
    let mut migrated = Migrated {
        workspaces: 0,
        sources: 0,
    };
    for (workspace, sources) in by_workspace {
        migrated.sources += sources.len();
        let unit = migrate_personal_sources(workspace, sources, mint_peer_id())?;
        tx.execute(
            "INSERT INTO workspace_personal_crdts
               (user_scope_id, workspace_id, snapshot, indexed_version, writer_peer_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                scope.to_storage(),
                workspace.to_storage(),
                unit.snapshot,
                unit.indexed_version,
                peer_to_i64(unit.writer_peer_id)
            ],
        )
        .map_err(sql_failed)?;
        write_due_rows(tx, scope, workspace, &unit.due_rows)?;
        write_receipt(
            tx,
            scope,
            RECEIPT_PERSONAL_V2,
            &workspace.to_storage(),
            serde_json::to_string(&unit.receipt),
            at_ms,
        )?;
        migrated.workspaces += 1;
    }
    Ok(migrated)
}

// ------------------------------------------------------------------ rows

/// SQLite has no u64; a PeerID is stored as the i64 with the same bits.
pub(super) fn peer_to_i64(peer: u64) -> i64 {
    peer as i64
}

pub(super) fn peer_from_i64(peer: i64) -> u64 {
    peer as u64
}

pub(super) fn local_user_scope(connection: &Connection) -> Result<UserScopeId> {
    let stored: Option<String> = connection
        .query_row(
            "SELECT value FROM store_state WHERE key = ?1",
            params![K_LOCAL_USER_SCOPE],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_failed)?;
    stored
        .as_deref()
        .and_then(UserScopeId::parse)
        .ok_or_else(|| Error::new(Code::CorruptState, "the local user scope is missing"))
}

pub(super) fn root_catalog_unit(
    connection: &Connection,
    scope: UserScopeId,
    unit: CatalogUnitRef,
) -> Result<Option<RootCatalogUnitRow>> {
    connection
        .query_row(
            "SELECT snapshot, revision FROM root_catalog_units
             WHERE user_scope_id = ?1 AND unit_kind = ?2 AND subject_id = ?3",
            params![scope.to_storage(), unit.kind().as_str(), unit.subject_id()],
            |row| {
                Ok(RootCatalogUnitRow {
                    snapshot: row.get(0)?,
                    revision: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(sql_failed)
}

pub(super) fn root_catalog_units(
    connection: &Connection,
    scope: UserScopeId,
    kind: CatalogUnitKind,
) -> Result<Vec<(CatalogUnitRef, RootCatalogUnitRow)>> {
    let mut statement = connection
        .prepare(
            "SELECT subject_id, snapshot, revision FROM root_catalog_units
             WHERE user_scope_id = ?1 AND unit_kind = ?2 ORDER BY subject_id",
        )
        .map_err(sql_failed)?;
    let rows = statement
        .query_map(params![scope.to_storage(), kind.as_str()], |row| {
            let subject: String = row.get(0)?;
            let unit = RootCatalogUnitRow {
                snapshot: row.get(1)?,
                revision: row.get(2)?,
            };
            Ok(CatalogUnitRef::parse(kind.as_str(), &subject)
                .ok_or_else(|| {
                    Error::new(
                        Code::CorruptState,
                        "root catalog subject is not a canonical UUID",
                    )
                })
                .map(|address| (address, unit)))
        })
        .map_err(sql_failed)?;
    collect(rows)
}

/// `None` inserts a row that must not exist; `Some(r)` replaces a row at
/// exactly `r` and stamps `r + 1`. The caller's transaction decides atomicity.
pub(super) fn commit_root_catalog_unit(
    tx: &Connection,
    scope: UserScopeId,
    commit: &RootCatalogUnitCommit,
) -> Result<()> {
    if commit.snapshot.is_empty() {
        return Err(Error::new(
            Code::InvalidRequest,
            "a root catalog unit snapshot is never empty",
        ));
    }
    let kind = commit.unit.kind().as_str();
    let subject = commit.unit.subject_id();
    match commit.expected_revision {
        None => {
            let inserted = tx
                .execute(
                    "INSERT INTO root_catalog_units
                       (user_scope_id, unit_kind, subject_id, snapshot, revision)
                     VALUES (?1, ?2, ?3, ?4, 1)
                     ON CONFLICT(user_scope_id, unit_kind, subject_id) DO NOTHING",
                    params![scope.to_storage(), kind, subject, commit.snapshot],
                )
                .map_err(sql_failed)?;
            if inserted != 1 {
                return Err(Error::new(
                    Code::IdempotencyConflict,
                    "root catalog unit already exists",
                ));
            }
        }
        Some(expected) => {
            let changed = tx
                .execute(
                    "UPDATE root_catalog_units SET snapshot = ?4, revision = ?5 + 1
                     WHERE user_scope_id = ?1 AND unit_kind = ?2 AND subject_id = ?3
                       AND revision = ?5",
                    params![scope.to_storage(), kind, subject, commit.snapshot, expected],
                )
                .map_err(sql_failed)?;
            if changed != 1 {
                return Err(Error::new(
                    Code::IdempotencyConflict,
                    "root catalog unit revision differs from the one read",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn root_catalog_overlay(
    connection: &Connection,
    scope: UserScopeId,
    unit: CatalogUnitRef,
) -> Result<Option<RootCatalogOverlay>> {
    connection
        .query_row(
            "SELECT base_vv, private_snapshot, writer_peer_id FROM root_catalog_overlays
             WHERE user_scope_id = ?1 AND unit_kind = ?2 AND subject_id = ?3",
            params![scope.to_storage(), unit.kind().as_str(), unit.subject_id()],
            |row| {
                Ok(RootCatalogOverlay {
                    base_vv: row.get(0)?,
                    private_snapshot: row.get(1)?,
                    writer_peer_id: peer_from_i64(row.get(2)?),
                })
            },
        )
        .optional()
        .map_err(sql_failed)
}

pub(super) fn root_catalog_overlays(
    connection: &Connection,
    scope: UserScopeId,
) -> Result<Vec<(CatalogUnitRef, RootCatalogOverlay)>> {
    let mut statement = connection
        .prepare(
            "SELECT unit_kind, subject_id, base_vv, private_snapshot, writer_peer_id
             FROM root_catalog_overlays
             WHERE user_scope_id = ?1 ORDER BY unit_kind, subject_id",
        )
        .map_err(sql_failed)?;
    let rows = statement
        .query_map(params![scope.to_storage()], |row| {
            let kind: String = row.get(0)?;
            let subject: String = row.get(1)?;
            let overlay = RootCatalogOverlay {
                base_vv: row.get(2)?,
                private_snapshot: row.get(3)?,
                writer_peer_id: peer_from_i64(row.get(4)?),
            };
            Ok(CatalogUnitRef::parse(&kind, &subject)
                .ok_or_else(|| {
                    Error::new(
                        Code::CorruptState,
                        "root catalog overlay subject is not a canonical UUID",
                    )
                })
                .map(|address| (address, overlay)))
        })
        .map_err(sql_failed)?;
    collect(rows)
}

pub(super) fn commit_root_catalog_overlay(
    connection: &Connection,
    scope: UserScopeId,
    unit: CatalogUnitRef,
    overlay: &RootCatalogOverlay,
) -> Result<()> {
    connection
        .execute(
            "INSERT INTO root_catalog_overlays
               (user_scope_id, unit_kind, subject_id, base_vv, private_snapshot, writer_peer_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(user_scope_id, unit_kind, subject_id) DO UPDATE SET
               base_vv = excluded.base_vv,
               private_snapshot = excluded.private_snapshot,
               writer_peer_id = excluded.writer_peer_id",
            params![
                scope.to_storage(),
                unit.kind().as_str(),
                unit.subject_id(),
                overlay.base_vv,
                overlay.private_snapshot,
                peer_to_i64(overlay.writer_peer_id)
            ],
        )
        .map_err(sql_failed)?;
    Ok(())
}

pub(super) fn clear_root_catalog_overlay(
    connection: &Connection,
    scope: UserScopeId,
    unit: CatalogUnitRef,
) -> Result<()> {
    connection
        .execute(
            "DELETE FROM root_catalog_overlays
             WHERE user_scope_id = ?1 AND unit_kind = ?2 AND subject_id = ?3",
            params![scope.to_storage(), unit.kind().as_str(), unit.subject_id()],
        )
        .map_err(sql_failed)?;
    Ok(())
}

pub(super) fn workspace_personal_unit(
    connection: &Connection,
    scope: UserScopeId,
    workspace: WorkspaceId,
) -> Result<Option<WorkspacePersonalUnitState>> {
    connection
        .query_row(
            "SELECT snapshot, indexed_version, writer_peer_id FROM workspace_personal_crdts
             WHERE user_scope_id = ?1 AND workspace_id = ?2",
            params![scope.to_storage(), workspace.to_storage()],
            |row| {
                Ok(WorkspacePersonalUnitState {
                    snapshot: row.get(0)?,
                    indexed_version: row.get(1)?,
                    writer_peer_id: peer_from_i64(row.get(2)?),
                })
            },
        )
        .optional()
        .map_err(sql_failed)
}

pub(super) fn workspace_personal_units(
    connection: &Connection,
    scope: UserScopeId,
) -> Result<Vec<(WorkspaceId, WorkspacePersonalUnitState)>> {
    let mut statement = connection
        .prepare(
            "SELECT workspace_id, snapshot, indexed_version, writer_peer_id
             FROM workspace_personal_crdts WHERE user_scope_id = ?1 ORDER BY workspace_id",
        )
        .map_err(sql_failed)?;
    let rows = statement
        .query_map(params![scope.to_storage()], |row| {
            let workspace: String = row.get(0)?;
            let state = WorkspacePersonalUnitState {
                snapshot: row.get(1)?,
                indexed_version: row.get(2)?,
                writer_peer_id: peer_from_i64(row.get(3)?),
            };
            Ok(WorkspaceId::parse(&workspace)
                .ok_or_else(|| {
                    Error::new(Code::CorruptState, "personal workspace id is not a uuid")
                })
                .map(|workspace| (workspace, state)))
        })
        .map_err(sql_failed)?;
    collect(rows)
}

/// Changed bytes clear the due-index stamp atomically, as the v1 commit did.
/// The writer is persisted with the first commit and never changes after.
pub(super) fn commit_workspace_personal_unit(
    connection: &Connection,
    scope: UserScopeId,
    workspace: WorkspaceId,
    snapshot: &[u8],
    writer_peer_id: u64,
) -> Result<()> {
    connection
        .execute(
            "INSERT INTO workspace_personal_crdts
               (user_scope_id, workspace_id, snapshot, indexed_version, writer_peer_id)
             VALUES (?1, ?2, ?3, NULL, ?4)
             ON CONFLICT(user_scope_id, workspace_id) DO UPDATE SET
               snapshot = excluded.snapshot, indexed_version = NULL
             WHERE workspace_personal_crdts.snapshot != excluded.snapshot",
            params![
                scope.to_storage(),
                workspace.to_storage(),
                snapshot,
                peer_to_i64(writer_peer_id)
            ],
        )
        .map_err(sql_failed)?;
    Ok(())
}

/// The v1 compare-and-set on the exact source snapshot, re-keyed. Inside
/// the caller's `IMMEDIATE` transaction so the compare and the write are one
/// write-serialized step.
pub(super) fn replace_workspace_due_index(
    tx: &Connection,
    scope: UserScopeId,
    workspace: WorkspaceId,
    source_snapshot: &[u8],
    indexed_version: &[u8],
    rows: &[WorkspaceReminderDue],
) -> Result<bool> {
    if rows.iter().any(|row| row.workspace != workspace) {
        return Err(Error::new(
            Code::InvalidRequest,
            "reminder index scope mismatch",
        ));
    }
    let matched = tx
        .execute(
            "UPDATE workspace_personal_crdts SET indexed_version = ?4
             WHERE user_scope_id = ?1 AND workspace_id = ?2 AND snapshot = ?3",
            params![
                scope.to_storage(),
                workspace.to_storage(),
                source_snapshot,
                indexed_version
            ],
        )
        .map_err(sql_failed)?;
    if matched == 0 {
        return Ok(false);
    }
    tx.execute(
        "DELETE FROM workspace_reminder_due WHERE user_scope_id = ?1 AND workspace_id = ?2",
        params![scope.to_storage(), workspace.to_storage()],
    )
    .map_err(sql_failed)?;
    write_due_rows(tx, scope, workspace, rows)?;
    Ok(true)
}

fn write_due_rows(
    tx: &Connection,
    scope: UserScopeId,
    workspace: WorkspaceId,
    rows: &[WorkspaceReminderDue],
) -> Result<()> {
    let mut insert = tx
        .prepare(
            "INSERT INTO workspace_reminder_due
               (user_scope_id, workspace_id, reminder_id, due_at_ms) VALUES (?1, ?2, ?3, ?4)",
        )
        .map_err(sql_failed)?;
    for row in rows {
        insert
            .execute(params![
                scope.to_storage(),
                workspace.to_storage(),
                row.reminder.to_storage(),
                row.due_at_ms
            ])
            .map_err(sql_failed)?;
    }
    Ok(())
}

pub(super) fn workspace_reminder_due(
    connection: &Connection,
    scope: UserScopeId,
) -> Result<Vec<WorkspaceReminderDue>> {
    let mut statement = connection
        .prepare(
            "SELECT workspace_id, reminder_id, due_at_ms FROM workspace_reminder_due
             WHERE user_scope_id = ?1 ORDER BY due_at_ms, workspace_id, reminder_id",
        )
        .map_err(sql_failed)?;
    let rows = statement
        .query_map(params![scope.to_storage()], |row| {
            let workspace: String = row.get(0)?;
            let reminder: String = row.get(1)?;
            let due_at_ms = row.get(2)?;
            Ok((|| {
                Ok(WorkspaceReminderDue {
                    workspace: WorkspaceId::parse(&workspace).ok_or_else(|| {
                        Error::new(Code::CorruptState, "reminder workspace id is not a uuid")
                    })?,
                    reminder: ReminderId::parse(&reminder).ok_or_else(|| {
                        Error::new(Code::CorruptState, "reminder id is not a uuid")
                    })?,
                    due_at_ms,
                })
            })())
        })
        .map_err(sql_failed)?;
    collect(rows)
}

/// Whether any personal row of `workspace` remains, under any scope: the
/// `RESTRICT` guard `purge_workspace` and `forget_workspace` keep (§5.3).
pub(super) fn any_personal_unit(connection: &Connection, workspace: WorkspaceId) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM workspace_personal_crdts WHERE workspace_id = ?1)",
            params![workspace.to_storage()],
            |row| row.get(0),
        )
        .map_err(sql_failed)
}

/// A personal row of `workspace` under a scope other than `scope` holds the
/// content: this purge is not authorized to retire another user's unit.
/// Rows under `scope` itself are no hold — `retire_workspace_personal_unit`
/// is their authorized job. In a single-scope local catalog the hold case
/// is another `user_scope_id`, which nothing writes yet; it is answered
/// honestly all the same.
pub(super) fn personal_unit_hold(
    connection: &Connection,
    scope: UserScopeId,
    workspace: WorkspaceId,
) -> Result<Option<PurgeHold>> {
    let foreign: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM workspace_personal_crdts
                           WHERE workspace_id = ?1 AND user_scope_id != ?2)",
            params![workspace.to_storage(), scope.to_storage()],
            |row| row.get(0),
        )
        .map_err(sql_failed)?;
    Ok(foreign.then_some(PurgeHold::ForeignScope))
}

/// The authorized retirement (§5.3): exactly one scope's personal row of
/// one Workspace, with its due rows. The due rows go first and explicitly
/// rather than by cascade, so the statement says what it drops. Inside the
/// caller's transaction.
pub(super) fn retire_workspace_personal_unit(
    tx: &Connection,
    scope: UserScopeId,
    workspace: WorkspaceId,
) -> Result<bool> {
    tx.execute(
        "DELETE FROM workspace_reminder_due WHERE user_scope_id = ?1 AND workspace_id = ?2",
        params![scope.to_storage(), workspace.to_storage()],
    )
    .map_err(sql_failed)?;
    let retired = tx
        .execute(
            "DELETE FROM workspace_personal_crdts WHERE user_scope_id = ?1 AND workspace_id = ?2",
            params![scope.to_storage(), workspace.to_storage()],
        )
        .map_err(sql_failed)?;
    Ok(retired == 1)
}
