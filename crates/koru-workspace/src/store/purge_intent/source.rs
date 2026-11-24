// SPDX-License-Identifier: MPL-2.0

//! Read coverage before constructing an immutable event. An unopened tab is
//! not missing data: the Store and durable local outbox are still sources.

use super::*;
use crate::{
    explore,
    store::{outbox, revisions},
};
use koru_core::{DocumentId, WorkspaceId, schema::WorkspaceDoc};

pub(crate) fn prepare(
    workspace: WorkspaceId,
    root: DocumentId,
    actor: &str,
    sources: Vec<(DocumentId, Option<loro::VersionVector>)>,
    store_root: Option<&Path>,
    mut snapshot: impl FnMut(DocumentId) -> Result<Option<Vec<u8>>, String>,
) -> Result<Vec<Event>, String> {
    sources
        .into_iter()
        .map(|(id, captured)| {
            // The UI only supplies an open VV after Store acknowledgement or a
            // durable page revision covers it. Never substitute an older snapshot.
            let version = match captured {
                Some(version) => Some(version),
                None => closed_version(workspace, id, store_root, snapshot(id)?)?,
            };
            koru_replication::cloud::transfer::removal::bounded_event(Event::new(
                id.as_uuid(),
                root.as_uuid(),
                version.as_ref(),
                actor,
            ))
            .map_err(|error| error.code().to_owned())
        })
        .collect()
}

fn closed_version(
    workspace: WorkspaceId,
    document: DocumentId,
    store_root: Option<&Path>,
    snapshot: Option<Vec<u8>>,
) -> Result<Option<loro::VersionVector>, String> {
    let mut doc = snapshot
        .as_deref()
        .map(WorkspaceDoc::from_snapshot)
        .transpose()
        .map_err(|error| error.to_string())?;
    let (buffers, errors) = outbox::read_all(
        store_root,
        explore::store_workspace(workspace),
        explore::store_document(document),
    );
    if !errors.is_empty() {
        return Err("cannot read local document outbox before final purge".into());
    }
    for buffer in buffers {
        if doc.is_none() {
            doc = Some(
                WorkspaceDoc::from_snapshot(&buffer.updates).map_err(|error| error.to_string())?,
            );
        }
        let source = doc.as_mut().expect("source initialized");
        if !buffer.applies_to(&source.version()) {
            return Err("document outbox has missing causal ancestors".into());
        }
        source
            .import(&buffer.updates)
            .map_err(|error| error.to_string())?;
    }
    if let Some(dir) = outbox::dir(
        store_root,
        explore::store_workspace(workspace),
        explore::store_document(document),
    ) {
        let found =
            revisions::discover(&revisions::DiskFs, &dir).map_err(|error| error.to_string())?;
        if !found.damaged.is_empty() || !found.incomplete.is_empty() || !found.parts.is_empty() {
            return Err("incomplete local document revision before final purge".into());
        }
        for revision in found.complete {
            let page = koru_core::ids::PageKey::parse(&revision.record.page)
                .ok_or("invalid revision page")?;
            if revision.record.workspace != workspace.to_storage()
                || revision.record.document != document.to_storage()
                || koru_core::ids::page_document_id(workspace, page, revision.record.generation)
                    != document
            {
                return Err("revision identity mismatch".into());
            }
            if doc.is_none() {
                doc = Some(
                    WorkspaceDoc::page_genesis(workspace, document)
                        .map_err(|error| error.to_string())?,
                );
            }
            let source = doc.as_mut().expect("source initialized");
            let bytes = fs::read(revision.dir.join(revisions::UPDATES))
                .map_err(|error| error.to_string())?;
            source.import(&bytes).map_err(|error| error.to_string())?;
            let version = loro::VersionVector::decode(&revision.record.version)
                .map_err(|error| error.to_string())?;
            if !source.version().includes_vv(&version) {
                return Err("revision has missing causal ancestors".into());
            }
        }
    }
    Ok(doc.map(|doc| doc.version()))
}

#[cfg(test)]
mod tests;
