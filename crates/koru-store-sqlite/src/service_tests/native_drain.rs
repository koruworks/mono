// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_store_core::{
    DocumentId, NativeEffectBinding, NativeEffectKind,
    errors::Code,
    ports::{Catalog, UpdateOrigin, WorkspaceRepo},
    service::{Backend, NativeUpdateError},
};
use std::sync::Arc;

struct Shared(Arc<SqliteBackend>);
impl Backend for Shared {
    type Workspace = crate::SqliteWorkspace;
    type Blobs = crate::blobs::SqliteBlobs;
    fn open_workspace(&self, id: WorkspaceId) -> koru_store_core::errors::Result<Self::Workspace> {
        self.0.open_workspace(id)
    }
    fn create_workspace(
        &self,
        id: WorkspaceId,
    ) -> koru_store_core::errors::Result<Self::Workspace> {
        self.0.create_workspace(id)
    }
    fn delete_workspace(&self, id: WorkspaceId) -> koru_store_core::errors::Result<()> {
        self.0.delete_workspace(id)
    }
    fn objects(&self) -> koru_store_core::errors::Result<Self::Blobs> {
        Backend::objects(self.0.as_ref())
    }
}
struct Harness {
    store: Store<SqliteCatalog, Shared>,
    catalog: SqliteCatalog,
    backend: Arc<SqliteBackend>,
    name: String,
    workspace: WorkspaceId,
}
fn harness() -> Harness {
    let name = format!("native-drain-{}", DocumentId::new().to_storage());
    let catalog = SqliteCatalog::shared_in_memory(&name).unwrap();
    let backend = Arc::new(SqliteBackend::in_memory());
    let mut store = Store::open(
        SqliteCatalog::shared_in_memory(&name).unwrap(),
        Shared(backend.clone()),
    )
    .unwrap();
    let profile = store
        .create_profile("Fixture", None, None, Locale::default(), at())
        .unwrap();
    let workspace = store
        .create_workspace(profile.id, "Fixture", at())
        .unwrap()
        .id;
    Harness {
        store,
        catalog,
        backend,
        name,
        workspace,
    }
}
fn binding(h: &Harness, kind: NativeEffectKind, document: DocumentId) -> NativeEffectBinding {
    NativeEffectBinding {
        user_scope: h.store.local_user_scope().unwrap(),
        scope: "fixture:native".into(),
        request_key: DocumentId::new().as_uuid(),
        workspace: h.workspace,
        document,
        kind,
        request_fingerprint: [7; 32],
    }
}
fn seed(text: &str) -> Vec<u8> {
    let doc = loro::LoroDoc::new();
    doc.get_map("body").insert("text", text).unwrap();
    doc.commit();
    doc.export(loro::ExportMode::all_updates()).unwrap()
}
fn create(h: &mut Harness) -> DocumentId {
    let id = DocumentId::new();
    let b = binding(h, NativeEffectKind::CreateDocument, id);
    h.store
        .create_native_document(&b, "Fixture", seed("original"), None, at())
        .unwrap();
    id
}
fn update(h: &mut Harness, id: DocumentId) -> (Vec<u8>, Vec<u8>) {
    let doc = loro::LoroDoc::new();
    doc.import(&h.store.snapshot(h.workspace, id).unwrap())
        .unwrap();
    let before = doc.oplog_vv();
    doc.get_map("body").insert("text", "edited").unwrap();
    doc.commit();
    (
        before.encode(),
        doc.export(loro::ExportMode::updates(&before)).unwrap(),
    )
}
fn fail_updates(repo: &crate::SqliteWorkspace, id: DocumentId) {
    repo.connection().execute_batch(&format!("CREATE TRIGGER fail_native_flush BEFORE INSERT ON loro_updates WHEN NEW.document_id='{}' BEGIN SELECT RAISE(ABORT,'injected'); END;",id.to_storage())).unwrap();
}

#[test]
fn a_delayed_execute_is_refused_before_create_or_edit_and_ordinary_edits_remain_available() {
    let mut h = harness();
    let id = create(&mut h);
    let (version, bytes) = update(&mut h, id);
    let b = binding(&h, NativeEffectKind::EditDocument, id);
    let proof = h.store.drain_native_effect(&b, at()).unwrap();
    assert!(
        matches!(h.store.apply_native_update_at_version(&b,&version,bytes.clone(),at(),UpdateOrigin::Store),Err(NativeUpdateError::Refused(e)) if e.code()==Code::Forbidden)
    );
    assert_eq!(
        h.store.version(h.workspace, id).unwrap(),
        loro::VersionVector::decode(&version).unwrap()
    );
    assert_eq!(
        h.store
            .drain_native_effect(&b, Timestamp::from_millis(1))
            .unwrap(),
        proof
    );
    let absent = binding(&h, NativeEffectKind::CreateDocument, DocumentId::new());
    h.store.drain_native_effect(&absent, at()).unwrap();
    assert!(
        matches!(h.store.create_native_document(&absent,"Fixture",seed("late"),None,at()),Err(NativeUpdateError::Refused(e)) if e.code()==Code::Forbidden)
    );
    assert!(
        h.backend
            .open_workspace(h.workspace)
            .unwrap()
            .document(absent.document)
            .unwrap()
            .is_none()
    );
    h.store
        .apply_update_at_version(h.workspace, id, &version, bytes, at(), UpdateOrigin::Store)
        .unwrap();
    assert_ne!(
        h.store.version(h.workspace, id).unwrap(),
        loro::VersionVector::decode(&version).unwrap()
    );
}

#[test]
fn a_failed_edit_flush_retains_deny_until_its_dirty_window_is_durable() {
    let mut h = harness();
    let id = create(&mut h);
    let (version, bytes) = update(&mut h, id);
    let b = binding(&h, NativeEffectKind::EditDocument, id);
    let repo = h.backend.open_workspace(h.workspace).unwrap();
    fail_updates(&repo, id);
    let before = repo.document(id).unwrap().unwrap().version_vector;
    assert!(matches!(
        h.store
            .apply_native_update_at_version(&b, &version, bytes, at(), UpdateOrigin::Store),
        Err(NativeUpdateError::Unconfirmed(_))
    ));
    assert!(
        h.store
            .drain_native_effect(&b, Timestamp::from_millis(100))
            .is_err()
    );
    let fence = h.catalog.native_effect_fence(&b).unwrap().unwrap();
    assert_eq!(fence.fenced_at, Timestamp::from_millis(100));
    assert!(fence.drained_at.is_none());
    assert_eq!(repo.document(id).unwrap().unwrap().version_vector, before);
    repo.connection()
        .execute_batch("DROP TRIGGER fail_native_flush")
        .unwrap();
    let proof = h
        .store
        .drain_native_effect(&b, Timestamp::from_millis(20))
        .unwrap();
    assert_eq!(proof.fenced_at, Timestamp::from_millis(100));
    assert_eq!(proof.drained_at, Timestamp::from_millis(20));
    assert_ne!(
        loro::VersionVector::decode(&repo.document(id).unwrap().unwrap().version_vector).unwrap(),
        loro::VersionVector::decode(&before).unwrap()
    );
    assert_eq!(
        h.store.flush(h.workspace, id, at()).unwrap(),
        None,
        "drain left no delayed native window"
    );
}

#[test]
fn creation_drain_settles_pending_manifest_without_repairing_or_replaying_the_seed() {
    let mut h = harness();
    let manifest = h.store.manifest_id(h.workspace).unwrap();
    let repo = h.backend.open_workspace(h.workspace).unwrap();
    fail_updates(&repo, manifest);
    let b = binding(&h, NativeEffectKind::CreateDocument, DocumentId::new());
    assert!(matches!(
        h.store
            .create_native_document(&b, "Fixture", seed("original"), None, at()),
        Err(NativeUpdateError::Unconfirmed(_))
    ));
    assert!(
        repo.document(b.document).unwrap().is_some(),
        "seed already committed"
    );
    let target_version = repo.document(b.document).unwrap().unwrap().version_vector;
    // Seeded creation rolls back its live manifest on direct commit failure.
    // A separately queued manifest window must still settle before drain;
    // neither case authorizes repairing the unpublished candidate.
    assert!(
        h.store
            .document_entry_live(h.workspace, b.document)
            .unwrap()
            .is_none()
    );
    let (_, manifest_update) = update(&mut h, manifest);
    h.store
        .apply_update(
            h.workspace,
            manifest,
            manifest_update,
            at(),
            UpdateOrigin::Store,
        )
        .unwrap();
    assert!(h.store.drain_native_effect(&b, at()).is_err());
    assert!(
        h.catalog
            .native_effect_fence(&b)
            .unwrap()
            .unwrap()
            .drained_at
            .is_none()
    );
    repo.connection()
        .execute_batch("DROP TRIGGER fail_native_flush")
        .unwrap();
    h.store.drain_native_effect(&b, at()).unwrap();
    assert_eq!(
        repo.document(b.document).unwrap().unwrap().version_vector,
        target_version
    );
    assert_eq!(h.store.flush(h.workspace, manifest, at()).unwrap(), None);
    assert!(
        h.store
            .document_entry_live(h.workspace, b.document)
            .unwrap()
            .is_none()
    );
}

#[test]
fn restart_discards_uncommitted_window_without_replaying_it_or_forgetting_the_fence() {
    let mut h = harness();
    let id = create(&mut h);
    let (version, bytes) = update(&mut h, id);
    let b = binding(&h, NativeEffectKind::EditDocument, id);
    let repo = h.backend.open_workspace(h.workspace).unwrap();
    fail_updates(&repo, id);
    assert!(
        h.store
            .apply_native_update_at_version(&b, &version, bytes.clone(), at(), UpdateOrigin::Store)
            .is_err()
    );
    assert!(h.store.drain_native_effect(&b, at()).is_err());
    let Harness {
        store,
        catalog,
        backend,
        name,
        workspace,
    } = h;
    drop(store);
    repo.connection()
        .execute_batch("DROP TRIGGER fail_native_flush")
        .unwrap();
    let mut reopened = Store::open(
        SqliteCatalog::shared_in_memory(&name).unwrap(),
        Shared(backend.clone()),
    )
    .unwrap();
    let proof = reopened
        .drain_native_effect(&b, Timestamp::from_millis(5))
        .unwrap();
    assert!(
        catalog
            .native_effect_fence(&b)
            .unwrap()
            .unwrap()
            .drained_at
            .is_some()
    );
    assert_eq!(
        reopened.version(workspace, id).unwrap(),
        loro::VersionVector::decode(&version).unwrap()
    );
    assert!(matches!(
        reopened.apply_native_update_at_version(&b, &version, bytes, at(), UpdateOrigin::Store),
        Err(NativeUpdateError::Refused(_))
    ));
    assert_eq!(reopened.drain_native_effect(&b, at()).unwrap(), proof);
}

#[test]
fn target_and_workspace_storage_purge_cannot_reenable_a_fenced_operation() {
    let mut h = harness();
    let id = create(&mut h);
    let (version, bytes) = update(&mut h, id);
    let b = binding(&h, NativeEffectKind::EditDocument, id);
    let proof = h.store.drain_native_effect(&b, at()).unwrap();
    h.backend
        .open_workspace(h.workspace)
        .unwrap()
        .drop_document(id)
        .unwrap();
    assert!(
        matches!(h.store.apply_native_update_at_version(&b,&version,bytes,at(),UpdateOrigin::Store),Err(NativeUpdateError::Refused(e)) if e.code()==Code::Forbidden)
    );
    let Harness {
        store,
        catalog,
        backend,
        name,
        workspace,
    } = h;
    drop(store);
    backend.delete_workspace(workspace).unwrap();
    let mut reopened = Store::open(
        SqliteCatalog::shared_in_memory(&name).unwrap(),
        Shared(backend),
    )
    .unwrap();
    assert_eq!(reopened.drain_native_effect(&b, at()).unwrap(), proof);
    assert!(
        catalog
            .native_effect_fence(&b)
            .unwrap()
            .unwrap()
            .drained_at
            .is_some()
    );
}

#[test]
fn moving_a_dirty_target_cannot_turn_an_orphan_window_into_a_drain_proof() {
    let mut h = harness();
    let id = create(&mut h);
    let (version, bytes) = update(&mut h, id);
    let b = binding(&h, NativeEffectKind::EditDocument, id);
    let repo = h.backend.open_workspace(h.workspace).unwrap();
    fail_updates(&repo, id);
    assert!(matches!(
        h.store
            .apply_native_update_at_version(&b, &version, bytes, at(), UpdateOrigin::Store),
        Err(NativeUpdateError::Unconfirmed(_))
    ));
    let profile = h
        .store
        .create_profile("Destination", None, None, Locale::default(), at())
        .unwrap();
    let target = h
        .store
        .create_workspace(profile.id, "Destination", at())
        .unwrap()
        .id;
    h.store
        .transfer_documents(h.workspace, target, id, None, 0, at())
        .unwrap();
    assert!(repo.document(id).unwrap().is_none());
    assert_eq!(h.store.flush(h.workspace, id, at()).unwrap(), None);
    assert_eq!(
        h.store.drain_native_effect(&b, at()).unwrap_err().code(),
        Code::StorageFailed
    );
    assert!(
        h.catalog
            .native_effect_fence(&b)
            .unwrap()
            .unwrap()
            .drained_at
            .is_none()
    );
}
