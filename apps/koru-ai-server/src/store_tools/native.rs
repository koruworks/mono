// SPDX-License-Identifier: MPL-2.0

//! Per-invocation native authority, checked by the existing Store owner at use.
use super::*;
use koru_mcp_server::native::{NativeStoreIdentity, NativeWriteError};
use koru_store_core::ids::{ProfileId, UserScopeId};
use koru_store_core::ports::DocumentVersion;
use koru_store_core::service::NativeUpdateError;
use koru_store_core::{
    CreationOriginLookup, NativeEffectBinding, NativeEffectKind, ProvenSeededCreation,
};
use koru_store_server::{Announcements, WorkerStore};

/// Supplied only by trusted Run classification and current AI owner evidence.
/// These values are not accepted from tool arguments or an HTTP principal.
#[derive(Clone)]
pub struct NativeStoreAuthority {
    pub owner_scope: UserScopeId,
    pub expected_profile: ProfileId,
    pub workspace: WorkspaceId,
}

pub fn native_store_backend(
    client: StoreWorkerClient,
    authority: NativeStoreAuthority,
) -> Arc<LocalBackend> {
    Arc::new(LocalBackend::with_embedded(Arc::new(NativeStore {
        client,
        authority,
    })))
}

/// Targeted metadata for a new edit intent; no document body is read.
pub fn native_document_title(
    client: StoreWorkerClient,
    authority: NativeStoreAuthority,
    document: DocumentId,
) -> Result<String, ToolError> {
    let workspace = authority.workspace;
    NativeStore { client, authority }.read(workspace, move |store| {
        let entry = live_entry(store, workspace, document, true)?;
        let mut end = entry.title.len().min(256);
        while !entry.title.is_char_boundary(end) {
            end -= 1;
        }
        let title = entry.title[..end].to_owned();
        if title.trim().is_empty() {
            return Err(invalid("native result requires a title"));
        }
        Ok(title)
    })
}

struct NativeStore {
    client: StoreWorkerClient,
    authority: NativeStoreAuthority,
}
fn denied() -> ToolError {
    ToolError {
        code: "permission_denied".into(),
        message: "The current native document authority does not allow this operation.".into(),
        details: None,
    }
}
fn unsupported() -> ToolError {
    ToolError {
        code: "native_operation_unsupported".into(),
        message: "This Store adapter exposes only scoped native document operations.".into(),
        details: None,
    }
}
fn native_update_error(error: NativeUpdateError) -> NativeWriteError {
    match error {
        NativeUpdateError::Refused(error) => NativeWriteError::Refused(error.into()),
        NativeUpdateError::Unconfirmed(error) => NativeWriteError::Unconfirmed(error.into()),
    }
}
impl NativeStoreAuthority {
    fn check(&self, store: &mut WorkerStore, workspace: WorkspaceId) -> Result<(), ToolError> {
        if workspace != self.workspace
            || store.local_user_scope().map_err(ToolError::from)? != self.owner_scope
            || store
                .current_placement_profile(workspace)
                .map_err(ToolError::from)?
                != Some(self.expected_profile)
        {
            return Err(denied());
        }
        store.workspace(workspace).map_err(ToolError::from)?;
        store
            .profile(self.expected_profile)
            .map_err(ToolError::from)?;
        Ok(())
    }
}
impl NativeStore {
    fn read<T: Send + 'static>(
        &self,
        workspace: WorkspaceId,
        operation: impl FnOnce(&mut WorkerStore) -> Result<T, ToolError> + Send + 'static,
    ) -> Result<T, ToolError> {
        let authority = self.authority.clone();
        self.client
            .with_store(move |store| {
                authority.check(store, workspace)?;
                operation(store)
            })
            .map_err(worker_error)?
    }
    /// One native effect in one Store hop. What it commits is told to the
    /// rooms open views listen to before it answers (`Announcements`), as
    /// the Store's own routes tell them.
    fn effect<T: Send + 'static>(
        &self,
        binding: NativeEffectBinding,
        operation: impl FnOnce(&mut WorkerStore, &mut Announcements) -> Result<T, NativeWriteError>
        + Send
        + 'static,
    ) -> Result<T, NativeWriteError> {
        let authority = self.authority.clone();
        self.client
            .with_store_announcing(move |store, announcements| {
                store
                    .check_native_effect(&binding)
                    .map_err(|error| NativeWriteError::Refused(error.into()))?;
                authority
                    .check(store, binding.workspace)
                    .map_err(NativeWriteError::Refused)?;
                operation(store, announcements)
            })
            .map_err(|error| NativeWriteError::Unconfirmed(worker_error(error)))?
    }
}
fn live_entry(
    store: &mut WorkerStore,
    workspace: WorkspaceId,
    document: DocumentId,
    page: bool,
) -> Result<DocumentEntry, ToolError> {
    let entry = store
        .document_entry_live(workspace, document)
        .map_err(ToolError::from)?
        .ok_or_else(denied)?;
    if page && entry.kind != "page" {
        return Err(denied());
    }
    Ok(entry)
}

impl EmbeddedStore for NativeStore {
    fn workspaces(&self) -> Result<Vec<Workspace>, ToolError> {
        let workspace = self.authority.workspace;
        self.read(workspace, move |store| {
            Ok(vec![store.workspace(workspace).map_err(ToolError::from)?])
        })
    }
    fn workspace(&self, workspace: WorkspaceId) -> Result<Workspace, ToolError> {
        self.read(workspace, move |store| {
            store.workspace(workspace).map_err(ToolError::from)
        })
    }
    fn documents(&self, workspace: WorkspaceId) -> Result<Vec<DocumentEntry>, ToolError> {
        self.read(workspace, move |store| {
            store.documents(workspace).map_err(ToolError::from)
        })
    }
    fn document_entry(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<Option<DocumentEntry>, ToolError> {
        self.read(workspace, move |store| {
            store
                .document_entry_live(workspace, document)
                .map_err(ToolError::from)
        })
    }
    fn snapshot(&self, workspace: WorkspaceId, document: DocumentId) -> Result<Vec<u8>, ToolError> {
        self.read(workspace, move |store| {
            live_entry(store, workspace, document, false)?;
            store.snapshot(workspace, document).map_err(ToolError::from)
        })
    }
    fn search(&self, request: SearchRequest) -> Result<SearchResponse, ToolError> {
        let workspace = self.authority.workspace;
        if request.query.workspaces.is_empty()
            || request.query.workspaces.iter().any(|id| *id != workspace)
        {
            return Err(denied());
        }
        self.read(workspace, |_| Ok(()))?;
        let result = flatten(self.client.search(request))?;
        // Derived search is outside the Store lock. Recheck before exposing any
        // snippets/counts/cursor; this is not a cross-service atomic snapshot.
        self.read(workspace, |_| Ok(()))?;
        Ok(result)
    }
    fn creation_origin(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<CreationOriginLookup, ToolError> {
        self.read(workspace, move |store| {
            let origin = store
                .creation_origin(workspace, document)
                .map_err(ToolError::from)?;
            // An unleased Prepared creation legitimately has no candidate yet.
            // Existing origin bytes remain private unless the page is visible.
            if !matches!(origin, CreationOriginLookup::Missing) {
                live_entry(store, workspace, document, true)?;
            }
            Ok(origin)
        })
    }
    fn seed_document_with_origin(
        &self,
        request: EmbeddedSeedDocument,
        identity: NativeStoreIdentity,
    ) -> Result<ProvenSeededCreation, NativeWriteError> {
        let fingerprint = *blake3::Hash::from_hex(&request.request_hash)
            .map_err(|_| NativeWriteError::Refused(invalid("invalid native fingerprint")))?
            .as_bytes();
        let binding = identity.binding(
            self.authority.owner_scope,
            request.workspace,
            NativeEffectKind::CreateDocument,
            fingerprint,
        )?;
        let workspace = request.workspace;
        self.effect(binding.clone(), move |store, announcements| {
            if request.parent.is_some()
                || request.index.is_some()
                || request.kind != "page"
                || request.idempotency_key != identity.request_key
            {
                return Err(NativeWriteError::Refused(denied()));
            }
            // Once the Store seed/publication begins, any returned error may
            // follow durable bytes. Its original-origin proof resolves retries.
            let created = store
                .create_native_document(
                    &binding,
                    &request.title,
                    request.initial_update,
                    request.attribution.as_deref(),
                    Timestamp::now(),
                )
                .map_err(native_update_error)?;
            if let Some(delta) = created.creation.manifest_delta.clone() {
                announcements.manifest(workspace, delta);
            }
            Ok(created)
        })
    }
    fn persist_update_at_version(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
        expected_version: Vec<u8>,
        update: Vec<u8>,
        identity: NativeStoreIdentity,
        request_fingerprint: [u8; 32],
    ) -> Result<DocumentVersion, NativeWriteError> {
        if identity.candidate != document {
            return Err(NativeWriteError::Refused(denied()));
        }
        let binding = identity.binding(
            self.authority.owner_scope,
            workspace,
            NativeEffectKind::EditDocument,
            request_fingerprint,
        )?;
        self.effect(binding.clone(), move |store, announcements| {
            live_entry(store, workspace, document, true).map_err(NativeWriteError::Refused)?;
            let version = store
                .apply_native_update_at_version(
                    &binding,
                    &expected_version,
                    update.clone(),
                    Timestamp::now(),
                    koru_store_core::ports::UpdateOrigin::Store,
                )
                .map_err(native_update_error)?;
            // Applied and flushed: a window with the page open takes the
            // same bytes, as from the Store's typed update command.
            announcements.document(document, update);
            Ok(version)
        })
    }
    fn retain_creation_version(
        &self,
        workspace: WorkspaceId,
        identity: NativeStoreIdentity,
        request_hash: String,
    ) -> Result<DocumentVersion, ToolError> {
        self.read(workspace, move |store| {
            live_entry(store, workspace, identity.candidate, true)?;
            store
                .retain_creation_version(
                    workspace,
                    identity.candidate,
                    &identity.scope,
                    &identity.request_key,
                    &request_hash,
                    Timestamp::now(),
                )
                .map_err(ToolError::from)
        })
    }
    // The neutral executor cannot escape its native path through an ordinary
    // create/update helper or introduce files, tasks or unrelated history writes.
    fn query_tasks(&self, _: TaskQuery) -> Result<TaskQueryResponse, ToolError> {
        Err(unsupported())
    }
    fn create_document(
        &self,
        _: WorkspaceId,
        _: String,
        _: String,
        _: Option<&str>,
    ) -> Result<DocumentId, ToolError> {
        Err(unsupported())
    }
    fn manifest_id(&self, _: WorkspaceId) -> Result<DocumentId, ToolError> {
        Err(unsupported())
    }
    fn ensure_file_note(
        &self,
        _: WorkspaceId,
        _: DocumentId,
    ) -> Result<(DocumentId, bool), ToolError> {
        Err(unsupported())
    }
    fn seed_document(&self, _: EmbeddedSeedDocument) -> Result<EmbeddedSeededDocument, ToolError> {
        Err(unsupported())
    }
    fn persist_update(&self, _: WorkspaceId, _: DocumentId, _: Vec<u8>) -> Result<(), ToolError> {
        Err(unsupported())
    }
    fn capture_history(
        &self,
        _: WorkspaceId,
        _: DocumentId,
        _: CaptureHistory,
    ) -> Result<HistoryEntry, ToolError> {
        Err(unsupported())
    }
    fn history(
        &self,
        _: WorkspaceId,
        _: DocumentId,
        _: HistoryQuery,
    ) -> Result<HistoryPage, ToolError> {
        Err(unsupported())
    }
    fn history_snapshot(&self, _: WorkspaceId, _: HistoryEntryId) -> Result<Vec<u8>, ToolError> {
        Err(unsupported())
    }
    fn blobs(&self, _: WorkspaceId) -> Result<Vec<Blob>, ToolError> {
        Err(unsupported())
    }
    fn get_blob(&self, _: WorkspaceId, _: BlobHash) -> Result<Vec<u8>, ToolError> {
        Err(unsupported())
    }
    fn status(&self) -> Result<Status, ToolError> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests;
