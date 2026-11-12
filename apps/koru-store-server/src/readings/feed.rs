// SPDX-License-Identifier: MPL-2.0

//! **How the mirror and the coordinator are told** — never by a timer.
//!
//! Three things happen on other threads that these two must hear about, and
//! each is turned into a few ids pushed onto one [`Inbox`] under a mutex held
//! for a push:
//!
//! - **A commit to a reading document** (the tee in
//!   `StoreSource::take_committed`). The projector drops such a commit before
//!   any consumer hears of it (TECHNICAL §7 "Never walked, never versioned,
//!   and still heard"), so the daemon carries it to the mirror itself.
//! - **Bytes landing** (`Store::take_blobs_stored`, drained on the same tick):
//!   a file's entry can arrive before its bytes, and the bytes arriving is
//!   what makes it readable (ROADMAP §3, the second trigger).
//! - **What the projector tells its consumers** ([`ReadingsFeed`]): a document
//!   that references a blob was projected (the first trigger), and a
//!   workspace's listing moved (the mirror's catch-up, and the coordinator's
//!   whole queue, are owed).
//!
//! Neither consumer of the inbox does any work on the thread that pushed.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use koru_projection::Consumer;
use koru_projection::model::DocumentProjection;
use koru_store_core::BlobStored;
use koru_store_core::ids::{DocumentId, WorkspaceId};

/// More fresh hints than this and the coordinator recomputes its whole queue
/// instead: a projection rebuild projects every document at once, and the
/// queue query is the cheaper way to hear about all of them.
const HINT_CEILING: usize = 4_096;

/// What is waiting for the mirror and the coordinator. Shared, in memory, and
/// never on disk: everything here is a trigger the two can recompute from the
/// Store, the projection and the mirror after a restart.
#[derive(Default)]
pub struct Inbox {
    state: Mutex<State>,
    mirror: Condvar,
    coordinator: Condvar,
}

#[derive(Default)]
pub(crate) struct State {
    // ---- the mirror's
    /// Reading documents committed and not yet applied.
    pub documents: BTreeSet<(WorkspaceId, DocumentId)>,
    /// Workspaces owed a catch-up.
    pub catch_up: BTreeSet<WorkspaceId>,
    /// The catch-up of every workspace the mirror or the projection names,
    /// owed at start.
    pub catch_up_all: bool,
    // ---- the coordinator's
    /// Documents projected with a blob in them.
    pub hint_documents: BTreeSet<(WorkspaceId, DocumentId)>,
    /// Bytes that landed, per workspace.
    pub hint_hashes: BTreeMap<WorkspaceId, BTreeSet<String>>,
    /// The whole queue is owed a recompute.
    pub dirty: bool,
    /// Set once by a composition that is being torn down.
    pub stopping: bool,
}

/// What the mirror takes in one go.
#[derive(Default)]
pub(crate) struct MirrorWork {
    pub documents: Vec<(WorkspaceId, DocumentId)>,
    pub catch_up: Vec<WorkspaceId>,
    pub catch_up_all: bool,
}

impl MirrorWork {
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty() && self.catch_up.is_empty() && !self.catch_up_all
    }
}

/// What the coordinator takes in one go.
#[derive(Default)]
pub(crate) struct CoordinatorWork {
    pub documents: Vec<(WorkspaceId, DocumentId)>,
    pub hashes: Vec<(WorkspaceId, Vec<String>)>,
    pub dirty: bool,
}

#[cfg(test)]
impl CoordinatorWork {
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty() && self.hashes.is_empty() && !self.dirty
    }
}

impl Inbox {
    pub fn new() -> Arc<Inbox> {
        let inbox = Inbox::default();
        {
            let mut state = inbox.lock();
            // Both owe their first full pass: the mirror its catch-up, the
            // coordinator its queue. A restart recomputes, never replays.
            state.catch_up_all = true;
            state.dirty = true;
        }
        Arc::new(inbox)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    // -------------------------------------------------------------- pushes

    /// Commits to reading documents, from the tee.
    pub fn reading_committed(
        &self,
        documents: impl IntoIterator<Item = (WorkspaceId, DocumentId)>,
    ) {
        let mut state = self.lock();
        let before = state.documents.len();
        state.documents.extend(documents);
        if state.documents.len() != before {
            self.mirror.notify_all();
        }
    }

    /// Bytes that landed, from the tee.
    pub fn blobs_stored(&self, stored: &[BlobStored]) {
        if stored.is_empty() {
            return;
        }
        let mut state = self.lock();
        for blob in stored {
            state
                .hint_hashes
                .entry(blob.workspace)
                .or_default()
                .insert(blob.hash.as_str().to_owned());
        }
        Self::cap(&mut state);
        self.coordinator.notify_all();
    }

    /// A document with a blob in it was projected.
    pub fn document_projected(&self, workspace: WorkspaceId, document: DocumentId) {
        let mut state = self.lock();
        state.hint_documents.insert((workspace, document));
        Self::cap(&mut state);
        self.coordinator.notify_all();
    }

    /// A workspace's listing moved: a reading document may have arrived or
    /// left, and an entry may have been trashed, restored or purged.
    pub fn listing_changed(&self, workspace: WorkspaceId) {
        let mut state = self.lock();
        state.catch_up.insert(workspace);
        state.dirty = true;
        self.mirror.notify_all();
        self.coordinator.notify_all();
    }

    /// The mirror applied a reading the coordinator may have been waiting
    /// on — another device read a file this one was about to.
    pub fn queue_dirty(&self) {
        let mut state = self.lock();
        state.dirty = true;
        self.coordinator.notify_all();
    }

    /// A composition is being torn down: wake both so they see it.
    pub fn stop(&self) {
        self.lock().stopping = true;
        self.mirror.notify_all();
        self.coordinator.notify_all();
    }

    fn cap(state: &mut State) {
        let hints = state.hint_documents.len()
            + state.hint_hashes.values().map(BTreeSet::len).sum::<usize>();
        if hints > HINT_CEILING {
            state.hint_documents.clear();
            state.hint_hashes.clear();
            state.dirty = true;
        }
    }

    // ---------------------------------------------------------------- takes

    pub(crate) fn take_mirror(&self) -> MirrorWork {
        let mut state = self.lock();
        MirrorWork {
            documents: std::mem::take(&mut state.documents).into_iter().collect(),
            catch_up: std::mem::take(&mut state.catch_up).into_iter().collect(),
            catch_up_all: std::mem::take(&mut state.catch_up_all),
        }
    }

    pub(crate) fn take_coordinator(&self) -> CoordinatorWork {
        let mut state = self.lock();
        CoordinatorWork {
            documents: std::mem::take(&mut state.hint_documents)
                .into_iter()
                .collect(),
            hashes: std::mem::take(&mut state.hint_hashes)
                .into_iter()
                .map(|(workspace, hashes)| (workspace, hashes.into_iter().collect()))
                .collect(),
            dirty: std::mem::take(&mut state.dirty),
        }
    }

    /// How much the mirror has not yet applied — "how far it is behind".
    pub fn mirror_backlog(&self) -> (usize, usize) {
        let state = self.lock();
        (state.documents.len(), state.catch_up.len())
    }

    pub fn is_stopping(&self) -> bool {
        self.lock().stopping
    }

    /// Wait for the mirror's next piece of work, at most `patience`. Returns
    /// whether there is some. The timeout exists only so a stop is seen; no
    /// work is ever started because time passed.
    pub fn wait_for_mirror(&self, patience: Duration) -> bool {
        let state = self.lock();
        let (state, _) = self
            .mirror
            .wait_timeout_while(state, patience, |s| {
                !s.stopping && s.documents.is_empty() && s.catch_up.is_empty() && !s.catch_up_all
            })
            .unwrap_or_else(|e| e.into_inner());
        !state.stopping
    }

    /// The coordinator's wait, on the same terms as [`Self::wait_for_mirror`].
    /// Returns whether a trigger arrived (as opposed to the patience running
    /// out or a stop).
    pub fn wait_for_coordinator(&self, patience: Duration) -> bool {
        let state = self.lock();
        let (state, timeout) = self
            .coordinator
            .wait_timeout_while(state, patience, |s| {
                !s.stopping && s.hint_documents.is_empty() && s.hint_hashes.is_empty() && !s.dirty
            })
            .unwrap_or_else(|e| e.into_inner());
        !state.stopping && !timeout.timed_out()
    }
}

/// The tee `StoreSource::take_committed` hands what it drained to.
#[derive(Clone)]
pub struct Tee(pub(crate) Arc<Inbox>);

impl Tee {
    pub fn reading_committed(&self, documents: Vec<(WorkspaceId, DocumentId)>) {
        if !documents.is_empty() {
            self.0.reading_committed(documents);
        }
    }

    pub fn blobs_stored(&self, stored: &[BlobStored]) {
        self.0.blobs_stored(stored);
    }
}

/// **The projector's half of the triggers.** Registered with the projector
/// beside the search feed; it only pushes ids, on the projector's thread.
pub struct ReadingsFeed {
    inbox: Arc<Inbox>,
}

impl ReadingsFeed {
    pub fn new(inbox: Arc<Inbox>) -> Self {
        Self { inbox }
    }
}

impl Consumer for ReadingsFeed {
    fn name(&self) -> &str {
        "readings"
    }

    /// **The first trigger** (ROADMAP §3): the document that references a
    /// blob was projected — a picture pasted into a page, a file imported.
    /// A document with no blob in it says nothing here.
    fn projected(&mut self, projection: &DocumentProjection) {
        if projection
            .nodes
            .iter()
            .any(|node| node.blob.is_some() && !node.in_trash)
        {
            self.inbox
                .document_projected(projection.workspace, projection.document);
        }
    }

    fn removed(&mut self, _workspace: WorkspaceId, _document: DocumentId) {}

    /// The listing is where a reading document arrives and leaves, and where
    /// an entry is trashed, restored or purged.
    fn listing_changed(&mut self, workspace: WorkspaceId) {
        self.inbox.listing_changed(workspace);
    }
}

#[cfg(test)]
mod tests;
