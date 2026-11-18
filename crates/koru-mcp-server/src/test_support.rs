// SPDX-License-Identifier: MPL-2.0

//! This crate's in-memory Store, for tests here and in the crates that run
//! the shared tools without a Store process.
//!
//! **Compiled for this crate's own tests, and for another crate only behind
//! the `test-support` feature**, which is off by default: a normal build of
//! `koru-mcp-server` does not contain this module. A dependency is never
//! compiled with `cfg(test)`, so a feature is the only way another crate's
//! tests can reach it (the same reason `koru-core` has `golden`).
//!
//! [`MemoryStore`] is the `EmbeddedStore` this crate's own tests have always
//! run [`LocalBackend`] over. It was a private item of the test module; it
//! lives here so that one copy serves both. It mirrors the Store's contract
//! (idempotency, files-only refusal, an empty note at creation, a history
//! marker that keeps the document as it stood); it is not the thing under
//! test.
//!
//! **What another crate can reach is deliberately small**: build one, name
//! its workspace, register a page, read back what a client would have read,
//! register an empty document, and get the backend over it. Everything else
//! is `pub(crate)`, for this crate's tests, and its fields are too.
//!
//! **Three port methods still panic**, as they did: `create_document`,
//! `manifest_id` and `status`. This crate's tests rely on that to prove a
//! refused call never reached them. A caller that needs them answers them in
//! its own `EmbeddedStore` wrapper around this one, with
//! [`MemoryStore::register_empty`] for the document a bare creation makes.

// Items only this crate's tests call are unused when the module is compiled
// for another crate.
#![cfg_attr(not(test), allow(dead_code))]

use super::*;

/// One workspace, in memory.
pub struct MemoryStore {
    pub(crate) workspace: StoreWorkspaceId,
    pub(crate) entries: Mutex<Vec<DocumentEntry>>,
    pub(crate) snapshots: Mutex<HashMap<StoreDocumentId, Vec<u8>>>,
    pub(crate) blobs: Mutex<Vec<Blob>>,
    /// The bytes of the blobs a test stored, by hash.
    pub(crate) blob_bytes: Mutex<HashMap<String, Vec<u8>>>,
    pub(crate) search_hits: Mutex<Vec<koru_store_wire::search::SearchHit>>,
    /// Every history marker captured, with the document as it stood.
    pub(crate) markers: Mutex<Vec<(HistoryEntry, Vec<u8>)>>,
    /// Each `create_document` idempotency key: the request hash, the
    /// document it made, and that document's first update.
    pub(crate) seeds: Mutex<HashMap<String, (String, StoreDocumentId, Vec<u8>)>>,
    /// Every tasks query the port was asked.
    pub(crate) task_queries: Mutex<Vec<TaskQuery>>,
}

impl MemoryStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            workspace: StoreWorkspaceId::new(),
            entries: Mutex::new(Vec::new()),
            snapshots: Mutex::new(HashMap::new()),
            blobs: Mutex::new(Vec::new()),
            blob_bytes: Mutex::new(HashMap::new()),
            search_hits: Mutex::new(Vec::new()),
            markers: Mutex::new(Vec::new()),
            seeds: Mutex::new(HashMap::new()),
            task_queries: Mutex::new(Vec::new()),
        })
    }

    /// Register a file entry exactly as an import leaves one: a manifest
    /// row of kind `file` whose document carries a single `Kind::File`
    /// root, plus the blob metadata `file_json` measures size from.
    pub(crate) fn add_file(self: &Arc<Self>, name: &str, mime: &str) -> StoreDocumentId {
        let id = StoreDocumentId::new();
        let hash = koru_core::BlobHash::new("b3", "0123abcd");
        let mut doc =
            WorkspaceDoc::create(CoreWorkspaceId::from_uuid(self.workspace.as_uuid()), name)
                .unwrap();
        doc.insert(Parent::Root, Node::file(name, hash.clone(), mime).unwrap())
            .unwrap();
        self.snapshots
            .lock()
            .unwrap()
            .insert(id, doc.export_snapshot().unwrap());
        self.entries.lock().unwrap().push(DocumentEntry {
            id,
            parent: None,
            ordinal: 0,
            title: name.into(),
            kind: koru_core::explore::KIND_FILE.into(),
            mime: Some(mime.into()),
            note: None,
            trashed_at: None,
            trashed_by: None,
        });
        self.blobs.lock().unwrap().push(Blob {
            hash: koru_store_core::model::BlobHash::parse(&hash.0).unwrap(),
            size: 3,
            mime: mime.into(),
            filename: Some(name.into()),
            created_at: koru_store_core::model::Timestamp::from_millis(1),
            attributes: Default::default(),
        });
        id
    }

    /// Register an ordinary page, so a refusal can be told apart from a
    /// missing id.
    pub fn add_page(self: &Arc<Self>, title: &str) -> StoreDocumentId {
        self.add_document(koru_core::explore::KIND_DOCUMENT, title, "")
    }

    /// Register a document of any manifest kind as an ordinary node tree —
    /// a root and one `line` — so that a tool which did not ask the kind
    /// would have everything it needs to succeed.
    pub fn add_document(self: &Arc<Self>, kind: &str, title: &str, line: &str) -> StoreDocumentId {
        let id = StoreDocumentId::new();
        self.entries.lock().unwrap().push(DocumentEntry {
            id,
            parent: None,
            ordinal: 0,
            title: title.into(),
            kind: kind.into(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        });
        self.seed_root_and_line(id, title, line);
        id
    }

    /// Put an image block at the end of a document's body, with its
    /// bytes in the blob store, and answer the block's id.
    pub(crate) fn add_image_block(
        &self,
        document: StoreDocumentId,
        digest: &str,
        bytes: Vec<u8>,
    ) -> NodeId {
        let hash = koru_core::BlobHash::new("b3", digest);
        let mut doc =
            WorkspaceDoc::from_snapshot(&self.snapshots.lock().unwrap()[&document]).unwrap();
        let root = doc.children(Parent::Root).unwrap()[0];
        let id = doc
            .insert(
                Parent::Node(root),
                Node::image(hash.clone(), "image/png", None, None).unwrap(),
            )
            .unwrap();
        self.snapshots
            .lock()
            .unwrap()
            .insert(document, doc.export_snapshot().unwrap());
        self.blob_bytes.lock().unwrap().insert(hash.0, bytes);
        id
    }

    /// Write the `Kind::Document` root and first paragraph — the opener's
    /// job in every app (ADR-80), done here so a note the tool just
    /// created can be read and edited the way a desktop-opened one is.
    pub(crate) fn seed_document_root(&self, document: StoreDocumentId, title: &str) {
        self.seed_root_and_line(document, title, "");
    }

    fn seed_root_and_line(&self, document: StoreDocumentId, title: &str, line: &str) {
        let mut doc =
            WorkspaceDoc::create(CoreWorkspaceId::from_uuid(self.workspace.as_uuid()), title)
                .unwrap();
        let root = doc
            .insert(Parent::Root, Node::document(title).unwrap())
            .unwrap();
        doc.insert(Parent::Node(root), Node::paragraph(line).unwrap())
            .unwrap();
        self.snapshots
            .lock()
            .unwrap()
            .insert(document, doc.export_snapshot().unwrap());
    }

    /// What a client would have read before writing — the revision and the
    /// first body line — taken from the store, because `read_document`
    /// refuses some of the documents these tests write to.
    pub fn stored(&self, document: StoreDocumentId) -> (String, NodeId) {
        let doc = WorkspaceDoc::from_snapshot(&self.snapshots.lock().unwrap()[&document]).unwrap();
        let root = doc.children(Parent::Root).unwrap()[0];
        let line = doc.children(Parent::Node(root)).unwrap()[0];
        (revision(&doc), line)
    }

    /// A history marker taken through the port itself, for a document no
    /// tool may mark.
    pub(crate) fn mark(&self, document: StoreDocumentId) -> HistoryEntry {
        self.capture_history(
            self.workspace,
            document,
            CaptureHistory {
                reason: "manual".into(),
                frontiers: None,
                label: None,
                message: None,
                boundary: None,
            },
        )
        .unwrap()
    }

    /// Everything a write could change: every stored document, the number
    /// of history markers and the number of manifest rows.
    pub(crate) fn state(&self) -> (HashMap<StoreDocumentId, Vec<u8>>, usize, usize) {
        (
            self.snapshots.lock().unwrap().clone(),
            self.markers.lock().unwrap().len(),
            self.entries.lock().unwrap().len(),
        )
    }

    pub fn backend(self: &Arc<Self>) -> LocalBackend {
        LocalBackend::with_embedded(self.clone())
    }

    /// The one workspace this store holds.
    pub fn workspace(&self) -> StoreWorkspaceId {
        self.workspace
    }

    /// Register a document with no content at all, as the Store's bare
    /// creation leaves one: a manifest row and an empty unit. For a caller
    /// that answers `EmbeddedStore::create_document` in its own wrapper; this
    /// store's own answer to that method stays a panic.
    pub fn register_empty(&self, title: &str, kind: &str) -> StoreDocumentId {
        let id = StoreDocumentId::new();
        self.entries.lock().unwrap().push(DocumentEntry {
            id,
            parent: None,
            ordinal: 0,
            title: title.into(),
            kind: kind.into(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        });
        let empty = loro::LoroDoc::new();
        self.snapshots
            .lock()
            .unwrap()
            .insert(id, empty.export(loro::ExportMode::Snapshot).unwrap());
        id
    }
}

impl EmbeddedStore for MemoryStore {
    fn workspaces(&self) -> Result<Vec<Workspace>, ToolError> {
        let mut workspace = Workspace::new(
            koru_store_core::ProfileId::new(),
            "Notes",
            koru_store_core::model::Timestamp::from_millis(1),
        )
        .unwrap();
        workspace.id = self.workspace;
        Ok(vec![workspace])
    }

    fn documents(&self, workspace: StoreWorkspaceId) -> Result<Vec<DocumentEntry>, ToolError> {
        assert_eq!(workspace, self.workspace);
        Ok(self.entries.lock().unwrap().clone())
    }

    fn search(
        &self,
        _request: SearchRequest,
    ) -> Result<koru_store_wire::search::SearchResponse, ToolError> {
        Ok(koru_store_wire::search::SearchResponse {
            page: koru_store_wire::search::SearchPage {
                hits: self.search_hits.lock().unwrap().clone(),
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

    /// An empty, projected page: what a swept workspace with nothing
    /// outstanding answers.
    fn query_tasks(&self, query: TaskQuery) -> Result<TaskQueryResponse, ToolError> {
        let limit = query.limit;
        self.task_queries.lock().unwrap().push(query);
        Ok(TaskQueryResponse {
            rows: Vec::new(),
            definitions: Vec::new(),
            tags: Vec::new(),
            groups: Vec::new(),
            projected: true,
            updating_workspaces: Vec::new(),
            limit,
            next_after: None,
            restarted: false,
            covered: None,
        })
    }

    fn snapshot(
        &self,
        _workspace: StoreWorkspaceId,
        document: StoreDocumentId,
    ) -> Result<Vec<u8>, ToolError> {
        self.snapshots
            .lock()
            .unwrap()
            .get(&document)
            .cloned()
            .ok_or_else(|| ToolError::invalid("the memory store has no such document"))
    }

    fn create_document(
        &self,
        _workspace: StoreWorkspaceId,
        _title: String,
        _kind: String,
        _attribution: Option<&str>,
    ) -> Result<StoreDocumentId, ToolError> {
        unreachable!("no test here creates a bare document, and a refused extract must not")
    }

    fn manifest_id(&self, _workspace: StoreWorkspaceId) -> Result<StoreDocumentId, ToolError> {
        unreachable!("no test here reports a manifest effect")
    }

    fn ensure_file_note(
        &self,
        workspace: StoreWorkspaceId,
        file: StoreDocumentId,
    ) -> Result<(StoreDocumentId, bool), ToolError> {
        assert_eq!(workspace, self.workspace);
        let mut entries = self.entries.lock().unwrap();
        let position = entries
            .iter()
            .position(|entry| entry.id == file)
            .ok_or_else(|| ToolError {
                code: "document_not_found".into(),
                message: "no such document".into(),
                details: None,
            })?;
        if let Some(note) = entries[position].note {
            return Ok((note, false));
        }
        if entries[position].kind != koru_core::explore::KIND_FILE {
            return Err(ToolError::invalid("a note can only be attached to a file"));
        }
        let note = StoreDocumentId::new();
        let title = entries[position].title.clone();
        entries[position].note = Some(note);
        entries.push(DocumentEntry {
            id: note,
            parent: Some(file),
            ordinal: 0,
            title,
            kind: koru_core::explore::KIND_NOTE.into(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        });
        drop(entries);
        // Registered empty, exactly as `Store::ensure_file_note` leaves it.
        let empty = loro::LoroDoc::new();
        self.snapshots
            .lock()
            .unwrap()
            .insert(note, empty.export(loro::ExportMode::Snapshot).unwrap());
        Ok((note, true))
    }

    /// The store's idempotency contract, in memory: a key seen with the
    /// same request hash replays the document it made, with a different
    /// one it is a conflict.
    fn seed_document(
        &self,
        request: EmbeddedSeedDocument,
    ) -> Result<EmbeddedSeededDocument, ToolError> {
        let mut seeds = self.seeds.lock().unwrap();
        if let Some((hash, id, update)) = seeds.get(&request.idempotency_key) {
            if *hash != request.request_hash {
                return Err(ToolError {
                    code: "idempotency_conflict".into(),
                    message: "that key made a different document".into(),
                    details: None,
                });
            }
            return Ok(EmbeddedSeededDocument {
                id: *id,
                initial_update: update.clone(),
                replayed: true,
            });
        }
        let id = StoreDocumentId::new();
        let doc = loro::LoroDoc::new();
        doc.import(&request.initial_update)
            .map_err(|error| ToolError::invalid(error.to_string()))?;
        self.snapshots.lock().unwrap().insert(
            id,
            doc.export(loro::ExportMode::Snapshot)
                .map_err(|error| ToolError::invalid(error.to_string()))?,
        );
        self.entries.lock().unwrap().push(DocumentEntry {
            id,
            parent: request.parent,
            ordinal: 0,
            title: request.title,
            kind: koru_core::explore::KIND_DOCUMENT.into(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        });
        seeds.insert(
            request.idempotency_key,
            (request.request_hash, id, request.initial_update.clone()),
        );
        Ok(EmbeddedSeededDocument {
            id,
            initial_update: request.initial_update,
            replayed: false,
        })
    }

    fn persist_update(
        &self,
        _workspace: StoreWorkspaceId,
        document: StoreDocumentId,
        update: Vec<u8>,
    ) -> Result<(), ToolError> {
        let doc = loro::LoroDoc::new();
        let mut snapshots = self.snapshots.lock().unwrap();
        let stored = snapshots
            .get(&document)
            .ok_or_else(|| ToolError::invalid("the memory store has no such document"))?;
        doc.import(stored)
            .and_then(|_| doc.import(&update))
            .map_err(|error| ToolError::invalid(error.to_string()))?;
        snapshots.insert(
            document,
            doc.export(loro::ExportMode::Snapshot)
                .map_err(|error| ToolError::invalid(error.to_string()))?,
        );
        Ok(())
    }

    /// A marker keeps the document as the store holds it, which is all
    /// the tools here need back: `history` lists it and
    /// `history_snapshot` opens it.
    fn capture_history(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
        request: CaptureHistory,
    ) -> Result<HistoryEntry, ToolError> {
        let snapshot = self.snapshot(workspace, document)?;
        // The entry this crate's tests have always been handed: a pinned,
        // labelled marker with a boundary.
        let entry = HistoryEntry {
            id: HistoryEntryId::new(),
            document,
            version: koru_store_core::ids::DocumentVersionId::new(),
            reason: request.reason,
            recorded_at: 1_700_000_000_000,
            label: Some("Before the rewrite".into()),
            message: None,
            boundary: Some(uuid::Uuid::new_v4().to_string()),
            pinned: true,
        };
        self.markers.lock().unwrap().push((entry.clone(), snapshot));
        Ok(entry)
    }

    fn history(
        &self,
        _workspace: StoreWorkspaceId,
        document: StoreDocumentId,
        _query: HistoryQuery,
    ) -> Result<HistoryPage, ToolError> {
        Ok(HistoryPage {
            entries: self
                .markers
                .lock()
                .unwrap()
                .iter()
                .filter(|(entry, _)| entry.document == document)
                .map(|(entry, _)| entry.clone())
                .collect(),
            next_before_at: None,
            next_before_id: None,
            stale: None,
        })
    }

    fn history_snapshot(
        &self,
        _workspace: StoreWorkspaceId,
        entry: HistoryEntryId,
    ) -> Result<Vec<u8>, ToolError> {
        self.markers
            .lock()
            .unwrap()
            .iter()
            .find(|(marker, _)| marker.id == entry)
            .map(|(_, snapshot)| snapshot.clone())
            .ok_or_else(|| ToolError::invalid("the memory store has no such history entry"))
    }

    fn blobs(&self, _workspace: StoreWorkspaceId) -> Result<Vec<Blob>, ToolError> {
        Ok(self.blobs.lock().unwrap().clone())
    }

    fn get_blob(
        &self,
        _workspace: StoreWorkspaceId,
        hash: koru_store_core::model::BlobHash,
    ) -> Result<Vec<u8>, ToolError> {
        self.blob_bytes
            .lock()
            .unwrap()
            .get(hash.as_str())
            .cloned()
            .ok_or_else(|| ToolError::invalid("the memory store holds no such blob"))
    }

    fn status(&self) -> Result<Status, ToolError> {
        unreachable!("the note tests never ask for status")
    }
}
