// SPDX-License-Identifier: MPL-2.0

//! TECHNICAL §14, *In-memory composition (F10)*: start with empty memory and
//! a read-only filesystem; reconcile; search; list a task; edit; query again.
//! The index becomes ready, and no content or sidecar file is written.
//!
//! Here at the Store and port level: the shared tools run over an
//! `EmbeddedStore` adapter onto the composition, and the Workspace arrives
//! through the extracted driver from an in-memory Cloud. The hosted mutation
//! path is Cloud MCP Plan 2.3's.

use std::sync::Arc;
use std::time::Duration;

use koru_mcp_server::{
    EmbeddedSeedDocument, EmbeddedSeededDocument, EmbeddedStore, LocalBackend, RequestContext,
    ToolError, WorkspaceToolExecutor,
};
use koru_replication::cloud::transfer::{Decision, MemoryBudget, Start};
use koru_store_core::ids::{DocumentId, HistoryEntryId};
use koru_store_core::model::{Blob, BlobHash, DocumentEntry, Workspace};
use koru_store_core::ports::{HistoryReason, UpdateOrigin};
use koru_store_core::{Locale, ProfileAccountBinding};
use koru_store_wire::search::{SearchRequest, SearchResponse};
use koru_store_wire::tasks::{TaskQuery, TaskQueryResponse};
use koru_store_wire::{CaptureHistory, HistoryEntry, HistoryPage, HistoryQuery, Status};
use serde_json::{Value, json};

use super::*;
use crate::daemon::cloud_sync::{
    self, Plan, Stage,
    driver::{Driver, Parts},
};
use crate::daemon::fake_cloud::{FakeCloud, Sessions, document_unit, workspace_scope};

/// The shared tools' port onto one composition, each call on the caller's
/// thread — what a host's own adapter does, on its actor.
struct Port(Arc<MemoryStore>);

impl Port {
    fn store<T>(
        &self,
        operation: impl FnOnce(&mut crate::WorkerStore) -> Result<T, Error>,
    ) -> Result<T, ToolError> {
        self.0
            .with_store(operation)
            .map_err(|error| ToolError {
                code: "store_unreachable".into(),
                message: error.to_string(),
                details: None,
            })?
            .map_err(ToolError::from)
    }
}

impl EmbeddedStore for Port {
    fn workspaces(&self) -> Result<Vec<Workspace>, ToolError> {
        self.store(|store| store.workspaces())
    }
    fn documents(&self, workspace: WorkspaceId) -> Result<Vec<DocumentEntry>, ToolError> {
        self.store(|store| store.documents(workspace))
    }
    fn search(&self, request: SearchRequest) -> Result<SearchResponse, ToolError> {
        self.0.search(&request).map_err(ToolError::from)
    }
    fn query_tasks(&self, query: TaskQuery) -> Result<TaskQueryResponse, ToolError> {
        self.0.query_tasks(query).map_err(ToolError::from)
    }
    fn snapshot(&self, workspace: WorkspaceId, document: DocumentId) -> Result<Vec<u8>, ToolError> {
        self.store(|store| store.snapshot(workspace, document))
    }
    fn create_document(
        &self,
        workspace: WorkspaceId,
        title: String,
        kind: String,
        attribution: Option<&str>,
    ) -> Result<DocumentId, ToolError> {
        self.store(|store| {
            store
                .create_document_attributed(
                    workspace,
                    &title,
                    &kind,
                    None,
                    None,
                    attribution,
                    Timestamp::now(),
                )
                .map(|(document, _)| document)
        })
    }
    fn manifest_id(&self, workspace: WorkspaceId) -> Result<DocumentId, ToolError> {
        self.store(|store| store.manifest_id(workspace))
    }
    fn ensure_file_note(
        &self,
        workspace: WorkspaceId,
        file: DocumentId,
    ) -> Result<(DocumentId, bool), ToolError> {
        self.store(|store| store.ensure_file_note(workspace, file, Timestamp::now()))
            .map(|ensured| (ensured.id, ensured.created))
    }
    fn seed_document(
        &self,
        request: EmbeddedSeedDocument,
    ) -> Result<EmbeddedSeededDocument, ToolError> {
        self.store(|store| {
            store.create_seeded_document_attributed(
                request.workspace,
                &request.title,
                &request.kind,
                request.parent,
                request.index,
                "memory-port",
                &request.idempotency_key,
                &request.request_hash,
                None,
                request.initial_update,
                request.attribution.as_deref(),
                Timestamp::now(),
            )
        })
        .map(|seeded| EmbeddedSeededDocument {
            id: seeded.document,
            initial_update: seeded.initial_update,
            replayed: seeded.replayed,
        })
    }
    fn persist_update(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
        update: Vec<u8>,
    ) -> Result<(), ToolError> {
        self.store(|store| {
            let at = Timestamp::now();
            store.apply_update(workspace, document, update, at, UpdateOrigin::Store)?;
            store.flush(workspace, document, at).map(|_| ())
        })
    }
    fn capture_history(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
        request: CaptureHistory,
    ) -> Result<HistoryEntry, ToolError> {
        let reason = HistoryReason::parse(&request.reason).expect("a known reason");
        self.store(|store| {
            store
                .capture_history(
                    workspace,
                    document,
                    request.frontiers.as_deref(),
                    reason,
                    Timestamp::now(),
                    request.label.as_deref(),
                    request.message.as_deref(),
                    request.boundary.as_deref(),
                )
                .map(|entry| HistoryEntry {
                    id: entry.id,
                    document: entry.document,
                    version: entry.version,
                    reason: entry.reason.as_str().to_owned(),
                    recorded_at: entry.recorded_at.millis(),
                    label: entry.label,
                    message: entry.message,
                    boundary: entry.boundary,
                    pinned: entry.pinned,
                })
        })
    }
    fn history(
        &self,
        _workspace: WorkspaceId,
        _document: DocumentId,
        _query: HistoryQuery,
    ) -> Result<HistoryPage, ToolError> {
        unreachable!("no step here lists versions")
    }
    fn history_snapshot(
        &self,
        workspace: WorkspaceId,
        entry: HistoryEntryId,
    ) -> Result<Vec<u8>, ToolError> {
        self.store(|store| {
            store
                .history_version(workspace, entry)
                .map(|version| version.snapshot)
        })
    }
    fn blobs(&self, workspace: WorkspaceId) -> Result<Vec<Blob>, ToolError> {
        self.store(|store| store.blobs(workspace))
    }
    fn get_blob(&self, workspace: WorkspaceId, hash: BlobHash) -> Result<Vec<u8>, ToolError> {
        self.store(|store| store.get_blob(workspace, &hash))
    }
    fn status(&self) -> Result<Status, ToolError> {
        Ok(Status {
            api_version: koru_store_core::version::API_VERSION,
            capabilities: Vec::new(),
            channel: "memory".into(),
            started_at: 0,
        })
    }
}

fn tools(store: &Arc<MemoryStore>) -> WorkspaceToolExecutor {
    WorkspaceToolExecutor::new(Arc::new(LocalBackend::with_embedded(Arc::new(Port(
        Arc::clone(store),
    )))))
}

/// A tool call, off the runtime's threads as a host runs one.
async fn call(tools: &WorkspaceToolExecutor, tool: &'static str, arguments: Value) -> Value {
    let tools = tools.clone();
    tokio::task::spawn_blocking(move || {
        let started = std::time::Instant::now();
        tools
            .invoke(&RequestContext::local(), tool, arguments)
            .unwrap_or_else(|error| {
                panic!(
                    "{tool}: {} {} after {:?}",
                    error.code,
                    error.message,
                    started.elapsed()
                )
            })
    })
    .await
    .unwrap()
}

/// Ask until the answer is the one wanted, for up to ten seconds: the read
/// model and the index trail a commit by design.
async fn until<F, A>(what: &str, mut ask: F) -> Value
where
    F: FnMut() -> A,
    A: Future<Output = Option<Value>>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(answer) = ask().await {
            return answer;
        }
        assert!(tokio::time::Instant::now() < deadline, "{what} never came");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

type Permission = fn(&Start) -> Decision;

fn accept(_: &Start) -> Decision {
    Decision::Accept
}

fn parts(
    jobs: StoreWorkerClient,
    workspace: WorkspaceId,
) -> Parts<StoreWorkerClient, Permission, fn(cloud_sync::Progress)> {
    Parts {
        plan: Plan::Workspace {
            workspace_id: workspace,
        },
        stage: Stage::Full,
        jobs,
        authorize: Arc::new(|_| Ok(())),
        downloads: Arc::new(cloud_sync::NoDownloads),
        deferrals: Arc::default(),
        permission: accept,
        progress: |_| {},
        uploads: None,
    }
}

async fn passes(driver: &Driver, at_least: u64) {
    let mut status = driver.status();
    tokio::time::timeout(Duration::from_secs(10), async {
        while status.borrow_and_update().passes < at_least {
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("the pass finished");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_in_memory_store_reconciles_searches_lists_edits_and_queries_again() {
    let cloud = FakeCloud::default();
    let budget = MemoryBudget::new(256 * 1024 * 1024);

    // Another device's Workspace — in memory too — uploaded to Cloud.
    let device = Arc::new(MemoryStore::open("device").unwrap());
    let (profile, workspace, manifest) = device
        .with_store(|store| {
            let at = Timestamp::now();
            let profile = store.create_profile("Person", None, None, Locale::default(), at)?;
            let workspace = store.create_workspace(profile.id, "Garden", at)?;
            let manifest = store.manifest_id(workspace.id)?;
            Ok::<_, Error>((profile.id, workspace.id, manifest))
        })
        .unwrap()
        .unwrap();
    let on_device = tools(&device);
    let created = call(
        &on_device,
        "create_document",
        json!({
            "workspace_id": workspace,
            "title": "Planting",
            "idempotency_key": uuid::Uuid::new_v4(),
        }),
    )
    .await;
    let document = DocumentId::parse(created["document_id"].as_str().unwrap()).unwrap();
    // A task is a node with task state; a due date alone makes one.
    let edited = call(
        &on_device,
        "edit_document",
        json!({
            "workspace_id": workspace,
            "document_id": document,
            "base_revision": created["revision"],
            "operations": [
                {"op": "insert", "node_id": "kauri", "parent_id": created["root_node_id"],
                 "kind": "task", "text": "Plant the kauri"},
                {"op": "set_attribute", "node_id": "kauri", "expected": "Plant the kauri",
                 "key": "koru.due", "value": {"type": "date", "value": "2026-12-01"}},
                {"op": "insert", "node_id": "notes", "parent_id": created["root_node_id"],
                 "after_node_id": "kauri", "kind": "paragraph", "text": "Notes on the grove"}
            ]
        }),
    )
    .await;
    let task = edited["created"]["kauri"].clone();
    assert!(task.is_string(), "{edited}");
    let scope = workspace_scope(workspace, profile, manifest);
    let upload = Driver::spawn(
        Sessions::new(cloud.clone(), scope, budget.clone(), 1),
        parts(device.client(), workspace),
    );
    passes(&upload, 1).await;
    upload.stop().await;
    assert!(
        cloud.unit(document_unit(workspace, document)).is_some(),
        "the device's document is in Cloud"
    );

    // The replica: an empty composition with no path anywhere in it.
    let replica = Arc::new(MemoryStore::open("replica").unwrap());
    assert!(
        replica
            .with_store(|store| store.workspaces().map(|all| all.is_empty()))
            .unwrap()
            .unwrap(),
        "empty memory"
    );
    replica
        .with_store(|store| {
            store.provision_account_profile(
                profile,
                ProfileAccountBinding::from_identity_uuid(uuid::Uuid::new_v4()).unwrap(),
                "Person",
                Locale::default(),
                Timestamp::now(),
            )
        })
        .unwrap()
        .unwrap();
    crate::cloud_driver::ensure_workspace(&replica.client(), profile, workspace, manifest)
        .await
        .unwrap();

    // Reconcile.
    let driver = Driver::spawn(
        Sessions::new(cloud.clone(), scope, budget.clone(), 1),
        parts(replica.client(), workspace),
    );
    passes(&driver, 1).await;
    assert!(
        cloud
            .log
            .lock()
            .unwrap()
            .downloaded
            .contains(&document_unit(workspace, document)),
        "the document came down from Cloud"
    );

    // The index becomes ready.
    let ticket = replica.request_sweep(workspace).expect("a read model");
    until("the sweep", || {
        let swept = replica.swept(workspace, ticket);
        async move { swept.then_some(Value::Null) }
    })
    .await;

    // Search.
    let on_replica = tools(&replica);
    let hits = until("a search hit", || async {
        let found = call(&on_replica, "search_documents", json!({ "query": "grove" })).await;
        (!found["hits"].as_array()?.is_empty()).then_some(found)
    })
    .await;
    assert!(
        hits["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["document"] == json!(document.to_storage())),
        "{hits}"
    );

    // List a task.
    let open = until("the open task", || async {
        let page = call(&on_replica, "list_tasks", json!({})).await;
        (!page["tasks"].as_array()?.is_empty()).then_some(page)
    })
    .await;
    assert_eq!(open["tasks"][0]["title"], json!("Plant the kauri"));
    assert_eq!(open["tasks"][0]["node_id"], json!(task));

    // Edit: complete it, through the port.
    let read = call(
        &on_replica,
        "read_document",
        json!({ "workspace_id": workspace, "document_id": document }),
    )
    .await;
    call(
        &on_replica,
        "edit_document",
        json!({
            "workspace_id": workspace,
            "document_id": document,
            "base_revision": read["revision"],
            "operations": [{"op": "set_done", "node_id": task, "expected": "Plant the kauri", "done": true}]
        }),
    )
    .await;

    // Query again: the read model has followed the edit.
    until("the completed task", || async {
        let open = call(&on_replica, "list_tasks", json!({})).await;
        let done = call(
            &on_replica,
            "list_tasks",
            json!({ "lifecycle": "completed" }),
        )
        .await;
        (open["tasks"].as_array()?.is_empty() && done["tasks"].as_array()?.len() == 1)
            .then_some(done)
    })
    .await;

    // And the live driver carried the edit back to Cloud.
    let local = replica
        .with_store(move |store| store.version(workspace, document))
        .unwrap()
        .unwrap();
    until("the edit in Cloud", || {
        let carried = cloud
            .unit(document_unit(workspace, document))
            .is_some_and(|unit| unit.oplog_vv().includes_vv(&local));
        async move { carried.then_some(Value::Null) }
    })
    .await;

    driver.stop().await;
    // No file bytes exist to write: the backend keeps none.
    let refused = replica
        .with_store(move |store| {
            store.put_blob(
                workspace,
                b"bytes",
                "text/plain",
                None,
                Timestamp::now(),
                None,
            )
        })
        .unwrap();
    assert!(refused.is_err(), "a Store in memory keeps no file bytes");
    drop(on_replica);
    drop(on_device);
    Arc::try_unwrap(replica).ok().unwrap().close();
    Arc::try_unwrap(device).ok().unwrap().close();
}

/// Closing stops every thread it started and refuses later work, rather than
/// leaving a replica's projector and queue running after it is gone.
#[test]
fn closing_stops_its_threads_and_refuses_later_work() {
    let store = MemoryStore::open("closing").unwrap();
    let client = store.client();
    client.with_store(|_| ()).unwrap();
    let started = std::time::Instant::now();
    store.close();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "joined within a few idle ticks"
    );
    assert!(matches!(
        client.with_store(|_| ()),
        Err(WorkerError::Stopped)
    ));
}
