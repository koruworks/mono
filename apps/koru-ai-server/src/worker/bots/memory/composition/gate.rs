// SPDX-License-Identifier: MPL-2.0
//! Test channels can delay real adapter calls; no Memory proof is fabricated.
use super::*;
use koru_ai_core::{ProfileId, StoreRevision};

type Held = (SyncSender<()>, Receiver<()>);
pub(in crate::worker::bots) struct Gate {
    pub(in crate::worker::bots) inner: Arc<BotAuthorityAdapter>,
    creation: Mutex<Option<Held>>,
    replay: Mutex<Option<Held>>,
    pub(in crate::worker::bots) replay_calls: std::sync::atomic::AtomicUsize,
}
impl Gate {
    pub(in crate::worker::bots) fn new(inner: Arc<BotAuthorityAdapter>) -> Self {
        Self {
            inner,
            creation: Mutex::new(None),
            replay: Mutex::new(None),
            replay_calls: Default::default(),
        }
    }
    pub(in crate::worker::bots) fn hold_creation(&self) -> (Receiver<()>, SyncSender<()>) {
        hold(&self.creation)
    }
    pub(in crate::worker::bots) fn hold_replay(&self) -> (Receiver<()>, SyncSender<()>) {
        hold(&self.replay)
    }
}
fn hold(slot: &Mutex<Option<Held>>) -> (Receiver<()>, SyncSender<()>) {
    let (entered, reached) = sync_channel(1);
    let (release, held) = sync_channel(1);
    assert!(slot.lock().unwrap().replace((entered, held)).is_none());
    (reached, release)
}
fn wait(slot: &Mutex<Option<Held>>) {
    if let Some((entered, held)) = slot.lock().unwrap().take() {
        entered.send(()).unwrap();
        held.recv_timeout(Duration::from_secs(15)).unwrap();
    }
}
impl BotAuthority for Gate {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.inner.owner()
    }
    fn workspace(&self, workspace: WorkspaceId) -> Result<ProfileId, BotError> {
        self.inner.workspace(workspace)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.inner.page(page)
    }
    fn inspect(
        &self,
        workspaces: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<BotAuthoritySnapshot, BotError> {
        self.inner.inspect(workspaces, pages)
    }
    fn capture(
        &self,
        owner: BotOwner,
        workspace: WorkspaceId,
        reader: ProfileId,
        pages: &[BotPage],
    ) -> Result<Vec<d::RunContextItem>, BotError> {
        self.inner.capture(owner, workspace, reader, pages)
    }
    fn pinned_document_revision(
        &self,
        owner: BotOwner,
        workspace: WorkspaceId,
        reader: ProfileId,
        document: DocumentId,
        version: d::DocumentVersionId,
    ) -> Result<StoreRevision, BotError> {
        self.inner
            .pinned_document_revision(owner, workspace, reader, document, version)
    }
    fn prepare_memory_page(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        identity: &BotMemoryIdentity,
        cancellation: &Cancellation,
    ) -> Result<PreparedMemoryPage, MemoryAuthorityError> {
        let result = self
            .inner
            .prepare_memory_page(owner, reader, identity, cancellation);
        wait(&self.creation);
        result
    }
    fn capture_memory(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        binding: &BotMemoryBinding,
        cancellation: &Cancellation,
    ) -> Result<PreparedMemoryCapture, MemoryAuthorityError> {
        self.inner
            .capture_memory(owner, reader, binding, cancellation)
    }
    fn replay_memory_index(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        home: WorkspaceId,
        capture: &BotLibraryCapture,
        binding: Option<&BotMemoryBinding>,
        cancellation: &Cancellation,
    ) -> Result<VerifiedMemoryIndex, MemoryAuthorityError> {
        self.replay_calls.fetch_add(1, Ordering::SeqCst);
        wait(&self.replay);
        self.inner
            .replay_memory_index(owner, reader, home, capture, binding, cancellation)
    }
    fn read_memory_page(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        binding: &BotMemoryBinding,
        snapshot: MemorySnapshot,
        position: MemoryReadPosition,
        cancellation: &Cancellation,
    ) -> Result<MemoryPageResult, MemoryAuthorityError> {
        self.inner
            .read_memory_page(owner, reader, binding, snapshot, position, cancellation)
    }
}
