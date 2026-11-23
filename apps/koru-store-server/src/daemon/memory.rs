// SPDX-License-Identifier: MPL-2.0

//! One Store composed entirely in this process's memory.
//!
//! The same composition the daemon builds from a root path — the Store over
//! `catalog.db` and its `workspace.db`s, the projector and its read ports, the
//! search index, the attribute and task queries, the typed queue — with every
//! database in memory ([`koru_sqlite::open_memory`]). It is what a hosted
//! replica holds: a Workspace synced from Cloud that is read, searched and
//! queried exactly as a device's is, by the same code, with **no path anywhere
//! in it**. Nothing here takes a directory, so nothing here can write one; a
//! process whose filesystem is read-only runs it unchanged.
//!
//! **What it leaves out is what has no meaning in memory or no owner here:**
//! file bytes (the backend refuses, [`SqliteBackend::in_memory`]), backups,
//! the events journal, peer replication and the HTTP listener. Automatic
//! history is off: history is device-local and a replica is ephemeral.
//!
//! **It stops.** A device's derived threads run for the life of the process;
//! a replica is torn down when idle (TECHNICAL C10), so [`MemoryStore::close`]
//! — and `Drop` — stop the projector, the indexer and the queue, and join
//! them. Joining waits at most one idle tick (250 ms) per thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use koru_projection::worker::Sweeps;
use koru_store_api::AppState;
use koru_store_core::errors::{Code, Error};
use koru_store_core::ids::WorkspaceId;
use koru_store_core::model::Timestamp;
use koru_store_core::service::Store;
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};

use super::{StoreWorkerClient, WORKER_COMMAND_CAPACITY, WorkerCommand, WorkerError, dispatch};
use crate::derived::{Derived, MemoryDerived};

/// One in-memory Store composition. See the module note.
pub struct MemoryStore {
    state: Arc<AppState>,
    requests: tokio::sync::mpsc::Sender<WorkerCommand>,
    sweeps: Option<Sweeps>,
    stop: Arc<AtomicBool>,
    /// Woken on close, so the readings mirror's thread sees the stop at once.
    readings: Option<Arc<crate::readings::Inbox>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    threads: Vec<std::thread::JoinHandle<()>>,
    /// Held for the composition's lifetime: the read model and the index live
    /// only while a connection holds them.
    _derived: Derived,
}

impl MemoryStore {
    /// Compose an empty Store in memory. `channel` is only the name its status
    /// reports.
    ///
    /// A read model or index that will not open is not fatal, as on a device:
    /// the Store still serves documents, and the queries answer that they are
    /// not projected rather than failing.
    pub fn open(channel: &str) -> Result<Self, Error> {
        let catalog =
            SqliteCatalog::in_named_memory(&koru_sqlite::MemoryName::fresh("catalog.db"))?;
        let store = Store::open(catalog, SqliteBackend::in_memory())?
            .with_unversioned_kinds(koru_projection::model::UNVERSIONED);
        let state = Arc::new(AppState::new(
            store,
            "memory".into(),
            channel.into(),
            Timestamp::now().millis(),
        ));
        let derived = Derived::Memory(Arc::new(MemoryDerived::open().map_err(|error| {
            Error::new(
                Code::StorageFailed,
                format!("the in-memory read model did not open: {error}"),
            )
        })?));
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();

        // The daemon's order: the index's consumer before the projector's first
        // tick, the projector, its read ports, then the indexer on its sweeps.
        let (prepared, mut consumers) = match crate::search::prepare(&derived) {
            Some((feed, prepared)) => (Some(prepared), vec![feed]),
            None => (None, Vec::new()),
        };
        // The readings mirror, so words read on a device are found here too.
        // No coordinator: a replica holds no file bytes and reads nothing.
        let readings = crate::readings::prepare(&derived);
        if let Some(readings) = readings.as_ref() {
            consumers.push(readings.consumer());
        }
        let readings_inbox = readings.as_ref().map(crate::readings::Prepared::inbox);
        let projection = crate::projection::spawn(
            Arc::clone(&state),
            &derived,
            consumers,
            readings.as_ref().map(crate::readings::Prepared::tee),
            Arc::clone(&stop),
        );
        let mut sweeps = None;
        if let Some((handle, projected)) = projection {
            threads.push(handle);
            if let Some((usage, outline, reminders)) =
                crate::projection::read_ports(&derived, projected.clone())
            {
                let _ = state.set_workspace_usage_service(usage);
                let _ = state.set_document_outline_service(outline);
                let _ = state.set_reminder_projection_service(reminders);
            }
            if let Some(port) =
                crate::attributes::attribute_port(&derived, projected.clone(), &state)
            {
                let _ = state.set_attribute_service(port);
            }
            if let Some(port) = crate::tasks::task_port(&derived, projected.clone(), &state) {
                let _ = state.set_task_service(port);
            }
            let search_queue = prepared.as_ref().map(crate::search::Prepared::queue);
            if let Some(prepared) = prepared
                && let Some(handle) = crate::search::spawn(
                    prepared,
                    Arc::clone(&state),
                    projected.clone(),
                    readings
                        .as_ref()
                        .and_then(crate::readings::Prepared::source),
                    readings.as_ref().map(crate::readings::Prepared::hub),
                    Arc::clone(&stop),
                )
            {
                threads.push(handle);
            }
            if let Some(readings) = readings {
                threads.extend(readings.spawn(
                    &state,
                    search_queue,
                    None,
                    None,
                    None,
                    Arc::clone(&stop),
                ));
            }
            sweeps = Some(projected);
        }

        // The typed queue on its own thread, so a `StoreWorkerClient` over this
        // composition is the same client a device's worker hands out.
        let (requests, receiver) = tokio::sync::mpsc::channel(WORKER_COMMAND_CAPACITY);
        let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
        let served = Arc::clone(&state);
        let queue = std::thread::Builder::new()
            .name("koru-memory-store".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    tracing::error!("the in-memory Store queue could not start a runtime");
                    return;
                };
                runtime.block_on(async move {
                    tokio::select! {
                        () = dispatch::serve(receiver, served) => {}
                        _ = stopped => {}
                    }
                });
            })
            .map_err(|error| {
                Error::new(
                    Code::Internal,
                    format!("the in-memory Store queue did not start: {error}"),
                )
            })?;
        threads.push(queue);

        Ok(Self {
            state,
            requests,
            sweeps,
            stop,
            readings: readings_inbox,
            shutdown: Some(shutdown),
            threads,
            _derived: derived,
        })
    }

    /// The catalog watch's current generation, for a test that asserts a
    /// Store write woke its readers.
    #[cfg(test)]
    pub(crate) fn catalog_generation(&self) -> u64 {
        *self.state.subscribe_catalog_changes().borrow()
    }

    /// The typed queue: every `StoreWorkerClient` operation, served in arrival
    /// order on this composition's own thread.
    pub fn client(&self) -> StoreWorkerClient {
        StoreWorkerClient {
            requests: self.requests.clone(),
        }
    }

    /// Run one bounded operation on the Store, **on the caller's thread**,
    /// under the composition's lock. For an executor that owns the thread its
    /// Store work runs on — a per-replica actor — rather than the queue.
    pub fn with_store<R>(
        &self,
        operation: impl FnOnce(&mut crate::WorkerStore) -> R,
    ) -> Result<R, WorkerError> {
        let mut store = self.state.store.lock().map_err(|_| WorkerError::Poisoned)?;
        Ok(operation(&mut store))
    }

    /// One search, from this composition's index, on the caller's thread.
    pub fn search(
        &self,
        request: &koru_store_wire::search::SearchRequest,
    ) -> Result<koru_store_wire::search::SearchResponse, Error> {
        dispatch::search(&self.state, request)
    }

    /// The tasks query, on the caller's thread: the same answer
    /// `POST /v1/tasks/query` gives, from the same port.
    pub fn query_tasks(
        &self,
        query: koru_store_wire::tasks::TaskQuery,
    ) -> Result<koru_store_wire::tasks::TaskQueryResponse, Error> {
        koru_store_api::routes::tasks::answer(&self.state, query)
            .map_err(koru_store_api::routes::tasks::TaskServiceError::into_error)
    }

    /// A subscription to one dataset's durable commits, for an executor that
    /// serves a Cloud pass's [`StoreJobs::watch_cloud`](super::StoreJobs) itself.
    pub fn watch_cloud(
        &self,
        dataset: koru_store_api::cloud_changes::Dataset,
    ) -> koru_store_api::cloud_changes::Subscription {
        self.state.cloud_changes.subscribe(dataset)
    }

    /// Join `workspace`'s manifest room, as a window's sync session does, to
    /// hear every change to its document list the Store tells the room from
    /// then on. The member leaves when dropped.
    pub fn join_manifest(&self, workspace: WorkspaceId) -> RoomMember {
        self.join_room(koru_store_api::rooms::Room::Manifest(workspace))
    }

    /// Join `document`'s room, as an open page's sync session does.
    pub fn join_document(&self, document: koru_store_core::ids::DocumentId) -> RoomMember {
        self.join_room(koru_store_api::rooms::Room::Document(document))
    }

    fn join_room(&self, room: koru_store_api::rooms::Room) -> RoomMember {
        let rooms = Arc::clone(&self.state.rooms);
        let who = rooms.next_connection();
        let (sender, frames) = tokio::sync::mpsc::unbounded_channel();
        rooms.join(room.clone(), who, sender);
        RoomMember {
            rooms,
            room,
            who,
            frames,
        }
    }

    /// The readiness signal: ask for `workspace` to be swept, and answer the
    /// ticket [`Self::swept`] is complete for. `None` when the read model did
    /// not open, which is never ready.
    pub fn request_sweep(&self, workspace: WorkspaceId) -> Option<u64> {
        self.sweeps.as_ref().map(|sweeps| sweeps.request(workspace))
    }

    /// Whether the sweep `ticket` names has finished: the read model has
    /// compared `workspace` against the Store at least as recently as the
    /// request that minted it.
    pub fn swept(&self, workspace: WorkspaceId, ticket: u64) -> bool {
        self.sweeps
            .as_ref()
            .is_some_and(|sweeps| sweeps.finished(workspace) >= ticket)
    }

    /// Stop the projector, the indexer and the queue, and join them. Store
    /// work already running finishes; a client's later command is refused as
    /// `Stopped`. Everything the composition held is dropped with it.
    pub fn close(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(readings) = self.readings.as_ref() {
            readings.stop();
        }
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        for thread in self.threads.drain(..) {
            if thread.join().is_err() {
                tracing::error!("an in-memory Store thread panicked");
            }
        }
    }
}

/// One member of a sync room of a [`MemoryStore`]
/// ([`MemoryStore::join_room`]).
pub struct RoomMember {
    rooms: Arc<koru_store_api::rooms::Rooms>,
    room: koru_store_api::rooms::Room,
    who: koru_store_api::rooms::ConnectionId,
    frames: tokio::sync::mpsc::UnboundedReceiver<koru_store_api::rooms::Frame>,
}

impl RoomMember {
    /// The Loro updates told to the room since the last call, in the order
    /// the Store sent them: one entry per update a frame carried. Frames
    /// that carry no update are skipped. Never waits.
    pub fn told(&mut self) -> Vec<Vec<u8>> {
        use loro_protocol::protocol::ProtocolMessage;
        let mut updates = Vec::new();
        while let Ok(frame) = self.frames.try_recv() {
            if let Ok(ProtocolMessage::DocUpdate {
                updates: carried, ..
            }) = loro_protocol::encoding::decode(&frame)
            {
                updates.extend(carried);
            }
        }
        updates
    }
}

impl Drop for RoomMember {
    fn drop(&mut self) {
        self.rooms.leave(&self.room, self.who);
    }
}

impl Drop for MemoryStore {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

#[cfg(test)]
mod tests;
