// SPDX-License-Identifier: MPL-2.0

//! Running the store.
//!
//! The order below is the whole of it, and every step is where it is for a
//! reason:
//!
//! ```text
//! 1  bind the private claim  nothing else may open these databases
//! 2  open catalog.db         ...and nothing else. See below.
//! 3  bind 127.0.0.1:0        the port is assigned, not chosen
//! 4  report test readiness   only the explicit standalone harness receives it
//! 5  serve until told to stop
//! ```
//!
//! **Step 2 opens `catalog.db` and nothing else.** No workspace database, no
//! `LoroDoc`, no blob directory — those are opened in response to a request.
//! Start-up therefore costs the same on a store holding one workspace and one
//! holding a thousand, which is the difference between a daemon a client can
//! spawn on demand and one that has to be running already.
//!
//! ## What used to be here
//!
//! An `fs2` lease, a `store.json` holding the port, and a `store.secret`
//! holding a bearer token — three files to say one thing, and a step 1a that
//! deleted whichever of them a power-switch kill had left behind. All three are
//! gone. The rendezvous does the lease's job by being bindable only once, does
//! `store.json`'s job by being connectable only while the process lives, and
//! replaces `store.secret` with a credential minted per connection that is
//! never written down.
//!
//! ## Binding and accepting are two moments
//!
//! Step 1 is the exclusion — a second daemon fails there, before it has opened
//! a file. Step 4 is the readiness signal. A client that connects in between
//! waits at `accept` until there is an address, rather than being handed one
//! that does not work yet. Neither had to be arranged; it falls out of the
//! endpoint being the authority.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub mod blob_sync;
pub mod cloud_driver;
pub mod cloud_sync;
mod dispatch;
#[cfg(any(test, feature = "test-support"))]
// The exported fixture API includes faults used by downstream host tests.
#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
pub mod fake_cloud;
pub mod memory;
mod session_io;
mod store_jobs;
pub use store_jobs::{StoreJobs, StoreReply};

use crate::tray;
use koru_store_api::AppState;
use koru_store_api::sessions::SessionTtl;
use koru_store_core::errors::{Code, Error, Result};
use koru_store_core::model::{Scope, Timestamp};
use koru_store_core::service::Store;
use koru_store_core::version::API_VERSION;
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};

use koru_channel as channel;

/// How long the data plane outlives the control session that granted it.
/// This covers an Agent restart without rebuilding every peer connection,
/// while keeping the one-sided revocation window bounded (ADR-160).
const SYNC_SESSION_GRACE: Duration = Duration::from_secs(30);

struct StandaloneGatewayPolicy {
    state: Arc<AppState>,
    endpoint: String,
    instance_id: String,
    channel: String,
}

impl koru_api_discovery::server::Policy for StandaloneGatewayPolicy {
    fn greet(
        &self,
        hello: &koru_api_discovery::ClientHello,
    ) -> koru_api_discovery::GatewayResponse {
        if let Some(refusal) =
            koru_api_discovery::admission_refusal(hello, &self.channel, koru_api_wire::API_VERSION)
        {
            return refusal;
        }
        let (token, expires_at_millis) =
            self.state.mint_session(&hello.product, Scope::ALL.to_vec());
        koru_api_discovery::GatewayResponse::Welcome(Box::new(koru_api_discovery::GatewayHello {
            api_version: koru_api_wire::API_VERSION,
            supervisor_launch_id: None,
            gateway_version: env!("CARGO_PKG_VERSION").into(),
            segment: self.channel.clone(),
            api_endpoint: self.endpoint.clone(),
            instance_id: self.instance_id.clone(),
            capabilities: vec!["store".into(), "ai-disabled".into()],
            session: koru_api_discovery::SessionCredential::new(token, expires_at_millis),
        }))
    }
}

pub const CATALOG_FILE: &str = "catalog.db";
pub const INTERNAL_READY_ENV: &str = koru_env::name::KORU_INTERNAL_STORE_READY;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct StandaloneReady {
    pub api_endpoint: String,
    pub token: String,
    pub expires_at_millis: i64,
}

impl StandaloneReady {
    pub fn discovery(&self) -> Result<koru_store_client::Discovery> {
        koru_store_client::Discovery::from_loopback_endpoint(
            &self.api_endpoint,
            &self.token,
            self.expires_at_millis,
        )
    }
}

/// Ordinary lifecycle/admission requests waiting for one Store worker.
/// Shutdown is deliberately out-of-band and cannot be starved by this queue.
pub const WORKER_COMMAND_CAPACITY: usize = 64;

#[derive(Clone)]
pub struct WorkerConfig {
    root: PathBuf,
    channel: String,
    console: bool,
    standalone_adapters: bool,
    standalone_ready: Option<PathBuf>,
    host_iroh: bool,
    serve_http: bool,
    claim_root: bool,
    /// Whether this process asks the transport-mode gate before moving blob
    /// bytes (§6's fourth point).
    ///
    /// **False on mobile, and that is not an oversight.** Migration 12 seeds
    /// every row `local_only`, and nothing promotes a row but a person: the
    /// desktop's Profile prompt, answered in the Manage tab, reaching
    /// `Store::choose_transport_mode` (D258). Android has no Cloud sync toggle
    /// at all (D238) and therefore no prompt, so a phone that asked the gate
    /// would refuse every picture with no way back — D254's outage, moved to
    /// the phone. Flip this to `true` in the same commit that gives Android a
    /// way for its owner to raise a Profile, and B5's metered-pause tests will
    /// tell you if it is wired the wrong way round.
    gate_transport_modes: bool,
    network_edge: Option<NetworkEdgeConfig>,
    /// The events worker the commit feed forwards into, when the host opened
    /// one (the gateway does; the standalone diagnostic host does not).
    events: Option<koru_events_server::EventsHandle>,
    #[cfg(test)]
    panic_after_claim: bool,
}

#[derive(Clone)]
struct NetworkEdgeConfig {
    egress: Arc<dyn koru_network_edge::NetworkEgress>,
    sessions: Arc<Mutex<Option<tokio::sync::mpsc::Receiver<koru_network_edge::TransportSession>>>>,
    connections:
        Arc<Mutex<Option<tokio::sync::mpsc::Receiver<koru_network_edge::ConnectionEvent>>>>,
}

impl std::fmt::Debug for WorkerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerConfig")
            .field("root", &self.root)
            .field("channel", &self.channel)
            .field("console", &self.console)
            .field("standalone_adapters", &self.standalone_adapters)
            .field("standalone_ready", &self.standalone_ready)
            .field("host_iroh", &self.host_iroh)
            .field("serve_http", &self.serve_http)
            .field("claim_root", &self.claim_root)
            .field("network_edge", &self.network_edge.is_some())
            .field("events", &self.events.is_some())
            .finish()
    }
}

impl WorkerConfig {
    pub fn standalone(
        root: PathBuf,
        channel: impl Into<String>,
        console: bool,
        standalone_ready: Option<PathBuf>,
    ) -> Self {
        Self {
            root,
            channel: channel.into(),
            console,
            standalone_adapters: true,
            standalone_ready,
            host_iroh: true,
            serve_http: true,
            claim_root: true,
            // The desktop initializer runs; see the field.
            gate_transport_modes: true,
            network_edge: None,
            events: None,
            #[cfg(test)]
            panic_after_claim: false,
        }
    }

    /// A co-located worker still claims the same root, but exposes neither the
    /// legacy Store rendezvous nor process-owned logging/tray UI.
    pub fn embedded(root: PathBuf, channel: impl Into<String>) -> Self {
        Self {
            root,
            channel: channel.into(),
            console: true,
            standalone_adapters: false,
            standalone_ready: None,
            host_iroh: false,
            serve_http: false,
            claim_root: true,
            // The desktop initializer runs; see the field.
            gate_transport_modes: true,
            network_edge: None,
            events: None,
            #[cfg(test)]
            panic_after_claim: false,
        }
    }

    /// Attach the events worker the commit feed will forward into.
    pub fn with_events(mut self, events: koru_events_server::EventsHandle) -> Self {
        self.events = Some(events);
        self
    }

    /// Desktop transition shape: no public Store rendezvous or process UI,
    /// while the Agent-owned identity/control plane provisions the device
    /// network adapter through its private endpoint.
    pub fn embedded_with_device_edge(root: PathBuf, channel: impl Into<String>) -> Self {
        let mut config = Self::embedded(root, channel);
        config.host_iroh = true;
        config
    }

    /// Product desktop shape: Store owns replication semantics and durable
    /// state, while an external device edge supplies authenticated opaque
    /// sessions. No endpoint implementation is constructed in this worker.
    pub fn embedded_with_network_edge(
        root: PathBuf,
        channel: impl Into<String>,
        egress: Arc<dyn koru_network_edge::NetworkEgress>,
        sessions: tokio::sync::mpsc::Receiver<koru_network_edge::TransportSession>,
        connections: tokio::sync::mpsc::Receiver<koru_network_edge::ConnectionEvent>,
    ) -> Self {
        let mut config = Self::embedded(root, channel);
        config.network_edge = Some(NetworkEdgeConfig {
            egress,
            sessions: Arc::new(Mutex::new(Some(sessions))),
            connections: Arc::new(Mutex::new(Some(connections))),
        });
        config
    }

    /// Mobile main-process shape. The worker is the sole Store owner but
    /// publishes no HTTP listener, desktop root claim, rendezvous, or network
    /// adapter; the lifecycle-owning mobile host supplies those concerns
    /// in-process where applicable.
    pub fn mobile(root: PathBuf, channel: impl Into<String>) -> Self {
        let mut config = Self::embedded(root, channel);
        config.claim_root = false;
        // See the field: Android has no transport-mode initializer yet, so
        // gating it would refuse every picture on every phone.
        config.gate_transport_modes = false;
        config
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn channel(&self) -> &str {
        &self.channel
    }
}

/// The private, root-qualified ownership claim held before any Store database
/// opens. It intentionally exposes no endpoint, handshake, or credential.
#[derive(Debug)]
pub struct StoreRootClaim {
    _listener: koru_ipc::Listener,
}

impl StoreRootClaim {
    pub fn acquire(channel: &str, root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)
            .map_err(|error| Error::new(Code::StorageFailed, format!("store root: {error}")))?;
        Self::acquire_private(channel, root).map_err(|error| {
            // A taken claim here is almost always an older installation's
            // store still serving after its bundle was replaced on disk —
            // macOS lets a running app's bundle be dragged over. Name the
            // holder, or everything upstream can only say "nothing is
            // listening" and the person starts an hour of process archaeology.
            let held_by = koru_ipc::Endpoint::for_service_root(koru_ipc::STORE, channel, root)
                .ok()
                .and_then(|endpoint| koru_ipc::diagnose::holder_of(endpoint.name()))
                .map(|holder| {
                    format!(
                        " — this channel's data is held by {holder}, usually an older \
                         Koru still running; quit it and relaunch"
                    )
                })
                .unwrap_or_default();
            Error::new(Code::StorageFailed, format!("{error}{held_by}"))
        })
    }

    fn acquire_private(
        channel: &str,
        root: &Path,
    ) -> std::result::Result<Self, koru_ipc::IpcError> {
        let endpoint = koru_ipc::Endpoint::for_service_root(koru_ipc::STORE, channel, root)?;
        koru_ipc::Listener::bind(endpoint).map(|listener| Self {
            _listener: listener,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerStatus {
    Starting,
    Ready { api_version: u32, port: u16 },
    Stopping,
    Stopped,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerError {
    Saturated,
    Stopped,
    TimedOut,
    Poisoned,
    Startup(String),
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Saturated => formatter.write_str("the Store worker command queue is saturated"),
            Self::Stopped => formatter.write_str("the Store worker has stopped"),
            Self::TimedOut => formatter.write_str("the Store worker did not answer in time"),
            Self::Poisoned => formatter.write_str("the Store worker state is poisoned"),
            Self::Startup(error) => write!(formatter, "the Store worker could not start: {error}"),
        }
    }
}

impl std::error::Error for WorkerError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerExit {
    Clean,
    Failed(String),
    Panicked,
}

/// Store-owned state read through the same bounded typed worker queue that
/// gateway requests will use. This is deliberately not a gateway type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreWorkerInfo {
    pub api_version: u32,
    pub capabilities: Vec<String>,
    pub channel: String,
    pub started_at: i64,
}

enum WorkerCommand {
    WatchCloud {
        dataset: koru_store_api::cloud_changes::Dataset,
        reply: tokio::sync::oneshot::Sender<koru_store_api::cloud_changes::Subscription>,
    },
    Noop(std::sync::mpsc::SyncSender<()>),
    Status(std::sync::mpsc::SyncSender<StoreWorkerInfo>),
    Http {
        request: axum::extract::Request,
        caller: koru_store_api::auth::Caller,
        reply: tokio::sync::oneshot::Sender<axum::response::Response>,
    },
    Store(
        Box<dyn FnOnce(std::result::Result<&mut crate::WorkerStore, WorkerError>) + Send + 'static>,
    ),
    Search {
        request: koru_store_wire::search::SearchRequest,
        reply: std::sync::mpsc::SyncSender<
            std::result::Result<koru_store_wire::search::SearchResponse, Error>,
        >,
    },
    Tasks {
        query: Box<koru_store_wire::tasks::TaskQuery>,
        reply: std::sync::mpsc::SyncSender<
            std::result::Result<koru_store_wire::tasks::TaskQueryResponse, Error>,
        >,
    },
    CreateDocument {
        workspace: koru_store_core::ids::WorkspaceId,
        title: String,
        kind: String,
        /// The app connection's `actor/1` message for the manifest commit, or
        /// `None` for every caller on a device.
        attribution: Option<String>,
        at: Timestamp,
        reply: std::sync::mpsc::SyncSender<
            std::result::Result<koru_store_core::ids::DocumentId, Error>,
        >,
    },
    ApplyDocumentUpdate {
        workspace: koru_store_core::ids::WorkspaceId,
        document: koru_store_core::ids::DocumentId,
        update: Vec<u8>,
        at: Timestamp,
        reply: std::sync::mpsc::SyncSender<std::result::Result<(), Error>>,
    },
    SeedDocument {
        workspace: koru_store_core::ids::WorkspaceId,
        title: String,
        kind: String,
        parent: Option<koru_store_core::ids::DocumentId>,
        index: Option<usize>,
        scope: String,
        idempotency_key: String,
        request_hash: String,
        initial_update: Vec<u8>,
        /// As `CreateDocument`'s: the manifest entry's label.
        attribution: Option<String>,
        at: Timestamp,
        reply: std::sync::mpsc::SyncSender<
            std::result::Result<koru_store_core::service::SeededCreation, Error>,
        >,
    },
}

struct ReadyParts {
    status: WorkerStatus,
    shutdown: Arc<tokio::sync::Notify>,
    /// The blob transfer engine this worker composed, for a host that embeds
    /// the worker and holds the account itself.
    ///
    /// **Handed over rather than composed twice.** On the desktop the account
    /// is in another process and reaches the engine over control 7; on a phone
    /// there is no boundary to cross, and the alternative to this field is a
    /// second engine — two state machines over one staging directory, which is
    /// the race the pass mutex exists to prevent, made permanent.
    ///
    /// `None` when the read model would not open, exactly as
    /// [`blob_sync::install`] answers.
    blobs: Option<blob_sync::BlobSync>,
}

pub struct StoreWorker {
    config: WorkerConfig,
}

impl StoreWorker {
    pub fn new(config: WorkerConfig) -> Self {
        Self { config }
    }

    /// Run on the current thread until shutdown, as the standalone host does.
    pub fn serve(self) -> Result<()> {
        self.serve_with(None, None)
    }

    /// Start an observed in-process worker. Readiness is returned only after
    /// root claim, Store recovery, derived workers, and listener bind succeed.
    pub fn spawn(self) -> std::result::Result<WorkerHandle, WorkerError> {
        let (requests, request_rx) = tokio::sync::mpsc::channel(WORKER_COMMAND_CAPACITY);
        let request_tx = requests.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let status = Arc::new(Mutex::new(WorkerStatus::Starting));
        let thread_status = Arc::clone(&status);
        let fallback_ready = ready_tx.clone();
        let join = std::thread::Builder::new()
            .name("koru-store-server worker".into())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.serve_with(
                        Some((
                            request_rx,
                            StoreWorkerClient {
                                requests: request_tx,
                            },
                        )),
                        Some(ready_tx),
                    )
                }));
                let exit = match outcome {
                    Ok(Ok(())) => WorkerExit::Clean,
                    Ok(Err(error)) => {
                        let message = error.to_string();
                        let _ = fallback_ready.try_send(Err(message.clone()));
                        WorkerExit::Failed(message)
                    }
                    Err(_) => {
                        let _ = fallback_ready.try_send(Err("worker panicked".into()));
                        WorkerExit::Panicked
                    }
                };
                *thread_status.lock().expect("worker status") = match &exit {
                    WorkerExit::Clean => WorkerStatus::Stopped,
                    WorkerExit::Failed(error) => WorkerStatus::Failed(error.clone()),
                    WorkerExit::Panicked => WorkerStatus::Failed("worker panicked".into()),
                };
                exit
            })
            .map_err(|error| WorkerError::Startup(error.to_string()))?;

        match ready_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(parts)) => {
                *status.lock().expect("worker status") = parts.status;
                Ok(WorkerHandle {
                    requests,
                    shutdown: parts.shutdown,
                    status,
                    blobs: parts.blobs,
                    join: Mutex::new(Some(join)),
                })
            }
            Ok(Err(error)) => {
                let _ = join.join();
                Err(WorkerError::Startup(error))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(WorkerError::TimedOut),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(WorkerError::Startup(
                "worker exited before readiness".into(),
            )),
        }
    }

    fn serve_with(
        self,
        requests: Option<(
            tokio::sync::mpsc::Receiver<WorkerCommand>,
            StoreWorkerClient,
        )>,
        ready: Option<std::sync::mpsc::SyncSender<std::result::Result<ReadyParts, String>>>,
    ) -> Result<()> {
        let _logging = self
            .config
            .standalone_adapters
            .then(|| crate::log::init(&self.config.root, self.config.console));
        tracing::info!(
            api = koru_store_core::version::API_VERSION,
            catalog_schema = koru_store_core::version::CATALOG_SCHEMA_VERSION,
            workspace_schema = koru_store_core::version::WORKSPACE_SCHEMA_VERSION,
            crdt_schema = koru_store_core::version::CRDT_SCHEMA_VERSION,
            bundle_format = koru_store_core::version::BUNDLE_FORMAT_VERSION,
            projection_schema = koru_projection::PROJECTION_SCHEMA_VERSION,
            search_schema = koru_search::SEARCH_SCHEMA_VERSION,
            root = %self.config.root.display(),
            "store build and schema support"
        );
        let session_ttl = SessionTtl::DEFAULT;
        tracing::info!(
            session_ttl_ms = session_ttl.millis(),
            "Store session credential lifetime"
        );
        let startup =
            koru_store_core::stats::Span::start(koru_store_core::stats::names::STORE_STARTUP);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|error| Error::new(Code::Internal, format!("runtime: {error}")))?;
        runtime.block_on(run(self.config, startup, session_ttl, requests, ready))
    }
}

#[derive(Debug)]
pub struct WorkerHandle {
    requests: tokio::sync::mpsc::Sender<WorkerCommand>,
    shutdown: Arc<tokio::sync::Notify>,
    status: Arc<Mutex<WorkerStatus>>,
    /// The blob transfer engine this worker composed, or `None` when its read
    /// model would not open. Read by a host that embeds the worker and holds
    /// the account in the same process; the standalone desktop never asks,
    /// because its account reaches the engine over control 7.
    blobs: Option<blob_sync::BlobSync>,
    /// Behind a mutex so the thread can be joined through a shared reference —
    /// a gateway holds this handle inside an `Arc<GatewayState>` and still has
    /// to be able to observe the exit on its own shutdown path, rather than
    /// leaving it to the destructor, which discards it.
    join: Mutex<Option<std::thread::JoinHandle<WorkerExit>>>,
}

#[derive(Clone)]
pub struct StoreWorkerClient {
    requests: tokio::sync::mpsc::Sender<WorkerCommand>,
}

impl StoreWorkerClient {
    /// Read Store capabilities through the bounded worker queue.
    pub fn store_status(&self) -> std::result::Result<StoreWorkerInfo, WorkerError> {
        let (reply, response) = std::sync::mpsc::sync_channel(1);
        self.requests
            .try_send(WorkerCommand::Status(reply))
            .map_err(map_admission_error)?;
        response
            .recv_timeout(Duration::from_secs(5))
            .map_err(map_response_error)
    }

    /// Publish a complete document through the worker so manifest subscribers
    /// and replication observe the same mutation as the HTTP route.
    #[allow(clippy::too_many_arguments)]
    pub fn seed_document(
        &self,
        workspace: koru_store_core::ids::WorkspaceId,
        title: String,
        kind: String,
        parent: Option<koru_store_core::ids::DocumentId>,
        index: Option<usize>,
        scope: String,
        idempotency_key: String,
        request_hash: String,
        initial_update: Vec<u8>,
        at: Timestamp,
    ) -> std::result::Result<
        std::result::Result<koru_store_core::ids::DocumentId, Error>,
        WorkerError,
    > {
        self.seed_document_full(
            workspace,
            title,
            kind,
            parent,
            index,
            scope,
            idempotency_key,
            request_hash,
            initial_update,
            None,
            at,
        )
        .map(|result| result.map(|creation| creation.document))
    }

    /// [`seed_document`](Self::seed_document), answering the whole creation,
    /// with the manifest entry's commit labelled `attribution` when one is
    /// given: an app connection's `actor/1` message
    /// (`contracts/connector-attribution-v1.md`).
    #[allow(clippy::too_many_arguments)]
    pub fn seed_document_full(
        &self,
        workspace: koru_store_core::ids::WorkspaceId,
        title: String,
        kind: String,
        parent: Option<koru_store_core::ids::DocumentId>,
        index: Option<usize>,
        scope: String,
        idempotency_key: String,
        request_hash: String,
        initial_update: Vec<u8>,
        attribution: Option<String>,
        at: Timestamp,
    ) -> std::result::Result<
        std::result::Result<koru_store_core::service::SeededCreation, Error>,
        WorkerError,
    > {
        let (reply, response) = std::sync::mpsc::sync_channel(1);
        self.requests
            .try_send(WorkerCommand::SeedDocument {
                workspace,
                title,
                kind,
                parent,
                index,
                scope,
                idempotency_key,
                request_hash,
                initial_update,
                attribution,
                at,
                reply,
            })
            .map_err(map_admission_error)?;
        response
            .recv_timeout(Duration::from_secs(30))
            .map_err(map_response_error)
    }

    pub fn search(
        &self,
        request: koru_store_wire::search::SearchRequest,
    ) -> std::result::Result<
        std::result::Result<koru_store_wire::search::SearchResponse, Error>,
        WorkerError,
    > {
        let (reply, response) = std::sync::mpsc::sync_channel(1);
        self.requests
            .try_send(WorkerCommand::Search { request, reply })
            .map_err(map_admission_error)?;
        response
            .recv_timeout(Duration::from_secs(30))
            .map_err(map_response_error)
    }

    /// The tasks query, answered by the composition's task port exactly as
    /// `POST /v1/tasks/query` answers it — the same clamp, and the same honest
    /// empty page from a Store whose read model did not open.
    pub fn query_tasks(
        &self,
        query: koru_store_wire::tasks::TaskQuery,
    ) -> std::result::Result<
        std::result::Result<koru_store_wire::tasks::TaskQueryResponse, Error>,
        WorkerError,
    > {
        let (reply, response) = std::sync::mpsc::sync_channel(1);
        self.requests
            .try_send(WorkerCommand::Tasks {
                query: Box::new(query),
                reply,
            })
            .map_err(map_admission_error)?;
        response
            .recv_timeout(Duration::from_secs(30))
            .map_err(map_response_error)
    }

    pub fn create_document(
        &self,
        workspace: koru_store_core::ids::WorkspaceId,
        title: String,
        kind: String,
        at: Timestamp,
    ) -> std::result::Result<
        std::result::Result<koru_store_core::ids::DocumentId, Error>,
        WorkerError,
    > {
        self.create_document_attributed(workspace, title, kind, None, at)
    }

    /// [`create_document`](Self::create_document), with the manifest commit
    /// labelled `attribution` when one is given: an app connection's
    /// `actor/1` message (`contracts/connector-attribution-v1.md`).
    pub fn create_document_attributed(
        &self,
        workspace: koru_store_core::ids::WorkspaceId,
        title: String,
        kind: String,
        attribution: Option<String>,
        at: Timestamp,
    ) -> std::result::Result<
        std::result::Result<koru_store_core::ids::DocumentId, Error>,
        WorkerError,
    > {
        let (reply, response) = std::sync::mpsc::sync_channel(1);
        self.requests
            .try_send(WorkerCommand::CreateDocument {
                workspace,
                title,
                kind,
                attribution,
                at,
                reply,
            })
            .map_err(map_admission_error)?;
        response
            .recv_timeout(Duration::from_secs(30))
            .map_err(map_response_error)
    }

    pub fn apply_document_update(
        &self,
        workspace: koru_store_core::ids::WorkspaceId,
        document: koru_store_core::ids::DocumentId,
        update: Vec<u8>,
        at: Timestamp,
    ) -> std::result::Result<std::result::Result<(), Error>, WorkerError> {
        let (reply, response) = std::sync::mpsc::sync_channel(1);
        self.requests
            .try_send(WorkerCommand::ApplyDocumentUpdate {
                workspace,
                document,
                update,
                at,
                reply,
            })
            .map_err(map_admission_error)?;
        response
            .recv_timeout(Duration::from_secs(30))
            .map_err(map_response_error)
    }
}

impl WorkerHandle {
    /// The blob transfer engine, for an embedding host that composes the
    /// account itself. `None` on a device whose read model would not open.
    pub fn blob_sync(&self) -> Option<&blob_sync::BlobSync> {
        self.blobs.as_ref()
    }

    pub fn client(&self) -> StoreWorkerClient {
        StoreWorkerClient {
            requests: self.requests.clone(),
        }
    }
    pub fn status(&self) -> WorkerStatus {
        self.status.lock().expect("worker status").clone()
    }

    /// A no-op typed request used to prove admission, boundedness, and worker
    /// observation without adding a second transport or serialization pass.
    pub fn ping(&self) -> std::result::Result<(), WorkerError> {
        let (reply, response) = std::sync::mpsc::sync_channel(1);
        self.requests
            .try_send(WorkerCommand::Noop(reply))
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => WorkerError::Saturated,
                tokio::sync::mpsc::error::TrySendError::Closed(_) => WorkerError::Stopped,
            })?;
        response
            .recv_timeout(Duration::from_secs(5))
            .map_err(|error| match error {
                std::sync::mpsc::RecvTimeoutError::Timeout => WorkerError::TimedOut,
                std::sync::mpsc::RecvTimeoutError::Disconnected => WorkerError::Stopped,
            })
    }

    /// Read real Store state through the typed in-process worker boundary.
    pub fn store_status(&self) -> std::result::Result<StoreWorkerInfo, WorkerError> {
        let (reply, response) = std::sync::mpsc::sync_channel(1);
        self.requests
            .try_send(WorkerCommand::Status(reply))
            .map_err(map_admission_error)?;
        response
            .recv_timeout(Duration::from_secs(5))
            .map_err(map_response_error)
    }

    /// Route one already-authenticated public request through Store's typed
    /// catalog without an internal socket or a second serialization pass.
    pub async fn request(
        &self,
        request: axum::extract::Request,
        caller: koru_store_api::auth::Caller,
    ) -> std::result::Result<axum::response::Response, WorkerError> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.requests
            .try_send(WorkerCommand::Http {
                request,
                caller,
                reply,
            })
            .map_err(map_admission_error)?;
        tokio::time::timeout(Duration::from_secs(30), response)
            .await
            .map_err(|_| WorkerError::TimedOut)?
            .map_err(|_| WorkerError::Stopped)
    }

    pub fn shutdown(&self) {
        *self.status.lock().expect("worker status") = WorkerStatus::Stopping;
        // `notify_one` retains a permit if shutdown races the server future's
        // first poll; `notify_waiters` would lose that early signal.
        self.shutdown.notify_one();
    }

    pub fn join(self) -> WorkerExit {
        self.shutdown_and_join().expect("worker join handle")
    }

    /// Ask the worker to stop and wait for its thread, **through a shared
    /// reference**, answering how it ended.
    ///
    /// `None` means this handle's thread was already joined — by an earlier
    /// call or by [`WorkerHandle::join`]. It is not a clean exit and must
    /// never be read as one: absence of a report is never evidence of a clean
    /// join.
    ///
    /// Blocking. It exists because the one caller that needs the answer — a
    /// gateway host, on its shutdown path — holds the handle inside an `Arc`
    /// it cannot unwrap, and the alternative was the destructor, which throws
    /// the `WorkerExit` away.
    pub fn shutdown_and_join(&self) -> Option<WorkerExit> {
        self.shutdown();
        let join = self
            .join
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()?;
        Some(join.join().unwrap_or(WorkerExit::Panicked))
    }
}

fn map_admission_error<T>(error: tokio::sync::mpsc::error::TrySendError<T>) -> WorkerError {
    match error {
        tokio::sync::mpsc::error::TrySendError::Full(_) => WorkerError::Saturated,
        tokio::sync::mpsc::error::TrySendError::Closed(_) => WorkerError::Stopped,
    }
}

fn map_response_error(error: std::sync::mpsc::RecvTimeoutError) -> WorkerError {
    match error {
        std::sync::mpsc::RecvTimeoutError::Timeout => WorkerError::TimedOut,
        std::sync::mpsc::RecvTimeoutError::Disconnected => WorkerError::Stopped,
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.shutdown.notify_one();
        if let Some(join) = self
            .join
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = join.join();
        }
    }
}

async fn run(
    config: WorkerConfig,
    startup: koru_store_core::stats::Span,
    session_ttl: SessionTtl,
    requests: Option<(
        tokio::sync::mpsc::Receiver<WorkerCommand>,
        StoreWorkerClient,
    )>,
    ready: Option<std::sync::mpsc::SyncSender<std::result::Result<ReadyParts, String>>>,
) -> Result<()> {
    // **Every host has the typed queue.** An observed worker was handed the
    // receiver by `spawn`; the standalone host used to run with none, which
    // left its own Cloud driver with no way to reach the Store it sits next
    // to. The queue costs nothing while nobody sends on it.
    let (requests, store_client) = match requests {
        Some((requests, client)) => (requests, client),
        None => {
            let (sender, receiver) = tokio::sync::mpsc::channel(WORKER_COMMAND_CAPACITY);
            (receiver, StoreWorkerClient { requests: sender })
        }
    };
    let WorkerConfig {
        root,
        gate_transport_modes,
        channel: channel_name,
        console,
        standalone_adapters,
        standalone_ready,
        host_iroh,
        serve_http,
        claim_root,
        network_edge,
        events,
        #[cfg(test)]
        panic_after_claim,
    } = config;
    tracing::info!("store startup: begin");
    // 1. The claim. Before anything is opened, so a second daemon fails here
    //    rather than halfway through touching the same files.
    std::fs::create_dir_all(&root)
        .map_err(|e| Error::new(Code::StorageFailed, format!("store root: {e}")))?;
    let claim = if !claim_root {
        None
    } else {
        Some(
            match StoreRootClaim::acquire_private(&channel_name, &root) {
                Ok(claim) => claim,
                // **Losing the race to bind is not a failure when nobody asked to
                // watch.** A store is a singleton per user and channel, and clients
                // auto-start one; several clients starting at once therefore *expect*
                // all but one of these processes to find the endpoint taken. The
                // postcondition the launcher wanted — a store is serving this channel —
                // holds, so this process stands down with nothing to say.
                //
                // It matters because a tray launch has no console, and the only way it
                // can report anything is a modal dialog. Under `cargo test --workspace`
                // several suites raced and each loser put one up: eight dialogs from a
                // green test run, which is how this was found.
                //
                // `--console` is the dividing line, and it is the right one: it means
                // somebody is watching this particular process — a person at a terminal
                // or a harness reading its output — and they asked *this* store to
                // serve. Telling them it did not is the whole point.
                Err(koru_ipc::IpcError::AlreadyBound(_)) if standalone_adapters && !console => {
                    tracing::info!(
                        channel = %channel_name,
                        "another Koru Store is already serving this channel; standing down"
                    );
                    return Ok(());
                }
                Err(error) => return Err(Error::new(Code::StorageFailed, error.to_string())),
            },
        )
    };
    tracing::info!("store startup: root claim acquired");
    // This file is only a black-box harness seam, not a lifecycle authority.
    // An abrupt test-process kill can leave it behind, so the new owner clears
    // it only after winning the root claim. A losing second process must never
    // erase the live owner's readiness record.
    if standalone_adapters && let Some(path) = standalone_ready.as_ref() {
        let _ = std::fs::remove_file(path);
    }
    #[cfg(test)]
    if panic_after_claim {
        panic!("requested Store worker panic after root claim");
    }

    if let Some(service) = channel::looks_synced(&root) {
        // Advisory, never fatal: a false positive must not lock someone out of
        // their own data. It is here so a corruption report carries the one
        // fact most likely to explain it.
        eprintln!(
            "warning: this store is inside a {service} folder. A live database in a synchronised \
             folder is two writers with no lock between them; move it somewhere local."
        );
    }

    // 2. `catalog.db`, and nothing else.
    let catalog = SqliteCatalog::open(&root.join(CATALOG_FILE))?;
    let mut store = Store::open(catalog, SqliteBackend::new(&root))?;
    tracing::info!("store startup: catalog and store opened");
    let purged = store.purge_deleted(Timestamp::from_millis(
        Timestamp::now()
            .millis()
            .saturating_sub(koru_store_core::TRASH_RETENTION_MS),
    ))?;
    if purged > 0 {
        tracing::info!(workspaces = purged, "purged expired deleted workspaces");
    }
    tracing::info!("store startup: purge complete");
    // Blob staging, after a crash. **Not a migration** — start-up hashes
    // nothing that is not already an abandoned pending file, and the layout
    // move happens lazily on the first read of each blob. A pending file is
    // never renderable at any age, so this costs a directory listing on the
    // ordinary start-up and says nothing.
    if let Err(error) = SqliteBackend::new(&root).sweep_pending_objects(Timestamp::now()) {
        // Advisory: a staging directory that will not be read is not a reason
        // to refuse somebody their store.
        tracing::warn!(%error, "could not sweep blob staging");
    }
    let started_at = Timestamp::now().millis();

    let state = Arc::new(AppState::with_session_ttl(
        store,
        root.display().to_string(),
        channel_name.clone(),
        started_at,
        session_ttl,
    ));
    let _ = state.set_backup_service(Arc::new(crate::store_backup::LiveBackup::new(&root)));
    // **Before anything is served**, so the first request's mutation already
    // has somewhere to report itself. A host that opened no journal leaves this
    // empty and every publish is a no-op — the store serves every route either
    // way, and windows fall back to refetching on open.
    if let Some(events) = events.clone() {
        let _ =
            state.set_catalog_event_sink(Arc::new(crate::events_feed::CatalogEvents::new(events)));
    }
    tracing::info!("store startup: backup service ready");
    let mut peer_hydration: Option<Arc<dyn koru_store_api::state::BlobHydrationService>> = None;
    // Filled once the read model opens, below. The Cloud acceptor has to exist
    // before the first bridged session can arrive and the engine cannot exist
    // before `projection.db` does, so the two meet through this rather than
    // being ordered against each other.
    let blob_engine_slot: cloud_driver::BlobEngineSlot = Arc::new(std::sync::OnceLock::new());
    // The durable document changes the blob engine's §10 trigger reads. Minted
    // before the projector starts, because its consumer is registered there and
    // the engine it feeds does not exist until the read model is open.
    let blob_changes = crate::daemon::blob_sync::BlobChanges::new();
    let replication = if host_iroh {
        tracing::info!("store startup: replication (iroh)");
        let replication_store = Arc::new(crate::replication::LiveReplicationStore::new(
            Arc::clone(&state),
        ));
        let replication = koru_network_iroh::RuntimeHandle::spawn_with_store(
            iroh_network_config(),
            replication_store,
        );
        // **Held rather than installed.** The blob engine wraps this with a
        // Cloud fallback once the read model is open, and the service is a
        // `OnceLock`: installing the peer half here would leave no way to add
        // the other. Nothing reads it until the listener binds, well below.
        peer_hydration = Some(Arc::new(crate::replication::LiveBlobHydration::new(
            replication.clone(),
        ))
            as Arc<dyn koru_store_api::state::BlobHydrationService>);
        let replication_wakeup = replication.clone();
        let _ = state.set_replication_notifier(Arc::new(move || {
            replication_wakeup.notify_local_change();
        }));
        crate::sync_control::start(
            &root,
            &channel_name,
            Arc::clone(&state.sync_control),
            replication.clone(),
            SYNC_SESSION_GRACE,
            store_client.clone(),
        )
        .map_err(|error| Error::new(Code::StorageFailed, format!("sync control: {error}")))?;
        Some(crate::sync_control::ReplicationRuntime::Iroh(replication))
    } else if let Some(network_edge) = network_edge {
        tracing::info!("store startup: replication (network edge)");
        let replication_store = Arc::new(crate::replication::LiveReplicationStore::new(
            Arc::clone(&state),
        ));
        let replication = koru_network_iroh::SessionRuntimeHandle::new(
            replication_store,
            Arc::clone(&network_edge.egress),
        );
        peer_hydration = Some(
            Arc::new(crate::replication::LiveBlobHydration::from_sessions(
                replication.clone(),
            )) as Arc<dyn koru_store_api::state::BlobHydrationService>,
        );
        let replication_wakeup = replication.clone();
        let _ = state.set_replication_notifier(Arc::new(move || {
            replication_wakeup.notify_local_change();
        }));
        crate::sync_control::start(
            &root,
            &channel_name,
            Arc::clone(&state.sync_control),
            crate::sync_control::ReplicationRuntime::Session(replication.clone()),
            SYNC_SESSION_GRACE,
            store_client.clone(),
        )
        .map_err(|error| Error::new(Code::StorageFailed, format!("sync control: {error}")))?;
        // The Cloud owner is installed before the first bridged session can
        // be accepted below, so a Cloud stream never meets a runtime that
        // has nowhere to send it.
        cloud_driver::install(
            &replication,
            Arc::clone(&state.sync_control),
            store_client.clone(),
            Arc::clone(&state.rooms),
            Arc::clone(&state.cloud_activity),
            Arc::clone(&blob_engine_slot),
        );
        let mut sessions = network_edge
            .sessions
            .lock()
            .map_err(|_| Error::new(Code::Internal, "network session receiver poisoned"))?
            .take()
            .ok_or_else(|| Error::new(Code::Internal, "network session receiver already taken"))?;
        let mut connections = network_edge
            .connections
            .lock()
            .map_err(|_| Error::new(Code::Internal, "network event receiver poisoned"))?
            .take()
            .ok_or_else(|| Error::new(Code::Internal, "network event receiver already taken"))?;
        let session_runtime = replication.clone();
        tokio::spawn(async move {
            while let Some(session) = sessions.recv().await {
                session_runtime.accept(session);
            }
        });
        let event_runtime = replication.clone();
        tokio::spawn(async move {
            while let Some(event) = connections.recv().await {
                event_runtime.connection_event(event);
            }
        });
        Some(crate::sync_control::ReplicationRuntime::Session(
            replication,
        ))
    } else {
        None
    };
    // A waiting list names the devices this runtime heard advertise a blob,
    // beside the blob's origin — the copy-holder the origin never names.
    if let Some(replication) = replication.clone() {
        let _ = state.set_blob_holder_directory(Arc::new(replication));
    }
    tracing::info!("store startup: replication ready");
    let shutdown = state.shutdown.clone();

    let dispatcher =
        Some(requests).map(|requests| tokio::spawn(dispatch::serve(requests, Arc::clone(&state))));

    // The read model, on its own thread. **After the store opens and before
    // anything can be served**, so a document committed by the first request
    // already has somewhere to be projected to — and started with the catalog
    // rather than with a workspace, because it opens nothing until it sweeps.
    //
    // The search index is opened first, so its consumer is registered before
    // the projector's first tick, and started afterwards, because it needs the
    // projector's sweep handle. Neither is fatal: a store whose derived state
    // will not open is a store whose search is stale, not one that has lost
    // anything.
    tracing::info!("store startup: preparing search");
    // A device's derived state lives under its root; its threads run for the
    // life of the process, so nothing ever sets this.
    let derived = crate::derived::Derived::root(&root);
    let runs_forever = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (prepared, mut consumers) = match crate::search::prepare(&derived) {
        Some((feed, prepared)) => (Some(prepared), vec![feed]),
        None => (None, Vec::new()),
    };
    // The events feed, beside the search feed: seeded from the projection's
    // own listings first (an upgrade must not announce every document as
    // born today), then one more consumer on the same commit feed.
    if let Some(events) = events
        && let Some(feed) = crate::events_feed::prepare(&root, events)
    {
        consumers.push(feed);
    }
    // The blob reference feed, beside them: the one place a *durable* document
    // change becomes a candidate for a Cloud blob pass. It records what each
    // document now references; whether the Workspace's set actually moved is
    // decided later, and only a `true` costs a walk.
    consumers.push(crate::daemon::blob_sync::BlobChangeFeed::new(
        blob_changes.clone(),
    ));
    tracing::info!("store startup: spawning projection");
    let projection = crate::projection::spawn(
        Arc::clone(&state),
        &derived,
        consumers,
        Arc::clone(&runs_forever),
    );
    if let Some((_, sweeps)) = projection.as_ref() {
        if let Some((usage, outline, reminders)) =
            crate::projection::read_ports(&derived, sweeps.clone())
        {
            let _ = state.set_workspace_usage_service(usage);
            let _ = state.set_document_outline_service(outline);
            let _ = state.set_reminder_projection_service(reminders);
        }
        // The attribute query port, beside the outline's: same sweep-first
        // read of the projection, plus the workspace system documents for
        // definitions and tags — which is why it also holds a weak handle to
        // the store.
        if let Some(port) = crate::attributes::attribute_port(&derived, sweeps.clone(), &state) {
            let _ = state.set_attribute_service(port);
        }
        // The tasks query port, beside the attribute one: the same read model
        // asked a second question, so the same sweep handle, the same weak
        // store handle for the system documents, and its own connection.
        if let Some(port) = crate::tasks::task_port(&derived, sweeps.clone(), &state) {
            let _ = state.set_task_service(port);
        }
    }
    // The blob transfer engine, after the read model it walks and before the
    // listener that serves the window's first `GET /blobs`. A device whose
    // projection would not open gets the peer path alone: reconciling against
    // a guess about which blobs a Workspace has would upload the wrong set.
    let blob_sync = match projection.as_ref() {
        Some((_, sweeps)) => crate::daemon::blob_sync::install(
            &root,
            sweeps.clone(),
            store_client.clone(),
            Arc::clone(&state.sync_control),
            Arc::clone(&state.rooms),
            peer_hydration.clone(),
            replication.clone(),
            gate_transport_modes,
        ),
        None => None,
    };
    match blob_sync.clone() {
        Some((sync, hydration)) => {
            let _ = blob_engine_slot.set(sync.clone());
            let _ = state.set_blob_hydration_service(hydration);
            // Askable, always live, and free: a handful of integers on
            // `GET /v1/debug/workspaces/{id}`, so nobody has to have switched
            // a flag on before the incident.
            let _ = state.set_blob_transfer_status_service(Arc::new(sync.clone()));
            let following = sync.clone();
            let changes = blob_changes.clone();
            tokio::spawn(async move { following.follow(changes).await });
            tracing::info!("store startup: blob transfer engine ready");
        }
        None => {
            if let Some(peers) = peer_hydration {
                let _ = state.set_blob_hydration_service(peers);
            }
        }
    }
    let _search = match (prepared, projection.as_ref()) {
        (Some(prepared), Some((_, sweeps))) => crate::search::spawn(
            prepared,
            Arc::clone(&state),
            sweeps.clone(),
            Arc::clone(&runs_forever),
        ),
        _ => None,
    };
    tracing::info!("store startup: search spawned");

    let app = koru_store_api::router(Arc::clone(&state));

    // 3. Loopback, ephemeral port. **Never `0.0.0.0`** — the store binds one
    //    interface, and there is no setting through which it could become
    //    reachable from the network.
    tracing::info!("store startup: binding listener");
    let listener = if serve_http {
        Some(
            tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|e| Error::new(Code::StorageFailed, format!("bind: {e}")))?,
        )
    } else {
        None
    };
    let port = match listener.as_ref() {
        Some(listener) => listener
            .local_addr()
            .map_err(|e| Error::new(Code::Internal, format!("local address: {e}")))?
            .port(),
        None => 0,
    };

    let standalone_gateway = if standalone_adapters {
        // The standalone executable is an unshipped integration host. Its
        // black-box suites still enter through the product API boundary, so it
        // emulates the gateway greeting while routing to this worker's HTTP
        // endpoint. Shipping launchers never start this process.
        let rendezvous = koru_api_discovery::server::Rendezvous::bind_at(&channel_name, &root)
            .map_err(|error| Error::new(Code::StorageFailed, error.to_string()))?;
        let policy = Arc::new(StandaloneGatewayPolicy {
            state: Arc::clone(&state),
            endpoint: format!("http://127.0.0.1:{port}"),
            instance_id: format!("store-integration-{}", std::process::id()),
            channel: channel_name.clone(),
        });
        Some(tokio::spawn(rendezvous.serve(policy)))
    } else {
        None
    };

    // 4. Only an explicitly configured standalone integration harness receives
    // the HTTP endpoint and service credential. Product clients discover the
    // public gateway instead, and embedded workers publish nothing here.
    let ready_file = if standalone_adapters {
        let path = standalone_ready.ok_or_else(|| {
            Error::new(
                Code::StorageFailed,
                format!("standalone Store requires {INTERNAL_READY_ENV}"),
            )
        })?;
        let (token, expires_at_millis) =
            state.mint_session("koru-store-integration", Scope::ALL.to_vec());
        let ready = StandaloneReady {
            api_endpoint: format!("http://127.0.0.1:{port}"),
            token,
            expires_at_millis,
        };
        write_ready_file(&path, &ready)?;
        Some(ReadyFile(path))
    } else {
        None
    };
    let _private_claim = claim;

    if serve_http {
        tracing::info!("koru-store-server listening on 127.0.0.1:{port} ({channel_name})");
    } else {
        tracing::info!("koru-store-server serving typed in-process requests ({channel_name})");
    }
    // **Where the data is, said out loud.** A store root is derived from the
    // channel and, for an explicit `--root`, digested into the endpoint name —
    // so "which database am I looking at" is a question that otherwise takes
    // reading two functions to answer. Printed on every launch, because it is
    // one line and it is the first thing anybody debugging asks.
    tracing::info!(root = %root.display(), "storage: {}", root.display());
    if console {
        // On a console the logs themselves scroll past; the file is where the
        // last run went, which is what a person wants when the interesting part
        // has already gone by.
        tracing::info!(logs = %root.join("logs").display(), "logs: {}", root.join("logs").display());
    }
    report_outbox(&root);
    startup.stop();

    if let Some(ready) = ready {
        ready
            .send(Ok(ReadyParts {
                status: WorkerStatus::Ready {
                    api_version: API_VERSION,
                    port,
                },
                shutdown: Arc::clone(&shutdown),
                blobs: blob_sync.map(|(sync, _)| sync),
            }))
            .map_err(|_| Error::new(Code::Internal, "worker readiness receiver disappeared"))?;
    }

    // 5. The icon, last — a store that is not yet answering must not offer a
    //    menu whose every item needs it to be. Never fatal: no icon resource,
    //    no shell, another process holding the notification area — the daemon
    //    logs it and serves anyway, because refusing to run over a picture
    //    would be a worse outcome than the one it was avoiding.
    let _tray = (standalone_adapters && !console)
        .then(|| tray::attach(&channel_name, Arc::clone(&state)))
        .flatten();

    // 5.
    let served = match listener {
        Some(listener) => axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.notified().await })
            .await
            .map_err(|e| Error::new(Code::Internal, format!("serve: {e}"))),
        None => {
            shutdown.notified().await;
            Ok(())
        }
    };

    drop(ready_file);
    if let Some(gateway) = standalone_gateway {
        gateway.abort();
    }
    if let Some(dispatcher) = dispatcher {
        dispatcher.abort();
    }
    if let Some(replication) = replication {
        replication.clear();
    }
    served?;

    // Free when `KORU_STORE_STATS` is unset, which is almost always.
    koru_store_core::stats::report("koru-store-server, where the time went");
    tracing::info!("koru-store-server stopped");
    Ok(())
}

/// Deployment-only network selection. This never reaches trust records or the
/// Store database, so changing relay/lookup providers cannot change identity.
///
/// Since Plan 5.2 the configuration comes from `koru-network-iroh`'s one
/// shared helper — isolated mode, the `KORU_IROH_RELAYS` override, and the
/// compiled Koru relay list for this build's channel — instead of a second
/// hand-rolled copy of the env parsing. This diagnostic host reads no relay
/// preference (that is the Agent's; decision №7), so relays are simply on.
fn iroh_network_config() -> koru_network_iroh::IrohNetworkConfig {
    koru_network_iroh::host_network_config(
        true,
        koru_network_iroh::RelayChannelKind::for_internal_build(
            koru_channel::ReleaseChannel::this_build_is_internal(),
        ),
    )
}

/// Say whether a Koru window has left work under this root that it has not been
/// able to hand over.
///
/// **The store does not read the outbox, and this line must not imply it
/// does.** Those files are written by an app that could not reach a store and
/// replayed *by that app* when it reconnects ([ADR-100](../../../docs/decisions/ADR-076-100.md#adr-100-outbox--work-the-store-has-not-confirmed-is-kept-beside-the-store-and-the-outbox-is-a-buffer-rather-than-a-transport));
/// nothing here can apply them, and a store that tried would be importing
/// updates on behalf of a replica it cannot speak for. What it *can* do is
/// count them, which is the whole of the question somebody asks after a crash:
/// is anything still waiting.
///
/// Silent when there is nothing, so a healthy launch says nothing new.
fn report_outbox(root: &std::path::Path) {
    let outbox = root.join("outbox");
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut documents = 0usize;
    // Two levels: `outbox/<workspace>/<document>/<replica>.loro`. Walked by
    // hand rather than with a crate, because it is two `read_dir`s and the
    // shape is fixed by the writer.
    let Ok(workspaces) = std::fs::read_dir(&outbox) else {
        return;
    };
    for workspace in workspaces.flatten() {
        let Ok(docs) = std::fs::read_dir(workspace.path()) else {
            continue;
        };
        for document in docs.flatten() {
            let Ok(entries) = std::fs::read_dir(document.path()) else {
                continue;
            };
            let mut any = false;
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata()
                    && meta.is_file()
                {
                    files += 1;
                    bytes += meta.len();
                    any = true;
                }
            }
            if any {
                documents += 1;
                // **The ids are DEBUG, and the count is INFO.** A document id is
                // not something anyone can act on — there is no way to open a
                // document by uuid — so at INFO twenty-two of these say nothing
                // the summary below does not, and bury the launch lines that do
                // matter. They stay reachable for the case where the question
                // really is *which* document.
                tracing::debug!(
                    document = %document.file_name().to_string_lossy(),
                    "outbox: work waiting for document {}",
                    document.file_name().to_string_lossy()
                );
            }
        }
    }
    if files > 0 {
        tracing::info!(
            files,
            bytes,
            documents,
            // **"when it connects" was wrong, and the wrongness cost a
            // question.** `Session::replay_buffered` runs per *document*, on
            // open — so connecting clears nothing by itself, and a buffer for a
            // document nobody reopens waits indefinitely. That is deliberate
            // (ADR-100: losing it is the unrecoverable bug), but a line that
            // implies a connection drains the pile sends somebody looking for a
            // flush that does not exist.
            "outbox: {files} file(s), {bytes} bytes, {documents} document(s) waiting under {} \
             — each is replayed by a Koru window when that document is next opened, \
             not on connect; this store never applies them",
            outbox.display()
        );
    }
}

fn write_ready_file(path: &Path, ready: &StandaloneReady) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| Error::new(Code::StorageFailed, error.to_string()))?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let bytes =
        serde_json::to_vec(ready).map_err(|error| Error::new(Code::Internal, error.to_string()))?;
    std::fs::write(&temporary, bytes)
        .map_err(|error| Error::new(Code::StorageFailed, error.to_string()))?;
    let _ = std::fs::remove_file(path);
    std::fs::rename(&temporary, path)
        .map_err(|error| Error::new(Code::StorageFailed, error.to_string()))
}

struct ReadyFile(PathBuf);

impl Drop for ReadyFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A typed client over a queue nobody serves, for tests of code that must
/// hold a client without a Store behind it.
#[cfg(test)]
pub(crate) fn test_store_client() -> StoreWorkerClient {
    let (requests, _receiver) = tokio::sync::mpsc::channel(1);
    StoreWorkerClient { requests }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This host's entry point is a thin wrapper over the shared helper:
    /// whatever the machine's `KORU_IROH_*` environment says, it is exactly
    /// `host_network_config` with relays on and this build's channel — the
    /// precedence rows themselves are the helper's table-driven suite in
    /// `koru-network-iroh`.
    #[test]
    fn iroh_network_config_is_the_shared_helper_with_relays_on() {
        assert_eq!(
            iroh_network_config(),
            koru_network_iroh::host_network_config(
                true,
                koru_network_iroh::RelayChannelKind::for_internal_build(
                    koru_channel::ReleaseChannel::this_build_is_internal(),
                ),
            )
        );
    }

    #[test]
    fn embedded_worker_is_ready_bounded_stoppable_and_restartable() {
        let root = tempfile::tempdir().unwrap();
        let config = WorkerConfig::embedded(root.path().to_path_buf(), "internal-debug");

        let worker = StoreWorker::new(config.clone()).spawn().unwrap();
        assert_eq!(
            worker.status(),
            WorkerStatus::Ready {
                api_version: API_VERSION,
                port: 0,
            }
        );
        worker.ping().unwrap();
        let status = worker.store_status().unwrap();
        assert_eq!(status.api_version, API_VERSION);
        assert_eq!(status.channel, "internal-debug");
        assert!(!status.capabilities.is_empty());
        assert!(status.started_at > 0);
        assert_eq!(worker.join(), WorkerExit::Clean);
        assert!(root.path().join(CATALOG_FILE).is_file());

        let restarted = StoreWorker::new(config).spawn().unwrap();
        restarted.ping().unwrap();
        assert_eq!(restarted.join(), WorkerExit::Clean);
    }

    #[test]
    fn mobile_worker_owns_store_behind_its_typed_port_without_a_listener() {
        let root = tempfile::tempdir().unwrap();
        let worker = StoreWorker::new(WorkerConfig::mobile(
            root.path().to_path_buf(),
            "mobile-test",
        ))
        .spawn()
        .unwrap();
        assert_eq!(
            worker.status(),
            WorkerStatus::Ready {
                api_version: API_VERSION,
                port: 0,
            }
        );

        let client = worker.client();
        let profile = client
            .with_store(|store| {
                store.create_profile(
                    "Mobile",
                    None,
                    None,
                    koru_store_core::Locale::parse("en").unwrap(),
                    Timestamp::from_millis(1),
                )
            })
            .unwrap()
            .unwrap();
        let names = client
            .with_store(|store| {
                store
                    .profiles()
                    .map(|profiles| profiles.into_iter().map(|row| row.name).collect::<Vec<_>>())
            })
            .unwrap()
            .unwrap();
        assert_eq!(names, vec![profile.name]);

        assert_eq!(worker.join(), WorkerExit::Clean);
    }

    #[test]
    fn root_claim_precedes_every_database_open() {
        let root = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let _claim = StoreRootClaim::acquire("internal-debug", root.path()).unwrap();

        let error = StoreWorker::new(WorkerConfig::embedded(
            root.path().to_path_buf(),
            "internal-debug",
        ))
        .spawn()
        .unwrap_err();

        assert!(matches!(error, WorkerError::Startup(_)));
        assert!(!root.path().join(CATALOG_FILE).exists());
    }

    #[test]
    fn command_admission_is_bounded() {
        let (sender, _receiver) = tokio::sync::mpsc::channel(WORKER_COMMAND_CAPACITY);
        for _ in 0..WORKER_COMMAND_CAPACITY {
            let (reply, _response) = std::sync::mpsc::sync_channel(1);
            sender.try_send(WorkerCommand::Noop(reply)).unwrap();
        }
        let (reply, _response) = std::sync::mpsc::sync_channel(1);
        assert!(matches!(
            sender.try_send(WorkerCommand::Noop(reply)),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_))
        ));
    }

    #[test]
    fn a_poisoned_store_replies_explicitly_without_stopping_the_worker() {
        let root = tempfile::tempdir().unwrap();
        let worker = StoreWorker::new(WorkerConfig::embedded(
            root.path().to_path_buf(),
            "internal-debug",
        ))
        .spawn()
        .unwrap();
        let client = worker.client();

        let first: std::result::Result<(), WorkerError> =
            client.with_store(|_| panic!("poison Store state for the regression test"));
        assert_eq!(first, Err(WorkerError::Stopped));
        assert_eq!(client.with_store(|_| 1), Err(WorkerError::Poisoned));
        worker.ping().unwrap();
        assert_eq!(worker.join(), WorkerExit::Clean);
    }

    #[test]
    fn a_startup_panic_is_reported_instead_of_becoming_false_readiness() {
        let root = tempfile::tempdir().unwrap();
        let mut config = WorkerConfig::embedded(root.path().to_path_buf(), "internal-debug");
        config.panic_after_claim = true;

        let error = StoreWorker::new(config).spawn().unwrap_err();

        assert_eq!(error, WorkerError::Startup("worker panicked".into()));
    }
}
