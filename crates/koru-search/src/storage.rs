// SPDX-License-Identifier: MPL-2.0

//! `search.db` — the index on disk.
//!
//! At the **store root**, beside `projection.db` and `catalog.db`. Not in the
//! app's cache directory: the store root is already per user and per channel,
//! which is the isolation the old `default_index_path` was approximating with
//! `directories`.
//!
//! ## One writer per row
//!
//! [`set_listing`](SearchIndex::set_listing) owns the workspace's own name
//! fragment and one title fragment per document;
//! [`replace_document`](SearchIndex::replace_document) owns that document's
//! node fragments. Neither touches the other's rows, which is what lets a
//! rename change a title without re-indexing and a document be re-indexed
//! without losing the title of a document nobody has opened.
//!
//! ## What it does not store
//!
//! No breadcrumb, no workspace name, no document title on a fragment row. Those
//! are assembled at query time from the mirrored [`listing`](Self::set_listing),
//! which is what makes a rename **one row** instead of a rewrite of every
//! descendant — and what deleted the forty lines of
//! `substr(breadcrumb,1,length(?)+3)` string surgery the old index needed to
//! repair itself after one.
//!
//! No `profile_id`, either. A profile is an authorization scope over
//! workspaces, resolved before a query gets here.
//!
//! ## A schema bump is a rebuild
//!
//! Same rule as the projection, one layer down, and for the same reason: this
//! data is derived. If the stored [`SEARCH_SCHEMA_VERSION`] differs, drop
//! everything and index the projection again. There is no migration engine and
//! there should not be one.
//!
//! ## The FTS table is external-content
//!
//! `fragment_fts` mirrors `fragments` through three triggers, and they are the
//! only thing keeping the two in step. **Delete through `fragments`, never
//! through `fragment_fts`** — a bulk delete that bypasses the triggers corrupts
//! the index with nothing to notice.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use koru_projection::model::ListedDocument;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value};
use sha2::{Digest, Sha256};

use crate::fuzzy::SharedVocabulary;
use crate::model::{
    AttrRow, Fragment, FuzzyIndexStats, IndexStats, Lifecycle, PreviewBlock, RecentQuery,
    SearchFacet, SearchFacets, SearchPage, SearchPreview, SearchQuery, SearchResult,
};
use crate::normalize::{normalize, term_counts};

/// The shape of everything below, **and what the text in it means**. A change
/// to either is a rebuild.
///
/// 4 is where `attr:koru.tag` fragments stopped carrying a `TagId` and started
/// carrying the tag's name (ADR-182). No column moved — which is exactly why
/// that was the bump that mattered: a schema check on shape alone would have
/// kept every existing index and quietly answered from ids nobody can type.
///
/// 5 adds [`attr_values`](self), which keeps every attribute in the **type it
/// was declared with** so a filter can compare dates and numbers rather than
/// only match words. Here a column really did move, and the rebuild is what
/// populates it: an older index has the fragments and none of the rows.
pub const SEARCH_SCHEMA_VERSION: i64 = 5;

/// The name in the log, so the file that ran is the file on disk.
const MIGRATION_NAME: &str = "013_search_attr_values_migration.sql";

const SEARCH_SCHEMA_MIGRATION: &str =
    include_str!("../migrations/013_search_attr_values_migration.sql");

/// One document as the index has it — the half of a listing row a query needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub document: String,
    pub workspace: String,
    pub parent: Option<String>,
    pub title: String,
    pub kind: String,
    pub mime: Option<String>,
    pub trashed_at: Option<i64>,
    pub trashed_by: Option<String>,
}

impl Listed {
    pub fn lifecycle(&self) -> Lifecycle {
        if self.trashed_at.is_some() {
            Lifecycle::Trashed
        } else {
            Lifecycle::Live
        }
    }
}

pub struct SearchIndex {
    pub(crate) connection: Connection,
    pub(crate) vocabulary: SharedVocabulary,
}

impl SearchIndex {
    /// Open `search.db` under a store root.
    pub fn open(store_root: &Path) -> SearchResult<Self> {
        Self::at(&store_root.join("search.db"))
    }

    pub fn at(path: &Path) -> SearchResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let index = Self::over(Connection::open(path)?, SharedVocabulary::warming())?;
        index.warm_fuzzy_vocabulary()?;
        Ok(index)
    }

    /// The writer and a reader, sharing one process-local fuzzy vocabulary.
    ///
    /// Two connections rather than one behind a mutex: an atomic replacement
    /// must never queue an interactive query behind it. WAL is what makes that
    /// safe, and the shared vocabulary is what keeps the two coherent.
    pub fn pair(store_root: &Path) -> SearchResult<(Self, Self)> {
        let path = store_root.join("search.db");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let vocabulary = SharedVocabulary::warming();
        let writer = Self::over(Connection::open(&path)?, vocabulary.clone())?;
        let reader = Self::over(Connection::open(&path)?, vocabulary)?;
        // The fuzzy vocabulary is warmed on the indexer thread, not here: a
        // large index takes long enough to rebuild from `fragments` that
        // blocking the store's startup on it exceeds the gateway's readiness
        // deadline. Queries during the warm simply have no typo suggestions.
        Ok((writer, reader))
    }

    /// [`Self::pair`] in this process's memory: the writer and a reader of one
    /// named database, with no file written for it.
    pub fn pair_in_memory(name: &koru_sqlite::MemoryName) -> SearchResult<(Self, Self)> {
        let vocabulary = SharedVocabulary::warming();
        let writer = Self::over(koru_sqlite::open_memory(name)?, vocabulary.clone())?;
        let reader = Self::over(koru_sqlite::open_memory(name)?, vocabulary)?;
        Ok((writer, reader))
    }

    /// In memory, for tests.
    pub fn in_memory() -> SearchResult<Self> {
        Self::over(Connection::open_in_memory()?, SharedVocabulary::ready())
    }

    fn over(connection: Connection, vocabulary: SharedVocabulary) -> SearchResult<Self> {
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        // `NORMAL`, not `FULL`: this is a rebuildable cache, and paying an
        // fsync per write to protect something the projection can regenerate is
        // paying for durability nobody needs. The store's own database is where
        // `FULL` belongs and is where it is.
        connection.pragma_update(None, "synchronous", "NORMAL")?;

        let found: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if found != 0 && found != SEARCH_SCHEMA_VERSION {
            let started = std::time::Instant::now();
            tracing::info!(
                database = "search.db",
                found,
                expected = SEARCH_SCHEMA_VERSION,
                migration = MIGRATION_NAME,
                "applying SQL migration; the derived search index will be rebuilt"
            );
            connection.execute_batch(SEARCH_SCHEMA_MIGRATION)?;
            tracing::info!(
                database = "search.db",
                from = found,
                to = SEARCH_SCHEMA_VERSION,
                migration = MIGRATION_NAME,
                elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                "SQL migration applied"
            );
        }
        connection.execute_batch(SCHEMA)?;
        connection.pragma_update(None, "user_version", SEARCH_SCHEMA_VERSION)?;
        Ok(Self {
            connection,
            vocabulary,
        })
    }

    // ------------------------------------------------------------- the fuzzy

    pub fn fuzzy_stats(&self) -> FuzzyIndexStats {
        self.vocabulary.stats()
    }

    pub fn compact_fuzzy_vocabulary(&self) -> bool {
        self.vocabulary.compact_if_needed(now_ms())
    }

    pub fn warm_fuzzy_vocabulary(&self) -> SearchResult<()> {
        let started = std::time::Instant::now();
        let mut statement = self.connection.prepare("SELECT text FROM fragments")?;
        let texts = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        tracing::info!(
            fragments = texts.len(),
            elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
            "search: loaded fragments for fuzzy vocabulary"
        );
        self.vocabulary
            .write()
            .replace_all(term_counts(texts.iter().map(String::as_str)), now_ms());
        tracing::info!(
            elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
            "search: fuzzy vocabulary warmed"
        );
        Ok(())
    }

    // ------------------------------------------------------------- cursors

    /// What the index consumed for each document — its own cursor, compared
    /// against the projection's `source_revision` to find what it is behind on.
    ///
    /// **Equal is current; everything else is work.** A recovered document's
    /// revision can go *backwards*, so a rule of "only a newer revision may
    /// replace an older one" would leave it stale for good.
    pub fn cursors(&self) -> SearchResult<HashMap<String, Vec<u8>>> {
        let mut q = self
            .connection
            .prepare("SELECT document_id, indexed_revision FROM documents")?;
        let rows = q.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
    }

    /// The listing digest consumed for each workspace, in the same idiom.
    pub fn digests(&self) -> SearchResult<HashMap<String, Vec<u8>>> {
        let mut q = self
            .connection
            .prepare("SELECT workspace_id, listing_digest FROM workspaces")?;
        let rows = q.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
    }

    pub fn indexed_documents(&self) -> SearchResult<usize> {
        Ok(self
            .connection
            .query_row("SELECT COUNT(*) FROM documents", [], |r| r.get::<_, i64>(0))?
            as usize)
    }

    pub fn fragment_count(&self) -> SearchResult<usize> {
        Ok(self
            .connection
            .query_row("SELECT COUNT(*) FROM fragments", [], |r| r.get::<_, i64>(0))?
            as usize)
    }

    // ------------------------------------------------------------- listing

    /// Mirror one workspace's listing, and its name.
    ///
    /// This is where a rename lands, and it costs one row per document plus one
    /// fragment for the workspace name. **No document is re-indexed**: a
    /// breadcrumb is assembled from these rows at query time, so every affected
    /// path is already right when this returns.
    ///
    /// **A document that leaves the listing leaves the index.** *Empty trash*
    /// removes the manifest entry while the store keeps the bytes, so the
    /// projector keeps the document and its node fragments would stay —
    /// answering *live* queries through the lifecycle join's NULL row. Rows of
    /// a document the new listing no longer names go with the listing.
    pub fn set_listing(
        &mut self,
        workspace: &str,
        name: &str,
        digest: &[u8],
        documents: &[ListedDocument],
    ) -> SearchResult<()> {
        // Everything on the listing path, replaced together: the workspace's
        // own name and every document's title.
        let mut old = self.texts_where(LISTING_OWNED, params![workspace])?;
        let previous: HashSet<String> = {
            let mut stmt = self
                .connection
                .prepare("SELECT document_id FROM listing WHERE workspace_id = ?1")?;
            let rows = stmt.query_map(params![workspace], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?.into_iter().collect()
        };
        let kept: HashSet<String> = documents
            .iter()
            .map(|document| document.document.to_storage())
            .collect();
        let vanished: Vec<String> = previous.difference(&kept).cloned().collect();
        // A vanished document's remaining rows are node-owned, so the listing
        // delta does not cover them; fold their terms into the removal. Node-
        // owned only: the title fragment is listing-owned and already counted.
        for gone in &vanished {
            old.extend(self.texts_where(NODE_OWNED, params![gone])?);
        }
        let new: Vec<String> = std::iter::once(name.to_owned())
            .chain(documents.iter().map(|d| d.title.clone()))
            .collect();
        let delta = term_delta(
            term_counts(old.iter().map(String::as_str)),
            term_counts(new.iter().map(String::as_str)),
        );

        let tx = self.connection.transaction()?;
        tx.execute(
            "DELETE FROM listing WHERE workspace_id = ?1",
            params![workspace],
        )?;
        for gone in &vanished {
            for sql in [
                "DELETE FROM fragments WHERE document_id = ?1",
                "DELETE FROM documents WHERE document_id = ?1",
                "DELETE FROM recent_targets WHERE document_id = ?1",
            ] {
                tx.execute(sql, params![gone])?;
            }
        }
        {
            let mut stmt = tx.prepare(
                "INSERT INTO listing (document_id, workspace_id, parent_id, ordinal, title,
                                      kind, mime, trashed_at, trashed_by)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            )?;
            for document in documents {
                stmt.execute(params![
                    document.document.to_storage(),
                    workspace,
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
            "INSERT INTO workspaces (workspace_id, name, listing_digest)
             VALUES (?1,?2,?3)
             ON CONFLICT(workspace_id) DO UPDATE SET
                 name = excluded.name, listing_digest = excluded.listing_digest",
            params![workspace, name, digest],
        )?;
        tx.execute(
            &format!("DELETE FROM fragments WHERE {LISTING_OWNED}"),
            params![workspace],
        )?;
        // The workspace's own name, so it stays matchable.
        if let Some(fragment) = crate::fragments::workspace_fragment(workspace, name) {
            insert_fragment(&tx, &fragment)?;
        }
        // And one title per document — **including documents with no content at
        // all**, which is how a document created and never opened is findable.
        for document in documents {
            if let Some(fragment) = crate::fragments::title_fragment(
                workspace,
                &document.document.to_storage(),
                &document.title,
            ) {
                insert_fragment(&tx, &fragment)?;
            }
        }
        let mut vocabulary = self.vocabulary.write();
        tx.commit()?;
        vocabulary.apply_delta(delta);
        Ok(())
    }

    /// A workspace the store no longer holds.
    pub fn remove_workspace(&mut self, workspace: &str) -> SearchResult<()> {
        let texts = self.texts_where("workspace_id = ?1", params![workspace])?;
        let delta = removal_delta(&texts);
        let tx = self.connection.transaction()?;
        for sql in [
            "DELETE FROM fragments WHERE workspace_id = ?1",
            "DELETE FROM documents WHERE workspace_id = ?1",
            "DELETE FROM listing WHERE workspace_id = ?1",
            "DELETE FROM workspaces WHERE workspace_id = ?1",
            "DELETE FROM recent_targets WHERE workspace_id = ?1",
        ] {
            tx.execute(sql, params![workspace])?;
        }
        let mut vocabulary = self.vocabulary.write();
        tx.commit()?;
        vocabulary.apply_delta(delta);
        Ok(())
    }

    /// Every document the index has a listing row for, in one workspace.
    pub fn listed_in(&self, workspace: &str) -> SearchResult<Vec<Listed>> {
        self.listing_rows("l.workspace_id = ?1", params![workspace])
    }

    pub fn listed(&self, document: &str) -> SearchResult<Option<Listed>> {
        Ok(self
            .listing_rows("l.document_id = ?1", params![document])?
            .pop())
    }

    fn listing_rows(
        &self,
        where_clause: &str,
        args: impl rusqlite::Params,
    ) -> SearchResult<Vec<Listed>> {
        let mut q = self.connection.prepare(&format!(
            "SELECT l.document_id, l.workspace_id, l.parent_id, l.title, l.kind, l.mime,
                    l.trashed_at, l.trashed_by
             FROM listing l WHERE {where_clause} ORDER BY l.ordinal, l.document_id"
        ))?;
        let rows = q.query_map(args, |r| {
            Ok(Listed {
                document: r.get(0)?,
                workspace: r.get(1)?,
                parent: r.get(2)?,
                title: r.get(3)?,
                kind: r.get(4)?,
                mime: r.get(5)?,
                trashed_at: r.get(6)?,
                trashed_by: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub(crate) fn workspace_names(&self) -> SearchResult<HashMap<String, String>> {
        let mut q = self
            .connection
            .prepare("SELECT workspace_id, name FROM workspaces")?;
        let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
    }

    /// Every listing row for the workspaces a page touched, keyed by document.
    ///
    /// **The whole of how a breadcrumb is built.** A workspace's listing is
    /// documents, not nodes, so this is small; walking `parent` through it in
    /// memory is cheaper than a recursive CTE and far easier to read.
    pub(crate) fn listing_of(
        &self,
        workspaces: &[String],
    ) -> SearchResult<HashMap<String, Listed>> {
        if workspaces.is_empty() {
            return Ok(HashMap::new());
        }
        let marks = std::iter::repeat_n("?", workspaces.len())
            .collect::<Vec<_>>()
            .join(",");
        let rows = self.listing_rows(
            &format!("l.workspace_id IN ({marks})"),
            params_from_iter(workspaces.iter().map(|w| Value::Text(w.clone()))),
        )?;
        Ok(rows
            .into_iter()
            .map(|row| (row.document.clone(), row))
            .collect())
    }

    // ----------------------------------------------------------- documents

    /// Replace one document's fragments, whole, in one transaction.
    ///
    /// **The unit of work.** Old rows stay queryable until this commits, so a
    /// search never sees a half-indexed document, and no other document is read
    /// or written.
    /// `values` are the typed attribute rows for the same nodes, replaced in
    /// the same transaction as the fragments — they describe one document, and
    /// a crash between two writes would leave a filter answering from
    /// attributes the text no longer has.
    pub fn replace_document(
        &mut self,
        workspace: &str,
        document: &str,
        fragments: &[Fragment],
        values: &[AttrRow],
        revision: &[u8],
        at_millis: i64,
    ) -> SearchResult<()> {
        let old = self.texts_where(NODE_OWNED, params![document])?;
        let delta = term_delta(
            term_counts(old.iter().map(String::as_str)),
            term_counts(fragments.iter().map(|f| f.text.as_str())),
        );
        let fingerprint = fingerprint(fragments);
        let tx = self.connection.transaction()?;
        // The title belongs to the listing and is not this writer's to remove.
        tx.execute(
            &format!("DELETE FROM fragments WHERE {NODE_OWNED}"),
            params![document],
        )?;
        for fragment in fragments {
            insert_fragment(&tx, fragment)?;
        }
        // The typed rows are wholly this writer's — unlike the fragments,
        // where the title belongs to the listing — so the delete is unqualified.
        tx.execute(
            "DELETE FROM attr_values WHERE document_id = ?1",
            params![document],
        )?;
        for value in values {
            insert_attr_value(&tx, value)?;
        }
        // **`observed_modified_at_ms` moves only when the text did.** It answers
        // *when did this document's searchable content last change*, not *when
        // did we last look* — a re-projection that changed nothing must not
        // send a document to the top of a recently-modified sort.
        tx.execute(
            "INSERT INTO documents (document_id, workspace_id, indexed_revision, fingerprint,
                                    observed_modified_at_ms, indexed_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(document_id) DO UPDATE SET
                 workspace_id = excluded.workspace_id,
                 indexed_revision = excluded.indexed_revision,
                 fingerprint = excluded.fingerprint,
                 observed_modified_at_ms = CASE
                     WHEN documents.fingerprint <> excluded.fingerprint
                     THEN excluded.observed_modified_at_ms
                     ELSE documents.observed_modified_at_ms END,
                 indexed_at_ms = excluded.indexed_at_ms",
            params![
                document,
                workspace,
                revision,
                fingerprint,
                at_millis,
                at_millis
            ],
        )?;
        let mut vocabulary = self.vocabulary.write();
        tx.commit()?;
        vocabulary.apply_delta(delta);
        Ok(())
    }

    /// A document the store dropped. **Not a document in the trash** — that one
    /// keeps its rows and is found under the trash filter, because the bin is a
    /// place somebody searches and restores from.
    pub fn remove_document(&mut self, document: &str) -> SearchResult<()> {
        let texts = self.texts_where("document_id = ?1", params![document])?;
        let delta = removal_delta(&texts);
        let tx = self.connection.transaction()?;
        tx.execute(
            "DELETE FROM fragments WHERE document_id = ?1",
            params![document],
        )?;
        tx.execute(
            "DELETE FROM attr_values WHERE document_id = ?1",
            params![document],
        )?;
        tx.execute(
            "DELETE FROM documents WHERE document_id = ?1",
            params![document],
        )?;
        tx.execute(
            "DELETE FROM listing WHERE document_id = ?1",
            params![document],
        )?;
        tx.execute(
            "DELETE FROM recent_targets WHERE document_id = ?1",
            params![document],
        )?;
        let mut vocabulary = self.vocabulary.write();
        tx.commit()?;
        vocabulary.apply_delta(delta);
        Ok(())
    }

    fn texts_where(
        &self,
        where_clause: &str,
        args: impl rusqlite::Params,
    ) -> SearchResult<Vec<String>> {
        let mut q = self
            .connection
            .prepare(&format!("SELECT text FROM fragments WHERE {where_clause}"))?;
        let rows = q.query_map(args, |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // -------------------------------------------------------------- reading

    /// A query with nothing in it is what a surface asks on the first keystroke
    /// of an empty box, and the useful answer is the recent queries rather than
    /// an empty page. **A tag filter counts as something in it**: a surface may
    /// carry one without a word beside it, and answering that with recents
    /// would drop a filter the person set.
    pub fn query(&self, query: &SearchQuery, updating: &[String]) -> SearchResult<SearchPage> {
        // **An empty box asks for recents; a filter is not an empty box.**
        // `attributes` belongs in this test and was missing from it. A typed
        // filter is stripped out of the text by the surface — `/due:after
        // 2026-08-29` leaves nothing behind — so *what is due this month*
        // arrived here looking exactly like a box nobody had typed in, and was
        // answered with the recently-opened documents. It is the worst shape a
        // wrong answer can take: plausible rows, no error, and they change when
        // you open something else rather than when you change the filter.
        if normalize(query.text.trim()).text.is_empty()
            && query.tags.is_empty()
            && query.attributes.is_empty()
        {
            return self.recents(query, updating);
        }
        crate::query::search(self, query, updating)
    }

    pub fn facets(&self) -> SearchResult<SearchFacets> {
        let mut q = self.connection.prepare(
            "SELECT workspace_id, name FROM workspaces ORDER BY name COLLATE NOCASE, workspace_id",
        )?;
        let workspaces = q
            .query_map([], |r| {
                Ok(SearchFacet {
                    workspace: r.get(0)?,
                    workspace_name: r.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SearchFacets { workspaces })
    }

    /// A window of one document in reading order, for a side preview.
    ///
    /// The matched node is centred where possible; a title match starts at the
    /// first body block rather than repeating the title.
    pub fn preview(&self, document: &str, matched: Option<&str>) -> SearchResult<SearchPreview> {
        let title = self
            .listed(document)?
            .map(|listed| listed.title)
            .unwrap_or_default();
        let from: i64 = matched
            .and_then(|node| {
                self.connection
                    .query_row(
                        "SELECT MIN(seq) FROM fragments WHERE document_id=?1 AND node_id=?2",
                        params![document, node],
                        |r| r.get::<_, Option<i64>>(0),
                    )
                    .optional()
                    .ok()
                    .flatten()
                    .flatten()
            })
            .map(|seq| (seq - 1).max(1))
            .unwrap_or(1);
        let mut q = self.connection.prepare(
            "SELECT node_id, kind, field, text FROM fragments
             WHERE document_id=?1 AND field<>'title' AND seq>=?2
               AND stable_key LIKE 'node:%:primary'
             ORDER BY seq LIMIT 12",
        )?;
        let blocks = q
            .query_map(params![document, from], |r| {
                let node: Option<String> = r.get(0)?;
                Ok(PreviewBlock {
                    matched: matched.is_some() && matched == node.as_deref(),
                    node,
                    kind: r.get(1)?,
                    field: r.get(2)?,
                    text: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SearchPreview { title, blocks })
    }

    // -------------------------------------------------------------- recents

    /// Record navigation. Machine-local, derived, and never written into a
    /// shared profile or a collaborative document: it describes this person's
    /// desktop activity.
    pub fn record_open(
        &self,
        workspace: &str,
        document: Option<&str>,
        node: Option<&str>,
        opened_at_ms: i64,
    ) -> SearchResult<()> {
        let key = format!(
            "{workspace}/{}/{}",
            document.unwrap_or("@workspace"),
            node.unwrap_or("@document")
        );
        self.connection.execute(
            "INSERT INTO recent_targets(target_key, workspace_id, document_id, node_id, opened_at_ms)
             VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(target_key) DO UPDATE SET opened_at_ms=excluded.opened_at_ms",
            params![key, workspace, document, node, opened_at_ms],
        )?;
        Ok(())
    }

    /// Record a query only once it led somewhere.
    pub fn record_query(&self, text: &str, used_at_ms: i64) -> SearchResult<()> {
        let text = text.trim();
        let normalized = normalize(text).text;
        if normalized.is_empty() {
            return Ok(());
        }
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO recent_queries(normalized, text, used_at_ms) VALUES (?1,?2,?3)
             ON CONFLICT(normalized) DO UPDATE SET
                 text=excluded.text, used_at_ms=excluded.used_at_ms",
            params![normalized, text, used_at_ms],
        )?;
        tx.execute(
            "DELETE FROM recent_queries WHERE normalized NOT IN (
                 SELECT normalized FROM recent_queries ORDER BY used_at_ms DESC LIMIT 20)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn recent_queries(&self, limit: usize) -> SearchResult<Vec<RecentQuery>> {
        let mut q = self.connection.prepare(
            "SELECT text, used_at_ms FROM recent_queries ORDER BY used_at_ms DESC LIMIT ?1",
        )?;
        let rows = q.query_map(params![limit.clamp(1, 20) as i64], |r| {
            Ok(RecentQuery {
                text: r.get(0)?,
                used_at_ms: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// What an empty query answers: where this person has been.
    fn recents(&self, query: &SearchQuery, updating: &[String]) -> SearchResult<SearchPage> {
        let mut values = Vec::<Value>::new();
        let mut scope = String::new();
        if !query.workspaces.is_empty() {
            scope.push_str(" AND r.workspace_id IN (");
            for (i, workspace) in query.workspaces.iter().enumerate() {
                if i > 0 {
                    scope.push(',');
                }
                values.push(Value::Text(workspace.to_storage()));
                scope.push_str(&format!("?{}", values.len()));
            }
            scope.push(')');
        }
        for workspace in &query.excluded_workspaces {
            values.push(Value::Text(workspace.to_storage()));
            scope.push_str(&format!(" AND r.workspace_id<>?{}", values.len()));
        }
        let page_size = query.page_size.clamp(1, 200);
        values.push(Value::Integer(page_size as i64 + 1));
        let limit = values.len();
        values.push(Value::Integer(query.offset as i64));
        let offset = values.len();
        let sql = format!(
            "SELECT r.workspace_id, r.document_id, r.node_id
             FROM recent_targets r WHERE 1=1 {scope}
             ORDER BY r.opened_at_ms DESC LIMIT ?{limit} OFFSET ?{offset}"
        );
        let mut q = self.connection.prepare(&sql)?;
        let targets = q
            .query_map(params_from_iter(values), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(q);

        let listing =
            self.listing_of(&targets.iter().map(|(w, ..)| w.clone()).collect::<Vec<_>>())?;
        let names = self.workspace_names()?;
        let mut hits = Vec::new();
        for (workspace, document, node) in targets {
            let listed = document.as_ref().and_then(|d| listing.get(d));
            let lifecycle = listed.map(Listed::lifecycle).unwrap_or_default();
            if !crate::query::wanted(query.lifecycle, lifecycle) {
                continue;
            }
            let name = names.get(&workspace).cloned().unwrap_or_default();
            let title = listed
                .map(|l| l.title.clone())
                .unwrap_or_else(|| name.clone());
            hits.push(crate::model::SearchHit {
                breadcrumb: crate::query::breadcrumb(&name, listed, &listing),
                workspace_name: name,
                workspace,
                document,
                node,
                document_kind: listed.map(|l| l.kind.clone()).unwrap_or_default(),
                mime: listed.and_then(|l| l.mime.clone()),
                kind: listed.map(|l| l.kind.clone()).unwrap_or("workspace".into()),
                field: listed
                    .map(|_| crate::model::field::TITLE.into())
                    .unwrap_or(crate::model::field::WORKSPACE.into()),
                snippet: title.clone(),
                title,
                breadcrumb_spans: Vec::new(),
                spans: Vec::new(),
                lifecycle,
                trashed_at: listed.and_then(|l| l.trashed_at),
                trashed_by: listed.and_then(|l| l.trashed_by.clone()),
                score: 0.0,
                observed_modified_at_ms: None,
                workspace_last_used_at_ms: None,
            });
        }
        let has_more = hits.len() > page_size;
        hits.truncate(page_size);
        Ok(SearchPage {
            hits,
            next_offset: has_more.then_some(query.offset + page_size),
            updating_workspaces: updating.to_vec(),
            request_seq: query.request_seq,
            corrected_query: None,
        })
    }

    // ---------------------------------------------------------------- stats

    pub fn stats(&self) -> IndexStats {
        IndexStats {
            schema_version: SEARCH_SCHEMA_VERSION,
            documents: self.indexed_documents().unwrap_or(0),
            fragments: self.fragment_count().unwrap_or(0),
            ..IndexStats::default()
        }
    }
}

/// **A plain `INSERT`, never `INSERT OR REPLACE`.** With recursive triggers off
/// — SQLite's default — a `REPLACE` conflict deletes the old row *without*
/// firing the `AFTER DELETE` trigger, so `fragment_fts` keeps a row for text
/// nothing holds and the index rots with nothing to notice. Every caller
/// deletes the rows it is about to replace, so a conflict here is a bug and
/// should say so.
fn insert_fragment(tx: &rusqlite::Transaction<'_>, fragment: &Fragment) -> SearchResult<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT INTO fragments
             (stable_key, workspace_id, document_id, node_id, seq, kind, field, text, normalized)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
    )?;
    stmt.execute(params![
        fragment.key,
        fragment.workspace,
        fragment.document,
        fragment.node,
        fragment.seq,
        fragment.kind,
        fragment.field,
        fragment.text,
        normalize(&fragment.text).text,
    ])?;
    Ok(())
}

/// One typed attribute row.
///
/// **`value` is normalized the way a fragment's text is**, so an equality
/// filter folds case and accents exactly as the text search does — `/priority
/// HIGH` and `/priority high` are one question, and a select token typed in
/// another script matches the way its words do. The ISO forms are unaffected
/// by folding, so date ordering is untouched.
fn insert_attr_value(tx: &rusqlite::Transaction<'_>, value: &AttrRow) -> SearchResult<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT INTO attr_values
             (workspace_id, document_id, node_id, role, kind, value, num, zone)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
    )?;
    stmt.execute(params![
        value.workspace,
        value.document,
        value.node,
        value.role,
        value.kind,
        normalize(&value.value).text,
        value.num,
        value.zone,
    ])?;
    Ok(())
}

/// Rows the listing path owns: the workspace's own name, and every title.
const LISTING_OWNED: &str =
    "workspace_id = ?1 AND (document_id IS NULL OR stable_key LIKE 'doc:%')";

/// Rows the document path owns: everything the node walk produced.
const NODE_OWNED: &str = "document_id = ?1 AND stable_key LIKE 'node:%'";

fn term_delta(old: HashMap<String, u64>, new: HashMap<String, u64>) -> HashMap<String, i64> {
    let mut delta = HashMap::new();
    for (term, count) in old {
        *delta.entry(term).or_insert(0) -= count.min(i64::MAX as u64) as i64;
    }
    for (term, count) in new {
        *delta.entry(term).or_insert(0) += count.min(i64::MAX as u64) as i64;
    }
    delta.retain(|_, change| *change != 0);
    delta
}

fn removal_delta(texts: &[String]) -> HashMap<String, i64> {
    term_counts(texts.iter().map(String::as_str))
        .into_iter()
        .map(|(term, count)| (term, -(count.min(i64::MAX as u64) as i64)))
        .collect()
}

/// What "this document's searchable content changed" means.
fn fingerprint(fragments: &[Fragment]) -> Vec<u8> {
    let mut ordered: Vec<_> = fragments.iter().collect();
    ordered.sort_by(|a, b| a.key.cmp(&b.key));
    let mut digest = Sha256::new();
    for fragment in ordered {
        for value in [
            fragment.key.as_str(),
            fragment.field.as_str(),
            fragment.text.as_str(),
        ] {
            digest.update((value.len() as u64).to_le_bytes());
            digest.update(value.as_bytes());
        }
    }
    digest.finalize().to_vec()
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

const SCHEMA: &str = "
-- The index's own cursor: which projection revision each document was built
-- from, and when its text last actually moved.
CREATE TABLE IF NOT EXISTS documents (
    document_id             TEXT PRIMARY KEY,
    workspace_id            TEXT NOT NULL,
    indexed_revision        BLOB NOT NULL,
    fingerprint             BLOB NOT NULL,
    observed_modified_at_ms INTEGER NOT NULL,
    indexed_at_ms           INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS documents_workspace ON documents(workspace_id);

-- The workspace listing, mirrored from the projection. Titles, parents and the
-- trash flag live **here and only here**: a breadcrumb is assembled from these
-- rows at query time, so a rename is one row rather than a rewrite of every
-- descendant's fragments.
CREATE TABLE IF NOT EXISTS listing (
    document_id  TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    parent_id    TEXT,
    ordinal      INTEGER NOT NULL,
    title        TEXT NOT NULL,
    kind         TEXT NOT NULL,
    mime         TEXT,
    trashed_at   INTEGER,
    trashed_by   TEXT
);
CREATE INDEX IF NOT EXISTS listing_workspace ON listing(workspace_id);

CREATE TABLE IF NOT EXISTS workspaces (
    workspace_id   TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    listing_digest BLOB NOT NULL
);

-- `document_id` is nullable because the workspace-name fragment belongs to no
-- document — which is also what keeps `DELETE … WHERE document_id = ?` from
-- ever touching it.
CREATE TABLE IF NOT EXISTS fragments (
    id           INTEGER PRIMARY KEY,
    stable_key   TEXT NOT NULL UNIQUE,
    workspace_id TEXT NOT NULL,
    document_id  TEXT,
    node_id      TEXT,
    seq          INTEGER NOT NULL,
    kind         TEXT NOT NULL,
    field        TEXT NOT NULL,
    text         TEXT NOT NULL,
    normalized   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS fragments_document ON fragments(document_id, seq);
CREATE INDEX IF NOT EXISTS fragments_workspace ON fragments(workspace_id, field);
-- The other fragments of one node. A `#tag` filter is a semi-join keyed on
-- exactly this pair, and without it every candidate's tag lookup was a scan.
-- **No version bump**: `IF NOT EXISTS` in a schema that runs on every open
-- means an existing index gains it in place, and an index is not content.
CREATE INDEX IF NOT EXISTS fragments_node ON fragments(document_id, node_id);

-- **Attributes in the type they were declared with**, beside the text
-- fragments rather than instead of them: a fragment answers *contains this
-- word*, a row here answers *before this date*, *above this number*, *is this
-- token*. Written by the same document walk, one row per attribute.
--
-- `value` is the canonical text — an ISO date, a select token, a reference id —
-- and `num` is the comparable number for the only two kinds that have one:
-- `number`, and `datetime_zoned` as epoch milliseconds. **There is deliberately
-- no column a `date` and a `datetime_zoned` could be compared through**, which
-- is ADR-189's rule about the three temporal types expressed as a table shape.
CREATE TABLE IF NOT EXISTS attr_values (
    id           INTEGER PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    document_id  TEXT NOT NULL,
    node_id      TEXT NOT NULL,
    role         TEXT NOT NULL,
    kind         TEXT NOT NULL,
    value        TEXT NOT NULL,
    num          REAL,
    zone         TEXT
);
-- The semi-join a filter makes: the other rows of one node.
CREATE INDEX IF NOT EXISTS attr_values_node ON attr_values(document_id, node_id);
-- And the one a filter narrows by before it gets there.
CREATE INDEX IF NOT EXISTS attr_values_role ON attr_values(workspace_id, role, kind);

CREATE TABLE IF NOT EXISTS recent_targets (
    target_key   TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    document_id  TEXT,
    node_id      TEXT,
    opened_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS recent_targets_opened ON recent_targets(opened_at_ms DESC);

CREATE TABLE IF NOT EXISTS recent_queries (
    normalized  TEXT PRIMARY KEY,
    text        TEXT NOT NULL,
    used_at_ms  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS recent_queries_used ON recent_queries(used_at_ms DESC);

CREATE TABLE IF NOT EXISTS state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS fragment_fts USING fts5(
    text, normalized, content='fragments', content_rowid='id',
    tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER IF NOT EXISTS fragments_ai AFTER INSERT ON fragments BEGIN
    INSERT INTO fragment_fts(rowid, text, normalized)
    VALUES (new.id, new.text, new.normalized);
END;
CREATE TRIGGER IF NOT EXISTS fragments_ad AFTER DELETE ON fragments BEGIN
    INSERT INTO fragment_fts(fragment_fts, rowid, text, normalized)
    VALUES ('delete', old.id, old.text, old.normalized);
END;
CREATE TRIGGER IF NOT EXISTS fragments_au AFTER UPDATE ON fragments BEGIN
    INSERT INTO fragment_fts(fragment_fts, rowid, text, normalized)
    VALUES ('delete', old.id, old.text, old.normalized);
    INSERT INTO fragment_fts(rowid, text, normalized)
    VALUES (new.id, new.text, new.normalized);
END;
";

#[cfg(test)]
mod tests;
