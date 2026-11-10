// SPDX-License-Identifier: MPL-2.0

//! A small, headless client for entity pickers.
//!
//! The full search surface and an inline picker have different presentation,
//! but they must not grow different matching rules.  This adapter uses the
//! same reader, query model, store endpoint, normalization and ranking as
//! global search, and exposes only document-shaped rows to its host.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use koru_store_core::ids::{DocumentId, ProfileId, WorkspaceId};
use koru_store_wire::search::{Lifecycle, LifecycleFilter, SearchQuery};

use crate::catalog::Catalog;
use crate::reader::{SearchReadKind, SearchReadResult, SearchReader};

/// How long a failed catalog read is rested before it is tried again. The
/// catalog assigns every hit to a profile, so without one each hit is dropped
/// — but a store that is down must not be dialled on every keystroke either.
const CATALOG_RETRY_EVERY: Duration = Duration::from_secs(2);

/// Split a kind prefix off the query. Case-insensitive; unknown prefixes leave
/// the whole string as plain text.
pub fn parse_kind_prefix(text: &str) -> (Vec<String>, String) {
    let trimmed = text.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("file:") {
        let original_rest = &trimmed[trimmed.len() - rest.len()..];
        return (vec!["file".into()], original_rest.trim().to_owned());
    }
    if let Some(rest) = lower.strip_prefix("doc:") {
        let original_rest = &trimmed[trimmed.len() - rest.len()..];
        return (vec!["document".into()], original_rest.trim().to_owned());
    }
    (Vec::new(), text.to_owned())
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EntityWorkspace {
    pub profile: ProfileId,
    pub profile_name: String,
    pub id: WorkspaceId,
    pub name: String,
}

#[derive(Clone, PartialEq, Debug)]
pub struct EntityDocument {
    pub profile: ProfileId,
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    /// The selected task node, if this row is a task rather than a page.
    pub node: Option<String>,
    pub title: String,
    pub breadcrumb: String,
    /// Manifest kind of the hit (`document` / `file`). Defaults to document
    /// when the store left it blank (older indexes).
    pub kind: String,
    pub mime: Option<String>,
}

/// One document's headings, as a picker needs them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EntityOutline {
    pub document: DocumentId,
    /// In reading order.
    pub headings: Vec<EntityHeading>,
    /// Whether the read model has this document at all. An empty list from an
    /// unprojected document means "not yet", not "none" — and the difference
    /// is what a picker draws.
    pub projected: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EntityHeading {
    pub node: String,
    pub level: u8,
    pub text: String,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EntityDocumentFace {
    pub document: DocumentId,
    pub title: String,
    pub is_file: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub struct EntitySearchUpdate {
    pub seq: u64,
    pub documents: Vec<EntityDocument>,
    pub workspaces: Vec<EntityWorkspace>,
    pub unavailable: bool,
}

/// Non-blocking, coalescing access to the store's shared search service.
pub struct EntitySearch {
    reader: Option<SearchReader>,
    catalog: Catalog,
    /// Set only by a *complete* catalog read — a failed load is not readiness.
    /// The first attempt runs at window creation, which is exactly when a
    /// store that is still starting (a freshly peered device, `koru.exe`
    /// launched before its agent and store) cannot answer; latching that
    /// empty answer discarded every hit for the lifetime of the window,
    /// because a hit is only kept when its workspace is in the catalog.
    catalog_ready: bool,
    catalog_requested_at: Option<Instant>,
    /// The query most recently asked for, so it can run again once the
    /// catalog it was waiting for (or the store it failed against) is back.
    last_request: Option<(String, Vec<WorkspaceId>)>,
    seq: u64,
    pending: bool,
    unavailable_seq: Option<u64>,
    last_documents: Vec<EntityDocument>,
    pending_hits: Option<(u64, Vec<koru_api_client::SearchHit>, bool)>,
    /// Outlines that have come back and not yet been collected. A queue rather
    /// than a slot: two pills into two documents are two separate answers, and
    /// the newest does not supersede the other.
    pending_outlines: Vec<EntityOutline>,
    pending_faces: Vec<EntityDocumentFace>,
}

impl EntitySearch {
    /// Tests and the `no-reader` build: no reader, no dialling.
    pub fn new() -> Self {
        Self::new_at(None)
    }

    /// `root` is the launch's `--root`, and it is not optional decoration:
    /// without it the reader dials the **channel's default store** — every
    /// workspace of every profile on this machine — so a rooted window's
    /// mention picker would offer documents the window cannot open (the same
    /// hazard `store.rs` warns the global-search overlay about; that one takes
    /// the root, this one now does too).
    pub fn new_at(root: Option<std::path::PathBuf>) -> Self {
        let reader = if cfg!(any(test, feature = "no-reader")) {
            None
        } else {
            SearchReader::start(root).ok()
        };
        if let Some(reader) = &reader {
            reader.request_catalog();
        }
        Self {
            catalog_requested_at: reader.is_some().then(Instant::now),
            reader,
            catalog: Catalog::default(),
            catalog_ready: false,
            last_request: None,
            seq: 0,
            pending: false,
            unavailable_seq: None,
            last_documents: Vec::new(),
            pending_hits: None,
            pending_outlines: Vec::new(),
            pending_faces: Vec::new(),
        }
    }

    pub fn has_reader(&self) -> bool {
        self.reader.is_some()
    }

    /// Re-ask for the catalog while no complete read of one has landed.
    /// Throttled: the maintenance tick calls this several times a second, and
    /// a store that is down must not be dialled on every one.
    fn ensure_catalog(&mut self) {
        if self.catalog_ready {
            return;
        }
        let due = self
            .catalog_requested_at
            .is_none_or(|at| at.elapsed() >= CATALOG_RETRY_EVERY);
        if !due {
            return;
        }
        if let Some(reader) = &self.reader {
            reader.request_catalog();
            self.catalog_requested_at = Some(Instant::now());
        }
    }

    /// Search live document and file titles in the selected workspaces.
    ///
    /// Kind prefixes restrict the query: `file:budget` only files, `doc:plan`
    /// only those title kinds. An unknown prefix is treated as plain text.
    /// A bare query matches document titles and task labels.
    pub fn request(&mut self, text: String, workspaces: Vec<WorkspaceId>) -> u64 {
        self.ensure_catalog();
        self.last_request = Some((text.clone(), workspaces.clone()));
        self.seq = self.seq.wrapping_add(1);
        // A result retained while it waited for the catalog belongs to the
        // query that produced it, not to this replacement request.
        self.pending_hits = None;
        self.unavailable_seq = None;
        if let Some(reader) = &self.reader {
            let (kinds, text) = parse_kind_prefix(&text);
            let query = SearchQuery {
                text,
                workspaces,
                kinds,
                entity_picker: true,
                lifecycle: LifecycleFilter::Live,
                page_size: 20,
                request_seq: self.seq,
                ..SearchQuery::default()
            };
            let queued = reader.request(
                self.seq,
                SearchReadKind::Page {
                    query,
                    append: false,
                },
            );
            self.pending = true;
            if !queued {
                self.unavailable_seq = Some(self.seq);
            }
        } else {
            self.unavailable_seq = Some(self.seq);
            self.pending = true;
        }
        self.seq
    }

    pub fn pending(&self) -> bool {
        self.pending
    }

    /// The live workspaces a picker may search, once a complete catalog has
    /// landed; `None` until then (and asks for it again, throttled).
    ///
    /// For a host that knows no *current* workspace — a chat composer with
    /// no workspace on screen — and so cannot seed a picker's scope the way a
    /// document tab does. Answered from the catalog already read on the worker thread; this
    /// never dials anything itself.
    pub fn catalog_workspaces(&mut self) -> Option<Vec<EntityWorkspace>> {
        self.pump();
        self.ensure_catalog();
        self.catalog_ready.then(|| self.workspaces())
    }

    /// Ask for one document's headings.
    ///
    /// Without a reader this answers itself immediately and unprojected, so a
    /// caller under test or in a `no-reader` build reaches a terminal state
    /// rather than waiting on a store it was built not to have.
    pub fn request_outline(&mut self, document: DocumentId) {
        let asked = self
            .reader
            .as_ref()
            .is_some_and(|reader| reader.request_outline(document));
        if !asked {
            self.pending_outlines.push(EntityOutline {
                document,
                headings: Vec::new(),
                projected: false,
            });
        }
    }

    /// Take one outline answer, if any has arrived.
    pub fn poll_outline(&mut self) -> Option<EntityOutline> {
        self.pump();
        if self.pending_outlines.is_empty() {
            return None;
        }
        Some(self.pending_outlines.remove(0))
    }

    /// Resolve titles and kinds for links loaded from a saved page.
    pub fn request_document_faces(&mut self, documents: Vec<DocumentId>) {
        if let Some(reader) = &self.reader {
            reader.request_document_faces(documents);
        }
    }

    pub fn poll_document_faces(&mut self) -> Vec<EntityDocumentFace> {
        self.pump();
        std::mem::take(&mut self.pending_faces)
    }

    /// Take everything the reader has said and file it.
    ///
    /// **Draining is separate from answering.** `poll` returns early in two
    /// cases — an unavailable request, and no reader at all — and a second
    /// kind of answer that only drained inside `poll` would be stranded behind
    /// either of them. Every entry point calls this first.
    fn pump(&mut self) {
        let Some(reader) = self.reader.as_ref() else {
            return;
        };
        let mut became_ready = false;
        for response in reader.drain() {
            match response.result {
                // An entity picker completes no `#tag` and no `/key`; it
                // never asks for either table.
                SearchReadResult::ScopeTables { .. } => {}
                SearchReadResult::DocumentFaces(faces) => self.pending_faces.extend(faces),
                SearchReadResult::Catalog(catalog) => {
                    // A failed read is distinguishable from an empty store
                    // (see `Catalog::load`), and only a complete one is
                    // readiness. A ready answer always wins; a failed one can
                    // only replace a catalog that was never ready.
                    became_ready = catalog.ready() && !self.catalog_ready;
                    if catalog.ready() || !self.catalog_ready {
                        self.catalog_ready = catalog.ready();
                        self.catalog = catalog;
                    }
                }
                SearchReadResult::Page { result, .. } if response.seq == self.seq => {
                    let (hits, unavailable) = match result {
                        Ok(response) => (response.page.hits, false),
                        Err(_) => (Vec::new(), true),
                    };
                    self.pending_hits = Some((response.seq, hits, unavailable));
                }
                SearchReadResult::Page { .. } => {}
                SearchReadResult::FederatedPage { .. } => {}
                SearchReadResult::Outline {
                    document,
                    headings,
                    projected,
                } => self.pending_outlines.push(EntityOutline {
                    document,
                    headings: headings
                        .into_iter()
                        .map(|heading| EntityHeading {
                            node: heading.node,
                            level: heading.level,
                            text: heading.text,
                        })
                        .collect(),
                    projected,
                }),
            }
        }
        // The catalog the last query was waiting on is here. Parked good hits
        // deliver themselves through `poll` now; anything else — a refused
        // request, a failed page — is run again so the picker recovers without
        // waiting for another keystroke.
        if became_ready {
            let parked_good =
                matches!(&self.pending_hits, Some((_, _, unavailable)) if !unavailable);
            if !parked_good && let Some((text, workspaces)) = self.last_request.clone() {
                self.request(text, workspaces);
            }
        }
    }

    /// Drain every available answer, returning only the newest query result.
    pub fn poll(&mut self) -> Option<EntitySearchUpdate> {
        self.pump();
        self.ensure_catalog();
        if let Some(seq) = self.unavailable_seq.take() {
            self.pending = false;
            return Some(EntitySearchUpdate {
                seq,
                documents: Vec::new(),
                workspaces: Vec::new(),
                unavailable: true,
            });
        }
        self.reader.as_ref()?;
        let mut answer = None;
        let ready = self.catalog_ready;
        if ready && let Some((seq, hits, unavailable)) = self.pending_hits.take() {
            self.pending = false;
            let documents = self.documents(hits);
            self.last_documents = documents.clone();
            answer = Some(EntitySearchUpdate {
                seq,
                documents,
                workspaces: self.workspaces(),
                unavailable,
            });
        } else if self.pending_hits.is_some() {
            // A result can beat the catalog requested when the worker starts.
            // Keep the host's maintenance deadline alive until that catalog
            // arrives and the document ids can be assigned to profiles. The
            // inverse ordering matters too: polling an outstanding request
            // before its page arrives must not clear `pending`, or the host
            // stops polling and the picker sits on "Searching…" until another
            // unrelated gesture wakes it.
            self.pending = true;
        }
        answer
    }

    fn documents(&self, hits: Vec<koru_api_client::SearchHit>) -> Vec<EntityDocument> {
        let mut seen = HashSet::new();
        hits.into_iter()
            .filter_map(|hit| {
                if hit.lifecycle != Lifecycle::Live {
                    return None;
                }
                let workspace = WorkspaceId::parse(&hit.workspace)?;
                let document = DocumentId::parse(hit.document.as_deref()?)?;
                if !seen.insert((workspace, document, hit.node.clone())) {
                    return None;
                }
                let kind = if hit.document_kind.is_empty() {
                    "document".into()
                } else {
                    hit.document_kind
                };
                Some(EntityDocument {
                    profile: self.catalog.owner_of(workspace)?,
                    workspace,
                    document,
                    node: hit.node,
                    title: hit.title,
                    breadcrumb: hit.breadcrumb,
                    kind,
                    mime: hit.mime,
                })
            })
            .collect()
    }

    fn workspaces(&self) -> Vec<EntityWorkspace> {
        let mut rows: Vec<_> = self
            .catalog
            .workspaces
            .iter()
            // The entity picker is an ordinary live-content search. Archived
            // and softly deleted workspaces remain available to the dedicated
            // global-search fallbacks, but never appear in this scope menu.
            .filter(|entry| self.catalog.is_active(entry.id))
            .map(|entry| EntityWorkspace {
                profile: entry.owner,
                profile_name: self
                    .catalog
                    .profile_name(entry.owner)
                    .unwrap_or_default()
                    .to_owned(),
                id: entry.id,
                name: entry.name.clone(),
            })
            .collect();
        rows.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(
                    a.profile_name
                        .to_lowercase()
                        .cmp(&b.profile_name.to_lowercase()),
                )
                .then(a.id.to_storage().cmp(&b.id.to_storage()))
        });
        rows
    }
}

impl Default for EntitySearch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::{SearchReadResponse, SearchReadResult};

    #[test]
    fn entity_scope_lists_only_live_unarchived_workspaces() {
        let profile = ProfileId::new();
        let active = WorkspaceId::new();
        let archived = WorkspaceId::new();
        let deleted = WorkspaceId::new();
        let mut search = EntitySearch::new();
        search.catalog = Catalog::for_test_with_lifecycle(
            vec![
                (active, "Active".into(), profile, false, false),
                (archived, "Archived".into(), profile, true, false),
                (deleted, "Deleted".into(), profile, false, true),
            ],
            vec![(profile, "Profile".into())],
        );

        let rows = search.workspaces();
        assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<_>>(), [active]);
    }

    #[test]
    fn polling_before_the_page_arrives_keeps_the_request_alive() {
        let (reader, response_tx, request_rx) = SearchReader::test_pair();
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let mut search = EntitySearch::new();
        search.reader = Some(reader);

        search.request(String::new(), vec![workspace]);
        request_rx.recv().unwrap();
        assert!(search.poll().is_none());
        assert!(search.pending(), "an outstanding page still needs polling");

        response_tx
            .send(SearchReadResponse {
                seq: 0,
                result: SearchReadResult::Catalog(Catalog::for_test(
                    vec![(workspace, "Workspace".into(), profile)],
                    vec![(profile, "Profile".into())],
                )),
            })
            .unwrap();
        assert!(search.poll().is_none());
        assert!(
            search.pending(),
            "catalog-first delivery must continue waiting for the page"
        );
    }

    /// The reason `pump` exists. An unavailable request makes `poll` return
    /// before it ever reaches the reader, so an outline sitting in the channel
    /// behind it would never be filed and the picker would wait forever.
    #[test]
    fn an_outline_answer_survives_an_unavailable_search_answer() {
        let (reader, response_tx, _request_rx) = SearchReader::test_pair();
        let document = DocumentId::new();
        let mut search = EntitySearch::new();
        search.reader = Some(reader);
        // Arm the early return: a request the reader refused to queue.
        search.unavailable_seq = Some(7);

        response_tx
            .send(SearchReadResponse {
                seq: 0,
                result: SearchReadResult::Outline {
                    document,
                    headings: vec![koru_api_client::OutlineHeading {
                        node: "n1".into(),
                        level: 2,
                        text: "Rollout".into(),
                    }],
                    projected: true,
                },
            })
            .unwrap();

        let update = search.poll().expect("the unavailable answer");
        assert!(update.unavailable);

        let outline = search.poll_outline().expect("the outline was not stranded");
        assert_eq!(outline.document, document);
        assert_eq!(outline.headings[0].text, "Rollout");
        assert!(outline.projected);
    }

    #[test]
    fn saved_link_metadata_is_delivered_without_a_picker_query() {
        let (reader, response_tx, _request_rx) = SearchReader::test_pair();
        let document = DocumentId::new();
        let mut search = EntitySearch::new();
        search.reader = Some(reader);
        response_tx
            .send(SearchReadResponse {
                seq: 0,
                result: SearchReadResult::DocumentFaces(vec![EntityDocumentFace {
                    document,
                    title: "Project plan".into(),
                    is_file: false,
                }]),
            })
            .unwrap();

        let faces = search.poll_document_faces();
        assert_eq!(faces.len(), 1);
        assert_eq!(faces[0].document, document);
        assert_eq!(faces[0].title, "Project plan");
        assert!(!faces[0].is_file);
        assert!(search.poll_document_faces().is_empty());
    }

    /// A surface built without a reader still has to reach a terminal state.
    /// "Not indexed" is the honest answer; waiting is not.
    #[test]
    fn an_outline_request_without_a_reader_reports_unprojected_rather_than_hanging() {
        let document = DocumentId::new();
        let mut search = EntitySearch::new();
        assert!(!search.has_reader());

        search.request_outline(document);

        let outline = search.poll_outline().expect("an immediate answer");
        assert_eq!(outline.document, document);
        assert!(outline.headings.is_empty());
        assert!(!outline.projected);
    }

    /// Two pills into two documents are two answers. A slot would drop one.
    #[test]
    fn outlines_for_two_documents_both_come_back() {
        let (one, two) = (DocumentId::new(), DocumentId::new());
        let mut search = EntitySearch::new();

        search.request_outline(one);
        search.request_outline(two);

        assert_eq!(search.poll_outline().unwrap().document, one);
        assert_eq!(search.poll_outline().unwrap().document, two);
        assert!(search.poll_outline().is_none());
    }

    #[test]
    fn kind_prefixes_restrict_the_kinds_and_strip_the_token() {
        assert_eq!(
            parse_kind_prefix("file:budget"),
            (vec!["file".into()], "budget".into())
        );
        assert_eq!(
            parse_kind_prefix("DOC: Roadmap"),
            (vec!["document".into()], "Roadmap".into())
        );
        assert_eq!(parse_kind_prefix("foo:bar"), (Vec::new(), "foo:bar".into()));
        assert_eq!(parse_kind_prefix("plain"), (Vec::new(), "plain".into()));
    }

    fn hit(workspace: WorkspaceId, document: DocumentId) -> koru_api_client::SearchHit {
        koru_api_client::SearchHit {
            workspace: workspace.to_string(),
            workspace_name: "Workspace".into(),
            document: Some(document.to_string()),
            node: None,
            document_kind: "page".into(),
            mime: None,
            kind: "document".into(),
            field: "title".into(),
            title: "GPT Home - Pins".into(),
            breadcrumb: "Workspace / GPT Home - Pins".into(),
            breadcrumb_spans: Vec::new(),
            snippet: "GPT Home - Pins".into(),
            spans: Vec::new(),
            lifecycle: Lifecycle::Live,
            trashed_at: None,
            trashed_by: None,
            score: 1.0,
            observed_modified_at_ms: None,
            workspace_last_used_at_ms: None,
        }
    }

    #[test]
    fn task_hits_keep_distinct_nodes_in_one_document() {
        let workspace = WorkspaceId::new();
        let document = DocumentId::new();
        let profile = ProfileId::new();
        let mut search = EntitySearch::new();
        search.catalog = Catalog::for_test(
            vec![(workspace, "Work".into(), profile)],
            vec![(profile, "Profile".into())],
        );
        let mut first = hit(workspace, document);
        first.node = Some("11111111-1111-4111-8111-111111111111".into());
        first.title = "Ship café".into();
        let mut second = first.clone();
        second.node = Some("22222222-2222-4222-8222-222222222222".into());
        second.title = "Ship a second task".into();
        let choices = search.documents(vec![first.clone(), second.clone(), first.clone()]);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].node, first.node);
        assert_eq!(choices[1].node, second.node);
        assert_eq!(choices[0].title, "Ship café");
        assert_eq!(choices[0].document, document);
        assert_eq!(choices[0].workspace, workspace);
    }

    #[test]
    fn removed_task_hits_are_ignored_and_stale_task_pages_cannot_replace_a_new_query() {
        let (reader, response_tx, request_rx, _background_rx) =
            SearchReader::test_pair_with_background();
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let document = DocumentId::new();
        let mut search = EntitySearch::new();
        search.reader = Some(reader);
        response_tx
            .send(SearchReadResponse {
                seq: 0,
                result: SearchReadResult::Catalog(Catalog::for_test(
                    vec![(workspace, "Workspace".into(), profile)],
                    vec![(profile, "Profile".into())],
                )),
            })
            .unwrap();
        search.pump();

        let stale = search.request("old".into(), vec![workspace]);
        request_rx.recv().unwrap();
        let current = search.request("new".into(), vec![workspace]);
        request_rx.recv().unwrap();
        let mut old_task = hit(workspace, document);
        old_task.node = Some("old-node".into());
        response_tx
            .send(SearchReadResponse {
                seq: stale,
                result: page(stale, vec![old_task]),
            })
            .unwrap();
        assert!(search.poll().is_none());

        let mut removed_task = hit(workspace, document);
        removed_task.node = Some("removed-node".into());
        removed_task.lifecycle = Lifecycle::Trashed;
        let mut live_task = hit(workspace, document);
        live_task.node = Some("live-node".into());
        response_tx
            .send(SearchReadResponse {
                seq: current,
                result: page(current, vec![removed_task, live_task]),
            })
            .unwrap();
        let update = search.poll().unwrap();
        assert_eq!(update.seq, current);
        assert_eq!(update.documents.len(), 1);
        assert_eq!(update.documents[0].node.as_deref(), Some("live-node"));
    }

    fn page(seq: u64, hits: Vec<koru_api_client::SearchHit>) -> SearchReadResult {
        SearchReadResult::Page {
            append: false,
            result: Ok(koru_api_client::SearchResponse {
                page: koru_api_client::SearchPage {
                    hits,
                    next_offset: None,
                    updating_workspaces: Vec::new(),
                    request_seq: seq,
                    corrected_query: None,
                },
                facets: None,
                recent_queries: None,
            }),
        }
    }

    /// The reported bug: the store could not answer the catalog at window
    /// creation (launched before its store, or a device being peered), and the
    /// failed read was latched as the catalog — so every hit was discarded for
    /// the window's lifetime. A failed read is not readiness: it is retried,
    /// and hits that arrived in the meantime deliver once a complete read
    /// lands.
    #[test]
    fn a_failed_catalog_is_retried_and_parked_hits_survive_the_wait() {
        let (reader, response_tx, request_rx, background_rx) =
            SearchReader::test_pair_with_background();
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let document = DocumentId::new();
        let mut search = EntitySearch::new();
        search.reader = Some(reader);

        // The poll loop re-asks for the catalog while none has landed...
        assert!(search.poll().is_none());
        assert!(matches!(
            background_rx.try_recv(),
            Ok(crate::reader::BackgroundWork::Catalog)
        ));
        // ...but not on every tick.
        assert!(search.poll().is_none());
        assert!(background_rx.try_recv().is_err(), "the retry is throttled");

        let seq = search.request("gpt".into(), vec![workspace]);
        request_rx.recv().unwrap();
        // The store answers the query, and the catalog retry comes back
        // failed — `Catalog::default()` is the failed load, not an empty store.
        response_tx
            .send(SearchReadResponse {
                seq,
                result: page(seq, vec![hit(workspace, document)]),
            })
            .unwrap();
        response_tx
            .send(SearchReadResponse {
                seq: 0,
                result: SearchReadResult::Catalog(Catalog::default()),
            })
            .unwrap();
        assert!(search.poll().is_none(), "hits wait for a *ready* catalog");
        assert!(search.pending(), "the request is not lost while it waits");

        // The retry lands, complete this time: the parked hits deliver.
        response_tx
            .send(SearchReadResponse {
                seq: 0,
                result: SearchReadResult::Catalog(Catalog::for_test(
                    vec![(workspace, "Workspace".into(), profile)],
                    vec![(profile, "Profile".into())],
                )),
            })
            .unwrap();
        let update = search.poll().expect("parked hits deliver once ready");
        assert_eq!(update.documents.len(), 1);
        assert_eq!(update.documents[0].document, document);
        assert_eq!(update.documents[0].profile, profile);
        assert_eq!(update.workspaces.len(), 1);
    }

    /// A query that failed against a down store is run again on its own once
    /// the catalog answers — the picker recovers without another keystroke.
    #[test]
    fn a_query_that_failed_is_run_again_once_the_catalog_is_ready() {
        let (reader, response_tx, request_rx, _background_rx) =
            SearchReader::test_pair_with_background();
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let mut search = EntitySearch::new();
        search.reader = Some(reader);

        let seq = search.request("gpt".into(), vec![workspace]);
        request_rx.recv().unwrap();
        // The store was down: the page comes back an error, and parks until
        // the catalog arrives.
        response_tx
            .send(SearchReadResponse {
                seq,
                result: SearchReadResult::Page {
                    append: false,
                    result: Err(koru_store_core::errors::Error::new(
                        koru_store_core::errors::Code::StoreUnreachable,
                        "down",
                    )),
                },
            })
            .unwrap();
        assert!(search.poll().is_none(), "parked until the catalog arrives");

        response_tx
            .send(SearchReadResponse {
                seq: 0,
                result: SearchReadResult::Catalog(Catalog::for_test(
                    vec![(workspace, "Workspace".into(), profile)],
                    vec![(profile, "Profile".into())],
                )),
            })
            .unwrap();
        assert!(
            search.poll().is_none(),
            "the failure was superseded, not shown"
        );
        let reissued = request_rx.recv().expect("the failed query was re-issued");
        let SearchReadKind::Page { query, .. } = reissued.into_kind() else {
            panic!("entity search must remain Store-only")
        };
        assert_eq!(query.text, "gpt");
        assert_eq!(query.workspaces, vec![workspace]);
    }
}
