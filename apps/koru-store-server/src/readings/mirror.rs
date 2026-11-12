// SPDX-License-Identifier: MPL-2.0

//! `readings.db` — the readings mirror (OCR TECHNICAL §7, "Derived,
//! machine-local").
//!
//! At the **store root**, beside `projection.db` and `search.db`, and for the
//! same reasons: derived, disposable, cross-workspace, never carried in a
//! bundle and never synced. What it mirrors is the reading documents the Store
//! holds — one per file, and one per page that holds pictures — so the index
//! can ask "what words were read out of this blob" with one indexed read
//! instead of opening a CRDT document per hit.
//!
//! ## Four tables
//!
//! - `readings` — one row per blob per parent entry: the entry's generation,
//!   status, reason, source and page count, as its reading document says.
//! - `reading_pages` — the words, **once per workspace and hash**, from the
//!   row with the highest generation and best status. The same picture in
//!   three pages is read once and its words are held once here.
//! - `reading_sources` — what the mirror has applied: each reading document's
//!   revision, its workspace and its parent. The catch-up compares this with
//!   the Store, and the parent is how a document that leaves the listing takes
//!   its rows with it.
//! - `read_attempts` — failures that are true **only on this computer** (a
//!   crash, an exhausted worker, a format it cannot decode, a file that needs
//!   the reader), and their backoff. Never synced: another device may succeed.
//!
//! ## A schema bump is a rebuild
//!
//! As the projection's and the index's: if the stored
//! [`READINGS_SCHEMA_VERSION`] differs, every table is dropped and the catch-up
//! applies every reading document again. There is no migration chain.
//!
//! ## What it never holds
//!
//! A file's name, a title or a path. The words are held in clear, as
//! `search.db` holds every document's text, under the user's filesystem
//! permissions (TECHNICAL §13). Nothing here logs one.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use koru_read::{GENERATION, Reading, ReadingStatus, Reason};
use koru_store_core::ids::{DocumentId, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

/// The shape of everything below. A change is a rebuild.
///
/// 1 is the first: `readings`, `reading_pages`, `reading_sources`,
/// `read_attempts`.
pub const READINGS_SCHEMA_VERSION: i64 = 1;

/// The name in the log, so the file that ran is the file on disk.
const MIGRATION_NAME: &str = "027_readings_mirror_migration.sql";

const READINGS_SCHEMA_MIGRATION: &str =
    include_str!("../../migrations/027_readings_mirror_migration.sql");

/// Statements carry at most this many hashes, well under SQLite's bound.
const CHUNK: usize = 500;

/// Why a mirror operation did not happen. Carries SQLite's message, which
/// names tables and constraints and never a row's value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MirrorError(pub String);

impl std::fmt::Display for MirrorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<rusqlite::Error> for MirrorError {
    fn from(e: rusqlite::Error) -> Self {
        Self(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, MirrorError>;

/// One reading document, as the mirror applies it: its identity, the revision
/// it was read at, and every entry it holds.
pub struct Applied {
    pub workspace: WorkspaceId,
    pub parent: DocumentId,
    pub document: DocumentId,
    /// `VersionVector::encode()` of the state the entries were read from.
    pub revision: Vec<u8>,
    pub entries: Vec<(String, Reading)>,
}

/// What an application or a removal moved: the parent entry, and every hash
/// whose row or words changed — what the search index is told about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changed {
    pub workspace: WorkspaceId,
    pub parent: DocumentId,
    pub hashes: Vec<String>,
}

/// A local failure, as `read_attempts` holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    pub code: AttemptCode,
    pub attempts: u32,
    pub last_at: i64,
}

/// Why a file could not be read **on this computer**: the worker's own
/// [`koru_read::FailureCode`]s that are worth remembering, and what the
/// coordinator saw happen to the process.
///
/// `busy` and `cancelled` are absent on purpose: both are about the
/// conversation, not the file, and recording them would hold back a file
/// that did nothing wrong.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AttemptCode {
    /// A format this worker cannot decode here. Not retried at this generation.
    Unsupported,
    /// The object could not be opened. Retried with backoff, a few times.
    Unreadable,
    /// The worker failed and said so. Retried with backoff, a few times.
    Internal,
    /// Recognition is needed and no reader is installed. Waits as
    /// `needs_reader` until one is; **not a failure**.
    NeedsReader,
    /// The worker died mid-job. One retry in a fresh worker, resuming past the
    /// page it died on; then left until the next generation.
    Crashed,
    /// The worker reached its memory limit on this file. As [`Self::Crashed`].
    MemoryLimit,
    /// The worker's own watchdog ended it on this file (exit code 3). Hostile:
    /// never fed again at this generation.
    Hostile,
}

impl AttemptCode {
    pub const ALL: [AttemptCode; 7] = [
        AttemptCode::Unsupported,
        AttemptCode::Unreadable,
        AttemptCode::Internal,
        AttemptCode::NeedsReader,
        AttemptCode::Crashed,
        AttemptCode::MemoryLimit,
        AttemptCode::Hostile,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AttemptCode::Unsupported => "unsupported",
            AttemptCode::Unreadable => "unreadable",
            AttemptCode::Internal => "internal",
            AttemptCode::NeedsReader => "needs-reader",
            AttemptCode::Crashed => "crashed",
            AttemptCode::MemoryLimit => "memory-limit",
            AttemptCode::Hostile => "hostile",
        }
    }

    pub fn parse(token: &str) -> Option<AttemptCode> {
        Self::ALL.into_iter().find(|code| code.as_str() == token)
    }
}

/// Counts the debug route and the subcommand print. Ids and numbers only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MirrorCounts {
    /// `readings` rows by status token (`unknown` for one this build cannot
    /// name), and the synced failure reasons beside them.
    pub by_status: BTreeMap<String, u64>,
    pub failed_by_reason: BTreeMap<String, u64>,
    pub page_capped: u64,
    /// `read_attempts` rows by code.
    pub attempts_by_code: BTreeMap<String, u64>,
    pub documents: u64,
    pub pages: u64,
}

/// One row of `readings`, for the subcommand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadingRow {
    pub workspace: String,
    pub parent: String,
    pub hash: String,
    pub generation: i64,
    pub status: Option<String>,
    pub reason: Option<String>,
    pub source: Option<String>,
    pub page_count: i64,
}

pub struct Mirror {
    db: Connection,
}

impl Mirror {
    /// Open (or create) `readings.db` under a store root.
    pub fn open(store_root: &Path) -> Result<Self> {
        Self::at(&store_root.join("readings.db"))
    }

    pub fn at(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| MirrorError(e.to_string()))?;
        }
        Self::over(Connection::open(path)?)
    }

    /// In this process's memory, under a name every reader of it opens too,
    /// as the projection and the index are in an in-memory composition.
    pub fn in_named_memory(name: &koru_sqlite::MemoryName) -> Result<Self> {
        Self::over(koru_sqlite::open_memory(name)?)
    }

    fn over(db: Connection) -> Result<Self> {
        // WAL so the indexer's reader is never blocked by the mirror's writer,
        // and `NORMAL`: a rebuildable cache, as the projection says of itself.
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        let found: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if found != 0 && found != READINGS_SCHEMA_VERSION {
            let started = std::time::Instant::now();
            tracing::info!(
                database = "readings.db",
                found,
                expected = READINGS_SCHEMA_VERSION,
                migration = MIGRATION_NAME,
                "applying SQL migration; the derived readings mirror will be rebuilt"
            );
            db.execute_batch(READINGS_SCHEMA_MIGRATION)?;
            tracing::info!(
                database = "readings.db",
                from = found,
                to = READINGS_SCHEMA_VERSION,
                migration = MIGRATION_NAME,
                elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                "SQL migration applied"
            );
        }
        db.execute_batch(SCHEMA)?;
        db.pragma_update(None, "user_version", READINGS_SCHEMA_VERSION)?;
        Ok(Self { db })
    }

    // ------------------------------------------------------------- applying

    /// Bring one reading document's rows level with what it says.
    ///
    /// One transaction: the parent's rows are replaced, the words of every
    /// hash this document now wins are written, the words of a hash it no
    /// longer holds go unless another parent in the workspace still holds it,
    /// and the revision is recorded. Returns the hashes whose row or words
    /// moved — empty when the document said nothing new.
    pub fn apply(&mut self, applied: &Applied) -> Result<Changed> {
        let workspace = applied.workspace.to_storage();
        let parent = applied.parent.to_storage();
        let tx = self.db.transaction()?;
        let before: HashMap<String, Row> = {
            let mut q = tx.prepare(
                "SELECT hash, generation, status, reason, source, page_count
                   FROM readings WHERE parent_document_id = ?1",
            )?;
            let rows = q.query_map(params![parent], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    Row {
                        generation: r.get(1)?,
                        status: r.get(2)?,
                        reason: r.get(3)?,
                        source: r.get(4)?,
                        page_count: r.get(5)?,
                    },
                ))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut changed: Vec<String> = Vec::new();
        let now: HashSet<&str> = applied.entries.iter().map(|(h, _)| h.as_str()).collect();

        for (hash, reading) in &applied.entries {
            let row = Row::of(reading);
            let moved = before.get(hash) != Some(&row);
            tx.execute(
                "INSERT INTO readings (workspace_id, parent_document_id, hash, generation,
                                       status, reason, source, page_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (parent_document_id, hash) DO UPDATE SET
                    workspace_id = excluded.workspace_id,
                    generation = excluded.generation,
                    status = excluded.status,
                    reason = excluded.reason,
                    source = excluded.source,
                    page_count = excluded.page_count",
                params![
                    workspace,
                    parent,
                    hash,
                    row.generation,
                    row.status,
                    row.reason,
                    row.source,
                    row.page_count
                ],
            )?;
            // **Is this the best reading of these bytes in the workspace?**
            // Ties go to this document: a reading is a function of the bytes,
            // the reader and the generation, so two equal rows hold the same
            // words and the newer application is as good as the older.
            let best_elsewhere: Option<(i64, i64, i64)> = {
                let mut q = tx.prepare(
                    "SELECT generation, status, reason, page_count FROM readings
                      WHERE workspace_id = ?1 AND hash = ?2 AND parent_document_id <> ?3",
                )?;
                let rows = q.query_map(params![workspace, hash, parent], |r| {
                    Ok(Row {
                        generation: r.get(0)?,
                        status: r.get(1)?,
                        reason: r.get(2)?,
                        source: None,
                        page_count: r.get(3)?,
                    })
                })?;
                let mut best = None;
                for row in rows {
                    let key = row?.rank();
                    best = best.max(Some(key));
                }
                best
            };
            if best_elsewhere.is_none_or(|other| row.rank() >= other)
                && replace_pages(&tx, &workspace, hash, reading)?
            {
                if !changed.contains(hash) {
                    changed.push(hash.clone());
                }
            } else if moved && !changed.contains(hash) {
                changed.push(hash.clone());
            }
        }

        for hash in before.keys().filter(|h| !now.contains(h.as_str())) {
            tx.execute(
                "DELETE FROM readings WHERE parent_document_id = ?1 AND hash = ?2",
                params![parent, hash],
            )?;
            forget_words_if_unheld(&tx, &workspace, hash)?;
            changed.push(hash.clone());
        }

        tx.execute(
            "INSERT INTO reading_sources (document_id, workspace_id, parent_document_id, revision)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (document_id) DO UPDATE SET
                workspace_id = excluded.workspace_id,
                parent_document_id = excluded.parent_document_id,
                revision = excluded.revision",
            params![
                applied.document.to_storage(),
                workspace,
                parent,
                applied.revision
            ],
        )?;
        tx.commit()?;
        changed.sort();
        changed.dedup();
        Ok(Changed {
            workspace: applied.workspace,
            parent: applied.parent,
            hashes: changed,
        })
    }

    /// A reading document left the listing — its parent was purged, or it
    /// moved to another workspace. Its rows go, and so do the words of each
    /// hash **unless another parent in the workspace still holds it**.
    /// `None` when the mirror never applied it.
    pub fn remove_document(&mut self, document: DocumentId) -> Result<Option<Changed>> {
        let tx = self.db.transaction()?;
        let Some((workspace, parent)): Option<(String, String)> = tx
            .query_row(
                "SELECT workspace_id, parent_document_id FROM reading_sources
                  WHERE document_id = ?1",
                params![document.to_storage()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
        else {
            return Ok(None);
        };
        let hashes: Vec<String> = {
            let mut q = tx.prepare(
                "SELECT hash FROM readings
                  WHERE parent_document_id = ?1 AND workspace_id = ?2 ORDER BY hash",
            )?;
            let rows = q.query_map(params![parent, workspace], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        tx.execute(
            "DELETE FROM readings WHERE parent_document_id = ?1 AND workspace_id = ?2",
            params![parent, workspace],
        )?;
        for hash in &hashes {
            forget_words_if_unheld(&tx, &workspace, hash)?;
        }
        tx.execute(
            "DELETE FROM reading_sources WHERE document_id = ?1",
            params![document.to_storage()],
        )?;
        tx.commit()?;
        let (Some(workspace), Some(parent)) =
            (WorkspaceId::parse(&workspace), DocumentId::parse(&parent))
        else {
            return Ok(None);
        };
        Ok(Some(Changed {
            workspace,
            parent,
            hashes,
        }))
    }

    /// What the mirror has applied in one workspace: each reading document's
    /// revision and parent.
    pub fn sources_in(
        &self,
        workspace: WorkspaceId,
    ) -> Result<HashMap<DocumentId, (DocumentId, Vec<u8>)>> {
        let mut q = self.db.prepare(
            "SELECT document_id, parent_document_id, revision FROM reading_sources
              WHERE workspace_id = ?1",
        )?;
        let rows = q.query_map(params![workspace.to_storage()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (document, parent, revision) = row?;
            if let (Some(document), Some(parent)) =
                (DocumentId::parse(&document), DocumentId::parse(&parent))
            {
                out.insert(document, (parent, revision));
            }
        }
        Ok(out)
    }

    /// The parent and workspace the mirror recorded for one reading document.
    pub fn source_of(&self, document: DocumentId) -> Result<Option<(WorkspaceId, DocumentId)>> {
        let found: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT workspace_id, parent_document_id FROM reading_sources
                  WHERE document_id = ?1",
                params![document.to_storage()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(found.and_then(|(workspace, parent)| {
            Some((WorkspaceId::parse(&workspace)?, DocumentId::parse(&parent)?))
        }))
    }

    /// Every workspace the mirror holds a reading document of.
    pub fn source_workspaces(&self) -> Result<Vec<WorkspaceId>> {
        let mut q = self
            .db
            .prepare("SELECT DISTINCT workspace_id FROM reading_sources ORDER BY workspace_id")?;
        let rows = q.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            if let Some(workspace) = WorkspaceId::parse(&row?) {
                out.push(workspace);
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------- the coordinator

    /// Every `(parent, hash)` in a workspace whose entry this build has nothing
    /// left to do with ([`Reading::is_settled_for_this_build`]).
    pub fn settled_in(&self, workspace: WorkspaceId) -> Result<HashSet<(DocumentId, String)>> {
        let mut q = self.db.prepare(
            "SELECT parent_document_id, hash, generation, status, reason
               FROM readings WHERE workspace_id = ?1",
        )?;
        let rows = q.query_map(params![workspace.to_storage()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut out = HashSet::new();
        for row in rows {
            let (parent, hash, generation, status, reason) = row?;
            if settled(generation, status.as_deref(), reason.as_deref())
                && let Some(parent) = DocumentId::parse(&parent)
            {
                out.insert((parent, hash));
            }
        }
        Ok(out)
    }

    /// **Words for these bytes that already exist on this store root** — a
    /// settled entry for `hash` in any workspace, under any parent but
    /// `except`. What the coordinator copies instead of reading again
    /// (TECHNICAL §9). The best one: highest generation, then best status.
    pub fn settled_anywhere(
        &self,
        hash: &str,
        except: DocumentId,
    ) -> Result<Option<(WorkspaceId, DocumentId)>> {
        let mut q = self.db.prepare(
            "SELECT workspace_id, parent_document_id, generation, status, reason, page_count
               FROM readings WHERE hash = ?1 AND parent_document_id <> ?2",
        )?;
        let rows = q.query_map(params![hash, except.to_storage()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                Row {
                    generation: r.get(2)?,
                    status: r.get(3)?,
                    reason: r.get(4)?,
                    source: None,
                    page_count: r.get(5)?,
                },
            ))
        })?;
        let mut best: Option<((i64, i64, i64), String, String)> = None;
        for row in rows {
            let (workspace, parent, row) = row?;
            if !settled(row.generation, row.status.as_deref(), row.reason.as_deref()) {
                continue;
            }
            let rank = row.rank();
            if best.as_ref().is_none_or(|(b, ..)| rank > *b) {
                best = Some((rank, workspace, parent));
            }
        }
        Ok(best.and_then(|(_, workspace, parent)| {
            Some((WorkspaceId::parse(&workspace)?, DocumentId::parse(&parent)?))
        }))
    }

    /// Every local attempt at this generation for the `text` reader, by hash.
    pub fn attempts(&self, reader: &str) -> Result<HashMap<String, Attempt>> {
        let mut q = self.db.prepare(
            "SELECT hash, code, attempts, last_at FROM read_attempts
              WHERE reader = ?1 AND generation = ?2",
        )?;
        let rows = q.query_map(params![reader, i64::from(GENERATION)], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (hash, code, attempts, last_at) = row?;
            if let Some(code) = AttemptCode::parse(&code) {
                out.insert(
                    hash,
                    Attempt {
                        code,
                        attempts: u32::try_from(attempts).unwrap_or(u32::MAX),
                        last_at,
                    },
                );
            }
        }
        Ok(out)
    }

    /// Record one more local attempt at `hash`. `attempts` counts up while
    /// the code stays the same and starts again at one when it changes.
    pub fn record_attempt(
        &mut self,
        hash: &str,
        reader: &str,
        code: AttemptCode,
        at: i64,
    ) -> Result<Attempt> {
        self.db.execute(
            "INSERT INTO read_attempts (hash, reader, generation, code, attempts, last_at)
             VALUES (?1, ?2, ?3, ?4, 1, ?5)
             ON CONFLICT (hash, reader, generation) DO UPDATE SET
                attempts = CASE WHEN read_attempts.code = excluded.code
                                THEN read_attempts.attempts + 1 ELSE 1 END,
                code = excluded.code,
                last_at = excluded.last_at",
            params![hash, reader, i64::from(GENERATION), code.as_str(), at],
        )?;
        Ok(self
            .db
            .query_row(
                "SELECT attempts FROM read_attempts
                  WHERE hash = ?1 AND reader = ?2 AND generation = ?3",
                params![hash, reader, i64::from(GENERATION)],
                |r| r.get::<_, i64>(0),
            )
            .map(|attempts| Attempt {
                code,
                attempts: u32::try_from(attempts).unwrap_or(u32::MAX),
                last_at: at,
            })?)
    }

    /// A reading finished: whatever was recorded against these bytes no
    /// longer holds them back.
    pub fn clear_attempts(&mut self, hash: &str) -> Result<()> {
        self.db
            .execute("DELETE FROM read_attempts WHERE hash = ?1", params![hash])?;
        Ok(())
    }

    // ------------------------------------------------------------ the index

    /// The winning reading of each of `hashes` in `workspace`, flattened for
    /// the index ([`koru_search::Readings`]). A hash with no words is absent.
    ///
    /// `page_count` is the largest any parent's entry for the hash records —
    /// the larger of its `pages_done` and its highest page — so a hit is
    /// numbered by page only for a file of more than one.
    pub fn readings_for(
        &self,
        workspace: WorkspaceId,
        hashes: &[String],
    ) -> Result<Vec<koru_search::ReadingText>> {
        let workspace = workspace.to_storage();
        let mut found: BTreeMap<String, koru_search::ReadingText> = BTreeMap::new();
        for chunk in hashes.chunks(CHUNK) {
            let mut pages = self.db.prepare(&reading_pages_sql(chunk.len()))?;
            let parameters =
                std::iter::once(workspace.as_str()).chain(chunk.iter().map(String::as_str));
            let rows = pages.query_map(params_from_iter(parameters), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (hash, page, text) = row?;
                let Ok(page) = u32::try_from(page) else {
                    continue;
                };
                found
                    .entry(hash.clone())
                    .or_insert_with(|| koru_search::ReadingText {
                        hash,
                        page_count: 0,
                        pages: Vec::new(),
                    })
                    .pages
                    .push((page, text));
            }
            let mut counts = self.db.prepare(&page_count_sql(chunk.len()))?;
            let parameters =
                std::iter::once(workspace.as_str()).chain(chunk.iter().map(String::as_str));
            let rows = counts.query_map(params_from_iter(parameters), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (hash, count) = row?;
                if let Some(reading) = found.get_mut(&hash) {
                    reading.page_count = u32::try_from(count).unwrap_or(0);
                }
            }
        }
        Ok(found
            .into_values()
            .map(|mut reading| {
                let highest = reading.pages.iter().map(|(p, _)| *p).max().unwrap_or(0);
                reading.page_count = reading.page_count.max(highest);
                reading
            })
            .collect())
    }

    // ------------------------------------------------------------ diagnosis

    pub fn counts(&self) -> Result<MirrorCounts> {
        let mut counts = MirrorCounts::default();
        {
            let mut q = self
                .db
                .prepare("SELECT status, reason, COUNT(*) FROM readings GROUP BY status, reason")?;
            let rows = q.query_map([], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (status, reason, n) = row?;
                let n = n.max(0) as u64;
                *counts
                    .by_status
                    .entry(status.clone().unwrap_or_else(|| "unknown".into()))
                    .or_default() += n;
                match (status.as_deref(), reason) {
                    (Some("failed"), Some(reason)) => {
                        *counts.failed_by_reason.entry(reason).or_default() += n;
                    }
                    (Some("partial"), Some(_)) => counts.page_capped += n,
                    _ => {}
                }
            }
        }
        {
            let mut q = self
                .db
                .prepare("SELECT code, COUNT(*) FROM read_attempts GROUP BY code")?;
            let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            for row in rows {
                let (code, n) = row?;
                counts.attempts_by_code.insert(code, n.max(0) as u64);
            }
        }
        counts.documents = self
            .db
            .query_row("SELECT COUNT(*) FROM reading_sources", [], |r| {
                r.get::<_, i64>(0)
            })?
            .max(0) as u64;
        counts.pages = self
            .db
            .query_row("SELECT COUNT(*) FROM reading_pages", [], |r| {
                r.get::<_, i64>(0)
            })?
            .max(0) as u64;
        Ok(counts)
    }

    /// Every `readings` row for one hash, across workspaces, for the
    /// subcommand that asked about it — or every row, with no hash.
    pub fn rows_for(&self, hash: Option<&str>) -> Result<Vec<ReadingRow>> {
        let mut q = self.db.prepare(
            "SELECT workspace_id, parent_document_id, hash, generation, status, reason, source,
                    page_count
               FROM readings WHERE ?1 IS NULL OR hash = ?1
              ORDER BY hash, workspace_id, parent_document_id",
        )?;
        let rows = q.query_map(params![hash], |r| {
            Ok(ReadingRow {
                workspace: r.get(0)?,
                parent: r.get(1)?,
                hash: r.get(2)?,
                generation: r.get(3)?,
                status: r.get(4)?,
                reason: r.get(5)?,
                source: r.get(6)?,
                page_count: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// One workspace's words for one hash, page by page — the subcommand's
    /// answer when a person asked for that hash's text, which is the point of
    /// asking.
    pub fn pages_of(&self, workspace: &str, hash: &str) -> Result<Vec<(i64, String)>> {
        let mut q = self.db.prepare(
            "SELECT page, text FROM reading_pages
              WHERE workspace_id = ?1 AND hash = ?2 ORDER BY page",
        )?;
        let rows = q.query_map(params![workspace, hash], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    #[cfg(test)]
    pub(crate) fn connection(&self) -> &Connection {
        &self.db
    }
}

/// One `readings` row's values, compared to tell whether an application
/// moved anything.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    generation: i64,
    status: Option<String>,
    reason: Option<String>,
    source: Option<String>,
    page_count: i64,
}

impl Row {
    fn of(reading: &Reading) -> Row {
        let highest = reading.pages.keys().copied().max().unwrap_or(0);
        Row {
            generation: i64::from(reading.generation),
            status: reading.status.map(|s| s.as_str().to_owned()),
            reason: reading.reason.map(|r| r.as_str().to_owned()),
            source: reading.source.map(|s| s.as_str().to_owned()),
            page_count: i64::from(highest.max(reading.pages_done.unwrap_or(0))),
        }
    }

    /// Which of two readings of the same bytes wins: the higher generation,
    /// then the better status, then the one that reached more pages.
    fn rank(&self) -> (i64, i64, i64) {
        (
            self.generation,
            status_rank(self.status.as_deref(), self.reason.as_deref()),
            self.page_count,
        )
    }
}

/// `read` over a capped `partial` over a still-going `partial` over `empty`
/// over `failed` over a status this build cannot name.
fn status_rank(status: Option<&str>, reason: Option<&str>) -> i64 {
    match (status, reason) {
        (Some("read"), _) => 5,
        (Some("partial"), Some(_)) => 4,
        (Some("partial"), None) => 3,
        (Some("empty"), _) => 2,
        (Some("failed"), _) => 1,
        _ => 0,
    }
}

/// [`Reading::is_settled_for_this_build`], over stored tokens.
pub fn settled(generation: i64, status: Option<&str>, reason: Option<&str>) -> bool {
    let current = i64::from(GENERATION);
    if generation > current {
        return true;
    }
    if generation < current {
        return false;
    }
    match status.and_then(ReadingStatus::parse) {
        Some(status) => status.is_finished(reason.and_then(Reason::parse)),
        None => false,
    }
}

/// Replace the workspace's words for `hash` with `reading`'s. Returns whether
/// they differ from what was held.
fn replace_pages(
    tx: &rusqlite::Transaction<'_>,
    workspace: &str,
    hash: &str,
    reading: &Reading,
) -> rusqlite::Result<bool> {
    let held: BTreeMap<i64, String> = {
        let mut q = tx.prepare(
            "SELECT page, text FROM reading_pages WHERE workspace_id = ?1 AND hash = ?2",
        )?;
        let rows = q.query_map(params![workspace, hash], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let fresh: BTreeMap<i64, String> = reading
        .pages
        .iter()
        .filter(|(_, text)| !text.is_empty())
        .map(|(page, text)| (i64::from(*page), text.as_str().to_owned()))
        .collect();
    if held == fresh {
        return Ok(false);
    }
    tx.execute(
        "DELETE FROM reading_pages WHERE workspace_id = ?1 AND hash = ?2",
        params![workspace, hash],
    )?;
    let mut insert = tx.prepare(
        "INSERT INTO reading_pages (workspace_id, hash, page, text) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (page, text) in &fresh {
        insert.execute(params![workspace, hash, page, text])?;
    }
    Ok(true)
}

/// Drop a workspace's words for `hash` when no parent in it holds the hash
/// any more.
fn forget_words_if_unheld(
    tx: &rusqlite::Transaction<'_>,
    workspace: &str,
    hash: &str,
) -> rusqlite::Result<()> {
    let held: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM readings WHERE workspace_id = ?1 AND hash = ?2)",
        params![workspace, hash],
        |r| r.get(0),
    )?;
    if !held {
        tx.execute(
            "DELETE FROM reading_pages WHERE workspace_id = ?1 AND hash = ?2",
            params![workspace, hash],
        )?;
    }
    Ok(())
}

/// The index's read of the words: one workspace, a chunk of hashes. One
/// function so the test that pins its plan reads the statement that runs.
pub(crate) fn reading_pages_sql(hashes: usize) -> String {
    let marks = vec!["?"; hashes.max(1)].join(", ");
    format!(
        "SELECT hash, page, text FROM reading_pages
          WHERE workspace_id = ? AND hash IN ({marks})
          ORDER BY hash, page"
    )
}

/// The index's read of the page counts, beside [`reading_pages_sql`].
pub(crate) fn page_count_sql(hashes: usize) -> String {
    let marks = vec!["?"; hashes.max(1)].join(", ");
    format!(
        "SELECT hash, MAX(page_count) FROM readings
          WHERE workspace_id = ? AND hash IN ({marks})
          GROUP BY hash"
    )
}

/// Every closed vocabulary is `CHECK`ed (PRINCIPLES, "Prefer SQL types over
/// encoded strings"); a token a newer build wrote that this one cannot name is
/// stored as `NULL`, as the reading types read it.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS readings (
    workspace_id       TEXT NOT NULL,
    parent_document_id TEXT NOT NULL,
    hash               TEXT NOT NULL,
    generation         INTEGER NOT NULL CHECK (generation >= 0),
    status             TEXT CHECK (status IN ('read', 'empty', 'partial', 'failed')),
    reason             TEXT CHECK (reason IN ('password', 'damaged', 'page-cap')),
    source             TEXT CHECK (source IN ('text-layer', 'recognized', 'mixed', 'plain')),
    -- The larger of `pages_done` and the highest page key.
    page_count         INTEGER NOT NULL CHECK (page_count >= 0),
    PRIMARY KEY (parent_document_id, hash)
) WITHOUT ROWID;
-- The index's question (a workspace and its hashes) and the purge's (does
-- another parent here still hold this hash).
CREATE INDEX IF NOT EXISTS readings_workspace_hash ON readings(workspace_id, hash);
-- The copy's question: does this store root hold words for these bytes at all.
CREATE INDEX IF NOT EXISTS readings_hash ON readings(hash);

CREATE TABLE IF NOT EXISTS reading_pages (
    workspace_id TEXT NOT NULL,
    hash         TEXT NOT NULL,
    page         INTEGER NOT NULL CHECK (page >= 1),
    text         TEXT NOT NULL,
    PRIMARY KEY (workspace_id, hash, page)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS reading_sources (
    document_id        TEXT PRIMARY KEY,
    workspace_id       TEXT NOT NULL,
    parent_document_id TEXT NOT NULL,
    revision           BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS reading_sources_workspace ON reading_sources(workspace_id);

CREATE TABLE IF NOT EXISTS read_attempts (
    hash       TEXT NOT NULL,
    reader     TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation >= 0),
    code       TEXT NOT NULL CHECK (code IN ('unsupported', 'unreadable', 'internal',
                                             'needs-reader', 'crashed', 'memory-limit',
                                             'hostile')),
    attempts   INTEGER NOT NULL CHECK (attempts >= 1),
    -- Epoch milliseconds UTC, for backoff only; nothing displays it.
    last_at    INTEGER NOT NULL,
    PRIMARY KEY (hash, reader, generation)
) WITHOUT ROWID;
";

/// Every token the schema's `CHECK`s admit, for the test that holds them to
/// `koru-read`'s own vocabularies.
#[cfg(test)]
pub(crate) fn checked_tokens() -> (Vec<&'static str>, Vec<&'static str>, Vec<&'static str>) {
    (
        ReadingStatus::ALL.iter().map(|s| s.as_str()).collect(),
        Reason::ALL.iter().map(|r| r.as_str()).collect(),
        koru_read::Source::ALL.iter().map(|s| s.as_str()).collect(),
    )
}

#[cfg(test)]
mod tests;
