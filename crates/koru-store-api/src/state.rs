// SPDX-License-Identifier: MPL-2.0

//! What every handler shares.
//!
//! One [`Store`] behind one mutex. That is a real serialisation point, and it
//! is the right shape for the MVP rather than an oversight:
//!
//! - The management routes are not a hot path — a workspace is listed when
//!   somebody opens a switcher, not per keystroke.
//! - The editing path is `apply_update` → `flush`, and `flush` is already
//!   serialised by SQLite's write lock. A finer-grained scheme would contend on
//!   the same disk.
//!
//! What it does foreclose is two workspaces committing in parallel, which will
//! matter with enough of them open at once. The shape that fixes it is a lock
//! per workspace rather than one for the store, and it is recorded in TECHDEBT
//! rather than guessed at now — the number to optimise against is the one in
//! the stage 10 measurements, and it does not exist yet.

use std::sync::Arc;
use std::sync::Mutex;
use std::{future::Future, pin::Pin};

use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_core::model::BlobHash;
use koru_store_core::model::Scope;
use koru_store_core::service::Store;
use koru_store_core::version::{API_VERSION, CAPABILITIES};
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};
use koru_store_wire::reminders::{ReminderQuery, ReminderQueryResponse, ReminderSnapshot};

use crate::auth::Caller;
use crate::sessions::{SessionTtl, Sessions};

/// The store, as this build assembles it.
pub type KoruStore = Store<SqliteCatalog, SqliteBackend>;

#[derive(Clone, Debug, Default)]
pub struct BlobHydrationFailure {
    pub code: String,
    pub retryable: bool,
    /// Display names of the devices a parked hydration is waiting on; empty
    /// for any other failure or when the holder is unknown.
    pub waiting_on: Vec<String>,
    /// **The blob engine's hold**, when one is what stopped this — its own
    /// code, not a [`koru_store_core::errors::Code`] (D259).
    ///
    /// `code` above is flattened to the handful of tokens the Store has codes
    /// for, which loses the distinction a person most needs: *nobody holding
    /// this is online right now* and *this device is not using Cloud for this
    /// Workspace* are two different sentences with two different remedies,
    /// and the second one is a switch away.
    pub hold: Option<String>,
}

/// The daemon's on-demand bridge to peer blob hydration.
///
/// The HTTP layer owns local authorization and the response lifetime, but it
/// must not learn about iroh or peer selection. The returned future may remain
/// pending while every authorized peer is offline; that wait is event-driven
/// inside replication and schedules no retry timer.
pub trait BlobHydrationService: Send + Sync {
    fn hydrate(
        &self,
        workspace: WorkspaceId,
        hash: BlobHash,
    ) -> Pin<Box<dyn Future<Output = Result<(), BlobHydrationFailure>> + Send + 'static>>;
}

/// **Which devices this session saw advertise a blob** — the peer runtime's
/// in-memory holder directory, read for a person.
///
/// A blob's durable origin names only the device that *created* it; a device
/// that holds a copy is known only to the runtime that heard it advertise one.
/// The daemon owns that runtime and this crate must not learn about iroh, so
/// the answer crosses as device ids in storage form, per hash, and is named by
/// [`crate::cloud_waiting::documents`] through the same authorization
/// snapshot as the origin. Nothing is copied: each call reads the directory.
pub trait BlobHolderDirectory: Send + Sync {
    fn advertised_holders(
        &self,
        workspace: WorkspaceId,
        hashes: &[BlobHash],
    ) -> std::collections::HashMap<BlobHash, Vec<String>>;
}

/// What blob transfer is doing, for the workspace debug report.
///
/// *A component nobody can inspect is a component nobody can trust*, and by
/// the time somebody wants to look it is too late to add the seam. The engine
/// lives in the daemon and this crate must not learn what a transfer is, so
/// the answer arrives already shaped for the wire.
pub trait BlobTransferStatusService: Send + Sync {
    fn status(&self, workspace: WorkspaceId) -> koru_store_wire::internal::BlobTransferReport;

    /// **The one thing a person can tell the transfer engine** (D256): hold
    /// this workspace's pictures, or let them go again.
    ///
    /// It is here rather than in a service of its own because it is the same
    /// seam read the other way, and a surface that can ask *"what is it
    /// doing"* and *"stop"* through two unrelated ports is a surface whose two
    /// answers can disagree.
    ///
    /// **Documents are untouched.** This reaches the blob engine and nothing
    /// else; what stops a document is the transport mode, which is a durable
    /// choice and a different operation.
    ///
    /// Returns the report as it stands afterwards, so a caller that changed
    /// something does not have to ask again to draw it.
    fn set_paused(
        &self,
        workspace: WorkspaceId,
        paused: bool,
    ) -> koru_store_wire::internal::BlobTransferReport;

    /// **Cloud was just turned on for this workspace** — §10's first trigger,
    /// and the only thing that makes *tell, then start* happen at all (D256).
    ///
    /// The enable pass is the one that walks the workspace, publishes what is
    /// about to move, and then begins. Nothing else raises it: a document
    /// change, a mapping change and a recovery are all about a workspace that
    /// was already syncing.
    ///
    /// Returns immediately. The pass is gigabytes and the caller is a request
    /// answering a person, so the two must not be the same thread.
    fn enabled(&self, workspace: WorkspaceId);

    /// **What one file is doing** (D259 §4).
    ///
    /// The same seam read at one file rather than at a workspace: the address
    /// the read model has for that file document, the local record the store
    /// already holds, and what the last coverage pass already found on Cloud.
    /// A selection is a pointer move, so this is a lookup and never a probe —
    /// nothing here reaches a region.
    ///
    /// `None` when the document holds no blob reference at all, which is the
    /// honest answer for a document that is not a file.
    ///
    /// Async, unlike its neighbours, because the local record lives behind
    /// the store worker: the counts above are integers in memory and this is
    /// two indexed rows on another thread, and the thread that answers a
    /// person does not wait on the world.
    fn file(
        &self,
        workspace: WorkspaceId,
        file: DocumentId,
    ) -> Pin<Box<dyn Future<Output = Option<koru_store_wire::internal::BlobFileReport>> + Send + '_>>;

    /// **Drop this device's copies of every file Koru Cloud holds** and keep
    /// every reference. The engine checks each address against the control
    /// plane before touching it; this crate only carries the answer.
    ///
    /// Async for `file`'s reason and one more: it is a network round trip
    /// per file, and the thread that answers a person does not wait on the
    /// world. `Err` carries a stable code — a store with no Cloud reach, a
    /// Local-only workspace — and the report carries how far it got when the
    /// control plane stopped part way.
    fn release_local(
        &self,
        workspace: WorkspaceId,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<koru_store_wire::internal::BlobReleaseReport, &'static str>>
                + Send
                + '_,
        >,
    >;

    /// **Which documents are waiting on a file Cloud does not hold yet**,
    /// with the addresses each one is missing.
    ///
    /// Read-only: the engine's parks are read and left exactly as they were,
    /// so expanding a row on screen is never a trigger. The addresses come
    /// back so the handler can ask the Store whose device first held them;
    /// they never reach the wire. `Err` carries a stable code — the walk
    /// could not be read.
    fn waiting(&self, workspace: WorkspaceId) -> WaitingFuture<'_>;

    /// **One document's own Cloud state**: whether Cloud holds it, and how
    /// many of the images and files it references Cloud holds durably.
    ///
    /// Read-only, for [`Self::waiting`]'s reason: a person looking at a
    /// document is not a trigger, so nothing here clears a park, starts a pass
    /// or reaches a region. `Err` carries a stable code — the reference
    /// projection could not be read.
    fn document_coverage(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> DocumentCoverageFuture<'_>;
}

/// What [`BlobTransferStatusService::document_coverage`] returns.
pub type DocumentCoverageFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<koru_store_wire::internal::CloudDocumentCoverage, &'static str>>
            + Send
            + 'a,
    >,
>;

/// What [`BlobTransferStatusService::waiting`] returns.
pub type WaitingFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<WaitingFiles>, &'static str>> + Send + 'a>>;

/// One document the blob engine says is waiting, as [`BlobTransferStatusService::waiting`]
/// hands it to the handler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitingFiles {
    pub document: DocumentId,
    /// Each once. For the handler to resolve, never to send.
    pub blobs: Vec<BlobHash>,
}

/// Daemon-owned document-model adapter for reminder snapshots. The HTTP crate
/// stays free of the document model. Authored zone-name validation uses the
/// focused timezone mechanism directly, so a write does not depend on
/// projection availability.
pub trait ReminderProjectionService: Send + Sync {
    fn snapshot(
        &self,
        reminders: &[koru_store_core::reminders::ReminderReductionInput],
        settings: &koru_store_core::reminders::ReminderSettings,
    ) -> koru_store_core::Result<ReminderSnapshot>;

    fn query(
        &self,
        reminders: &[koru_store_core::reminders::ReminderReductionInput],
        settings: &koru_store_core::reminders::ReminderSettings,
        query: &ReminderQuery,
    ) -> koru_store_core::Result<ReminderQueryResponse>;
}

/// Everything a request can reach.
pub struct AppState {
    /// Credentials minted over the rendezvous. In memory, so none of them
    /// outlives this process — which is the whole reason `store.secret` is
    /// gone. See `sessions.rs`.
    pub sessions: Sessions,
    pub store: Mutex<KoruStore>,
    /// Where this store keeps its data. Reported as **one opaque string** on a
    /// debug route and never as anything a client can take apart — third
    /// parties do not need the physical layout, and telling them makes it a
    /// compatibility surface we would then have to keep.
    pub root: String,
    pub channel: String,
    pub started_at: i64,
    /// Who is subscribed to what.
    pub rooms: Arc<crate::rooms::Rooms>,
    pub cloud_changes: crate::cloud_changes::Changes,
    /// Ask the daemon to stop. Held here so the route that triggers it goes
    /// through the same authentication as everything else.
    pub shutdown: Arc<tokio::sync::Notify>,
    /// What the projection layer is doing, as the daemon last published it.
    ///
    /// **Opaque JSON, and that is the point.** The projection lives above this
    /// crate and links `koru-core`; typing this field would drag the node model
    /// into the HTTP layer to answer a diagnostic question. `null` when nothing
    /// is projecting, which is the honest answer for a build that runs no
    /// projector at all.
    pub projection: Arc<std::sync::Mutex<serde_json::Value>>,
    /// What the search index is doing, as the daemon last published it.
    ///
    /// Opaque JSON for the same reason as [`Self::projection`], and **counters
    /// only**: `/v1/debug/*` is metadata, never content, and an index is the
    /// component most tempted to answer with a title.
    pub search: Arc<std::sync::Mutex<serde_json::Value>>,
    /// What reading words out of files is doing — the coordinator's queue by
    /// reason, the current job's stage, the worker, how far the readings
    /// mirror is behind — as the daemon last published it (OCR TECHNICAL §14).
    ///
    /// Opaque JSON for the reason [`Self::projection`] gives, and **ids,
    /// counts and stages only**: never a file's name, a title or a word read.
    pub readings: Arc<std::sync::Mutex<serde_json::Value>>,
    /// Volatile replication authority. It starts empty after every Store
    /// restart and is cleared when the live Agent control session disappears.
    pub sync_control: Arc<std::sync::Mutex<koru_sync_control::ControlHost>>,
    /// What the Cloud driver's passes did, per document and per dataset.
    ///
    /// Written by `cloud_driver` beside the stage it already sets on
    /// [`Self::sync_control`], read by the Cloud sync query. **Memory only**,
    /// bounded and coalesced — see [`crate::cloud_activity`], which says why a
    /// record that survived a restart would be the wrong thing entirely.
    pub cloud_activity: Arc<std::sync::Mutex<crate::cloud_activity::CloudActivity>>,
    /// The index a query is served from, once the daemon has opened one.
    ///
    /// **A port rather than the thing itself.** `koru-search` links the node
    /// model and an FTS engine; this crate holds neither, so the daemon
    /// implements the trait and hands the result down. See
    /// [`routes::search`](crate::routes::search).
    ///
    /// `OnceLock` because it is written once during start-up, before anything is
    /// served, and read on every request afterwards — a `Mutex` would be a lock
    /// on the query path protecting a value that never changes. **Empty is a
    /// real state**, not an oversight: a store whose `search.db` will not open
    /// serves documents and answers 503 here.
    search_service: std::sync::OnceLock<Arc<dyn crate::routes::search::SearchService>>,
    /// The projection query used to match live documents to their blobs.
    /// Kept behind a port for the same reason as search: this HTTP crate must
    /// not link the application's node model or the derived database.
    workspace_usage_service:
        std::sync::OnceLock<Arc<dyn crate::routes::workspaces::WorkspaceUsageService>>,
    /// The projection query that answers a document's headings. Same port, same
    /// reason: an outline is derived data and this crate does not link the
    /// database that holds it.
    document_outline_service:
        std::sync::OnceLock<Arc<dyn crate::routes::workspaces::DocumentOutlineService>>,
    /// The attribute query — rows from the read model plus the definitions and
    /// tag tables. Same port, same reason as the outline.
    attribute_service: std::sync::OnceLock<Arc<dyn crate::routes::attributes::AttributeService>>,
    /// The tasks query — the same read model, asked a second question. Same
    /// port, same reason as the attribute query: this crate does not link
    /// `koru-projection`, and a task row is derived data.
    task_service: std::sync::OnceLock<Arc<dyn crate::routes::tasks::TaskService>>,
    /// One reading, from the readings mirror and the reading document — the
    /// same port, for the same reason: this crate links neither the mirror nor
    /// the reading document's shape (OCR TECHNICAL §10).
    reading_service: std::sync::OnceLock<Arc<dyn crate::routes::readings::ReadingService>>,
    /// What the attribute query service is doing, as its port last published
    /// it. **Counters only** — queries served, rows answered, milliseconds —
    /// never a key, a value or a tag name (the [`Self::search`] rule).
    pub attributes: Arc<std::sync::Mutex<serde_json::Value>>,
    /// What the tasks query service is doing, as its port last published it.
    /// **Counters only** — queries served, rows answered, sweeps requested,
    /// cursor restarts, milliseconds. Never a title, a breadcrumb, a due date
    /// or a task reference: a reference names a document and a node, which is
    /// exactly the shape of thing a bug report must not carry.
    pub tasks: Arc<std::sync::Mutex<serde_json::Value>>,
    /// The daemon's whole-Store backup implementation. The HTTP layer owns
    /// authentication and serialization; the binary owns the archive format.
    backup_service: std::sync::OnceLock<Arc<dyn crate::routes::admin::BackupService>>,
    /// On-demand peer hydration for a locally missing blob. Empty in embedded
    /// API tests and builds without a replication runtime.
    blob_hydration_service: std::sync::OnceLock<Arc<dyn BlobHydrationService>>,
    /// Empty on a build with no Cloud blob path, which is why the report's
    /// field is an `Option` rather than a row of zeroes claiming an engine
    /// that is not there.
    blob_transfer_status: std::sync::OnceLock<Arc<dyn BlobTransferStatusService>>,
    /// The peer runtime's holder directory. Empty in embedded API tests and
    /// builds without a replication runtime, where a waiting list names only
    /// each blob's origin.
    blob_holder_directory: std::sync::OnceLock<Arc<dyn BlobHolderDirectory>>,
    reminder_projection_service: std::sync::OnceLock<Arc<dyn ReminderProjectionService>>,
    /// A daemon-owned, transport-neutral wakeup invoked only after durable
    /// local mutations. The HTTP layer does not know which peer transport the
    /// daemon hosts.
    replication_notifier: std::sync::OnceLock<Arc<dyn Fn() + Send + Sync>>,
    /// A content-free, sticky invalidation for workspace-catalog readers.
    ///
    /// The generation is process-local: it resets when the Store starts and is
    /// ordering evidence rather than durable catalog data. A `watch` sender
    /// keeps the newest generation even when nobody is listening, so
    /// registering after a commit cannot leave a reader waiting for some later
    /// workspace. Readers reconcile by listing the catalog again.
    catalog_changes: tokio::sync::watch::Sender<u64>,
    /// Where a committed catalog fact goes. Empty in embedded API tests and in
    /// a build whose events journal would not open, and empty is a real state:
    /// the store serves every route, and the menus of every window refresh on
    /// their next open rather than on an event. See
    /// [`crate::catalog_events`].
    catalog_events: std::sync::OnceLock<Arc<dyn crate::catalog_events::CatalogEventSink>>,
    /// The installer of the reader that finds words in pictures (OCR Plan
    /// 2.4), once the daemon has started one. A port for the reason
    /// [`Self::search_service`] is one; **empty is a real state** — a phone
    /// and the integration host run none, and `/v1/readings/reader` answers
    /// `reader_unavailable` there.
    reader_service: std::sync::OnceLock<Arc<dyn crate::routes::reader::ReaderService>>,
}

impl AppState {
    /// Hand the reader's installer down, once, before anything is served.
    pub fn set_reader_service(
        &self,
        service: Arc<dyn crate::routes::reader::ReaderService>,
    ) -> bool {
        self.reader_service.set(service).is_ok()
    }

    pub fn reader_service(&self) -> Option<Arc<dyn crate::routes::reader::ReaderService>> {
        self.reader_service.get().cloned()
    }

    pub fn new(store: KoruStore, root: String, channel: String, started_at: i64) -> Self {
        Self::with_session_ttl(store, root, channel, started_at, SessionTtl::DEFAULT)
    }

    /// Assemble a Store with the credential lifetime chosen at process start.
    /// The policy belongs here, beside the in-memory session table, rather than
    /// in each client that will later consume a credential.
    pub fn with_session_ttl(
        mut store: KoruStore,
        root: String,
        channel: String,
        started_at: i64,
        session_ttl: SessionTtl,
    ) -> Self {
        let cloud_changes = crate::cloud_changes::Changes::default();
        store.set_commit_signal(Arc::new(cloud_changes.clone()));
        Self {
            sessions: Sessions::with_ttl(session_ttl),
            store: Mutex::new(store),
            root,
            channel,
            started_at,
            rooms: crate::rooms::Rooms::new(),
            cloud_changes,
            shutdown: Arc::new(tokio::sync::Notify::new()),
            projection: Arc::new(std::sync::Mutex::new(serde_json::Value::Null)),
            search: Arc::new(std::sync::Mutex::new(serde_json::Value::Null)),
            readings: Arc::new(std::sync::Mutex::new(serde_json::Value::Null)),
            sync_control: Arc::new(std::sync::Mutex::new(
                koru_sync_control::ControlHost::default(),
            )),
            cloud_activity: Arc::new(std::sync::Mutex::new(
                crate::cloud_activity::CloudActivity::new(),
            )),
            search_service: std::sync::OnceLock::new(),
            workspace_usage_service: std::sync::OnceLock::new(),
            document_outline_service: std::sync::OnceLock::new(),
            attribute_service: std::sync::OnceLock::new(),
            task_service: std::sync::OnceLock::new(),
            reading_service: std::sync::OnceLock::new(),
            attributes: Arc::new(std::sync::Mutex::new(serde_json::Value::Null)),
            tasks: Arc::new(std::sync::Mutex::new(serde_json::Value::Null)),
            backup_service: std::sync::OnceLock::new(),
            blob_hydration_service: std::sync::OnceLock::new(),
            blob_transfer_status: std::sync::OnceLock::new(),
            blob_holder_directory: std::sync::OnceLock::new(),
            reminder_projection_service: std::sync::OnceLock::new(),
            replication_notifier: std::sync::OnceLock::new(),
            catalog_changes: tokio::sync::watch::channel(0).0,
            catalog_events: std::sync::OnceLock::new(),
            reader_service: std::sync::OnceLock::new(),
        }
    }

    /// Hand the index down, once, before anything is served.
    ///
    /// Returns whether it was taken. A second call is a wiring mistake rather
    /// than a race — the daemon does this on one thread during start-up — so it
    /// is reported rather than merged or panicked on.
    pub fn set_search_service(
        &self,
        service: Arc<dyn crate::routes::search::SearchService>,
    ) -> bool {
        self.search_service.set(service).is_ok()
    }

    pub fn search_service(&self) -> Option<Arc<dyn crate::routes::search::SearchService>> {
        self.search_service.get().cloned()
    }

    pub fn set_workspace_usage_service(
        &self,
        service: Arc<dyn crate::routes::workspaces::WorkspaceUsageService>,
    ) -> bool {
        self.workspace_usage_service.set(service).is_ok()
    }

    pub fn workspace_usage_service(
        &self,
    ) -> Option<Arc<dyn crate::routes::workspaces::WorkspaceUsageService>> {
        self.workspace_usage_service.get().cloned()
    }

    pub fn set_document_outline_service(
        &self,
        service: Arc<dyn crate::routes::workspaces::DocumentOutlineService>,
    ) -> bool {
        self.document_outline_service.set(service).is_ok()
    }

    pub fn document_outline_service(
        &self,
    ) -> Option<Arc<dyn crate::routes::workspaces::DocumentOutlineService>> {
        self.document_outline_service.get().cloned()
    }

    /// Hand the readings port down, once, before anything is served.
    pub fn set_reading_service(
        &self,
        service: Arc<dyn crate::routes::readings::ReadingService>,
    ) -> bool {
        self.reading_service.set(service).is_ok()
    }

    pub fn reading_service(&self) -> Option<Arc<dyn crate::routes::readings::ReadingService>> {
        self.reading_service.get().cloned()
    }

    pub fn set_attribute_service(
        &self,
        service: Arc<dyn crate::routes::attributes::AttributeService>,
    ) -> bool {
        self.attribute_service.set(service).is_ok()
    }

    pub fn attribute_service(
        &self,
    ) -> Option<Arc<dyn crate::routes::attributes::AttributeService>> {
        self.attribute_service.get().cloned()
    }

    pub fn set_task_service(&self, service: Arc<dyn crate::routes::tasks::TaskService>) -> bool {
        self.task_service.set(service).is_ok()
    }

    pub fn task_service(&self) -> Option<Arc<dyn crate::routes::tasks::TaskService>> {
        self.task_service.get().cloned()
    }

    /// Hand the Store's backup implementation down once, before serving.
    pub fn set_backup_service(
        &self,
        service: Arc<dyn crate::routes::admin::BackupService>,
    ) -> bool {
        self.backup_service.set(service).is_ok()
    }

    pub fn backup_service(&self) -> Option<Arc<dyn crate::routes::admin::BackupService>> {
        self.backup_service.get().cloned()
    }

    pub fn set_blob_hydration_service(&self, service: Arc<dyn BlobHydrationService>) -> bool {
        self.blob_hydration_service.set(service).is_ok()
    }

    pub fn set_blob_transfer_status_service(
        &self,
        service: Arc<dyn BlobTransferStatusService>,
    ) -> bool {
        self.blob_transfer_status.set(service).is_ok()
    }

    pub fn blob_transfer_status_service(&self) -> Option<Arc<dyn BlobTransferStatusService>> {
        self.blob_transfer_status.get().cloned()
    }

    pub fn blob_hydration_service(&self) -> Option<Arc<dyn BlobHydrationService>> {
        self.blob_hydration_service.get().cloned()
    }

    pub fn set_blob_holder_directory(&self, directory: Arc<dyn BlobHolderDirectory>) -> bool {
        self.blob_holder_directory.set(directory).is_ok()
    }

    pub fn blob_holder_directory(&self) -> Option<Arc<dyn BlobHolderDirectory>> {
        self.blob_holder_directory.get().cloned()
    }

    pub fn set_reminder_projection_service(
        &self,
        service: Arc<dyn ReminderProjectionService>,
    ) -> bool {
        self.reminder_projection_service.set(service).is_ok()
    }

    pub fn reminder_projection_service(&self) -> Option<Arc<dyn ReminderProjectionService>> {
        self.reminder_projection_service.get().cloned()
    }

    pub fn set_replication_notifier(&self, notifier: Arc<dyn Fn() + Send + Sync>) -> bool {
        self.replication_notifier.set(notifier).is_ok()
    }

    pub fn notify_replication(&self) {
        self.cloud_changes.profiles_changed();
        if let Some(notifier) = self.replication_notifier.get() {
            notifier();
        }
    }

    /// Wake catalog readers after a durable topology change.
    ///
    /// This is separate from the event journal: it carries no event name,
    /// workspace id or content, and its only meaning is "list again". Calls
    /// belong after the Store lock has been released so a woken reader can
    /// immediately reconcile.
    pub fn notify_catalog_change(&self) {
        self.catalog_changes
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    /// Subscribe to the process-local generation used by the API watch route.
    ///
    /// Public for daemon-owned mutation adapters such as replication tests;
    /// product clients consume the authenticated route, never `AppState`.
    pub fn subscribe_catalog_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.catalog_changes.subscribe()
    }

    /// Hand the events journal down once, before serving.
    pub fn set_catalog_event_sink(
        &self,
        sink: Arc<dyn crate::catalog_events::CatalogEventSink>,
    ) -> bool {
        self.catalog_events.set(sink).is_ok()
    }

    /// Record one committed catalog fact.
    ///
    /// **Called only after the mutation it reports has committed**, and never
    /// while the store lock is held — the same ordering
    /// [`Self::broadcast_manifest`] obeys, for the same reason. A build with no
    /// journal does nothing here, which is why this returns nothing a caller
    /// could be tempted to branch on: a request never succeeds or fails on
    /// whether its observation was recorded.
    pub fn publish_catalog_event(&self, event: impl Into<koru_events::Draft>) {
        if let Some(sink) = self.catalog_events.get() {
            sink.publish(event.into());
        }
    }

    /// Mint a session for a client that has introduced itself over the
    /// rendezvous. The only way a credential is created.
    pub fn mint_session(&self, label: &str, scopes: Vec<Scope>) -> (String, i64) {
        self.sessions.mint(
            label,
            scopes,
            koru_store_core::model::Timestamp::now().millis(),
        )
    }

    /// Resolve a presented token to what it may do.
    ///
    /// Sessions first, because that is what every Koru client presents and the
    /// answer is a hash lookup in memory. Registered third-party clients are
    /// looked up in `catalog.db` after.
    ///
    /// Both resolve to `None` when unknown, expired *or* revoked, deliberately
    /// **indistinguishable from each other** — a difference between those is an
    /// oracle for guessing valid tokens.
    ///
    /// There is no constant-time comparison on this path any more, and that is
    /// not an omission: nothing is compared. A presented token is hashed and
    /// the hash is looked up, so the time taken says nothing about how much of
    /// a guess was right. `auth::constant_time_eq` survives for anything that
    /// does still compare a secret to a secret.
    pub fn resolve(&self, presented: &str) -> Option<Caller> {
        let now = koru_store_core::model::Timestamp::now().millis();
        if let Some(caller) = self.sessions.resolve(presented, now) {
            return Some(caller);
        }
        let hash = token_hash(presented);
        let store = self.store.lock().ok()?;
        let client = store.client_by_token(&hash).ok()??;
        Some(Caller {
            scopes: client.scopes.clone(),
            label: client.name,
        })
    }

    pub fn api_version(&self) -> u32 {
        API_VERSION
    }

    pub fn capabilities(&self) -> &'static [&'static str] {
        CAPABILITIES
    }
}

/// How a token is stored: BLAKE3 of its bytes.
///
/// The token itself is never written down, so a leaked `catalog.db` is not a
/// set of working keys. The same hash the store uses everywhere else — one
/// function rather than two.
pub fn token_hash(token: &str) -> Vec<u8> {
    koru_store_core::lifecycle::checksum(token.as_bytes()).into_bytes()
}

#[cfg(test)]
mod tests;

impl AppState {
    /// Tell a workspace's manifest room about a change made outside the
    /// WebSocket.
    ///
    /// **Always after the commit.** The HTTP handler has already returned from
    /// the store call that persisted this, so the same ordering the protocol
    /// path obeys — persist, then tell anyone else — holds here too.
    ///
    /// `ConnectionId(u64::MAX)` as the exclusion is deliberate: an HTTP request
    /// is not a member of the room, so there is nobody to leave out, and a
    /// sentinel that can never be a real connection says that plainly.
    pub fn broadcast_manifest(&self, workspace: koru_store_core::ids::WorkspaceId, delta: Vec<u8>) {
        self.broadcast_loro(crate::rooms::Room::Manifest(workspace), delta);
    }

    /// Tell a document room about a durable change made through a typed
    /// in-process worker command rather than a WebSocket connection.
    pub fn broadcast_document(&self, document: koru_store_core::ids::DocumentId, delta: Vec<u8>) {
        self.broadcast_loro(crate::rooms::Room::Document(document), delta);
    }

    fn broadcast_loro(&self, room: crate::rooms::Room, delta: Vec<u8>) {
        use loro_protocol::encoding;
        use loro_protocol::protocol::{BatchId, CrdtType, ProtocolMessage};

        let message = ProtocolMessage::DocUpdate {
            crdt: CrdtType::Loro,
            room_id: room.to_id(),
            updates: vec![delta],
            batch_id: BatchId([0; 8]),
        };
        // A delta too large to frame is the same case the WebSocket path
        // handles: the work is durable, the relay is not possible, and a
        // subscriber finds out by falling behind. Rare enough to be worth a
        // line rather than a mechanism — see TECHDEBT 21.
        if let Ok(frame) = encoding::encode(&message) {
            self.rooms
                .broadcast(&room, crate::rooms::ConnectionId(u64::MAX), frame);
        }
    }
}
