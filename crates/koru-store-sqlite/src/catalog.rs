// SPDX-License-Identifier: MPL-2.0

//! `catalog.db` — profiles, the workspace index, lineage, credentials, state.
//!
//! **The only database opened at startup.** Everything else is opened in
//! response to a request, which is what keeps store start-up independent of how
//! many workspaces exist or how much is in them.
//!
//! Knows nothing about Loro. A workspace row here is a name and a pointer; the
//! CRDT state it points at lives in its own database and is never read from
//! this one.
//!
//! ## Two things stored deliberately oddly
//!
//! - **A credential is stored as a hash, never as a token.** A catalog that
//!   leaked would otherwise be a set of working keys. The comparison happens in
//!   SQL on the hash, so the token never has to be reconstructed to be checked.
//! - **`locale` is `TEXT`, validated on the way in and again on the way out.**
//!   A row written by a future build with a tag this one cannot parse falls
//!   back to `und` rather than failing the read: a profile is not unusable
//!   because its language is unfamiliar.

use koru_store_core::ProfileAccountBinding;
use koru_store_core::errors::{Code, Error, Result};
use koru_store_core::ids::{ClientId, DocumentId, ProfileId, ReminderId, WorkspaceId};
use koru_store_core::model::{
    ApiClient, Lineage, LineageKind, Profile, ProfileKind, Scope, Timestamp, Workspace,
};
use koru_store_core::ports::{
    Catalog, LocalProfileRemoval, LocalProfileRestoration, ProfileCatalogState, ProfileCrdtState,
    ProfileLifecycleInventoryItem, ProfileUnitState, ReminderDue, SyncDocumentDigest,
    SyncStateEvent,
};
use koru_store_core::profile_units::ProfileUnitRef;
use koru_store_core::text::Locale;
use koru_store_core::transport::{
    self, TransportMode, TransportModeCommit, TransportModeGroup, TransportModeRequest,
    TransportModeState, TransportSubject,
};
use koru_store_core::version::CATALOG_SCHEMA_VERSION;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::db::{self, Migration, sql_failed};

/// Crate-visible so a fixture elsewhere can stamp a catalog at an **older**
/// version and then let the ordinary open migrate it — the only honest way to
/// test what an existing installation meets.
pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        to: 1,
        name: "catalog: profiles and workspaces",
        columns: &[],
        sql: "
    CREATE TABLE profiles (
      id          TEXT PRIMARY KEY,
      name        TEXT    NOT NULL,
      purpose     TEXT,
      avatar      TEXT,
      account     TEXT,
      locale      TEXT    NOT NULL,
      created_at  INTEGER NOT NULL
    );

    CREATE TABLE workspaces (
      id             TEXT PRIMARY KEY,
      owner          TEXT    NOT NULL REFERENCES profiles(id),
      name           TEXT    NOT NULL,
      created_at     INTEGER NOT NULL,
      last_opened_at INTEGER
    );
    CREATE INDEX workspaces_owner ON workspaces(owner);

    CREATE TABLE workspace_lineage (
      workspace    TEXT    NOT NULL,
      derived_from TEXT    NOT NULL,
      kind         TEXT    NOT NULL,
      at           INTEGER NOT NULL,
      PRIMARY KEY (workspace, derived_from, at)
    );

    CREATE TABLE api_clients (
      id         TEXT PRIMARY KEY,
      name       TEXT    NOT NULL,
      token_hash BLOB    NOT NULL UNIQUE,
      scopes     TEXT    NOT NULL,
      created_at INTEGER NOT NULL,
      revoked_at INTEGER
    );

    CREATE TABLE store_state (
      key   TEXT PRIMARY KEY,
      value TEXT NOT NULL
    );

    -- Which workspace a document lives in. Derived — rebuildable by walking
    -- the workspaces — so a stale row costs a lookup and never a document.
    -- It exists so a globally unique document id can be named on its own,
    -- which is what lets a room be `document:<id>` rather than carrying the
    -- workspace around with it.
    CREATE TABLE document_locations (
      document_id  TEXT PRIMARY KEY,
      workspace_id TEXT NOT NULL
    );
    CREATE INDEX document_locations_workspace ON document_locations(workspace_id);
    ",
    },
    Migration {
        to: 2,
        name: "workspaces: icon and color",
        // How a workspace appears in a switcher. Columns rather than a JSON blob:
        // they are four small facts a listing sorts and filters on, and `archived`
        // in particular is a `WHERE` clause waiting to happen.
        //
        // `pinned` and `archived` default to 0 rather than being nullable — the
        // absence of a pin is *not pinned*, and a three-valued flag is a branch
        // somebody will forget.
        //
        // Declared as columns rather than written as SQL, so appending a fifth
        // one here later cannot repeat what `documents.trashed_by` did: a
        // column added to a migration that has already run reaches no database
        // stamped with that version. A new migration adds it, and this one
        // stays a no-op on the databases that have it.
        sql: "",
        columns: &[
            ("workspaces", "icon", "TEXT"),
            ("workspaces", "color", "TEXT"),
            ("workspaces", "pinned", "INTEGER NOT NULL DEFAULT 0"),
            ("workspaces", "archived", "INTEGER NOT NULL DEFAULT 0"),
        ],
    },
    Migration {
        to: 3,
        name: "workspaces: deleted_at",
        sql: "",
        columns: &[("workspaces", "deleted_at", "INTEGER")],
    },
    Migration {
        to: 4,
        name: "workspaces: position",
        // Seed from the old sort key, so upgrading changes no visible order.
        sql: "
    ALTER TABLE workspaces ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
    UPDATE workspaces SET position = created_at;
    ",
        columns: &[],
    },
    Migration {
        to: 5,
        name: "profile_crdts",
        sql: "
    CREATE TABLE profile_crdts (
      profile_id TEXT PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,
      snapshot   BLOB NOT NULL
    );
    ",
        columns: &[],
    },
    Migration {
        to: 6,
        name: "sync_document_digests",
        sql: "
    CREATE TABLE sync_document_digests (
      workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
      document_id  TEXT NOT NULL,
      source_revision BLOB NOT NULL,
      digest       BLOB NOT NULL,
      PRIMARY KEY (workspace_id, document_id)
    );
    ",
        columns: &[],
    },
    Migration {
        to: 7,
        name: "catalog step 7",
        sql: include_str!("../migrations/007_catalog_profile_appearance_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 8,
        name: "catalog step 8",
        sql: include_str!("../migrations/008_catalog_sync_state_events_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 9,
        name: "catalog step 9",
        sql: include_str!("../migrations/009_catalog_local_profile_removal_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 10,
        name: "profiles: removed_at and kind",
        sql: include_str!("../migrations/015_catalog_profile_lineage_reconcile_migration.sql"),
        // OAuth and dev independently stamped schema 9: one added removed_at,
        // the other kind. Reconcile either lineage without repeating an ALTER.
        columns: &[
            ("profiles", "removed_at", "INTEGER"),
            ("profiles", "kind", "TEXT"),
        ],
    },
    Migration {
        to: 11,
        name: "018_catalog_profile_units_migration.sql",
        sql: include_str!("../migrations/018_catalog_profile_units_migration.sql"),
        columns: &[],
    },
    Migration {
        to: 12,
        name: "024_catalog_transport_modes_migration.sql",
        sql: include_str!("../migrations/024_catalog_transport_modes_migration.sql"),
        columns: &[],
    },
];

pub struct SqliteCatalog {
    connection: Connection,
}

impl SqliteCatalog {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Self::from_connection(db::open(path)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::from_connection(db::open_in_memory()?)
    }

    /// One named in-memory database that several catalogs can open in turn —
    /// how a test restarts a `Store` over the bytes the last one wrote. See
    /// [`db::open_shared_in_memory`] for why the ordinary in-memory open
    /// cannot do that.
    #[cfg(test)]
    pub fn shared_in_memory(name: &str) -> Result<Self> {
        Self::from_connection(db::open_shared_in_memory(name)?)
    }

    fn from_connection(mut connection: Connection) -> Result<Self> {
        db::migrate(
            &mut connection,
            "catalog.db",
            MIGRATIONS,
            CATALOG_SCHEMA_VERSION,
        )?;
        Ok(Self { connection })
    }

    pub fn schema_version(&self) -> Result<i64> {
        db::user_version(&self.connection)
    }

    pub fn size_bytes(&self) -> Result<u64> {
        db::size_bytes(&self.connection)
    }

    pub fn quick_check(&self) -> Result<bool> {
        db::quick_check(&self.connection)
    }
}

impl Catalog for SqliteCatalog {
    // -------------------------------------------------------------- profiles

    fn put_profile(&mut self, profile: &Profile) -> Result<()> {
        // A transaction for what looks like one statement: the projection and
        // the Profile's default transport mode row have to become visible
        // together, or a Profile exists for a moment with no answer to "may
        // this move data" and every read of it is a storage error.
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_failed)?;
        write_profile_projection(&tx, profile)?;
        tx.commit().map_err(sql_failed)
    }

    fn profile(&self, id: ProfileId) -> Result<Option<Profile>> {
        self.connection
            .query_row(
                "SELECT id, name, purpose, avatar, account, locale, created_at, icon, color, kind
                 FROM profiles WHERE id = ?1 AND removed_at IS NULL",
                params![id.to_storage()],
                read_profile,
            )
            .optional()
            .map_err(sql_failed)?
            .transpose()
    }

    fn profile_including_removed(&self, id: ProfileId) -> Result<Option<Profile>> {
        self.connection
            .query_row(
                "SELECT id, name, purpose, avatar, account, locale, created_at, icon, color, kind
                 FROM profiles WHERE id = ?1",
                params![id.to_storage()],
                read_profile,
            )
            .optional()
            .map_err(sql_failed)?
            .transpose()
    }

    fn profiles(&self) -> Result<Vec<Profile>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT id, name, purpose, avatar, account, locale, created_at, icon, color, kind
                 FROM profiles WHERE removed_at IS NULL ORDER BY created_at, id",
            )
            .map_err(sql_failed)?;
        let rows = stmt.query_map([], read_profile).map_err(sql_failed)?;
        collect(rows)
    }

    fn profiles_including_removed(&self) -> Result<Vec<Profile>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT id, name, purpose, avatar, account, locale, created_at, icon, color, kind
                 FROM profiles ORDER BY created_at, id",
            )
            .map_err(sql_failed)?;
        let rows = stmt.query_map([], read_profile).map_err(sql_failed)?;
        collect(rows)
    }

    fn profile_lifecycle_inventory(&self) -> Result<Vec<ProfileLifecycleInventoryItem>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT id, account, removed_at FROM profiles
                 WHERE account IS NOT NULL ORDER BY created_at, id",
            )
            .map_err(sql_failed)?;
        let rows = stmt
            .query_map([], |row| {
                let profile = row.get::<_, String>(0)?;
                let stored = row.get::<_, String>(1)?;
                let removed_at = row.get::<_, Option<i64>>(2)?;
                Ok((|| {
                    Ok(ProfileLifecycleInventoryItem {
                        profile: ProfileId::parse(&profile).ok_or_else(|| {
                            Error::new(Code::CorruptState, "invalid Profile id in catalog")
                        })?,
                        account_binding: ProfileAccountBinding::parse_storage(&stored)?,
                        removed_at: removed_at.map(Timestamp::from_millis),
                    })
                })())
            })
            .map_err(sql_failed)?;
        collect(rows)
    }

    fn profile_catalog_state(&self, id: ProfileId) -> Result<Option<ProfileCatalogState>> {
        self.connection
            .query_row(
                "SELECT removed_at IS NOT NULL FROM profiles WHERE id = ?1",
                params![id.to_storage()],
                |row| {
                    Ok(if row.get::<_, bool>(0)? {
                        ProfileCatalogState::Removed
                    } else {
                        ProfileCatalogState::Active
                    })
                },
            )
            .optional()
            .map_err(sql_failed)
    }

    fn remove_profile_local(
        &mut self,
        id: ProfileId,
        at: Timestamp,
    ) -> Result<LocalProfileRemoval> {
        // IMMEDIATE makes the empty check and marker one write-serialized decision.
        // The Profile, CRDT, and every Workspace row deliberately remain in place.
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_failed)?;
        let removed_at: Option<Option<i64>> = tx
            .query_row(
                "SELECT removed_at FROM profiles WHERE id = ?1",
                params![id.to_storage()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_failed)?;
        match removed_at {
            None => return Err(Error::bare(Code::ProfileNotFound)),
            Some(Some(_)) => return Ok(LocalProfileRemoval::AlreadyRemoved),
            Some(None) => {}
        }
        let active: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM workspaces WHERE owner = ?1 AND deleted_at IS NULL",
                params![id.to_storage()],
                |row| row.get(0),
            )
            .map_err(sql_failed)?;
        if active != 0 {
            return Err(Error::bare(Code::ProfileNotEmpty));
        }
        tx.execute(
            "UPDATE profiles SET removed_at = ?2 WHERE id = ?1",
            params![id.to_storage(), at.millis()],
        )
        .map_err(sql_failed)?;
        tx.commit().map_err(sql_failed)?;
        Ok(LocalProfileRemoval::Removed)
    }

    fn restore_profile_local(&mut self, id: ProfileId) -> Result<LocalProfileRestoration> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_failed)?;
        let removed_at: Option<Option<i64>> = tx
            .query_row(
                "SELECT removed_at FROM profiles WHERE id = ?1",
                params![id.to_storage()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_failed)?;
        match removed_at {
            None => return Err(Error::bare(Code::ProfileNotFound)),
            Some(None) => return Ok(LocalProfileRestoration::AlreadyActive),
            Some(Some(_)) => {}
        }
        tx.execute(
            "UPDATE profiles SET removed_at = NULL WHERE id = ?1",
            params![id.to_storage()],
        )
        .map_err(sql_failed)?;
        tx.commit().map_err(sql_failed)?;
        Ok(LocalProfileRestoration::Restored)
    }

    fn profiles_removed_before(&self, before: Timestamp) -> Result<Vec<ProfileId>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT id FROM profiles
                 WHERE removed_at IS NOT NULL AND removed_at <= ?1
                 ORDER BY removed_at, id",
            )
            .map_err(sql_failed)?;
        let rows = stmt
            .query_map(params![before.millis()], |row| {
                let profile = row.get::<_, String>(0)?;
                Ok(ProfileId::parse(&profile)
                    .ok_or_else(|| Error::new(Code::CorruptState, "invalid Profile id in catalog")))
            })
            .map_err(sql_failed)?;
        collect(rows)
    }

    fn profile_crdt(&self, id: ProfileId) -> Result<Option<ProfileCrdtState>> {
        self.connection
            .query_row(
                "SELECT snapshot FROM profile_crdts WHERE profile_id = ?1",
                params![id.to_storage()],
                |row| {
                    Ok(ProfileCrdtState {
                        profile: id,
                        snapshot: row.get(0)?,
                    })
                },
            )
            .optional()
            .map_err(sql_failed)
    }

    fn commit_profile_crdt(&mut self, profile: &Profile, snapshot: &[u8]) -> Result<()> {
        let transaction = self.connection.transaction().map_err(sql_failed)?;
        write_profile_projection(&transaction, profile)?;
        transaction
            .execute(
                "INSERT INTO profile_crdts (profile_id, snapshot) VALUES (?1, ?2)
                 ON CONFLICT(profile_id) DO UPDATE SET snapshot = excluded.snapshot",
                params![profile.id.to_storage(), snapshot],
            )
            .map_err(sql_failed)?;
        transaction.commit().map_err(sql_failed)
    }

    fn initialize_profile_units(
        &mut self,
        profile: &Profile,
        identity: &[u8],
        private: &[u8],
    ) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_failed)?;
        let (has_identity, has_private): (bool, bool) = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM profile_identity_crdts WHERE profile_id = ?1),
                    EXISTS(SELECT 1 FROM profile_private_crdts WHERE profile_id = ?1)",
                params![profile.id.to_storage()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(sql_failed)?;
        if has_identity {
            if !has_private {
                return Err(Error::new(
                    Code::CorruptState,
                    "Profile private authority is missing",
                ));
            }
            // The caller reloads retained authority. A competing initializer's
            // candidate must not overwrite its bytes or listing projection.
            return Ok(());
        }
        write_identity_projection(&tx, profile)?;
        tx.execute(
            "INSERT INTO profile_identity_crdts (profile_id, snapshot) VALUES (?1, ?2)",
            params![profile.id.to_storage(), identity],
        )
        .map_err(sql_failed)?;
        if !has_private {
            tx.execute(
                "INSERT INTO profile_private_crdts (profile_id, snapshot) VALUES (?1, ?2)",
                params![profile.id.to_storage(), private],
            )
            .map_err(sql_failed)?;
        }
        tx.commit().map_err(sql_failed)
    }

    fn profile_unit(&self, unit: ProfileUnitRef) -> Result<Option<ProfileUnitState>> {
        let sql = match unit {
            ProfileUnitRef::LegacyPrivate { .. } => {
                "SELECT snapshot, NULL FROM profile_crdts WHERE profile_id = ?1"
            }
            ProfileUnitRef::Identity { .. } => {
                "SELECT snapshot, NULL FROM profile_identity_crdts WHERE profile_id = ?1"
            }
            ProfileUnitRef::Private { .. } => {
                "SELECT snapshot, NULL FROM profile_private_crdts WHERE profile_id = ?1"
            }
            ProfileUnitRef::Personal { .. } => {
                "SELECT snapshot, indexed_version FROM profile_personal_crdts
                 WHERE owner_profile_id = ?1 AND workspace_id = ?2"
            }
        };
        let read = |row: &rusqlite::Row<'_>| {
            Ok(ProfileUnitState {
                unit,
                snapshot: row.get(0)?,
                indexed_version: row.get(1)?,
            })
        };
        let result = if let ProfileUnitRef::Personal { owner, workspace } = unit {
            self.connection.query_row(
                sql,
                params![owner.to_storage(), workspace.to_storage()],
                read,
            )
        } else {
            self.connection
                .query_row(sql, params![unit.profile().to_storage()], read)
        };
        result.optional().map_err(sql_failed)
    }

    fn profile_units(&self, profile: ProfileId) -> Result<Vec<ProfileUnitState>> {
        let mut units = Vec::new();
        for unit in [
            ProfileUnitRef::LegacyPrivate { profile },
            ProfileUnitRef::Identity { profile },
            ProfileUnitRef::Private { profile },
        ] {
            if let Some(state) = self.profile_unit(unit)? {
                units.push(state);
            }
        }
        let mut statement = self
            .connection
            .prepare(
                "SELECT workspace_id, snapshot, indexed_version FROM profile_personal_crdts
             WHERE owner_profile_id = ?1 ORDER BY workspace_id",
            )
            .map_err(sql_failed)?;
        let rows = statement
            .query_map(params![profile.to_storage()], |row| {
                let workspace: String = row.get(0)?;
                let snapshot = row.get(1)?;
                let indexed_version = row.get(2)?;
                Ok(WorkspaceId::parse(&workspace)
                    .ok_or_else(|| {
                        Error::new(Code::CorruptState, "personal workspace id is not a uuid")
                    })
                    .map(|workspace| ProfileUnitState {
                        unit: ProfileUnitRef::Personal {
                            owner: profile,
                            workspace,
                        },
                        snapshot,
                        indexed_version,
                    }))
            })
            .map_err(sql_failed)?;
        units.extend(collect(rows)?);
        Ok(units)
    }

    fn workspace_has_personal_units(&self, workspace: WorkspaceId) -> Result<bool> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM profile_personal_crdts WHERE workspace_id = ?1)",
                params![workspace.to_storage()],
                |row| row.get(0),
            )
            .map_err(sql_failed)
    }

    fn commit_profile_identity(&mut self, profile: &Profile, snapshot: &[u8]) -> Result<()> {
        let tx = self.connection.transaction().map_err(sql_failed)?;
        write_identity_projection(&tx, profile)?;
        tx.execute(
            "INSERT INTO profile_identity_crdts (profile_id, snapshot) VALUES (?1, ?2)
             ON CONFLICT(profile_id) DO UPDATE SET snapshot = excluded.snapshot",
            params![profile.id.to_storage(), snapshot],
        )
        .map_err(sql_failed)?;
        tx.commit().map_err(sql_failed)
    }

    fn reconcile_profile_creation_time(
        &mut self,
        profile: ProfileId,
        created_at: Timestamp,
        source_snapshot: &[u8],
    ) -> Result<bool> {
        self.connection
            .execute(
                "UPDATE profiles SET created_at = ?2
             WHERE id = ?1 AND created_at != ?2
               AND EXISTS (SELECT 1 FROM profile_identity_crdts
                           WHERE profile_id = ?1 AND snapshot = ?3)",
                params![profile.to_storage(), created_at.millis(), source_snapshot],
            )
            .map(|changed| changed != 0)
            .map_err(sql_failed)
    }

    fn commit_profile_unit(&mut self, unit: ProfileUnitRef, snapshot: &[u8]) -> Result<()> {
        write_profile_unit(&self.connection, unit, snapshot)
    }

    fn commit_profile_units(&mut self, units: &[(ProfileUnitRef, Vec<u8>)]) -> Result<()> {
        let tx = self.connection.transaction().map_err(sql_failed)?;
        for (unit, snapshot) in units {
            write_profile_unit(&tx, *unit, snapshot)?;
        }
        tx.commit().map_err(sql_failed)
    }

    fn replace_profile_due_index(
        &mut self,
        owner: ProfileId,
        workspace: WorkspaceId,
        source_snapshot: &[u8],
        source_version: &[u8],
        rows: &[ReminderDue],
    ) -> Result<bool> {
        if rows
            .iter()
            .any(|row| row.owner != owner || row.workspace != workspace)
        {
            return Err(Error::new(
                Code::InvalidRequest,
                "reminder index scope mismatch",
            ));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_failed)?;
        let matched = tx
            .execute(
                "UPDATE profile_personal_crdts SET indexed_version = ?4
             WHERE owner_profile_id = ?1 AND workspace_id = ?2 AND snapshot = ?3",
                params![
                    owner.to_storage(),
                    workspace.to_storage(),
                    source_snapshot,
                    source_version
                ],
            )
            .map_err(sql_failed)?;
        if matched == 0 {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM profile_reminder_due WHERE owner_profile_id = ?1 AND workspace_id = ?2",
            params![owner.to_storage(), workspace.to_storage()],
        )
        .map_err(sql_failed)?;
        {
            let mut insert = tx
                .prepare(
                    "INSERT INTO profile_reminder_due
                 (owner_profile_id, workspace_id, reminder_id, due_at_ms) VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(sql_failed)?;
            for row in rows {
                insert
                    .execute(params![
                        owner.to_storage(),
                        workspace.to_storage(),
                        row.reminder.to_storage(),
                        row.due_at_ms
                    ])
                    .map_err(sql_failed)?;
            }
        }
        tx.commit().map_err(sql_failed)?;
        Ok(true)
    }

    fn profile_reminder_due(&self, owner: ProfileId) -> Result<Vec<ReminderDue>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT workspace_id, reminder_id, due_at_ms FROM profile_reminder_due
             WHERE owner_profile_id = ?1 ORDER BY due_at_ms, workspace_id, reminder_id",
            )
            .map_err(sql_failed)?;
        let rows = statement
            .query_map(params![owner.to_storage()], |row| {
                let workspace: String = row.get(0)?;
                let reminder: String = row.get(1)?;
                let due_at_ms = row.get(2)?;
                Ok((|| {
                    Ok(ReminderDue {
                        owner,
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

    // ------------------------------------------------------------ workspaces

    fn put_workspace(&mut self, workspace: &Workspace) -> Result<()> {
        // One transaction, because a move changes two rows: the Workspace's
        // owner and the owner projection its mode row carries. Contract 3
        // requires both in the same catalog transaction, so a move can never
        // leave a mode row filed under the Profile the Workspace just left.
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_failed)?;
        let changed = tx
            .execute(
                "INSERT INTO workspaces                  (id, owner, name, created_at, last_opened_at, icon, color, pinned, archived, deleted_at, position)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11
                 WHERE EXISTS (SELECT 1 FROM profiles WHERE id = ?2 AND removed_at IS NULL)
                 ON CONFLICT(id) DO UPDATE SET
                   name = excluded.name, owner = excluded.owner,
                   icon = excluded.icon, color = excluded.color,
                   pinned = excluded.pinned, archived = excluded.archived,
                   deleted_at = excluded.deleted_at, position = excluded.position",
                params![
                    workspace.id.to_storage(),
                    workspace.owner.to_storage(),
                    workspace.name,
                    workspace.created_at.millis(),
                    workspace.last_opened_at.map(|t| t.millis()),
                    workspace.icon,
                    workspace.color,
                    workspace.pinned,
                    workspace.archived,
                    workspace.deleted_at.map(|at| at.millis()),
                    workspace.position,
                ],
            )
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(f, _)
                    if f.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    Error::new(Code::ProfileNotFound, "no such owner profile")
                }
                other => sql_failed(other),
            })?;
        if changed == 0 {
            return Err(Error::new(
                Code::ProfileNotFound,
                "owner profile is absent or locally removed",
            ));
        }
        // **A new Workspace is seeded at its Profile's own choice**, and at
        // Local-only when nobody has made one. An existing Workspace keeps
        // its choice and only has its owner projection corrected.
        //
        // This is not the evidence path D258 withdrew, and the difference is
        // the whole of why it is here: the value copied is a *person's*
        // explicit Profile choice, read downward at the moment they create a
        // child under it. Seeding Local-only instead would mean a Workspace
        // made after answering the prompt never moved a byte — hosted
        // activation already inherits the Profile's switch (D238), so the two
        // halves would disagree and the symptom would be pictures that never
        // arrive in a Workspace whose documents do.
        //
        // It lands as a **default**, not a choice: the person can still set
        // this Workspace Local-only, and that choice then outranks everything
        // for the reason every other rule here exists.
        //
        // A move that puts the Workspace above its new Profile's ceiling is
        // **not** repaired here -- the effective read reports the mismatch and
        // the affected transport stops, because silently lowering a choice is
        // the failure contract 3 exists to prevent.
        tx.execute(
            "INSERT INTO local_transport_modes
               (subject_kind, subject_id, owner_profile_id, workspace_id,
                mode, explicit_choice, revision)
             SELECT 'workspace', ?1, ?2, ?1,
                    COALESCE(
                      (SELECT owner.mode FROM local_transport_modes owner
                        WHERE owner.subject_kind = 'profile'
                          AND owner.subject_id = ?2
                          AND owner.explicit_choice = 1),
                      'local_only'),
                    0, 1
              WHERE 1
             ON CONFLICT(subject_kind, subject_id)
               DO UPDATE SET owner_profile_id = excluded.owner_profile_id",
            params![workspace.id.to_storage(), workspace.owner.to_storage()],
        )
        .map_err(sql_failed)?;
        tx.commit().map_err(sql_failed)
    }

    fn workspace(&self, id: WorkspaceId) -> Result<Option<Workspace>> {
        self.connection
            .query_row(
                "SELECT id, owner, name, created_at, last_opened_at, icon, color, pinned, archived, deleted_at, position
                 FROM workspaces WHERE id = ?1",
                params![id.to_storage()],
                read_workspace,
            )
            .optional()
            .map_err(sql_failed)?
            .transpose()
    }

    fn workspaces(&self) -> Result<Vec<Workspace>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT id, owner, name, created_at, last_opened_at, icon, color, pinned, archived, deleted_at, position
                 FROM workspaces WHERE deleted_at IS NULL ORDER BY position, id",
            )
            .map_err(sql_failed)?;
        let rows = stmt.query_map([], read_workspace).map_err(sql_failed)?;
        collect(rows)
    }

    fn workspaces_including_deleted(&self) -> Result<Vec<Workspace>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT id, owner, name, created_at, last_opened_at, icon, color, pinned, archived, deleted_at, position
                 FROM workspaces ORDER BY position, id",
            )
            .map_err(sql_failed)?;
        let rows = stmt.query_map([], read_workspace).map_err(sql_failed)?;
        collect(rows)
    }

    fn reorder_workspaces(&mut self, owner: ProfileId, ids: &[WorkspaceId]) -> Result<()> {
        let tx = self.connection.transaction().map_err(sql_failed)?;
        for (position, id) in ids.iter().enumerate() {
            let changed = tx
                .execute(
                    "UPDATE workspaces SET position = ?3 WHERE id = ?1 AND owner = ?2",
                    params![id.to_storage(), owner.to_storage(), position as i64],
                )
                .map_err(sql_failed)?;
            if changed != 1 {
                return Err(Error::bare(Code::InvalidRequest));
            }
        }
        tx.commit().map_err(sql_failed)
    }

    fn touch_workspace(&mut self, id: WorkspaceId, at: Timestamp) -> Result<()> {
        let n = self
            .connection
            .execute(
                "UPDATE workspaces SET last_opened_at = ?2 WHERE id = ?1",
                params![id.to_storage(), at.millis()],
            )
            .map_err(sql_failed)?;
        if n == 0 {
            return Err(Error::bare(Code::WorkspaceNotFound));
        }
        Ok(())
    }

    fn forget_workspace(&mut self, id: WorkspaceId) -> Result<()> {
        if self.workspace_has_personal_units(id)? {
            return Err(Error::new(
                Code::WorkspaceInUse,
                "retained personal units prevent workspace forget",
            ));
        }
        // The row goes; the workspace's own database does not. Keeping the two
        // separate is what makes an accidental unregister recoverable.
        //
        // The mode row is removed first and explicitly: its foreign key is
        // ON DELETE RESTRICT, so physical removal has to say out loud that it
        // is dropping the choice rather than let it cascade away unseen.
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_failed)?;
        delete_workspace_transport_mode(&tx, id)?;
        tx.execute(
            "DELETE FROM workspaces WHERE id = ?1",
            params![id.to_storage()],
        )
        .map_err(sql_failed)?;
        tx.commit().map_err(sql_failed)
    }

    fn purge_workspace(&mut self, id: WorkspaceId) -> Result<()> {
        if self.workspace_has_personal_units(id)? {
            return Err(Error::new(
                Code::WorkspaceInUse,
                "retained personal units prevent workspace purge",
            ));
        }
        let tx = self.connection.transaction().map_err(sql_failed)?;
        delete_workspace_transport_mode(&tx, id)?;
        tx.execute(
            "DELETE FROM document_locations WHERE workspace_id = ?1",
            params![id.to_storage()],
        )
        .map_err(sql_failed)?;
        tx.execute(
            "DELETE FROM workspace_lineage WHERE workspace = ?1 OR derived_from = ?1",
            params![id.to_storage()],
        )
        .map_err(sql_failed)?;
        tx.execute(
            "DELETE FROM workspaces WHERE id = ?1",
            params![id.to_storage()],
        )
        .map_err(sql_failed)?;
        tx.commit().map_err(sql_failed)
    }

    fn set_workspace_deleted(
        &mut self,
        id: WorkspaceId,
        deleted_at: Option<Timestamp>,
        documents: &[DocumentId],
    ) -> Result<()> {
        let tx = self.connection.transaction().map_err(sql_failed)?;
        let changed = tx
            .execute(
                "UPDATE workspaces SET deleted_at = ?2
                 WHERE id = ?1
                   AND (?2 IS NOT NULL OR EXISTS (
                     SELECT 1 FROM profiles
                     WHERE profiles.id = workspaces.owner
                       AND profiles.removed_at IS NULL
                   ))",
                params![id.to_storage(), deleted_at.map(|at| at.millis())],
            )
            .map_err(sql_failed)?;
        if changed == 0 {
            let exists = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM workspaces WHERE id = ?1)",
                    params![id.to_storage()],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(sql_failed)?;
            return Err(Error::bare(if exists {
                Code::ProfileNotFound
            } else {
                Code::WorkspaceNotFound
            }));
        }
        tx.execute(
            "DELETE FROM document_locations WHERE workspace_id = ?1",
            params![id.to_storage()],
        )
        .map_err(sql_failed)?;
        if deleted_at.is_none() {
            for document in documents {
                tx.execute(
                    "INSERT OR REPLACE INTO document_locations (document_id, workspace_id) VALUES (?1, ?2)",
                    params![document.to_storage(), id.to_storage()],
                )
                .map_err(sql_failed)?;
            }
        }
        tx.commit().map_err(sql_failed)?;
        Ok(())
    }

    // --------------------------------------------------------------- lineage

    fn record_lineage(&mut self, lineage: &Lineage) -> Result<()> {
        self.connection
            .execute(
                "INSERT OR REPLACE INTO workspace_lineage (workspace, derived_from, kind, at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    lineage.workspace.to_storage(),
                    lineage.derived_from.to_storage(),
                    lineage.kind.as_str(),
                    lineage.at.millis(),
                ],
            )
            .map_err(sql_failed)?;
        Ok(())
    }

    fn lineage(&self, workspace: WorkspaceId) -> Result<Vec<Lineage>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT workspace, derived_from, kind, at
                 FROM workspace_lineage WHERE workspace = ?1 ORDER BY at",
            )
            .map_err(sql_failed)?;
        let rows = stmt
            .query_map(params![workspace.to_storage()], read_lineage)
            .map_err(sql_failed)?;
        collect(rows)
    }

    // --------------------------------------------------------------- clients

    fn put_client(&mut self, client: &ApiClient, token_hash: &[u8]) -> Result<()> {
        self.connection
            .execute(
                "INSERT INTO api_clients (id, name, token_hash, scopes, created_at, revoked_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET
                   name = excluded.name, scopes = excluded.scopes,
                   revoked_at = excluded.revoked_at",
                params![
                    client.id.to_storage(),
                    client.name,
                    token_hash,
                    write_scopes(&client.scopes),
                    client.created_at.millis(),
                    client.revoked_at.map(|t| t.millis()),
                ],
            )
            .map_err(sql_failed)?;
        Ok(())
    }

    fn client_by_token(&self, token_hash: &[u8]) -> Result<Option<ApiClient>> {
        self.connection
            .query_row(
                "SELECT id, name, scopes, created_at, revoked_at
                 FROM api_clients WHERE token_hash = ?1",
                params![token_hash],
                read_client,
            )
            .optional()
            .map_err(sql_failed)?
            .transpose()
    }

    fn revoke_client(&mut self, id: ClientId, at: Timestamp) -> Result<()> {
        let n = self
            .connection
            .execute(
                "UPDATE api_clients SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
                params![id.to_storage(), at.millis()],
            )
            .map_err(sql_failed)?;
        if n == 0 {
            // Already revoked, or never existed. Both are "not a usable
            // credential", and saying which would help someone enumerate ids.
            return Err(Error::bare(Code::Forbidden));
        }
        Ok(())
    }

    fn clients(&self) -> Result<Vec<ApiClient>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT id, name, scopes, created_at, revoked_at
                 FROM api_clients ORDER BY created_at, id",
            )
            .map_err(sql_failed)?;
        let rows = stmt.query_map([], read_client).map_err(sql_failed)?;
        collect(rows)
    }

    // ----------------------------------------------------------------- state

    fn state(&self, key: &str) -> Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT value FROM store_state WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_failed)
    }

    fn set_state(&mut self, key: &str, value: &str) -> Result<()> {
        self.connection
            .execute(
                "INSERT INTO store_state (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(sql_failed)?;
        Ok(())
    }

    fn sync_document_digests(&self, workspace: WorkspaceId) -> Result<Vec<SyncDocumentDigest>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT document_id, source_revision, digest FROM sync_document_digests
                 WHERE workspace_id = ?1 ORDER BY document_id",
            )
            .map_err(sql_failed)?;
        let rows = statement
            .query_map(params![workspace.to_storage()], |row| {
                let document: String = row.get(0)?;
                Ok((
                    document,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            })
            .map_err(sql_failed)?;
        rows.map(|row| {
            let (document, source_revision, digest) = row.map_err(sql_failed)?;
            Ok(SyncDocumentDigest {
                workspace,
                document: DocumentId::parse(&document)
                    .ok_or_else(|| Error::new(Code::CorruptState, "invalid sync document id"))?,
                source_revision,
                digest,
            })
        })
        .collect()
    }

    fn put_sync_document_digest(&mut self, digest: &SyncDocumentDigest) -> Result<()> {
        self.connection
            .execute(
                "INSERT INTO sync_document_digests
                    (workspace_id, document_id, source_revision, digest)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(workspace_id, document_id) DO UPDATE SET
                    source_revision = excluded.source_revision,
                    digest = excluded.digest",
                params![
                    digest.workspace.to_storage(),
                    digest.document.to_storage(),
                    digest.source_revision,
                    digest.digest,
                ],
            )
            .map_err(sql_failed)?;
        Ok(())
    }

    fn replace_sync_document_digests(
        &mut self,
        workspace: WorkspaceId,
        digests: &[SyncDocumentDigest],
    ) -> Result<()> {
        let transaction = self.connection.transaction().map_err(sql_failed)?;
        transaction
            .execute(
                "DELETE FROM sync_document_digests WHERE workspace_id = ?1",
                params![workspace.to_storage()],
            )
            .map_err(sql_failed)?;
        for digest in digests {
            transaction
                .execute(
                    "INSERT INTO sync_document_digests
                        (workspace_id, document_id, source_revision, digest)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        digest.workspace.to_storage(),
                        digest.document.to_storage(),
                        digest.source_revision,
                        digest.digest,
                    ],
                )
                .map_err(sql_failed)?;
        }
        transaction.commit().map_err(sql_failed)
    }

    // ------------------------------------------------------- sync event log

    fn record_sync_state_event(
        &mut self,
        event: &SyncStateEvent,
        retain_after: Timestamp,
    ) -> Result<SyncStateEvent> {
        let items = event
            .items
            .map(i64::try_from)
            .transpose()
            .map_err(|_| Error::new(Code::InvalidRequest, "event item count is too large"))?;
        let transaction = self.connection.transaction().map_err(sql_failed)?;
        transaction
            .execute(
                "INSERT INTO sync_state_events
                    (received_at, peer_device_id, kind, profile_id, workspace_id,
                     document_id, document_name, blob_hash, detail, items)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    event.received_at.millis(),
                    event.peer_device_id.as_str(),
                    event.kind.as_str(),
                    event.profile.map(|id| id.to_storage()),
                    event.workspace.map(|id| id.to_storage()),
                    event.document.map(|id| id.to_storage()),
                    event.document_name.as_deref(),
                    event.blob_hash.as_deref(),
                    event.detail.as_deref(),
                    items,
                ],
            )
            .map_err(sql_failed)?;
        let id = transaction.last_insert_rowid();
        transaction
            .execute(
                "DELETE FROM sync_state_events WHERE received_at < ?1",
                params![retain_after.millis()],
            )
            .map_err(sql_failed)?;
        transaction.commit().map_err(sql_failed)?;
        let mut stored = event.clone();
        stored.id = Some(id);
        Ok(stored)
    }

    fn sync_state_events(
        &self,
        peer_device_id: &str,
        retain_after: Timestamp,
        before_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<SyncStateEvent>> {
        let limit = i64::try_from(limit)
            .map_err(|_| Error::new(Code::InvalidRequest, "event page is too large"))?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, received_at, peer_device_id, kind, profile_id,
                        workspace_id, document_id, document_name, blob_hash,
                        detail, items
                 FROM sync_state_events
                 WHERE peer_device_id = ?1 AND received_at >= ?2
                   AND (?3 IS NULL OR id < ?3)
                 ORDER BY id DESC LIMIT ?4",
            )
            .map_err(sql_failed)?;
        let rows = statement
            .query_map(
                params![peer_device_id, retain_after.millis(), before_id, limit],
                read_sync_state_event,
            )
            .map_err(sql_failed)?;
        collect(rows)
    }

    fn latest_sync_state_event_times(
        &self,
        retain_after: Timestamp,
    ) -> Result<Vec<(String, Timestamp)>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT peer_device_id, MAX(received_at)
                 FROM sync_state_events WHERE received_at >= ?1
                 GROUP BY peer_device_id ORDER BY peer_device_id",
            )
            .map_err(sql_failed)?;
        let rows = statement
            .query_map(params![retain_after.millis()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(sql_failed)?;
        rows.map(|row| {
            let (peer, at) = row.map_err(sql_failed)?;
            Ok((peer, Timestamp::from_millis(at)))
        })
        .collect()
    }

    fn prune_sync_state_events(&mut self, retain_after: Timestamp) -> Result<usize> {
        self.connection
            .execute(
                "DELETE FROM sync_state_events WHERE received_at < ?1",
                params![retain_after.millis()],
            )
            .map_err(sql_failed)
    }

    // ------------------------------------------------------------- locations

    fn locate(&self, document: DocumentId) -> Result<Option<WorkspaceId>> {
        let found: Option<String> = self
            .connection
            .query_row(
                "SELECT workspace_id FROM document_locations WHERE document_id = ?1",
                params![document.to_storage()],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_failed)?;
        // A row naming something that is not a uuid is a stale row, not a
        // failure: the index is derived, so the answer is simply "unknown".
        Ok(found.as_deref().and_then(WorkspaceId::parse))
    }

    fn record_location(&mut self, document: DocumentId, workspace: WorkspaceId) -> Result<()> {
        self.connection
            .execute(
                "INSERT INTO document_locations (document_id, workspace_id) VALUES (?1, ?2)
                 ON CONFLICT(document_id) DO UPDATE SET workspace_id = excluded.workspace_id",
                params![document.to_storage(), workspace.to_storage()],
            )
            .map_err(sql_failed)?;
        Ok(())
    }

    fn forget_location(&mut self, document: DocumentId) -> Result<()> {
        self.connection
            .execute(
                "DELETE FROM document_locations WHERE document_id = ?1",
                params![document.to_storage()],
            )
            .map_err(sql_failed)?;
        Ok(())
    }

    // ------------------------------------------------------ transport modes

    fn transport_mode(&self, subject: TransportSubject) -> Result<TransportModeState> {
        read_transport_mode(&self.connection, subject)
    }

    fn transport_mode_group(&self, owner: ProfileId) -> Result<TransportModeGroup> {
        read_transport_mode_group(&self.connection, owner)
    }

    fn commit_transport_modes(
        &mut self,
        owner: ProfileId,
        requests: &[TransportModeRequest],
    ) -> Result<Vec<TransportModeCommit>> {
        // IMMEDIATE, so the read the decision is made from and the write it
        // produces are one write-serialized step. A compare-and-set whose read
        // runs in a deferred transaction can have its revision overtaken
        // between the read and the write and would still believe it matched.
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_failed)?;
        let group = read_transport_mode_group(&tx, owner)?;
        // Every subject named must belong to this group. Resolving it here
        // rather than trusting the request is what stops one Profile's
        // operation from writing a row filed under another.
        for request in requests {
            let subject = request.subject();
            let known = subject == group.profile.subject
                || group.children.iter().any(|child| child.subject == subject);
            if !known {
                return Err(Error::new(
                    Code::InvalidRequest,
                    "a transport mode request names a subject outside this Profile",
                ));
            }
        }
        let plan = transport::plan(&group, requests)?;
        for write in &plan.writes {
            let changed = tx
                .execute(
                    "UPDATE local_transport_modes
                        SET mode = ?3, explicit_choice = ?4, revision = ?5
                      WHERE subject_kind = ?1 AND subject_id = ?2",
                    params![
                        write.subject.kind(),
                        write.subject.id(),
                        write.mode.as_storage(),
                        i64::from(write.explicit_choice),
                        write.revision,
                    ],
                )
                .map_err(sql_failed)?;
            if changed != 1 {
                return Err(Error::new(
                    Code::CorruptState,
                    "a transport mode row vanished inside its own transaction",
                ));
            }
        }
        tx.commit().map_err(sql_failed)?;
        Ok(plan.outcomes)
    }
}

// ---------------------------------------------------------- transport modes

/// Drop one Workspace's mode row. Called by the two physical removals before
/// they touch `workspaces`, because the foreign key is `ON DELETE RESTRICT`.
fn delete_workspace_transport_mode(connection: &Connection, id: WorkspaceId) -> Result<()> {
    connection
        .execute(
            "DELETE FROM local_transport_modes
              WHERE subject_kind = 'workspace' AND subject_id = ?1",
            params![id.to_storage()],
        )
        .map_err(sql_failed)?;
    Ok(())
}

fn read_transport_mode(
    connection: &Connection,
    subject: TransportSubject,
) -> Result<TransportModeState> {
    let row: Option<(String, String, i64, i64)> = connection
        .query_row(
            "SELECT owner_profile_id, mode, explicit_choice, revision
               FROM local_transport_modes
              WHERE subject_kind = ?1 AND subject_id = ?2",
            params![subject.kind(), subject.id()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(sql_failed)?;
    let Some((owner, mode, explicit, revision)) = row else {
        // Deliberately not a default. See `Catalog::transport_mode`.
        return Err(Error::new(
            Code::CorruptState,
            format!(
                "no device-local transport mode row for this {}",
                subject.kind()
            ),
        ));
    };
    decode_transport_mode(subject, &owner, &mode, explicit, revision)
}

fn decode_transport_mode(
    subject: TransportSubject,
    owner: &str,
    mode: &str,
    explicit: i64,
    revision: i64,
) -> Result<TransportModeState> {
    let owner = ProfileId::parse(owner).ok_or_else(|| {
        Error::new(
            Code::CorruptState,
            "a transport mode row's owner Profile id is not a uuid",
        )
    })?;
    // The CHECK constraints make these unreachable through this build's own
    // writes. They are decoded strictly anyway, because a row that got here
    // another way must stop the transport rather than be guessed at.
    let explicit_choice = match explicit {
        0 => false,
        1 => true,
        _ => {
            return Err(Error::new(
                Code::CorruptState,
                "a transport mode row's explicit_choice is not 0 or 1",
            ));
        }
    };
    if revision <= 0 {
        return Err(Error::new(
            Code::CorruptState,
            "a transport mode row's revision is not positive",
        ));
    }
    Ok(TransportModeState {
        subject,
        owner,
        mode: TransportMode::parse(mode)?,
        explicit_choice,
        revision,
    })
}

/// One Profile's row and every Workspace row beneath it.
///
/// The child query is driven from `workspaces` rather than from the mode table,
/// so a Workspace whose row is missing, or whose `owner_profile_id` has drifted
/// from `workspaces.owner`, is found rather than quietly skipped. That is the
/// ownership validation contract 2 asks for: the two foreign keys say each id
/// exists, and nothing says they agree.
fn read_transport_mode_group(
    connection: &Connection,
    owner: ProfileId,
) -> Result<TransportModeGroup> {
    let profile = read_transport_mode(connection, TransportSubject::Profile(owner))?;
    let mut statement = connection
        .prepare(
            "SELECT w.id, m.owner_profile_id, m.mode, m.explicit_choice, m.revision
               FROM workspaces w
               LEFT JOIN local_transport_modes m
                 ON m.subject_kind = 'workspace' AND m.subject_id = w.id
              WHERE w.owner = ?1
              ORDER BY w.id",
        )
        .map_err(sql_failed)?;
    let rows = statement
        .query_map(params![owner.to_storage()], |row| {
            let id: String = row.get(0)?;
            let mode_owner: Option<String> = row.get(1)?;
            let mode: Option<String> = row.get(2)?;
            let explicit: Option<i64> = row.get(3)?;
            let revision: Option<i64> = row.get(4)?;
            Ok((|| {
                let workspace = WorkspaceId::parse(&id)
                    .ok_or_else(|| Error::new(Code::CorruptState, "workspace id is not a uuid"))?;
                let subject = TransportSubject::Workspace(workspace);
                let (Some(mode_owner), Some(mode), Some(explicit), Some(revision)) =
                    (mode_owner, mode, explicit, revision)
                else {
                    return Err(Error::new(
                        Code::CorruptState,
                        "a retained Workspace has no device-local transport mode row",
                    ));
                };
                let state = decode_transport_mode(subject, &mode_owner, &mode, explicit, revision)?;
                if state.owner != owner {
                    return Err(Error::new(
                        Code::CorruptState,
                        "a transport mode row is filed under a different Profile than \
                         the Workspace it names",
                    ));
                }
                Ok(state)
            })())
        })
        .map_err(sql_failed)?;
    Ok(TransportModeGroup {
        profile,
        children: collect(rows)?,
    })
}

// ------------------------------------------------------------------ row readers
//
// Each returns `Result<T>` *inside* the rusqlite row result, so a malformed
// value is a store error rather than a panic in a callback. `collect` flattens
// the two layers.

type RowResult<T> = rusqlite::Result<Result<T>>;

fn write_profile_unit(
    connection: &Connection,
    unit: ProfileUnitRef,
    snapshot: &[u8],
) -> Result<()> {
    match unit {
        ProfileUnitRef::Identity { .. } => {
            return Err(Error::new(
                Code::InvalidRequest,
                "identity commits require their listing projection",
            ));
        }
        ProfileUnitRef::LegacyPrivate { profile } => connection.execute(
            "INSERT INTO profile_crdts (profile_id, snapshot) VALUES (?1, ?2)
             ON CONFLICT(profile_id) DO UPDATE SET snapshot = excluded.snapshot",
            params![profile.to_storage(), snapshot],
        ),
        ProfileUnitRef::Private { profile } => connection.execute(
            "INSERT INTO profile_private_crdts (profile_id, snapshot) VALUES (?1, ?2)
             ON CONFLICT(profile_id) DO UPDATE SET snapshot = excluded.snapshot",
            params![profile.to_storage(), snapshot],
        ),
        ProfileUnitRef::Personal { owner, workspace } => connection.execute(
            "INSERT INTO profile_personal_crdts (owner_profile_id, workspace_id, snapshot)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(owner_profile_id, workspace_id) DO UPDATE SET
               snapshot = excluded.snapshot, indexed_version = NULL
             WHERE profile_personal_crdts.snapshot != excluded.snapshot",
            params![owner.to_storage(), workspace.to_storage(), snapshot],
        ),
    }
    .map_err(sql_failed)?;
    Ok(())
}

/// Only identity-bearing commits may replace a placeholder creation time.
/// Plain catalog writes and legacy compatibility bytes must not rewrite this
/// immutable identity fact. It is immutable in the CRDT, not in its projection.
fn write_identity_projection(connection: &Connection, profile: &Profile) -> Result<()> {
    write_profile_projection(connection, profile)?;
    connection
        .execute(
            "UPDATE profiles SET created_at = ?2 WHERE id = ?1 AND created_at != ?2",
            params![profile.id.to_storage(), profile.created_at.millis()],
        )
        .map_err(sql_failed)?;
    Ok(())
}

/// Bootstrap/legacy refreshes preserve creation time, kind and local removal.
fn write_profile_projection(connection: &Connection, profile: &Profile) -> Result<()> {
    connection.execute(
        "INSERT INTO profiles (id, name, purpose, avatar, account, locale, created_at, icon, color, kind)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET
           name = excluded.name, purpose = excluded.purpose,
           avatar = excluded.avatar, account = excluded.account,
           locale = excluded.locale, icon = excluded.icon, color = excluded.color",
        params![profile.id.to_storage(), profile.name, profile.purpose, profile.avatar,
            profile.account, profile.locale.as_str(), profile.created_at.millis(),
            profile.icon, profile.color, profile.kind.as_str()],
    ).map_err(sql_failed)?;
    seed_profile_transport_mode(connection, profile.id)
}

/// Give a Profile its Local-only default row if it has not got one.
///
/// `DO NOTHING` rather than an upsert, and that is the whole point: this runs on
/// every projection write, and a re-registration must never reset a mode
/// somebody chose. The seeded value is the one migration 12 wrote for the
/// Profiles that already existed.
fn seed_profile_transport_mode(connection: &Connection, profile: ProfileId) -> Result<()> {
    connection
        .execute(
            "INSERT INTO local_transport_modes
               (subject_kind, subject_id, owner_profile_id, workspace_id,
                mode, explicit_choice, revision)
             VALUES ('profile', ?1, ?1, NULL, 'local_only', 0, 1)
             ON CONFLICT(subject_kind, subject_id) DO NOTHING",
            params![profile.to_storage()],
        )
        .map_err(sql_failed)?;
    Ok(())
}

fn read_profile(row: &rusqlite::Row<'_>) -> RowResult<Profile> {
    let id: String = row.get(0)?;
    let locale: String = row.get(5)?;
    Ok((|| {
        Ok(Profile {
            id: ProfileId::parse(&id)
                .ok_or_else(|| Error::new(Code::CorruptState, "profile id is not a uuid"))?,
            name: row_string(row, 1)?,
            purpose: row_opt_string(row, 2)?,
            avatar: row_opt_string(row, 3)?,
            icon: row_opt_string(row, 7)?,
            color: row_opt_string(row, 8)?,
            account: row_opt_string(row, 4)?,
            // A tag a newer build understands and this one does not must not
            // make the profile unreadable.
            locale: Locale::parse(&locale).unwrap_or_default(),
            created_at: Timestamp::from_millis(row_i64(row, 6)?),
            // Absent is a persona, and so is a token this build does not know:
            // see `ProfileKind::parse`. A row is never unreadable because of it.
            kind: row_opt_string(row, 9)?
                .map(|value| ProfileKind::parse(&value))
                .unwrap_or_default(),
        })
    })())
}

fn read_workspace(row: &rusqlite::Row<'_>) -> RowResult<Workspace> {
    let id: String = row.get(0)?;
    let owner: String = row.get(1)?;
    let icon: Option<String> = row.get(5)?;
    let color: Option<String> = row.get(6)?;
    let pinned: bool = row.get(7)?;
    let archived: bool = row.get(8)?;
    let deleted_at: Option<i64> = row.get(9)?;
    let position: i64 = row.get(10)?;
    Ok((|| {
        Ok(Workspace {
            id: WorkspaceId::parse(&id)
                .ok_or_else(|| Error::new(Code::CorruptState, "workspace id is not a uuid"))?,
            owner: ProfileId::parse(&owner)
                .ok_or_else(|| Error::new(Code::CorruptState, "owner id is not a uuid"))?,
            name: row_string(row, 2)?,
            created_at: Timestamp::from_millis(row_i64(row, 3)?),
            last_opened_at: row_opt_i64(row, 4)?.map(Timestamp::from_millis),
            // **Read strictly.** A `unwrap_or` here would turn a query that
            // forgot a column into a workspace that silently lost its colour —
            // which is the shape of the bug this whole change exists to fix.
            icon: icon.clone(),
            color: color.clone(),
            pinned,
            archived,
            deleted_at: deleted_at.map(Timestamp::from_millis),
            position,
        })
    })())
}

fn read_lineage(row: &rusqlite::Row<'_>) -> RowResult<Lineage> {
    let workspace: String = row.get(0)?;
    let from: String = row.get(1)?;
    let kind: String = row.get(2)?;
    Ok((|| {
        Ok(Lineage {
            workspace: WorkspaceId::parse(&workspace)
                .ok_or_else(|| Error::new(Code::CorruptState, "lineage workspace is not a uuid"))?,
            derived_from: WorkspaceId::parse(&from)
                .ok_or_else(|| Error::new(Code::CorruptState, "lineage origin is not a uuid"))?,
            kind: LineageKind::parse(&kind)
                .ok_or_else(|| Error::new(Code::CorruptState, "unknown lineage kind"))?,
            at: Timestamp::from_millis(row_i64(row, 3)?),
        })
    })())
}

fn read_client(row: &rusqlite::Row<'_>) -> RowResult<ApiClient> {
    let id: String = row.get(0)?;
    let scopes: String = row.get(2)?;
    Ok((|| {
        Ok(ApiClient {
            id: ClientId::parse(&id)
                .ok_or_else(|| Error::new(Code::CorruptState, "client id is not a uuid"))?,
            name: row_string(row, 1)?,
            scopes: read_scopes(&scopes),
            created_at: Timestamp::from_millis(row_i64(row, 3)?),
            revoked_at: row_opt_i64(row, 4)?.map(Timestamp::from_millis),
        })
    })())
}

fn read_sync_state_event(row: &rusqlite::Row<'_>) -> RowResult<SyncStateEvent> {
    let profile: Option<String> = row.get(4)?;
    let workspace: Option<String> = row.get(5)?;
    let document: Option<String> = row.get(6)?;
    let items: Option<i64> = row.get(10)?;
    Ok((|| {
        Ok(SyncStateEvent {
            id: Some(row_i64(row, 0)?),
            received_at: Timestamp::from_millis(row_i64(row, 1)?),
            peer_device_id: row_string(row, 2)?,
            kind: row_string(row, 3)?,
            profile: profile
                .as_deref()
                .map(ProfileId::parse)
                .transpose_option("invalid sync event profile id")?,
            workspace: workspace
                .as_deref()
                .map(WorkspaceId::parse)
                .transpose_option("invalid sync event workspace id")?,
            document: document
                .as_deref()
                .map(DocumentId::parse)
                .transpose_option("invalid sync event document id")?,
            document_name: row_opt_string(row, 7)?,
            blob_hash: row_opt_string(row, 8)?,
            detail: row_opt_string(row, 9)?,
            items: items
                .map(usize::try_from)
                .transpose()
                .map_err(|_| Error::new(Code::CorruptState, "negative sync event item count"))?,
        })
    })())
}

trait ParsedOption<T> {
    fn transpose_option(self, message: &'static str) -> Result<Option<T>>;
}

impl<T> ParsedOption<T> for Option<Option<T>> {
    fn transpose_option(self, message: &'static str) -> Result<Option<T>> {
        self.map(|value| value.ok_or_else(|| Error::new(Code::CorruptState, message)))
            .transpose()
    }
}

/// Space-separated tokens. Not JSON: the set is small, closed and never nested,
/// and a scope column a person can read in a database browser is worth more
/// here than a structure nobody will nest.
fn write_scopes(scopes: &[Scope]) -> String {
    scopes
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// **Unknown tokens are dropped, not defaulted.** A scope this build does not
/// recognise grants nothing — the safe direction, and the only one that keeps a
/// newer build's credential from being silently over-privileged here.
fn read_scopes(s: &str) -> Vec<Scope> {
    s.split_whitespace().filter_map(Scope::parse).collect()
}

fn row_string(row: &rusqlite::Row<'_>, i: usize) -> Result<String> {
    row.get(i).map_err(sql_failed)
}

fn row_opt_string(row: &rusqlite::Row<'_>, i: usize) -> Result<Option<String>> {
    row.get(i).map_err(sql_failed)
}

fn row_i64(row: &rusqlite::Row<'_>, i: usize) -> Result<i64> {
    row.get(i).map_err(sql_failed)
}

fn row_opt_i64(row: &rusqlite::Row<'_>, i: usize) -> Result<Option<i64>> {
    row.get(i).map_err(sql_failed)
}

pub(crate) fn collect<T>(
    rows: impl Iterator<Item = rusqlite::Result<Result<T>>>,
) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(sql_failed)??);
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
