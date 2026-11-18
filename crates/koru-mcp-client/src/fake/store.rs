// SPDX-License-Identifier: MPL-2.0

//! The fake's Store port: `koru-mcp-server`'s in-memory Store, with a script
//! in front of each submission.
//!
//! **How an outcome is scripted.** The hosted server's Store port classifies
//! Koru Cloud's answer to each submission into `saved`, `not_saved` or
//! `unknown`, and the shared tool layer builds the call's answer from those
//! (`koru_mcp_server::effects`, which is `contracts/hosted-tools-v1.md`
//! sections 1 and 2 in code). So this port does the same from a script: a
//! test arms one submission of the next call with a [`Fate`], the port
//! answers that submission the way the convention says (`Ok`, a refusal, or
//! `save_status_unknown`), and **the real tool layer writes the envelope**.
//! Nothing here builds a `partially_saved` or a `save_status_unknown` by
//! hand.
//!
//! A submission is counted the way section 1's table counts effects: one
//! `persist_update` is one; `seed_document` is two (`document`, `manifest`);
//! a bare `create_document` is two (an extraction's `destination_seed`,
//! `manifest`).
//!
//! **Not modelled.** An edit that creates a tag makes a `system` effect
//! through a bare creation of the system document, which this counter would
//! count as two; no test here arms one. The 15 seconds the hosted server
//! waits before it says `unknown` is not simulated: a fate is immediate.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use koru_mcp_server::effects::{
    EDIT_NOT_SAVED, Effect, EffectKind, EffectState, SAVE_STATUS_UNKNOWN,
    SAVE_STATUS_UNKNOWN_MESSAGE,
};
use koru_mcp_server::test_support::MemoryStore;
use koru_mcp_server::{EmbeddedSeedDocument, EmbeddedSeededDocument, EmbeddedStore, ToolError};
use koru_store_core::ids::{
    DocumentId as StoreDocumentId, HistoryEntryId, WorkspaceId as StoreWorkspaceId,
};
use koru_store_core::model::{Blob, BlobHash, DocumentEntry, Workspace};
use koru_store_wire::search::{
    Lifecycle, MatchSpan, SearchHit, SearchPage, SearchRequest, SearchResponse, field,
};
use koru_store_wire::tasks::{TaskQuery, TaskQueryResponse};
use koru_store_wire::{CaptureHistory, HistoryEntry, HistoryPage, HistoryQuery, Status};

use super::Fate;

/// The manifest kind of a page, as `list_documents` filters on it
/// (`koru_core::explore::KIND_DOCUMENT`).
pub(super) const KIND_PAGE: &str = "page";

pub(super) struct ScriptedStore {
    inner: Arc<MemoryStore>,
    /// The id the `manifest` effect is reported under.
    manifest: StoreDocumentId,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The submission of the next call that does not simply save.
    armed: Option<(usize, Fate)>,
    /// Submissions made by the call in progress.
    submissions: usize,
    /// Documents Cloud holds whose manifest entry it does not: "the document
    /// exists and is not in the workspace's list" (section 2).
    unlisted: HashSet<StoreDocumentId>,
    /// The first line of each page a test registered, for content search.
    lines: Vec<(StoreDocumentId, String)>,
    /// The documents whose bodies the calling connection has opened.
    resident: HashSet<StoreDocumentId>,
}

impl ScriptedStore {
    pub(super) fn new(inner: Arc<MemoryStore>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            manifest: StoreDocumentId::new(),
            state: Mutex::new(State::default()),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn arm(&self, submission: usize, fate: Fate) {
        self.state().armed = Some((submission, fate));
    }

    /// Start a call: no submission made yet, and this connection's resident
    /// documents are the ones content search may look in.
    pub(super) fn begin_call(&self, resident: HashSet<StoreDocumentId>) {
        let mut state = self.state();
        state.submissions = 0;
        state.resident = resident;
    }

    /// End a call. A script is for one call: one that made any submission
    /// has used its script up, whether or not the armed one was reached.
    pub(super) fn end_call(&self) {
        let mut state = self.state();
        if state.submissions > 0 {
            state.armed = None;
        }
    }

    pub(super) fn index_line(&self, document: StoreDocumentId, line: &str) {
        self.state().lines.push((document, line.to_owned()));
    }

    /// The pages in the workspace's list.
    pub(super) fn listed_pages(&self) -> Vec<DocumentEntry> {
        let unlisted = self.state().unlisted.clone();
        self.inner
            .documents(self.inner.workspace())
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| entry.kind == KIND_PAGE && !unlisted.contains(&entry.id))
            .collect()
    }

    /// The fates of the next `count` submissions, in order.
    fn fates(&self, count: usize) -> Vec<Option<Fate>> {
        let mut state = self.state();
        let first = state.submissions;
        state.submissions += count;
        (first..first + count)
            .map(|index| match state.armed {
                Some((armed, fate)) if armed == index => Some(fate),
                _ => None,
            })
            .collect()
    }

    fn effect(
        &self,
        effect: EffectKind,
        document: Option<StoreDocumentId>,
        state: EffectState,
    ) -> Effect {
        Effect {
            effect,
            document_id: document.map(|document| document.to_storage()),
            state,
        }
    }

    /// The error for a two-effect port call that did not wholly save, saying
    /// how each of its two units ended, as the port convention asks
    /// (`ToolError::with_effects`, naming them `document` and `manifest`).
    fn pair_error(
        &self,
        document: Option<StoreDocumentId>,
        first: EffectState,
        second: EffectState,
    ) -> ToolError {
        let unknown = first == EffectState::Unknown || second == EffectState::Unknown;
        let error = if unknown { unconfirmed() } else { refused() };
        error.with_effects(&[
            self.effect(EffectKind::Document, document, first),
            self.effect(EffectKind::Manifest, Some(self.manifest), second),
        ])
    }

    /// Run a two-effect port call under the script. `make` does both effects
    /// in the in-memory Store and answers the document it made.
    fn pair<T>(
        &self,
        make: impl FnOnce() -> Result<(StoreDocumentId, T), ToolError>,
    ) -> Result<T, ToolError> {
        let fates = self.fates(2);
        match (fates[0], fates[1]) {
            (None, None) => {
                let (document, made) = make()?;
                // A replay of a creation that was left unlisted finishes it.
                self.state().unlisted.remove(&document);
                Ok(made)
            }
            // The first submission did not save, so the second was never
            // made: "stops at the first that does not end `saved`".
            (Some(Fate::NotSaved), _) => {
                Err(self.pair_error(None, EffectState::NotSaved, EffectState::NotSaved))
            }
            (Some(Fate::UnknownAndLost), _) => {
                Err(self.pair_error(None, EffectState::Unknown, EffectState::NotSaved))
            }
            (Some(Fate::UnknownButLanded), _) => {
                let (document, _) = make()?;
                self.state().unlisted.insert(document);
                Err(self.pair_error(Some(document), EffectState::Unknown, EffectState::NotSaved))
            }
            // The first saved; the manifest entry is the one in question.
            (None, Some(fate)) => {
                let (document, _) = make()?;
                let second = match fate {
                    Fate::NotSaved => EffectState::NotSaved,
                    Fate::UnknownAndLost | Fate::UnknownButLanded => EffectState::Unknown,
                };
                if fate != Fate::UnknownButLanded {
                    self.state().unlisted.insert(document);
                }
                Err(self.pair_error(Some(document), EffectState::Saved, second))
            }
        }
    }
}

/// A refusal with no code of its own, which leaves the unit unmoved: the tool
/// layer reports it as `edit_not_saved` (section 2).
fn refused() -> ToolError {
    ToolError {
        code: EDIT_NOT_SAVED.into(),
        message: "Koru Cloud refused this write. Nothing was saved.".into(),
        details: None,
    }
}

/// Submitted, then never acknowledged: the port convention's code for it.
fn unconfirmed() -> ToolError {
    ToolError {
        code: SAVE_STATUS_UNKNOWN.into(),
        message: SAVE_STATUS_UNKNOWN_MESSAGE.into(),
        details: None,
    }
}

impl EmbeddedStore for ScriptedStore {
    fn workspaces(&self) -> Result<Vec<Workspace>, ToolError> {
        self.inner.workspaces()
    }

    fn documents(&self, workspace: StoreWorkspaceId) -> Result<Vec<DocumentEntry>, ToolError> {
        let unlisted = self.state().unlisted.clone();
        Ok(self
            .inner
            .documents(workspace)?
            .into_iter()
            .filter(|entry| !unlisted.contains(&entry.id))
            .collect())
    }

    /// Titles of every listed page, and content only in the pages this
    /// connection has opened: the hosted server's search, as the tool's own
    /// description states it. Content here is the first line of a page a
    /// test registered, which is enough to tell the two apart.
    fn search(&self, request: SearchRequest) -> Result<SearchResponse, ToolError> {
        let wanted = request.query.text.to_lowercase();
        let (lines, resident) = {
            let state = self.state();
            (state.lines.clone(), state.resident.clone())
        };
        let workspace = self.inner.workspace().to_storage();
        let mut hits = Vec::new();
        for entry in self.listed_pages() {
            let hit = |field: &str, snippet: &str| SearchHit {
                workspace: workspace.clone(),
                workspace_name: "Notes".into(),
                document: Some(entry.id.to_storage()),
                node: None,
                document_kind: entry.kind.clone(),
                mime: None,
                kind: "document".into(),
                field: field.into(),
                title: entry.title.clone(),
                breadcrumb: entry.title.clone(),
                breadcrumb_spans: Vec::new(),
                snippet: snippet.into(),
                spans: Vec::<MatchSpan>::new(),
                lifecycle: Lifecycle::Live,
                trashed_at: None,
                trashed_by: None,
                score: 1.0,
                observed_modified_at_ms: None,
                workspace_last_used_at_ms: None,
                page: None,
                picture_name: None,
                list: None,
                task: None,
            };
            if entry.title.to_lowercase().contains(&wanted) {
                hits.push(hit(field::TITLE, &entry.title));
            }
            if !resident.contains(&entry.id) {
                continue;
            }
            for (document, line) in &lines {
                if *document == entry.id && line.to_lowercase().contains(&wanted) {
                    hits.push(hit(field::BODY, line));
                }
            }
        }
        Ok(SearchResponse {
            page: SearchPage {
                hits,
                next_offset: None,
                updating_workspaces: Vec::new(),
                request_seq: 0,
                corrected_query: None,
                reading: None,
            },
            facets: None,
            recent_queries: None,
        })
    }

    fn query_tasks(&self, query: TaskQuery) -> Result<TaskQueryResponse, ToolError> {
        self.inner.query_tasks(query)
    }

    fn snapshot(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
    ) -> Result<Vec<u8>, ToolError> {
        self.inner.snapshot(workspace, document)
    }

    /// An extraction's new destination: the empty unit, then its entry.
    fn create_document(
        &self,
        _workspace: StoreWorkspaceId,
        title: String,
        kind: String,
        _attribution: Option<&str>,
    ) -> Result<StoreDocumentId, ToolError> {
        self.pair(|| {
            let document = self.inner.register_empty(&title, &kind);
            Ok((document, document))
        })
    }

    fn manifest_id(&self, _workspace: StoreWorkspaceId) -> Result<StoreDocumentId, ToolError> {
        Ok(self.manifest)
    }

    fn ensure_file_note(
        &self,
        workspace: StoreWorkspaceId,
        file: StoreDocumentId,
    ) -> Result<(StoreDocumentId, bool), ToolError> {
        self.inner.ensure_file_note(workspace, file)
    }

    /// `create_document`: the seed, then its entry.
    fn seed_document(
        &self,
        request: EmbeddedSeedDocument,
    ) -> Result<EmbeddedSeededDocument, ToolError> {
        self.pair(|| {
            let seeded = self.inner.seed_document(request)?;
            Ok((seeded.id, seeded))
        })
    }

    fn persist_update(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
        update: Vec<u8>,
    ) -> Result<(), ToolError> {
        match self.fates(1)[0] {
            None => self.inner.persist_update(workspace, document, update),
            Some(Fate::NotSaved) => Err(refused()),
            Some(Fate::UnknownAndLost) => Err(unconfirmed()),
            Some(Fate::UnknownButLanded) => {
                self.inner.persist_update(workspace, document, update)?;
                Err(unconfirmed())
            }
        }
    }

    fn capture_history(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
        request: CaptureHistory,
    ) -> Result<HistoryEntry, ToolError> {
        self.inner.capture_history(workspace, document, request)
    }

    fn history(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
        query: HistoryQuery,
    ) -> Result<HistoryPage, ToolError> {
        self.inner.history(workspace, document, query)
    }

    fn history_snapshot(
        &self,
        workspace: StoreWorkspaceId,
        entry: HistoryEntryId,
    ) -> Result<Vec<u8>, ToolError> {
        self.inner.history_snapshot(workspace, entry)
    }

    fn blobs(&self, workspace: StoreWorkspaceId) -> Result<Vec<Blob>, ToolError> {
        self.inner.blobs(workspace)
    }

    fn get_blob(&self, workspace: StoreWorkspaceId, hash: BlobHash) -> Result<Vec<u8>, ToolError> {
        self.inner.get_blob(workspace, hash)
    }

    fn status(&self) -> Result<Status, ToolError> {
        self.inner.status()
    }
}
