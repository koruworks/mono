// SPDX-License-Identifier: MPL-2.0

//! Mobile adapter for durable permanent-delete evidence and local decisions.
use super::*;
mod journal;
use koru_core::{
    SystemDoc, WorkspaceDoc,
    pages::purge::{Coverage, Event, Insert},
};
use koru_replication::cloud::{client_store::removal, transfer};
use koru_store_core::{Backend, Store, ports::Catalog};

#[derive(Serialize)]
pub(super) struct Prompt {
    pub document_id: String,
    pub workspace_id: String,
    pub title: String,
    pub unknown: bool,
    pub expected_version: Vec<u8>,
    pub needs_quiescence: bool,
}

fn db<T>(value: koru_store_core::Result<T>) -> Result<T, String> {
    value.map_err(|error| error.code().as_str().to_owned())
}
fn invalid() -> String {
    "document_removal_failed".into()
}

fn model(snapshot: &[u8]) -> Result<WorkspaceDoc, String> {
    WorkspaceDoc::from_snapshot(snapshot).map_err(|_| invalid())
}

fn source_title(source: &WorkspaceDoc) -> Option<String> {
    source
        .children(koru_core::schema::Parent::Root)
        .ok()?
        .into_iter()
        .find(|root| source.kind_of(*root) == Some(koru_core::Kind::Document))
        .and_then(|root| source.read(root).ok())
        .and_then(|node| node.content.as_text().map(str::to_owned))
        .or_else(|| source.name())
}

fn identity(
    document: DocumentId,
    peer: u64,
    version: &[u8],
    purpose: &str,
) -> (String, uuid::Uuid) {
    let key = format!(
        "{purpose}:{}:{peer:x}:{}",
        document,
        koru_store_core::checksum(version)
    );
    let derived = uuid::Uuid::new_v5(&document.as_uuid(), key.as_bytes());
    // A stable, UUIDv4-shaped candidate per device and exact frozen coverage.
    // Store's durable seeded-creation receipt is the authority on replay.
    (
        key,
        uuid::Builder::from_random_bytes(*derived.as_bytes()).into_uuid(),
    )
}

pub(super) fn permanent<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    document: DocumentId,
    at: Timestamp,
) -> Result<(), String> {
    let started = std::time::Instant::now();
    let entries = db(store.documents(workspace))?;
    let entry = entries
        .iter()
        .find(|entry| entry.id == document)
        .ok_or_else(invalid)?;
    if !koru_core::explore::listed(&entry.kind) {
        return Err(invalid());
    }
    db(store.flush(workspace, document, at))?;
    let version = db(store.version(workspace, document))?;
    let mut system = match removal::load(store, workspace).map_err(|_| invalid())? {
        Some(system) => system,
        None => {
            let content = SystemDoc::create().map_err(|_| invalid())?;
            let snapshot = content.export_snapshot().map_err(|_| invalid())?;
            let (document, _) = db(store.import_document(
                workspace,
                "",
                koru_core::explore::KIND_SYSTEM,
                None,
                snapshot,
                at,
            ))?;
            removal::System { document, content }
        }
    };
    let mut event = Event::new(document.as_uuid(), document.as_uuid(), Some(&version), "");
    event.event_id = identity(document, store.peer_id(), &version.encode(), "del").1;
    let event = transfer::removal::bounded_event(event).map_err(|_| invalid())?;
    if system
        .content
        .write_purge_event(&event)
        .map_err(|_| invalid())?
        == Insert::Conflict
    {
        return Err(invalid());
    }
    let update = system.content.export_all_updates().map_err(|_| invalid())?;
    db(store.apply_update(
        workspace,
        system.document,
        update,
        at,
        koru_store_core::ports::UpdateOrigin::Store,
    ))?;
    db(store.flush(workspace, system.document, at))?;
    tracing::info!(target: "cloud.document.removal", event_id = %event.event_id, document_id = %document, stage = "event", code = "intent_durable", age_ms = started.elapsed().as_millis() as u64, age_scope = "intent");
    // Only the acknowledged system tombstone permits freezing and release.
    db(store.freeze_document_purge(workspace, document, at))?;
    db(store.release_document_purge(workspace, document, &version, None, at))?;
    tracing::info!(target: "cloud.document.removal", document_id = %document, stage = "reconciled", code = "deleted", age_ms = started.elapsed().as_millis() as u64, age_scope = "intent");
    Ok(())
}

pub(super) fn reconcile<C: Catalog, B: Backend>(
    root: &Path,
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    at: Timestamp,
) -> Result<(Vec<Prompt>, Vec<String>), String> {
    let Some(system) = removal::load(store, workspace).map_err(|_| invalid())? else {
        return Ok((Vec::new(), Vec::new()));
    };
    let entries = db(store.documents(workspace))?;
    let mut prompts = Vec::new();
    let mut tombstones = Vec::new();
    for core_id in system.content.purged_documents() {
        tombstones.push(core_id.to_storage());
        let document = DocumentId::from_uuid(core_id.as_uuid());
        if db(store.locate(document))? != Some(workspace) {
            journal::remove(root, document);
            continue;
        }
        let Some(mut retained) = journal::load(root, store, workspace, document, at)? else {
            continue;
        };
        if retained.decision.is_some() {
            complete_decision(root, store, workspace, document, retained, at)?;
            continue;
        }
        let source = model(&retained.snapshot)?;
        let version = source.version();
        let coverage = system
            .content
            .purge_coverage(core_id)
            .unwrap_or(Coverage::Unknown);
        if retained.quiesced && (coverage.covers(&version) || version.is_empty()) {
            retained.decision = Some(journal::Decision::Delete);
            journal::save(root, document, &retained)?;
            complete_decision(root, store, workspace, document, retained, at)?;
            continue;
        }
        let title = entries
            .iter()
            .find(|entry| entry.id == document)
            .map(|entry| entry.title.clone())
            .filter(|title| !title.is_empty())
            .or_else(|| source_title(&source))
            .unwrap_or_default();
        prompts.push(Prompt {
            document_id: document.to_storage(),
            workspace_id: workspace.to_storage(),
            title,
            unknown: matches!(coverage, Coverage::Unknown),
            expected_version: version.encode(),
            needs_quiescence: !retained.quiesced,
        });
    }
    Ok((prompts, tombstones))
}

pub(super) fn decide<C: Catalog, B: Backend>(
    root: &Path,
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    document: DocumentId,
    expected: &[u8],
    keep: bool,
    at: Timestamp,
) -> Result<(), String> {
    let expected = loro::VersionVector::decode(expected).map_err(|_| invalid())?;
    let Some(mut retained) = journal::load(root, store, workspace, document, at)? else {
        return Ok(());
    };
    let source = model(&retained.snapshot)?;
    if !retained.quiesced {
        return Err("document_removal_needs_quiescence".into());
    }
    if source.version() != expected {
        return Err("document_removal_changed".into());
    }
    if let Some(decision) = &retained.decision {
        if decision.candidate()?.is_some() != keep {
            return Err("document_removal_changed".into());
        }
    } else {
        retained.decision = Some(if keep {
            let entry = db(store.documents(workspace))?
                .into_iter()
                .find(|entry| entry.id == document);
            let title = entry
                .as_ref()
                .map(|entry| entry.title.clone())
                .or_else(|| source_title(&source))
                .unwrap_or_default();
            let file = source
                .children(koru_core::schema::Parent::Root)
                .map_err(|_| invalid())?
                .into_iter()
                .any(|root| source.kind_of(root) == Some(koru_core::Kind::File));
            let kind = entry
                .as_ref()
                .map(|entry| entry.kind.as_str())
                .unwrap_or(if file {
                    "file"
                } else {
                    koru_core::explore::KIND_DOCUMENT
                })
                .to_owned();
            let (key, candidate) = identity(document, store.peer_id(), &expected.encode(), "keep");
            journal::Decision::Keep {
                candidate: DocumentId::from_uuid(candidate).to_storage(),
                title,
                kind,
                key,
            }
        } else {
            journal::Decision::Delete
        });
        // The exact candidate, title and decision survive a crash before seed
        // or release. Inventory resumes this choice without asking again.
        journal::save(root, document, &retained)?;
    }
    complete_decision(root, store, workspace, document, retained, at)
}

fn complete_decision<C: Catalog, B: Backend>(
    root: &Path,
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    document: DocumentId,
    retained: journal::Retained,
    at: Timestamp,
) -> Result<(), String> {
    let started = std::time::Instant::now();
    let source = model(&retained.snapshot)?;
    let expected = source.version();
    let decision = retained.decision.as_ref().ok_or_else(invalid)?;
    let kept = match decision {
        journal::Decision::Delete => None,
        journal::Decision::Keep {
            candidate,
            title,
            kind,
            key,
        } => {
            let candidate = DocumentId::parse(candidate).ok_or_else(invalid)?;
            let copy = source
                .fork_as_document(koru_core::DocumentId::from_uuid(candidate.as_uuid()))
                .map_err(|_| invalid())?;
            let update = copy.export_all_updates().map_err(|_| invalid())?;
            let seeded = db(store.create_seeded_document(
                workspace,
                title,
                kind,
                None,
                None,
                "document-removal-keep-v1",
                key,
                &koru_store_core::checksum(&expected.encode()),
                Some(candidate),
                update,
                at,
            ))?;
            Some(seeded.document)
        }
    };
    let fence = loro::VersionVector::decode(&retained.fence).map_err(|_| invalid())?;
    db(store.release_document_purge(workspace, document, &fence, kept, at))?;
    journal::remove(root, document);
    tracing::info!(target: "cloud.document.removal", document_id = %document, stage = "reconciled", code = if kept.is_some() { "kept" } else { "deleted" }, age_ms = started.elapsed().as_millis() as u64, age_scope = "release");
    Ok(())
}

impl MobileHost {
    pub(super) fn stage_removed_document_text(
        &self,
        id: &str,
        edit: koru_contract::mobile_proto::DocumentTextEdit,
    ) -> Result<(), String> {
        use prost::Message;
        let document = DocumentId::parse(id).ok_or_else(invalid)?;
        let block = koru_core::NodeId::parse(&edit.block_id).ok_or_else(invalid)?;
        if edit.before.len() > 1024 * 1024 || edit.after.len() > 1024 * 1024 {
            return Err(invalid());
        }
        let receipt = koru_store_core::checksum(&edit.encode_to_vec());
        let root = self.root.clone();
        let peer = self.document_edit_peer;
        self.with_store(move |store| {
            let workspace = db(store.locate(document))?.ok_or_else(invalid)?;
            let Some(mut retained) = journal::load(
                &root,
                store,
                workspace,
                document,
                Timestamp::from_millis(now_millis()),
            )?
            else {
                return Err("document_removal_changed".into());
            };
            if retained.applied.contains(&receipt) {
                return Ok(());
            }
            if retained.quiesced || retained.applied.len() >= 16_384 {
                return Err(invalid());
            }
            let mut source = model(&retained.snapshot)?;
            source.doc().set_peer_id(peer).map_err(|_| invalid())?;
            super::document_edit::apply_text_edit(&mut source, block, &edit)?;
            retained.snapshot = source.export_snapshot().map_err(|_| invalid())?;
            retained.applied.push(receipt);
            journal::save(&root, document, &retained)
        })
    }

    pub(super) fn quiesce_document_removal(&self, id: &str) -> Result<(), String> {
        let document = DocumentId::parse(id).ok_or_else(invalid)?;
        let root = self.root.clone();
        self.with_store(move |store| {
            let Some(workspace) = db(store.locate(document))? else {
                return Ok(());
            };
            let Some(mut retained) = journal::load(
                &root,
                store,
                workspace,
                document,
                Timestamp::from_millis(now_millis()),
            )?
            else {
                return Ok(());
            };
            retained.quiesced = true;
            journal::save(&root, document, &retained)
        })
    }
}

impl MobileHost {
    pub(super) fn decide_document_removal(
        &self,
        document: &str,
        expected: &[u8],
        keep: bool,
    ) -> Result<(), String> {
        let document = DocumentId::parse(document).ok_or_else(invalid)?;
        let expected = expected.to_vec();
        let root = self.root.clone();
        self.with_store(move |store| {
            let Some(workspace) = db(store.locate(document))? else {
                journal::remove(&root, document);
                return Ok(());
            };
            decide(
                &root,
                store,
                workspace,
                document,
                &expected,
                keep,
                Timestamp::from_millis(now_millis()),
            )
        })?;
        self.document_views
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&document);
        self.network.notify_local_change();
        self.wake_cloud();
        Ok(())
    }
}
