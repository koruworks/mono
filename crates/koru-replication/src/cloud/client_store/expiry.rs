// SPDX-License-Identifier: MPL-2.0
//! **Both trashes empty themselves after 30 days**
//! (docs/cloud-platform/koru-free/COLLECTOR.final.md, rung 2): trashed
//! documents in the Workspace listing, and rows in each document's own bin.
//! With them go the retained copies of deleted task-list notes
//! ([`due_notes`]) and the OCR text of pictures a page no longer holds
//! anywhere, its bin included ([`forget_readings`]).
//! The rule — 30 days promised, 31 waited, and whose clock decides — is
//! [`koru_core::trash_expiry`]; this applies it to one Workspace's Store state.
//!
//! Synchronous Store work, run by the Store owner on both hosts (the desktop
//! Store process, Android's embedded Store) at start and daily. Every removal
//! is an ordinary durable edit, so other devices converge on it and the
//! publication that carries it is what proves a file unused in the Cloud.
//!
//! **Every step hands back what it wrote** ([`Wrote`]). The Store applies a
//! Store-origin update without telling anyone, so the host passes each one on:
//! to the views that have that document or the Workspace listing open, and to
//! the replication that carries it to peers and the Cloud. Without that, an
//! open window keeps showing what the sweep removed until it reopens.
use super::*;
use koru_core::ids::PageKey;
use koru_core::pages::deletion::Resolution;
use koru_core::schema::file_references::{ReferenceBudget, inspect_file_references};
use koru_core::{WorkspaceDoc, trash_expiry::expired};
use koru_store_core::ids::reading_document_id;
use std::collections::{HashMap, HashSet};

/// What one sweep removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Swept {
    /// Documents purged from the Workspace trash, children included.
    pub documents: usize,
    /// Retained copies of deleted task-list notes purged, children included.
    pub notes: usize,
    /// Rows deleted from documents' own bins.
    pub rows: usize,
    /// Documents rewritten to delete those rows.
    pub rewritten: usize,
    /// OCR readings of pictures their page no longer holds.
    pub readings: usize,
}

/// One edit a step made through the Store, for its host to pass on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wrote {
    /// `update` was applied to `document`: a bin, a reading document, or the
    /// system document that records a purge.
    Document {
        document: DocumentId,
        update: Vec<u8>,
    },
    /// `update` was applied to `workspace`'s listing: a purge released rows.
    Listing {
        workspace: WorkspaceId,
        update: Vec<u8>,
    },
}

/// Sweep one Workspace by `trusted_now_ms`: the Cloud-vouched time for a Cloud
/// Workspace, the device clock for a local-only one (see
/// [`koru_core::trash_expiry`]). `peer` is the Loro peer this process edits
/// bins with; `at` stamps the Store's own records and decides nothing.
///
/// All of it in one call, for a caller that may hold the Store that long.
/// A Store owner that also serves people runs the steps one at a time instead
/// ([`due_documents`], [`due_notes`], [`purge_subtree`], [`bin_candidates`],
/// [`sweep_bin`], [`reading_candidates`], [`forget_readings`]), so a save
/// never waits behind
/// a whole Workspace. `tell` receives every edit, in the order made.
pub fn sweep<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    trusted_now_ms: i64,
    peer: u64,
    at: Timestamp,
    mut tell: impl FnMut(Wrote),
) -> Result<Swept, Failure> {
    let mut swept = Swept::default();
    for root in due_documents(store, workspace, trusted_now_ms)? {
        let (documents, wrote) = purge_subtree(store, workspace, root, at)?;
        swept.documents += documents;
        wrote.into_iter().for_each(&mut tell);
    }
    for root in due_notes(store, workspace, trusted_now_ms)? {
        let (notes, wrote) = purge_subtree(store, workspace, root, at)?;
        swept.notes += notes;
        wrote.into_iter().for_each(&mut tell);
    }
    for document in bin_candidates(store, workspace)? {
        let (rows, wrote) = sweep_bin(store, workspace, document, trusted_now_ms, peer, at)?;
        swept.rows += rows;
        swept.rewritten += usize::from(rows > 0);
        wrote.into_iter().for_each(&mut tell);
    }
    for page in reading_candidates(store, workspace)? {
        let (readings, wrote) = forget_readings(store, workspace, page, peer, at)?;
        swept.readings += readings;
        wrote.into_iter().for_each(&mut tell);
    }
    Ok(swept)
}

/// Listed documents whose own trash stamp is due by `trusted_now_ms`.
pub fn due_documents<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    trusted_now_ms: i64,
) -> Result<Vec<DocumentId>, Failure> {
    Ok(store
        .documents(workspace)?
        .into_iter()
        .filter(|entry| koru_core::explore::listed(&entry.kind))
        .filter(|entry| {
            entry
                .trashed_at
                .is_some_and(|stamp| expired(stamp.millis(), trusted_now_ms))
        })
        .map(|entry| entry.id)
        .collect())
}

/// The retained copies of deleted task-list notes that are due by
/// `trusted_now_ms` (COLLECTOR.final rung 2, decision 1).
///
/// Stable v0.1.6–v0.1.9 deleted a note by writing a deletion event to the
/// system document and keeping the note's document, recoverable without a
/// time limit; nothing shows that copy except the *Keep deleted* / *Recover*
/// prompt after a conflicting edit. A copy is due when its generation is
/// plainly deleted and the newest event still in force is due — a Recover
/// ends an event, and a later deletion restarts the 30 days.
///
/// **A conflict is never due.** Someone wrote the note after it was deleted
/// and the person is being asked which wins; purging would answer for them.
/// A copy in the Workspace trash is left to its own trash stamp, which is
/// what its Trash row promises.
pub fn due_notes<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    trusted_now_ms: i64,
) -> Result<Vec<DocumentId>, Failure> {
    let Some(system) = removal::load(store, workspace)? else {
        return Ok(Vec::new());
    };
    let entries = store.documents(workspace)?;
    let parents: HashMap<_, _> = entries
        .iter()
        .map(|entry| (entry.id, (entry.parent, entry.trashed_at.is_some())))
        .collect();
    let trashed = |mut document: DocumentId| {
        // Bounded: a malformed cycle cannot loop forever.
        for _ in 0..=parents.len() {
            match parents.get(&document) {
                Some((_, true)) => return true,
                Some((Some(parent), false)) => document = *parent,
                _ => return false,
            }
        }
        false
    };
    let owner = koru_core::WorkspaceId::from_uuid(workspace.as_uuid());
    let mut pages: Vec<PageKey> = system
        .content
        .all_lists()
        .into_iter()
        .map(|list| PageKey::List(list.id))
        .collect();
    pages.extend(
        koru_core::tasks::Bucket::ALL
            .into_iter()
            .map(PageKey::Bucket),
    );
    let mut due = Vec::new();
    for page in pages {
        for generation in system.content.page_generations(page) {
            let record = system.content.deletion_record(page, generation);
            if record.resolve() != Resolution::Deleted {
                continue;
            }
            let newest = record
                .events_in_force()
                .into_iter()
                .filter_map(|event| record.deletion(event).map(|deletion| deletion.deleted_ms))
                .max();
            if !newest.is_some_and(|deleted_ms| expired(deleted_ms, trusted_now_ms)) {
                continue;
            }
            let document = DocumentId::from_uuid(
                koru_core::ids::page_document_id(owner, page, generation).as_uuid(),
            );
            if parents.contains_key(&document) && !trashed(document) {
                due.push(document);
            }
        }
    }
    Ok(due)
}

/// Purge `root` with its whole subtree — notes, OCR readings, and children
/// trashed with it by inheritance — deepest first, because releasing a parent
/// detaches its children rather than losing them. Returns how many documents
/// went (0 when `root` is already gone: it went with a due ancestor), and what
/// the purges wrote to the system documents and the listing.
pub fn purge_subtree<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    root: DocumentId,
    at: Timestamp,
) -> Result<(usize, Vec<Wrote>), Failure> {
    let entries = store.documents(workspace)?;
    if !entries.iter().any(|entry| entry.id == root) {
        return Ok((0, Vec::new()));
    }
    let manifest = store.manifest_id(workspace)?;
    let listing = store.version(workspace, manifest)?;
    // The system documents a window may have open. One the first purge
    // creates is announced by its row in the listing instead.
    let systems: Vec<_> = entries
        .iter()
        .filter(|entry| entry.kind == koru_core::explore::KIND_SYSTEM)
        .filter_map(|entry| Some((entry.id, store.version(workspace, entry.id).ok()?)))
        .collect();
    let order = deepest_first(&entries, root);
    for document in &order {
        removal::purge(store, workspace, *document, root, at)?;
    }
    let mut wrote = Vec::new();
    for (system, before) in systems {
        if let Some(update) = written_since(store, workspace, system, &before)? {
            wrote.push(Wrote::Document {
                document: system,
                update,
            });
        }
    }
    if let Some(update) = written_since(store, workspace, manifest, &listing)? {
        wrote.push(Wrote::Listing { workspace, update });
    }
    Ok((order.len(), wrote))
}

/// What `document` holds beyond `before`, or `None` when nothing was written.
fn written_since<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    document: DocumentId,
    before: &loro::VersionVector,
) -> Result<Option<Vec<u8>>, Failure> {
    if store.version(workspace, document)? == *before {
        return Ok(None);
    }
    Ok(Some(store.updates_since(workspace, document, before)?))
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

/// The documents whose bins a sweep looks in: every listed one.
pub fn bin_candidates<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
) -> Result<Vec<DocumentId>, Failure> {
    Ok(store
        .documents(workspace)?
        .into_iter()
        .filter(|entry| koru_core::explore::listed(&entry.kind))
        .map(|entry| entry.id)
        .collect())
}

/// Delete `document`'s bin rows whose stamp is due; returns how many, and the
/// update that deleted them. The document is read only to look, and rewritten
/// only when it has such rows. A row with no stamp — binned by a build that
/// did not record when — stays: its age cannot be known.
pub fn sweep_bin<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    document: DocumentId,
    trusted_now_ms: i64,
    peer: u64,
    at: Timestamp,
) -> Result<(usize, Option<Wrote>), Failure> {
    let fork = match store.fork_document(workspace, document) {
        Ok(fork) => fork,
        // Listed before its body arrived, or purged since it was listed.
        Err(error) if error.code() == Code::DocumentNotFound => return Ok((0, None)),
        Err(error) => return Err(error.into()),
    };
    let Ok(mut model) = WorkspaceDoc::adopt(fork) else {
        // Not a node document (a file's bytes, a newer schema): not ours to
        // rewrite.
        return Ok((0, None));
    };
    let Ok(binned) = model.trashed() else {
        return Ok((0, None));
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
        return Ok((0, None));
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
    apply(store, workspace, document, &update, at)?;
    Ok((due.len(), Some(Wrote::Document { document, update })))
}

/// The pages whose reading document is listed: the only ones that can hold
/// the OCR text of a picture they no longer have.
pub fn reading_candidates<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
) -> Result<Vec<DocumentId>, Failure> {
    let entries = store.documents(workspace)?;
    let listed: HashSet<_> = entries.iter().map(|entry| entry.id).collect();
    Ok(entries
        .iter()
        .filter(|entry| entry.kind == koru_core::explore::KIND_DOCUMENT)
        .filter(|entry| listed.contains(&reading_document_id(entry.id)))
        .map(|entry| entry.id)
        .collect())
}

/// Remove from `page`'s reading document the OCR text of every picture the
/// page no longer holds anywhere, its bin included (COLLECTOR.final rung 2),
/// so the words of a deleted picture are not kept. Returns how many readings
/// went, and the update that removed them. Not tied to the 30 days: a picture
/// deleted outright has no bin row to wait for.
///
/// What the page holds is the strict walk the storage count trusts
/// ([`inspect_file_references`]): every raw image slot, bin and retained
/// slots included. A page it cannot prove — a newer schema, a damaged shape,
/// over its budget — keeps every reading, because "not found" there means
/// "not known", not "gone". A picture restored or pasted again afterwards is
/// read again or, for the same bytes, finds its words where they were
/// ([`koru_read::ReadingDoc::forget`]).
pub fn forget_readings<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    page: DocumentId,
    peer: u64,
    at: Timestamp,
) -> Result<(usize, Option<Wrote>), Failure> {
    let body = match store.fork_document(workspace, page) {
        Ok(body) => body,
        Err(error) if error.code() == Code::DocumentNotFound => return Ok((0, None)),
        Err(error) => return Err(error.into()),
    };
    let Ok(held) = inspect_file_references(&body, ReferenceBudget::default()) else {
        return Ok((0, None));
    };
    let held: HashSet<&str> = held.blobs().iter().map(String::as_str).collect();
    let document = reading_document_id(page);
    let readings = match store.fork_document(workspace, document) {
        Ok(readings) => koru_read::ReadingDoc::from_doc(readings),
        Err(error) if error.code() == Code::DocumentNotFound => return Ok((0, None)),
        Err(error) => return Err(error.into()),
    };
    let stale: Vec<String> = readings
        .hashes()
        .into_iter()
        .filter(|hash| !held.contains(hash.as_str()))
        .collect();
    if stale.is_empty() {
        return Ok((0, None));
    }
    readings
        .doc()
        .set_peer_id(peer)
        .map_err(|_| Error::Invalid)?;
    let before = readings.version();
    for hash in &stale {
        readings.forget(hash).map_err(|_| Error::Invalid)?;
    }
    let update = readings
        .export_updates_since(&before)
        .map_err(|_| Error::Invalid)?;
    apply(store, workspace, document, &update, at)?;
    Ok((stale.len(), Some(Wrote::Document { document, update })))
}

/// One Store-origin edit, flushed like any commit.
fn apply<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    document: DocumentId,
    update: &[u8],
    at: Timestamp,
) -> Result<(), Failure> {
    store.apply_update(
        workspace,
        document,
        update.to_vec(),
        at,
        koru_store_core::ports::UpdateOrigin::Store,
    )?;
    store.flush(workspace, document, at)?;
    Ok(())
}
