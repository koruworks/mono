// SPDX-License-Identifier: MPL-2.0

//! `workspace.db` — one workspace's Loro state.
//!
//! ## The sequence space
//!
//! Updates and checkpoints **share one sequence number per document**. A
//! checkpoint's `seq` is the highest update it incorporates, which is what
//! makes the rules below expressible as one comparison rather than as a pair of
//! counters that can disagree. Opening is *load the newest sound checkpoint,
//! replay everything above it*, and "is this update already in the snapshot" is
//! `seq <= latest_checkpoint_seq`.
//!
//! ## Pruning keeps one generation of overlap
//!
//! A checkpoint prunes only what the **previous** checkpoint already covered,
//! never everything it covers itself:
//!
//! ```text
//! seq   1    2    3    4    5    6    7
//!       u    u    u    u    u    u    u
//!            ^cp(2)                 ^cp(6)
//!       └─┬─┘└──────────┬──────────────┘
//!      pruned        retained
//! ```
//!
//! The obvious rule — prune everything up to the new checkpoint — is wrong, and
//! wrong in a way that only shows up on a bad day. It deletes exactly the
//! journal that makes an *older* checkpoint useful, so the moment the newest
//! snapshot fails its checksum, falling back to `cp(2)` loses updates 3 to 7.
//! Keeping more than one checkpoint would be pointless: the fall-back would
//! land on a valid snapshot of a document from long ago and call it recovery.
//!
//! With the overlap, `cp(6)` failing means *load `cp(2)`, replay 3 onwards* and
//! **lose nothing**. Two consecutive damaged checkpoints is a deeper failure
//! and is lossy — but it is reported rather than silent, and it takes two
//! independent corruptions rather than one.
//!
//! The cost is one checkpoint interval of journal kept longer than strictly
//! necessary. That is the price of the newest snapshot being allowed to fail.
//!
//! ## The two transactions
//!
//! Both are the reason this file exists, and both are easy to break by
//! splitting a function in half later:
//!
//! - [`commit_updates`] writes the journal rows **and** the document's new
//!   position in one commit. A journal ahead of its position replays work
//!   already applied; a position ahead of its journal claims durability for
//!   bytes that are gone.
//! - [`commit_checkpoint`] writes the snapshot **and** deletes what it covers
//!   in one commit. Between those two statements is the only moment at which
//!   both copies of a change could be absent from the disk at once.
//!
//! [`commit_updates`]: SqliteWorkspace::commit_updates
//! [`commit_checkpoint`]: SqliteWorkspace::commit_checkpoint

use koru_store_core::errors::{Code, Error, Result};
use koru_store_core::ids::{ClientId, DocumentId, DocumentVersionId, HistoryEntryId, ProfileId};
use koru_store_core::model::{Blob, BlobHash, DocumentKind, Timestamp};
use koru_store_core::ports::{
    Checkpoint, CheckpointHeader, DocumentState, DocumentVersion, HistoryCapture, HistoryEntry,
    HistoryReason, JournalSince, SeedDocument, SeededDocument, StoredUpdate, UpdateBatch,
    UpdateOrigin, WorkspaceRepo,
};
use koru_store_core::version::WORKSPACE_SCHEMA_VERSION;
use rusqlite::{Connection, OptionalExtension, params};

use crate::catalog::collect;
use crate::db::{self, Migration, sql_failed};

const MIGRATIONS: &[Migration] = &[
    Migration {
        to: 1,
        name: "workspace: loro_documents and documents",
        columns: &[],
        sql: "
    CREATE TABLE loro_documents (
      id                    TEXT PRIMARY KEY,
      kind                  TEXT    NOT NULL,
      latest_checkpoint_seq INTEGER,
      version_vector        BLOB    NOT NULL,
      frontiers             BLOB    NOT NULL,
      updated_at            INTEGER NOT NULL
    );

    CREATE TABLE loro_checkpoints (
      document_id    TEXT    NOT NULL REFERENCES loro_documents(id) ON DELETE CASCADE,
      seq            INTEGER NOT NULL,
      snapshot       BLOB    NOT NULL,
      version_vector BLOB    NOT NULL,
      frontiers      BLOB    NOT NULL,
      checksum       TEXT    NOT NULL,
      created_at     INTEGER NOT NULL,
      PRIMARY KEY (document_id, seq)
    );

    CREATE TABLE loro_updates (
      document_id  TEXT    NOT NULL REFERENCES loro_documents(id) ON DELETE CASCADE,
      seq          INTEGER NOT NULL,
      update_bytes BLOB    NOT NULL,
      received_at  INTEGER NOT NULL,
      from_client  TEXT,
      PRIMARY KEY (document_id, seq)
    );

    -- The derived mirror of the manifest. Rebuildable, always: the manifest is
    -- the truth and this is a projection kept so that listing a workspace does
    -- not mean opening everything in it.
    CREATE TABLE documents (
      doc_id    TEXT PRIMARY KEY,
      parent_id TEXT,
      ordinal   INTEGER NOT NULL,
      title     TEXT    NOT NULL,
      kind      TEXT    NOT NULL,
      position  INTEGER NOT NULL
    );
    CREATE INDEX documents_walk ON documents(position);

    CREATE TABLE blobs (
      hash       TEXT PRIMARY KEY,
      size       INTEGER NOT NULL,
      mime       TEXT    NOT NULL,
      filename   TEXT,
      created_at INTEGER NOT NULL
    );
    ",
    },
    Migration {
        to: 2,
        name: "documents: trashed_at",
        // **Columns, and no backfill.** `documents` is a projection of the
        // manifest — `reindex` replaces it wholesale — so there is nothing to
        // migrate *into*: the values arrive with the next reindex, and until then
        // every row reads as not trashed, which is what every existing row is.
        //
        // Deleting and rebuilding the table would say the same thing more loudly and
        // would throw away a listing that is perfectly good in the meantime.
        sql: "",
        columns: &[("documents", "trashed_at", "INTEGER")],
    },
    Migration {
        to: 3,
        name: "documents: trashed_by",
        // **`trashed_by` again, because it was added to migration 2 after that
        // migration had already run.** A database stamped `2` never re-runs it,
        // so the column existed on every workspace created afterwards and on
        // none created before — and the failure surfaced as *Couldn't create
        // that document: table documents has no column named trashed_by*, at a
        // moment that has nothing to do with the trash.
        //
        // Declared through `columns` rather than as SQL so this is a no-op on
        // the databases that did get it from the edited migration 2. Editing a
        // migration that has shipped is the mistake; making the fix idempotent
        // is what stops the fix needing a fix.
        sql: "",
        columns: &[("documents", "trashed_by", "TEXT")],
    },
    Migration {
        to: 4,
        name: "document_versions",
        columns: &[],
        sql: "
    CREATE TABLE document_versions (
      id             TEXT PRIMARY KEY,
      document_id    TEXT    NOT NULL REFERENCES loro_documents(id) ON DELETE CASCADE,
      snapshot       BLOB    NOT NULL,
      version_vector BLOB    NOT NULL,
      frontiers      BLOB    NOT NULL,
      checksum       TEXT    NOT NULL,
      captured_at    INTEGER NOT NULL,
      UNIQUE(document_id, frontiers)
    );
    CREATE INDEX document_versions_document
      ON document_versions(document_id, captured_at DESC);

    CREATE TABLE document_history (
      id          TEXT PRIMARY KEY,
      document_id TEXT    NOT NULL REFERENCES loro_documents(id) ON DELETE CASCADE,
      version_id  TEXT    NOT NULL REFERENCES document_versions(id) ON DELETE RESTRICT,
      reason      TEXT    NOT NULL,
      recorded_at INTEGER NOT NULL,
      label       TEXT,
      message     TEXT,
      boundary    TEXT,
      pinned      INTEGER NOT NULL
    );
    CREATE INDEX document_history_timeline
      ON document_history(document_id, recorded_at DESC, id DESC);
    ",
    },
    Migration {
        to: 5,
        name: "document_creations",
        columns: &[],
        sql: "
    CREATE TABLE document_creations (
      scope          TEXT    NOT NULL,
      request_key    TEXT    NOT NULL,
      request_hash   TEXT    NOT NULL,
      document_id    TEXT    NOT NULL REFERENCES loro_documents(id) ON DELETE CASCADE,
      initial_update BLOB    NOT NULL,
      created_at     INTEGER NOT NULL,
      PRIMARY KEY (scope, request_key)
    );
    CREATE INDEX document_creations_expiry
      ON document_creations(created_at);
    ",
    },
    Migration {
        to: 6,
        name: "loro_updates: origin_json",
        // Existing rows retain `from_client` and decode through the fallback
        // below. New rows write a full per-update origin here.
        sql: "",
        columns: &[("loro_updates", "origin_json", "BLOB")],
    },
    Migration {
        to: 7,
        name: "documents: mime",
        // Derived listing metadata. Existing rows safely read as unknown until
        // the manifest's next wholesale reindex fills the column.
        sql: "",
        columns: &[("documents", "mime", "TEXT")],
    },
    Migration {
        to: 8,
        name: "origin_json and mime, for the databases that missed them",
        // Two development branches briefly gave schema versions 6 and 7
        // different meanings. A database can therefore be stamped 7 with
        // either of these columns absent. Repeating both additions through the
        // idempotent `columns` path converges every historical shape without
        // rewriting durable updates or the rebuildable document projection.
        sql: "",
        columns: &[
            ("loro_updates", "origin_json", "BLOB"),
            ("documents", "mime", "TEXT"),
        ],
    },
    Migration {
        to: 9,
        name: "blobs: attributes",
        // A blob's typed attribute map as JSON (origin device today, the
        // extension point for every later fact). Nullable: a row without it
        // simply has no attributes. The reviewable artifact is
        // crates/koru-store-sqlite/migrations/009_workspace_blob_attributes_migration.sql; execution
        // stays on the idempotent columns path because SQLite's ADD COLUMN
        // cannot be made re-runnable in plain SQL.
        sql: "",
        columns: &[("blobs", "attributes", "TEXT")],
    },
    Migration {
        to: 10,
        name: "blob origins",
        // Durable provenance learned from peer advertisements, so a freshly
        // booted device can still name the origin of a blob it never
        // downloaded. First record wins; never consulted for routing.
        columns: &[],
        sql: include_str!("../migrations/010_workspace_blob_origins_migration.sql"),
    },
    Migration {
        to: 11,
        name: "documents: note_doc",
        // The derived projection of a file's attached note. Same shape as the
        // `mime` column it sits beside: `documents` is a rebuildable
        // projection, so there is nothing to backfill — the next wholesale
        // `reindex` writes the values, and until then a row reads as having
        // no note, which is what every existing row is.
        sql: "",
        columns: &[("documents", "note_doc", "TEXT")],
    },
    Migration {
        to: 12,
        name: "Cloud origin fence",
        // D172: existing origin_json stores Cloud provenance. No columns or
        // Loro bytes change; this is the older-writer compatibility fence.
        sql: "",
        columns: &[],
    },
    Migration {
        to: 13,
        name: "document_private_crdts",
        // A reader's private, device-local view state per document (toggle
        // folds). Cascades with the `loro_documents` row.
        columns: &[],
        sql: include_str!("../migrations/013_workspace_document_private_crdts_migration.sql"),
    },
];

/// The checksum a checkpoint is verified against.
///
/// BLAKE3, the same function that addresses blobs — one hash in the store
/// rather than two, and it is fast enough that verifying on every load is not a
/// reason to skip verifying.
pub fn checksum(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

pub struct SqliteWorkspace {
    connection: Connection,
}

impl SqliteWorkspace {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Self::from_connection(db::open(path)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::from_connection(db::open_in_memory()?)
    }

    fn from_connection(mut connection: Connection) -> Result<Self> {
        let before = db::user_version(&connection)?;
        let after = db::migrate(
            &mut connection,
            "workspace.db",
            MIGRATIONS,
            WORKSPACE_SCHEMA_VERSION,
        )?;
        let mut missing = Vec::new();
        for (table, column) in [("loro_updates", "origin_json"), ("documents", "mime")] {
            if !db::has_column(&connection, table, column)? {
                missing.push(format!("{table}.{column}"));
            }
        }
        if !missing.is_empty() {
            return Err(Error::new(
                Code::CorruptState,
                format!(
                    "workspace schema {before} migrated to {after}, but required columns are missing: {}",
                    missing.join(", ")
                ),
            ));
        }
        Ok(Self { connection })
    }

    pub fn schema_version(&self) -> Result<i64> {
        db::user_version(&self.connection)
    }

    pub fn quick_check(&self) -> Result<bool> {
        db::quick_check(&self.connection)
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    pub(crate) fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }

    /// The highest sequence this document has reached, over **both** tables.
    ///
    /// Both, because pruning removes update rows: after a checkpoint at 4 with
    /// nothing since, `max(seq)` over `loro_updates` is null and the next
    /// update must still be 5, not 1.
    fn head(connection: &Connection, id: DocumentId) -> Result<u64> {
        let key = id.to_storage();
        let from_updates: Option<i64> = connection
            .query_row(
                "SELECT max(seq) FROM loro_updates WHERE document_id = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_failed)?
            .flatten();
        let from_checkpoint: Option<i64> = connection
            .query_row(
                "SELECT latest_checkpoint_seq FROM loro_documents WHERE id = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_failed)?
            .flatten();
        Ok(from_updates
            .unwrap_or(0)
            .max(from_checkpoint.unwrap_or(0))
            .max(0) as u64)
    }

    fn require_document(&self, id: DocumentId) -> Result<()> {
        let known: Option<i64> = self
            .connection
            .query_row(
                "SELECT 1 FROM loro_documents WHERE id = ?1",
                params![id.to_storage()],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_failed)?;
        known
            .map(|_| ())
            .ok_or_else(|| Error::bare(Code::DocumentNotFound))
    }
}

impl WorkspaceRepo for SqliteWorkspace {
    // ------------------------------------------------------------- documents

    fn documents(&self) -> Result<Vec<DocumentState>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT id, kind, latest_checkpoint_seq, version_vector, frontiers, updated_at
                 FROM loro_documents ORDER BY id",
            )
            .map_err(sql_failed)?;
        let rows = stmt.query_map([], read_state).map_err(sql_failed)?;
        collect(rows)
    }

    fn document(&self, id: DocumentId) -> Result<Option<DocumentState>> {
        self.connection
            .query_row(
                "SELECT id, kind, latest_checkpoint_seq, version_vector, frontiers, updated_at
                 FROM loro_documents WHERE id = ?1",
                params![id.to_storage()],
                read_state,
            )
            .optional()
            .map_err(sql_failed)?
            .transpose()
    }

    fn create_document(&mut self, id: DocumentId, kind: DocumentKind, at: Timestamp) -> Result<()> {
        let n = self
            .connection
            .execute(
                "INSERT OR IGNORE INTO loro_documents
                   (id, kind, latest_checkpoint_seq, version_vector, frontiers, updated_at)
                 VALUES (?1, ?2, NULL, x'', x'', ?3)",
                params![id.to_storage(), kind.as_str(), at.millis()],
            )
            .map_err(sql_failed)?;
        if n == 0 {
            return Err(Error::new(
                Code::DocumentExists,
                "already in this workspace",
            ));
        }
        Ok(())
    }

    fn seed_document(&mut self, seed: &SeedDocument) -> Result<SeededDocument> {
        let tx = self.connection.transaction().map_err(sql_failed)?;
        tx.execute(
            "DELETE FROM document_creations WHERE created_at < ?1",
            params![seed.expires_before.millis()],
        )
        .map_err(sql_failed)?;

        // A v2 caller-minted candidate is the create identity. Content was
        // committed before manifest publication, so finding this row can mean
        // the earlier call died in that safe gap. Reuse its original first
        // update and let Store finish publication; never replace those bytes
        // with a later retry body. This deliberately precedes receipt/hash
        // lookup because the transport receipt is allowed to expire while the
        // document identity is not.
        if seed.recover_candidate {
            let existing: Option<(String, Vec<u8>)> = tx
                .query_row(
                    "SELECT d.kind, u.update_bytes
                       FROM loro_documents d
                       JOIN loro_updates u ON u.document_id = d.id AND u.seq = 1
                      WHERE d.id = ?1",
                    params![seed.candidate.to_storage()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(sql_failed)?;
            if let Some((kind, initial_update)) = existing {
                if kind != DocumentKind::Document.as_str() {
                    return Err(Error::new(
                        Code::IdempotencyConflict,
                        "the candidate identity belongs to another document kind",
                    ));
                }
                tx.commit().map_err(sql_failed)?;
                return Ok(SeededDocument {
                    document: seed.candidate,
                    initial_update,
                    replayed: true,
                });
            }
        }

        let existing: Option<(String, String, Vec<u8>)> = tx
            .query_row(
                "SELECT request_hash, document_id, initial_update
                   FROM document_creations
                  WHERE scope = ?1 AND request_key = ?2",
                params![seed.scope, seed.key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(sql_failed)?;
        if let Some((request_hash, document, initial_update)) = existing {
            let document = DocumentId::parse(&document).ok_or_else(|| {
                Error::new(
                    Code::CorruptState,
                    "an idempotency record contains an invalid document id",
                )
            })?;
            if request_hash != seed.request_hash {
                return Err(Error::new(
                    Code::IdempotencyConflict,
                    "that idempotency key was already used for a different request",
                ));
            }
            tx.commit().map_err(sql_failed)?;
            return Ok(SeededDocument {
                document,
                initial_update,
                replayed: true,
            });
        }

        let document = seed.candidate.to_storage();
        tx.execute(
            "INSERT INTO loro_documents
               (id, kind, latest_checkpoint_seq, version_vector, frontiers, updated_at)
             VALUES (?1, ?2, NULL, ?3, ?4, ?5)",
            params![
                document,
                DocumentKind::Document.as_str(),
                seed.version_vector,
                seed.frontiers,
                seed.created_at.millis()
            ],
        )
        .map_err(sql_failed)?;
        tx.execute(
            "INSERT INTO loro_updates
               (document_id, seq, update_bytes, received_at, from_client)
             VALUES (?1, 1, ?2, ?3, NULL)",
            params![document, seed.initial_update, seed.created_at.millis()],
        )
        .map_err(sql_failed)?;
        tx.execute(
            "INSERT INTO document_creations
               (scope, request_key, request_hash, document_id, initial_update, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                seed.scope,
                seed.key,
                seed.request_hash,
                document,
                seed.initial_update,
                seed.created_at.millis()
            ],
        )
        .map_err(sql_failed)?;
        tx.commit().map_err(sql_failed)?;
        Ok(SeededDocument {
            document: seed.candidate,
            initial_update: seed.initial_update.clone(),
            replayed: false,
        })
    }

    fn drop_document(&mut self, id: DocumentId) -> Result<()> {
        // Cascades to checkpoints and updates. Always the *later* of the two
        // steps: the manifest unregisters first, so a crash between them leaves
        // unreferenced bytes rather than a reference to nothing.
        self.connection
            .execute(
                "DELETE FROM loro_documents WHERE id = ?1",
                params![id.to_storage()],
            )
            .map_err(sql_failed)?;
        Ok(())
    }

    // ----------------------------------------------------------- checkpoints

    fn checkpoints(&self, id: DocumentId) -> Result<Vec<CheckpointHeader>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT seq, length(snapshot), checksum, created_at
                 FROM loro_checkpoints WHERE document_id = ?1 ORDER BY seq DESC",
            )
            .map_err(sql_failed)?;
        let rows = stmt
            .query_map(params![id.to_storage()], |row| {
                Ok(Ok(CheckpointHeader {
                    seq: row.get::<_, i64>(0)? as u64,
                    size: row.get::<_, i64>(1)?.max(0) as u64,
                    checksum: row.get(2)?,
                    created_at: Timestamp::from_millis(row.get(3)?),
                }))
            })
            .map_err(sql_failed)?;
        collect(rows)
    }

    fn checkpoint(&self, id: DocumentId, seq: u64) -> Result<Option<Checkpoint>> {
        self.connection
            .query_row(
                "SELECT seq, snapshot, version_vector, frontiers, checksum, created_at
                 FROM loro_checkpoints WHERE document_id = ?1 AND seq = ?2",
                params![id.to_storage(), seq as i64],
                read_checkpoint,
            )
            .optional()
            .map_err(sql_failed)?
            .transpose()
    }

    fn latest_sound_checkpoint(&self, id: DocumentId) -> Result<Option<Checkpoint>> {
        // Newest first, returning the first whose bytes match what was recorded
        // when they were written. A damaged checkpoint is skipped rather than
        // returned — which is the entire reason more than one is kept.
        let mut stmt = self
            .connection
            .prepare(
                "SELECT seq, snapshot, version_vector, frontiers, checksum, created_at
                 FROM loro_checkpoints WHERE document_id = ?1 ORDER BY seq DESC",
            )
            .map_err(sql_failed)?;
        let rows = stmt
            .query_map(params![id.to_storage()], read_checkpoint)
            .map_err(sql_failed)?;
        for row in rows {
            let checkpoint = row.map_err(sql_failed)??;
            if checksum(&checkpoint.snapshot) == checkpoint.checksum {
                return Ok(Some(checkpoint));
            }
        }
        Ok(None)
    }

    // --------------------------------------------------------------- updates

    fn updates_after(&self, id: DocumentId, after: Option<u64>) -> Result<Vec<StoredUpdate>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT seq, update_bytes, received_at, from_client, origin_json
                 FROM loro_updates WHERE document_id = ?1 AND seq > ?2 ORDER BY seq",
            )
            .map_err(sql_failed)?;
        let rows = stmt
            .query_map(params![id.to_storage(), after.unwrap_or(0) as i64], |row| {
                let from: Option<String> = row.get(3)?;
                let origin_json: Option<Vec<u8>> = row.get(4)?;
                let origin = match origin_json.as_deref() {
                    Some(bytes) => match serde_json::from_slice(bytes) {
                        Ok(origin) => origin,
                        Err(_) => {
                            return Ok(Err(Error::new(
                                Code::CorruptState,
                                "journal update origin is not a recognized value",
                            )));
                        }
                    },
                    // SQL NULL is a historical row, not malformed JSON. Do
                    // not rewrite it merely because a new reader opened it.
                    None => from.as_deref().and_then(ClientId::parse).into(),
                };
                Ok(Ok(StoredUpdate {
                    seq: row.get::<_, i64>(0)? as u64,
                    bytes: row.get(1)?,
                    received_at: Timestamp::from_millis(row.get(2)?),
                    origin,
                }))
            })
            .map_err(sql_failed)?;
        collect(rows)
    }

    fn latest_update_origin(&self, id: DocumentId) -> Result<Option<(Timestamp, UpdateOrigin)>> {
        let row = self
            .connection
            .query_row(
                "SELECT received_at, from_client, origin_json
                 FROM loro_updates WHERE document_id = ?1 ORDER BY seq DESC LIMIT 1",
                params![id.to_storage()],
                |row| {
                    let received_at: i64 = row.get(0)?;
                    let from: Option<String> = row.get(1)?;
                    let origin_json: Option<Vec<u8>> = row.get(2)?;
                    Ok((received_at, from, origin_json))
                },
            )
            .optional()
            .map_err(sql_failed)?;
        let Some((received_at, from, origin_json)) = row else {
            return Ok(None);
        };
        let origin = match origin_json.as_deref() {
            // A row this reader cannot decode is reported as the store's own
            // rather than refused: this answers a diagnostic question, and
            // failing the whole query over one unrecognised provenance would
            // hide every fact beside it.
            Some(bytes) => serde_json::from_slice(bytes).unwrap_or_default(),
            // SQL NULL is a historical row, not malformed JSON — the same
            // reading `updates_after` gives it.
            None => from.as_deref().and_then(ClientId::parse).into(),
        };
        Ok(Some((Timestamp::from_millis(received_at), origin)))
    }

    fn update_count(&self, id: DocumentId) -> Result<u64> {
        let n: i64 = self
            .connection
            .query_row(
                "SELECT count(*) FROM loro_updates WHERE document_id = ?1",
                params![id.to_storage()],
                |r| r.get(0),
            )
            .map_err(sql_failed)?;
        Ok(n.max(0) as u64)
    }

    fn journal_since(&self, id: DocumentId, after: Option<u64>) -> Result<JournalSince> {
        let (head, bytes): (Option<i64>, i64) = self
            .connection
            .query_row(
                "SELECT MAX(seq), COALESCE(SUM(LENGTH(update_bytes)), 0)
                 FROM loro_updates WHERE document_id = ?1 AND seq > ?2",
                params![id.to_storage(), after.map_or(-1, |after| after as i64)],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(sql_failed)?;
        Ok(JournalSince {
            head: head.map(|head| head.max(0) as u64),
            bytes: bytes.max(0) as u64,
        })
    }

    /// **One transaction**: the journal rows and the document's new position.
    fn commit_updates(&mut self, batch: &UpdateBatch) -> Result<u64> {
        self.require_document(batch.document)?;
        let head = Self::head(&self.connection, batch.document)?;
        if batch.updates.is_empty() {
            // Nothing to persist. Deliberately not an error and deliberately
            // not a write: an empty window is a normal thing for a commit
            // timer to see.
            return Ok(head);
        }

        let key = batch.document.to_storage();
        if batch.origins.len() != batch.updates.len() {
            return Err(Error::new(
                Code::Internal,
                "an update batch did not carry exactly one origin per update",
            ));
        }
        let tx = self
            .connection
            .transaction()
            .map_err(|e| Error::new(Code::StorageFailed, format!("begin commit: {e}")))?;

        let mut seq = head;
        {
            let mut insert = tx
                .prepare(
                    "INSERT INTO loro_updates
                       (document_id, seq, update_bytes, received_at, from_client, origin_json)
                     VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
                )
                .map_err(sql_failed)?;
            for (bytes, origin) in batch.updates.iter().zip(&batch.origins) {
                seq += 1;
                let origin = serde_json::to_vec(origin).map_err(|error| {
                    Error::new(Code::Internal, format!("encode update origin: {error}"))
                })?;
                insert
                    .execute(params![
                        key,
                        seq as i64,
                        bytes.as_slice(),
                        batch.received_at.millis(),
                        origin
                    ])
                    .map_err(sql_failed)?;
            }
        }

        tx.execute(
            "UPDATE loro_documents
               SET version_vector = ?2, frontiers = ?3, updated_at = ?4
             WHERE id = ?1",
            params![
                key,
                batch.version_vector.as_slice(),
                batch.frontiers.as_slice(),
                batch.received_at.millis()
            ],
        )
        .map_err(sql_failed)?;

        // The fsync. Nothing is acknowledged before this returns.
        tx.commit()
            .map_err(|e| Error::new(Code::StorageFailed, format!("commit: {e}")))?;
        Ok(seq)
    }

    /// **One transaction**: write the checkpoint, move the position, prune what
    /// the checkpoint now covers.
    fn commit_checkpoint(&mut self, id: DocumentId, checkpoint: &Checkpoint) -> Result<usize> {
        self.require_document(id)?;
        if checksum(&checkpoint.snapshot) != checkpoint.checksum {
            // Refusing here rather than at load is the difference between a
            // caller's bug and a recovery.
            return Err(Error::new(
                Code::CorruptState,
                "the checkpoint's checksum does not match its bytes",
            ));
        }

        let key = id.to_storage();
        // The previous checkpoint, read *before* the new one is inserted. Only
        // what it already covered may be pruned — see the module docs. Without
        // a previous checkpoint nothing is pruned at all, which is correct: the
        // first snapshot has no older one to fall back to.
        let previous: Option<i64> = self
            .connection
            .query_row(
                "SELECT max(seq) FROM loro_checkpoints WHERE document_id = ?1 AND seq < ?2",
                params![key, checkpoint.seq as i64],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_failed)?
            .flatten();

        let tx = self
            .connection
            .transaction()
            .map_err(|e| Error::new(Code::StorageFailed, format!("begin checkpoint: {e}")))?;

        tx.execute(
            "INSERT OR REPLACE INTO loro_checkpoints
               (document_id, seq, snapshot, version_vector, frontiers, checksum, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                key,
                checkpoint.seq as i64,
                checkpoint.snapshot.as_slice(),
                checkpoint.version_vector.as_slice(),
                checkpoint.frontiers.as_slice(),
                checkpoint.checksum,
                checkpoint.created_at.millis()
            ],
        )
        .map_err(sql_failed)?;

        tx.execute(
            "UPDATE loro_documents
               SET latest_checkpoint_seq = ?2, version_vector = ?3, frontiers = ?4, updated_at = ?5
             WHERE id = ?1",
            params![
                key,
                checkpoint.seq as i64,
                checkpoint.version_vector.as_slice(),
                checkpoint.frontiers.as_slice(),
                checkpoint.created_at.millis()
            ],
        )
        .map_err(sql_failed)?;

        let pruned = match previous {
            Some(p) => tx
                .execute(
                    "DELETE FROM loro_updates WHERE document_id = ?1 AND seq <= ?2",
                    params![key, p],
                )
                .map_err(sql_failed)?,
            None => 0,
        };

        tx.commit()
            .map_err(|e| Error::new(Code::StorageFailed, format!("commit checkpoint: {e}")))?;
        Ok(pruned)
    }

    // --------------------------------------------------------------- history

    fn capture_history(&mut self, capture: &HistoryCapture<'_>) -> Result<HistoryEntry> {
        self.require_document(capture.document)?;
        if checksum(capture.snapshot) != capture.checksum {
            return Err(Error::new(
                Code::CorruptState,
                "the history version checksum does not match its bytes",
            ));
        }
        let document = capture.document.to_storage();
        let tx = self.connection.transaction().map_err(sql_failed)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM document_versions WHERE document_id = ?1 AND frontiers = ?2",
                params![document, capture.frontiers],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_failed)?;
        let version = match existing.as_deref().and_then(DocumentVersionId::parse) {
            Some(id) => id,
            None => {
                let id = DocumentVersionId::new();
                tx.execute(
                    "INSERT INTO document_versions
                       (id, document_id, snapshot, version_vector, frontiers, checksum, captured_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        id.to_storage(),
                        document,
                        capture.snapshot,
                        capture.version_vector,
                        capture.frontiers,
                        capture.checksum,
                        capture.recorded_at.millis()
                    ],
                )
                .map_err(sql_failed)?;
                id
            }
        };
        let entry = HistoryEntry {
            id: HistoryEntryId::new(),
            document: capture.document,
            version,
            reason: capture.reason,
            recorded_at: capture.recorded_at,
            label: capture.label.map(str::to_owned),
            message: capture.message.map(str::to_owned),
            boundary: capture.boundary.map(str::to_owned),
            pinned: capture.reason.pinned(),
        };
        tx.execute(
            "INSERT INTO document_history
               (id, document_id, version_id, reason, recorded_at, label, message, boundary, pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                entry.id.to_storage(),
                document,
                entry.version.to_storage(),
                entry.reason.as_str(),
                entry.recorded_at.millis(),
                entry.label,
                entry.message,
                entry.boundary,
                i64::from(entry.pinned)
            ],
        )
        .map_err(sql_failed)?;
        tx.commit().map_err(sql_failed)?;
        Ok(entry)
    }

    fn history_entries(
        &self,
        id: DocumentId,
        before: Option<(Timestamp, HistoryEntryId)>,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>> {
        self.require_document(id)?;
        let limit = limit.clamp(1, 200) as i64;
        let (before_at, before_id) = before
            .map(|(at, id)| (at.millis(), id.to_storage()))
            .unwrap_or((i64::MAX, "ffffffff-ffff-ffff-ffff-ffffffffffff".to_string()));
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, document_id, version_id, reason, recorded_at,
                    label, message, boundary, pinned
               FROM document_history
              WHERE document_id = ?1
                AND (recorded_at < ?2 OR (recorded_at = ?2 AND id < ?3))
              ORDER BY recorded_at DESC, id DESC LIMIT ?4",
            )
            .map_err(sql_failed)?;
        let rows = statement
            .query_map(
                params![id.to_storage(), before_at, before_id, limit],
                read_history_entry,
            )
            .map_err(sql_failed)?;
        collect(rows)
    }

    fn history_entry(&self, id: HistoryEntryId) -> Result<Option<HistoryEntry>> {
        self.connection
            .query_row(
                "SELECT id, document_id, version_id, reason, recorded_at,
                    label, message, boundary, pinned
               FROM document_history WHERE id = ?1",
                params![id.to_storage()],
                read_history_entry,
            )
            .optional()
            .map_err(sql_failed)?
            .transpose()
    }

    fn relabel_history_entry(
        &mut self,
        id: HistoryEntryId,
        label: Option<&str>,
    ) -> Result<HistoryEntry> {
        let changed = self
            .connection
            .execute(
                "UPDATE document_history SET label = ?2 WHERE id = ?1",
                params![id.to_storage(), label],
            )
            .map_err(sql_failed)?;
        if changed == 0 {
            return Err(Error::bare(Code::HistoryEntryNotFound));
        }
        self.history_entry(id)?
            .ok_or_else(|| Error::bare(Code::HistoryEntryNotFound))
    }

    /// One indexed row and one small blob — no snapshot is read.
    ///
    /// The `ORDER BY` matches `history_entries`' newest-first ordering exactly,
    /// down to the `id DESC` tiebreak, so "the newest marker" means the same
    /// thing to both. It rides `document_history_timeline`.
    fn newest_history_version(&self, id: DocumentId) -> Result<Option<Vec<u8>>> {
        self.connection
            .query_row(
                "SELECT v.version_vector
                   FROM document_history h
                   JOIN document_versions v ON v.id = h.version_id
                  WHERE h.document_id = ?1
                  ORDER BY h.recorded_at DESC, h.id DESC LIMIT 1",
                params![id.to_storage()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_failed)
    }

    fn document_version(&self, id: DocumentVersionId) -> Result<Option<DocumentVersion>> {
        let version = self
            .connection
            .query_row(
                "SELECT id, document_id, snapshot, version_vector, frontiers, checksum, captured_at
               FROM document_versions WHERE id = ?1",
                params![id.to_storage()],
                read_document_version,
            )
            .optional()
            .map_err(sql_failed)?
            .transpose()?;
        match version {
            Some(version) if checksum(&version.snapshot) != version.checksum => Err(Error::new(
                Code::CorruptState,
                "the requested history version failed its checksum",
            )),
            other => Ok(other),
        }
    }

    // ----------------------------------------------------------------- blobs

    fn put_blob(&mut self, blob: &Blob) -> Result<()> {
        // Empty is NULL, not `{}` — the `COALESCE` below treats NULL as "no
        // attributes yet" and adopts the newcomer's, while `{}` would be a
        // record that blocks adoption forever.
        let attributes =
            if blob.attributes.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&blob.attributes).map_err(|e| {
                    Error::new(Code::StorageFailed, format!("blob attributes: {e}"))
                })?)
            };
        self.connection
            .execute(
                "INSERT INTO blobs (hash, size, mime, filename, created_at, attributes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(hash) DO UPDATE SET
                   mime = excluded.mime, filename = excluded.filename,
                   -- Provenance, not last-touch: the first record of these
                   -- bytes keeps its attributes (its origin device is the
                   -- truth); only a row with none yet adopts the newcomer's.
                   attributes = COALESCE(blobs.attributes, excluded.attributes)",
                params![
                    blob.hash.as_str(),
                    blob.size as i64,
                    blob.mime,
                    blob.filename,
                    blob.created_at.millis(),
                    attributes
                ],
            )
            .map_err(sql_failed)?;
        Ok(())
    }

    fn blob(&self, hash: &BlobHash) -> Result<Option<Blob>> {
        self.connection
            .query_row(
                "SELECT hash, size, mime, filename, created_at, attributes FROM blobs WHERE hash = ?1",
                params![hash.as_str()],
                read_blob,
            )
            .optional()
            .map_err(sql_failed)?
            .transpose()
    }

    fn record_blob_origin(&mut self, hash: &BlobHash, device: &str, at: Timestamp) -> Result<()> {
        // First record wins — provenance is who *first* held the bytes, and a
        // re-advertisement of the same hash is the same fact, not a new one.
        self.connection
            .execute(
                "INSERT INTO blob_origins (hash, origin_device, recorded_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(hash) DO NOTHING",
                params![hash.as_str(), device, at.millis()],
            )
            .map_err(sql_failed)?;
        Ok(())
    }

    fn advertised_blob_origin(&self, hash: &BlobHash) -> Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT origin_device FROM blob_origins WHERE hash = ?1",
                params![hash.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_failed)
    }

    fn blobs(&self) -> Result<Vec<Blob>> {
        let mut stmt = self
            .connection
            .prepare("SELECT hash, size, mime, filename, created_at, attributes FROM blobs ORDER BY hash")
            .map_err(sql_failed)?;
        let rows = stmt.query_map([], read_blob).map_err(sql_failed)?;
        collect(rows)
    }

    fn private_snapshot(&self, document: DocumentId, owner: ProfileId) -> Result<Option<Vec<u8>>> {
        self.connection
            .query_row(
                "SELECT snapshot FROM document_private_crdts
                 WHERE document_id = ?1 AND owner_profile_id = ?2",
                params![document.to_storage(), owner.to_storage()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_failed)
    }

    fn put_private_snapshot(
        &mut self,
        document: DocumentId,
        owner: ProfileId,
        snapshot: &[u8],
        at: Timestamp,
    ) -> Result<()> {
        // One statement is one transaction: the row is either the old
        // snapshot or the new one, never a mix.
        self.connection
            .execute(
                "INSERT INTO document_private_crdts
                   (document_id, owner_profile_id, snapshot, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (document_id, owner_profile_id)
                 DO UPDATE SET snapshot = excluded.snapshot, updated_at = excluded.updated_at",
                params![
                    document.to_storage(),
                    owner.to_storage(),
                    snapshot,
                    at.millis()
                ],
            )
            .map_err(sql_failed)?;
        Ok(())
    }

    fn private_snapshots(&self, document: DocumentId) -> Result<Vec<(ProfileId, Vec<u8>)>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT owner_profile_id, snapshot FROM document_private_crdts
                 WHERE document_id = ?1 ORDER BY owner_profile_id",
            )
            .map_err(sql_failed)?;
        let rows = stmt
            .query_map(params![document.to_storage()], |row| {
                Ok(Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)))
            })
            .map_err(sql_failed)?;
        Ok(collect(rows)?
            .into_iter()
            .filter_map(|(owner, bytes)| Some((ProfileId::parse(&owner)?, bytes)))
            .collect())
    }

    fn prune_private_snapshots(&mut self, listed: &[DocumentId]) -> Result<usize> {
        // Bounded by the private rows, which exist only for documents a
        // reader on this device folded something in — not by the workspace.
        let listed: std::collections::HashSet<String> =
            listed.iter().map(|id| id.to_storage()).collect();
        let held: Vec<String> = {
            let mut stmt = self
                .connection
                .prepare("SELECT DISTINCT document_id FROM document_private_crdts")
                .map_err(sql_failed)?;
            let rows = stmt
                .query_map([], |row| Ok(Ok(row.get::<_, String>(0)?)))
                .map_err(sql_failed)?;
            collect(rows)?
        };
        let tx = self.connection.transaction().map_err(sql_failed)?;
        let mut pruned = 0;
        for document in held.iter().filter(|id| !listed.contains(*id)) {
            pruned += tx
                .execute(
                    "DELETE FROM document_private_crdts WHERE document_id = ?1",
                    params![document],
                )
                .map_err(sql_failed)?;
        }
        tx.commit().map_err(sql_failed)?;
        Ok(pruned)
    }

    fn size_bytes(&self) -> Result<u64> {
        db::size_bytes(&self.connection)
    }
}

// ------------------------------------------------------------------ row readers

type RowResult<T> = rusqlite::Result<Result<T>>;

fn read_state(row: &rusqlite::Row<'_>) -> RowResult<DocumentState> {
    let id: String = row.get(0)?;
    let kind: String = row.get(1)?;
    let latest: Option<i64> = row.get(2)?;
    let version_vector: Vec<u8> = row.get(3)?;
    let frontiers: Vec<u8> = row.get(4)?;
    let updated_at: i64 = row.get(5)?;
    Ok((|| {
        Ok(DocumentState {
            id: DocumentId::parse(&id)
                .ok_or_else(|| Error::new(Code::CorruptState, "document id is not a uuid"))?,
            kind: DocumentKind::parse(&kind)
                .ok_or_else(|| Error::new(Code::CorruptState, "unknown document kind"))?,
            latest_checkpoint_seq: latest.map(|s| s.max(0) as u64),
            version_vector,
            frontiers,
            updated_at: Timestamp::from_millis(updated_at),
        })
    })())
}

fn read_checkpoint(row: &rusqlite::Row<'_>) -> RowResult<Checkpoint> {
    Ok(Ok(Checkpoint {
        seq: row.get::<_, i64>(0)?.max(0) as u64,
        snapshot: row.get(1)?,
        version_vector: row.get(2)?,
        frontiers: row.get(3)?,
        checksum: row.get(4)?,
        created_at: Timestamp::from_millis(row.get(5)?),
    }))
}

fn read_blob(row: &rusqlite::Row<'_>) -> RowResult<Blob> {
    let hash: String = row.get(0)?;
    let size: i64 = row.get(1)?;
    let mime: String = row.get(2)?;
    let filename: Option<String> = row.get(3)?;
    let created_at: i64 = row.get(4)?;
    let attributes: Option<String> = row.get(5)?;
    Ok((|| {
        Ok(Blob {
            hash: BlobHash::parse(&hash)?,
            size: size.max(0) as u64,
            mime,
            filename,
            created_at: Timestamp::from_millis(created_at),
            // Attributes are hints (origin, and future facts): a value this
            // build cannot parse reads as none rather than orphaning the blob
            // behind a read error on every workspace open.
            attributes: attributes
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_default(),
        })
    })())
}

fn read_history_entry(row: &rusqlite::Row<'_>) -> RowResult<HistoryEntry> {
    let id: String = row.get(0)?;
    let document: String = row.get(1)?;
    let version: String = row.get(2)?;
    let reason: String = row.get(3)?;
    let recorded_at: i64 = row.get(4)?;
    let label: Option<String> = row.get(5)?;
    let message: Option<String> = row.get(6)?;
    let boundary: Option<String> = row.get(7)?;
    let pinned: i64 = row.get(8)?;
    Ok((|| {
        Ok(HistoryEntry {
            id: HistoryEntryId::parse(&id)
                .ok_or_else(|| Error::new(Code::CorruptState, "history entry id is not a uuid"))?,
            document: DocumentId::parse(&document).ok_or_else(|| {
                Error::new(Code::CorruptState, "history document id is not a uuid")
            })?,
            version: DocumentVersionId::parse(&version).ok_or_else(|| {
                Error::new(Code::CorruptState, "history version id is not a uuid")
            })?,
            reason: HistoryReason::parse(&reason)
                .ok_or_else(|| Error::new(Code::CorruptState, "unknown history reason"))?,
            recorded_at: Timestamp::from_millis(recorded_at),
            label,
            message,
            boundary,
            pinned: pinned != 0,
        })
    })())
}

fn read_document_version(row: &rusqlite::Row<'_>) -> RowResult<DocumentVersion> {
    let id: String = row.get(0)?;
    let document: String = row.get(1)?;
    let snapshot: Vec<u8> = row.get(2)?;
    let version_vector: Vec<u8> = row.get(3)?;
    let frontiers: Vec<u8> = row.get(4)?;
    let checksum: String = row.get(5)?;
    let captured_at: i64 = row.get(6)?;
    Ok((|| {
        Ok(DocumentVersion {
            id: DocumentVersionId::parse(&id).ok_or_else(|| {
                Error::new(Code::CorruptState, "history version id is not a uuid")
            })?,
            document: DocumentId::parse(&document).ok_or_else(|| {
                Error::new(Code::CorruptState, "history document id is not a uuid")
            })?,
            snapshot,
            version_vector,
            frontiers,
            checksum,
            captured_at: Timestamp::from_millis(captured_at),
        })
    })())
}

#[cfg(test)]
mod tests;
