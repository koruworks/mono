// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_store_core::{
    model::Timestamp,
    ports::{DocumentVersion, UpdateOrigin},
    service::Store,
};
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};
use std::sync::atomic::{AtomicBool, Ordering};

mod memory;

type MemoryStore = Store<SqliteCatalog, SqliteBackend>;
struct Embedded {
    store: Mutex<MemoryStore>,
    workspace: StoreWorkspaceId,
    lose_seed_reply: AtomicBool,
    refuse_seed: AtomicBool,
    lose_edit_reply: AtomicBool,
    lose_history_reply: AtomicBool,
    race_edit: AtomicBool,
    snapshot_override: Mutex<Option<Vec<u8>>>,
}
fn at() -> Timestamp {
    Timestamp::from_millis(1_800_000_000_000)
}
impl Embedded {
    fn new() -> Arc<Self> {
        let mut store = Store::open(
            SqliteCatalog::in_memory().unwrap(),
            SqliteBackend::in_memory(),
        )
        .unwrap();
        let profile = store
            .create_profile("Test", None, None, koru_store_core::Locale::default(), at())
            .unwrap();
        let workspace = store.create_workspace(profile.id, "Work", at()).unwrap().id;
        Arc::new(Self {
            store: Mutex::new(store),
            workspace,
            lose_seed_reply: AtomicBool::new(false),
            refuse_seed: AtomicBool::new(false),
            lose_edit_reply: AtomicBool::new(false),
            lose_history_reply: AtomicBool::new(false),
            race_edit: AtomicBool::new(false),
            snapshot_override: Mutex::new(None),
        })
    }
    fn backend(self: &Arc<Self>) -> LocalBackend {
        LocalBackend::with_embedded(self.clone())
    }
}
fn unknown() -> ToolError {
    ToolError {
        code: effects::SAVE_STATUS_UNKNOWN.into(),
        message: "injected lost reply".into(),
        details: None,
    }
}
impl EmbeddedStore for Embedded {
    fn workspace(&self, ws: StoreWorkspaceId) -> Result<Workspace, ToolError> {
        Ok(self.store.lock().unwrap().workspace(ws)?)
    }
    fn document_entry(
        &self,
        ws: StoreWorkspaceId,
        id: StoreDocumentId,
    ) -> Result<Option<DocumentEntry>, ToolError> {
        Ok(self.store.lock().unwrap().document_entry_live(ws, id)?)
    }

    fn workspaces(&self) -> Result<Vec<Workspace>, ToolError> {
        Ok(self.store.lock().unwrap().workspaces()?)
    }
    fn documents(&self, ws: StoreWorkspaceId) -> Result<Vec<DocumentEntry>, ToolError> {
        Ok(self.store.lock().unwrap().documents(ws)?)
    }
    fn snapshot(&self, ws: StoreWorkspaceId, id: StoreDocumentId) -> Result<Vec<u8>, ToolError> {
        if let Some(snapshot) = self.snapshot_override.lock().unwrap().as_ref() {
            return Ok(snapshot.clone());
        }
        Ok(self.store.lock().unwrap().snapshot(ws, id)?)
    }
    fn creation_origin(
        &self,
        ws: StoreWorkspaceId,
        id: StoreDocumentId,
    ) -> Result<CreationOriginLookup, ToolError> {
        Ok(self.store.lock().unwrap().creation_origin(ws, id)?)
    }
    fn seed_document_with_origin(
        &self,
        r: EmbeddedSeedDocument,
        i: NativeStoreIdentity,
    ) -> Result<koru_store_core::ProvenSeededCreation, NativeWriteError> {
        if self.refuse_seed.swap(false, Ordering::SeqCst) {
            return Err(NativeWriteError::Refused(ToolError::permission_denied(
                "injected current authority refusal",
            )));
        }
        let result = self
            .store
            .lock()
            .unwrap()
            .create_seeded_document_with_origin(
                r.workspace,
                &r.title,
                &r.kind,
                r.parent,
                r.index,
                &i.scope,
                &i.request_key,
                &r.request_hash,
                i.candidate,
                r.initial_update,
                r.attribution.as_deref(),
                at(),
            )
            .map_err(|error| NativeWriteError::Unconfirmed(error.into()))?;
        if self.lose_seed_reply.swap(false, Ordering::SeqCst) {
            Err(NativeWriteError::Unconfirmed(unknown()))
        } else {
            Ok(result)
        }
    }
    fn retain_creation_version(
        &self,
        ws: StoreWorkspaceId,
        i: NativeStoreIdentity,
        hash: String,
    ) -> Result<DocumentVersion, ToolError> {
        let result = self.store.lock().unwrap().retain_creation_version(
            ws,
            i.candidate,
            &i.scope,
            &i.request_key,
            &hash,
            at(),
        )?;
        if self.lose_history_reply.swap(false, Ordering::SeqCst) {
            Err(unknown())
        } else {
            Ok(result)
        }
    }
    fn persist_update_at_version(
        &self,
        ws: StoreWorkspaceId,
        id: StoreDocumentId,
        vv: Vec<u8>,
        update: Vec<u8>,
        _identity: NativeStoreIdentity,
        _request_fingerprint: [u8; 32],
    ) -> Result<DocumentVersion, NativeWriteError> {
        let mut store = self.store.lock().unwrap();
        if self.race_edit.swap(false, Ordering::SeqCst) {
            let human = loro::LoroDoc::new();
            human
                .import(
                    &store
                        .snapshot(ws, id)
                        .map_err(|error| NativeWriteError::Refused(error.into()))?,
                )
                .unwrap();
            human.get_map("human").insert("change", "retained").unwrap();
            human.commit();
            store
                .apply_update(
                    ws,
                    id,
                    human.export(loro::ExportMode::all_updates()).unwrap(),
                    at(),
                    UpdateOrigin::Store,
                )
                .map_err(|error| NativeWriteError::Refused(error.into()))?;
            store
                .flush(ws, id, at())
                .map_err(|error| NativeWriteError::Refused(error.into()))?;
        }
        let result = store
            .apply_update_at_version(ws, id, &vv, update, at(), UpdateOrigin::Store)
            .map_err(|error| match error {
                koru_store_core::service::NativeUpdateError::Refused(error) => {
                    NativeWriteError::Refused(error.into())
                }
                koru_store_core::service::NativeUpdateError::Unconfirmed(error) => {
                    NativeWriteError::Unconfirmed(error.into())
                }
            })?;
        if self.lose_edit_reply.swap(false, Ordering::SeqCst) {
            Err(NativeWriteError::Unconfirmed(unknown()))
        } else {
            Ok(result)
        }
    }
    fn search(
        &self,
        _: SearchRequest,
    ) -> Result<koru_store_wire::search::SearchResponse, ToolError> {
        Err(unavailable())
    }
    fn query_tasks(&self, _: TaskQuery) -> Result<TaskQueryResponse, ToolError> {
        Err(unavailable())
    }
    fn create_document(
        &self,
        _: StoreWorkspaceId,
        _: String,
        _: String,
        _: Option<&str>,
    ) -> Result<StoreDocumentId, ToolError> {
        Err(unavailable())
    }
    fn manifest_id(&self, _: StoreWorkspaceId) -> Result<StoreDocumentId, ToolError> {
        Err(unavailable())
    }
    fn ensure_file_note(
        &self,
        _: StoreWorkspaceId,
        _: StoreDocumentId,
    ) -> Result<(StoreDocumentId, bool), ToolError> {
        Err(unavailable())
    }
    fn seed_document(&self, _: EmbeddedSeedDocument) -> Result<EmbeddedSeededDocument, ToolError> {
        Err(unavailable())
    }
    fn persist_update(
        &self,
        _: StoreWorkspaceId,
        _: StoreDocumentId,
        _: Vec<u8>,
    ) -> Result<(), ToolError> {
        Err(unavailable())
    }
    fn capture_history(
        &self,
        _: StoreWorkspaceId,
        _: StoreDocumentId,
        _: CaptureHistory,
    ) -> Result<HistoryEntry, ToolError> {
        Err(unavailable())
    }
    fn history(
        &self,
        _: StoreWorkspaceId,
        _: StoreDocumentId,
        _: HistoryQuery,
    ) -> Result<HistoryPage, ToolError> {
        Err(unavailable())
    }
    fn history_snapshot(
        &self,
        _: StoreWorkspaceId,
        _: HistoryEntryId,
    ) -> Result<Vec<u8>, ToolError> {
        Err(unavailable())
    }
    fn blobs(&self, _: StoreWorkspaceId) -> Result<Vec<Blob>, ToolError> {
        Err(unavailable())
    }
    fn get_blob(
        &self,
        _: StoreWorkspaceId,
        _: koru_store_core::model::BlobHash,
    ) -> Result<Vec<u8>, ToolError> {
        Err(unavailable())
    }
    fn status(&self) -> Result<Status, ToolError> {
        Err(unavailable())
    }
}
fn identity() -> NativeStoreIdentity {
    NativeStoreIdentity {
        candidate: StoreDocumentId::new(),
        scope: "native:owner".into(),
        request_key: uuid::Uuid::new_v4().to_string(),
    }
}
fn create(backend: &LocalBackend, ws: StoreWorkspaceId) -> PreparedNativeWrite {
    backend.prepare_native_write("create_document",json!({"workspace_id":ws.to_storage(),"title":"Result","initial_operations":[{"op":"insert","node_id":"body","parent_id":"root","after_node_id":null,"kind":"paragraph","text":"Original"}]})).unwrap()
}
fn edit(backend: &LocalBackend, store: &Embedded, id: StoreDocumentId) -> PreparedNativeWrite {
    let doc = WorkspaceDoc::from_snapshot(&store.snapshot(store.workspace, id).unwrap()).unwrap();
    let roots = doc.children(Parent::Root).unwrap();
    let root = roots[0];
    backend.prepare_native_write("edit_document",json!({"workspace_id":store.workspace.to_storage(),"document_id":id.to_storage(),"base_revision":revision(&doc),"operations":[{"op":"insert","parent_id":root.to_storage(),"after_node_id":null,"kind":"paragraph","text":"New paragraph"}]})).unwrap()
}

#[test]
fn shared_native_create_edit_and_exact_race_use_real_memory_store() {
    let store = Embedded::new();
    let backend = store.backend();
    let prepared = create(&backend, store.workspace);
    let key = identity();
    let created = backend
        .execute_native_write(&prepared, key.clone())
        .unwrap();
    assert_eq!(created.document, key.candidate);
    let editing = edit(&backend, &store, key.candidate);
    let mut edit_key = identity();
    edit_key.candidate = key.candidate;
    let changed = backend
        .execute_native_write(&editing, edit_key.clone())
        .unwrap();
    assert_eq!(changed.kind, NativeWriteKind::Edited);
    assert_ne!(changed.version, created.version);
    let racing = edit(&backend, &store, key.candidate);
    store.race_edit.store(true, Ordering::SeqCst);
    assert!(backend.execute_native_write(&racing, edit_key).is_err());
    let doc = WorkspaceDoc::from_snapshot(&store.snapshot(store.workspace, key.candidate).unwrap())
        .unwrap();
    assert_eq!(
        doc.doc()
            .get_map("human")
            .get("change")
            .unwrap()
            .into_value()
            .unwrap()
            .as_string()
            .unwrap()
            .as_str(),
        "retained"
    );
}

#[test]
fn uncertain_seed_and_history_recover_without_resubmitting_or_duplicate_markers() {
    for lose_seed in [true, false] {
        let store = Embedded::new();
        let backend = store.backend();
        let prepared = create(&backend, store.workspace);
        let key = identity();
        if lose_seed {
            store.lose_seed_reply.store(true, Ordering::SeqCst);
        } else {
            store.lose_history_reply.store(true, Ordering::SeqCst);
        }
        assert_eq!(
            backend
                .execute_native_write(&prepared, key.clone())
                .err()
                .unwrap()
                .error()
                .code,
            effects::SAVE_STATUS_UNKNOWN
        );
        let NativeReconciliation::Verified(first) = backend
            .reconcile_native_write(&prepared, key.clone())
            .unwrap()
        else {
            panic!("must recover exact proof")
        };
        let NativeReconciliation::Verified(retry) = backend
            .reconcile_native_write(&prepared, key.clone())
            .unwrap()
        else {
            panic!("must replay")
        };
        assert!(first == retry);
        let mut owner = store.store.lock().unwrap();
        assert_eq!(owner.documents(store.workspace).unwrap().len(), 1);
        assert_eq!(
            owner
                .history_entries(store.workspace, key.candidate, None, 10)
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn missing_and_foreign_origin_do_not_create_or_repair_and_edits_do_not_replay() {
    let store = Embedded::new();
    let backend = store.backend();
    let prepared = create(&backend, store.workspace);
    let key = identity();
    assert!(matches!(
        backend
            .reconcile_native_write(&prepared, key.clone())
            .unwrap(),
        NativeReconciliation::Unconfirmed
    ));
    assert!(store.documents(store.workspace).unwrap().is_empty());
    backend
        .execute_native_write(&prepared, key.clone())
        .unwrap();
    let mut foreign = key.clone();
    foreign.scope = "foreign".into();
    assert!(backend.reconcile_native_write(&prepared, foreign).is_err());
    let editing = edit(&backend, &store, key.candidate);
    assert!(matches!(
        backend.reconcile_native_write(&editing, key).unwrap(),
        NativeReconciliation::Unconfirmed
    ));
}

#[test]
fn native_preparation_rejects_secondary_effects_and_normalizes_revision_semantics() {
    let store = Embedded::new();
    let backend = store.backend();
    for extra in [
        json!({"kind":"run"}),
        json!({"parent_document_id":StoreDocumentId::new().to_storage()}),
        json!({"idempotency_key":"caller"}),
    ] {
        let mut args = json!({"workspace_id":store.workspace.to_storage(),"title":"Page"});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(
            backend
                .prepare_native_write("create_document", args)
                .is_err()
        );
    }
    let target = StoreDocumentId::new();
    let node = NodeId::new();
    let request = |revision: &str| json!({"workspace_id":store.workspace.to_storage(),"document_id":target.to_storage(),"base_revision":revision,"operations":[{"op":"replace_text","node_id":node.to_storage(),"expected":"old","text":"new"}]});
    let a = backend
        .prepare_native_write(
            "edit_document",
            request("029493bab193e8c5c20150c4f9a99c9db5ebf68d0116"),
        )
        .unwrap();
    let b = backend
        .prepare_native_write(
            "edit_document",
            request("02c4f9a99c9db5ebf68d01169493bab193e8c5c20150"),
        )
        .unwrap();
    assert_eq!(a.description().fingerprint, b.description().fingerprint);
    assert!(
        backend
            .prepare_native_write("edit_document", request("\u{2603}\u{2603}"))
            .is_err()
    );
    let mut args = request("029493bab193e8c5c20150c4f9a99c9db5ebf68d0116");
    args["operations"] =
        json!([{"op":"add_tag","node_id":node.to_storage(),"expected":"old","tag":"new"}]);
    assert!(backend.prepare_native_write("edit_document", args).is_err());
    assert!(store.documents(store.workspace).unwrap().is_empty());
}

#[test]
fn native_catalog_excludes_unavailable_fields_and_secondary_operations() {
    let catalog = native_tool_catalog();
    assert_eq!(catalog.len(), 4);
    let create = catalog
        .iter()
        .find(|tool| tool.name == "create_document")
        .unwrap();
    let fields = create.input_schema["properties"].as_object().unwrap();
    assert!(!fields.contains_key("parent_document_id"));
    assert!(!fields.contains_key("idempotency_key"));
    for tool in catalog
        .iter()
        .filter(|tool| tool.name == "create_document" || tool.name == "edit_document")
    {
        let encoded = serde_json::to_string(&tool.input_schema).unwrap();
        for denied in [
            "add_tag",
            "remove_tag",
            "set_attribute",
            "insert_inline",
            "set_done",
        ] {
            assert!(!encoded.contains(denied), "advertised {denied}");
        }
        assert!(!encoded.contains("\"task\""));
        assert!(encoded.contains("replace_text") || encoded.contains("insert"));
    }
}

#[test]
fn native_identity_refuses_wrong_uuid_variant_and_noncanonical_key() {
    let mut key = identity();
    key.request_key = "11111111-1111-4111-0111-111111111111".into();
    assert!(validate_identity(&key).is_err());
    key.request_key = uuid::Uuid::new_v4().to_string().to_uppercase();
    assert!(validate_identity(&key).is_err());
    key.request_key = uuid::Uuid::new_v4().to_string();
    key.candidate = StoreDocumentId::parse("11111111-1111-4111-0111-111111111111").unwrap();
    assert!(validate_identity(&key).is_err());
}

#[test]
fn no_change_is_a_definite_refusal_before_store_write_or_saved_evidence() {
    let store = Embedded::new();
    let backend = store.backend();
    let key = identity();
    backend
        .execute_native_write(&create(&backend, store.workspace), key.clone())
        .unwrap();
    let doc = WorkspaceDoc::from_snapshot(&store.snapshot(store.workspace, key.candidate).unwrap())
        .unwrap();
    let root = doc.children(Parent::Root).unwrap()[0];
    let body = doc.children(Parent::Node(root)).unwrap()[0];
    let prepared=backend.prepare_native_write("edit_document",json!({"workspace_id":store.workspace.to_storage(),"document_id":key.candidate.to_storage(),"base_revision":revision(&doc),"operations":[{"op":"replace_text","node_id":body.to_storage(),"expected":"Original","text":"Original"}]})).unwrap();
    let result = backend.execute_native_write(&prepared, key.clone());
    assert!(
        matches!(result,Err(NativeWriteError::Refused(ref error)) if error.code=="native_no_change")
    );
    let mut owner = store.store.lock().unwrap();
    assert_eq!(
        owner.version(store.workspace, key.candidate).unwrap(),
        doc.version()
    );
    assert_eq!(
        owner
            .history_entries(store.workspace, key.candidate, None, 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn bounded_argument_counter_refuses_before_a_second_serialized_copy() {
    use std::io::Write;
    let mut counter = ArgumentCounter(4 * 1024 * 1024);
    assert!(counter.write(b"x").is_err());
    let store = Embedded::new();
    let backend = store.backend();
    assert!(
        backend
            .prepare_native_write(
                "create_document",
                json!({"workspace_id":store.workspace.to_storage(),"title":"x".repeat(4*1024*1024)})
            )
            .is_err()
    );
    assert!(store.documents(store.workspace).unwrap().is_empty());
}

#[test]
fn typed_effect_boundary_preserves_prewrite_refusal_and_postwrite_uncertainty() {
    let store = Embedded::new();
    let backend = store.backend();
    let prepared = create(&backend, store.workspace);
    let key = identity();
    store.refuse_seed.store(true, Ordering::SeqCst);
    assert!(matches!(
        backend.execute_native_write(&prepared, key.clone()),
        Err(NativeWriteError::Refused(_))
    ));
    assert!(store.documents(store.workspace).unwrap().is_empty());
    backend
        .execute_native_write(&prepared, key.clone())
        .unwrap();
    let before = store
        .store
        .lock()
        .unwrap()
        .version(store.workspace, key.candidate)
        .unwrap();
    let editing = edit(&backend, &store, key.candidate);
    store.lose_edit_reply.store(true, Ordering::SeqCst);
    assert!(matches!(
        backend.execute_native_write(&editing, key.clone()),
        Err(NativeWriteError::Unconfirmed(_))
    ));
    let after = store
        .store
        .lock()
        .unwrap()
        .version(store.workspace, key.candidate)
        .unwrap();
    assert_ne!(before, after);
    assert!(matches!(
        backend
            .reconcile_native_write(&editing, key.clone())
            .unwrap(),
        NativeReconciliation::Unconfirmed
    ));
    assert_eq!(
        store
            .store
            .lock()
            .unwrap()
            .version(store.workspace, key.candidate)
            .unwrap(),
        after
    );
}

#[test]
fn blank_result_titles_refuse_before_effect() {
    let store = Embedded::new();
    let backend = store.backend();
    assert!(
        backend
            .prepare_native_write(
                "create_document",
                json!({"workspace_id":store.workspace.to_storage(),"title":"   "})
            )
            .is_err()
    );
    let key = identity();
    backend
        .execute_native_write(&create(&backend, store.workspace), key.clone())
        .unwrap();
    let editing = edit(&backend, &store, key.candidate);
    let mut owner = store.store.lock().unwrap();
    owner
        .rename_document_entry(store.workspace, key.candidate, "   ", at())
        .unwrap();
    let before = owner.version(store.workspace, key.candidate).unwrap();
    drop(owner);
    assert!(matches!(
        backend.execute_native_write(&editing, key.clone()),
        Err(NativeWriteError::Refused(_))
    ));
    assert_eq!(
        store
            .store
            .lock()
            .unwrap()
            .version(store.workspace, key.candidate)
            .unwrap(),
        before
    );
}

/// Cohorts `m6_save_keeps_version_id` and `m6_pinned_read`, the shared half:
/// both native writes return the Store's version id with their evidence, and
/// a snapshot is rendered exactly as `read_document` renders the live page.
#[test]
fn m6_save_keeps_version_id_and_m6_pinned_read_render_what_read_document_renders() {
    let store = Embedded::new();
    let backend = store.backend();
    let prepared = create(&backend, store.workspace);
    let key = identity();
    let created = backend
        .execute_native_write(&prepared, key.clone())
        .unwrap();
    let first = created.version_id.expect("a create carries the Store's id");
    let read = |document: StoreDocumentId| {
        backend
            .invoke(
                &RequestContext::local(),
                "read_document",
                json!({"workspace_id": store.workspace.to_storage(), "document_id": document.to_storage()}),
            )
            .unwrap()
    };
    // The page as it is now, and its snapshot rendered by the pinned
    // renderer: the same typed-node tree and the same revision token.
    let live = read(key.candidate);
    let pinned_bytes = store.snapshot(store.workspace, key.candidate).unwrap();
    assert_eq!(
        render_pinned_document(key.candidate, &pinned_bytes).unwrap(),
        live
    );
    // An edit produces another id, and the earlier bytes still render the
    // earlier page: the renderer reads its argument and nothing else.
    let editing = edit(&backend, &store, key.candidate);
    let mut edit_key = identity();
    edit_key.candidate = key.candidate;
    let edited = backend.execute_native_write(&editing, edit_key).unwrap();
    let second = edited.version_id.expect("an edit carries the Store's id");
    assert_ne!(first, second);
    let after = read(key.candidate);
    assert_ne!(after, live);
    assert_eq!(
        render_pinned_document(key.candidate, &pinned_bytes).unwrap(),
        live
    );
    assert!(!live.to_string().contains("New paragraph"));
    assert!(after.to_string().contains("New paragraph"));
    // Bytes that are not a page are refused, never rendered as an empty one.
    assert!(render_pinned_document(key.candidate, b"not a page").is_err());
}
