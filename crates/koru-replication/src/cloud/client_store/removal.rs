// SPDX-License-Identifier: MPL-2.0
//! Both deletion carriers converge in the durable workspace system document.
//! Execute inside the Store owner. A network answer never advances a cursor.
use super::*;
use koru_core::{
    SystemDoc,
    pages::purge::{Event, Insert},
};
use uuid::Uuid;

pub struct System {
    pub document: DocumentId,
    pub content: SystemDoc,
}

fn invalid() -> Failure {
    Error::Invalid.into()
}

fn system_ids<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
) -> Result<Vec<DocumentId>, Failure> {
    let mut ids: Vec<_> = store
        .documents(workspace)?
        .into_iter()
        .filter(|entry| entry.kind == koru_core::explore::KIND_SYSTEM)
        .map(|entry| entry.id)
        .collect();
    ids.sort_unstable();
    Ok(ids)
}

pub fn load<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
) -> Result<Option<System>, Failure> {
    let mut merged: Option<System> = None;
    for document in system_ids(store, workspace)? {
        // A freshly downloaded manifest can list a system whose content has
        // not arrived yet. Missing/empty placeholders are not corrupt system
        // documents and must not block enumeration or the next download.
        let bytes = match store.snapshot(workspace, document) {
            Ok(bytes) => bytes,
            Err(error) if error.code() == Code::DocumentNotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let content = SystemDoc::from_snapshot(&bytes).map_err(|_| invalid())?;
        if content.version().is_empty() {
            continue;
        }
        content.purge_events().map_err(|_| invalid())?;
        match &mut merged {
            Some(system) => system.content.import(&bytes).map_err(|_| invalid())?,
            None => merged = Some(System { document, content }),
        }
    }
    Ok(merged)
}

/// The read list is also the acknowledgement set. Only events absent from
/// that durable set are sent; a document-level acknowledgement loses races.
pub fn pending<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    incarnation: Uuid,
) -> Result<Vec<Event>, Failure> {
    let Some(system) = load(store, workspace)? else {
        return Ok(Vec::new());
    };
    let mut pending = Vec::new();
    for event in system.content.purge_events().map_err(|_| invalid())? {
        if !store.document_removal_received(
            workspace,
            incarnation,
            system.document,
            event.event_id,
        )? {
            if !transfer::removal::fits(&event) {
                return Err(invalid());
            }
            pending.push(event);
        }
    }
    Ok(pending)
}

pub fn cursor<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    incarnation: Uuid,
) -> Result<u64, Failure> {
    let Some(system) = load(store, workspace)? else {
        return Ok(0);
    };
    Ok(store.document_removal_cursor(workspace, incarnation, system.document)?)
}

pub fn consume<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    scope: Scope,
    entries: &[transfer::removal::Entry],
    cursor: u64,
    at: Timestamp,
) -> Result<(), Failure> {
    let koru_device::cloud::CloudReplicaPrincipalV1::Workspace {
        workspace_id,
        incarnation_id,
        ..
    } = scope.principal
    else {
        return Err(invalid());
    };
    let workspace = scope.eligibility_workspace;
    let incarnation = Uuid::from_bytes(*incarnation_id.as_bytes());
    if workspace_id.as_bytes() != workspace.as_uuid().as_bytes()
        || incarnation.is_nil()
        || entries.len() > 64
    {
        return Err(invalid());
    }
    // Reuse the complete carrier validation even for direct Store callers:
    // identity equality, ordered sequences, exact cursor, payload fields and
    // the 4096-byte bound are checked before any persistent mutation.
    transfer::encode_record(&Record::Control(transfer::removal::page(
        entries.to_vec(),
        cursor,
    )))?;
    if entries
        .iter()
        .any(|entry| !transfer::removal::fits(&entry.event))
    {
        return Err(invalid());
    }
    if store.manifest_id(workspace)? != scope.manifest {
        return Err(invalid());
    }
    let protected = system_ids(store, workspace)?;
    if entries.iter().any(|entry| {
        entry.document_id == scope.manifest.as_uuid()
            || protected.iter().any(|id| id.as_uuid() == entry.document_id)
    }) {
        return Err(invalid());
    }
    let mut initial = None;
    let mut system = match load(store, workspace)? {
        Some(system) => system,
        None if entries.is_empty() => return Ok(()),
        None => {
            let content = SystemDoc::create().map_err(|_| invalid())?;
            initial = Some(content.export_snapshot().map_err(|_| invalid())?);
            System {
                document: DocumentId::new(),
                content,
            }
        }
    };
    let mut events = Vec::new();
    for entry in entries {
        if entry.document_id == system.document.as_uuid() {
            return Err(invalid());
        }
        match system
            .content
            .write_purge_event(&entry.event)
            .map_err(|_| invalid())?
        {
            Insert::Conflict => return Err(invalid()),
            Insert::Inserted | Insert::Present => {}
        }
        events.push(entry.event.event_id);
    }
    // A first page is validated and merged in memory before even creating its
    // system row. Conflicting same-id payloads cannot leave a partial bootstrap.
    if let Some(initial) = initial {
        store.import_document_at(
            workspace,
            system.document,
            "",
            koru_core::explore::KIND_SYSTEM,
            None,
            None,
            initial,
            at,
        )?;
    }
    // load may have merged independently created SystemDocs. Persist that union
    // even for an empty page before binding any cursor to the canonical id.
    let update = system.content.export_all_updates().map_err(|_| invalid())?;
    store.apply_update(
        workspace,
        system.document,
        update,
        at,
        scope.principal.update_origin(),
    )?;
    store.commit_document_removals(workspace, incarnation, system.document, &events, cursor, at)?;
    Ok(())
}
