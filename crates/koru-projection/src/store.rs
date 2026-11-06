// SPDX-License-Identifier: MPL-2.0

//! `projection.db` — the read model on disk.
//!
//! At the **store root**, beside `catalog.db`, never inside
//! `workspaces/<id>/`: it is derived, disposable and cross-workspace, and a
//! per-workspace file would be carried by `Store::export_workspace`, shipping a
//! rebuildable cache inside somebody's bundle.
//!
//! ## Rows per node, not a blob per document
//!
//! A serialised document per row would make replacement one `UPDATE` and
//! foreclose every cross-document question — a view asking *every task with a
//! due date in this workspace* would have to deserialise every document to
//! answer. Rows keep that open at no real cost: replacement is a delete and an
//! insert inside one transaction, still work proportional to the changed
//! document.
//!
//! Nothing here exposes a cross-document query yet. It is simply not foreclosed.
//!
//! ## No migration engine, deliberately
//!
//! `koru-store-sqlite` has one because its data is authoritative. This data is
//! not: if the stored [`PROJECTION_SCHEMA_VERSION`] differs, the right answer is
//! to drop everything and walk the documents again. That is simpler than a
//! migration *and* strictly safer — and it is why the `trashed_by` mistake (a
//! column appended to a migration that had already shipped, and therefore
//! reaching nobody) cannot happen here.
//!
//! ## Two notions of trash, and which query answers which
//!
//! They are different things and this file holds both, so the distinction is
//! written down once here rather than guessed at each call site.
//!
//! - **A binned *document*** is a stamp on the workspace manifest's entry,
//!   inherited down the document tree ([ADR-95]). It lives in `listing`
//!   (`trashed_at`, `trashed_by`), and [`TRASHED_CTE`] is how a query asks for
//!   it. The rule is **flagged, never hidden**: `attribute_rows` returns those
//!   rows with `trashed` set, and `attribute_groups` counts them, because a
//!   list and the group header above it must agree.
//! - **A binned *node*** is a move into the document's own hidden
//!   `Kind::Trash` root ([ADR-69]). It lives in `nodes.in_trash`, written by
//!   the walk. The rule is **stored, and left out of the answer unless asked
//!   for**: every query here that means *content a person can see* filters
//!   `in_trash = 0`, and the doors that mean otherwise say so
//!   ([`read_with_trash`](Projections::read_with_trash), and
//!   [`live_file_blob_hashes`](Projections::live_file_blob_hashes), which must
//!   never let a restorable node's bytes be swept).
//!
//! The asymmetry is not an accident. A binned document keeps a listing row, so
//! every consumer can see the flag and decide; a binned node has no such
//! channel — the shipped consumers of this database have no node-level trash
//! vocabulary at all — so handing them the rows unasked would put binned
//! content into ordinary search results with nothing to mark it.
//!
//! [ADR-69]: ../../../docs/decisions/ADR-051-075.md#adr-69--trashing-a-document-is-a-move-into-a-hidden-library-node-and-nothing-ever-purges-it
//! [ADR-95]: ../../../docs/decisions/ADR-076-100.md#adr-95-trash--a-document-in-the-trash-is-a-flag-on-its-manifest-entry

use std::collections::{HashMap, HashSet};
use std::path::Path;

use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_core::personal_attributes::EntityRef;
use rusqlite::{Connection, OptionalExtension, params};

use crate::model::{
    DocumentProjection, ListedDocument, PROJECTION_SCHEMA_VERSION, ProjectedAttr, ProjectedHeading,
    ProjectedNode, ProjectedRun, ProjectionSourceStatus, UnfiledDocument, WorkspaceListing,
};

/// One node's blob reference, with the facts about its document that decide
/// whether it is live.
///
/// `document` is the storage-form id as a string: this row is grouped and
/// counted, never opened, and parsing every id to throw it away is work the
/// caller would pay per node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobReferenceRow {
    pub document: String,
    /// The ADR-95 stamp on this document's own manifest entry. **Not
    /// consulted by the walk** — a reference in the Trash still counts — and
    /// carried anyway, because a status surface that can say "three of these
    /// are in the Trash" is the difference between a number and an
    /// explanation.
    pub trashed: bool,
    /// The canonical `b3:<hex>` address, as the node stores it.
    pub blob: String,
    pub mime: Option<String>,
}

/// One document as a blob-reference **sample** read it: its identity, the exact
/// state the read model projected from, whether it is binned, and every
/// reference it holds.
///
/// `state` is `None` for a document the listing names and nothing has projected
/// yet — a real state and not zero. A caller that treated it as "no references"
/// would report a Workspace complete while one of its documents had never been
/// read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampledDocumentRow {
    /// The storage-form id as a string, like [`BlobReferenceRow::document`]:
    /// grouped and compared, never opened.
    pub document: String,
    /// Encoded version vector. Opaque here; the comparison belongs to whoever
    /// holds both sides of it.
    pub state: Option<Vec<u8>>,
    pub trashed: bool,
    /// `(address, media type)` per referencing node, in the order the
    /// reference query returned them.
    pub references: Vec<(String, Option<String>)>,
}

/// The whole migration: drop every table, from any stored version to this one.
///
/// **Version-agnostic on purpose**, which is why a schema bump does not add a
/// step here — the file's own header says so. What the bump changes is which
/// file this points at: the `6 -> 7` rebuild (`nodes.in_trash`) ran for a while
/// under its predecessor's `5 -> 6` title, because a rebuild does not care
/// where it started, and a log line naming the wrong bump is the one cost of
/// that. So the file is named for the bump it serves, and `7 -> 8`
/// (`attrs_probe`) repointed it rather than chaining onto it.
const PROJECTION_SCHEMA_MIGRATION: &str =
    include_str!("../migrations/015_projection_attrs_probe_rebuild.sql");

/// The migration's file name, for the two log lines that name it. One constant
/// so repointing [`PROJECTION_SCHEMA_MIGRATION`] cannot leave a log claiming
/// the previous file ran.
const PROJECTION_SCHEMA_MIGRATION_NAME: &str = "015_projection_attrs_probe_rebuild.sql";

/// Why a projection operation did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionError(pub String);

impl std::fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<rusqlite::Error> for ProjectionError {
    fn from(e: rusqlite::Error) -> Self {
        Self(e.to_string())
    }
}

type Result<T> = std::result::Result<T, ProjectionError>;

/// Whether a document's projection is up to date, and what to do if not.
///
/// **Not an ordering.** Two version vectors can be incomparable — two replicas
/// that have each seen something the other has not — and a *recovered* document's
/// can go backwards, because `Opened::Recovered` means the store fell back to a
/// checkpoint. A rule of the form *only a newer revision may replace an older
/// one* would leave that document stale forever with nothing to notice.
///
/// So the rule is: **equal is current, and everything else is a re-projection.**
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Staleness {
    /// Never projected.
    Missing,
    /// Projected from exactly this revision.
    Current,
    /// Projected from some other revision — older, newer or incomparable.
    Stale,
}

/// The read model's storage.
pub struct Projections {
    db: Connection,
}

impl Projections {
    /// Open (or create) `projection.db` under a store root.
    ///
    /// **A schema that does not match is not migrated, it is discarded.** The
    /// caller then rebuilds; see the module note.
    pub fn open(store_root: &Path) -> Result<Self> {
        Self::at(&store_root.join("projection.db"))
    }

    pub fn at(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ProjectionError(e.to_string()))?;
        }
        let db = Connection::open(path)?;
        Self::over(db)
    }

    /// In memory, for tests.
    pub fn in_memory() -> Result<Self> {
        Self::over(Connection::open_in_memory()?)
    }

    fn over(db: Connection) -> Result<Self> {
        // WAL so a reader is never blocked by the writer, and `NORMAL` rather
        // than `FULL`: this is a rebuildable cache, and paying an fsync per
        // commit to protect something we can regenerate would be paying for
        // durability we do not need. The store's own database is where `FULL`
        // belongs and is where it is.
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;

        let found: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let listing_has_mime = found == 0
            || db.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM pragma_table_info('listing') WHERE name = 'mime'
                 )",
                [],
                |row| row.get::<_, bool>(0),
            )?;
        if found != 0 && (found != PROJECTION_SCHEMA_VERSION || !listing_has_mime) {
            let started = std::time::Instant::now();
            tracing::info!(
                database = "projection.db",
                found,
                expected = PROJECTION_SCHEMA_VERSION,
                listing_has_mime,
                migration = PROJECTION_SCHEMA_MIGRATION_NAME,
                "applying SQL migration; the derived projection will be rebuilt"
            );
            db.execute_batch(PROJECTION_SCHEMA_MIGRATION)?;
            tracing::info!(
                database = "projection.db",
                from = found,
                to = PROJECTION_SCHEMA_VERSION,
                migration = PROJECTION_SCHEMA_MIGRATION_NAME,
                elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                "SQL migration applied"
            );
        }
        db.execute_batch(SCHEMA)?;
        db.pragma_update(None, "user_version", PROJECTION_SCHEMA_VERSION)?;
        Ok(Self { db })
    }

    // ------------------------------------------------------------- revisions

    /// What every projected document was built from.
    ///
    /// **The whole of how a consumer reconciles.** It compares this against its
    /// own record and re-reads what differs — which is what makes a missed
    /// event a delay rather than a loss.
    pub fn revisions(&self) -> Result<HashMap<DocumentId, Vec<u8>>> {
        let mut q = self.db.prepare(
            "SELECT d.document_id, d.source_revision FROM documents d
                 WHERE NOT EXISTS (
                   SELECT 1 FROM source_status s WHERE s.document_id = d.document_id
                 )",
        )?;
        let rows = q.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (id, revision) = row?;
            if let Some(id) = DocumentId::parse(&id) {
                out.insert(id, revision);
            }
        }
        Ok(out)
    }

    /// Every document any workspace listing names.
    ///
    /// **A document the listing does not name cannot be indexed**, and the
    /// consumer needs to know that *before* it queues the work rather than
    /// after: `koru-search`'s indexer discovered it at the point of indexing,
    /// returned without recording anything, and was handed the same document
    /// again by its next reconcile - a loop that never converged, wrote the
    /// workspace listing again on every attempt, and logged nothing because the
    /// give-up path was not a failure.
    pub fn listed_documents(&self) -> Result<HashSet<DocumentId>> {
        let mut q = self.db.prepare("SELECT document_id FROM listing")?;
        let rows = q.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = HashSet::new();
        for row in rows {
            if let Some(id) = DocumentId::parse(&row?) {
                out.insert(id);
            }
        }
        Ok(out)
    }

    /// The manifest kind the mirrored listing holds for one document, or
    /// `None` when no listing names it yet — a document whose manifest commit
    /// has not been mirrored, which the projector treats as an ordinary page.
    /// One primary-key read; asked once per commit before a fork.
    pub fn listed_kind(&self, document: DocumentId) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT kind FROM listing WHERE document_id = ?1",
                params![document.to_storage()],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    /// The documents of one workspace whose listed kind is one of `kinds` —
    /// what a sweep leaves alone. One query for the workspace.
    pub fn listed_of_kinds(
        &self,
        workspace: WorkspaceId,
        kinds: &[&str],
    ) -> Result<HashSet<DocumentId>> {
        let mut q = self
            .db
            .prepare("SELECT document_id, kind FROM listing WHERE workspace_id = ?1")?;
        let rows = q.query_map(params![workspace.to_storage()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = HashSet::new();
        for row in rows {
            let (document, kind) = row?;
            if kinds.contains(&kind.as_str())
                && let Some(id) = DocumentId::parse(&document)
            {
                out.insert(id);
            }
        }
        Ok(out)
    }

    /// Every document the read model holds that no listing names.
    ///
    /// **The report behind a review, not a repair.** It answers *what is
    /// unreachable and what is in it*, with enough of each document to judge:
    /// the title from its own first row, a preview, how big it is, and whether
    /// a listed document in the same workspace shares its title. It changes
    /// nothing — deciding is the caller's, and there is deliberately no purge
    /// anywhere near this.
    ///
    /// **Read from the projection rather than the manifest**, because the
    /// content a reviewer has to see is only here. The cost is that a document
    /// which has not been projected yet is not reported; the projector is what
    /// closes that gap, and a document with no projection has no preview to
    /// show either, so there would be nothing to review.
    pub fn unfiled_documents(&self) -> Result<Vec<UnfiledDocument>> {
        // Titles the listing *does* carry, per workspace, for the duplicate
        // flag. One query rather than one per candidate.
        let mut listed_titles: HashMap<String, HashSet<String>> = HashMap::new();
        {
            let mut q = self.db.prepare("SELECT workspace_id, title FROM listing")?;
            let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (workspace, title) = row?;
                listed_titles.entry(workspace).or_default().insert(title);
            }
        }

        let mut candidates = Vec::new();
        {
            let mut q = self.db.prepare(
                "SELECT d.document_id, d.workspace_id, COALESCE(w.name, ''), d.projected_at
                   FROM documents d
                   LEFT JOIN workspaces w ON w.workspace_id = d.workspace_id
                  WHERE NOT EXISTS (
                        SELECT 1 FROM listing l WHERE l.document_id = d.document_id
                  )
                  ORDER BY d.projected_at DESC",
            )?;
            let rows = q.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                candidates.push(row?);
            }
        }

        let mut out = Vec::with_capacity(candidates.len());
        for (document, workspace, workspace_name, projected_at) in candidates {
            let (Some(document_id), Some(workspace_id)) =
                (DocumentId::parse(&document), WorkspaceId::parse(&workspace))
            else {
                continue;
            };
            // Live nodes only: this is a report somebody reads to decide what a
            // document *is*, and counting its bin into "how big is it" or
            // previewing a line out of it would be answering with content the
            // author had already thrown away.
            let nodes: usize = self.db.query_row(
                &format!(
                    "SELECT COUNT(*) FROM nodes WHERE document_id = ?1 {bin}",
                    bin = NodeScope::Live.clause("in_trash"),
                ),
                params![document],
                |r| r.get::<_, i64>(0),
            )? as usize;
            let mut q = self.db.prepare(&format!(
                "SELECT text FROM nodes WHERE document_id = ?1 {bin} ORDER BY seq LIMIT 4",
                bin = NodeScope::Live.clause("in_trash"),
            ))?;
            let texts: Vec<String> = q
                .query_map(params![document], |r| {
                    Ok(r.get::<_, Option<String>>(0)?.unwrap_or_default())
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let title = texts.first().cloned().unwrap_or_default().trim().to_owned();
            let preview = texts
                .iter()
                .skip(1)
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join(" / ");
            let same_title_as_listed = !title.is_empty()
                && listed_titles
                    .get(&workspace)
                    .is_some_and(|titles| titles.contains(&title));
            out.push(UnfiledDocument {
                workspace: workspace_id,
                workspace_name,
                document: document_id,
                title,
                preview,
                nodes,
                projected_at,
                same_title_as_listed,
            });
        }
        Ok(out)
    }

    /// Every document projected for one workspace.
    ///
    /// A sweep is per workspace, because the store opens per workspace — so
    /// asking about all of them would open all of them.
    pub fn revisions_in(&self, workspace: WorkspaceId) -> Result<Vec<DocumentId>> {
        let mut q = self.db.prepare(
            "SELECT document_id FROM documents WHERE workspace_id = ?1
             UNION
             SELECT document_id FROM source_status WHERE workspace_id = ?1",
        )?;
        let rows = q.query_map(params![workspace.to_storage()], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            if let Some(id) = DocumentId::parse(&row?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    pub fn revision(&self, document: DocumentId) -> Result<Option<Vec<u8>>> {
        Ok(self
            .db
            .query_row(
                "SELECT source_revision FROM documents WHERE document_id = ?1",
                params![document.to_storage()],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()?)
    }

    /// When this document's projected **content** last moved, in epoch-ms.
    ///
    /// Not `projected_at`: a sweep, a recovery or a rebuild re-projects
    /// documents nobody edited, and this stays where it was for all three — a
    /// rebuild included, because the value comes from the store's own durable
    /// stamp rather than from this database. `None` for a document nothing has
    /// projected.
    pub fn modified_at(&self, document: DocumentId) -> Result<Option<i64>> {
        Ok(self
            .db
            .query_row(
                "SELECT modified_at FROM documents WHERE document_id = ?1",
                params![document.to_storage()],
                |r| r.get::<_, i64>(0),
            )
            .optional()?)
    }

    /// Whether a document projected from `revision` would be doing any work.
    pub fn staleness(&self, document: DocumentId, revision: &[u8]) -> Result<Staleness> {
        if let Some(ProjectionSourceStatus::SchemaTooNew {
            found,
            source_revision,
            ..
        }) = self.source_status(document)?
            && source_revision == revision
            && found > koru_core::schema::SCHEMA_VERSION
        {
            return Ok(Staleness::Current);
        }
        Ok(match self.revision(document)? {
            None => Staleness::Missing,
            Some(have) if have == revision => Staleness::Current,
            Some(_) => Staleness::Stale,
        })
    }

    pub fn source_status(&self, document: DocumentId) -> Result<Option<ProjectionSourceStatus>> {
        Ok(self
            .db
            .query_row(
                "SELECT found, understood, source_revision FROM source_status
                 WHERE document_id = ?1",
                params![document.to_storage()],
                |row| {
                    Ok(ProjectionSourceStatus::SchemaTooNew {
                        found: row.get(0)?,
                        understood: row.get(1)?,
                        source_revision: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn mark_schema_too_new(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        source_revision: &[u8],
        found: i64,
        understood: i64,
        at_millis: i64,
    ) -> Result<()> {
        self.db.execute(
            "INSERT INTO source_status
               (document_id, workspace_id, source_revision, found, understood, checked_at)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(document_id) DO UPDATE SET
               workspace_id = excluded.workspace_id,
               source_revision = excluded.source_revision,
               found = excluded.found,
               understood = excluded.understood,
               checked_at = excluded.checked_at",
            params![
                document.to_storage(),
                workspace.to_storage(),
                source_revision,
                found,
                understood,
                at_millis,
            ],
        )?;
        Ok(())
    }

    pub fn documents(&self) -> Result<usize> {
        Ok(self
            .db
            .query_row("SELECT COUNT(*) FROM documents", [], |r| r.get::<_, i64>(0))?
            as usize)
    }

    /// Join every reminder target in one projection query. Callers request one
    /// sweep per distinct workspace before using this snapshot; `projected`
    /// keeps a cold/stale absence distinguishable from an authoritative
    /// present-to-missing transition.
    pub fn reminder_targets(
        &self,
        targets: &[EntityRef],
    ) -> Result<Vec<crate::reminders::ReminderTargetRow>> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        // Stay below SQLite's conservative 999-variable build limit. This is
        // a bounded batch, never one query per reminder.
        if targets.len() > 250 {
            let mut out = Vec::with_capacity(targets.len());
            for chunk in targets.chunks(250) {
                out.extend(self.reminder_targets(chunk)?);
            }
            return Ok(out);
        }
        let values = (0..targets.len())
            .map(|index| format!("({index}, ?, ?, ?)"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "WITH RECURSIVE
             targets(ord, workspace_id, document_id, node_id) AS (VALUES {values}),
             ancestors(ord, workspace_id, document_id, parent_id, title, depth, visited) AS (
                 SELECT t.ord, t.workspace_id, l.document_id, l.parent_id, l.title, 0,
                        ',' || l.document_id || ','
                   FROM targets t
                   JOIN listing l
                     ON l.workspace_id = t.workspace_id AND l.document_id = t.document_id
                 UNION ALL
                 SELECT a.ord, a.workspace_id, parent.document_id, parent.parent_id,
                        parent.title, a.depth + 1,
                        a.visited || parent.document_id || ','
                   FROM ancestors a
                   JOIN listing parent
                     ON parent.workspace_id = a.workspace_id
                    AND parent.document_id = a.parent_id
                  WHERE instr(a.visited, ',' || parent.document_id || ',') = 0
             ),
             document_paths(ord, path) AS (
                 SELECT ord, group_concat(title, ' / ')
                   FROM (SELECT ord, title FROM ancestors ORDER BY ord, depth DESC)
                  GROUP BY ord
             ),
             parent_paths(ord, path) AS (
                 SELECT ord, group_concat(title, ' / ')
                   FROM (SELECT ord, title FROM ancestors WHERE depth > 0 ORDER BY ord, depth DESC)
                  GROUP BY ord
             )
             SELECT t.ord, l.title, n.text, n.name, l.trashed_at,
                    d.document_id, s.document_id, n.node_id,
                    a.kind, a.value, a.zone, w.name,
                    document_paths.path, parent_paths.path
               FROM targets t
               LEFT JOIN listing l
                 ON l.workspace_id = t.workspace_id AND l.document_id = t.document_id
               LEFT JOIN workspaces w ON w.workspace_id = t.workspace_id
               LEFT JOIN document_paths ON document_paths.ord = t.ord
               LEFT JOIN parent_paths ON parent_paths.ord = t.ord
               LEFT JOIN documents d ON d.document_id = t.document_id
               LEFT JOIN source_status s ON s.document_id = t.document_id
               LEFT JOIN nodes n
                 ON n.document_id = t.document_id AND n.node_id = t.node_id
               LEFT JOIN attrs a ON a.rowid = (
                    SELECT due.rowid FROM attrs due
                     WHERE due.document_id = t.document_id
                       AND due.node_id = t.node_id
                       AND due.role = 'koru.due'
                     ORDER BY due.attr_id LIMIT 1)
              ORDER BY t.ord"
        );
        let mut params = Vec::with_capacity(targets.len() * 3);
        for target in targets {
            params.push(target.workspace().to_storage());
            params.push(target.document().to_storage());
            params.push(target.node().hyphenated().to_string());
        }
        let mut statement = self.db.prepare(&sql)?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                Ok((
                    row.get::<_, usize>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (
            ord,
            document_title,
            node_text,
            node_name,
            trashed_at,
            document,
            stale,
            node,
            kind,
            value,
            zone,
            workspace_name,
            document_path,
            parent_path,
        ) in rows
        {
            let target = targets[ord].clone();
            let projected = document.is_some() && stale.is_none() && document_title.is_some();
            let state = if document_title.is_none() || node.is_none() {
                koru_store_core::reminders::TargetState::Missing
            } else if trashed_at.is_some() {
                koru_store_core::reminders::TargetState::InBin
            } else {
                koru_store_core::reminders::TargetState::Active
            };
            let title = reminder_title(
                node_name.as_deref(),
                node_text.as_deref(),
                document_title.as_deref(),
            );
            let path = if target.kind() == "document" {
                parent_path
            } else {
                document_path
            };
            let source_location = [workspace_name.as_deref(), path.as_deref()]
                .into_iter()
                .flatten()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" / ");
            out.push(crate::reminders::ReminderTargetRow {
                target,
                title,
                source_location,
                state,
                due: crate::reminders::decode_due(kind, value, zone),
                listed: document_title.is_some(),
                projected,
            });
        }
        Ok(out)
    }

    // -------------------------------------------------------------- listing

    /// Replace one workspace's listing, whole, and say whether it moved.
    ///
    /// **Whole is right here and nowhere else in this file.** A listing is one
    /// row per *document*, not per node — a thousand-document workspace is a
    /// thousand short rows — and the store hands it over as a unit, so there is
    /// nothing to be gained by working out which row changed.
    ///
    /// `false` means the digest was unchanged: a manifest commit that moved
    /// nothing a consumer joins against costs nothing downstream.
    pub fn set_listing(
        &mut self,
        workspace: WorkspaceId,
        name: &str,
        documents: &[ListedDocument],
        at_millis: i64,
    ) -> Result<bool> {
        let digest = listing_digest(name, documents);
        if self.listing_digest(workspace)?.as_deref() == Some(digest.as_slice()) {
            return Ok(false);
        }
        let id = workspace.to_storage();
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM listing WHERE workspace_id = ?1", params![id])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO listing (document_id, workspace_id, parent_id, ordinal, title,
                                      kind, mime, trashed_at, trashed_by)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            )?;
            for document in documents {
                stmt.execute(params![
                    document.document.to_storage(),
                    id,
                    document.parent.map(|p| p.to_storage()),
                    document.ordinal as i64,
                    document.title,
                    document.kind,
                    document.mime,
                    document.trashed_at,
                    document.trashed_by,
                ])?;
            }
        }
        tx.execute(
            "INSERT INTO workspaces (workspace_id, name, listing_digest, listed_at)
             VALUES (?1,?2,?3,?4)
             ON CONFLICT(workspace_id) DO UPDATE SET
                 name = excluded.name,
                 listing_digest = excluded.listing_digest,
                 listed_at = excluded.listed_at",
            params![id, name, digest, at_millis],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn listing_digest(&self, workspace: WorkspaceId) -> Result<Option<Vec<u8>>> {
        Ok(self
            .db
            .query_row(
                "SELECT listing_digest FROM workspaces WHERE workspace_id = ?1",
                params![workspace.to_storage()],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()?)
    }

    /// One workspace's listing, as the projection has it.
    pub fn listing(&self, workspace: WorkspaceId) -> Result<Option<WorkspaceListing>> {
        let id = workspace.to_storage();
        let Some((name, digest)) = self
            .db
            .query_row(
                "SELECT name, listing_digest FROM workspaces WHERE workspace_id = ?1",
                params![id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?
        else {
            return Ok(None);
        };
        let mut q = self.db.prepare(
            "SELECT document_id, parent_id, ordinal, title, kind, mime, trashed_at, trashed_by
             FROM listing WHERE workspace_id = ?1 ORDER BY ordinal, document_id",
        )?;
        let documents = q
            .query_map(params![id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter_map(
                |(document, parent, ordinal, title, kind, mime, trashed_at, trashed_by)| {
                    Some(ListedDocument {
                        document: DocumentId::parse(&document)?,
                        parent: parent.as_deref().and_then(DocumentId::parse),
                        ordinal: ordinal.max(0) as u32,
                        title,
                        kind,
                        mime,
                        trashed_at,
                        trashed_by,
                    })
                },
            )
            .collect();
        Ok(Some(WorkspaceListing {
            workspace,
            name,
            digest,
            documents,
        }))
    }

    /// Distinct content-addressed blobs referenced by first-class file entries
    /// that are live in one workspace's listing.
    ///
    /// **Live is inherited through the document tree.** A child does not carry
    /// its own trash stamp when an ancestor is binned, so filtering only
    /// `listing.trashed_at` would count the child's files while the whole
    /// subtree is hidden. Starting at live roots and descending through live
    /// children gives the same answer as the Library projection without
    /// opening a document. `UNION` also makes a corrupt parent cycle terminate
    /// instead of turning a diagnostic read into an unbounded query.
    ///
    /// **`live` is the document, never the node**, and a binned node's blob is
    /// deliberately still counted. This list is what a sweep keeps; dropping a
    /// hash because the node holding it sits in the document's bin would let
    /// the bytes go while the node is still restorable, and the restore would
    /// hand back a broken picture. It is the same exclusion `koru-core`'s
    /// `blob_hashes` refuses to make, for the same reason — and the one query
    /// here where including the bin is the safe direction, because a hash in
    /// this list can only ever prevent a deletion.
    pub fn live_file_blob_hashes(&self, workspace: WorkspaceId) -> Result<Vec<String>> {
        let mut query = self.db.prepare(
            "WITH RECURSIVE live(document_id) AS (
                 SELECT document_id
                   FROM listing
                  WHERE workspace_id = ?1
                    AND parent_id IS NULL
                    AND trashed_at IS NULL
                 UNION
                 SELECT child.document_id
                   FROM listing child
                   JOIN live parent ON child.parent_id = parent.document_id
                  WHERE child.workspace_id = ?1
                    AND child.trashed_at IS NULL
             )
             SELECT DISTINCT nodes.blob
               FROM nodes
              JOIN live ON live.document_id = nodes.document_id
              JOIN listing listed ON listed.document_id = live.document_id
              WHERE nodes.blob IS NOT NULL
                AND listed.workspace_id = ?1
                AND listed.kind = 'file'
              ORDER BY nodes.blob",
        )?;
        Ok(query
            .query_map(params![workspace.to_storage()], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every blob reference a Workspace's **listed** documents hold, with the
    /// two facts about each document the canonical referenced-blob walk needs.
    ///
    /// Deliberately unlike [`live_file_blob_hashes`](Self::live_file_blob_hashes),
    /// and the difference is the whole reason this exists rather than reusing
    /// it. That query answers "what may a workspace manager sweep", so it
    /// starts at live roots and descends through live children, and it asks
    /// only about first-class file entries. A transfer and a bill ask a
    /// different question — *which blobs does this Workspace have* — and the
    /// approved answer (`contracts/vectors/logical-usage-v1`, cases 12 and 13)
    /// counts a reference from the Trash and counts an image pasted into an
    /// ordinary page. Reusing the sweep query would under-count both, and
    /// `12-blob-referenced-only-from-trash.json` says so in as many words:
    /// *"Accounting must not reuse that query."*
    ///
    /// **This returns rows, not an answer.** The rule that turns them into a
    /// set of live hashes lives in exactly one place — `koru-blob-sync`'s
    /// canonical walk — and this read model does not get a second copy of it.
    /// The join to `listing` is what makes `durable` true for every row: a
    /// permanently removed document has no listing entry, so it contributes
    /// nothing without a predicate anybody could forget to write.
    pub fn blob_references(&self, workspace: WorkspaceId) -> Result<Vec<BlobReferenceRow>> {
        let mut query = self.db.prepare(
            "SELECT nodes.document_id,
                    listing.trashed_at IS NOT NULL,
                    nodes.blob,
                    nodes.mime
               FROM nodes
               JOIN listing ON listing.document_id = nodes.document_id
              WHERE listing.workspace_id = ?1
                AND nodes.blob IS NOT NULL
              ORDER BY nodes.document_id, nodes.blob",
        )?;
        Ok(query
            .query_map(params![workspace.to_storage()], |row| {
                Ok(BlobReferenceRow {
                    document: row.get::<_, String>(0)?,
                    trashed: row.get::<_, bool>(1)?,
                    blob: row.get::<_, String>(2)?,
                    mime: row.get::<_, Option<String>>(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// **The blob one file document points at** (D259 §4).
    ///
    /// A workspace *file* is a document whose root node is the blob, so the
    /// address a person's selection means is one indexed row rather than a
    /// snapshot decode — which is what a window would otherwise pay per
    /// click, on the thread that has to stay ready, to say anything at all
    /// about the file somebody just selected.
    ///
    /// Ordered rather than taken at random: a document with more than one
    /// blob reference is not a file, and the *first* one is at least a stable
    /// answer instead of whatever SQLite happened to return this time.
    pub fn file_blob(&self, document: DocumentId) -> Result<Option<(String, Option<String>)>> {
        let mut query = self.db.prepare(
            "SELECT blob, mime
               FROM nodes
              WHERE document_id = ?1
                AND blob IS NOT NULL
              ORDER BY depth, seq
              LIMIT 1",
        )?;
        let mut rows = query.query_map(params![document.to_storage()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        rows.next().transpose().map_err(Into::into)
    }

    /// **One read of a Workspace's blob-reference sample** — every listed
    /// document, the exact state its rows were projected from, and the
    /// references it holds.
    ///
    /// The whole point is the *one*: both halves come out of a single deferred
    /// read transaction on this connection, so under WAL they are one snapshot
    /// and the references a document contributes belong to the state named
    /// beside them. [`blob_references`](Self::blob_references) taken next to a
    /// separate revision read would be two states wearing one answer, and Cloud
    /// coverage would then be a guarantee about a Workspace that never existed.
    ///
    /// **Every listed document appears, whether or not it references a blob.**
    /// A document with no pictures still has a state, and coverage has to be
    /// able to say whether Cloud holds it. A document the listing does not name
    /// has been permanently removed and contributes nothing, which is the same
    /// join that already makes the walk's `durable` true without a predicate
    /// anybody could forget.
    pub fn blob_reference_sample(&self, workspace: WorkspaceId) -> Result<Vec<SampledDocumentRow>> {
        let tx = self.db.unchecked_transaction()?;
        let mut documents = self.db.prepare(
            "SELECT listing.document_id,
                    listing.trashed_at IS NOT NULL,
                    documents.source_revision
               FROM listing
               LEFT JOIN documents ON documents.document_id = listing.document_id
              WHERE listing.workspace_id = ?1
              ORDER BY listing.document_id",
        )?;
        let mut sample: Vec<SampledDocumentRow> = documents
            .query_map(params![workspace.to_storage()], |row| {
                Ok(SampledDocumentRow {
                    document: row.get::<_, String>(0)?,
                    trashed: row.get::<_, bool>(1)?,
                    state: row.get::<_, Option<Vec<u8>>>(2)?,
                    references: Vec::new(),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(documents);
        for row in self.blob_references(workspace)? {
            if let Ok(index) = sample.binary_search_by(|held| held.document.cmp(&row.document)) {
                sample[index].references.push((row.blob, row.mime));
            }
        }
        tx.commit()?;
        Ok(sample)
    }

    /// One document's headings, in reading order.
    ///
    /// What an `@`-picker offering jump targets asks for. It is answered from
    /// the read model rather than the document because a picker must not pay a
    /// Loro decode per keystroke; the cost is that this trails a live edit by
    /// the projector's debounce.
    ///
    /// **A heading is not a `Kind`.** `NodeMeta::heading` is a block property,
    /// so a heading node's `kind` reads `paragraph` and filtering on kind finds
    /// nothing. `heading IS NOT NULL` is the whole predicate.
    ///
    /// An empty heading is kept rather than skipped: it is still a legal jump
    /// target, and a caller that hides it makes a row of the document
    /// unreachable. Naming it is the caller's business.
    /// A binned heading is not a jump target: it is not on the page, so
    /// offering it would send somebody to a row they cannot see.
    pub fn headings(&self, document: DocumentId) -> Result<Vec<ProjectedHeading>> {
        let mut query = self.db.prepare(&format!(
            "SELECT node_id, heading, text, seq
               FROM nodes
              WHERE document_id = ?1
                AND heading IS NOT NULL
                {bin}
              ORDER BY seq",
            bin = NodeScope::Live.clause("in_trash"),
        ))?;
        Ok(query
            .query_map(params![document.to_storage()], |row| {
                let level: i64 = row.get(1)?;
                Ok(ProjectedHeading {
                    node: row.get(0)?,
                    // Clamped here and never in SQL: a row written by a build
                    // that knew a seventh level must come back as one this one
                    // can draw, not as a panic in a `u8` cast.
                    level: level.clamp(1, 6) as u8,
                    text: row.get(2)?,
                    seq: row.get::<_, i64>(3)?.max(0) as usize,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every workspace with a listing, and what it hashes to. What a consumer
    /// reconciles against.
    pub fn listings(&self) -> Result<Vec<(WorkspaceId, Vec<u8>)>> {
        let mut q = self
            .db
            .prepare("SELECT workspace_id, listing_digest FROM workspaces")?;
        let rows = q.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, digest) = row?;
            if let Some(id) = WorkspaceId::parse(&id) {
                out.push((id, digest));
            }
        }
        Ok(out)
    }

    // ----------------------------------------------------------- replacement

    /// Replace one document's projection, whole, in one transaction.
    ///
    /// **Whole means the whole of the *changed* document**, not the workspace.
    /// Sub-document patching is the only optimisation deferred, and it is
    /// deferred until something measures a reason for it.
    ///
    /// `at_millis` is when the projector ran. `modified_millis` is the *store's*
    /// durable stamp for this document ([`Source::modified_at`]) — `None` when
    /// the store could not answer, which falls the stamp back to the projector's
    /// clock. The two are separate arguments because they answer separate
    /// questions and only one of them survives a rebuild of this database.
    ///
    /// [`Source::modified_at`]: crate::worker::Source::modified_at
    pub fn replace(
        &mut self,
        projection: &DocumentProjection,
        at_millis: i64,
        modified_millis: Option<i64>,
    ) -> Result<()> {
        let tx = self.db.transaction()?;
        let document = projection.document.to_storage();
        tx.execute(
            "DELETE FROM source_status WHERE document_id = ?1",
            params![document],
        )?;
        {
            // Children first is not required — no foreign keys are declared, on
            // purpose, because a cascade would make the cost of a delete depend
            // on SQLite's plan rather than on the rows we know we wrote.
            for table in ["attrs", "runs", "nodes"] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE document_id = ?1"),
                    params![document],
                )?;
            }

            let mut node_stmt = tx.prepare(
                "INSERT INTO nodes (document_id, node_id, seq, parent_id, ordinal, depth, kind,
                                    text, heading, list_style, done, level_name, name, icon,
                                    colour, language, blob, mime, embed, in_trash)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
            )?;
            let mut run_stmt = tx.prepare(
                "INSERT INTO runs (document_id, node_id, start, end, bold, italic, underline,
                                   strike, code, highlight, script, pill)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            )?;
            let mut attr_stmt = tx.prepare(
                "INSERT INTO attrs (document_id, node_id, attr_id, role, kind, value, zone, label)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            )?;

            for (seq, node) in projection.nodes.iter().enumerate() {
                node_stmt.execute(params![
                    document,
                    node.id,
                    seq as i64,
                    node.parent,
                    node.ordinal as i64,
                    node.depth as i64,
                    node.kind,
                    node.text,
                    node.heading.map(i64::from),
                    node.list_style,
                    node.done as i64,
                    node.level_name,
                    node.name,
                    node.icon,
                    node.colour,
                    node.language,
                    node.blob,
                    node.mime,
                    node.embed,
                    node.in_trash as i64,
                ])?;
                for run in &node.runs {
                    // **Plain runs are not written.** Unformatted prose is one
                    // plain run per node, so storing them would double the row
                    // count of an ordinary document to record the absence of
                    // formatting. A consumer that finds no run for a range reads
                    // it as unformatted, which it is.
                    if run.is_plain() {
                        continue;
                    }
                    run_stmt.execute(params![
                        document,
                        node.id,
                        run.start as i64,
                        run.end as i64,
                        run.bold as i64,
                        run.italic as i64,
                        run.underline as i64,
                        run.strike as i64,
                        run.code as i64,
                        run.highlight,
                        run.script,
                        run.pill,
                    ])?;
                }
                for attr in &node.attrs {
                    attr_stmt.execute(params![
                        document, node.id, attr.id, attr.role, attr.kind, attr.value, attr.zone,
                        attr.label,
                    ])?;
                }
            }

            // **The digest decides *whether* the stamp moves; the store decides
            // *what* it moves to.** Two rules, and both are needed.
            //
            // The digest is compared against the one already stored and the
            // stamp is kept when they agree — the same rule the search index
            // applies to `observed_modified_at_ms`, and the whole difference
            // between this and `projected_at`. A sweep, a recovery and a
            // `Staleness::Stale` comparison all re-project documents nobody
            // touched; stamping those would collapse a recency order into "when
            // did the projector last run".
            //
            // The *value* is the store's durable per-document stamp, because the
            // digest alone cannot survive this database being rebuilt: a schema
            // bump drops every row, so the first projection afterwards has
            // nothing to compare against, takes the `NULL` branch for *every*
            // document at once and would stamp them all with the moment of the
            // rebuild. Reading it back out of `workspace.db` is what makes a
            // recency order come through a rebuild intact. `None` — a store that
            // cannot answer — falls back to the projector's clock, which is the
            // behaviour this had before.
            //
            // SQLite reads the old row's values on the right of an UPDATE SET,
            // so the digest may be overwritten in the same statement that
            // compares against it. `IS NOT` rather than `<>` because the first
            // projection after a rebuild compares against NULL.
            tx.execute(
                "INSERT INTO documents (document_id, workspace_id, source_revision, projected_at,
                                        content_digest, modified_at)
                 VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(document_id) DO UPDATE SET
                     workspace_id = excluded.workspace_id,
                     source_revision = excluded.source_revision,
                     projected_at = excluded.projected_at,
                     modified_at = CASE
                         WHEN documents.content_digest IS NOT excluded.content_digest
                         THEN excluded.modified_at
                         ELSE documents.modified_at END,
                     content_digest = excluded.content_digest",
                params![
                    document,
                    projection.workspace.to_storage(),
                    projection.source_revision,
                    at_millis,
                    content_digest(&projection.nodes),
                    modified_millis.unwrap_or(at_millis),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Forget a document entirely — it was dropped, not binned.
    ///
    /// **Binning is not this.** A document in the trash is a flag on its
    /// manifest entry and still has content somebody can search and restore
    /// from; removing its projection would make restore hand back something no
    /// consumer had ever heard of.
    pub fn remove(&mut self, document: DocumentId) -> Result<()> {
        let tx = self.db.transaction()?;
        let id = document.to_storage();
        // The listing row goes too. A drop removes the manifest entry as well,
        // so the next refresh would clear it — but "would" is not "has", and a
        // consumer joining against a listing that still names a document
        // nothing holds would draw a result that cannot be opened.
        for table in [
            "attrs",
            "runs",
            "nodes",
            "documents",
            "listing",
            "source_status",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE document_id = ?1"),
                params![id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Forget every derived row for a workspace the authoritative Store no
    /// longer has, returning the documents consumers must retire too.
    ///
    /// This is deliberately distinct from an empty workspace: an empty
    /// workspace still has a listing row and may receive documents later. A
    /// missing workspace must disappear from search facets, or every query
    /// asks for the same impossible sweep and waits for its timeout.
    pub fn remove_workspace(&mut self, workspace: WorkspaceId) -> Result<Vec<DocumentId>> {
        let documents = self.revisions_in(workspace)?;
        let id = workspace.to_storage();
        let tx = self.db.transaction()?;
        for table in ["attrs", "runs", "nodes"] {
            tx.execute(
                &format!(
                    "DELETE FROM {table} WHERE document_id IN
                     (SELECT document_id FROM documents WHERE workspace_id = ?1
                      UNION
                      SELECT document_id FROM source_status WHERE workspace_id = ?1)"
                ),
                params![id],
            )?;
        }
        tx.execute("DELETE FROM documents WHERE workspace_id = ?1", params![id])?;
        tx.execute(
            "DELETE FROM source_status WHERE workspace_id = ?1",
            params![id],
        )?;
        tx.execute("DELETE FROM listing WHERE workspace_id = ?1", params![id])?;
        tx.execute(
            "DELETE FROM workspaces WHERE workspace_id = ?1",
            params![id],
        )?;
        tx.commit()?;
        Ok(documents)
    }

    // ------------------------------------------------------------- reading

    /// Assemble one document's **live** projection back out of its rows.
    ///
    /// **Nodes in the document's own bin are left out**, which is the one place
    /// this is not the exact inverse of [`replace`](Self::replace). The rows are
    /// all there — [`read_with_trash`](Self::read_with_trash) is how the bin is
    /// asked for — but the default answer is what a person can see.
    ///
    /// The reason is the consumer, not the data. Every shipped reader of this
    /// projection turns it into something keyed by document: the search index
    /// joins its fragments to `listing` and knows exactly one kind of trash, the
    /// document kind. Handing it binned *nodes* would put their text into
    /// ordinary results with nothing to mark it, and the flag it would need to
    /// filter on does not exist in its index yet. Until it does, the honest
    /// default here is the live document.
    ///
    /// It costs nothing on the way back: binning or restoring a node reprojects
    /// the document, which enqueues it, which re-reads through this — so the
    /// text leaves the index when it is binned and returns when it is restored,
    /// with no consumer having to know that node-level trash exists.
    pub fn read(&self, document: DocumentId) -> Result<Option<DocumentProjection>> {
        self.read_inner(document, NodeScope::Live)
    }

    /// The same, **including the bin** — every node, each carrying
    /// [`ProjectedNode::in_trash`](crate::model::ProjectedNode::in_trash).
    ///
    /// The exact inverse of [`replace`](Self::replace), and the door for the one
    /// kind of reader whose job is the other answer: a surface that searches the
    /// bin so somebody can put a binned node back.
    pub fn read_with_trash(&self, document: DocumentId) -> Result<Option<DocumentProjection>> {
        self.read_inner(document, NodeScope::WithTrash)
    }

    fn read_inner(
        &self,
        document: DocumentId,
        scope: NodeScope,
    ) -> Result<Option<DocumentProjection>> {
        let id = document.to_storage();
        let Some((workspace, revision)) = self
            .db
            .query_row(
                "SELECT workspace_id, source_revision FROM documents WHERE document_id = ?1",
                params![id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?
        else {
            return Ok(None);
        };
        let Some(workspace) = WorkspaceId::parse(&workspace) else {
            return Ok(None);
        };

        let mut runs: HashMap<String, Vec<ProjectedRun>> = HashMap::new();
        let mut q = self.db.prepare(
            "SELECT node_id, start, end, bold, italic, underline, strike, code, highlight,
                    script, pill
             FROM runs WHERE document_id = ?1 ORDER BY node_id, start",
        )?;
        for row in q.query_map(params![id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                ProjectedRun {
                    start: r.get::<_, i64>(1)? as usize,
                    end: r.get::<_, i64>(2)? as usize,
                    bold: r.get::<_, i64>(3)? != 0,
                    italic: r.get::<_, i64>(4)? != 0,
                    underline: r.get::<_, i64>(5)? != 0,
                    strike: r.get::<_, i64>(6)? != 0,
                    code: r.get::<_, i64>(7)? != 0,
                    highlight: r.get::<_, Option<String>>(8)?,
                    script: r.get::<_, Option<String>>(9)?,
                    pill: r.get::<_, Option<String>>(10)?,
                },
            ))
        })? {
            let (node, run) = row?;
            runs.entry(node).or_default().push(run);
        }

        let mut attrs: HashMap<String, Vec<ProjectedAttr>> = HashMap::new();
        let mut q = self.db.prepare(
            "SELECT node_id, attr_id, role, kind, value, zone, label
             FROM attrs WHERE document_id = ?1 ORDER BY node_id, rowid",
        )?;
        for row in q.query_map(params![id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                ProjectedAttr {
                    id: r.get::<_, String>(1)?,
                    role: r.get::<_, String>(2)?,
                    kind: r.get::<_, String>(3)?,
                    value: r.get::<_, String>(4)?,
                    zone: r.get::<_, Option<String>>(5)?,
                    label: r.get::<_, Option<String>>(6)?,
                },
            ))
        })? {
            let (node, attr) = row?;
            attrs.entry(node).or_default().push(attr);
        }

        let mut q = self.db.prepare(&format!(
            "SELECT node_id, parent_id, ordinal, depth, kind, text, heading, list_style, done,
                    level_name, name, icon, colour, language, blob, mime, embed, in_trash
             FROM nodes WHERE document_id = ?1 {bin} ORDER BY seq",
            bin = scope.clause("in_trash"),
        ))?;
        let nodes = q
            .query_map(params![id], |r| {
                let node_id: String = r.get(0)?;
                Ok(ProjectedNode {
                    parent: r.get(1)?,
                    ordinal: r.get::<_, i64>(2)? as usize,
                    depth: r.get::<_, i64>(3)? as usize,
                    kind: r.get(4)?,
                    text: r.get(5)?,
                    heading: r.get::<_, Option<i64>>(6)?.map(|h| h as u8),
                    list_style: r.get(7)?,
                    done: r.get::<_, i64>(8)? != 0,
                    level_name: r.get(9)?,
                    name: r.get(10)?,
                    icon: r.get(11)?,
                    colour: r.get(12)?,
                    language: r.get(13)?,
                    blob: r.get(14)?,
                    mime: r.get(15)?,
                    embed: r.get(16)?,
                    in_trash: r.get::<_, i64>(17)? != 0,
                    runs: runs.remove(&node_id).unwrap_or_default(),
                    attrs: attrs.remove(&node_id).unwrap_or_default(),
                    id: node_id,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(Some(DocumentProjection {
            workspace,
            document,
            source_revision: revision,
            nodes,
        }))
    }

    // ---------------------------------------------------- attribute queries

    /// The nodes matching an attribute query, one row each, page-bounded.
    ///
    /// Answered entirely from `nodes` + `attrs` + `listing` — no document is
    /// ever opened. Each row carries `(workspace, document, node)` together,
    /// because a node id alone means nothing outside its document, plus every
    /// attribute the node holds, the node's kind, a character-bounded text
    /// excerpt, and the source document's title and **inherited** trashed flag
    /// from the listing (a child of a binned parent carries no stamp of its
    /// own — the same recursive walk [`live_file_blob_hashes`] uses, inverted:
    /// trashed rows are flagged, never hidden, or restore could not find
    /// them).
    ///
    /// **A binned *node* is a different question and gets the other answer.**
    /// [`AttributeQueryRow::trashed`] says the *document* was binned; a node
    /// somebody binned inside a live document is simply not in this list, and
    /// nothing on the wire could have told a caller otherwise. See the module
    /// note.
    ///
    /// [`live_file_blob_hashes`]: Self::live_file_blob_hashes
    pub fn attribute_rows(&self, query: &AttributeRowQuery) -> Result<Vec<AttributeQueryRow>> {
        use koru_core::attributes::KEY_PRIORITY;

        let (conditions, params) = attribute_conditions(query, NodeScope::Live)?;

        // Sort keys are correlated subqueries rather than joins so a node with
        // no due date is still one row, sorted last. Ties break on
        // (document, seq) so a page boundary is stable between requests.
        let order = match query.sort {
            AttrSort::Due => {
                let key = due_sort_key_sql();
                format!("({key}) IS NULL, ({key}) ASC, n.document_id, n.seq")
            }
            AttrSort::Priority => {
                let key = format!(
                    "(SELECT MAX({}) FROM attrs a WHERE a.document_id = n.document_id \
                     AND a.node_id = n.node_id AND a.role = '{KEY_PRIORITY}')",
                    priority_rank_sql()
                );
                format!("({key}) IS NULL, ({key}) DESC, n.document_id, n.seq")
            }
            // **`modified_at`, never `projected_at`.** The two differ exactly
            // when the projector ran over a document nothing had changed — a
            // sweep, a recovery, a schema rebuild — which is every occasion the
            // old proxy silently reordered the list.
            AttrSort::Modified => "d.modified_at DESC, n.document_id, n.seq".to_owned(),
        };

        let sql = format!(
            "{TRASHED_CTE}
             SELECT n.document_id, n.node_id, l.workspace_id, n.kind,
                    substr(n.text, 1, {EXCERPT_CHARS}), l.title,
                    EXISTS(SELECT 1 FROM trashed t WHERE t.document_id = n.document_id)
               FROM nodes n
               JOIN listing l ON l.document_id = n.document_id
               JOIN documents d ON d.document_id = n.document_id
              WHERE {conditions}
              ORDER BY {order}
              LIMIT {limit} OFFSET {offset}",
            conditions = conditions.join(" AND "),
            limit = query.limit,
            offset = query.offset,
        );

        let mut stmt = self.db.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, bool>(6)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut attrs_stmt = self.db.prepare(
            "SELECT attr_id, role, kind, value, zone, label FROM attrs
              WHERE document_id = ?1 AND node_id = ?2 ORDER BY rowid",
        )?;
        let mut out = Vec::with_capacity(rows.len());
        for (document, node, workspace, kind, excerpt, title, trashed) in rows {
            let (Some(workspace), Some(document_id)) =
                (WorkspaceId::parse(&workspace), DocumentId::parse(&document))
            else {
                continue;
            };
            let attrs = attrs_stmt
                .query_map(params![document, node], |r| {
                    Ok(ProjectedAttr {
                        id: r.get(0)?,
                        role: r.get(1)?,
                        kind: r.get(2)?,
                        value: r.get(3)?,
                        zone: r.get(4)?,
                        label: r.get(5)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            out.push(AttributeQueryRow {
                workspace,
                document: document_id,
                node,
                node_kind: kind,
                excerpt,
                title,
                trashed,
                attrs,
            });
        }
        Ok(out)
    }

    /// How many nodes each tag holds over the **whole** matching corpus — the
    /// same filter as [`attribute_rows`], counted rather than paged.
    ///
    /// **A second statement, not a shape of the first.** Counting the rows a
    /// page returned would count a page, and a group header saying *42* while
    /// the list holds the twenty it fetched is worse than no header. The
    /// conditions and the `trashed` CTE are literally the row query's, so the
    /// set counted here is the set that query answers.
    ///
    /// `COUNT(DISTINCT document || node)` because a node carrying two tags is
    /// one row of each group and must never be two of either. The counts
    /// therefore add up to more than the row count, which is what grouping by a
    /// many-valued key means.
    ///
    /// **`aliases` folds merged tags into the tag they were merged into.**
    /// `merge_tags` re-points no node ([ADR-176]), so nodes go on carrying an
    /// id the workspace's live table cannot name — group on the raw value and a
    /// merge visibly splits one group into a named half and a nameless one. The
    /// fold happens in SQL rather than by adding the counts afterwards,
    /// because a node carrying *both* a tag and a tag merged into it is one
    /// node of one group, and two counts added together would say two.
    ///
    /// Trashed rows are counted and flagged, never dropped — the row query
    /// returns them, so a group that hid them would disagree with the list
    /// underneath it. [`AttributeGroup::trashed`] is how many of the count are
    /// binned.
    ///
    /// [`attribute_rows`]: Self::attribute_rows
    /// [ADR-176]: ../../../docs/decisions/ADR-176-200.md#adr-176-consenton--telemetry-consent-defaults-on-and-a-discoverable-opt-out-is-what-buys-it
    pub fn attribute_groups(
        &self,
        query: &AttributeRowQuery,
        aliases: &[(String, String)],
    ) -> Result<Vec<AttributeGroup>> {
        use koru_core::attributes::KEY_TAG;

        let (conditions, condition_params) = attribute_conditions(query, NodeScope::Live)?;

        // The fold, and its parameters — which bind **first**, because the
        // expression sits in the SELECT list and SQLite binds `?` by position
        // in the statement text.
        let mut params: Vec<String> = Vec::new();
        let folded = if aliases.is_empty() {
            "a.value".to_owned()
        } else {
            let mut sql = String::from("CASE a.value");
            for (from, into) in aliases {
                sql.push_str(" WHEN ? THEN ?");
                params.push(from.clone());
                params.push(into.clone());
            }
            sql.push_str(" ELSE a.value END");
            sql
        };
        params.extend(condition_params);

        let node_key = "n.document_id || ':' || n.node_id";
        let sql = format!(
            "{TRASHED_CTE}
             SELECT l.workspace_id,
                    {folded} AS tag,
                    COUNT(DISTINCT {node_key}),
                    COUNT(DISTINCT CASE WHEN EXISTS(
                            SELECT 1 FROM trashed t WHERE t.document_id = n.document_id)
                        THEN {node_key} END)
               FROM nodes n
               JOIN listing l ON l.document_id = n.document_id
               JOIN documents d ON d.document_id = n.document_id
               JOIN attrs a ON a.document_id = n.document_id AND a.node_id = n.node_id
              WHERE a.role = '{KEY_TAG}' AND {conditions}
              GROUP BY l.workspace_id, tag
              ORDER BY 3 DESC, tag",
            conditions = conditions.join(" AND "),
        );

        let mut stmt = self.db.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(workspace, tag, count, trashed)| {
                Some(AttributeGroup {
                    workspace: WorkspaceId::parse(&workspace)?,
                    tag,
                    count: count.max(0) as usize,
                    trashed: trashed.max(0) as usize,
                })
            })
            .collect())
    }

    // --------------------------------------------------------- tasks queries

    /// The tasks matching a query, one page of rows — **the same read model,
    /// asked a second question** ([ADR-287]).
    ///
    /// Answered entirely from `nodes` + `attrs` + `listing` + `documents`. **No
    /// document is ever opened**, which is the standing directive a task
    /// surface exists under: *what should I do next* comes from an index or it
    /// does not come at all.
    ///
    /// It is [`attribute_rows`](Self::attribute_rows) plus three predicates and
    /// a different page boundary, and the three are the whole of what the
    /// projection knows about tasks:
    ///
    /// - **the derivation** — `koru.task` or a due, the legacy bare `due` role
    ///   included, which is [`koru_core::tasks::is_task`] spelled in SQL;
    /// - **the capable kinds**, rendered from
    ///   [`TASK_CAPABLE_KINDS`](koru_core::tasks::TASK_CAPABLE_KINDS) the way
    ///   `priority_rank_sql` is rendered from `Priority::ALL`, so the closed
    ///   set cannot drift from the domain's copy of it;
    /// - **the lifecycle**, where *absence is open* — a due-derived task is open
    ///   without anybody having written a value.
    ///
    /// The filters it shares with an attribute query go through the one
    /// `attribute_conditions` builder, so "priority at least high" cannot mean
    /// one thing to a task page and another to an attribute page.
    ///
    /// **Both trash answers come back, and only one of them filters.** A binned
    /// *document* is flagged ([`TaskQueryRow::document_trashed`]) and never
    /// hidden, because restore has to find it; a binned *node* is returned only
    /// under [`TaskRowQuery::include_trashed`], which is the one thing that
    /// moves the node scope — see the module note for why those are two
    /// questions rather than one.
    ///
    /// [ADR-287]: ../../../docs/decisions/ADR-276-300.md#adr-287-taskread--the-tasks-query-is-a-second-question-asked-of-the-one-read-model
    pub fn task_rows(&self, query: &TaskRowQuery) -> Result<TaskPage> {
        let scope = if query.include_trashed {
            NodeScope::WithTrash
        } else {
            NodeScope::Live
        };
        let (mut conditions, mut params) = attribute_conditions(&query.attributes, scope)?;
        conditions.push(task_derivation_sql());
        conditions.push(task_kind_sql());
        // **A bucket names a lifecycle, so it replaces the lifecycle filter
        // rather than ANDing with it.** `Bucket::Completed` under the default
        // `Open` would otherwise answer nothing at all, which is a silent empty
        // page on the most obvious request anybody makes of this field.
        let recently = query.recently_completed_since.as_ref();
        if query.bucket.is_none() {
            if let Some(clause) = lifecycle_sql(query.lifecycle, recently) {
                conditions.push(clause);
                if recently.is_some() && query.lifecycle == LifecycleFilter::Open {
                    params.push(recently.expect("checked").clone());
                }
            }
        }
        if let Some(list) = &query.list {
            conditions.push(list_sql());
            params.push(list.clone());
        }
        if let Some(bucket) = query.bucket {
            let windows = query.windows.as_ref().ok_or_else(|| {
                ProjectionError("a bucket filter needs the reader's bucket windows".to_owned())
            })?;
            conditions.push(bucket_sql(bucket, windows, &mut params, recently)?);
        }
        if let Some(day) = &query.completed_on_or_after {
            conditions.push(completed_window_sql());
            params.push(day.clone());
        }

        let key = task_sort_key_sql(query.sort);

        // **The cursor is re-derived, never carried.** `after` is a task
        // reference; the sort key that row has is whatever it holds *now*, and
        // asking the client to send one back would let a stale number decide a
        // page boundary. A reference the filter no longer matches — completed,
        // removed or binned between pages — restarts the list rather than
        // failing it.
        let mut restarted = false;
        if let Some(after) = &query.after {
            match self.cursor_key(&conditions, &params, &key, after)? {
                Some(cursor) => {
                    conditions.push(keyset_sql(query.sort, &key, cursor));
                    params.push(after.clone());
                }
                None => restarted = true,
            }
        }

        let sql = format!(
            "{TRASHED_CTE}
             SELECT n.document_id, n.node_id, l.workspace_id, {TASK_REF_SQL}, n.kind,
                    n.text, n.name, n.in_trash,
                    EXISTS(SELECT 1 FROM trashed t WHERE t.document_id = n.document_id),
                    {completed}
               FROM nodes n
               JOIN listing l ON l.document_id = n.document_id
               JOIN documents d ON d.document_id = n.document_id
              WHERE {conditions}
              ORDER BY {order}
              LIMIT {limit}",
            completed = completed_sql(),
            conditions = conditions.join(" AND "),
            order = task_order_sql(query.sort, &key),
            limit = query.limit,
        );

        let mut stmt = self.db.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, bool>(7)?,
                    r.get::<_, bool>(8)?,
                    r.get::<_, bool>(9)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut attrs_stmt = self.db.prepare(
            "SELECT attr_id, role, kind, value, zone, label FROM attrs
              WHERE document_id = ?1 AND node_id = ?2 ORDER BY rowid",
        )?;
        // One page touches a handful of documents and one or two workspaces, so
        // the two walks up the tree are cached per page rather than repeated per
        // row.
        let mut workspace_names: HashMap<String, String> = HashMap::new();
        let mut document_paths: HashMap<String, Vec<String>> = HashMap::new();

        let mut out = Vec::with_capacity(rows.len());
        for (
            document,
            node,
            workspace,
            task,
            kind,
            text,
            name,
            in_trash,
            document_trashed,
            completed,
        ) in rows
        {
            let (Some(workspace_id), Some(document_id)) =
                (WorkspaceId::parse(&workspace), DocumentId::parse(&document))
            else {
                continue;
            };
            let attrs = attrs_stmt
                .query_map(params![document, node], |r| {
                    Ok(ProjectedAttr {
                        id: r.get(0)?,
                        role: r.get(1)?,
                        kind: r.get(2)?,
                        value: r.get(3)?,
                        zone: r.get(4)?,
                        label: r.get(5)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;

            if !workspace_names.contains_key(&workspace) {
                let found = self.workspace_name(&workspace)?;
                workspace_names.insert(workspace.clone(), found);
            }
            if !document_paths.contains_key(&document) {
                let found = self.document_path(&document)?;
                document_paths.insert(document.clone(), found);
            }
            // Workspace, then the document path, then the ancestor rows — and
            // the rule that joins them is the domain's, not this crate's.
            let mut segments: Vec<String> = Vec::new();
            segments.push(workspace_names[&workspace].clone());
            segments.extend(document_paths[&document].iter().cloned());
            segments.extend(self.node_ancestor_titles(&document, &node)?);
            let borrowed: Vec<&str> = segments.iter().map(String::as_str).collect();

            out.push(TaskQueryRow {
                workspace: workspace_id,
                document: document_id,
                node,
                task,
                node_kind: kind,
                lifecycle: if completed {
                    koru_core::attributes::Lifecycle::Completed
                } else {
                    koru_core::attributes::Lifecycle::Open
                }
                .as_str()
                .to_owned(),
                document_trashed,
                in_trash,
                breadcrumb: koru_core::tasks::breadcrumb(&borrowed),
                title: koru_title::resolve_title(&text, name.as_deref()).into_owned(),
                attrs,
            });
        }
        Ok(TaskPage {
            rows: out,
            restarted,
        })
    }

    /// **How many tasks each list holds, over the whole matching corpus** —
    /// [`task_rows`](Self::task_rows)'s filter, counted rather than paged, and
    /// split into the count the rail draws and the overdue share that tints it.
    ///
    /// A second statement, not a shape of the first, for
    /// [`attribute_groups`](Self::attribute_groups)'s reason: counting the rows
    /// a page returned would count a page, and a rail saying *14* over a list
    /// the store capped at 100 is worse than no number. The conditions are
    /// literally the row query's — the cursor aside, which is a page boundary
    /// rather than a filter — so the set counted here is the set that query
    /// answers. A `list` filter on the query is honoured, which makes this
    /// *the one list's* counts; a query naming none counts every list.
    ///
    /// **`overdue` is the caller's definition of overdue, not this layer's.**
    /// Bucketing is a pure function of the reader's display zone (SPEC §7), and
    /// this crate carries no timezone database: the caller passes the windows
    /// it computed — a calendar window for dates and floating wall clocks, an
    /// instant window for zoned dues — and they are compared each in its own
    /// terms, never folded ([ADR-183], rule 5). Absent means the overdue column
    /// is zero everywhere, which is the honest answer to a question nobody
    /// asked rather than a guess at midnight.
    ///
    /// Dateless tasks are normal in a list (SPEC §8), so they count toward
    /// [`TaskListCount::count`] and never toward the overdue share. Binned
    /// *documents* are counted and flagged like their rows, never dropped.
    ///
    /// [ADR-183]: ../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
    pub fn task_list_counts(
        &self,
        query: &TaskRowQuery,
        overdue: Option<&DueFilter>,
    ) -> Result<Vec<TaskListCount>> {
        let scope = if query.include_trashed {
            NodeScope::WithTrash
        } else {
            NodeScope::Live
        };
        let (mut conditions, mut params) = attribute_conditions(&query.attributes, scope)?;
        conditions.push(task_derivation_sql());
        conditions.push(task_kind_sql());
        conditions.extend(lifecycle_sql(query.lifecycle, None));
        if let Some(list) = &query.list {
            conditions.push(list_sql());
            params.push(list.clone());
        }
        if let Some(day) = &query.completed_on_or_after {
            conditions.push(completed_window_sql());
            params.push(day.clone());
        }

        // The overdue predicate sits in the SELECT list, and SQLite binds `?`
        // by position in the statement text — so its parameters bind **first**,
        // ahead of the conditions', exactly as the group query's alias fold
        // does.
        let mut overdue_params: Vec<String> = Vec::new();
        let overdue = match overdue {
            None => "0".to_owned(),
            Some(filter) => attr_exists(&format!(
                "a.role = '{}' AND ({})",
                koru_core::attributes::KEY_DUE,
                due_sql(filter, &mut overdue_params)?
            )),
        };
        overdue_params.extend(params);
        let params = overdue_params;

        let node_key = "n.document_id || ':' || n.node_id";
        let sql = format!(
            "{TRASHED_CTE}
             SELECT l.workspace_id,
                    a.value AS list,
                    COUNT(DISTINCT {node_key}),
                    COUNT(DISTINCT CASE WHEN {overdue} THEN {node_key} END),
                    COUNT(DISTINCT CASE WHEN EXISTS(
                            SELECT 1 FROM trashed t WHERE t.document_id = n.document_id)
                        THEN {node_key} END)
               FROM nodes n
               JOIN listing l ON l.document_id = n.document_id
               JOIN documents d ON d.document_id = n.document_id
               JOIN attrs a ON a.document_id = n.document_id AND a.node_id = n.node_id
              WHERE a.role = '{key}' AND {conditions}
              GROUP BY l.workspace_id, list
              ORDER BY 3 DESC, list",
            key = koru_core::attributes::KEY_LIST,
            conditions = conditions.join(" AND "),
        );

        let mut stmt = self.db.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(workspace, list, count, overdue, trashed)| {
                Some(TaskListCount {
                    workspace: WorkspaceId::parse(&workspace)?,
                    list,
                    count: count.max(0) as usize,
                    overdue: overdue.max(0) as usize,
                    trashed: trashed.max(0) as usize,
                })
            })
            .collect())
    }

    /// **How many tasks each agenda bucket holds, over the whole matching
    /// corpus** — the Tasks rail's *Today 5 · 3 overdue, Next 12, Upcoming 40,
    /// Later 9, Completed 118*.
    ///
    /// [`task_list_counts`](Self::task_list_counts)' rule, over a different key:
    /// a second statement rather than a fold over the page, because counting
    /// what came back counts a page. The filter is the row query's — the cursor
    /// aside, which is a page boundary rather than a filter — with two
    /// deliberate exceptions, and both are the bucket function's own rules
    /// rather than convenience:
    ///
    /// - **`lifecycle` is not applied.** The five buckets partition by
    ///   lifecycle: `Completed` is one of them and the other four are open, so
    ///   filtering first would empty a column the rail has to draw.
    /// - **`bucket` is not applied either.** A rail asks for every bucket's
    ///   number while the list under it shows one, so a query can carry both
    ///   and mean both.
    ///
    /// **One statement, one pass.** The five arms are an ordered `CASE` in the
    /// `SELECT` list, so the corpus is walked once and each row is placed by
    /// the first arm it matches — which is also what makes the counts a
    /// partition rather than five overlapping answers. Five scoped `COUNT`s
    /// would have been five walks of the same rows to answer one question.
    ///
    /// A bucket nothing matches has **no row** rather than a zero, the group
    /// query's rule: a rail draws five columns from its own vocabulary and
    /// reads a missing group as empty.
    pub fn task_bucket_counts(
        &self,
        query: &TaskRowQuery,
        windows: &BucketWindows,
    ) -> Result<Vec<TaskBucketCount>> {
        let scope = if query.include_trashed {
            NodeScope::WithTrash
        } else {
            NodeScope::Live
        };
        let (mut conditions, filter_params) = attribute_conditions(&query.attributes, scope)?;
        conditions.push(task_derivation_sql());
        conditions.push(task_kind_sql());
        let mut list_params: Vec<String> = Vec::new();
        if let Some(list) = &query.list {
            conditions.push(list_sql());
            list_params.push(list.clone());
        }
        // **The same window the page is drawn under**, so the rail's Completed
        // number counts the rows the page would list rather than every row
        // ever finished.
        if let Some(day) = &query.completed_on_or_after {
            conditions.push(completed_window_sql());
            list_params.push(day.clone());
        }

        // The `CASE` sits in the `SELECT` list and SQLite binds `?` by position
        // in the statement text, so **its parameters bind first**, ahead of the
        // conditions' — the fold `task_list_counts` and `attribute_groups`
        // already do, with three arms instead of one.
        let arms = bucket_arms(windows)?;
        // The pinned column below re-states the today window, and SQLite binds
        // by position, so its parameters have to be bound a second time.
        let today_again = arms.today.1.clone();
        let mut params: Vec<String> = Vec::new();
        params.extend(arms.today.1);
        params.extend(arms.next.1);
        params.extend(arms.upcoming.1);
        let mut overdue_params: Vec<String> = Vec::new();
        let overdue = due_window_sql(&windows.overdue_window(), &mut overdue_params)?;
        params.extend(overdue_params);
        params.extend(today_again);
        params.extend(filter_params);
        params.extend(list_params);

        let completed = completed_sql();
        let sql = format!(
            "{TRASHED_CTE}
             SELECT l.workspace_id,
                    CASE WHEN {completed} THEN '{done}'
                         WHEN {today} THEN '{today_token}'
                         WHEN {next} THEN '{next_token}'
                         WHEN {upcoming} THEN '{upcoming_token}'
                         ELSE '{later_token}' END AS bucket,
                    COUNT(*),
                    COUNT(CASE WHEN NOT {completed} AND {overdue} THEN 1 END),
                    COUNT(CASE WHEN EXISTS(
                            SELECT 1 FROM trashed t WHERE t.document_id = n.document_id)
                        THEN 1 END),
                    COUNT(CASE WHEN NOT {completed} AND {pinned} AND NOT {today}
                        THEN 1 END)
               FROM nodes n
               JOIN listing l ON l.document_id = n.document_id
               JOIN documents d ON d.document_id = n.document_id
              WHERE {conditions}
              GROUP BY l.workspace_id, bucket",
            today = arms.today.0,
            next = arms.next.0,
            upcoming = arms.upcoming.0,
            pinned = pinned_today_sql(),
            done = Bucket::Completed.as_str(),
            today_token = Bucket::Today.as_str(),
            next_token = Bucket::Next.as_str(),
            upcoming_token = Bucket::Upcoming.as_str(),
            later_token = Bucket::Later.as_str(),
            conditions = conditions.join(" AND "),
        );

        let mut stmt = self.db.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // **Pinned rows are counted twice on purpose, and the `GROUP BY` can
        // only count them once.**
        //
        // The `CASE` above is a partition: one row, one bucket, which is what
        // makes the five numbers add up to the corpus. The Today *queue* is not
        // a partition — `bucket_sql` lists the natural Today **plus** the
        // pinned, so a task due Friday and pinned is on two pages. Left as it
        // was, the rail said Today 0 over a page showing two rows.
        //
        // So each group also counts its own pinned-but-not-naturally-Today
        // rows, and those are folded into Today here. The sub-counts are not
        // adjusted and do not need to be: `overdue` is a window inside Today,
        // so a row outside Today cannot be overdue, and `trashed` describes
        // rows the page hides unless asked for them.
        let mut extra_today: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();
        for (workspace, _, _, _, _, pinned) in &rows {
            *extra_today.entry(workspace.clone()).or_default() += *pinned;
        }
        let mut out: Vec<TaskBucketCount> = rows
            .into_iter()
            .filter_map(|(workspace, bucket, count, overdue, trashed, _)| {
                let bucket = *Bucket::ALL.iter().find(|b| b.as_str() == bucket)?;
                let extra = if bucket == Bucket::Today {
                    extra_today.remove(&workspace).unwrap_or(0)
                } else {
                    0
                };
                Some(TaskBucketCount {
                    workspace: WorkspaceId::parse(&workspace)?,
                    bucket,
                    count: (count + extra).max(0) as usize,
                    overdue: overdue.max(0) as usize,
                    trashed: trashed.max(0) as usize,
                })
            })
            .collect();
        // A workspace whose only Today rows are pinned ones has no `today`
        // group at all — the partition put every row somewhere else — so the
        // row it needs is made here rather than lost. This is the shape the
        // report showed: Today 0 beside a page listing two.
        for (workspace, pinned) in extra_today {
            if pinned <= 0 {
                continue;
            }
            let Some(workspace) = WorkspaceId::parse(&workspace) else {
                continue;
            };
            out.push(TaskBucketCount {
                workspace,
                bucket: Bucket::Today,
                count: pinned.max(0) as usize,
                overdue: 0,
                trashed: 0,
            });
        }
        // Agenda order within a workspace, not SQLite's grouping order: the
        // rail draws them in this order and a caller should not have to sort an
        // answer that has one.
        out.sort_by_key(|row| {
            (
                row.workspace.to_storage(),
                Bucket::ALL.iter().position(|b| *b == row.bucket),
            )
        });
        Ok(out)
    }

    /// The sort key the row named by a `task:` reference holds **now**, or
    /// `None` when the filter no longer matches it. The inner `Option` is the
    /// key itself, which may legitimately be NULL — an undated task paginates
    /// through the tail of the list like any other.
    fn cursor_key(
        &self,
        conditions: &[String],
        params: &[String],
        key: &str,
        after: &str,
    ) -> Result<Option<Option<i64>>> {
        let sql = format!(
            "SELECT {key}
               FROM nodes n
               JOIN listing l ON l.document_id = n.document_id
               JOIN documents d ON d.document_id = n.document_id
              WHERE {conditions} AND {TASK_REF_SQL} = ?",
            conditions = conditions.join(" AND "),
        );
        let mut bound: Vec<&str> = params.iter().map(String::as_str).collect();
        bound.push(after);
        let mut stmt = self.db.prepare(&sql)?;
        Ok(stmt
            .query_row(rusqlite::params_from_iter(bound), |r| {
                r.get::<_, Option<i64>>(0)
            })
            .optional()?)
    }

    /// One workspace's name, or an empty segment when nothing has listed it —
    /// which [`breadcrumb`](koru_core::tasks::breadcrumb) drops rather than
    /// drawing as a gap.
    fn workspace_name(&self, workspace: &str) -> Result<String> {
        Ok(self
            .db
            .query_row(
                "SELECT name FROM workspaces WHERE workspace_id = ?1",
                params![workspace],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_default())
    }

    /// The listing titles from the workspace root down to this document,
    /// inclusive — the same walk up `parent_id` the trash CTE does, read in the
    /// other direction.
    ///
    /// Depth-bounded rather than trusted: a corrupt parent cycle must terminate,
    /// and `UNION ALL` over a row carrying its own depth would otherwise never
    /// repeat a row and never stop.
    fn document_path(&self, document: &str) -> Result<Vec<String>> {
        let mut stmt = self.db.prepare(&format!(
            "WITH RECURSIVE up(document_id, parent_id, title, depth) AS (
                 SELECT document_id, parent_id, title, 0
                   FROM listing WHERE document_id = ?1
                 UNION ALL
                 SELECT l.document_id, l.parent_id, l.title, up.depth + 1
                   FROM listing l
                   JOIN up ON l.document_id = up.parent_id
                  WHERE up.depth < {ANCESTOR_LIMIT}
             )
             SELECT title FROM up ORDER BY depth DESC"
        ))?;
        Ok(stmt
            .query_map(params![document], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The titles of the node's ancestors inside its document, root first and
    /// **excluding the node itself** — its own title is the row's `title`.
    ///
    /// The hidden trash root is skipped: it is a place rather than a heading,
    /// and it has no text, so leaving it in would put a bare "Untitled" segment
    /// into the breadcrumb of every binned task.
    fn node_ancestor_titles(&self, document: &str, node: &str) -> Result<Vec<String>> {
        let mut stmt = self.db.prepare(&format!(
            "WITH RECURSIVE up(node_id, parent_id, kind, text, name, depth) AS (
                 SELECT node_id, parent_id, kind, text, name, 0
                   FROM nodes WHERE document_id = ?1 AND node_id = ?2
                 UNION ALL
                 SELECT n.node_id, n.parent_id, n.kind, n.text, n.name, up.depth + 1
                   FROM nodes n
                   JOIN up ON n.node_id = up.parent_id AND n.document_id = ?1
                  WHERE up.depth < {ANCESTOR_LIMIT}
             )
             SELECT text, name FROM up
              WHERE depth > 0 AND kind <> '{trash}'
              ORDER BY depth DESC",
            trash = koru_core::node::Kind::Trash.as_str(),
        ))?;
        Ok(stmt
            .query_map(params![document, node], |r| {
                Ok(koru_title::resolve_title(
                    &r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?.as_deref(),
                )
                .into_owned())
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ------------------------------------------------------- attr snapshots

    // ------------------------------------------------------------ coverage

    /// Whether the rows this projection holds for a document **include** one
    /// write — the question behind `TaskQueryResponse::covered`.
    ///
    /// **One writer's own progress, not causal inclusion of a whole vector.**
    /// The requirement names the session that wrote and how far its own
    /// operations had got; the applied state is asked for that one peer's
    /// entry. Twenty commits are still one projection (the worker coalesces),
    /// so a caller waiting on counter 5 against rows applied at 6 is told
    /// *yes* — the comparison is `>=`, which is what makes coalescing safe.
    ///
    /// This replaced `stored.includes_vv(requirement)`, which was correct and
    /// unusable: the requirement it compared against grew one entry per writer
    /// the document had ever had, and the transport refused it long before the
    /// comparison could run.
    ///
    /// **Conservative in the safe direction.** The stored `source_revision` is
    /// captured before the fork the rows were read from, so the rows are at or
    /// ahead of it; this can answer a false *no*, never a false *yes*. An
    /// optimisation that read the revision after the fork would reverse that.
    ///
    /// `SchemaTooNew` is *no*: a source this build cannot read is not a
    /// projection of readable rows, whatever revision it was settled at. A
    /// document nothing has projected is *no*. A stored revision that does not
    /// decode is *no*, and a peer the applied state has never seen is *no* —
    /// which is the honest answer for a write that has not arrived.
    pub fn covers(&self, requirement: &Requirement) -> Result<bool> {
        if let Some(ProjectionSourceStatus::SchemaTooNew { .. }) =
            self.source_status(requirement.document)?
        {
            return Ok(false);
        }
        let Some(stored) = self.revision(requirement.document)? else {
            return Ok(false);
        };
        let Ok(stored) = loro::VersionVector::decode(&stored) else {
            return Ok(false);
        };
        let applied = stored.get(&requirement.peer).copied().unwrap_or(0);
        Ok(i64::from(applied) >= requirement.counter)
    }

    /// **The tasks query, answered as one read.**
    ///
    /// Rows, group counts and the coverage evidence come from one deferred
    /// read transaction on this connection, so a projection committing through
    /// the writer's connection between the steps cannot certify an older page:
    /// under WAL the snapshot is taken at the first read and held to the
    /// commit. The port used to run the rows and the counts as two statements
    /// under its own mutex with a comment saying why — that mutex serialises
    /// *users of this connection* and says nothing about the projector's, and
    /// this method is what that comment was reaching for.
    ///
    /// Nothing here touches the store or opens a document; the transaction is
    /// the projection reads and nothing else, which is why it lives at this
    /// seam rather than in the caller.
    pub fn task_answer(
        &self,
        query: &TaskRowQuery,
        grouping: TaskGrouping<'_>,
        covering: &[Requirement],
    ) -> Result<TaskAnswer> {
        let tx = self.db.unchecked_transaction()?;
        let page = self.task_rows(query)?;
        let groups = match grouping {
            TaskGrouping::Rows => TaskGroups::None,
            TaskGrouping::Lists { overdue } => {
                TaskGroups::Lists(self.task_list_counts(query, overdue)?)
            }
            TaskGrouping::Buckets => {
                let windows = query.windows.as_ref().ok_or_else(|| {
                    ProjectionError("bucket counts need the reader's bucket windows".to_owned())
                })?;
                TaskGroups::Buckets(self.task_bucket_counts(query, windows)?)
            }
        };
        let mut covered = true;
        for requirement in covering {
            if !self.covers(requirement)? {
                covered = false;
                break;
            }
        }
        tx.commit()?;
        Ok(TaskAnswer {
            page,
            groups,
            covered,
        })
    }

    /// Every attr row one document's projection holds, with the node each sits
    /// on — what the event diff reads **before** [`replace`](Self::replace)
    /// swaps the rows out from under it. One query for the whole document,
    /// never one per node, and O(that document's attrs).
    ///
    /// Row order reproduces write order — `read_attrs`'s deterministic
    /// `(role, id)` per node — so the first row under a key is the same fold
    /// winner every reader of the projection sees.
    ///
    /// **Live nodes only, and that is what makes the events right.** Binning a
    /// node used to delete its rows outright, so the diff saw the due date
    /// vanish and said `Cleared`. The rows survive the bin now; joining to
    /// `nodes` and skipping the binned ones keeps the same sentence being said
    /// — a binned due date has left the live corpus, and a restored one arrives
    /// as a `Set`, which is exactly what a consumer tracking *everything due
    /// this week* has to be told.
    pub fn document_attrs(&self, document: DocumentId) -> Result<Vec<(String, ProjectedAttr)>> {
        let mut q = self.db.prepare(&format!(
            "SELECT a.node_id, a.attr_id, a.role, a.kind, a.value, a.zone, a.label
               FROM attrs a
               JOIN nodes n ON n.document_id = a.document_id AND n.node_id = a.node_id
              WHERE a.document_id = ?1 {bin}
              ORDER BY a.node_id, a.rowid",
            bin = NodeScope::Live.clause("n.in_trash"),
        ))?;
        Ok(q.query_map(params![document.to_storage()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                ProjectedAttr {
                    id: r.get(1)?,
                    role: r.get(2)?,
                    kind: r.get(3)?,
                    value: r.get(4)?,
                    zone: r.get(5)?,
                    label: r.get(6)?,
                },
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Stable node ids in the current projection, ordered by document order.
    pub fn node_ids(&self, document: DocumentId) -> Result<Vec<String>> {
        let mut query = self
            .db
            .prepare("SELECT node_id FROM nodes WHERE document_id = ?1 ORDER BY seq")?;
        Ok(query
            .query_map(params![document.to_storage()], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The same, for every document one workspace holds — what retiring a
    /// missing workspace reads before [`remove_workspace`](Self::remove_workspace)
    /// deletes the rows. One query for the lot.
    ///
    /// **Live nodes only, for the same reason and one more.** These become
    /// `Cleared` events, and a `Cleared` for an attribute no consumer was ever
    /// told about is noise a consumer cannot act on: a binned node's attrs were
    /// never announced by [`document_attrs`](Self::document_attrs), so they must
    /// not be withdrawn here either.
    pub fn attrs_in_workspace(
        &self,
        workspace: WorkspaceId,
    ) -> Result<HashMap<DocumentId, Vec<(String, ProjectedAttr)>>> {
        let mut q = self.db.prepare(&format!(
            "SELECT a.document_id, a.node_id, a.attr_id, a.role, a.kind, a.value, a.zone, a.label
               FROM attrs a
               JOIN documents d ON d.document_id = a.document_id
               JOIN nodes n ON n.document_id = a.document_id AND n.node_id = a.node_id
              WHERE d.workspace_id = ?1 {bin}
              ORDER BY a.document_id, a.node_id, a.rowid",
            bin = NodeScope::Live.clause("n.in_trash"),
        ))?;
        let mut out: HashMap<DocumentId, Vec<(String, ProjectedAttr)>> = HashMap::new();
        for row in q.query_map(params![workspace.to_storage()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                ProjectedAttr {
                    id: r.get(2)?,
                    role: r.get(3)?,
                    kind: r.get(4)?,
                    value: r.get(5)?,
                    zone: r.get(6)?,
                    label: r.get(7)?,
                },
            ))
        })? {
            let (document, node, attr) = row?;
            if let Some(document) = DocumentId::parse(&document) {
                out.entry(document).or_default().push((node, attr));
            }
        }
        Ok(out)
    }

    // --------------------------------------------------------------- state

    /// A small key/value table, for the rebuild cursor and nothing else yet.
    pub fn state(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT value FROM state WHERE key = ?1",
                params![key],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    pub fn set_state(&mut self, key: &str, value: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn clear_state(&mut self, key: &str) -> Result<()> {
        self.db
            .execute("DELETE FROM state WHERE key = ?1", params![key])?;
        Ok(())
    }
}

/// How many *characters* of a node's text an attribute row carries. Characters
/// and never bytes — SQLite's `substr` counts characters over TEXT, so no
/// language is cut mid-glyph (the i18n length rule).
const EXCERPT_CHARS: usize = 240;

/// One attribute query, as [`Projections::attribute_rows`] takes it.
///
/// Everything here is already validated and typed — the wire layer's job is
/// done by the time this exists. Filters compose with AND; `tags` is any-of.
#[derive(Clone, Debug, Default)]
pub struct AttributeRowQuery {
    /// Empty means every workspace in the read model.
    pub workspaces: Vec<WorkspaceId>,
    /// Only nodes carrying this attribute key.
    pub has: Option<String>,
    /// Only nodes not carrying this key (within the base set of nodes that
    /// carry at least one attribute).
    pub missing: Option<String>,
    /// `TagId`s in storage form; a node matches carrying any of them.
    pub tags: Vec<String>,
    /// `koru.priority` equals exactly this.
    pub priority: Option<koru_core::attributes::Priority>,
    /// `koru.priority` is this or more important (the semantic order).
    pub min_priority: Option<koru_core::attributes::Priority>,
    /// Which `koru.due` values match, **stated per temporal type**.
    pub due: Option<DueFilter>,
    pub sort: AttrSort,
    pub limit: usize,
    pub offset: usize,
}

/// Which due values match, **one window per temporal type** ([ADR-183]).
///
/// The shape this replaces was a single bound that had to answer for values of
/// every type, so a calendar bound was silently folded onto instants at the
/// UTC day boundary — which put a Zurich evening deadline on the wrong side of
/// midnight for a third of the planet. Rule 5 of the temporal decision forbids
/// exactly that: **queries differ by type and never coerce.**
///
/// So a caller states what it means. A row matches when its own type has a
/// window here *and* it falls inside it; the windows compose with **OR**,
/// because "this week's deadlines, however they were set" is two questions
/// asked of two kinds of value. A caller that names neither window is asking
/// nothing, and is refused rather than silently matching everything.
///
/// [ADR-183]: ../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DueFilter {
    /// Compared against dues stored as a **calendar date** — and against
    /// floating wall clocks, whose day needs no zone to read.
    pub dates: Option<DateWindow>,
    /// Compared against dues stored as a **zoned instant**.
    pub instants: Option<InstantWindow>,
}

/// A half-open calendar window, `[after, before)`. Strictly `YYYY-MM-DD`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DateWindow {
    pub after: Option<String>,
    pub before: Option<String>,
}

/// A half-open instant window, `[after_ms, before_ms)`, epoch-ms UTC.
///
/// The caller chose these numbers, which means the caller chose the zone the
/// window's edges were computed in. That is the point: this layer carries no
/// timezone database and must not invent one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstantWindow {
    pub after_ms: Option<i64>,
    pub before_ms: Option<i64>,
}

/// What orders the page. The default is recency of the source document — what
/// a surface with no opinion wants — and `Due`/`Priority` sort set values
/// first, unset last.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum AttrSort {
    /// Soonest first, undated last.
    Due,
    /// Most important first (`urgent` → `low`), unset last.
    Priority,
    /// Most recently **modified** source document first — `documents.modified_at`,
    /// which moves when the projected content moves and stays put when the
    /// projector merely ran again.
    ///
    /// It is still a *document* stamp, not a node one: two rows of the same
    /// document tie and break on `(document, seq)`. Its one honest limit is a
    /// rebuild, which has no earlier digest to compare against and therefore
    /// stamps every document at the moment it was rebuilt — the order recovers
    /// as real edits land.
    #[default]
    Modified,
}

/// One matching node. Identity is the triple, together — a `node` id means
/// nothing outside `document`, and `document` means nothing without its
/// workspace once several are in scope.
#[derive(Clone, Debug)]
pub struct AttributeQueryRow {
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    pub node: String,
    pub node_kind: String,
    /// The node's text, truncated to [`EXCERPT_CHARS`] characters.
    pub excerpt: String,
    /// The source document's listing title.
    pub title: String,
    /// In the trash, inherited down the document tree.
    pub trashed: bool,
    /// Everything the node carries, not only what the filter named.
    pub attrs: Vec<ProjectedAttr>,
}

/// One tag's share of a query's whole corpus, from
/// [`Projections::attribute_groups`].
///
/// **Keyed by `(workspace, tag)`, because a `TagId` belongs to one workspace's
/// table.** A multi-workspace query gets a group per table, exactly as it gets
/// a tag list per table — folding them by id would join two workspaces' tags
/// into a heading neither of them named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeGroup {
    pub workspace: WorkspaceId,
    /// The `TagId` in storage form, **after** the alias fold: the id a live tag
    /// table can name, not necessarily the id the nodes carry.
    pub tag: String,
    /// Distinct nodes carrying it, over the whole matching corpus and not the
    /// page. A node with two tags counts once in each group, so the counts sum
    /// to more than the row count.
    pub count: usize,
    /// How many of [`count`](Self::count) sit in a binned document. Counted and
    /// flagged rather than dropped, so this agrees with the rows underneath —
    /// which are flagged, never hidden.
    pub trashed: usize,
}

// ------------------------------------------------------------ tasks queries

/// SPEC §7's agenda bucket — **the domain's enum, re-exported rather than
/// spelled again**.
///
/// The SQL in this file is a second implementation of one rule, and the one
/// thing that would make it a second *rule* is a second vocabulary. So the
/// tokens, the arms and the order all come from `koru_core::tasks::bucket`'s
/// own type, and `bucket_placement_agrees_with_the_domain_function` asserts the
/// two implementations agree row by row over the fixture corpus.
pub use koru_core::tasks::Bucket;

/// One tasks query, as [`Projections::task_rows`] takes it.
///
/// Everything here is validated and typed — the wire layer's job is done by the
/// time it exists.
#[derive(Clone, Debug, Default)]
pub struct TaskRowQuery {
    /// The filters this shares with an attribute query, **spelled once**. Its
    /// `sort`, `limit` and `offset` are ignored: a task page orders and
    /// paginates by its own rules.
    pub attributes: AttributeRowQuery,
    pub lifecycle: LifecycleFilter,
    /// **Hide tasks completed before this day**, a canonical `YYYY-MM-DD`.
    ///
    /// One predicate, applied to every path that counts or lists tasks, so
    /// the page and the rail's number cannot disagree about what Completed
    /// holds. It can only exclude a *completed* row, so the four open buckets
    /// are untouched by it; and a completed row carrying no day is kept,
    /// because a store that cannot date something must not call it old.
    pub completed_on_or_after: Option<String>,
    /// **Tasks completed on or after this day stay where they were** — in
    /// the queue their due puts them in, and in their list — instead of only
    /// in Completed. A canonical `YYYY-MM-DD`.
    ///
    /// It widens *where* a completed row is listed and nothing else: the row
    /// is still completed, still in the Completed queue, and every other
    /// filter still applies to it.
    pub recently_completed_since: Option<String>,
    /// Only tasks in this list — a [`ListId`] in storage form.
    ///
    /// **A list is a filter, not a place** (SPEC §8): this is one more
    /// predicate over one more attribute, so "the Bike Project list" and
    /// "everything due this week" are the same kind of question and compose
    /// with AND like every other filter here. An id no list table names simply
    /// matches nothing, which is what makes deleting a list cost the read side
    /// nothing.
    ///
    /// [`ListId`]: koru_core::ids::ListId
    pub list: Option<String>,
    /// Tasks in binned *documents* are flagged, never hidden — restore has to
    /// be able to find them. Tasks on binned *nodes* are the other question,
    /// and this is it: off by default, and on is what makes a binned task
    /// restorable from a Tasks surface.
    pub include_trashed: bool,
    /// Only tasks in one **agenda bucket**, placed by
    /// [`windows`](Self::windows) — which is required with it and refused
    /// without it.
    ///
    /// **A bucket supersedes [`lifecycle`](Self::lifecycle)**, because it names
    /// one: `Completed` is the first rule of the bucket function, and the other
    /// four are open by construction. ANDing the two would answer
    /// `Bucket::Completed` with nothing at all under the default `Open`, which
    /// is a silent empty page on the most obvious request.
    pub bucket: Option<Bucket>,
    /// The reader's zone, already resolved into days and instants. Required by
    /// [`bucket`](Self::bucket) and by [`task_bucket_counts`]; this layer
    /// carries no timezone database and never invents one.
    ///
    /// [`task_bucket_counts`]: Projections::task_bucket_counts
    pub windows: Option<BucketWindows>,
    pub sort: TaskSort,
    pub limit: usize,
    /// The `task:` reference the previous page ended on, exclusive.
    pub after: Option<String>,
}

/// **The reader's zone, resolved into days and instants by whoever asked.**
///
/// SPEC §7 makes a bucket a pure function of `(due, lifecycle, priority, today,
/// zone)` and this crate carries no timezone database, so the caller states the
/// edges and the arithmetic here is zone-free string and integer comparison —
/// [`DueFilter`]'s rule, applied to a second question.
///
/// The two horizons are each stated twice, once as a **calendar day** for dues
/// stored as a date or a floating wall clock and once as an **instant** for
/// dues stored zoned, and the two are never folded into each other ([ADR-183]
/// rule 5). The days are inclusive last days, because the domain function reads
/// *within seven days* and *within thirty*; the instants are the moments those
/// days end, which is the same edge said the other way and is what a half-open
/// window wants.
///
/// [ADR-183]: ../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BucketWindows {
    /// Today in the reader's zone, strictly `YYYY-MM-DD`. A due on or before it
    /// is [`Bucket::Today`] — overdue included, deliberately.
    pub today: String,
    /// The last day of [`Bucket::Next`], strictly `YYYY-MM-DD`.
    pub horizon: String,
    /// The last day of [`Bucket::Upcoming`], strictly `YYYY-MM-DD`.
    pub upcoming_horizon: String,
    /// The instant [`today`](Self::today) ends at, epoch-ms UTC.
    pub today_ends_ms: i64,
    /// The instant [`horizon`](Self::horizon) ends at, epoch-ms UTC.
    pub horizon_ends_ms: i64,
    /// The instant [`upcoming_horizon`](Self::upcoming_horizon) ends at,
    /// epoch-ms UTC.
    pub upcoming_ends_ms: i64,
    /// Now, epoch-ms UTC — what makes a **zoned** due overdue, beside
    /// [`today`](Self::today), which is what makes a dated one overdue.
    pub now_ms: i64,
    /// The priority an **undated** task must reach to be [`Bucket::Next`]
    /// rather than [`Bucket::Later`] — SPEC §7's *priority ≥ High*, as a value
    /// rather than a constant, because it is the caller's product rule.
    pub min_next_priority: koru_core::attributes::Priority,
}

impl BucketWindows {
    /// Whether these edges are usable: three strict `YYYY-MM-DD` days in
    /// order, and three instants in the same order.
    ///
    /// **Checked once, up front**, rather than discovered halfway through a
    /// statement: a caller that transposed two horizons would otherwise get a
    /// silently empty `Upcoming` and a fat `Later`, which is a wrong answer
    /// wearing the shape of a right one.
    pub fn check(&self) -> Result<()> {
        let today = parse_calendar_date(&self.today)?;
        let horizon = parse_calendar_date(&self.horizon)?;
        let upcoming = parse_calendar_date(&self.upcoming_horizon)?;
        if !(today <= horizon && horizon <= upcoming) {
            return Err(ProjectionError(
                "bucket windows run today ≤ horizon ≤ upcoming horizon".to_owned(),
            ));
        }
        if !(self.today_ends_ms <= self.horizon_ends_ms
            && self.horizon_ends_ms <= self.upcoming_ends_ms)
        {
            return Err(ProjectionError(
                "bucket window instants run today ≤ horizon ≤ upcoming horizon".to_owned(),
            ));
        }
        Ok(())
    }

    /// The window a due must fall in to be [`Bucket::Today`]: any day at or
    /// before today, in both coordinate systems.
    fn today_window(&self) -> DueFilter {
        DueFilter {
            dates: Some(DateWindow {
                after: None,
                before: Some(day_after(&self.today)),
            }),
            instants: Some(InstantWindow {
                after_ms: None,
                before_ms: Some(self.today_ends_ms),
            }),
        }
    }

    /// The **dated** half of [`Bucket::Next`] — tomorrow through the horizon.
    /// The undated half is a priority question and is not a window.
    fn next_window(&self) -> DueFilter {
        DueFilter {
            dates: Some(DateWindow {
                after: Some(day_after(&self.today)),
                before: Some(day_after(&self.horizon)),
            }),
            instants: Some(InstantWindow {
                after_ms: Some(self.today_ends_ms),
                before_ms: Some(self.horizon_ends_ms),
            }),
        }
    }

    /// [`Bucket::Upcoming`] — past the horizon, through the upcoming one.
    fn upcoming_window(&self) -> DueFilter {
        DueFilter {
            dates: Some(DateWindow {
                after: Some(day_after(&self.horizon)),
                before: Some(day_after(&self.upcoming_horizon)),
            }),
            instants: Some(InstantWindow {
                after_ms: Some(self.horizon_ends_ms),
                before_ms: Some(self.upcoming_ends_ms),
            }),
        }
    }

    /// What counts as **overdue** for a bucket rail's tint: a dated due before
    /// today, or a zoned instant before now.
    ///
    /// Not a second notion of overdue — the same [`DueFilter`] and the same
    /// [`due_sql`] the query's own `overdue` goes through, stated by the object
    /// the grouping already required rather than by a second one the caller
    /// would have to keep in step with it.
    fn overdue_window(&self) -> DueFilter {
        DueFilter {
            dates: Some(DateWindow {
                after: None,
                before: Some(self.today.clone()),
            }),
            instants: Some(InstantWindow {
                after_ms: None,
                before_ms: Some(self.now_ms),
            }),
        }
    }
}

/// Which lifecycle a task query wants. **Absence is
/// [`Open`](LifecycleFilter::Open)**, in the query and in the data alike.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum LifecycleFilter {
    #[default]
    Open,
    Completed,
    All,
}

/// What orders a task page. `Due` by default, because a task list is asked
/// *what should I do next*; `Due` and `Priority` sort set values first and
/// unset last, and `Modified` is the source document's stamp.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum TaskSort {
    #[default]
    Due,
    Priority,
    Modified,
    /// The dragged order: `koru.rank` ascending, then the never-ranked tasks
    /// by due, then the undated ones of those — see [`MANUAL_UNRANKED`].
    Manual,
}

/// Where a never-ranked task's due puts it in [`TaskSort::Manual`]: far past
/// every rank a person can drag to (ranks stay inside an `f64`'s exact
/// integers, under 2^53), so the unranked follow the ranked, by due, inside
/// the same one-number key the keyset cursor pages on.
pub const MANUAL_UNRANKED: i64 = 1 << 60;

/// One page of tasks, and whether the cursor that asked for it still existed.
///
/// A struct rather than a bare `Vec` because [`restarted`](Self::restarted) is
/// the one fact the caller cannot re-derive: an `after` naming a row the filter
/// no longer matches is answered from the top, and a client that could not tell
/// that from a continuation would silently draw page one twice.
#[derive(Clone, Debug, Default)]
pub struct TaskPage {
    pub rows: Vec<TaskQueryRow>,
    pub restarted: bool,
}

/// One write a query wants included — the decoded form of the wire's
/// `RevisionRequirement`. Decoding is the caller's, at the boundary, because a
/// requirement that does not decode is a client bug to refuse rather than a
/// document to report uncovered.
#[derive(Clone, Debug)]
pub struct Requirement {
    pub document: DocumentId,
    /// The writing session, and the exclusive end of its own operations when
    /// the write committed. See `koru_store_wire::tasks::RevisionRequirement`
    /// for why this is one operation rather than a version vector.
    pub peer: u64,
    pub counter: i64,
}

impl Requirement {
    pub fn new(document: DocumentId, peer: u64, counter: i64) -> Self {
        Self {
            document,
            peer,
            counter,
        }
    }
}

/// Which counts [`Projections::task_answer`] adds beside the page.
#[derive(Clone, Copy, Debug)]
pub enum TaskGrouping<'a> {
    Rows,
    Lists { overdue: Option<&'a DueFilter> },
    Buckets,
}

/// The counts that came back, in the grouping's own shape.
#[derive(Clone, Debug)]
pub enum TaskGroups {
    None,
    Lists(Vec<TaskListCount>),
    Buckets(Vec<TaskBucketCount>),
}

/// One coherent answer: the page, its counts, and whether they include every
/// write the caller named. `covered` is `true` when nothing was required.
#[derive(Clone, Debug)]
pub struct TaskAnswer {
    pub page: TaskPage,
    pub groups: TaskGroups,
    pub covered: bool,
}

/// One task. Identity is the triple, together, and [`task`](Self::task) is that
/// triple as the one string that is also the page cursor and the subject its
/// activity is filed under.
#[derive(Clone, Debug)]
pub struct TaskQueryRow {
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    pub node: String,
    /// `task:<workspace>:<document>:<node>`.
    pub task: String,
    pub node_kind: String,
    /// `open` or `completed`.
    pub lifecycle: String,
    /// The **document** is binned, inherited down the document tree. Flagged,
    /// never hidden.
    pub document_trashed: bool,
    /// The **node** sits in its document's own hidden trash root. Returned only
    /// under [`TaskRowQuery::include_trashed`].
    pub in_trash: bool,
    /// Workspace, then the document path, then the ancestor rows.
    pub breadcrumb: String,
    /// `koru-title`'s chain applied to this node.
    pub title: String,
    /// Everything the node carries, so a row draws without a second request.
    pub attrs: Vec<ProjectedAttr>,
}

/// One list's share of a query's whole corpus, from
/// [`Projections::task_list_counts`].
///
/// **Keyed by `(workspace, list)`**, for [`AttributeGroup`]'s reason: a
/// [`ListId`](koru_core::ids::ListId) belongs to one workspace's list table,
/// and folding two workspaces' lists by id would join rails neither of them
/// named.
///
/// The counts are exclusive of nothing: a task counts once in exactly one list,
/// because `koru.list` is cardinality-one — so unlike a tag group these sum to
/// at most the corpus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskListCount {
    pub workspace: WorkspaceId,
    /// The `ListId` in storage form. A list table names it; this layer does
    /// not, and an id no table names still counts — a rail draws what it can
    /// name and a stale membership is not a missing task.
    pub list: String,
    /// Matching tasks in this list, over the whole corpus and not the page.
    pub count: usize,
    /// How many of [`count`](Self::count) are overdue by the windows the caller
    /// passed. Zero when it passed none.
    pub overdue: usize,
    /// How many of [`count`](Self::count) sit in a binned document. Counted and
    /// flagged rather than dropped, so this agrees with the rows underneath.
    pub trashed: usize,
}

/// One bucket's share of a query's whole corpus, from
/// [`Projections::task_bucket_counts`].
///
/// **Keyed by `(workspace, bucket)`** for [`TaskListCount`]'s reason, though
/// the reason is weaker here: a bucket token is universal where a `ListId` is
/// not. They stay per workspace because a task belongs to exactly one, so a
/// cross-workspace rail sums them and loses nothing, while folding here would
/// throw away an answer a caller may want.
///
/// The five **partition** the corpus: every matching task is in exactly one, so
/// unlike a tag group these sum to the whole matching set rather than to more
/// or less than it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskBucketCount {
    pub workspace: WorkspaceId,
    pub bucket: Bucket,
    /// Matching tasks in this bucket, over the whole corpus and not the page.
    pub count: usize,
    /// How many of [`count`](Self::count) are overdue by the windows'
    /// `today` and `now_ms`. Meaningful for [`Bucket::Today`] — the rail's
    /// *5 · 3 overdue* — and zero everywhere else by construction, because
    /// nothing overdue is placed anywhere else.
    pub overdue: usize,
    /// How many of [`count`](Self::count) sit in a binned document. Counted and
    /// flagged rather than dropped, so this agrees with the rows underneath.
    pub trashed: usize,
}

/// Whether a node carries a due value this query can read a day out of.
///
/// **`koru.due` only, and only the three temporal kinds** — the legacy bare
/// role derives a *task* but never places one, exactly as it does for
/// [`due_sql`] and [`due_sort_key_sql`]; and a value of some other kind is a
/// due nothing can place, which is what the domain function is handed as
/// `None`… except that `None` there means *undated*, and an undated task with a
/// high priority is `Next`. So this asks the narrower question — *is there a
/// readable day* — and a due of an unreadable kind falls through every window
/// into `Later` rather than being promoted by a priority it was never asked
/// about.
fn has_readable_due_sql() -> String {
    use koru_core::time::TemporalKind;
    let kinds = [
        TemporalKind::Date,
        TemporalKind::Floating,
        TemporalKind::Zoned,
    ]
    .map(|k| format!("'{}'", k.token()))
    .join(", ");
    attr_exists(&format!(
        "a.role = '{}' AND a.kind IN ({kinds})",
        koru_core::attributes::KEY_DUE
    ))
}

/// A due falling inside one window, as a predicate over the row.
fn due_window_sql(filter: &DueFilter, params: &mut Vec<String>) -> Result<String> {
    Ok(attr_exists(&format!(
        "a.role = '{}' AND ({})",
        koru_core::attributes::KEY_DUE,
        due_sql(filter, params)?
    )))
}

/// `koru.priority` at or above the floor — the semantic order, from
/// [`attribute_conditions`]' own arithmetic so the two floors cannot drift.
fn priority_floor_sql(floor: koru_core::attributes::Priority) -> String {
    let rank = koru_core::attributes::Priority::ALL
        .iter()
        .position(|p| *p == floor)
        .unwrap_or(0);
    attr_exists(&format!(
        "a.role = '{}' AND ({} >= {rank})",
        koru_core::attributes::KEY_PRIORITY,
        priority_rank_sql()
    ))
}

/// **The four open buckets as SQL, mutually exclusive by construction.**
///
/// A second implementation of `koru_core::tasks::bucket`, and the rules are
/// its rules in its order:
///
/// 1. a day at or before today → `Today` (overdue included);
/// 2. a day through the horizon → `Next`;
/// 3. **no readable day, and priority ≥ the floor** → `Next` — the union that
///    cannot be spelled as a filter, because filters AND;
/// 4. a day through the upcoming horizon → `Upcoming`;
/// 5. everything else → `Later`.
///
/// Each arm excludes the ones above it rather than trusting the data to be
/// unambiguous: a node carrying two `koru.due` values would otherwise match two
/// windows and be counted twice, where the domain function — handed one value —
/// answers once. Exclusion makes the five a partition whatever the row holds,
/// which is what lets the counts sum to the corpus.
///
/// `Later` is the **complement**, exactly as the domain function's final `else`
/// is: naming it positively would be a fourth chance to disagree.
///
/// **Each arm carries its own parameters**, because SQLite binds `?` by
/// position in the statement text: a caller that writes two of the three arms
/// must bind two of the three parameter lists, in the order it wrote them.
/// Pushing them all into one vector up front — the shape this started as — put
/// an `Upcoming` window's dates into a `Today` page's placeholders.
struct BucketArms {
    today: (String, Vec<String>),
    next: (String, Vec<String>),
    upcoming: (String, Vec<String>),
}

fn bucket_arms(windows: &BucketWindows) -> Result<BucketArms> {
    windows.check()?;
    let mut today_params = Vec::new();
    let today = due_window_sql(&windows.today_window(), &mut today_params)?;
    let mut next_params = Vec::new();
    let next_dated = due_window_sql(&windows.next_window(), &mut next_params)?;
    let next = format!(
        "({next_dated} OR (NOT {has_due} AND {floor}))",
        has_due = has_readable_due_sql(),
        floor = priority_floor_sql(windows.min_next_priority),
    );
    let mut upcoming_params = Vec::new();
    let upcoming = due_window_sql(&windows.upcoming_window(), &mut upcoming_params)?;
    Ok(BucketArms {
        today: (today, today_params),
        next: (next, next_params),
        upcoming: (upcoming, upcoming_params),
    })
}

/// One bucket's whole predicate, lifecycle included — what
/// [`TaskRowQuery::bucket`] filters a page with.
///
/// The lifecycle term is part of the bucket rather than ANDed beside it,
/// because rule 1 of the domain function is a lifecycle: `Completed` is a
/// bucket, and the other four mean *open* whatever else is true.
fn bucket_sql(
    bucket: Bucket,
    windows: &BucketWindows,
    params: &mut Vec<String>,
    recently: Option<&String>,
) -> Result<String> {
    if bucket == Bucket::Completed {
        // **Completed is unchanged by the window.** A task that stays on
        // Today for a day is still a completed task, and taking it out of the
        // queue it belongs to would be the gesture hiding its own result.
        return Ok(completed_sql());
    }
    let arms = bucket_arms(windows)?;
    let open = open_sql(recently);
    // `open` leads every arm below, so its parameter binds first.
    if let Some(day) = recently {
        params.push(day.clone());
    }
    let (today, today_params) = arms.today;
    let (next, next_params) = arms.next;
    let (upcoming, upcoming_params) = arms.upcoming;
    Ok(match bucket {
        Bucket::Today => {
            params.extend(today_params);
            // **The Today *queue* is the natural Today plus the pinned**, and
            // the OR belongs here rather than in `bucket_arms`.
            //
            // `today` is used four times: once as this bucket's own predicate,
            // and three times as the `NOT today` that keeps Next, Upcoming and
            // Later disjoint from it. Widening it there would have taken a
            // pinned task *out* of the bucket its due names — a pin would have
            // become a silent edit of somebody's deadline. Here it adds a
            // membership and nothing else: a task due Friday and pinned is on
            // Today and still on Upcoming.
            //
            // Which is also why `bucket` in `koru_core::tasks` is untouched
            // and this SQL still agrees with it: the domain function names the
            // one *natural* bucket, and a pin is not one.
            format!(
                "({open} AND ({today} OR {pinned}))",
                pinned = pinned_today_sql()
            )
        }
        Bucket::Next => {
            params.extend(today_params);
            params.extend(next_params);
            format!("({open} AND NOT {today} AND {next})")
        }
        Bucket::Upcoming => {
            params.extend(today_params);
            params.extend(next_params);
            params.extend(upcoming_params);
            format!("({open} AND NOT {today} AND NOT {next} AND {upcoming})")
        }
        _ => {
            params.extend(today_params);
            params.extend(next_params);
            params.extend(upcoming_params);
            format!("({open} AND NOT {today} AND NOT {next} AND NOT {upcoming})")
        }
    })
}

/// **Pinned to Today by hand** — `koru.today` present and true.
///
/// The value is checked rather than only the record's existence, so this agrees
/// with `TaskFields::pinned_today`, which reads the bool and treats a stored
/// `false` as unpinned. `unpin_from_today` removes the record rather than
/// writing `false`, so the two can only disagree over a value no command in
/// this build produces — and where they could, both say *not pinned*.
/// `AttrValue::Bool` reaches the `attrs` table through `b.to_string()`, so the
/// literal is `'true'`.
fn pinned_today_sql() -> String {
    attr_exists(&format!(
        "a.role = '{}' AND a.value = 'true'",
        koru_core::attributes::KEY_TODAY
    ))
}

/// The `koru.list` membership predicate, with the id bound as a `?`.
fn list_sql() -> String {
    attr_exists(&format!(
        "a.role = '{}' AND a.value = ?",
        koru_core::attributes::KEY_LIST
    ))
}

/// `task:<workspace>:<document>:<node>` in SQL.
///
/// **Built where it is ordered.** It is the row's identity, its tie-break and
/// its cursor, and the three have to be one string or a page boundary means
/// something different from the thing it is compared against.
const TASK_REF_SQL: &str =
    "('task:' || l.workspace_id || ':' || n.document_id || ':' || n.node_id)";

/// How far up a parent chain a breadcrumb walk goes before it stops.
///
/// A bound rather than a trust: a corrupt `parent_id` cycle must terminate, and
/// a recursive `UNION ALL` over rows carrying their own depth never repeats a
/// row and so would never stop on its own.
const ANCESTOR_LIMIT: usize = 64;

/// `is_task = marker OR koru.due`, the legacy bare `due` role included — the
/// whole derivation ([`koru_core::tasks::is_task`]), spelled in SQL.
fn task_derivation_sql() -> String {
    use koru_core::attributes::{KEY_DUE, KEY_TASK, LEGACY_DUE_ROLE};
    attr_exists(&format!(
        "a.role IN ('{KEY_TASK}', '{KEY_DUE}', '{LEGACY_DUE_ROLE}')"
    ))
}

/// SPEC §4's closed set, rendered from
/// [`TASK_CAPABLE_KINDS`](koru_core::tasks::TASK_CAPABLE_KINDS) so a kind
/// added to or taken out of the domain's list cannot leave this query behind.
fn task_kind_sql() -> String {
    let kinds: Vec<String> = koru_core::tasks::TASK_CAPABLE_KINDS
        .iter()
        .map(|kind| format!("'{}'", kind.as_str()))
        .collect();
    format!("n.kind IN ({})", kinds.join(", "))
}

/// Whether the node carries a completed lifecycle. Used as a *column* on every
/// row and as a *condition* under a filter, from one definition — the two must
/// agree or a row says `open` in a list of completed tasks.
fn completed_sql() -> String {
    use koru_core::attributes::{KEY_TASK_STATE, Lifecycle};
    attr_exists(&format!(
        "a.role = '{KEY_TASK_STATE}' AND a.value = '{}'",
        Lifecycle::Completed.as_str()
    ))
}

/// **A completed task old enough to have fallen out of the window.**
///
/// `NOT (completed AND finished before the boundary)` — so it excludes stale
/// completed rows and nothing else: an open task fails the first half, and a
/// completed one with no recorded day fails the second, which is the rule
/// that keeps work recorded by an older build in sight.
/// **Not completed — or completed recently enough to stay in place.**
///
/// The predicate every open bucket and the open lifecycle filter are built
/// on. With no window it is plain `NOT completed`, which is what those two
/// have always meant; with one, a row finished on or after the boundary
/// passes it too, so a queue lists it by its due and a list lists it beside
/// the open work.
fn open_sql(recently: Option<&String>) -> String {
    let open = format!("NOT {}", completed_sql());
    match recently {
        None => open,
        Some(_) => {
            let recent = attr_exists(&format!(
                "a.role = '{}' AND a.value >= ?",
                koru_core::attributes::KEY_COMPLETED_AT
            ));
            format!("({open} OR {recent})")
        }
    }
}

fn completed_window_sql() -> String {
    let stale = attr_exists(&format!(
        "a.role = '{}' AND a.value < ?",
        koru_core::attributes::KEY_COMPLETED_AT
    ));
    format!("NOT ({} AND {stale})", completed_sql())
}

/// The lifecycle condition, or nothing at all for
/// [`LifecycleFilter::All`]. **Absence of the attribute is `open`**, which is
/// why the open arm is a `NOT EXISTS` rather than an equality against a token
/// nothing wrote.
fn lifecycle_sql(filter: LifecycleFilter, recently: Option<&String>) -> Option<String> {
    match filter {
        LifecycleFilter::Open => Some(open_sql(recently)),
        // The other two name completed rows themselves, so a window that only
        // widens where a completed row is listed has nothing to say to them.
        LifecycleFilter::Completed => Some(completed_sql()),
        LifecycleFilter::All => None,
    }
}

/// The one-number sort key for a task page, parenthesised so a caller can write
/// `{key} IS NULL` around it.
fn task_sort_key_sql(sort: TaskSort) -> String {
    match sort {
        TaskSort::Due => format!("({})", due_sort_key_sql()),
        TaskSort::Priority => format!(
            "(SELECT MAX({}) FROM attrs a WHERE a.document_id = n.document_id \
             AND a.node_id = n.node_id AND a.role = '{}')",
            priority_rank_sql(),
            koru_core::attributes::KEY_PRIORITY
        ),
        TaskSort::Modified => "d.modified_at".to_owned(),
        // One number, so the keyset cursor pages it as it pages the others:
        // the rank where there is one, else the due pushed past every rank,
        // else NULL — last, by reference, the tie-break every sort shares.
        TaskSort::Manual => format!(
            "(COALESCE((SELECT MIN(CAST(a.value AS INTEGER)) FROM attrs a \
             WHERE a.document_id = n.document_id AND a.node_id = n.node_id \
             AND a.role = '{}'), {MANUAL_UNRANKED} + ({})))",
            koru_core::attributes::KEY_RANK,
            due_sort_key_sql()
        ),
    }
}

/// Whether smaller is earlier. Only a due date is: a bigger priority rank and a
/// later modification both sort first.
fn ascending(sort: TaskSort) -> bool {
    matches!(sort, TaskSort::Due | TaskSort::Manual)
}

/// `ORDER BY (key) IS NULL, (key) …, task ASC`.
///
/// The `IS NULL` first term is the NULLS-LAST idiom
/// [`Projections::attribute_rows`] already uses, so an undated task sorts last
/// instead of first. The tie-break is the **task reference**, which is unique
/// per row and stable across every edit to it — so no two rows tie, and a
/// concurrent insert can neither repeat nor skip a row across a page boundary.
fn task_order_sql(sort: TaskSort, key: &str) -> String {
    let direction = if ascending(sort) { "ASC" } else { "DESC" };
    format!("{key} IS NULL, {key} {direction}, {TASK_REF_SQL} ASC")
}

/// Everything strictly after `(cursor, after)` in the declared order, with the
/// reference bound as the trailing `?`.
///
/// The NULL-keyed tail is the half that is easy to get wrong: from a row that
/// *has* a key, every NULL-keyed row is still to come; from a row that does
/// not, only NULL-keyed rows with a later reference are.
fn keyset_sql(sort: TaskSort, key: &str, cursor: Option<i64>) -> String {
    match cursor {
        Some(at) if ascending(sort) => {
            format!("({key} IS NULL OR {key} > {at} OR ({key} = {at} AND {TASK_REF_SQL} > ?))")
        }
        Some(at) => {
            format!("({key} IS NULL OR {key} < {at} OR ({key} = {at} AND {TASK_REF_SQL} > ?))")
        }
        None => format!("({key} IS NULL AND {TASK_REF_SQL} > ?)"),
    }
}

/// Whether a query over `nodes` means the live document or the whole of it,
/// bin included.
///
/// **The node-level trash, never the document-level one** — those are separate
/// questions answered by separate columns, and the module note says which is
/// which. One type rather than a bare `bool` at four call sites, so a query
/// that means *live content* says so in words.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum NodeScope {
    /// Nodes a person can see. The default answer everywhere.
    Live,
    /// Everything, each row carrying its flag.
    WithTrash,
}

impl NodeScope {
    /// The predicate, on its own — for a query that assembles its conditions.
    fn condition(self, column: &str) -> Option<String> {
        match self {
            Self::Live => Some(format!("{column} = 0")),
            Self::WithTrash => None,
        }
    }

    /// The predicate as a trailing `AND …` — for a query with a `WHERE`
    /// already written. Empty for [`WithTrash`](Self::WithTrash).
    fn clause(self, column: &str) -> String {
        match self.condition(column) {
            Some(condition) => format!("AND {condition}"),
            None => String::new(),
        }
    }
}

/// Which documents are in the trash, **inherited down the document tree**.
///
/// The manifest stamps only the entry that was binned, so a child of a binned
/// parent carries no stamp of its own — the same recursive walk
/// [`Projections::live_file_blob_hashes`] uses, inverted. One constant because
/// the row query and the group query must answer *is this binned* the same way:
/// two copies that drift make a group's count disagree with the list under it.
const TRASHED_CTE: &str = "WITH RECURSIVE trashed(document_id) AS (
                 SELECT document_id FROM listing WHERE trashed_at IS NOT NULL
                 UNION
                 SELECT child.document_id
                   FROM listing child
                   JOIN trashed parent ON child.parent_id = parent.document_id
             )";

/// The `WHERE` clauses an attribute question is made of, and the parameters
/// they bind, **in binding order**.
///
/// Shared by [`Projections::attribute_rows`] and
/// [`Projections::attribute_groups`] so a filter cannot mean one thing to a
/// page and another to the count above it. Every `?` is a string; every number
/// is formatted into the SQL directly — an `i64`/`usize` rendered with `{}`
/// cannot carry an injection, and one uniform parameter type keeps the
/// placeholder ordering checkable by reading top to bottom.
///
/// The aliases `n` (nodes) and `l` (listing) are the caller's to provide.
///
/// `scope` is the **node-level** bin, and it is a parameter rather than a
/// constant because the two questions this builder serves may one day differ: a
/// tasks query offering to restore a binned task asks for
/// [`NodeScope::WithTrash`], while an attribute page and its group counts both
/// mean the live document — and they must agree, or a header says 42 over a
/// list of 41. Appending a condition cannot undo one, so the choice belongs
/// here rather than at a call site. The **document-level** bin is a separate
/// question and stays with [`TRASHED_CTE`], where rows are flagged rather than
/// dropped.
fn attribute_conditions(
    query: &AttributeRowQuery,
    scope: NodeScope,
) -> Result<(Vec<String>, Vec<String>)> {
    use koru_core::attributes::{KEY_DUE, KEY_PRIORITY, KEY_TAG};

    let mut params: Vec<String> = Vec::new();
    let mut conditions = vec![
        // The base set: nodes carrying at least one attribute. An attribute
        // query is never a full-text walk of every paragraph, even under
        // `missing`.
        "EXISTS (SELECT 1 FROM attrs a WHERE a.document_id = n.document_id \
         AND a.node_id = n.node_id)"
            .to_owned(),
    ];
    conditions.extend(scope.condition("n.in_trash"));

    if !query.workspaces.is_empty() {
        let marks = vec!["?"; query.workspaces.len()].join(", ");
        conditions.push(format!("l.workspace_id IN ({marks})"));
        params.extend(query.workspaces.iter().map(|w| w.to_storage()));
    }
    if let Some(key) = &query.has {
        conditions.push(attr_exists("a.role = ?"));
        params.push(key.clone());
    }
    if let Some(key) = &query.missing {
        conditions.push(format!("NOT {}", attr_exists("a.role = ?")));
        params.push(key.clone());
    }
    if !query.tags.is_empty() {
        let marks = vec!["?"; query.tags.len()].join(", ");
        conditions.push(attr_exists(&format!(
            "a.role = '{KEY_TAG}' AND a.value IN ({marks})"
        )));
        params.extend(query.tags.iter().cloned());
    }
    if let Some(priority) = query.priority {
        conditions.push(attr_exists(&format!(
            "a.role = '{KEY_PRIORITY}' AND a.value = ?"
        )));
        params.push(priority.as_str().to_owned());
    }
    if let Some(floor) = query.min_priority {
        // The *semantic* order, `low < medium < high < urgent` — never the
        // alphabetic one, under which "high ≥ medium" is false. The CASE is
        // derived from `Priority::ALL` so a token cannot drift; an unknown
        // token maps to -1 and matches no floor.
        let rank = koru_core::attributes::Priority::ALL
            .iter()
            .position(|p| *p == floor)
            .unwrap_or(0);
        conditions.push(attr_exists(&format!(
            "a.role = '{KEY_PRIORITY}' AND ({} >= {rank})",
            priority_rank_sql()
        )));
    }
    if let Some(due) = &query.due {
        conditions.push(attr_exists(&format!(
            "a.role = '{KEY_DUE}' AND ({})",
            due_sql(due, &mut params)?
        )));
    }
    Ok((conditions, params))
}

/// An `EXISTS` over the row's own attrs, with `inner` as the extra predicate.
fn attr_exists(inner: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM attrs a WHERE a.document_id = n.document_id \
         AND a.node_id = n.node_id AND {inner})"
    )
}

/// `CASE a.value ... END` mapping priority tokens to their semantic rank,
/// derived from [`Priority::ALL`] so the SQL cannot drift from the enum. An
/// unknown token ranks -1: below every floor, above nothing.
///
/// [`Priority::ALL`]: koru_core::attributes::Priority::ALL
fn priority_rank_sql() -> String {
    let whens: String = koru_core::attributes::Priority::ALL
        .iter()
        .enumerate()
        .map(|(rank, p)| format!("WHEN '{}' THEN {rank} ", p.as_str()))
        .collect();
    format!("CASE a.value {whens}ELSE -1 END")
}

/// The one-number due sort key: every temporal type folded onto epoch
/// milliseconds, a calendar day and a floating wall clock read **as if UTC**.
///
/// **Folding is legitimate for ordering and never for filtering**, and the two
/// are different jobs. A sorted page has to put every row somewhere, so a
/// single number is unavoidable; a *filter* answers a question with a yes or a
/// no, and folding there would answer the wrong question silently — which is
/// why [`due_sql`] compares each type in its own terms instead ([ADR-183],
/// rule 5).
///
/// What it costs: a zoned deadline and a floating one within a few hours of
/// each other can order the "wrong" way round by up to the zone's offset. That
/// is a visible, bounded imprecision in a list's order, not a wrong answer to
/// a question anybody asked.
///
/// [ADR-183]: ../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
fn due_sort_key_sql() -> String {
    let date = koru_core::time::TemporalKind::Date.token();
    let floating = koru_core::time::TemporalKind::Floating.token();
    let zoned = koru_core::time::TemporalKind::Zoned.token();
    format!(
        "SELECT MIN(CASE a.kind              WHEN '{date}' THEN CAST(strftime('%s', a.value) AS INTEGER) * 1000              WHEN '{floating}' THEN CAST(strftime('%s', a.value) AS INTEGER) * 1000              WHEN '{zoned}' THEN CAST(a.value AS INTEGER) END)          FROM attrs a WHERE a.document_id = n.document_id          AND a.node_id = n.node_id AND a.role = '{}'",
        koru_core::attributes::KEY_DUE
    )
}

/// The SQL for a [`DueFilter`] — **one arm per temporal type, OR-ed**, and
/// nothing compared across types.
///
/// - The **date arm** matches `date` and `datetime_floating` rows. Both carry
///   a day whose meaning needs no zone (a floating value's day is the first
///   ten characters of its wall clock), and canonical `YYYY-MM-DD` compares
///   lexicographically in date order, which is why the strings can be compared
///   directly.
/// - The **instant arm** matches `datetime_zoned` rows numerically. The
///   caller supplied the millisecond edges, so the caller chose the zone they
///   were computed in — this layer carries no timezone database and must not
///   invent one.
///
/// A row of a type with no arm simply does not match. A filter with no arm at
/// all is refused: it is a question nobody asked, and answering "everything"
/// would be a guess.
fn due_sql(filter: &DueFilter, params: &mut Vec<String>) -> Result<String> {
    let mut arms: Vec<String> = Vec::new();
    if let Some(window) = &filter.dates {
        let date = koru_core::time::TemporalKind::Date.token();
        let floating = koru_core::time::TemporalKind::Floating.token();
        // The first ten characters are the day, for both spellings.
        let mut parts = vec![format!("a.kind IN ('{date}', '{floating}')")];
        for (bound, op) in [(&window.after, ">="), (&window.before, "<")] {
            if let Some(day) = bound {
                parse_calendar_date(day)?;
                parts.push(format!("substr(a.value, 1, 10) {op} ?"));
                params.push(day.clone());
            }
        }
        arms.push(format!("({})", parts.join(" AND ")));
    }
    if let Some(window) = &filter.instants {
        let zoned = koru_core::time::TemporalKind::Zoned.token();
        let mut parts = vec![format!("a.kind = '{zoned}'")];
        for (bound, op) in [(window.after_ms, ">="), (window.before_ms, "<")] {
            if let Some(ms) = bound {
                parts.push(format!("CAST(a.value AS INTEGER) {op} {ms}"));
            }
        }
        arms.push(format!("({})", parts.join(" AND ")));
    }
    if arms.is_empty() {
        return Err(ProjectionError(
            "a due filter names a calendar window, an instant window, or both".to_owned(),
        ));
    }
    Ok(arms.join(" OR "))
}

/// Strict `YYYY-MM-DD` to days since the Unix epoch. Strict because the
/// string is also compared lexicographically against stored canonical dates:
/// an unpadded `2026-9-1` would parse fine and then sort before every August.
fn parse_calendar_date(s: &str) -> Result<i64> {
    let bytes = s.as_bytes();
    let all_digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !all_digits(0..4)
        || !all_digits(5..7)
        || !all_digits(8..10)
    {
        return Err(ProjectionError(
            "a calendar date bound must be YYYY-MM-DD".to_owned(),
        ));
    }
    let year: i64 = s[0..4].parse().unwrap_or(0);
    let month: i64 = s[5..7].parse().unwrap_or(0);
    let day: i64 = s[8..10].parse().unwrap_or(0);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(ProjectionError(
            "a calendar date bound must name a real month and day".to_owned(),
        ));
    }
    Ok(days_from_civil(year, month, day))
}

/// Select the only authored text a native reminder may show.
///
/// A named non-text node (for example an image) owns its name. Text nodes use
/// one readable line only; carrying the complete projected value would leak a
/// second line into a lock-screen notification. The document title is the
/// final label when the item itself has neither.
fn reminder_title(name: Option<&str>, text: Option<&str>, document: Option<&str>) -> String {
    name.and_then(nonempty)
        .or_else(|| text.and_then(|value| value.lines().find_map(reminder_text_line)))
        .or_else(|| document.and_then(nonempty))
        .unwrap_or_default()
        .to_owned()
}

/// Remove the transient slash-command suffix from reminder-owned text. The
/// editor deletes this exact range on submission, but projection can observe
/// the document between command materialization and that follow-up edit. A
/// native notification must never expose implementation syntax from that
/// intermediate snapshot.
fn reminder_text_line(value: &str) -> Option<&str> {
    let value = value.trim();
    let command = value
        .match_indices("/remind")
        .find_map(|(offset, command)| {
            let before = value[..offset].chars().next_back();
            let after = value[offset + command.len()..].chars().next();
            (before.is_none_or(char::is_whitespace) && after.is_none_or(char::is_whitespace))
                .then_some(offset)
        });
    nonempty(command.map_or(value, |offset| &value[..offset]))
}

fn nonempty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

/// Days since 1970-01-01 for a proleptic Gregorian date — Howard Hinnant's
/// `days_from_civil`, which is also what `chrono` implements. Here as twelve
/// lines rather than as a dependency this crate does not otherwise need.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`] — Hinnant's `civil_from_days`, and the
/// two are a pair or neither is trustworthy.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The day after a canonical `YYYY-MM-DD`, canonical again.
///
/// **Calendar arithmetic is not zone knowledge.** *The day after 2026-02-28* has
/// one answer everywhere on the planet, which is exactly why [`DateOnly`] exists
/// as a type with no instant in it — so computing it here is not the timezone
/// database this crate refuses to carry. It is here because the caller states
/// horizons as **inclusive last days**, the way SPEC §7 reads them, and
/// [`DueFilter`]'s windows are half-open.
///
/// A string this cannot parse is returned unchanged: the bound is parsed again
/// inside [`due_sql`], which is where a malformed date is refused, and
/// answering twice would put the refusal in two places.
///
/// [`DateOnly`]: koru_core::time::DateOnly
fn day_after(day: &str) -> String {
    match parse_calendar_date(day) {
        Ok(days) => {
            let (y, m, d) = civil_from_days(days + 1);
            format!("{y:04}-{m:02}-{d:02}")
        }
        Err(_) => day.to_owned(),
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS documents (
    document_id     TEXT PRIMARY KEY,
    workspace_id    TEXT NOT NULL,
    source_revision BLOB NOT NULL,
    -- **When the projector last ran over this document**, which is not the same
    -- question as when its content moved. Kept because a diagnostic wants it;
    -- never sorted on, for exactly the reason `modified_at` exists.
    projected_at    INTEGER NOT NULL,
    -- What the projected nodes hash to. Compared against the next projection's
    -- so a re-projection that changed nothing does not move `modified_at`.
    content_digest  BLOB,
    -- **When this document's projected content last moved.** The honest answer
    -- to a sort by modification, and the reason `projected_at` is not it: a
    -- sweep, a recovery or a rebuild re-projects documents nobody edited.
    -- Written from the *store's* durable per-document stamp, which is what
    -- carries the order through this database being dropped and rebuilt.
    modified_at     INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS documents_workspace ON documents(workspace_id);

CREATE TABLE IF NOT EXISTS source_status (
    document_id     TEXT PRIMARY KEY,
    workspace_id    TEXT NOT NULL,
    source_revision BLOB NOT NULL,
    found           INTEGER NOT NULL,
    understood      INTEGER NOT NULL,
    checked_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS source_status_workspace ON source_status(workspace_id);

-- **Keyed by (document, node), never node alone.** A `NodeId` means nothing
-- outside the document that minted it, and treating one as globally unique is
-- the single richest source of bugs in this codebase.
CREATE TABLE IF NOT EXISTS nodes (
    document_id TEXT NOT NULL,
    node_id     TEXT NOT NULL,
    -- Document order, which (parent, ordinal) cannot reconstruct without a
    -- walk. A consumer wanting the order a person reads asks for this.
    seq         INTEGER NOT NULL,
    parent_id   TEXT,
    ordinal     INTEGER NOT NULL,
    depth       INTEGER NOT NULL,
    kind        TEXT NOT NULL,
    text        TEXT NOT NULL,
    heading     INTEGER,
    list_style  TEXT,
    done        INTEGER NOT NULL,
    level_name  TEXT,
    name        TEXT,
    icon        TEXT,
    colour      TEXT,
    language    TEXT,
    blob        TEXT,
    mime        TEXT,
    embed       TEXT,
    -- **The node is in this document's own bin** — the hidden `Kind::Trash`
    -- root, or something under it. Not the same question as `listing.trashed_at`,
    -- which is a binned *document*; see the module note. Stored rather than
    -- omitted, because the bin is a place somebody searches and restores from;
    -- left out of every query that means live content.
    in_trash    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (document_id, node_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS nodes_document ON nodes(document_id);
CREATE INDEX IF NOT EXISTS nodes_kind ON nodes(kind);

CREATE TABLE IF NOT EXISTS runs (
    document_id TEXT NOT NULL,
    node_id     TEXT NOT NULL,
    start       INTEGER NOT NULL,
    end         INTEGER NOT NULL,
    bold        INTEGER NOT NULL,
    italic      INTEGER NOT NULL,
    underline   INTEGER NOT NULL,
    strike      INTEGER NOT NULL,
    code        INTEGER NOT NULL,
    highlight   TEXT,
    script      TEXT,
    pill        TEXT
);
CREATE INDEX IF NOT EXISTS runs_node ON runs(document_id, node_id);

CREATE TABLE IF NOT EXISTS attrs (
    document_id TEXT NOT NULL,
    node_id     TEXT NOT NULL,
    attr_id     TEXT NOT NULL,
    role        TEXT NOT NULL,
    kind        TEXT NOT NULL,
    value       TEXT NOT NULL,
    -- The IANA zone a `datetime_zoned` value was meant in; NULL for every
    -- other kind. Beside the instant rather than dropped, because an instant
    -- with no zone is the ambiguous shape the three temporal types abolish
    -- (ADR-183).
    zone        TEXT,
    label       TEXT
);
CREATE INDEX IF NOT EXISTS attrs_node ON attrs(document_id, node_id);
-- **The covering index the tasks query is built on** (ADR-274, P-Base §5).
--
-- Every task predicate is a correlated `EXISTS` over this table, correlated on
-- `(document_id, node_id)` and then testing `role`, usually `value` and
-- sometimes `kind` — `attr_exists` in this file is the one builder, so there is
-- exactly one shape to serve. With `attrs_node` alone SQLite could seek the
-- node's attribute rows and then had to read each **table row** to test the
-- rest, which is a random row read per attribute per candidate node, and the
-- candidate set is the workspace. `EXPLAIN QUERY PLAN` said so in words:
-- `SEARCH a USING INDEX attrs_node (document_id=? AND node_id=?)` at most of the
-- probes, and — worse, and not what the sizing had assumed — `SEARCH a USING
-- INDEX attrs_value (role=? AND value=?)` wherever the lifecycle test appears as
-- a *column* rather than a filter, which scans **every completed row in the
-- corpus** once per candidate node. That second shape is O(corpus) inside a loop
-- over the workspace, and it is the missing account of the super-linearity
-- P-Base measured and could not explain.
--
-- With `role, value, kind` carried along, the same probes read
-- `SEARCH a USING COVERING INDEX attrs_probe (document_id=? AND node_id=? AND
-- role=? AND value=?)` and no table row is touched at all. Measured on the
-- roadmap corpus, one workspace: a page of 100 falls 220 ms -> 14 ms, the rail's
-- five counts 407 ms -> 21 ms, and a Tasks tab's three statements together
-- 830 ms -> 43 ms. The warm page also stopped being super-linear in the corpus
-- (exponent 1.33 -> 0.78), which the sizing had not expected and which is the
-- more valuable half: the constant came down *and* the curve flattened.
--
-- The column order is the constraint order: the two correlated columns lead
-- because they are equality-bound on every probe without exception, `role`
-- follows because every probe has one, `value` next because the lifecycle test
-- (three quarters of a Tasks page) is an equality on it, and `kind` last
-- because only the due arms read it and they read it after a range on `value`.
--
-- `attrs_node` stays beside it rather than being folded in: it is this index's
-- own prefix, but it is also what the per-row attribute fetch uses, and that
-- one wants `attr_id`, `zone` and `label` off the table in `rowid` order.
CREATE INDEX IF NOT EXISTS attrs_probe ON attrs(document_id, node_id, role, value, kind);
-- The join a view wants: every due date, every owner, without opening anything.
CREATE INDEX IF NOT EXISTS attrs_role ON attrs(role, kind);
-- **The one a GROUP BY on the value can use.** `attrs_role` is `(role, kind)`,
-- and every `koru.tag` row shares one kind — so counting a corpus by tag would
-- scan the table. This is the same key with the *value* second.
CREATE INDEX IF NOT EXISTS attrs_value ON attrs(role, value);

-- **The workspace's listing, mirrored.** One row per document, so a rename is
-- one row — which is what makes this safe to carry when denormalizing the same
-- title onto every *node* row would not be.
CREATE TABLE IF NOT EXISTS listing (
    document_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    parent_id   TEXT,
    ordinal     INTEGER NOT NULL,
    title       TEXT NOT NULL,
    kind        TEXT NOT NULL,
    mime        TEXT,
    -- Binned, not deleted. The content stays and so does the projection.
    trashed_at  INTEGER,
    trashed_by  TEXT
);
CREATE INDEX IF NOT EXISTS listing_workspace ON listing(workspace_id);

CREATE TABLE IF NOT EXISTS workspaces (
    workspace_id   TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    -- What the listing hashes to, so a consumer reconciles by comparison
    -- rather than by having been told.
    listing_digest BLOB NOT NULL,
    listed_at      INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// What a listing hashes to.
///
/// Every field a consumer can see, fed to the hasher separately — `str`'s own
/// `Hash` terminates each one, so two adjacent titles cannot be rearranged into
/// the same bytes and a rename go unnoticed.
///
/// `DefaultHasher` rather than a cryptographic digest: this answers *did this
/// change*, not *is this authentic*, and the alternative is a dependency. Its
/// one property worth knowing is that the value is fixed within a Rust version
/// but not promised across them — an upgrade costs one spurious listing
/// refresh, which is a refresh of derived state and therefore free.
/// What one document's projected content hashes to.
///
/// **The whole of what a consumer can see** — structure, kind, text, runs and
/// attrs — so a change to any of it moves the digest and therefore
/// `modified_at`, and a re-projection that reproduced the same rows moves
/// neither. Nothing outside the nodes goes in: the source revision moves
/// whenever any peer touched the document, including a change that projected
/// identically, and the workspace moves on a transfer that edited nothing.
///
/// Same hasher and same reasoning as [`listing_digest`]: this answers *did this
/// change*, not *is this authentic*. A `DefaultHasher` value is not promised
/// across Rust versions, which costs one spurious `modified_at` stamp per
/// document on the first projection after a toolchain upgrade — a derived
/// order, briefly wrong, and never a lost row.
fn content_digest(nodes: &[ProjectedNode]) -> Vec<u8> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for node in nodes {
        node.hash(&mut hasher);
    }
    hasher.finish().to_le_bytes().to_vec()
}

fn listing_digest(name: &str, documents: &[ListedDocument]) -> Vec<u8> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    for document in documents {
        document.document.to_storage().hash(&mut hasher);
        document.parent.map(|p| p.to_storage()).hash(&mut hasher);
        document.ordinal.hash(&mut hasher);
        document.title.hash(&mut hasher);
        document.kind.hash(&mut hasher);
        document.mime.hash(&mut hasher);
        document.trashed_at.hash(&mut hasher);
        document.trashed_by.hash(&mut hasher);
    }
    hasher.finish().to_le_bytes().to_vec()
}

#[cfg(test)]
mod tests;
