// SPDX-License-Identifier: MPL-2.0

//! **The mirror's half: reading documents in, rows out, search told.**
//!
//! A reading document is never projected (TECHNICAL D14), so nothing in the
//! projector would ever tell the index that the words of a file arrived. This
//! is what does (TECHNICAL §7 "Never walked, never versioned, and still
//! heard"):
//!
//! 1. A commit to a reading document reaches the [`Inbox`] through the tee in
//!    `StoreSource::take_committed`. The applier forks **that one document**
//!    under the Store lock — O(1), a structural share — reads its entries
//!    outside it, and upserts them into the mirror.
//! 2. It then tells the search queue: the parent entry by name, and every
//!    changed hash, which the indexer turns into the other documents carrying
//!    it with one read of the projection.
//! 3. **Catch-up** covers everything the tee can miss — a reading that arrived
//!    while this was not running, a mirror rebuilt after a schema change, a
//!    reading document whose parent was purged. At start, and whenever a
//!    workspace's listing moves, each listed reading document's stored
//!    revision is compared with what the mirror last applied, and the ones
//!    behind are applied. One that left the listing takes its rows.
//!
//! **The lock discipline is `systemdoc.rs`'s**: in, read or fork, out. No
//! database write of the mirror's own happens under the Store lock.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};

use koru_core::explore::KIND_READING;
use koru_projection::Projections;
use koru_read::ReadingDoc;
use koru_store_api::state::AppState;
use koru_store_core::errors::Code;
use koru_store_core::ids::{DocumentId, WorkspaceId};

use super::feed::Inbox;
use super::mirror::{Applied, Changed, Mirror};

/// What the mirror has done, for the debug route. Counts only.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct MirrorStats {
    pub applied: u64,
    pub removed: u64,
    pub catch_ups: u64,
    pub failed: u64,
    /// Reading documents the applier forked and found it could not read —
    /// news, said once per distinct cause.
    pub last_error: Option<String>,
}

/// One reading document the Store holds, as the catch-up sees it.
struct Listed {
    parent: Option<DocumentId>,
    revision: Option<Vec<u8>>,
}

pub struct Applier {
    state: Weak<AppState>,
    mirror: Mirror,
    projections: Option<Projections>,
    inbox: Arc<Inbox>,
    search: Option<koru_search::Queue>,
    pub stats: MirrorStats,
}

impl Applier {
    pub fn new(
        state: &Arc<AppState>,
        mirror: Mirror,
        projections: Option<Projections>,
        inbox: Arc<Inbox>,
        search: Option<koru_search::Queue>,
    ) -> Self {
        Self {
            state: Arc::downgrade(state),
            mirror,
            projections,
            inbox,
            search,
            stats: MirrorStats::default(),
        }
    }

    /// Everything waiting, in one go. Returns whether anything was waiting.
    pub fn step(&mut self) -> bool {
        let work = self.inbox.take_mirror();
        if work.is_empty() {
            return false;
        }
        let mut catch_up: HashSet<WorkspaceId> = work.catch_up.into_iter().collect();
        if work.catch_up_all {
            catch_up.extend(self.every_workspace());
        }
        for (workspace, document) in work.documents {
            if catch_up.contains(&workspace) {
                // The catch-up below reads this document's revision anyway.
                continue;
            }
            match self.mirror.source_of(document) {
                Ok(Some((known, parent))) if known == workspace => {
                    self.apply(workspace, document, parent);
                }
                // **First sight, or a document that moved.** Its parent is
                // what the listing says, and the catch-up reads the listing.
                Ok(_) => {
                    catch_up.insert(workspace);
                }
                Err(error) => self.failed(error.0),
            }
        }
        let mut catch_up: Vec<WorkspaceId> = catch_up.into_iter().collect();
        catch_up.sort();
        for workspace in catch_up {
            self.catch_up(workspace);
        }
        true
    }

    /// Every workspace that holds a reading document as far as either side
    /// knows: the mirror's own sources, and the projection's listings.
    fn every_workspace(&self) -> Vec<WorkspaceId> {
        let mut out: HashSet<WorkspaceId> = self
            .mirror
            .source_workspaces()
            .unwrap_or_default()
            .into_iter()
            .collect();
        if let Some(projections) = self.projections.as_ref()
            && let Ok(listings) = projections.listings()
        {
            for (workspace, _) in listings {
                if projections
                    .listed_of_kinds(workspace, &[KIND_READING])
                    .is_ok_and(|found| !found.is_empty())
                {
                    out.insert(workspace);
                }
            }
        }
        let mut out: Vec<WorkspaceId> = out.into_iter().collect();
        out.sort();
        out
    }

    /// Compare one workspace's reading documents with what the mirror last
    /// applied, apply the ones behind, and drop the ones that left.
    pub fn catch_up(&mut self, workspace: WorkspaceId) {
        // **A workspace with no reading document costs no Store read.** The
        // projection's listing is refreshed before it announces a change, so
        // it already names a reading document registered by the commit that
        // caused this; the mirror names every one it ever applied.
        let mirrored = self
            .mirror
            .sources_in(workspace)
            .is_ok_and(|sources| !sources.is_empty());
        let listed = self.projections.as_ref().is_none_or(|projections| {
            projections
                .listed_of_kinds(workspace, &[KIND_READING])
                .map_or(true, |found| !found.is_empty())
        });
        if !mirrored && !listed {
            return;
        }
        self.stats.catch_ups += 1;
        let listed = match self.listed(workspace) {
            Ok(listed) => listed,
            // A workspace that is gone takes every reading document in it.
            Err(Code::WorkspaceNotFound) => HashMap::new(),
            Err(code) => {
                self.failed(format!("catch-up could not list a workspace: {code:?}"));
                return;
            }
        };
        let applied = match self.mirror.sources_in(workspace) {
            Ok(applied) => applied,
            Err(error) => return self.failed(error.0),
        };
        let mut documents: Vec<(&DocumentId, &Listed)> = listed.iter().collect();
        documents.sort_by_key(|(document, _)| **document);
        for (document, listed) in documents {
            let Some(parent) = listed.parent else {
                continue;
            };
            let current = match (applied.get(document), listed.revision.as_ref()) {
                (Some((known_parent, theirs)), Some(ours)) => {
                    *known_parent == parent && same_revision(theirs, ours)
                }
                _ => false,
            };
            if !current {
                self.apply(workspace, *document, parent);
            }
        }
        let mut gone: Vec<DocumentId> = applied
            .keys()
            .filter(|document| !listed.contains_key(document))
            .copied()
            .collect();
        gone.sort();
        for document in gone {
            match self.mirror.remove_document(document) {
                Ok(Some(changed)) => {
                    self.stats.removed += 1;
                    self.tell_search(&changed, false);
                }
                Ok(None) => {}
                Err(error) => self.failed(error.0),
            }
        }
    }

    /// The reading documents the Store lists in a workspace, with their
    /// journal positions — one hold of the Store lock, opening no document.
    fn listed(&self, workspace: WorkspaceId) -> Result<HashMap<DocumentId, Listed>, Code> {
        let state = self.state.upgrade().ok_or(Code::Internal)?;
        let mut store = state.store.lock().map_err(|_| Code::Internal)?;
        let entries = store.documents(workspace).map_err(|e| e.code())?;
        let positions: HashMap<DocumentId, Vec<u8>> = store
            .document_positions(workspace)
            .map_err(|e| e.code())?
            .into_iter()
            .collect();
        drop(store);
        Ok(entries
            .into_iter()
            .filter(|entry| entry.kind == KIND_READING)
            .map(|entry| {
                (
                    entry.id,
                    Listed {
                        parent: entry.parent,
                        revision: positions.get(&entry.id).cloned(),
                    },
                )
            })
            .collect())
    }

    /// Fork one reading document, read its entries, and upsert them.
    pub fn apply(&mut self, workspace: WorkspaceId, document: DocumentId, parent: DocumentId) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        // In, fork, out. Everything after this runs without the Store lock.
        let fork = match state.store.lock() {
            Ok(mut store) => store.fork_document(workspace, document),
            Err(_) => return self.failed("the store lock is poisoned".into()),
        };
        drop(state);
        let fork = match fork {
            Ok(fork) => fork,
            // Registered and not yet written anywhere this device can see:
            // nothing to mirror until its first commit, which the tee brings.
            Err(error) if error.code() == Code::DocumentNotFound => return,
            Err(error) => {
                return self.failed(format!(
                    "a reading document did not fork: {:?}",
                    error.code()
                ));
            }
        };
        let reading = ReadingDoc::from_doc(fork);
        let applied = Applied {
            workspace,
            parent,
            document,
            revision: reading.version().encode(),
            entries: reading.readings(),
        };
        match self.mirror.apply(&applied) {
            Ok(changed) => {
                self.stats.applied += 1;
                self.tell_search(&changed, true);
                if !changed.hashes.is_empty() {
                    // A reading this device was about to make may have just
                    // arrived from another one.
                    self.inbox.queue_dirty();
                }
            }
            Err(error) => self.failed(error.0),
        }
    }

    /// **Wake search**: the parent by name, and every changed hash, so the
    /// other documents carrying the same picture are re-indexed too.
    fn tell_search(&self, changed: &Changed, parent_lives: bool) {
        if changed.hashes.is_empty() {
            return;
        }
        let Some(queue) = self.search.as_ref() else {
            return;
        };
        if parent_lives {
            queue.enqueue_document(changed.workspace, changed.parent);
        }
        queue.enqueue_hashes(changed.workspace, changed.hashes.iter().cloned());
    }

    fn failed(&mut self, why: String) {
        self.stats.failed += 1;
        if self.stats.last_error.as_deref() != Some(why.as_str()) {
            // Unconditional: a reading document that could not be applied is
            // news about somebody's data (TECHNICAL §14). Ids and codes only.
            tracing::warn!(target: "koru_readings", "readings mirror: {why}");
            self.stats.last_error = Some(why);
        }
    }

    pub fn mirror(&self) -> &Mirror {
        &self.mirror
    }
}

/// Two encoded version vectors name the same state. Decoded, because an
/// encoding has no canonical peer order (`projection.rs` says the same of
/// automatic history).
fn same_revision(left: &[u8], right: &[u8]) -> bool {
    if left == right {
        return true;
    }
    match (
        loro::VersionVector::decode(left),
        loro::VersionVector::decode(right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
mod tests;
