// SPDX-License-Identifier: MPL-2.0

//! Koru AI's direct, bounded bridge to the shared workspace-tool semantics.

use std::sync::Arc;

use koru_ai_core::{
    AiError, AiErrorClass, AiToolCall, AiToolDefinition, AiToolExecutionContext, AiToolExecutor,
    AiToolResult, AiToolSafety, Cancellation, Timestamp,
};
use koru_mcp_server::{
    EmbeddedSeedDocument, EmbeddedSeededDocument, EmbeddedStore, LocalBackend, RequestContext,
    ToolError, WorkspaceToolExecutor, workspace_tool_catalog,
};
use koru_store_core::ids::{DocumentId, HistoryEntryId, WorkspaceId};
use koru_store_core::model::{Blob, BlobHash, DocumentEntry, Scope, Workspace};
use koru_store_core::ports::HistoryReason;
use koru_store_server::{StoreWorkerClient, WorkerError};
use koru_store_wire::search::{SearchRequest, SearchResponse};
use koru_store_wire::tasks::{TaskQuery, TaskQueryResponse};
use koru_store_wire::{CaptureHistory, HistoryEntry, HistoryPage, HistoryQuery, Status};

/// Executes provider-requested tools through the same catalog, authorization,
/// and CRDT-aware implementation exposed by Koru MCP, without HTTP or an MCP
/// serialization hop inside the desktop process.
#[derive(Clone)]
pub struct StoreToolAdapter {
    executor: WorkspaceToolExecutor,
    definitions: Vec<AiToolDefinition>,
    /// The same Store, for which profile owns a granted workspace.
    store: Arc<WorkerEmbeddedStore>,
}

impl StoreToolAdapter {
    pub fn new(client: StoreWorkerClient) -> Self {
        let store = Arc::new(WorkerEmbeddedStore { client });
        let backend = Arc::new(LocalBackend::with_embedded(store.clone()));
        Self {
            store,
            executor: WorkspaceToolExecutor::new(backend),
            definitions: workspace_tool_catalog()
                .into_iter()
                .map(|tool| AiToolDefinition {
                    name: tool.name,
                    title: tool.title,
                    description: tool.description,
                    input_schema: tool.input_schema,
                    output_schema: tool.output_schema,
                    safety: AiToolSafety {
                        read_only: tool.safety.read_only,
                        destructive: tool.safety.destructive,
                        idempotent: tool.safety.idempotent,
                        open_world: tool.safety.open_world,
                    },
                })
                .collect(),
        }
    }
}

impl StoreToolAdapter {
    /// **The chat's grants in its own profile, and no others** (AI schema
    /// 13). A grant to another profile's workspace stays in the chat's
    /// policy, so it is there again if the person grants it in that
    /// profile's window — but no tool reaches it, lists it or counts it: the
    /// selection the tools are authorized by is made of these alone. A chat
    /// not yet placed reaches none.
    fn readable_workspaces(
        &self,
        context: &AiToolExecutionContext,
    ) -> Result<Vec<WorkspaceId>, AiError> {
        let Some(profile) = context.profile_id else {
            return Ok(Vec::new());
        };
        if context.workspace_ids.is_empty() {
            return Ok(Vec::new());
        }
        let granted = context.workspace_ids.clone();
        let owned = self
            .store
            .read(move |store| {
                Ok(store
                    .workspaces_including_deleted()?
                    .into_iter()
                    .filter(|workspace| workspace.owner == profile)
                    .map(|workspace| workspace.id)
                    .collect::<std::collections::HashSet<_>>())
            })
            .map_err(tool_failure)?;
        Ok(granted
            .into_iter()
            .filter(|workspace| owned.contains(workspace))
            .collect())
    }
}

impl AiToolExecutor for StoreToolAdapter {
    fn definitions(&self) -> Vec<AiToolDefinition> {
        self.definitions.clone()
    }

    fn invoke(
        &self,
        context: &AiToolExecutionContext,
        call: &AiToolCall,
        cancellation: &Cancellation,
    ) -> Result<AiToolResult, AiError> {
        if cancellation.is_cancelled() {
            return Err(AiError::new(
                AiErrorClass::Cancelled,
                "The AI request was cancelled.",
                false,
            ));
        }
        let request = RequestContext::selected(
            "local",
            format!("koru-ai:{}", context.chat_id),
            [Scope::ReadDocuments, Scope::EditDocuments],
            self.readable_workspaces(context)?,
        );
        tracing::info!(
            chat = %context.chat_id,
            attempt = %context.attempt_id,
            invocation = %context.invocation_id,
            tool = %call.name,
            "AI workspace tool started"
        );
        let result = self
            .executor
            .invoke(&request, &call.name, call.arguments.clone())
            .map_err(tool_failure)?;
        tracing::info!(
            chat = %context.chat_id,
            attempt = %context.attempt_id,
            invocation = %context.invocation_id,
            tool = %call.name,
            "AI workspace tool completed"
        );
        Ok(AiToolResult {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            content: result,
            is_error: false,
        })
    }
}

struct WorkerEmbeddedStore {
    client: StoreWorkerClient,
}

impl EmbeddedStore for WorkerEmbeddedStore {
    fn workspaces(&self) -> Result<Vec<Workspace>, ToolError> {
        self.read(|store| store.workspaces())
    }

    fn documents(&self, workspace: WorkspaceId) -> Result<Vec<DocumentEntry>, ToolError> {
        self.read(move |store| store.documents(workspace))
    }

    fn search(&self, request: SearchRequest) -> Result<SearchResponse, ToolError> {
        flatten(self.client.search(request))
    }

    /// The Store worker's task port, through its typed queue: the page
    /// `POST /v1/tasks/query` answers, so Koru AI lists what the desktop does.
    fn query_tasks(&self, query: TaskQuery) -> Result<TaskQueryResponse, ToolError> {
        flatten(self.client.query_tasks(query))
    }

    fn snapshot(&self, workspace: WorkspaceId, document: DocumentId) -> Result<Vec<u8>, ToolError> {
        self.read(move |store| store.snapshot(workspace, document))
    }

    fn create_document(
        &self,
        workspace: WorkspaceId,
        title: String,
        kind: String,
    ) -> Result<DocumentId, ToolError> {
        flatten(
            self.client
                .create_document(workspace, title, kind, Timestamp::now()),
        )
    }

    fn ensure_file_note(
        &self,
        workspace: WorkspaceId,
        file: DocumentId,
    ) -> Result<(DocumentId, bool), ToolError> {
        self.read(move |store| store.ensure_file_note(workspace, file, Timestamp::now()))
            // `created` is a fact the store reports — the same fact the HTTP
            // route passes through.
            .map(|ensured| (ensured.id, ensured.created))
    }

    fn seed_document(
        &self,
        request: EmbeddedSeedDocument,
    ) -> Result<EmbeddedSeededDocument, ToolError> {
        flatten(self.client.seed_document_full(
            request.workspace,
            request.title,
            request.kind,
            request.parent,
            request.index,
            "koru-ai-tools".into(),
            request.idempotency_key,
            request.request_hash,
            request.initial_update,
            Timestamp::now(),
        ))
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
        flatten(
            self.client
                .apply_document_update(workspace, document, update, Timestamp::now()),
        )
    }

    fn capture_history(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
        request: CaptureHistory,
    ) -> Result<HistoryEntry, ToolError> {
        let reason = HistoryReason::parse(&request.reason)
            .ok_or_else(|| invalid("unknown history reason"))?;
        self.read(move |store| {
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
                .map(wire_history_entry)
        })
    }

    fn history(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
        query: HistoryQuery,
    ) -> Result<HistoryPage, ToolError> {
        let before = match (query.before_at, query.before_id) {
            (Some(at), Some(id)) => Some((Timestamp::from_millis(at), id)),
            (None, None) => None,
            _ => return Err(invalid("history cursor is incomplete")),
        };
        let limit = query.limit.unwrap_or(50).clamp(1, 200);
        self.read(move |store| {
            let entries = store.history_entries(workspace, document, before, limit)?;
            // The same answer the HTTP listing gives, from the same store
            // method — a second adapter over one store must not disagree with
            // the first about whether the document has moved on.
            let stale = store.history_staleness(workspace, document)?;
            let next = (entries.len() == limit).then(|| {
                let last = entries.last().expect("a full history page is non-empty");
                (last.recorded_at.millis(), last.id)
            });
            Ok(HistoryPage {
                entries: entries.into_iter().map(wire_history_entry).collect(),
                next_before_at: next.map(|value| value.0),
                next_before_id: next.map(|value| value.1),
                stale,
            })
        })
    }

    fn history_snapshot(
        &self,
        workspace: WorkspaceId,
        entry: HistoryEntryId,
    ) -> Result<Vec<u8>, ToolError> {
        self.read(move |store| {
            store
                .history_version(workspace, entry)
                .map(|version| version.snapshot)
        })
    }

    fn blobs(&self, workspace: WorkspaceId) -> Result<Vec<Blob>, ToolError> {
        self.read(move |store| store.blobs(workspace))
    }

    fn get_blob(&self, workspace: WorkspaceId, hash: BlobHash) -> Result<Vec<u8>, ToolError> {
        self.read(move |store| store.get_blob(workspace, &hash))
    }

    fn status(&self) -> Result<Status, ToolError> {
        self.client
            .store_status()
            .map(|status| Status {
                api_version: status.api_version,
                capabilities: status.capabilities,
                channel: status.channel,
                started_at: status.started_at,
            })
            .map_err(worker_error)
    }
}

impl WorkerEmbeddedStore {
    fn read<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut koru_store_server::WorkerStore) -> Result<T, koru_store_core::Error>
        + Send
        + 'static,
    ) -> Result<T, ToolError> {
        flatten(self.client.with_store(operation))
    }
}

fn flatten<T>(
    result: Result<Result<T, koru_store_core::Error>, WorkerError>,
) -> Result<T, ToolError> {
    result.map_err(worker_error)?.map_err(ToolError::from)
}

fn worker_error(error: WorkerError) -> ToolError {
    ToolError {
        code: "store_unreachable".into(),
        message: error.to_string(),
        details: None,
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError {
        code: "invalid_request".into(),
        message: message.into(),
        details: None,
    }
}

fn wire_history_entry(entry: koru_store_core::ports::HistoryEntry) -> HistoryEntry {
    HistoryEntry {
        id: entry.id,
        document: entry.document,
        version: entry.version,
        reason: entry.reason.as_str().to_owned(),
        recorded_at: entry.recorded_at.millis(),
        label: entry.label,
        message: entry.message,
        boundary: entry.boundary,
        pinned: entry.pinned,
    }
}

fn tool_failure(error: ToolError) -> AiError {
    let class = match error.code.as_str() {
        "permission_denied" | "forbidden" => AiErrorClass::Authentication,
        "invalid_request" | "invalid_id" => AiErrorClass::Provider,
        _ => AiErrorClass::Storage,
    };
    tracing::warn!(code = %error.code, "AI workspace tool failed");
    AiError::new(class, error.message, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_ai_core::{ChatId, InferenceAttemptId, ProfileId, ToolInvocationId};
    use koru_store_core::Locale;
    use serde_json::json;

    fn execution_context(
        workspaces: Vec<WorkspaceId>,
        profile: Option<ProfileId>,
    ) -> AiToolExecutionContext {
        AiToolExecutionContext {
            chat_id: ChatId::new(),
            attempt_id: InferenceAttemptId::new(),
            invocation_id: ToolInvocationId::new(),
            workspace_ids: workspaces,
            profile_id: profile,
        }
    }

    fn call(name: &str, arguments: serde_json::Value) -> AiToolCall {
        AiToolCall {
            call_id: format!("call-{name}"),
            name: name.into(),
            arguments,
            thought_signatures: Vec::new(),
        }
    }

    #[test]
    fn embedded_tools_deny_empty_grants_and_persist_crdt_mutations() {
        let root = tempfile::tempdir().unwrap();
        let worker = koru_store_server::StoreWorker::new(
            koru_store_server::WorkerConfig::embedded(root.path().to_owned(), "internal-debug"),
        )
        .spawn()
        .unwrap();
        let client = worker.client();
        let (profile, workspace) = client
            .with_store(|store| {
                let at = Timestamp::from_millis(1);
                let profile =
                    store.create_profile("Test", None, None, Locale::parse("en").unwrap(), at)?;
                let workspace = store.create_workspace(profile.id, "Workspace", at)?;
                Ok::<_, koru_store_core::Error>((profile.id, workspace.id))
            })
            .unwrap()
            .unwrap();
        let adapter = StoreToolAdapter::new(client.clone());
        let cancellation = Cancellation::default();

        let denied = adapter.invoke(
            &execution_context(Vec::new(), Some(profile)),
            &call(
                "create_document",
                json!({ "workspace_id": workspace, "title": "AI draft" }),
            ),
            &cancellation,
        );
        assert_eq!(denied.unwrap_err().class, AiErrorClass::Authentication);

        let context = execution_context(vec![workspace], Some(profile));
        let created = adapter
            .invoke(
                &context,
                &call(
                    "create_document",
                    json!({
                        "workspace_id": workspace,
                        "title": "AI draft",
                        "idempotency_key": uuid::Uuid::new_v4(),
                    }),
                ),
                &cancellation,
            )
            .unwrap();
        let document = DocumentId::parse(
            created.content["document_id"]
                .as_str()
                .expect("created document id"),
        )
        .unwrap();
        let revision = created.content["revision"]
            .as_str()
            .expect("created revision")
            .to_owned();
        let root_node = created.content["root_node_id"]
            .as_str()
            .expect("created root")
            .to_owned();
        let stale_revision = revision.clone();

        adapter
            .invoke(
                &context,
                &call(
                    "edit_document",
                    json!({
                        "workspace_id": workspace,
                        "document_id": document,
                        "base_revision": revision,
                        "operations": [{
                            "op": "replace_text",
                            "node_id": root_node.clone(),
                            "expected": "AI draft",
                            "text": "Durable AI draft"
                        }]
                    }),
                ),
                &cancellation,
            )
            .unwrap();

        let conflict = adapter.invoke(
            &context,
            &call(
                "edit_document",
                json!({
                    "workspace_id": workspace,
                    "document_id": document,
                    "base_revision": stale_revision,
                    "operations": [{
                        "op": "replace_text",
                        "node_id": root_node,
                        "expected": "AI draft",
                        "text": "Overwrite"
                    }]
                }),
            ),
            &cancellation,
        );
        assert_eq!(conflict.unwrap_err().class, AiErrorClass::Storage);

        let stored = client
            .with_store(move |store| store.snapshot(workspace, document))
            .unwrap()
            .unwrap();
        let document = koru_core::WorkspaceDoc::from_snapshot(&stored).unwrap();
        let title = document
            .walk_all()
            .unwrap()
            .into_iter()
            .find_map(|(node, _)| {
                document
                    .read(node)
                    .ok()
                    .filter(|value| value.kind == koru_core::Kind::Document)
                    .and_then(|value| value.content.as_text().map(str::to_owned))
            });
        assert_eq!(title.as_deref(), Some("Durable AI draft"));

        drop(adapter);
        drop(client);
        assert_eq!(worker.join(), koru_store_server::WorkerExit::Clean);
    }

    /// **Koru AI's `list_tasks` is answered**, where the embedded host used
    /// to refuse it `tasks_unavailable`: the port asks the Store's own task
    /// port through the worker's queue. Over a Store kept in memory, so the
    /// test touches no disk.
    #[test]
    fn list_tasks_is_answered_from_the_task_port() {
        let memory = koru_store_server::memory::MemoryStore::open("internal-debug").unwrap();
        let client = memory.client();
        let (profile, workspace) = client
            .with_store(|store| {
                let at = Timestamp::from_millis(1);
                let profile =
                    store.create_profile("Test", None, None, Locale::parse("en").unwrap(), at)?;
                let workspace = store.create_workspace(profile.id, "Workspace", at)?;
                Ok::<_, koru_store_core::Error>((profile.id, workspace.id))
            })
            .unwrap()
            .unwrap();
        let adapter = StoreToolAdapter::new(client);
        let page = adapter
            .invoke(
                &execution_context(vec![workspace], Some(profile)),
                &call("list_tasks", json!({})),
                &Cancellation::default(),
            )
            .expect("the tasks query is answered, not refused");
        assert!(page.content["tasks"].is_array(), "{}", page.content);
        assert!(page.content["updating_workspaces"].is_array());
        drop(adapter);
        memory.close();
    }

    #[test]
    fn cancelled_invocation_never_reaches_store() {
        let root = tempfile::tempdir().unwrap();
        let worker = koru_store_server::StoreWorker::new(
            koru_store_server::WorkerConfig::embedded(root.path().to_owned(), "internal-debug"),
        )
        .spawn()
        .unwrap();
        let adapter = StoreToolAdapter::new(worker.client());
        let cancellation = Cancellation::default();
        cancellation.cancel();
        let result = adapter.invoke(
            &execution_context(Vec::new(), None),
            &call("list_workspaces", json!({})),
            &cancellation,
        );
        assert_eq!(result.unwrap_err().class, AiErrorClass::Cancelled);
        drop(adapter);
        assert_eq!(worker.join(), koru_store_server::WorkerExit::Clean);
    }

    /// **A chat's tools reach only its own profile's workspaces** (AI
    /// schema 13). The chat is granted a workspace in each of two
    /// profiles; its tools list, read and write only its own profile's, and
    /// the other grant is neither listed nor counted. A chat not yet placed
    /// reaches none.
    #[test]
    fn a_grant_to_another_profiles_workspace_is_never_reached() {
        let root = tempfile::tempdir().unwrap();
        let worker = koru_store_server::StoreWorker::new(
            koru_store_server::WorkerConfig::embedded(root.path().to_owned(), "internal-debug"),
        )
        .spawn()
        .unwrap();
        let client = worker.client();
        let (private, home, office) = client
            .with_store(|store| {
                let at = Timestamp::from_millis(1);
                let private = store.create_profile(
                    "Private",
                    None,
                    None,
                    Locale::parse("en").unwrap(),
                    at,
                )?;
                let work =
                    store.create_profile("Work", None, None, Locale::parse("en").unwrap(), at)?;
                let home = store.create_workspace(private.id, "Home", at)?;
                let office = store.create_workspace(work.id, "Office", at)?;
                Ok::<_, koru_store_core::Error>((private.id, home.id, office.id))
            })
            .unwrap()
            .unwrap();
        let adapter = StoreToolAdapter::new(client.clone());
        let cancellation = Cancellation::default();
        let both = execution_context(vec![home, office], Some(private));

        let listed = adapter
            .invoke(&both, &call("list_workspaces", json!({})), &cancellation)
            .unwrap()
            .content;
        let listed = listed["workspaces"].as_array().unwrap();
        assert_eq!(listed.len(), 1, "only the chat's own profile's: {listed:?}");
        assert_eq!(listed[0]["id"].as_str(), Some(home.to_storage().as_str()));

        let foreign = adapter.invoke(
            &both,
            &call(
                "create_document",
                json!({
                    "workspace_id": office,
                    "title": "Not here",
                    "idempotency_key": uuid::Uuid::new_v4(),
                }),
            ),
            &cancellation,
        );
        assert_eq!(foreign.unwrap_err().class, AiErrorClass::Authentication);
        adapter
            .invoke(
                &both,
                &call(
                    "create_document",
                    json!({
                        "workspace_id": home,
                        "title": "Here",
                        "idempotency_key": uuid::Uuid::new_v4(),
                    }),
                ),
                &cancellation,
            )
            .unwrap();

        let unplaced = execution_context(vec![home, office], None);
        let nothing = adapter
            .invoke(
                &unplaced,
                &call("list_workspaces", json!({})),
                &cancellation,
            )
            .unwrap()
            .content;
        assert_eq!(nothing["workspaces"].as_array().map(Vec::len), Some(0));

        drop(adapter);
        drop(client);
        assert_eq!(worker.join(), koru_store_server::WorkerExit::Clean);
    }
}
