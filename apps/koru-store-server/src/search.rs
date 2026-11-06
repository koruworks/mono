// SPDX-License-Identifier: MPL-2.0

//! Running the search index beside the read model.
//!
//! The same shape as `projection.rs` one layer down: a tick, an OS thread, and
//! the counters published where a debug route can read them. The two threads
//! are separate on purpose — a slow index must not hold the projection back,
//! which is the [`Consumer`](koru_projection::Consumer) contract, and the
//! projection's own thread is the one that takes the store's lock.
//!
//! ## Wiring order
//!
//! The index has to be registered with the projector **before** the projector
//! starts, and it needs the projector's [`Sweeps`] handle **after**. So it is
//! opened in two halves: [`prepare`] builds the consumer, and [`spawn`] starts
//! the thread once the projector exists. `daemon.rs` does them in that order,
//! before anything is served.

use std::sync::Arc;

use koru_projection::Consumer;
use koru_projection::worker::Sweeps;
use koru_search::{Feed, IndexStats, Indexer, Queue, Search, SearchIndex};
use koru_store_api::state::AppState;
use koru_store_core::errors::{Code, Error};
use koru_store_wire::search::{
    Navigation, SearchFacets, SearchPage, SearchRequest, SearchResponse,
};

/// How long the indexer sleeps when there is nothing to do. Woken by nothing —
/// the same reasoning as the projector's: a condvar in the commit path to save
/// a quarter of a second on work nobody is waiting for is not a trade worth
/// making.
const IDLE: std::time::Duration = std::time::Duration::from_millis(250);

/// What has been opened but not yet started.
pub struct Prepared {
    indexer: Indexer,
    reader: SearchIndex,
    queue: Queue,
    /// Carried through the two halves so [`spawn`] can open the tag source
    /// without `daemon.rs` handing it the location a second time.
    derived: crate::derived::Derived,
}

/// Open `search.db` and build the consumer the projector needs.
///
/// **Not fatal.** An index that will not open is a store whose search is stale,
/// not a store that has lost anything — and refusing to serve documents because
/// a cache is broken would be refusing for the wrong reason.
pub fn prepare(derived: &crate::derived::Derived) -> Option<(Box<dyn Consumer>, Prepared)> {
    let (writer, reader) = match derived.search_pair() {
        Ok(pair) => pair,
        Err(why) => {
            tracing::error!("the search index did not open: {why}");
            return None;
        }
    };
    tracing::info!("store startup: search index opened");
    let projections = match derived.projections() {
        Ok(projections) => projections,
        Err(why) => {
            tracing::error!("the search index has no read model to consume: {why}");
            return None;
        }
    };
    tracing::info!("store startup: projection read model opened");
    let queue = Queue::default();
    let indexer = Indexer::new(writer, projections, queue.clone());
    let feed: Box<dyn Consumer> = Box::new(Feed::new(queue.clone()));
    Some((
        feed,
        Prepared {
            indexer,
            reader,
            queue,
            derived: derived.clone(),
        },
    ))
}

/// **The adapter.** `koru-store-api` declares what it needs from an index and
/// links no index; this is the half that knows both.
///
/// A newtype rather than an impl on `Search` itself, because the trait belongs
/// to the HTTP crate and `Search` belongs to `koru-search` — neither should
/// have to name the other, and this file is where they already meet.
struct Port(Arc<Search>);

impl koru_store_api::routes::search::SearchService for Port {
    fn search(
        &self,
        scope: &koru_store_api::routes::search::WorkspaceSearchScope,
        request: &SearchRequest,
    ) -> Result<SearchResponse, Error> {
        let Some(query) = scoped_query(scope, &request.query) else {
            return Ok(empty_response(request));
        };
        let page = self.0.query(&query).map_err(failed)?;
        Ok(SearchResponse {
            page,
            facets: request
                .include_facets
                .then(|| scoped_facets(scope, self.0.facets()?))
                .transpose()
                .map_err(failed)?,
            recent_queries: request
                .recent_queries
                // Recent query text is not attributable to one workspace.
                // Suppress it under a restricted grant rather than risk
                // leaking a query made in another profile/workspace.
                .filter(|_| {
                    matches!(
                        scope,
                        koru_store_api::routes::search::WorkspaceSearchScope::AllLocal
                    )
                })
                .map(|limit| self.0.recent_queries(limit))
                .transpose()
                .map_err(failed)?,
        })
    }

    fn record_navigation(
        &self,
        scope: &koru_store_api::routes::search::WorkspaceSearchScope,
        navigation: &Navigation,
    ) -> Result<(), Error> {
        if !scope.allows(navigation.workspace) {
            return Err(Error::new(
                Code::Forbidden,
                "the search grant does not include that workspace",
            ));
        }
        self.0
            .record_navigation(
                &navigation.workspace.to_storage(),
                navigation.document.as_deref(),
                navigation.node.as_deref(),
                &navigation.query,
            )
            .map_err(failed)
    }
}

fn scoped_query(
    scope: &koru_store_api::routes::search::WorkspaceSearchScope,
    requested: &koru_store_wire::search::SearchQuery,
) -> Option<koru_store_wire::search::SearchQuery> {
    use koru_store_api::routes::search::WorkspaceSearchScope;
    match scope {
        WorkspaceSearchScope::AllLocal => Some(requested.clone()),
        WorkspaceSearchScope::Allowed(allowed) if allowed.is_empty() => None,
        WorkspaceSearchScope::Allowed(allowed) => {
            let mut query = requested.clone();
            query.workspaces = if requested.workspaces.is_empty() {
                allowed.iter().copied().collect()
            } else {
                requested
                    .workspaces
                    .iter()
                    .copied()
                    .filter(|workspace| allowed.contains(workspace))
                    .collect()
            };
            (!query.workspaces.is_empty()).then_some(query)
        }
    }
}

fn scoped_facets(
    scope: &koru_store_api::routes::search::WorkspaceSearchScope,
    mut facets: SearchFacets,
) -> Result<SearchFacets, koru_search::SearchError> {
    facets.workspaces.retain(|facet| {
        koru_store_core::WorkspaceId::parse(&facet.workspace)
            .is_some_and(|workspace| scope.allows(workspace))
    });
    Ok(facets)
}

fn empty_response(request: &SearchRequest) -> SearchResponse {
    SearchResponse {
        page: SearchPage {
            hits: Vec::new(),
            next_offset: None,
            updating_workspaces: Vec::new(),
            request_seq: request.query.request_seq,
            corrected_query: None,
        },
        facets: request.include_facets.then(SearchFacets::default),
        recent_queries: None,
    }
}

/// A search that failed is the index's problem, not the store's.
///
/// `SearchUnavailable` rather than `StorageFailed`: the workspace databases are
/// fine, and a client should not be told its data is in trouble because a
/// rebuildable cache is.
fn failed(why: koru_search::SearchError) -> Error {
    Error::new(Code::SearchUnavailable, format!("search: {why}"))
}

/// Start the indexer, now that there is a projector to ask for sweeps.
///
/// Also hands the query surface to the API, which is why this runs before
/// anything is served: a request arriving first would be answered 503 by a
/// store that does in fact have an index.
///
/// `stop` ends the thread at its next tick, as [`crate::projection::spawn`]'s
/// does; a device never sets it.
pub fn spawn(
    prepared: Prepared,
    state: Arc<AppState>,
    sweeps: Sweeps,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    let Prepared {
        mut indexer,
        reader,
        queue,
        derived,
    } = prepared;
    // **Here rather than in `prepare`**, because reading a tag table takes the
    // store's lock and `prepare` runs before there is an `AppState` to take it
    // from. Absent, the index carries on writing the ids it always wrote — a
    // word nobody can type, never a lost document.
    if let Some(tags) = crate::tagnames::tag_source(&derived, &state) {
        indexer = indexer.with_tag_names(tags);
    }
    // **One `Search`, shared.** It owns the mutex over its own connection, so
    // the indexer's tick and every query can hold it at once — and, more to the
    // point, a query waiting for a cold workspace's sweep is not holding it
    // while it waits.
    let search = Arc::new(Search::new(reader, queue, sweeps));
    state.set_search_service(Arc::new(Port(Arc::clone(&search))));

    let published = state.search.clone();
    let handle = std::thread::Builder::new()
        .name("koru-search".into())
        .spawn(move || {
            // Restore the fuzzy vocabulary off the startup path. It was opened
            // "warming" (see `SearchIndex::pair`), so queries answer without
            // typo suggestions until this rebuild — a full read of `fragments`
            // plus a BK-tree — finishes.
            if let Err(why) = indexer.index().warm_fuzzy_vocabulary() {
                tracing::error!("the fuzzy vocabulary could not be warmed: {why}");
            }
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                let now = koru_store_core::model::Timestamp::now().millis();
                let busy = indexer.tick(now);
                if let Ok(mut slot) = published.lock() {
                    *slot = report(&search.stats());
                }
                if !busy {
                    // A vocabulary that has lost a fifth of its terms is worth
                    // rebuilding, and an idle moment is when it costs nothing.
                    indexer.index().compact_fuzzy_vocabulary();
                    std::thread::sleep(IDLE);
                }
            }
        })
        .ok();
    if handle.is_some() {
        tracing::info!("search: indexing");
    }
    handle
}

/// **Counters, never content.** `/v1/debug/*` is metadata only — no titles, no
/// query text, no fragment text — and a search index is the component most
/// tempted to break that rule.
fn report(stats: &IndexStats) -> serde_json::Value {
    serde_json::json!({
        "schema_version": stats.schema_version,
        "pending": stats.pending,
        "indexed": stats.indexed,
        "removed": stats.removed,
        "listings": stats.listings,
        "failed": stats.failed,
        "caught_up": stats.caught_up,
        "documents": stats.documents,
        "fragments": stats.fragments,
        // **The one that says search is stale.** Everything above says what the
        // index has done; this says what it has not.
        "behind": stats.behind,
        // **And the one that says search never will be.** `behind` is work in
        // progress; this is work that cannot be done - documents in the store
        // that no listing names. Non-zero is a fact about the store.
        "unlisted": stats.unlisted,
        "last_millis": stats.last_millis,
        "total_millis": stats.total_millis,
        "max_millis": stats.max_millis,
        "read_millis": stats.read_millis,
        "build_millis": stats.build_millis,
        "write_millis": stats.write_millis,
        "last_error": stats.last_error,
    })
}

/// `koru-store-server search [<text>]` — look at the index.
///
/// Reads `search.db` directly, like `koru-store-server projection`, and for the same
/// reason: a diagnostic that needs the daemon to be healthy is a diagnostic
/// that is missing exactly when it is wanted. **It cannot ask a running store
/// to sweep**, so what it prints is what is on the disk now, which may be
/// behind a store that is still catching up.
pub fn report_to_console(
    root: &std::path::Path,
    text: Option<&str>,
) -> koru_store_core::errors::Result<()> {
    use koru_store_core::errors::{Code, Error};

    let path = root.join("search.db");
    if !path.exists() {
        println!("No index at {}.", path.display());
        println!("Nothing has been projected yet, or it has been deleted — either way it is");
        println!("rebuilt from the read model the next time the store runs.");
        return Ok(());
    }
    let index = SearchIndex::open(root)
        .map_err(|e| Error::new(Code::StorageFailed, format!("search: {e}")))?;

    let Some(text) = text else {
        println!("{}", path.display());
        println!(
            "{} document(s), {} fragment(s)",
            index.indexed_documents().unwrap_or(0),
            index.fragment_count().unwrap_or(0)
        );
        for facet in index
            .facets()
            .map_err(|e| Error::new(Code::StorageFailed, format!("search: {e}")))?
            .workspaces
        {
            println!("  {}  {}", facet.workspace, facet.workspace_name);
        }
        println!();
        println!("Any text searches it.");
        return Ok(());
    };

    let page = index
        .query(
            &koru_search::SearchQuery {
                text: text.to_owned(),
                ..Default::default()
            },
            &[],
        )
        .map_err(|e| Error::new(Code::StorageFailed, format!("search: {e}")))?;
    if page.hits.is_empty() {
        println!("Nothing matches {text:?}.");
        return Ok(());
    }
    for hit in &page.hits {
        println!("{:>6.0}  {:<12} {}", hit.score, hit.field, hit.breadcrumb);
        println!("        {}", hit.snippet);
    }
    Ok(())
}

#[cfg(test)]
mod scope_tests {
    use std::collections::BTreeSet;

    use super::*;
    use koru_store_api::routes::search::WorkspaceSearchScope;
    use koru_store_wire::search::SearchQuery;

    #[test]
    fn an_empty_authorized_set_is_deny_all_not_query_everywhere() {
        let scope = WorkspaceSearchScope::Allowed(BTreeSet::new());
        assert!(scoped_query(&scope, &SearchQuery::default()).is_none());
    }

    #[test]
    fn requested_workspaces_are_intersected_with_the_trusted_grant() {
        let allowed = koru_store_core::WorkspaceId::new();
        let denied = koru_store_core::WorkspaceId::new();
        let scope = WorkspaceSearchScope::Allowed([allowed].into_iter().collect());
        let query = SearchQuery {
            workspaces: vec![allowed, denied],
            ..Default::default()
        };

        assert_eq!(scoped_query(&scope, &query).unwrap().workspaces, [allowed]);
    }

    #[test]
    fn an_unfiltered_query_expands_to_exactly_the_trusted_grant() {
        let one = koru_store_core::WorkspaceId::new();
        let two = koru_store_core::WorkspaceId::new();
        let scope = WorkspaceSearchScope::Allowed([one, two].into_iter().collect());

        let effective = scoped_query(&scope, &SearchQuery::default()).unwrap();

        assert_eq!(
            effective.workspaces.into_iter().collect::<BTreeSet<_>>(),
            [one, two].into_iter().collect()
        );
    }
}
