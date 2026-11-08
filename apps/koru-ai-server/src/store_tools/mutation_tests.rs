// SPDX-License-Identifier: MPL-2.0

//! The mutation path's evidence, over a real Store kept in memory and this
//! crate's own port onto it (Cloud MCP Plan 2.1, TECHNICAL §14 and §16):
//!
//! - **local history with deferred stamps (S3)**, on a local host: every
//!   write path on a document missing both id stamps succeeds, its history
//!   entries open and restore, and a refused call leaves no stamp and no
//!   entry;
//! - **effects (R1) at the port**: a port that refuses a chosen write, and
//!   the per-effect answer each refusal earns. What Cloud then holds is Plan
//!   2.3's half;
//! - **the attribution sweep**: every commit a connection's call makes, in
//!   every unit it touches, carries its `actor/1` message — the Store-made
//!   ones included — and no Koru path writes one of its own.
//!
//! No disk: every Store here is a `MemoryStore`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use koru_core::{Node, Parent, SystemDoc, WorkspaceDoc};
use koru_mcp_server::effects::{Effect, EffectKind, EffectState};
use koru_mcp_server::{ActorMessage, ChangeAttribution, HostCapabilities};
use koru_store_core::Locale;
use koru_store_server::memory::MemoryStore;
use serde_json::{Value, json};

use super::*;

// ------------------------------------------------------------------ the port

/// What the next writes through [`FaultyPort`] should meet.
#[derive(Default)]
struct Faults {
    /// Refuse the n-th `persist_update` from now (1-based) with this code,
    /// before it reaches the Store.
    persist: Mutex<Option<(usize, String)>>,
    persists: AtomicUsize,
    /// The next seed or new page is kept, and its manifest entry reported
    /// not saved — what a hosted port answers when Cloud refuses the entry.
    manifest_refused: AtomicBool,
    /// Every write the port was asked for, and every history capture.
    writes: AtomicUsize,
    captures: AtomicUsize,
}

impl Faults {
    fn refuse_persist(&self, nth: usize, code: &str) {
        self.persists.store(0, Ordering::SeqCst);
        *self.persist.lock().unwrap() = Some((nth, code.to_owned()));
    }
}

/// [`WorkerEmbeddedStore`], with a refusal wherever a test arms one.
struct FaultyPort {
    inner: WorkerEmbeddedStore,
    faults: Arc<Faults>,
}

impl FaultyPort {
    /// The two-unit report a hosted port gives when the content landed and
    /// the manifest entry did not.
    fn manifest_refused(&self, workspace: WorkspaceId, document: DocumentId) -> ToolError {
        let manifest = self.inner.manifest_id(workspace).ok();
        ToolError {
            code: koru_mcp_server::effects::EDIT_NOT_SAVED.into(),
            message: "Cloud refused the manifest entry".into(),
            details: None,
        }
        .with_effects(&[
            Effect {
                effect: EffectKind::Document,
                document_id: Some(document.to_storage()),
                state: EffectState::Saved,
            },
            Effect {
                effect: EffectKind::Manifest,
                document_id: manifest.map(|id| id.to_storage()),
                state: EffectState::NotSaved,
            },
        ])
    }
}

impl EmbeddedStore for FaultyPort {
    fn workspaces(&self) -> Result<Vec<Workspace>, ToolError> {
        self.inner.workspaces()
    }
    fn documents(&self, workspace: WorkspaceId) -> Result<Vec<DocumentEntry>, ToolError> {
        self.inner.documents(workspace)
    }
    fn search(&self, request: SearchRequest) -> Result<SearchResponse, ToolError> {
        self.inner.search(request)
    }
    fn query_tasks(&self, query: TaskQuery) -> Result<TaskQueryResponse, ToolError> {
        self.inner.query_tasks(query)
    }
    fn snapshot(&self, workspace: WorkspaceId, document: DocumentId) -> Result<Vec<u8>, ToolError> {
        self.inner.snapshot(workspace, document)
    }
    fn create_document(
        &self,
        workspace: WorkspaceId,
        title: String,
        kind: String,
        attribution: Option<&str>,
    ) -> Result<DocumentId, ToolError> {
        self.faults.writes.fetch_add(1, Ordering::SeqCst);
        let id = self
            .inner
            .create_document(workspace, title, kind, attribution)?;
        if self.faults.manifest_refused.swap(false, Ordering::SeqCst) {
            return Err(self.manifest_refused(workspace, id));
        }
        Ok(id)
    }
    fn manifest_id(&self, workspace: WorkspaceId) -> Result<DocumentId, ToolError> {
        self.inner.manifest_id(workspace)
    }
    fn ensure_file_note(
        &self,
        workspace: WorkspaceId,
        file: DocumentId,
    ) -> Result<(DocumentId, bool), ToolError> {
        self.inner.ensure_file_note(workspace, file)
    }
    fn seed_document(
        &self,
        request: EmbeddedSeedDocument,
    ) -> Result<EmbeddedSeededDocument, ToolError> {
        self.faults.writes.fetch_add(1, Ordering::SeqCst);
        let workspace = request.workspace;
        let seeded = self.inner.seed_document(request)?;
        if self.faults.manifest_refused.swap(false, Ordering::SeqCst) {
            return Err(self.manifest_refused(workspace, seeded.id));
        }
        Ok(seeded)
    }
    fn persist_update(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
        update: Vec<u8>,
    ) -> Result<(), ToolError> {
        self.faults.writes.fetch_add(1, Ordering::SeqCst);
        let nth = self.faults.persists.fetch_add(1, Ordering::SeqCst) + 1;
        let mut armed = self.faults.persist.lock().unwrap();
        if armed.as_ref().is_some_and(|(at, _)| *at == nth) {
            let (_, code) = armed.take().unwrap();
            return Err(ToolError {
                code,
                message: "refused at the port".into(),
                details: None,
            });
        }
        drop(armed);
        self.inner.persist_update(workspace, document, update)
    }
    fn capture_history(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
        request: CaptureHistory,
    ) -> Result<HistoryEntry, ToolError> {
        self.faults.captures.fetch_add(1, Ordering::SeqCst);
        self.inner.capture_history(workspace, document, request)
    }
    fn history(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
        query: HistoryQuery,
    ) -> Result<HistoryPage, ToolError> {
        self.inner.history(workspace, document, query)
    }
    fn history_snapshot(
        &self,
        workspace: WorkspaceId,
        entry: HistoryEntryId,
    ) -> Result<Vec<u8>, ToolError> {
        self.inner.history_snapshot(workspace, entry)
    }
    fn blobs(&self, workspace: WorkspaceId) -> Result<Vec<Blob>, ToolError> {
        self.inner.blobs(workspace)
    }
    fn get_blob(&self, workspace: WorkspaceId, hash: BlobHash) -> Result<Vec<u8>, ToolError> {
        self.inner.get_blob(workspace, hash)
    }
    fn status(&self) -> Result<Status, ToolError> {
        self.inner.status()
    }
}

// ------------------------------------------------------------- the fixture

/// One workspace in a Store in memory, and the shared tools over the port.
struct Fixture {
    /// Held for the fixture's life; dropping it stops the composition.
    _memory: MemoryStore,
    client: StoreWorkerClient,
    faults: Arc<Faults>,
    tools: WorkspaceToolExecutor,
    workspace: WorkspaceId,
}

impl Fixture {
    fn new() -> Self {
        let memory = MemoryStore::open("internal-debug").unwrap();
        let client = memory.client();
        let workspace = client
            .with_store(|store| {
                let at = Timestamp::from_millis(1);
                let profile =
                    store.create_profile("Test", None, None, Locale::parse("en").unwrap(), at)?;
                Ok::<_, koru_store_core::Error>(store.create_workspace(profile.id, "Work", at)?.id)
            })
            .unwrap()
            .unwrap();
        let faults = Arc::new(Faults::default());
        let port = FaultyPort {
            inner: WorkerEmbeddedStore {
                client: client.clone(),
            },
            faults: Arc::clone(&faults),
        };
        let tools =
            WorkspaceToolExecutor::new(Arc::new(LocalBackend::with_embedded(Arc::new(port))));
        Self {
            _memory: memory,
            client,
            faults,
            tools,
            workspace,
        }
    }

    fn store<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut koru_store_server::WorkerStore) -> Result<T, koru_store_core::Error>
        + Send
        + 'static,
    ) -> T {
        self.client.with_store(operation).unwrap().unwrap()
    }

    /// A local host: Koru AI's own context, history on.
    fn local(&self) -> RequestContext {
        RequestContext::selected(
            "local",
            "test",
            [Scope::ReadDocuments, Scope::EditDocuments],
            [self.workspace],
        )
    }

    /// A hosted call through an app connection: rung 1's tools, attributed.
    fn hosted(&self) -> RequestContext {
        self.local()
            .with_capabilities(HostCapabilities::rung1())
            .with_attribution(connection())
    }

    fn call(
        &self,
        context: &RequestContext,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, ToolError> {
        let mut arguments = arguments;
        arguments["workspace_id"] = json!(self.workspace.to_storage());
        self.tools.invoke(context, tool, arguments)
    }

    /// A valid document **missing both id stamps**: what the Store hands out
    /// and an app seeds without stamping. Its root and one paragraph per line.
    fn unstamped(&self, title: &str, lines: &[&str]) -> (DocumentId, Vec<koru_core::NodeId>) {
        let workspace = self.workspace;
        let id = flatten(self.client.create_document(
            workspace,
            title.into(),
            "page".into(),
            Timestamp::now(),
        ))
        .unwrap();
        let mut doc =
            WorkspaceDoc::from_snapshot(&self.store(move |store| store.snapshot(workspace, id)))
                .unwrap();
        let root = doc
            .insert(Parent::Root, Node::document(title).unwrap())
            .unwrap();
        let lines = lines
            .iter()
            .map(|line| {
                doc.insert(Parent::Node(root), Node::paragraph(*line).unwrap())
                    .unwrap()
            })
            .collect();
        flatten(self.client.apply_document_update(
            workspace,
            id,
            doc.export_all_updates().unwrap(),
            Timestamp::now(),
        ))
        .unwrap();
        assert_eq!(self.stamps(id), (false, false), "seeded unstamped");
        (id, lines)
    }

    fn doc(&self, document: DocumentId) -> WorkspaceDoc {
        let workspace = self.workspace;
        WorkspaceDoc::from_snapshot(&self.store(move |store| store.snapshot(workspace, document)))
            .unwrap()
    }

    fn bytes(&self, document: DocumentId) -> Vec<u8> {
        let workspace = self.workspace;
        self.store(move |store| store.snapshot(workspace, document))
    }

    /// Whether the stored document carries its workspace id and its document
    /// id.
    fn stamps(&self, document: DocumentId) -> (bool, bool) {
        stamps_of(&self.doc(document))
    }

    fn revision(&self, document: DocumentId) -> String {
        self.call(
            &self.local(),
            "read_document",
            json!({ "document_id": document.to_storage() }),
        )
        .unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// Every text in the stored document, in tree order.
    fn texts(&self, document: DocumentId) -> Vec<String> {
        let doc = self.doc(document);
        doc.walk_all()
            .unwrap()
            .into_iter()
            .filter_map(|(node, _)| {
                doc.read(node)
                    .ok()
                    .and_then(|value| value.content.as_text().map(str::to_owned))
            })
            .collect()
    }

    fn history(&self, document: DocumentId) -> Vec<koru_store_core::ports::HistoryEntry> {
        let workspace = self.workspace;
        self.store(move |store| store.history_entries(workspace, document, None, 200))
    }

    fn history_snapshot(&self, entry: HistoryEntryId) -> Vec<u8> {
        let workspace = self.workspace;
        self.store(move |store| store.history_version(workspace, entry).map(|v| v.snapshot))
    }

    fn system_tags(&self) -> Vec<String> {
        let workspace = self.workspace;
        let Some(system) = self
            .store(move |store| store.documents(workspace))
            .into_iter()
            .find(|entry| entry.kind == koru_core::explore::KIND_SYSTEM)
        else {
            return Vec::new();
        };
        SystemDoc::from_snapshot(&self.bytes(system.id))
            .unwrap()
            .tags()
            .into_iter()
            .map(|tag| tag.name)
            .collect()
    }

    fn listed(&self, document: DocumentId) -> bool {
        let workspace = self.workspace;
        self.store(move |store| store.documents(workspace))
            .iter()
            .any(|entry| entry.id == document)
    }

    /// Every unit the workspace has — its manifest and every document, the
    /// system document included — by id, each as a Loro document.
    fn units(&self) -> Vec<(DocumentId, loro::LoroDoc)> {
        let workspace = self.workspace;
        let manifest = self.store(move |store| store.manifest_id(workspace));
        let mut ids = vec![manifest];
        ids.extend(
            self.store(move |store| store.documents(workspace))
                .into_iter()
                .map(|entry| entry.id),
        );
        ids.into_iter()
            .map(|id| {
                let doc = loro::LoroDoc::new();
                doc.import(&self.bytes(id)).unwrap();
                (id, doc)
            })
            .collect()
    }

    fn versions(&self) -> HashMap<DocumentId, loro::VersionVector> {
        self.units()
            .into_iter()
            .map(|(id, doc)| (id, doc.oplog_vv()))
            .collect()
    }

    /// Every change made since `before`, in every unit, with its message.
    fn changes_since(
        &self,
        before: &HashMap<DocumentId, loro::VersionVector>,
    ) -> Vec<(DocumentId, Option<String>)> {
        let mut changes = Vec::new();
        for (id, doc) in self.units() {
            let base = before.get(&id).cloned().unwrap_or_default();
            for (last, message) in all_changes(&doc) {
                if !base.includes_id(last) {
                    changes.push((id, message));
                }
            }
        }
        changes
    }
}

fn stamps_of(doc: &WorkspaceDoc) -> (bool, bool) {
    (
        doc.id().is_some(),
        doc.doc().get_map("meta").get("document").is_some(),
    )
}

/// Every change in `doc`, by the id of its last operation, with its message.
fn all_changes(doc: &loro::LoroDoc) -> Vec<(loro::ID, Option<String>)> {
    let ids: Vec<loro::ID> = doc.oplog_frontiers().iter().collect();
    let mut changes = Vec::new();
    doc.travel_change_ancestors(&ids, &mut |change| {
        let last = loro::ID::new(change.id.peer, change.id.counter + change.len as i32 - 1);
        changes.push((last, change.message.as_deref().map(str::to_owned)));
        std::ops::ControlFlow::Continue(())
    })
    .unwrap();
    changes
}

fn connection() -> ChangeAttribution {
    ChangeAttribution {
        connection: uuid::Uuid::new_v4(),
        name: "Claude – work".into(),
        client: "https://claude.ai/oauth/client.json".into(),
        verified: true,
        account: uuid::Uuid::new_v4(),
    }
}

fn edit(document: DocumentId, revision: &str, operations: Value) -> Value {
    json!({
        "document_id": document.to_storage(),
        "base_revision": revision,
        "operations": operations,
    })
}

fn replace(node: koru_core::NodeId, expected: &str, text: &str) -> Value {
    json!({ "op": "replace_text", "node_id": node.to_storage(), "expected": expected, "text": text })
}

fn add_tag(node: koru_core::NodeId, expected: &str, tag: &str) -> Value {
    json!({ "op": "add_tag", "node_id": node.to_storage(), "expected": expected, "tag": tag })
}

fn due(node: koru_core::NodeId, expected: &str) -> Value {
    json!({
        "op": "set_attribute", "node_id": node.to_storage(), "expected": expected,
        "key": "koru.due", "value": {"type": "date", "value": "2026-11-20"},
    })
}

fn extract(
    source: DocumentId,
    revision: &str,
    node: koru_core::NodeId,
    expected: &str,
    target: Option<DocumentId>,
) -> Value {
    json!({
        "source_document_id": source.to_storage(),
        "base_revision": revision,
        "source_node_id": node.to_storage(),
        "expected": expected,
        "target_document_id": target.map(|id| id.to_storage()),
        "new_document_title": target.is_none().then_some("Moved out"),
    })
}

fn effects_of(error: &ToolError) -> Vec<(String, String)> {
    error.details.as_ref().expect("an envelope")["effects"]
        .as_array()
        .expect("effects")
        .iter()
        .map(|effect| {
            (
                effect["effect"].as_str().unwrap().to_owned(),
                effect["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected
        .iter()
        .map(|(effect, state)| ((*effect).to_owned(), (*state).to_owned()))
        .collect()
}

// ---------------------------------------------- local history, deferred (S3)

/// An entry's state opens, and carries what it should.
#[track_caller]
fn opens(fixture: &Fixture, entry: &Value) -> WorkspaceDoc {
    let id = HistoryEntryId::parse(entry["id"].as_str().expect("an entry id")).unwrap();
    WorkspaceDoc::from_snapshot(&fixture.history_snapshot(id)).expect("the entry opens")
}

/// Restore `entry` through the tool, and the document still carries both ids.
#[track_caller]
fn restores(fixture: &Fixture, document: DocumentId, entry: &str) {
    let revision = fixture.revision(document);
    fixture
        .call(
            &fixture.local(),
            "restore_document_version",
            json!({
                "document_id": document.to_storage(),
                "base_revision": revision,
                "entry_id": entry,
            }),
        )
        .expect("the entry restores");
    assert_eq!(
        fixture.stamps(document),
        (true, true),
        "a restore keeps both ids"
    );
}

/// **S3, edit and a task change through edit:** on a document missing both
/// stamps, each succeeds in one write; its before-entry is the document as the
/// Store held it — no stamps — and its after-entry the edit with them; both
/// open and restore, and the document keeps both ids after restoring the
/// before-entry (TECHNICAL §16).
#[test]
fn an_edit_and_a_task_change_on_an_unstamped_document_keep_their_history() {
    let fixture = Fixture::new();
    let (page, lines) = fixture.unstamped("Plan", &["Draft the letter", "Post it"]);
    let revision = fixture.revision(page);

    let answer = fixture
        .call(
            &fixture.local(),
            "edit_document",
            edit(
                page,
                &revision,
                json!([replace(lines[0], "Draft the letter", "Write the letter")]),
            ),
        )
        .expect("the first edit of an unstamped document succeeds");
    assert_eq!(
        fixture.stamps(page),
        (true, true),
        "the stamps rode the edit"
    );
    let history = fixture.history(page);
    assert_eq!(history.len(), 2, "mcp_before and mcp_after");
    let before = history
        .iter()
        .find(|entry| entry.reason.as_str() == "mcp_before")
        .expect("a before-entry");
    let before_state = WorkspaceDoc::from_snapshot(&fixture.history_snapshot(before.id)).unwrap();
    assert_eq!(
        stamps_of(&before_state),
        (false, false),
        "the before-entry is the document as the Store held it"
    );
    let after_state = opens(&fixture, &answer["history"]);
    assert_eq!(stamps_of(&after_state), (true, true));

    restores(&fixture, page, &before.id.to_storage());
    assert!(
        fixture.texts(page).contains(&"Draft the letter".to_owned()),
        "{:?}",
        fixture.texts(page)
    );

    // A task change through edit is the case that needed the workspace id:
    // step 0 of every task command reads it, on a document that has none.
    let (tasks, lines) = fixture.unstamped("Errands", &["Buy stamps"]);
    let revision = fixture.revision(tasks);
    let answer = fixture
        .call(
            &fixture.local(),
            "edit_document",
            edit(tasks, &revision, json!([due(lines[0], "Buy stamps")])),
        )
        .expect("a due on an unstamped document makes a task");
    assert_eq!(fixture.stamps(tasks), (true, true));
    opens(&fixture, &answer["history"]);
    let before = fixture
        .history(tasks)
        .into_iter()
        .find(|entry| entry.reason.as_str() == "mcp_before")
        .unwrap();
    restores(&fixture, tasks, &before.id.to_storage());
}

/// **S3, extraction:** into a new destination and into an existing one, from
/// and into documents missing both stamps. Each end's entries open and
/// restore, and every document involved carries both ids after.
#[test]
fn an_extraction_between_unstamped_documents_keeps_its_history() {
    let fixture = Fixture::new();
    for into_existing in [false, true] {
        let (source, lines) = fixture.unstamped("Notes", &["Keep", "Move me"]);
        let target = into_existing.then(|| fixture.unstamped("Archive", &["Old"]).0);
        let revision = fixture.revision(source);
        let answer = fixture
            .call(
                &fixture.local(),
                "extract_to_document",
                extract(source, &revision, lines[1], "Move me", target),
            )
            .expect("an extraction between unstamped documents succeeds");
        let destination = DocumentId::parse(answer["document_id"].as_str().unwrap()).unwrap();
        for document in [source, destination] {
            assert_eq!(fixture.stamps(document), (true, true), "{answer}");
        }
        assert!(fixture.texts(destination).contains(&"Move me".to_owned()));
        for (document, side) in [(destination, "document"), (source, "source")] {
            opens(&fixture, &answer["history"][side]);
            let entries = fixture.history(document);
            assert_eq!(entries.len(), 2, "{side}: mcp_before and mcp_after");
            let before = entries
                .iter()
                .find(|entry| entry.reason.as_str() == "mcp_before")
                .unwrap();
            restores(&fixture, document, &before.id.to_storage());
        }
    }
}

/// **S3, `restore_document_version` itself**, on a document missing both
/// stamps: a marker saved before any edit restores, and the document carries
/// both ids after.
#[test]
fn a_restore_on_an_unstamped_document_stamps_it() {
    let fixture = Fixture::new();
    let (page, _) = fixture.unstamped("Plan", &["As it was"]);
    let saved = fixture
        .call(
            &fixture.local(),
            "save_document_version",
            json!({ "document_id": page.to_storage(), "label": "before" }),
        )
        .unwrap();
    restores(&fixture, page, saved["id"].as_str().unwrap());
    assert_eq!(
        fixture.history(page).len(),
        3,
        "the marker, restore_before, restore_after"
    );
}

/// **S3's refusals write nothing:** a stale base, and an extraction whose
/// precondition fails, on documents missing both stamps — no stamp, no
/// history entry, not a byte changed.
#[test]
fn a_refused_call_on_an_unstamped_document_leaves_no_stamp_and_no_entry() {
    let fixture = Fixture::new();
    let (page, lines) = fixture.unstamped("Plan", &["Line"]);
    let stale = {
        let mut ahead = fixture.doc(page).fork_for_read();
        ahead
            .stamp_document_id(koru_core::DocumentId::new())
            .unwrap();
        ahead
            .version()
            .encode()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let stored = fixture.bytes(page);
    let refused = fixture
        .call(
            &fixture.local(),
            "edit_document",
            edit(page, &stale, json!([replace(lines[0], "Line", "Changed")])),
        )
        .unwrap_err();
    assert_eq!(refused.code, "revision_conflict");
    assert_eq!(fixture.bytes(page), stored, "not even a stamp");
    assert!(fixture.history(page).is_empty());

    let revision = fixture.revision(page);
    let refused = fixture
        .call(
            &fixture.local(),
            "extract_to_document",
            extract(page, &revision, lines[0], "Not the text", None),
        )
        .unwrap_err();
    assert_eq!(refused.code, "precondition_failed");
    assert_eq!(fixture.bytes(page), stored);
    assert!(fixture.history(page).is_empty());
    assert_eq!(
        fixture.faults.writes.load(Ordering::SeqCst),
        0,
        "no write at all"
    );
}

// -------------------------------------------------------- effects at the port

/// **R1: a tag created, then the body refused.** `partially_saved`, the
/// system document saved and the document not; the tag is there, the body is
/// unchanged, and the assistant is told a retry reuses the tag.
#[test]
fn a_refused_body_after_a_created_tag_is_partially_saved() {
    let fixture = Fixture::new();
    let (page, lines) = fixture.unstamped("Plan", &["Ship it"]);
    // The first edit makes the workspace's system document; the second
    // creates a tag in the one that exists.
    let revision = fixture.revision(page);
    fixture
        .call(
            &fixture.hosted(),
            "edit_document",
            edit(
                page,
                &revision,
                json!([add_tag(lines[0], "Ship it", "first")]),
            ),
        )
        .unwrap();
    let revision = fixture.revision(page);
    let stored = fixture.bytes(page);
    fixture.faults.refuse_persist(2, "storage_failed");
    let error = fixture
        .call(
            &fixture.hosted(),
            "edit_document",
            edit(
                page,
                &revision,
                json!([
                    add_tag(lines[0], "Ship it", "urgent"),
                    replace(lines[0], "Ship it", "Ship it today")
                ]),
            ),
        )
        .unwrap_err();
    assert_eq!(error.code, "partially_saved", "{error:?}");
    assert_eq!(
        effects_of(&error),
        pairs(&[("system", "saved"), ("document", "not_saved")])
    );
    assert!(error.message.contains("existing tags are reused"));
    assert!(fixture.system_tags().contains(&"urgent".to_owned()));
    assert_eq!(fixture.bytes(page), stored, "the body is unchanged");
    let details = error.details.unwrap();
    assert!(
        uuid::Uuid::parse_str(details["operation_id"].as_str().unwrap()).is_ok(),
        "{details}"
    );
}

/// **R1: a stale base on a document that still needs its stamps** is
/// `revision_conflict`, and no write reaches the port — the stamp included.
/// So is extraction's `precondition_failed` on a never-stamped source.
#[test]
fn a_hosted_refusal_before_the_first_write_writes_nothing() {
    let fixture = Fixture::new();
    let (page, lines) = fixture.unstamped("Plan", &["Line"]);
    let stale = {
        let mut ahead = fixture.doc(page).fork_for_read();
        ahead
            .stamp_document_id(koru_core::DocumentId::new())
            .unwrap();
        ahead
            .version()
            .encode()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let error = fixture
        .call(
            &fixture.hosted(),
            "edit_document",
            edit(page, &stale, json!([replace(lines[0], "Line", "Changed")])),
        )
        .unwrap_err();
    assert_eq!(error.code, "revision_conflict");
    let revision = fixture.revision(page);
    let error = fixture
        .call(
            &fixture.hosted(),
            "extract_to_document",
            extract(page, &revision, lines[0], "Not the text", None),
        )
        .unwrap_err();
    assert_eq!(error.code, "precondition_failed");
    assert_eq!(fixture.faults.writes.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.stamps(page), (false, false));
}

/// **R1: create, then the manifest entry refused.** `partially_saved`, the
/// document saved, the entry not, the key answered; a retry with that key
/// finishes it as the one document.
#[test]
fn a_refused_manifest_entry_is_partially_saved_and_the_key_finishes_it() {
    let fixture = Fixture::new();
    fixture
        .faults
        .manifest_refused
        .store(true, Ordering::SeqCst);
    let request = json!({ "title": "Minutes" });
    let error = fixture
        .call(&fixture.hosted(), "create_document", request.clone())
        .unwrap_err();
    assert_eq!(error.code, "partially_saved", "{error:?}");
    assert_eq!(
        effects_of(&error),
        pairs(&[("document", "saved"), ("manifest", "not_saved")])
    );
    let details = error.details.as_ref().unwrap();
    let key = details["idempotency_key"]
        .as_str()
        .expect("a hosted creation always answers its key");
    let created = details["effects"][0]["document_id"].as_str().unwrap();
    assert!(
        details["effects"][1]["document_id"].is_string(),
        "the manifest is named"
    );

    let mut retry = request;
    retry["idempotency_key"] = json!(key);
    let finished = fixture
        .call(&fixture.hosted(), "create_document", retry)
        .unwrap();
    assert_eq!(finished["document_id"], created, "one document");
    assert_eq!(finished["idempotency_key"], key);
    assert!(fixture.listed(DocumentId::parse(created).unwrap()));
}

/// **R1: extraction into an existing destination, the source refused.** The
/// content is in both, `source_unchanged: true`, with the recovery fields.
#[test]
fn a_refused_source_after_the_destination_is_partially_saved_with_recovery() {
    let fixture = Fixture::new();
    let (source, lines) = fixture.unstamped("Notes", &["Move me"]);
    let (target, _) = fixture.unstamped("Archive", &["Old"]);
    let revision = fixture.revision(source);
    let stored = fixture.bytes(source);
    fixture.faults.refuse_persist(2, "storage_failed");
    let error = fixture
        .call(
            &fixture.hosted(),
            "extract_to_document",
            extract(source, &revision, lines[0], "Move me", Some(target)),
        )
        .unwrap_err();
    assert_eq!(error.code, "partially_saved", "{error:?}");
    assert_eq!(
        effects_of(&error),
        pairs(&[("destination", "saved"), ("source", "not_saved")])
    );
    let details = error.details.as_ref().unwrap();
    assert_eq!(details["source_unchanged"], true);
    assert_eq!(details["recovery_required"], true);
    assert_eq!(details["target_document_id"], target.to_storage());
    assert_eq!(details["created_document_id"], Value::Null);
    assert!(fixture.texts(target).contains(&"Move me".to_owned()));
    assert_eq!(fixture.bytes(source), stored, "the source is unchanged");
}

/// **R1: extraction into a new destination, each later effect refused in
/// turn** — the manifest entry, the moved content, the source — and an
/// unconfirmed source write, which is `save_status_unknown`.
#[test]
fn a_new_destination_reports_every_saved_prefix() {
    let fixture = Fixture::new();
    let run = |arm: &dyn Fn(&Faults)| {
        let (source, lines) = fixture.unstamped("Notes", &["Move me"]);
        let revision = fixture.revision(source);
        arm(&fixture.faults);
        let error = fixture
            .call(
                &fixture.hosted(),
                "extract_to_document",
                extract(source, &revision, lines[0], "Move me", None),
            )
            .unwrap_err();
        (source, error)
    };

    let (_, error) = run(&|faults| faults.manifest_refused.store(true, Ordering::SeqCst));
    assert_eq!(error.code, "partially_saved", "{error:?}");
    assert_eq!(
        effects_of(&error),
        pairs(&[
            ("destination_seed", "saved"),
            ("manifest", "not_saved"),
            ("destination", "not_saved"),
            ("source", "not_saved"),
        ])
    );
    assert!(
        error.message.contains("not listed yet"),
        "{}",
        error.message
    );
    let details = error.details.as_ref().unwrap();
    assert_eq!(
        details["created_document_id"],
        details["target_document_id"]
    );
    assert_eq!(details["source_unchanged"], true);

    let (_, error) = run(&|faults| faults.refuse_persist(1, "storage_failed"));
    assert_eq!(
        effects_of(&error),
        pairs(&[
            ("destination_seed", "saved"),
            ("manifest", "saved"),
            ("destination", "not_saved"),
            ("source", "not_saved"),
        ])
    );
    assert!(
        error.message.contains("delete the page"),
        "{}",
        error.message
    );

    let (source, error) = run(&|faults| faults.refuse_persist(2, "storage_failed"));
    assert_eq!(
        effects_of(&error),
        pairs(&[
            ("destination_seed", "saved"),
            ("manifest", "saved"),
            ("destination", "saved"),
            ("source", "not_saved"),
        ])
    );
    assert!(
        error.message.contains("in both documents"),
        "{}",
        error.message
    );
    assert_eq!(
        fixture.stamps(source),
        (false, false),
        "the source is untouched"
    );

    let (_, error) = run(&|faults| faults.refuse_persist(2, "save_status_unknown"));
    assert_eq!(error.code, "save_status_unknown");
    assert_eq!(
        error.message,
        koru_mcp_server::effects::SAVE_STATUS_UNKNOWN_MESSAGE
    );
    assert_eq!(
        effects_of(&error).last().unwrap(),
        &pair("source", "unknown")
    );
}

fn pair(effect: &str, state: &str) -> (String, String) {
    (effect.to_owned(), state.to_owned())
}

/// **No hosted answer carries `history`, and no capture is written** — while
/// the same calls on a local host keep both. The task log is a device's too.
#[test]
fn a_hosted_call_captures_no_history() {
    let fixture = Fixture::new();
    let (page, lines) = fixture.unstamped("Plan", &["Line", "Moved"]);
    let (target, _) = fixture.unstamped("Archive", &["Old"]);
    let hosted = fixture.hosted();
    let revision = fixture.revision(page);
    let edited = fixture
        .call(
            &hosted,
            "edit_document",
            edit(page, &revision, json!([due(lines[0], "Line")])),
        )
        .unwrap();
    let revision = fixture.revision(page);
    let extracted = fixture
        .call(
            &hosted,
            "extract_to_document",
            extract(page, &revision, lines[1], "Moved", Some(target)),
        )
        .unwrap();
    let created = fixture
        .call(&hosted, "create_document", json!({ "title": "New" }))
        .unwrap();
    for answer in [&edited, &extracted, &created] {
        assert!(answer.get("history").is_none(), "{answer}");
    }
    assert!(
        uuid::Uuid::parse_str(created["idempotency_key"].as_str().unwrap()).is_ok(),
        "a hosted creation answers the key it minted"
    );
    assert_eq!(fixture.faults.captures.load(Ordering::SeqCst), 0);
    assert!(fixture.history(page).is_empty());

    // The local catalog is unchanged: history, and no key it was not sent.
    let local = fixture
        .call(
            &fixture.local(),
            "create_document",
            json!({ "title": "Local" }),
        )
        .unwrap();
    assert!(local.get("idempotency_key").is_none(), "{local}");
    assert_eq!(fixture.faults.captures.load(Ordering::SeqCst), 2);
}

// ------------------------------------------------------ the attribution sweep

/// Every change since `before`, which must all carry this connection's
/// `actor/1` and one operation id; answers how many there were per unit.
#[track_caller]
fn attributed(
    fixture: &Fixture,
    before: &HashMap<DocumentId, loro::VersionVector>,
    connection: &ChangeAttribution,
) -> HashMap<DocumentId, Vec<ActorMessage>> {
    let mut operation = None;
    let mut found: HashMap<DocumentId, Vec<ActorMessage>> = HashMap::new();
    for (unit, message) in fixture.changes_since(before) {
        let message = message.unwrap_or_else(|| panic!("an unattributed commit in {unit}"));
        let actor =
            ActorMessage::read(&message).unwrap_or_else(|| panic!("not actor/1: {message}"));
        assert_eq!(actor.connection, connection.connection);
        assert_eq!(actor.account, connection.account);
        assert_eq!(actor.name, connection.name);
        assert_eq!(
            *operation.get_or_insert(actor.op),
            actor.op,
            "one op per call"
        );
        found.entry(unit).or_default().push(actor);
    }
    assert!(!found.is_empty(), "the call wrote nothing");
    found
}

/// **The attribution sweep (TECHNICAL §5.4 item 2, §16).** Every rung-1
/// mutation, through an app connection's context: every commit it makes, in
/// every unit — the document, the system document, the manifest entries the
/// Store makes for a created document, an extraction's destination and a new
/// system document — carries the connection's `actor/1` with the call's one
/// `op`; a created document's seed, and only its seed, adds
/// `create.request_hash`. Walking every change of every unit, rather than the
/// ones expected, is what proves no commit site was missed.
#[test]
fn every_commit_a_connection_makes_is_attributed() {
    let fixture = Fixture::new();
    let connection = connection();
    let hosted = fixture
        .local()
        .with_capabilities(HostCapabilities::rung1())
        .with_attribution(connection.clone());
    let workspace = fixture.workspace;
    let manifest = fixture.store(move |store| store.manifest_id(workspace));
    let (page, lines) = fixture.unstamped("Plan", &["Ship it", "Move me"]);
    let (target, _) = fixture.unstamped("Archive", &["Old"]);

    // Create: the seed, and the Store's manifest entry for it.
    let before = fixture.versions();
    let created = fixture
        .call(&hosted, "create_document", json!({ "title": "Minutes" }))
        .unwrap();
    let created = DocumentId::parse(created["document_id"].as_str().unwrap()).unwrap();
    let found = attributed(&fixture, &before, &connection);
    assert!(
        found[&created].iter().all(|actor| actor.create.is_some()),
        "the seed"
    );
    assert!(
        found[&manifest].iter().all(|actor| actor.create.is_none()),
        "the entry"
    );
    assert_eq!(found.len(), 2, "{:?}", found.keys());

    // An edit whose tag makes the workspace's system document, with a task
    // change: the stamps and the body, the new system document, and its
    // registration in the manifest.
    let before = fixture.versions();
    let revision = fixture.revision(page);
    fixture
        .call(
            &hosted,
            "edit_document",
            edit(
                page,
                &revision,
                json!([
                    add_tag(lines[0], "Ship it", "first"),
                    due(lines[0], "Ship it")
                ]),
            ),
        )
        .unwrap();
    let found = attributed(&fixture, &before, &connection);
    let system = fixture
        .store(move |store| store.documents(workspace))
        .into_iter()
        .find(|entry| entry.kind == koru_core::explore::KIND_SYSTEM)
        .expect("a system document")
        .id;
    for unit in [page, system, manifest] {
        assert!(found.contains_key(&unit), "{unit} has no attributed change");
    }

    // An edit creating a tag in the system document that exists.
    let before = fixture.versions();
    let revision = fixture.revision(page);
    fixture
        .call(
            &hosted,
            "edit_document",
            edit(
                page,
                &revision,
                json!([add_tag(lines[0], "Ship it", "second")]),
            ),
        )
        .unwrap();
    let found = attributed(&fixture, &before, &connection);
    assert!(found.contains_key(&system) && found.contains_key(&page));

    // Extraction into a new destination: its entry, its content, the source.
    let before = fixture.versions();
    let revision = fixture.revision(page);
    let moved = fixture
        .call(
            &hosted,
            "extract_to_document",
            extract(page, &revision, lines[1], "Move me", None),
        )
        .unwrap();
    let destination = DocumentId::parse(moved["document_id"].as_str().unwrap()).unwrap();
    let found = attributed(&fixture, &before, &connection);
    for unit in [manifest, destination, page] {
        assert!(found.contains_key(&unit), "{unit} has no attributed change");
    }
    assert!(found.values().flatten().all(|actor| actor.create.is_none()));

    // Extraction into an unstamped existing destination.
    let (source, lines) = fixture.unstamped("Inbox", &["File me"]);
    let before = fixture.versions();
    let revision = fixture.revision(source);
    fixture
        .call(
            &hosted,
            "extract_to_document",
            extract(source, &revision, lines[0], "File me", Some(target)),
        )
        .unwrap();
    let found = attributed(&fixture, &before, &connection);
    assert!(found.contains_key(&source) && found.contains_key(&target));
}

/// **No Koru path writes a commit message of its own (§16).** The same
/// mutations and the Store's own commits, on a local host: not one change in
/// any unit carries a message. With the sweep above, it is what makes
/// `actor/1` mean "an app connection made this".
#[test]
fn no_local_path_writes_a_commit_message() {
    let fixture = Fixture::new();
    let local = fixture.local();
    let (page, lines) = fixture.unstamped("Plan", &["Ship it", "Move me"]);
    let (target, _) = fixture.unstamped("Archive", &["Old"]);
    fixture
        .call(&local, "create_document", json!({ "title": "Minutes" }))
        .unwrap();
    for tag in ["first", "second"] {
        let revision = fixture.revision(page);
        fixture
            .call(
                &local,
                "edit_document",
                edit(page, &revision, json!([add_tag(lines[0], "Ship it", tag)])),
            )
            .unwrap();
    }
    let revision = fixture.revision(page);
    fixture
        .call(
            &local,
            "edit_document",
            edit(page, &revision, json!([due(lines[0], "Ship it")])),
        )
        .unwrap();
    fixture
        .call(
            &local,
            "update_task",
            json!({
                "document_id": page.to_storage(),
                "node_id": lines[0].to_storage(),
                "expected": "Ship it",
                "done": true,
            }),
        )
        .unwrap();
    let revision = fixture.revision(page);
    fixture
        .call(
            &local,
            "extract_to_document",
            extract(page, &revision, lines[1], "Move me", None),
        )
        .unwrap();
    let (source, lines) = fixture.unstamped("Inbox", &["File me"]);
    let revision = fixture.revision(source);
    fixture
        .call(
            &local,
            "extract_to_document",
            extract(source, &revision, lines[0], "File me", Some(target)),
        )
        .unwrap();

    let labelled: Vec<_> = fixture
        .changes_since(&HashMap::new())
        .into_iter()
        .filter(|(_, message)| message.is_some())
        .collect();
    assert!(labelled.is_empty(), "{labelled:?}");
}
