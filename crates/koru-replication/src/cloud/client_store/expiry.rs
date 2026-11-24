// SPDX-License-Identifier: MPL-2.0
//! **Both trashes empty themselves after 30 days**
//! (docs/cloud-platform/koru-free/COLLECTOR.final.md, rung 2): trashed
//! documents in the Workspace listing, and rows in each document's own bin.
//! The rule — 30 days promised, 31 waited, and whose clock decides — is
//! [`koru_core::trash_expiry`]; this applies it to one Workspace's Store state.
//!
//! Synchronous Store work, run by the Store owner on both hosts (the desktop
//! Store process, Android's embedded Store) at start and daily. Every removal
//! is an ordinary durable edit, so other devices converge on it and the
//! publication that carries it is what proves a file unused in the Cloud.
use super::*;
use koru_core::{WorkspaceDoc, trash_expiry::expired};

/// What one sweep removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Swept {
    /// Documents purged from the Workspace trash, children included.
    pub documents: usize,
    /// Rows deleted from documents' own bins.
    pub rows: usize,
    /// Documents rewritten to delete those rows.
    pub rewritten: usize,
}

/// Sweep one Workspace by `trusted_now_ms`: the Cloud-vouched time for a Cloud
/// Workspace, the device clock for a local-only one (see
/// [`koru_core::trash_expiry`]). `peer` is the Loro peer this process edits
/// bins with; `at` stamps the Store's own records and decides nothing.
pub fn sweep<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    trusted_now_ms: i64,
    peer: u64,
    at: Timestamp,
) -> Result<Swept, Failure> {
    let documents = sweep_documents(store, workspace, trusted_now_ms, at)?;
    let (rows, rewritten) = sweep_bins(store, workspace, trusted_now_ms, peer, at)?;
    Ok(Swept {
        documents,
        rows,
        rewritten,
    })
}

/// Purge every listed document whose own trash stamp is due, with its whole
/// subtree — notes, OCR readings, and children trashed with it by inheritance —
/// deepest first, because releasing a parent detaches its children rather than
/// losing them.
fn sweep_documents<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    trusted_now_ms: i64,
    at: Timestamp,
) -> Result<usize, Failure> {
    let entries = store.documents(workspace)?;
    let due: Vec<DocumentId> = entries
        .iter()
        .filter(|entry| koru_core::explore::listed(&entry.kind))
        .filter(|entry| {
            entry
                .trashed_at
                .is_some_and(|stamp| expired(stamp.millis(), trusted_now_ms))
        })
        .map(|entry| entry.id)
        .collect();
    let mut purged = 0;
    for root in due {
        // A due document inside another due subtree went with it.
        let entries = store.documents(workspace)?;
        if !entries.iter().any(|entry| entry.id == root) {
            continue;
        }
        for document in deepest_first(&entries, root) {
            removal::purge(store, workspace, document, root, at)?;
            purged += 1;
        }
    }
    Ok(purged)
}

/// `root` and everything under it in the listing, children before parents.
fn deepest_first(
    entries: &[koru_store_core::model::DocumentEntry],
    root: DocumentId,
) -> Vec<DocumentId> {
    let mut order = Vec::new();
    let mut stack = vec![(root, false)];
    while let Some((document, expanded)) = stack.pop() {
        if expanded {
            order.push(document);
            continue;
        }
        stack.push((document, true));
        for child in entries
            .iter()
            .filter(|entry| entry.parent == Some(document))
        {
            // A malformed cycle cannot loop forever: each id enters once.
            if child.id != root
                && !order.contains(&child.id)
                && !stack.iter().any(|(id, _)| *id == child.id)
            {
                stack.push((child.id, false));
            }
        }
    }
    order
}

/// Delete each listed document's bin rows whose stamp is due. A document is
/// read only to look, and rewritten only when it has such rows. A row with no
/// stamp — binned by a build that did not record when — stays: its age cannot
/// be known.
fn sweep_bins<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    trusted_now_ms: i64,
    peer: u64,
    at: Timestamp,
) -> Result<(usize, usize), Failure> {
    let documents: Vec<DocumentId> = store
        .documents(workspace)?
        .into_iter()
        .filter(|entry| koru_core::explore::listed(&entry.kind))
        .map(|entry| entry.id)
        .collect();
    let (mut rows, mut rewritten) = (0, 0);
    for document in documents {
        let fork = match store.fork_document(workspace, document) {
            Ok(fork) => fork,
            // Listed before its body arrived: nothing to sweep yet.
            Err(error) if error.code() == Code::DocumentNotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let Ok(mut model) = WorkspaceDoc::adopt(fork) else {
            // Not a node document (a file's bytes, a newer schema): not ours
            // to rewrite.
            continue;
        };
        let Ok(binned) = model.trashed() else {
            continue;
        };
        let due: Vec<_> = binned
            .into_iter()
            .filter(|row| {
                row.trashed_at
                    .is_some_and(|stamp| expired(stamp, trusted_now_ms))
            })
            .map(|row| row.node)
            .collect();
        if due.is_empty() {
            continue;
        }
        model.doc().set_peer_id(peer).map_err(|_| Error::Invalid)?;
        let before = model.version();
        for node in &due {
            model
                .delete_permanently(*node)
                .map_err(|_| Error::Invalid)?;
        }
        let update = model
            .export_updates_since(&before)
            .map_err(|_| Error::Invalid)?;
        store.apply_update(
            workspace,
            document,
            update,
            at,
            koru_store_core::ports::UpdateOrigin::Store,
        )?;
        store.flush(workspace, document, at)?;
        rows += due.len();
        rewritten += 1;
    }
    Ok((rows, rewritten))
}
