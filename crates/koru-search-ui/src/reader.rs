// SPDX-License-Identifier: MPL-2.0

//! Non-blocking interactive reads for the shared search surface.
//!
//! **The shape is unchanged from when this read a local SQLite index**: two
//! named threads, unbounded channels, keystrokes coalesced to the newest
//! request, and a `seq` on every answer so an older one cannot replace newer
//! visible state. What changed is where the answer comes from.
//!
//! ## Three calls became one
//!
//! A search surface wants the page, the workspace list for its scope picker,
//! and the recent queries on the same keystroke. Against the local index that
//! was three separate reads. `POST /v1/search` carries all three, so it is one
//! round trip — and the two extras are asked for only when they will be used.
//!
//! ## Dead under test, and that is the point
//!
//! `SearchSurface::start_reader` is `None` under `cfg(test)`, so no unit test
//! dials the developer's own store — see the testing section of `AGENTS.md`.
//! The consequence is that the worker and every message it *sends* have no
//! caller in a test build. Allowed here, module-scoped, rather than annotating
//! six items one at a time: the alternative was leaving six warnings standing,
//! and warnings that are always there are warnings nobody reads.
#![cfg_attr(test, allow(dead_code))]

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use koru_api_client::{
    FederatedSearchRequest, FederatedSearchResponse, Navigation, SearchQuery, SearchRequest,
    SearchResponse,
};
use koru_store_core::errors::Result;
use koru_store_core::ids::{DocumentId, WorkspaceId};

use super::store::StoreSearch;

pub(super) enum SearchReadKind {
    Page {
        query: SearchQuery,
        append: bool,
    },
    FederatedPage {
        query: SearchQuery,
        append: bool,
        include_ai: bool,
        cursor: Option<String>,
        /// Only this profile's chats among the AI hits (AI schema 13).
        profile: Option<koru_store_core::ids::ProfileId>,
    },
}

/// Background work that must never sit in front of a keystroke.
///
/// **Its own channel, for the same reason navigation has one.** The read thread
/// coalesces to the newest request, which is right for a query and wrong for
/// anything that only needs doing once — a catalog request queued behind two
/// fast keystrokes would simply be dropped.
pub(super) enum BackgroundWork {
    Navigation(NavigationRecord),
    Catalog,
    /// One document's headings, for a picker offering jump targets. Background
    /// rather than a request kind for exactly the reason above: it needs doing
    /// once, and coalescing would drop it behind the next two keystrokes.
    Outline(DocumentId),
    /// Resolve saved page links without opening their documents on the UI thread.
    DocumentFaces(Vec<DocumentId>),
    /// Both tables the query box completes against — every live tag in these
    /// workspaces, and every filterable attribute definition in scope.
    ///
    /// **One item, because it is one request.** The attribute query answers
    /// both in one response, and splitting it would put two handshakes behind
    /// a box that needs both the moment anybody types a sigil.
    ///
    /// Background for the same reason again — it is fetched once per scope,
    /// not once per keystroke, and a coalescing channel would drop it behind
    /// the typing it exists to help.
    ScopeTables(Vec<WorkspaceId>),
}

pub(super) struct NavigationRecord {
    workspace: WorkspaceId,
    document: Option<String>,
    node: Option<String>,
    query: String,
}

pub(super) struct SearchReadRequest {
    seq: u64,
    kind: SearchReadKind,
}

#[cfg(test)]
impl SearchReadRequest {
    pub(super) fn into_kind(self) -> SearchReadKind {
        self.kind
    }
}

pub(super) enum SearchReadResult {
    Page {
        append: bool,
        /// The page, and — when they were asked for — the scope list and the
        /// recent queries, in one answer because they arrived in one.
        result: Result<SearchResponse>,
    },
    FederatedPage {
        append: bool,
        result: Result<FederatedSearchResponse>,
    },
    Catalog(super::catalog::Catalog),
    Outline {
        document: DocumentId,
        /// In reading order. Empty on failure as well as on a document with no
        /// headings — `projected` is what separates them, and a caller that
        /// cannot reach the store is told the same thing as one whose document
        /// is not indexed yet: not now, rather than never.
        headings: Vec<koru_api_client::OutlineHeading>,
        projected: bool,
    },
    DocumentFaces(Vec<super::entity_search::EntityDocumentFace>),
    /// The scoped workspaces' tags — name, colour and live usage, ready to
    /// complete a half-typed `#tag` — and the filterable attribute definitions
    /// the `/key` chooser is built from. Both empty on failure: a box that
    /// cannot complete simply does not open, which is the same thing it does
    /// before the first fetch lands.
    ScopeTables {
        tags: Vec<super::TagCompletion>,
        definitions: Vec<crate::filters::FilterDefinition>,
    },
}

pub(super) struct SearchReadResponse {
    pub(super) seq: u64,
    pub(super) result: SearchReadResult,
}

/// Owns the only potentially expensive reads made by a search surface.
///
/// The UI sender is deliberately unbounded: sending must never wait for the
/// store. Before each read the worker drains its queue and keeps only the
/// newest request, so fast typing cannot build a backlog of obsolete searches.
/// The surface also checks `seq`, making an old in-flight response incapable of
/// replacing newer visible state.
pub(super) struct SearchReader {
    requests: Sender<SearchReadRequest>,
    background: Sender<BackgroundWork>,
    responses: Receiver<SearchReadResponse>,
}

impl SearchReader {
    pub(super) fn start(root: Option<std::path::PathBuf>) -> std::io::Result<Self> {
        let (requests, request_rx) = mpsc::channel::<SearchReadRequest>();
        let (navigation, navigation_rx) = mpsc::channel::<BackgroundWork>();
        let (response_tx, responses) = mpsc::channel::<SearchReadResponse>();
        let catalog_tx = response_tx.clone();

        // **One handle, two threads.** Two clients would mint two sessions and
        // put two "session started" lines in the store's log per search window.
        let store = StoreSearch::new(root);
        let history_store = store.clone();

        std::thread::Builder::new()
            .name("koru-search-read".into())
            .spawn(move || {
                while let Ok(request) = request_rx.recv() {
                    let request = newest_request(request, &request_rx);
                    let seq = request.seq;
                    let started = Instant::now();
                    let result = match request.kind {
                        SearchReadKind::Page { query, append } => {
                            // A page-2 append needs neither the scope picker
                            // nor the history: the surface already has both.
                            let body = SearchRequest {
                                query,
                                include_facets: !append,
                                recent_queries: (!append).then_some(8),
                            };
                            SearchReadResult::Page {
                                append,
                                result: store.call(|client| client.store().search(&body)),
                            }
                        }
                        SearchReadKind::FederatedPage {
                            query,
                            append,
                            include_ai,
                            cursor,
                            profile,
                        } => {
                            let body = FederatedSearchRequest {
                                query,
                                include_facets: !append,
                                recent_queries: (!append).then_some(8),
                                include_ai,
                                cursor,
                                profile,
                            };
                            SearchReadResult::FederatedPage {
                                append,
                                result: store.call(|client| client.search(&body)),
                            }
                        }
                    };
                    let elapsed = started.elapsed();
                    // **Half a second, not a quarter.** A query against a
                    // workspace nobody has swept since the store came up waits
                    // up to `SWEEP_WAIT_MILLIS` by design, so the old 250 ms
                    // threshold would now fire on an ordinary first search and
                    // the log would stop meaning anything.
                    if elapsed >= Duration::from_millis(750) {
                        crate::search_debug!(
                            "[Koru][search-read][pid={}] slow query: seq={seq} elapsed_ms={}",
                            std::process::id(),
                            elapsed.as_millis(),
                        );
                    }
                    if response_tx
                        .send(SearchReadResponse { seq, result })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;

        std::thread::Builder::new()
            .name("koru-search-background".into())
            .spawn(move || {
                while let Ok(work) = navigation_rx.recv() {
                    match work {
                        BackgroundWork::Navigation(record) => {
                            // **One call, because it is one gesture.** Where
                            // somebody went and what they had typed to get there
                            // are recorded together, and a query earns its place
                            // in the history only by leading somewhere.
                            let navigation = Navigation {
                                workspace: record.workspace,
                                document: record.document,
                                node: record.node,
                                query: record.query,
                            };
                            if let Err(error) =
                                history_store.call(|client| client.record_navigation(&navigation))
                            {
                                // Worth a line and nothing more: the search
                                // worked, and what was lost is one entry in a
                                // recents list.
                                crate::search_debug!(
                                    "[Koru][search-history][pid={}] not recorded: {error}",
                                    std::process::id(),
                                );
                            }
                        }
                        BackgroundWork::Outline(document) => {
                            let answer = history_store
                                .call(|client| client.document_outline(document))
                                .ok();
                            // An error is a terminal answer, not a silence: a
                            // picker left waiting shows "Indexing…" forever.
                            let (headings, projected) = match answer {
                                Some(outline) => (outline.headings, outline.projected),
                                None => (Vec::new(), false),
                            };
                            if catalog_tx
                                .send(SearchReadResponse {
                                    // Zero for the same reason a catalog is: an
                                    // outline answers no query.
                                    seq: 0,
                                    result: SearchReadResult::Outline {
                                        document,
                                        headings,
                                        projected,
                                    },
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        BackgroundWork::DocumentFaces(documents) => {
                            let mut faces = Vec::new();
                            let mut by_workspace: std::collections::HashMap<
                                WorkspaceId,
                                std::collections::HashSet<DocumentId>,
                            > = std::collections::HashMap::new();
                            for document in documents {
                                if let Ok(location) = history_store
                                    .call(|client| client.store().locate_document(document))
                                {
                                    by_workspace
                                        .entry(location.workspace)
                                        .or_default()
                                        .insert(document);
                                }
                            }
                            for (workspace, wanted) in by_workspace {
                                if let Ok(entries) =
                                    history_store.call(|client| client.store().documents(workspace))
                                {
                                    faces.extend(entries.into_iter().filter_map(|entry| {
                                        wanted.contains(&entry.id).then_some(
                                            super::entity_search::EntityDocumentFace {
                                                document: entry.id,
                                                title: entry.title,
                                                is_file: entry.kind == "file",
                                            },
                                        )
                                    }));
                                }
                            }
                            if catalog_tx
                                .send(SearchReadResponse {
                                    seq: 0,
                                    result: SearchReadResult::DocumentFaces(faces),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        BackgroundWork::ScopeTables(workspaces) => {
                            // `scope_tables` is the adapter's, not the
                            // client's: it does the folding, the ordering and
                            // the digest of the definitions, and it already
                            // goes through `call`.
                            let (tags, definitions) =
                                history_store.scope_tables(&workspaces).unwrap_or_default();
                            if catalog_tx
                                .send(SearchReadResponse {
                                    // Zero, for the reason the catalog gives
                                    // below: this answers no query.
                                    seq: 0,
                                    result: SearchReadResult::ScopeTables { tags, definitions },
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        BackgroundWork::Catalog => {
                            let catalog = super::catalog::Catalog::load(&history_store);
                            if catalog_tx
                                .send(SearchReadResponse {
                                    // **Zero, and deliberately not a live
                                    // sequence.** The surface matches `seq`
                                    // against the query it is waiting for; a
                                    // catalog answers no query, so it carries a
                                    // number no request will ever have.
                                    seq: 0,
                                    result: SearchReadResult::Catalog(catalog),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
            })?;

        Ok(Self {
            requests,
            background: navigation,
            responses,
        })
    }

    pub(super) fn request(&self, seq: u64, kind: SearchReadKind) -> bool {
        self.requests.send(SearchReadRequest { seq, kind }).is_ok()
    }

    /// Ask for the workspace catalog, on the background channel so it cannot
    /// delay a keystroke.
    pub(super) fn request_catalog(&self) {
        let _ = self.background.send(BackgroundWork::Catalog);
    }

    /// Ask for the scoped workspaces' tag table and attribute definitions, on
    /// the background channel.
    pub(super) fn request_scope_tables(&self, workspaces: Vec<WorkspaceId>) -> bool {
        self.background
            .send(BackgroundWork::ScopeTables(workspaces))
            .is_ok()
    }

    /// Ask for one document's headings, on the background channel.
    pub(super) fn request_outline(&self, document: DocumentId) -> bool {
        self.background
            .send(BackgroundWork::Outline(document))
            .is_ok()
    }

    pub(super) fn request_document_faces(&self, documents: Vec<DocumentId>) -> bool {
        self.background
            .send(BackgroundWork::DocumentFaces(documents))
            .is_ok()
    }

    pub(super) fn record_navigation(
        &self,
        _seq: u64,
        workspace: WorkspaceId,
        document: Option<String>,
        node: Option<String>,
        query: String,
    ) {
        let _ = self
            .background
            .send(BackgroundWork::Navigation(NavigationRecord {
                workspace,
                document,
                node,
                query,
            }));
    }

    pub(super) fn drain(&self) -> Vec<SearchReadResponse> {
        self.responses.try_iter().collect()
    }

    #[cfg(test)]
    pub(super) fn test_pair() -> (
        Self,
        Sender<SearchReadResponse>,
        Receiver<SearchReadRequest>,
    ) {
        let (reader, response_tx, request_rx, _) = Self::test_pair_with_background();
        (reader, response_tx, request_rx)
    }

    /// The same pair, plus the background channel's receiver, so a test can
    /// see catalog and outline requests too.
    #[cfg(test)]
    pub(super) fn test_pair_with_background() -> (
        Self,
        Sender<SearchReadResponse>,
        Receiver<SearchReadRequest>,
        Receiver<BackgroundWork>,
    ) {
        let (requests, request_rx) = mpsc::channel();
        let (background, background_rx) = mpsc::channel();
        let (response_tx, responses) = mpsc::channel();
        let reader = Self {
            requests,
            background,
            responses,
        };
        (reader, response_tx, request_rx, background_rx)
    }
}

fn newest_request(
    mut request: SearchReadRequest,
    pending: &Receiver<SearchReadRequest>,
) -> SearchReadRequest {
    for newer in pending.try_iter() {
        request = newer;
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_keystrokes_coalesce_to_the_newest_request() {
        let (tx, rx) = mpsc::channel();
        for seq in 1..=3 {
            tx.send(SearchReadRequest {
                seq,
                kind: SearchReadKind::Page {
                    query: SearchQuery {
                        request_seq: seq,
                        ..SearchQuery::default()
                    },
                    append: false,
                },
            })
            .unwrap();
        }

        let first = rx.recv().unwrap();
        assert_eq!(newest_request(first, &rx).seq, 3);
    }
}
