// SPDX-License-Identifier: MPL-2.0

//! Narrow adapter from the AI context port to the bounded Store worker.

use koru_ai_core::{
    AiError, AiErrorClass, DocumentContext, DocumentDestination, DocumentId, ProfileCatalog,
    ProfileId, StoreContextPort, StoreRevision, WorkspaceId,
};
use koru_core::WorkspaceDoc;
use koru_export::NoBlobs;
use koru_store_server::StoreWorkerClient;
use uuid::Uuid;

const MAX_DOCUMENT_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct StoreContextAdapter {
    client: StoreWorkerClient,
}

impl StoreContextAdapter {
    pub fn new(client: StoreWorkerClient) -> Self {
        Self { client }
    }
}

struct StoredDocument {
    title: String,
    snapshot: Vec<u8>,
}

impl StoreContextPort for StoreContextAdapter {
    fn resolve_document(
        &self,
        document: DocumentId,
        reader: Option<ProfileId>,
    ) -> Result<DocumentContext, AiError> {
        // A chat not yet placed reads nothing (AI schema 13).
        let reader = reader.ok_or_else(document_unavailable)?;
        let stored = self
            .client
            .with_store(move |store| {
                let workspace = store
                    .locate(document)
                    .map_err(|_| context_unavailable())?
                    .ok_or_else(document_unavailable)?;
                // **Only the reader's profile's documents**: another
                // profile's is the same answer as a missing one.
                if store
                    .workspace(workspace)
                    .map_err(|_| context_unavailable())?
                    .owner
                    != reader
                {
                    return Err(document_unavailable());
                }
                let entry = store
                    .documents(workspace)
                    .map_err(|_| context_unavailable())?
                    .into_iter()
                    .find(|entry| entry.id == document && entry.trashed_at.is_none())
                    .ok_or_else(document_unavailable)?;
                let snapshot = store
                    .snapshot(workspace, document)
                    .map_err(|_| context_unavailable())?;
                Ok::<_, AiError>(StoredDocument {
                    title: entry.title,
                    snapshot,
                })
            })
            .map_err(|_| context_unavailable())??;

        if stored.snapshot.len() > MAX_DOCUMENT_SNAPSHOT_BYTES {
            return Err(AiError::new(
                AiErrorClass::ContextTooLarge,
                "The selected document is too large to use as AI context.",
                false,
            ));
        }
        let workspace = WorkspaceDoc::from_snapshot(&stored.snapshot).map_err(|_| {
            AiError::new(
                AiErrorClass::Storage,
                "The selected document could not be read.",
                false,
            )
        })?;
        let revision = StoreRevision(workspace.version().encode());
        let markdown = koru_export_markdown::render_document(&workspace, &NoBlobs, "")
            .map_err(|_| {
                AiError::new(
                    AiErrorClass::Storage,
                    "The selected document could not be prepared for AI.",
                    false,
                )
            })?
            .markdown;
        Ok(DocumentContext {
            document_id: document,
            display_title: stored.title,
            source_revision: revision,
            markdown,
        })
    }

    fn create_document(
        &self,
        idempotency_key: Uuid,
        destination: &DocumentDestination,
        markdown: &str,
    ) -> Result<DocumentId, AiError> {
        let workspace = self
            .client
            .with_store({
                let workspace = destination.workspace_id;
                move |store| store.workspace(workspace)
            })
            .map_err(|_| export_unavailable())?
            .map_err(|_| export_unavailable())?;
        if workspace.owner != destination.profile_id {
            return Err(AiError::new(
                AiErrorClass::Storage,
                "The selected document destination is no longer available.",
                false,
            ));
        }
        let snapshot = koru_import_markdown::document_snapshot(
            koru_core::WorkspaceId::from_uuid(destination.workspace_id.as_uuid()),
            &destination.title,
            markdown,
        )
        .map_err(|_| {
            AiError::new(
                AiErrorClass::Storage,
                "The AI response could not be prepared as a Koru document.",
                false,
            )
        })?;
        let mut request = blake3::Hasher::new();
        for value in [
            destination.profile_id.to_storage(),
            destination.workspace_id.to_storage(),
            destination
                .parent_document_id
                .map(|id| id.to_storage())
                .unwrap_or_default(),
            destination
                .index
                .map(|index| index.to_string())
                .unwrap_or_default(),
            destination.title.clone(),
            markdown.to_owned(),
        ] {
            request.update(value.as_bytes());
            request.update(&[0]);
        }
        self.client
            .seed_document(
                destination.workspace_id,
                destination.title.clone(),
                koru_core::explore::KIND_DOCUMENT.to_owned(),
                destination.parent_document_id,
                destination.index,
                "koru-ai-export".into(),
                idempotency_key.to_string(),
                request.finalize().to_hex().to_string(),
                snapshot,
                koru_ai_core::Timestamp::now(),
            )
            .map_err(|_| export_unavailable())?
            .map_err(|_| export_unavailable())
    }

    /// The live profiles in the Store's order — the order Koru lists them,
    /// oldest first — and the owner of every workspace, those in the bin
    /// too: a chat that last read a workspace since binned is still that
    /// profile's.
    fn profile_catalog(&self) -> Result<ProfileCatalog, AiError> {
        self.client
            .with_store(|store| {
                let profiles = store.profiles()?.into_iter().map(|profile| profile.id);
                let owners = store
                    .workspaces_including_deleted()?
                    .into_iter()
                    .map(|workspace| (workspace.id, workspace.owner));
                Ok::<_, koru_store_core::Error>(ProfileCatalog {
                    profiles: profiles.collect(),
                    workspace_owners: owners.collect(),
                })
            })
            .map_err(|_| profiles_unavailable())?
            .map_err(|_| profiles_unavailable())
    }

    fn document_workspace(&self, document: DocumentId) -> Result<Option<WorkspaceId>, AiError> {
        self.client
            .with_store(move |store| store.locate(document))
            .map_err(|_| context_unavailable())?
            .map_err(|_| context_unavailable())
    }

    fn retired_profiles(&self) -> Result<Vec<ProfileId>, AiError> {
        // The window the Store's own purge keeps a deleted workspace for.
        let before = koru_store_core::Timestamp::from_millis(
            koru_store_core::Timestamp::now()
                .millis()
                .saturating_sub(koru_store_core::TRASH_RETENTION_MS),
        );
        self.client
            .with_store(move |store| store.profiles_removed_before(before))
            .map_err(|_| profiles_unavailable())?
            .map_err(|_| profiles_unavailable())
    }
}

fn profiles_unavailable() -> AiError {
    AiError::new(
        AiErrorClass::Storage,
        "Koru's profiles are temporarily unavailable.",
        true,
    )
}

fn export_unavailable() -> AiError {
    AiError::new(
        AiErrorClass::Storage,
        "The Koru document could not be created right now.",
        true,
    )
}

fn document_unavailable() -> AiError {
    AiError::new(
        AiErrorClass::Storage,
        "The selected document is not available.",
        false,
    )
}

fn context_unavailable() -> AiError {
    AiError::new(
        AiErrorClass::Storage,
        "Document context is temporarily unavailable.",
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_ai_core::Timestamp;
    use koru_core::{Node, Parent, WorkspaceId};
    use koru_store_core::{Locale, ports::UpdateOrigin};

    #[test]
    fn adapter_reads_one_authoritative_snapshot_and_renders_markdown() {
        let root = tempfile::tempdir().unwrap();
        let worker = koru_store_server::StoreWorker::new(
            koru_store_server::WorkerConfig::embedded(root.path().to_owned(), "internal-debug"),
        )
        .spawn()
        .unwrap();
        let client = worker.client();
        let (profile, other, document) = client
            .with_store(|store| {
                let at = Timestamp::from_millis(1);
                let profile =
                    store.create_profile("Test", None, None, Locale::parse("en").unwrap(), at)?;
                let other = store.create_profile(
                    "Other",
                    None,
                    None,
                    Locale::parse("en").unwrap(),
                    Timestamp::from_millis(2),
                )?;
                let workspace = store.create_workspace(profile.id, "Workspace", at)?;
                let (document, _) =
                    store.create_document(workspace.id, "Roadmap", "page", None, None, at)?;
                let mut content = WorkspaceDoc::create(WorkspaceId::new(), "Roadmap").unwrap();
                let root = content
                    .insert(Parent::Root, Node::document("Roadmap").unwrap())
                    .unwrap();
                content
                    .insert(Parent::Node(root), Node::paragraph("Ship context").unwrap())
                    .unwrap();
                store.apply_update(
                    workspace.id,
                    document,
                    content.export_all_updates().unwrap(),
                    at,
                    UpdateOrigin::Store,
                )?;
                store.flush(workspace.id, document, at)?;
                Ok::<_, koru_store_core::Error>((profile.id, other.id, document))
            })
            .unwrap()
            .unwrap();

        let adapter = StoreContextAdapter::new(client);
        let resolved = adapter.resolve_document(document, Some(profile)).unwrap();
        assert_eq!(resolved.document_id, document);
        assert_eq!(resolved.display_title, "Roadmap");
        assert_eq!(resolved.markdown, "# Roadmap\n\nShip context\n");
        assert!(!resolved.source_revision.0.is_empty());

        // **Another profile's chat reads nothing of it** (AI schema 13), and
        // a chat not yet placed reads nothing at all: both are the answer a
        // missing document gets.
        let missing = adapter
            .resolve_document(DocumentId::new(), Some(profile))
            .unwrap_err();
        for reader in [Some(other), None] {
            let refused = adapter.resolve_document(document, reader).unwrap_err();
            assert_eq!(refused.public_message, missing.public_message);
            assert_eq!(refused.class, missing.class);
        }
        let catalog = adapter.profile_catalog().unwrap();
        assert_eq!(catalog.profiles.len(), 2);
        assert_eq!(catalog.first(), Some(profile), "the older one first");
        assert_eq!(catalog.workspace_owners.len(), 1);
        assert!(adapter.document_workspace(document).unwrap().is_some());

        drop(adapter);
        assert_eq!(worker.join(), koru_store_server::WorkerExit::Clean);
    }

    #[test]
    fn export_is_content_first_retry_safe_and_publishes_one_semantic_document() {
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
        let adapter = StoreContextAdapter::new(client.clone());
        let destination = DocumentDestination {
            profile_id: profile,
            workspace_id: workspace,
            parent_document_id: None,
            index: Some(0),
            title: "Saved conversation".into(),
        };
        let key = Uuid::new_v4();
        let first = adapter
            .create_document(key, &destination, "## You\n\nHello\n")
            .unwrap();
        let replay = adapter
            .create_document(key, &destination, "## You\n\nHello\n")
            .unwrap();
        assert_eq!(replay, first);
        let (entries, snapshot) = client
            .with_store(move |store| {
                Ok::<_, koru_store_core::Error>((
                    store.documents(workspace)?,
                    store.snapshot(workspace, first)?,
                ))
            })
            .unwrap()
            .unwrap();
        assert_eq!(entries.iter().filter(|entry| entry.id == first).count(), 1);
        let document = WorkspaceDoc::from_snapshot(&snapshot).unwrap();
        let rendered = koru_export_markdown::render_document(&document, &NoBlobs, "").unwrap();
        assert!(rendered.markdown.starts_with("# Saved conversation\n"));
        drop(adapter);
        drop(client);
        assert_eq!(worker.join(), koru_store_server::WorkerExit::Clean);
    }
}
