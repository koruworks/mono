// SPDX-License-Identifier: MPL-2.0

//! The search index — the read model's first consumer.
//!
//! ```text
//! koru-store-core     authoritative. Opaque bytes. No node model.
//!       │  Committed — a trigger, never truth
//!       ▼
//! koru-projection     the read model: nodes, runs, attrs, and the workspace's
//!       │             document listing
//!       │  Consumer::{projected, removed, listing_changed}
//!       ▼
//! koru-search         fragments, FTS5, a BK-tree, ranking. Disposable.
//! ```
//!
//! All three inside the `koru-store-server` process.
//!
//! ## What this crate does not know
//!
//! **There is no `koru-core` in its manifest and no type from it in its
//! source.** The projection is strings and primitives precisely so that a
//! consumer does not acquire an opinion about what a `Kind::ListBlock` is;
//! everything here works from `koru-projection`'s model and `rusqlite`.
//!
//! **There is no profile.** A profile is an authorization and query scope over
//! *workspaces*, resolved through the store's catalog before a query arrives.
//! An index that stored one would be storing a fact about who may look rather
//! than about what is written.
//!
//! ## Disposable, and enumerable
//!
//! Deleting `search.db` must be safe. It is rebuilt by comparing the index's
//! own cursors against the projection — the same comparison that catches a
//! document projected while the indexer was stopped, so the repair path and the
//! ordinary path are one piece of code.
//!
//! ## The obligation this crate carries
//!
//! **Ask for a sweep before trusting a workspace.** The projector follows the
//! store's laziness: a restarted store does not re-project a workspace until
//! something uses it. Nothing is stale that anybody has written to; what is
//! stale is a workspace nobody has touched since the store came up — which is
//! exactly the workspace a query is about to ask about. [`Search::query`] does
//! this. Skipping it is quiet: results from before the restart, and no error
//! anywhere.

pub mod fragments;
mod fuzzy;
pub mod lists;
pub mod model;
mod normalize;
mod query;
pub mod readings;
pub mod storage;
pub mod tags;
pub mod worker;

use std::path::Path;
use std::sync::Mutex;

use koru_projection::worker::Sweeps;
use koru_store_core::ids::WorkspaceId;

pub use lists::{ListNames, ListRow};
pub use model::{
    Fragment, FuzzyIndexState, FuzzyIndexStats, IndexStats, Lifecycle, LifecycleFilter, MatchSpan,
    PreviewBlock, ReadingCounts, RecentQuery, SearchError, SearchFacet, SearchFacets, SearchHit,
    SearchPage, SearchPreview, SearchQuery, SearchResult, SearchSort, field,
};
pub use readings::{NoReadings, ReadingText, Readings};
pub use storage::{Listed, SEARCH_SCHEMA_VERSION, SearchIndex};
pub use tags::{KIND_SYSTEM, TAG_ROLE, TagNames, TagTable};
pub use worker::{Feed, Indexer, Queue};

/// How long a query will wait for a workspace to be compared against the store
/// before answering anyway.
///
/// A healthy sweep is one SQLite read per document and opens no document at
/// all, so this is normally invisible. An unhealthy one must not become a hung
/// query — the answer names the workspace as updating and moves on.
pub const SWEEP_WAIT_MILLIS: u64 = 500;

/// The read side: a query, and the two handles that make its answer honest.
///
/// ## Why the mutex is in here rather than around the whole thing
///
/// A `rusqlite::Connection` is `Send` and not `Sync`, so serving a query from
/// more than one thread needs a lock somewhere. Putting it *outside* — an
/// `Arc<Mutex<Search>>` in shared state — is the obvious move and is quietly
/// wrong: [`Search::query`] waits up to [`SWEEP_WAIT_MILLIS`] for a cold
/// workspace, and an outer lock would make every other query wait behind that
/// too. Two cold queries would take a second between them, and the second one
/// would be paying for a sweep it did not ask for.
///
/// Held here, the lock covers the two moments that touch SQLite and nothing in
/// between. The wait happens unlocked, so queries overlap during it — and they
/// are waiting on the *same* sweeps, so the wait is shared rather than
/// repeated.
pub struct Search {
    index: Mutex<SearchIndex>,
    queue: Queue,
    sweeps: Sweeps,
}

impl Search {
    pub fn new(index: SearchIndex, queue: Queue, sweeps: Sweeps) -> Self {
        Self {
            index: Mutex::new(index),
            queue,
            sweeps,
        }
    }

    /// The index, for the caller that owns the writing half's tick loop.
    ///
    /// **Locks.** A caller holding this across a query blocks that query, which
    /// is the whole thing the granularity above exists to avoid.
    pub fn index(&self) -> std::sync::MutexGuard<'_, SearchIndex> {
        self.lock()
    }

    /// Poisoning is not a reason to stop answering queries: the index is a
    /// rebuildable cache, and a panic in one read says nothing about the rows.
    fn lock(&self) -> std::sync::MutexGuard<'_, SearchIndex> {
        self.index.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Answer a query, having first made sure somebody has compared the
    /// workspaces it is about against the store.
    ///
    /// An unscoped query asks for every workspace the index knows. That opens
    /// each workspace's *database* — not its documents — which is the honest
    /// cost of a global search and is bounded by how many workspaces there are.
    pub fn query(&self, query: &SearchQuery) -> SearchResult<SearchPage> {
        let scope = self.scope(query)?;
        let cold: Vec<WorkspaceId> = scope
            .iter()
            .copied()
            .filter(|w| self.sweeps.finished(*w) == 0)
            .collect();
        for workspace in &cold {
            self.sweeps.request(*workspace);
        }
        // **Unlocked, deliberately.** See the type's note: the whole point of
        // holding the lock inside `Search` is that this wait is not inside it.
        if !cold.is_empty() {
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_millis(SWEEP_WAIT_MILLIS);
            while std::time::Instant::now() < deadline
                && cold.iter().any(|w| self.sweeps.finished(*w) == 0)
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        let updating = self.updating(&scope);
        self.lock().query(query, &updating)
    }

    /// Record navigation, so a workspace's recency and a person's recent
    /// queries can inform the next answer.
    ///
    /// **One call because it is one gesture.** The surface records where
    /// somebody went and what they had typed to get there at the same moment,
    /// and a query only earns a place in the history by leading somewhere.
    pub fn record_navigation(
        &self,
        workspace: &str,
        document: Option<&str>,
        node: Option<&str>,
        query: &str,
    ) -> SearchResult<()> {
        let index = self.lock();
        index.record_open(workspace, document, node, storage::now_ms())?;
        index.record_query(query, storage::now_ms())
    }

    /// Which workspaces this answer may be behind on: not yet compared against
    /// the store, or with indexing still queued.
    fn updating(&self, scope: &[WorkspaceId]) -> Vec<String> {
        let pending = self.queue.pending_workspaces();
        let mut out: Vec<String> = scope
            .iter()
            .filter(|w| self.sweeps.finished(**w) == 0 || pending.contains(w))
            .map(|w| w.to_storage())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Which workspaces this query is about.
    ///
    /// **An unparseable id can no longer get this far.** `SearchQuery` carries
    /// `WorkspaceId`, which deserializes through `parse`, so a malformed one is
    /// refused at the boundary with a code. It used to be *dropped* here, which
    /// emptied the scope — and because the "no workspaces named" branch had
    /// already been taken, an empty scope means *no results* rather than
    /// *everywhere*. The caller got a successful, empty page and no hint that it
    /// had asked for something impossible.
    fn scope(&self, query: &SearchQuery) -> SearchResult<Vec<WorkspaceId>> {
        let named: Vec<WorkspaceId> = if query.workspaces.is_empty() {
            self.lock()
                .facets()?
                .workspaces
                .into_iter()
                .filter_map(|f| WorkspaceId::parse(&f.workspace))
                .collect()
        } else {
            query.workspaces.clone()
        };
        let excluded = &query.excluded_workspaces;
        Ok(named
            .into_iter()
            .filter(|w| !excluded.contains(w))
            .collect())
    }

    pub fn facets(&self) -> SearchResult<SearchFacets> {
        self.lock().facets()
    }

    pub fn recent_queries(&self, limit: usize) -> SearchResult<Vec<RecentQuery>> {
        self.lock().recent_queries(limit)
    }

    pub fn preview(
        &self,
        document: &str,
        matched: Option<&str>,
    ) -> SearchResult<model::SearchPreview> {
        self.lock().preview(document, matched)
    }

    /// What the index is doing, for `GET /v1/debug/search`. Counters only.
    pub fn stats(&self) -> IndexStats {
        let mut stats = self.queue.stats();
        let index = self.lock();
        stats.documents = index.indexed_documents().unwrap_or(0);
        stats.fragments = index.fragment_count().unwrap_or(0);
        stats
    }
}

/// `Search` is shared between the indexer's thread and every request that asks
/// it something, so this has to hold. It is asserted rather than assumed
/// because the thing that would break it — a field whose type is not `Send` —
/// would otherwise show up as an error at the far-away call site that shares it.
#[cfg(test)]
const _: fn() = || {
    fn assert_sync<T: Sync + Send>() {}
    let _ = assert_sync::<Search>;
};

/// Open the index beside a store root, ready to be wired up.
///
/// Returns the writer half — which the [`Indexer`] owns — and a reader for
/// queries. Two connections rather than one behind a mutex: an atomic
/// replacement must never queue an interactive query behind it.
pub fn open(store_root: &Path) -> SearchResult<(SearchIndex, SearchIndex)> {
    SearchIndex::pair(store_root)
}

/// The obligation this crate carries, tested where it lives.
#[cfg(test)]
mod tests {
    use super::*;
    use koru_projection::model::ListedDocument;
    use koru_store_core::ids::DocumentId;

    fn a_search(sweeps: Sweeps) -> (Search, WorkspaceId) {
        let mut index = SearchIndex::in_memory().unwrap();
        let workspace = WorkspaceId::new();
        let document = DocumentId::new();
        index
            .set_listing(
                &workspace.to_storage(),
                "Acme",
                b"d1",
                &[ListedDocument {
                    document,
                    parent: None,
                    ordinal: 0,
                    title: "Roadmap".into(),
                    kind: "page".into(),
                    mime: None,
                    trashed_at: None,
                    trashed_by: None,
                }],
            )
            .unwrap();
        index.warm_fuzzy_vocabulary().unwrap();
        (Search::new(index, Queue::default(), sweeps), workspace)
    }

    /// **A workspace nobody has touched since the store came up is exactly the
    /// one a query is about to ask about.** Answering from before the restart
    /// is the quiet failure this exists to prevent, so a query asks for the
    /// comparison and says so until it has been made.
    #[test]
    fn a_query_asks_for_a_sweep_of_a_workspace_nothing_has_compared() {
        let sweeps = Sweeps::default();
        let (search, workspace) = a_search(sweeps.clone());

        let page = search
            .query(&SearchQuery {
                text: "Acme".into(),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(
            page.updating_workspaces,
            vec![workspace.to_storage()],
            "an answer from an uncompared workspace claimed to be current"
        );
        assert!(!page.hits.is_empty(), "it answered with nothing at all");
    }

    /// **Concurrent queries wait together, not in turn.**
    ///
    /// The reason the mutex is a field rather than something wrapped around
    /// this type. Four queries against a workspace nobody has swept each wait
    /// up to `SWEEP_WAIT_MILLIS`; if the lock covered the wait they would take
    /// four times that between them, and three of them would be paying for a
    /// sweep they did not ask for.
    ///
    /// A ratio rather than a budget, in `perf.rs`'s idiom: the assertion is
    /// that the cost does not grow with the number of callers, and the margin
    /// is wide enough that a slow machine cannot fail it while a serialised
    /// implementation cannot pass it.
    #[test]
    fn concurrent_queries_share_one_sweep_wait_rather_than_queueing() {
        use std::sync::Arc;

        let sweeps = Sweeps::default();
        let (search, _workspace) = a_search(sweeps.clone());
        let search = Arc::new(search);

        let started = std::time::Instant::now();
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let search = Arc::clone(&search);
                std::thread::spawn(move || {
                    search
                        .query(&SearchQuery {
                            text: "Acme".into(),
                            ..Default::default()
                        })
                        .expect("query")
                })
            })
            .collect();
        for thread in threads {
            let page = thread.join().expect("thread");
            assert!(!page.hits.is_empty());
        }
        let taken = started.elapsed();

        assert!(
            taken < std::time::Duration::from_millis(SWEEP_WAIT_MILLIS * 2),
            "four concurrent queries took {taken:?}; serialised behind each \
             other's sweep wait they would take about {}ms, overlapped about {}ms",
            SWEEP_WAIT_MILLIS * 4,
            SWEEP_WAIT_MILLIS,
        );
    }

    /// And once one has finished, it stops saying so — a permanent "updating"
    /// is the same as none at all.
    #[test]
    fn a_swept_workspace_is_not_reported_as_updating() {
        let sweeps = Sweeps::default();
        let (search, workspace) = a_search(sweeps.clone());
        // As the projector does when it finishes one.
        sweeps.request(workspace);
        sweeps.record_for_test(workspace);

        let page = search
            .query(&SearchQuery {
                text: "Acme".into(),
                ..Default::default()
            })
            .unwrap();

        assert!(page.updating_workspaces.is_empty());
    }
}
