// SPDX-License-Identifier: MPL-2.0

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use koru_session_profiles::ProfileLabel;
use koru_session_protocol::{
    AttachmentId, ControllerLeaseToken, ControllerState, ErrorCode, SessionFailure, SessionId,
};
use koru_session_runtime::providers::{
    AcpFactory, AcpRoute, CodexFactory, ManagedProviderRegistry,
};
use koru_session_runtime::{
    AgentEventUpdate, AttachmentPathResolver, EnvironmentPolicy, RawOutputUpdate, SessionCreate,
    SessionLaunch, SessionRegistry, SessionResourceLifecycle, SessionRuntimeConfig, SpawnRequest,
    StreamReadError,
};
use koru_session_wire::{
    FRAME_VERSION, FromHost, Greeting, HostHealth, HostHello, LaunchBootstrap, OutputUpdate,
    Request, RequestFrame, Response, ResponseFrame, SubscriptionFrame, SubscriptionId,
    SubscriptionUpdate,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, Semaphore, mpsc};

use crate::lifecycle::LifecycleQueue;
use crate::projects::ProjectRegistry;

/// The first negotiated version that can carry the project verbs.
const PROJECTS_PROTOCOL_VERSION: u32 = 10;
/// The first negotiated version that can carry the per-project scope rewrite.
///
/// A second version constant against the *same* capability, for
/// [`WORKSPACE_MIGRATION_PROTOCOL_VERSION`]'s reason one family over: the
/// `projects` capability says a peer speaks the project family at all, and the
/// version says which of its frames it can read. A v12 peer holding `projects`
/// would answer that `set_project_scope` is unreadable, and the host is what
/// refuses first instead.
const PROJECT_SCOPE_PROTOCOL_VERSION: u32 = 13;
/// The first negotiated version that can carry the workspace-lifecycle verb.
const WORKSPACE_LIFECYCLE_PROTOCOL_VERSION: u32 = 11;
/// The first negotiated version that can carry the project-rescope verb.
///
/// A second version constant against the *same* capability, which is the
/// shape a family gets when it grows a verb: the capability says a peer speaks
/// `workspace-lifecycle` at all, and the version says which of its frames that
/// peer can read. A v11 host offering the capability is handed a rescope it
/// cannot parse otherwise.
const WORKSPACE_MIGRATION_PROTOCOL_VERSION: u32 = 12;
/// The first negotiated version that can carry the delegation family: the
/// non-creating lookup, key retirement, the handover verbs, and the one
/// session-event tag that goes with them. The protocol crate owns the
/// number; the host gates on it beside the `delegation` capability.
const DELEGATION_PROTOCOL_VERSION: u32 = koru_session_protocol::DELEGATION_PROTOCOL_VERSION;

const CONTROL_QUEUE: usize = 64;
const SUBSCRIPTION_QUEUE: usize = 8;
const LOG_FILTER: &str = "warn,koru=info,koru_session_host=info";

#[derive(Debug)]
pub enum HostError {
    Bootstrap(String),
    Bind(String),
    Cleanup(String),
    Runtime(String),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bootstrap(value) => write!(formatter, "invalid Agent bootstrap: {value}"),
            Self::Bind(value) => write!(formatter, "could not bind session endpoint: {value}"),
            Self::Cleanup(value) => write!(formatter, "session startup cleanup failed: {value}"),
            Self::Runtime(value) => formatter.write_str(value),
        }
    }
}

impl std::error::Error for HostError {}

struct HostState {
    registry: SessionRegistry,
    model_providers: Arc<ManagedProviderRegistry>,
    model_slots: Semaphore,
    providers: Arc<koru_session_runtime::providers::ProviderReadinessCache>,
    projects: ProjectRegistry,
    attachments: Arc<koru_session_attachments::AttachmentStore>,
    instance_id: String,
    bootstrap: LaunchBootstrap,
    started: Instant,
    next_subscription: AtomicU64,
    control_slots: Arc<Semaphore>,
    create_replays: Mutex<VecDeque<CreateReplay>>,
    stop: Notify,
    /// The desktop MCP bridge a chat's agent starts beside itself, when the
    /// bridge is installed beside this host (AGENTUX UC16).
    bridge: Option<koru_session_runtime::providers::McpServerLaunch>,
    /// Where the files a tab's CLI reads the same bridge from were written
    /// at start, when they could be (`tab_bridge`).
    tab_bridge_files: Option<PathBuf>,
}

const CREATE_REPLAY_CAPACITY: usize = 1_024;

#[derive(Clone)]
struct CreateReplay {
    key: koru_session_protocol::IdempotencyKey,
    request: koru_session_wire::CreateProfileSession,
    response: koru_session_protocol::SessionSummary,
}

#[derive(Clone)]
struct ConnectionProtocol {
    version: u32,
    attachments: bool,
    quoted_paths: bool,
    profile_launch: bool,
    observer_output: bool,
    observation_cursors: bool,
    managed_sessions: bool,
    managed_events: bool,
    managed_approval: bool,
    presented_attention: bool,
    projects: bool,
    workspace_lifecycle: bool,
    delegation: bool,
    session_origin: bool,
    launch_selection: bool,
    /// The creation generation this connection holds, when it registered
    /// as `ai_control` (protocol v16). Constant for the connection's life;
    /// compared at admission with the registry's current one.
    ai_control: Option<koru_session_protocol::AiControlGeneration>,
    capabilities: koru_session_protocol::CapabilitySet,
}

impl ConnectionProtocol {
    fn dynamic_approvals(&self) -> bool {
        self.version >= 8
            && self.managed_approval
            && self
                .capabilities
                .supports(koru_session_protocol::capability::DYNAMIC_APPROVALS)
    }
}

struct AttachmentResourceLifecycle {
    store: Arc<koru_session_attachments::AttachmentStore>,
    runtime: tokio::runtime::Handle,
}

impl SessionResourceLifecycle for AttachmentResourceLifecycle {
    fn session_started(&self, session_id: SessionId) {
        self.store.session_started(session_id);
    }

    fn session_ended(&self, session_id: SessionId) {
        let store = Arc::clone(&self.store);
        drop(self.runtime.spawn_blocking(move || {
            if let Err(error) = store.session_ended(session_id) {
                tracing::warn!(
                    target: "koru::session_host",
                    code = %error.code,
                    "session attachment cleanup failed"
                );
            }
        }));
    }
}

impl AttachmentPathResolver for AttachmentResourceLifecycle {
    fn resolve(
        &self,
        session_id: SessionId,
        attachment_id: AttachmentId,
    ) -> Result<PathBuf, ErrorCode> {
        self.store
            .path(session_id, attachment_id)
            .map_err(|error| error.code)
    }
}

#[derive(Default)]
struct ConnectionResources {
    leases: Vec<(SessionId, ControllerLeaseToken)>,
    cancels: Vec<Box<dyn Fn() + Send>>,
}

pub async fn serve(
    bootstrap: LaunchBootstrap,
    explicit_root: Option<&Path>,
    parent_pipe: Option<crate::ParentPipe>,
) -> Result<(), HostError> {
    validate_bootstrap(&bootstrap, explicit_root)?;
    let endpoint = crate::endpoint(&bootstrap.segment, explicit_root)
        .map_err(|error| HostError::Bind(error.to_string()))?;
    let listener = koru_ipc::AsyncListener::bind(endpoint)
        .map_err(|error| HostError::Bind(error.to_string()))?;
    validate_owned_paths(&bootstrap)?;
    let _logging = koru_log::init_file(
        &bootstrap.store_directory,
        "koru-session-host.log",
        LOG_FILTER,
    );
    tracing::info!(
        target: "koru::startup",
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        root = %bootstrap.root.display(),
        "session host started"
    );

    let paths = koru_session_paths::SessionPaths::from_resolved_directory(
        bootstrap.sessions_directory.clone(),
    );
    let bridge = chat_bridge(explicit_root);
    let tab_bridge_files = bridge.as_ref().and_then(|bridge| {
        let directory = paths.directory().join("bridge");
        match tab_bridge::write_files(&directory, |program| bridge_for(bridge, program)) {
            Ok(()) => Some(directory),
            Err(error) => {
                tracing::warn!(
                    event = "tab.bridge.unwritable",
                    directory = %directory.display(),
                    %error,
                    "the files a tab's agent reads the bridge from could not be written; \
                     Claude, Qwen and Copilot tabs run without Koru's tools"
                );
                None
            }
        }
    });
    let authority = koru_session_paths::CleanupPredicate::new(true, true)
        .authority()
        .expect("the authenticated root and bound endpoint prove cleanup authority");
    let cleaned = paths
        .cleanup_abandoned_attachments(authority)
        .map_err(|error| HostError::Cleanup(error.to_string()))?;
    tracing::info!(
        target: "koru::session_host",
        removed_entries = cleaned.removed_entries,
        "session host claimed its rooted endpoint"
    );

    let attachments = Arc::new(
        koru_session_attachments::AttachmentStore::new(
            paths.clone(),
            koru_session_attachments::AttachmentLimits::from_env(),
        )
        .map_err(|error| HostError::Runtime(error.message().into()))?,
    );
    let resources = Arc::new(AttachmentResourceLifecycle {
        store: Arc::clone(&attachments),
        runtime: tokio::runtime::Handle::current(),
    });

    // One queue, two publishers: the runtime's session facts and the project
    // registry's two registry facts share the same bounded handoff, so
    // neither performs journal or network I/O on the caller.
    let lifecycle: Arc<dyn koru_session_runtime::SessionLifecycleSink> = Arc::new(
        LifecycleQueue::start(bootstrap.segment.clone(), bootstrap.root.clone()),
    );
    let mut managed_providers = ManagedProviderRegistry::default();
    managed_providers
        .register(
            koru_session_profiles::CODEX_REGISTRATION,
            Arc::new(CodexFactory::new(resources.clone())),
        )
        .expect("built-in provider registrations are valid and unique");
    for (registration, route) in [
        (
            koru_session_profiles::CLAUDE_REGISTRATION,
            AcpRoute::claude_code(),
        ),
        (
            koru_session_profiles::OPENCODE_REGISTRATION,
            AcpRoute::opencode(),
        ),
        (koru_session_profiles::KIMI_REGISTRATION, AcpRoute::kimi()),
        (koru_session_profiles::GROK_REGISTRATION, AcpRoute::grok()),
        (
            koru_session_profiles::ANTIGRAVITY_REGISTRATION,
            AcpRoute::antigravity(),
        ),
    ] {
        managed_providers
            .register(
                registration,
                Arc::new(AcpFactory::new(route, resources.clone())),
            )
            .expect("built-in provider registrations are valid and unique");
    }
    let managed_providers = Arc::new(managed_providers);
    let providers = koru_session_runtime::providers::ProviderReadinessCache::with_probe_policy(
        Arc::clone(&managed_providers),
        bootstrap.sessions_directory.clone(),
        !cfg!(feature = "no-provider-probes"),
    );
    providers.refresh_all();
    let state = Arc::new(HostState {
        model_providers: Arc::clone(&managed_providers),
        model_slots: Semaphore::new(2),
        providers,
        registry: SessionRegistry::new(
            SessionRuntimeConfig::from_env(Arc::clone(&lifecycle))
                .with_resources(resources.clone())
                .with_attachment_paths(resources.clone())
                .with_managed_providers(managed_providers),
        ),
        projects: ProjectRegistry::open(&paths, lifecycle),
        attachments,
        instance_id: uuid::Uuid::new_v4().to_string(),
        bootstrap,
        started: Instant::now(),
        next_subscription: AtomicU64::new(1),
        control_slots: Arc::new(Semaphore::new(16)),
        create_replays: Mutex::new(VecDeque::with_capacity(CREATE_REPLAY_CAPACITY)),
        stop: Notify::new(),
        bridge,
        tab_bridge_files,
    });
    // The shared watcher, in `koru-agent-worker` beside the reader that left
    // this pipe open. A host that cannot start it would never learn that its
    // Agent had gone, so the failure is reported rather than dropped.
    if let Some(parent_pipe) = parent_pipe {
        let parent_state = Arc::clone(&state);
        parent_pipe
            .watch("koru-session parent", move || {
                parent_state.stop.notify_one();
            })
            .map_err(|error| {
                HostError::Runtime(format!("could not watch the Agent bootstrap pipe: {error}"))
            })?;
    }

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok(stream) => {
                        let state = Arc::clone(&state);
                        tokio::task::spawn_local(async move {
                            if koru_ipc::same_user(&stream) {
                                let _ = serve_connection(stream, state).await;
                            }
                        });
                    }
                    Err(error) => tracing::warn!(target: "koru::session_host", %error, "session accept failed"),
                }
            }
            () = state.stop.notified() => {
                tokio::time::sleep(Duration::from_millis(25)).await;
                break;
            }
        }
    }
    state.registry.shutdown();
    Ok(())
}

fn validate_bootstrap(
    bootstrap: &LaunchBootstrap,
    explicit_root: Option<&Path>,
) -> Result<(), HostError> {
    if bootstrap.launch_id.parse::<uuid::Uuid>().is_err()
        || bootstrap.authority().parse::<uuid::Uuid>().is_err()
        || bootstrap.segment != koru_channel::segment()
        || bootstrap.explicit_root != explicit_root.is_some()
    {
        return Err(HostError::Bootstrap(
            "launch id, authority, channel, or root mode does not match".into(),
        ));
    }
    let root = canonical(&bootstrap.root)?;
    let expected_root = canonical(
        explicit_root
            .map(Path::to_path_buf)
            .or_else(koru_channel::channel_root)
            .ok_or_else(|| HostError::Bootstrap("could not resolve the channel root".into()))?
            .as_path(),
    )?;
    if root != expected_root {
        return Err(HostError::Bootstrap("the resolved roots differ".into()));
    }
    Ok(())
}

/// Only after the endpoint is held: creating these directories is a mutation,
/// while a losing host must stand down without touching session-owned state.
fn validate_owned_paths(bootstrap: &LaunchBootstrap) -> Result<(), HostError> {
    let root = canonical(&bootstrap.root)?;
    let lexical_root = normalize_lexically(&bootstrap.root);
    for (label, path) in [
        ("sessions", &bootstrap.sessions_directory),
        ("Store", &bootstrap.store_directory),
    ] {
        let lexical_path = normalize_lexically(path);
        if lexical_path == lexical_root || !lexical_path.starts_with(&lexical_root) {
            return Err(HostError::Bootstrap(format!(
                "the resolved {label} directory is outside the selected root"
            )));
        }
        std::fs::create_dir_all(path).map_err(|error| HostError::Bootstrap(error.to_string()))?;
        let path = canonical(path)?;
        if path == root || !path.starts_with(&root) {
            return Err(HostError::Bootstrap(format!(
                "the resolved {label} directory is outside the selected root"
            )));
        }
    }
    Ok(())
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn canonical(path: &Path) -> Result<PathBuf, HostError> {
    std::fs::canonicalize(path).map_err(|error| HostError::Bootstrap(error.to_string()))
}

async fn serve_connection(
    mut stream: impl AsyncRead + AsyncWrite + Unpin + 'static,
    state: Arc<HostState>,
) -> Result<(), HostError> {
    let hello = tokio::time::timeout(
        Duration::from_millis(koru_session_wire::HANDSHAKE_TIMEOUT_MS),
        read_frame::<_, koru_session_wire::ClientHello>(&mut stream),
    )
    .await
    .map_err(|_| HostError::Runtime("session handshake timed out".into()))?
    .map_err(|error| HostError::Runtime(error.to_string()))?;
    let (frame_version, hello) = hello;
    if frame_version != FRAME_VERSION {
        return Ok(());
    }
    let agreement = match koru_session_protocol::negotiate(&hello.offer, &crate::protocol_offer()) {
        Ok(agreement) => agreement,
        Err(refusal) => {
            write_frame(&mut stream, &Greeting::Refused { refusal })
                .await
                .map_err(|error| HostError::Runtime(error.to_string()))?;
            return Ok(());
        }
    };
    let protocol = ConnectionProtocol {
        version: agreement.version,
        attachments: agreement
            .capabilities
            .supports(koru_session_protocol::capability::ATTACHMENTS),
        quoted_paths: agreement
            .capabilities
            .supports(koru_session_protocol::capability::QUOTED_PATH_DELIVERY),
        profile_launch: agreement
            .capabilities
            .supports(koru_session_protocol::capability::PROFILE_LAUNCH),
        observer_output: agreement
            .capabilities
            .supports(koru_session_protocol::capability::OBSERVER_OUTPUT),
        observation_cursors: agreement
            .capabilities
            .supports(koru_session_protocol::capability::OBSERVATION_CURSORS),
        managed_sessions: agreement
            .capabilities
            .supports(koru_session_protocol::capability::MANAGED_SESSIONS),
        managed_events: agreement
            .capabilities
            .supports(koru_session_protocol::capability::MANAGED_EVENTS),
        managed_approval: agreement
            .capabilities
            .supports(koru_session_protocol::capability::MANAGED_APPROVAL),
        presented_attention: agreement
            .capabilities
            .supports(koru_session_protocol::capability::PRESENTED_ATTENTION),
        projects: agreement
            .capabilities
            .supports(koru_session_protocol::capability::PROJECTS),
        workspace_lifecycle: agreement
            .capabilities
            .supports(koru_session_protocol::capability::WORKSPACE_LIFECYCLE),
        delegation: agreement
            .capabilities
            .supports(koru_session_protocol::capability::DELEGATION),
        session_origin: agreement.version >= koru_session_protocol::SESSION_ORIGIN_PROTOCOL_VERSION
            && agreement
                .capabilities
                .supports(koru_session_protocol::capability::SESSION_ORIGIN),
        launch_selection: agreement.version
            >= koru_session_protocol::SESSION_ORIGIN_PROTOCOL_VERSION
            && agreement
                .capabilities
                .supports(koru_session_protocol::capability::LAUNCH_SELECTION),
        ai_control: None,
        capabilities: agreement.capabilities.clone(),
    };
    // An `ai_control` hello mints the next creation generation under the
    // creation-admission mutex, so a successor's registration and an older
    // worker's create cannot interleave (protocol v16). A peer that did not
    // negotiate the family asked for a role the host cannot honour; it is
    // refused rather than left believing it may create.
    let protocol = match hello.role {
        Some(koru_session_protocol::ClientRole::AiControl) => {
            if !protocol.session_origin {
                write_frame(
                    &mut stream,
                    &Greeting::Refused {
                        refusal: koru_session_protocol::ProtocolRefusal {
                            client: hello.offer.versions,
                            server: crate::protocol_offer().versions,
                            error: failure(ErrorCode::CapabilityNotNegotiated),
                        },
                    },
                )
                .await
                .map_err(|error| HostError::Runtime(error.to_string()))?;
                return Ok(());
            }
            let generation = {
                let _admission = state
                    .create_replays
                    .lock()
                    .unwrap_or_else(|held| held.into_inner());
                state.registry.register_ai_control()
            };
            tracing::info!(
                target: "koru::session_host",
                generation = generation.get(),
                "ai control connection registered"
            );
            ConnectionProtocol {
                ai_control: Some(generation),
                ..protocol
            }
        }
        None => protocol,
    };
    write_frame(
        &mut stream,
        &Greeting::Welcome {
            hello: HostHello {
                product: "koru-session-host".into(),
                product_version: env!("CARGO_PKG_VERSION").into(),
                instance_id: state.instance_id.clone(),
                supervisor_launch_id: state.bootstrap.launch_id.clone(),
                agreement,
            },
        },
    )
    .await
    .map_err(|error| HostError::Runtime(error.to_string()))?;

    let (mut reader, writer) = tokio::io::split(stream);
    let (control_send, control_receive) = mpsc::channel(CONTROL_QUEUE);
    let (subscription_send, subscription_receive) = mpsc::channel(SUBSCRIPTION_QUEUE);
    let writer_task = tokio::task::spawn_local(writer_loop(
        writer,
        control_receive,
        subscription_receive,
        protocol.dynamic_approvals(),
    ));
    let resources = Arc::new(Mutex::new(ConnectionResources::default()));

    loop {
        let (version, frame) = match read_frame::<_, RequestFrame>(&mut reader).await {
            Ok(value) => value,
            Err(_) => break,
        };
        if version != FRAME_VERSION || frame.id == 0 {
            break;
        }
        let permit = Arc::clone(&state.control_slots)
            .acquire_owned()
            .await
            .map_err(|error| HostError::Runtime(error.to_string()))?;
        let request_state = Arc::clone(&state);
        let product = hello.product.clone();
        let request_subscriptions = subscription_send.clone();
        let request_resources = Arc::clone(&resources);
        let request_protocol = protocol.clone();
        let (result, stop, disconnect) = tokio::task::spawn_blocking(move || {
            let answer = handle_request(
                &request_state,
                &product,
                frame.request,
                &request_subscriptions,
                &request_resources,
                &request_protocol,
            );
            drop(permit);
            answer
        })
        .await
        .map_err(|error| HostError::Runtime(format!("session control worker failed: {error}")))?;
        if control_send
            .send(FromHost::Response {
                frame: ResponseFrame {
                    id: frame.id,
                    result,
                },
            })
            .await
            .is_err()
        {
            break;
        }
        if stop || disconnect {
            if stop {
                state.stop.notify_one();
            }
            break;
        }
    }

    let resources = std::mem::take(&mut *resources.lock().unwrap_or_else(|held| held.into_inner()));
    for cancel in resources.cancels {
        cancel();
    }
    for (session_id, lease) in resources.leases {
        if let Ok(session) = state.registry.get(session_id) {
            let _ = session.detach(lease);
        }
    }
    drop(control_send);
    drop(subscription_send);
    let _ = writer_task.await;
    Ok(())
}

fn handle_request(
    state: &Arc<HostState>,
    product: &str,
    request: Request,
    subscriptions: &mpsc::Sender<FromHost>,
    resources: &Mutex<ConnectionResources>,
    protocol: &ConnectionProtocol,
) -> (Result<Response, SessionFailure>, bool, bool) {
    let stop_requested = matches!(
        &request,
        Request::StopHost { authority }
            if authority == state.bootstrap.authority() || product == "koru-session-host-cli"
    );
    let disconnect_requested = matches!(&request, Request::Disconnect);
    let result = match request {
        Request::Disconnect => Ok(Response::Ok),
        Request::Health => {
            let (catalog_generation, sessions) = state.registry.catalog();
            Ok(Response::Health {
                health: HostHealth {
                    instance_id: state.instance_id.clone(),
                    supervisor_launch_id: state.bootstrap.launch_id.clone(),
                    session_count: sessions.len().try_into().unwrap_or(u32::MAX),
                    controlled_count: sessions
                        .iter()
                        .filter(|session| session.controller == ControllerState::Controlled)
                        .count()
                        .try_into()
                        .unwrap_or(u32::MAX),
                    catalog_generation,
                    uptime_ms: state
                        .started
                        .elapsed()
                        .as_millis()
                        .try_into()
                        .unwrap_or(u64::MAX),
                },
            })
        }
        Request::ListSessions => Ok(Response::Sessions {
            sessions: state.projects.decorate(state.registry.list()),
        }),
        Request::Catalog => {
            if protocol.version < 3 {
                Err(failure(ErrorCode::ProtocolIncompatible))
            } else {
                let (generation, sessions) = state.registry.catalog();
                Ok(Response::Catalog {
                    catalog: koru_session_wire::SessionCatalog {
                        generation,
                        sessions: state.projects.decorate(sessions),
                    },
                })
            }
        }
        Request::PrepareShutdown {
            authority,
            observed_generation,
        } => {
            if protocol.version < 3 {
                Err(failure(ErrorCode::ProtocolIncompatible))
            } else if authority != state.bootstrap.authority() {
                Err(failure(ErrorCode::Forbidden))
            } else {
                state
                    .registry
                    .prepare_shutdown(observed_generation)
                    .map(|(generation, sessions)| Response::Catalog {
                        catalog: koru_session_wire::SessionCatalog {
                            generation,
                            sessions: state.projects.decorate(sessions),
                        },
                    })
                    .map_err(failure)
            }
        }
        Request::CancelShutdown { authority } => {
            if protocol.version < 3 {
                Err(failure(ErrorCode::ProtocolIncompatible))
            } else if authority != state.bootstrap.authority() {
                Err(failure(ErrorCode::Forbidden))
            } else {
                state.registry.cancel_shutdown();
                Ok(Response::Ok)
            }
        }
        Request::CreateSession { create } => {
            let requested_project = create.project_id;
            let parent_session_id = create.parent_session_id;
            if requested_project.is_some() && !projects_negotiated(protocol) {
                Err(failure(ErrorCode::CapabilityNotNegotiated))
            } else {
                map_create(create, protocol.quoted_paths, &protocol.capabilities).and_then(
                    |create| {
                        let placement = state
                            .projects
                            .place_at_create(requested_project, parent_session_id, &|| {
                                state.registry.list()
                            })
                            .map_err(failure)?;
                        let session = state.registry.create(create)?;
                        let summary = session.summary().with_project(placement.project());
                        placement.commit(summary.id);
                        Ok(Response::Created { session: summary })
                    },
                )
            }
        }
        Request::CreateProfileSession { create } => {
            let managed_without_capability = matches!(
                &create.mode,
                koru_session_protocol::SessionMode::Managed { .. }
            ) && (protocol.version < 8
                || !protocol.managed_sessions);
            let placed_without_capability =
                create.project_id.is_some() && !projects_negotiated(protocol);
            // Managed delegation requires the recovery capability at
            // admission: a provider spawned for a peer that cannot look it
            // up again could never be recovered without spawning twice.
            let delegated_without_capability =
                create.delegation.is_some() && !delegation_negotiated(protocol);
            // A chat origin needs the family that can look it up again, and
            // an explicit selection needs the family that promises to honour
            // it; an older peer would have had both ignored (protocol v16).
            let origin_without_capability =
                !matches!(create.origin, koru_session_protocol::SessionOrigin::Manual)
                    && !protocol.session_origin;
            let selection_without_capability = create
                .selection
                .as_ref()
                .is_some_and(koru_session_protocol::LaunchSelection::is_explicit)
                && !protocol.launch_selection;
            let resume_without_capability = create.resume
                && !protocol
                    .capabilities
                    .supports(koru_session_protocol::capability::LAUNCH_RESUME);
            if protocol.version < 6
                || origin_without_capability
                || selection_without_capability
                || resume_without_capability
                || !protocol.profile_launch
                || managed_without_capability
                || placed_without_capability
                || delegated_without_capability
                || (create
                    .delegation
                    .as_ref()
                    .is_some_and(|launch| launch.model.is_some())
                    && !models_negotiated(protocol))
            {
                Err(failure(ErrorCode::CapabilityNotNegotiated))
            } else {
                create_profile_session(state, create, protocol)
                    .map(|session| Response::Created { session })
            }
        }
        Request::IngestAttachment { upload } => {
            if let Err(error) = require_attachments(protocol.version, protocol.attachments) {
                Err(error)
            } else {
                with_session(state, upload.session_id, |session| {
                    session
                        .can_ingest_attachment(upload.controller_lease)
                        .map_err(failure)?;
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(upload.data_base64)
                        .map_err(|_| failure(ErrorCode::InvalidRequest))?;
                    let attachment = state
                        .attachments
                        .ingest(koru_session_attachments::IngestRequest {
                            session_id: upload.session_id,
                            attachment_id: upload.attachment_id,
                            source: upload.source,
                            bytes,
                            created_at_ms: crate::now_millis(),
                        })
                        .map_err(attachment_failure)?;
                    let attachment =
                        match session.add_attachment(upload.controller_lease, attachment) {
                            Ok(attachment) => attachment,
                            Err(error) => {
                                let _ = state
                                    .attachments
                                    .remove(upload.session_id, upload.attachment_id);
                                return Err(failure(error));
                            }
                        };
                    Ok(Response::AttachmentIngested { attachment })
                })
            }
        }
        Request::ListAttachments { session_id } => {
            if let Err(error) = require_attachments(protocol.version, protocol.attachments) {
                Err(error)
            } else {
                with_session(state, session_id, |session| {
                    Ok(Response::Attachments {
                        attachments: session.attachments(),
                    })
                })
            }
        }
        Request::GetAttachmentPreview {
            session_id,
            attachment_id,
            max_edge,
        } => {
            if let Err(error) = require_attachments(protocol.version, protocol.attachments) {
                Err(error)
            } else {
                with_session(state, session_id, |_| {
                    let preview = state
                        .attachments
                        .preview(session_id, attachment_id, max_edge)
                        .map_err(attachment_failure)?;
                    Ok(Response::AttachmentPreview {
                        preview: koru_session_wire::AttachmentPreview {
                            attachment_id: preview.attachment_id,
                            dimensions: preview.dimensions,
                            media_type: "image/png".into(),
                            data_base64: base64::engine::general_purpose::STANDARD
                                .encode(preview.bytes),
                        },
                    })
                })
            }
        }
        Request::RemoveAttachment {
            session_id,
            controller_lease,
            attachment_id,
        } => {
            if let Err(error) = require_attachments(protocol.version, protocol.attachments) {
                Err(error)
            } else {
                with_session(state, session_id, |session| {
                    session
                        .can_ingest_attachment(controller_lease)
                        .map_err(failure)?;
                    state
                        .attachments
                        .remove(session_id, attachment_id)
                        .map_err(attachment_failure)?;
                    let attachment = session
                        .remove_attachment(controller_lease, attachment_id)
                        .map_err(failure)?;
                    Ok(Response::AttachmentRemoved { attachment })
                })
            }
        }
        Request::SendPendingInput {
            session_id,
            controller_lease,
            input,
        } => {
            if protocol.version < 5 {
                Err(failure(ErrorCode::ProtocolIncompatible))
            } else if let Err(error) = require_attachments(protocol.version, protocol.attachments) {
                Err(error)
            } else {
                with_session(state, session_id, |session| {
                    session
                        .send_pending_input(controller_lease, input)
                        .map(|result| Response::PendingInputSent { result })
                        .map_err(failure)
                })
            }
        }
        Request::DiscoverProviderModels {
            provider,
            working_directory,
        } => {
            if !models_negotiated(protocol) {
                Err(failure(ErrorCode::CapabilityNotNegotiated))
            } else if !working_directory.is_absolute() {
                Err(failure(ErrorCode::InvalidRequest))
            } else if let Ok(_slot) = state.model_slots.try_acquire() {
                state
                    .model_providers
                    .discover_models(&provider, working_directory)
                    .map(|models| Response::ProviderModels { models })
                    .map_err(|error| failure(error.code))
            } else {
                Err(failure(ErrorCode::Backpressure))
            }
        }
        Request::SubscribeProviders => {
            if protocol.version < 8
                || !protocol
                    .capabilities
                    .supports(koru_session_protocol::capability::PROVIDER_READINESS)
            {
                Err(failure(ErrorCode::CapabilityNotNegotiated))
            } else {
                next_subscription(state).map(|id| {
                    let (providers, receiver) = state.providers.subscribe();
                    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let cancel = Arc::clone(&cancelled);
                    resources
                        .lock()
                        .unwrap_or_else(|held| held.into_inner())
                        .cancels
                        .push(Box::new(move || cancel.store(true, Ordering::Release)));
                    forward(id, subscriptions.clone(), move || {
                        loop {
                            if cancelled.load(Ordering::Acquire) {
                                return None;
                            }
                            match receiver.recv_timeout(Duration::from_millis(250)) {
                                Ok(provider) => {
                                    return Some(SubscriptionUpdate::Providers { provider });
                                }
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                                    return Some(SubscriptionUpdate::Closed {
                                        error: Some(failure(ErrorCode::StreamGap)),
                                    });
                                }
                            }
                        }
                    });
                    Response::ProvidersSubscribed {
                        subscription_id: id,
                        providers,
                    }
                })
            }
        }
        Request::RefreshProvider { kind } => {
            if protocol.version < 8
                || !protocol
                    .capabilities
                    .supports(koru_session_protocol::capability::PROVIDER_READINESS)
            {
                Err(failure(ErrorCode::CapabilityNotNegotiated))
            } else {
                state
                    .providers
                    .refresh(kind)
                    .map(|()| Response::Ok)
                    .map_err(failure)
            }
        }
        Request::AnswerApproval {
            session_id,
            controller_lease,
            idempotency_key,
            approval_id,
            decision,
            choice,
        } => {
            if protocol.version < 8
                || !protocol.managed_approval
                || (choice.is_some() && !protocol.dynamic_approvals())
            {
                Err(failure(ErrorCode::CapabilityNotNegotiated))
            } else {
                with_session(state, session_id, |session| {
                    session
                        .answer_approval(
                            controller_lease,
                            idempotency_key,
                            approval_id,
                            decision,
                            choice,
                        )
                        .map_err(failure)?;
                    Ok(Response::Ok)
                })
            }
        }
        Request::CancelTurn {
            session_id,
            controller_lease,
            idempotency_key,
            turn_id,
            reason,
        } => {
            if protocol.version < 9
                || !protocol
                    .capabilities
                    .supports(koru_session_protocol::capability::MANAGED_CANCEL)
            {
                Err(failure(ErrorCode::CapabilityNotNegotiated))
            } else {
                with_session(state, session_id, |session| {
                    session
                        .cancel_turn(controller_lease, idempotency_key, turn_id, reason)
                        .map_err(failure)?;
                    Ok(Response::Ok)
                })
            }
        }
        Request::AcknowledgeAttention {
            session_id,
            controller_lease,
            through,
        } => {
            if protocol.version < 9 || !protocol.presented_attention {
                Err(failure(ErrorCode::CapabilityNotNegotiated))
            } else {
                with_session(state, session_id, |session| {
                    session
                        .acknowledge_attention(controller_lease, through)
                        .map_err(failure)?;
                    Ok(Response::Ok)
                })
            }
        }
        Request::Attach {
            session_id,
            viewport_generation,
            columns,
            overscan_rows,
        } => with_session(state, session_id, |session| {
            let attachment = session
                .attach_live(
                    viewport_generation,
                    columns,
                    overscan_rows,
                    crate::now_millis(),
                )
                .map_err(failure)?;
            let id = next_subscription(state)?;
            let cancel = attachment.render.cancel_handle();
            let mut owned = resources.lock().unwrap_or_else(|held| held.into_inner());
            owned.leases.push((session_id, attachment.lease.token));
            owned.cancels.push(Box::new(move || cancel.cancel()));
            drop(owned);
            forward_render(id, attachment.render, subscriptions.clone());
            Ok(Response::Attached {
                lease: attachment.lease,
                subscription_id: id,
            })
        }),
        Request::Detach { session_id, lease } => with_session(state, session_id, |session| {
            session.detach(lease).map_err(failure)?;
            resources
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .leases
                .retain(|current| *current != (session_id, lease));
            Ok(Response::Ok)
        }),
        Request::SuspendRender { session_id, lease } => {
            with_session(state, session_id, |session| {
                session.suspend_render(lease).map_err(failure)?;
                Ok(Response::Ok)
            })
        }
        Request::SetViewport {
            session_id,
            lease,
            viewport,
        } => with_session(state, session_id, |session| {
            let render = session.set_viewport(lease, viewport).map_err(failure)?;
            let id = next_subscription(state)?;
            let cancel = render.cancel_handle();
            resources
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .cancels
                .push(Box::new(move || cancel.cancel()));
            forward_render(id, render, subscriptions.clone());
            Ok(Response::Subscribed {
                subscription_id: id,
            })
        }),
        Request::RecoverSnapshot { session_id, lease } => {
            with_session(state, session_id, |session| {
                session.recover_snapshot(lease).map_err(failure)?;
                Ok(Response::Ok)
            })
        }
        Request::Snapshot {
            session_id,
            viewport_generation,
            range,
            columns,
        } => {
            if protocol.version < 6 || !protocol.observer_output {
                Err(failure(ErrorCode::CapabilityNotNegotiated))
            } else {
                with_session(state, session_id, |session| {
                    session
                        .snapshot(viewport_generation, range, columns)
                        .map(|snapshot| Response::Snapshot { snapshot })
                        .map_err(failure)
                })
            }
        }
        Request::ObserveSnapshot {
            session_id,
            viewport_generation,
            range,
            columns,
        } => {
            if protocol.version < 7 || !protocol.observation_cursors {
                Err(failure(ErrorCode::CapabilityNotNegotiated))
            } else {
                with_session(state, session_id, |session| {
                    session
                        .observe_snapshot(viewport_generation, range, columns)
                        .map(|observation| Response::ObservedSnapshot { observation })
                        .map_err(failure)
                })
            }
        }
        Request::ObserveAgentEvents { session_id } => {
            if protocol.version < 9 || !protocol.managed_events {
                Err(failure(ErrorCode::CapabilityNotNegotiated))
            } else {
                with_session(state, session_id, |session| {
                    session
                        .observe_agent_events()
                        .map(|observation| Response::AgentEventsObserved { observation })
                        .map_err(failure)
                })
            }
        }
        Request::ScrollbackPage {
            session_id,
            lease,
            terminal_generation,
            viewport_generation,
            range,
        } => with_session(state, session_id, |session| {
            session
                .scrollback_page(lease, terminal_generation, viewport_generation, range)
                .map(|page| Response::ScrollbackPage { page })
                .map_err(failure)
        }),
        Request::ObserveScrollbackPage {
            session_id,
            terminal_generation,
            viewport_generation,
            range,
        } => {
            if protocol.version < 6 || !protocol.observer_output {
                Err(failure(ErrorCode::CapabilityNotNegotiated))
            } else {
                with_session(state, session_id, |session| {
                    session
                        .observe_scrollback_page(terminal_generation, viewport_generation, range)
                        .map(|page| Response::ScrollbackPage { page })
                        .map_err(failure)
                })
            }
        }
        Request::Command { command } => {
            if command.action.minimum_protocol_version() > protocol.version {
                Err(failure(ErrorCode::ProtocolIncompatible))
            } else {
                with_session(state, command.session_id, |session| {
                    session.command(command).map_err(failure)?;
                    Ok(Response::Ok)
                })
            }
        }
        Request::SubscribeEvents { session_id } => with_session(state, session_id, |session| {
            let id = next_subscription(state)?;
            let events = session.subscribe_events();
            let cancel = events.cancel_handle();
            resources
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .cancels
                .push(Box::new(move || cancel.cancel()));
            forward_events(id, events, subscriptions.clone(), protocol.version);
            Ok(Response::Subscribed {
                subscription_id: id,
            })
        }),
        Request::SubscribeEventsFrom { session_id, after } => {
            if protocol.version < 7 || !protocol.observation_cursors {
                Err(failure(ErrorCode::CapabilityNotNegotiated))
            } else {
                with_session(state, session_id, |session| {
                    let id = next_subscription(state)?;
                    let events = session.subscribe_events_from(after);
                    let cancel = events.cancel_handle();
                    resources
                        .lock()
                        .unwrap_or_else(|held| held.into_inner())
                        .cancels
                        .push(Box::new(move || cancel.cancel()));
                    forward_events(id, events, subscriptions.clone(), protocol.version);
                    Ok(Response::Subscribed {
                        subscription_id: id,
                    })
                })
            }
        }
        Request::SubscribeOutput { session_id } => with_session(state, session_id, |session| {
            let id = next_subscription(state)?;
            let output = session.subscribe_output();
            let cancel = output.cancel_handle();
            resources
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .cancels
                .push(Box::new(move || cancel.cancel()));
            forward_output(id, output, subscriptions.clone());
            Ok(Response::Subscribed {
                subscription_id: id,
            })
        }),
        Request::SubscribeOutputFrom { session_id, after } => {
            if protocol.version < 7 || !protocol.observation_cursors {
                Err(failure(ErrorCode::CapabilityNotNegotiated))
            } else {
                with_session(state, session_id, |session| {
                    let id = next_subscription(state)?;
                    let output = session.subscribe_output_from(after);
                    let cancel = output.cancel_handle();
                    resources
                        .lock()
                        .unwrap_or_else(|held| held.into_inner())
                        .cancels
                        .push(Box::new(move || cancel.cancel()));
                    forward_output(id, output, subscriptions.clone());
                    Ok(Response::Subscribed {
                        subscription_id: id,
                    })
                })
            }
        }
        Request::SubscribeAgentEventsFrom { session_id, after } => {
            if protocol.version < 8 || !protocol.managed_events {
                Err(failure(ErrorCode::CapabilityNotNegotiated))
            } else {
                with_session(state, session_id, |session| {
                    if !matches!(
                        session.summary().mode,
                        koru_session_protocol::SessionMode::Managed { .. }
                    ) {
                        return Err(failure(ErrorCode::CapabilityNotNegotiated));
                    }
                    let id = next_subscription(state)?;
                    let events = session.subscribe_agent_events_from(after);
                    let cancel = events.cancel_handle();
                    resources
                        .lock()
                        .unwrap_or_else(|held| held.into_inner())
                        .cancels
                        .push(Box::new(move || cancel.cancel()));
                    forward_agent_events(id, events, subscriptions.clone());
                    Ok(Response::Subscribed {
                        subscription_id: id,
                    })
                })
            }
        }
        Request::ListProjects => require_projects(protocol).map(|()| Response::Projects {
            projects: state.projects.list(),
        }),
        Request::CreateProject { create } => require_projects(protocol).and_then(|()| {
            state
                .projects
                .create(create, crate::now_millis())
                .map(|project| Response::Project { project })
                .map_err(failure)
        }),
        Request::RenameProject { project_id, name } => require_projects(protocol).and_then(|()| {
            state
                .projects
                .rename(project_id, name)
                .map(|project| Response::Project { project })
                .map_err(failure)
        }),
        Request::SetProjectRoot {
            project_id,
            root_directory,
        } => require_projects(protocol).and_then(|()| {
            state
                .projects
                .set_root(project_id, root_directory)
                .map(|project| Response::Project { project })
                .map_err(failure)
        }),
        Request::SetProjectScope {
            project_id,
            workspace_scope,
        } => require_project_scope(protocol).and_then(|()| {
            state
                .projects
                .set_scope(project_id, workspace_scope)
                .map(|project| Response::Project { project })
                .map_err(failure)
        }),
        Request::DeleteProject { project_id } => require_projects(protocol).and_then(|()| {
            state
                .projects
                .delete(project_id, &|| state.registry.list())
                .map(|()| Response::Ok)
                .map_err(failure)
        }),
        Request::AssignProject {
            session_id,
            project_id,
        } => require_projects(protocol).and_then(|()| {
            state
                .projects
                .assign(session_id, project_id, &|| state.registry.list())
                .map(|()| Response::Ok)
                .map_err(failure)
        }),
        Request::SubscribeProjects => require_projects(protocol).and_then(|()| {
            let id = next_subscription(state)?;
            let events = state.projects.subscribe(id);
            let owner = Arc::clone(state);
            resources
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .cancels
                .push(Box::new(move || owner.projects.unsubscribe(id)));
            forward_catalog(id, events, subscriptions.clone());
            Ok(Response::Subscribed {
                subscription_id: id,
            })
        }),
        Request::TerminateWorkspaceSessions { authority, scope } => {
            require_workspace_lifecycle(protocol)
                .and_then(|()| terminate_workspace_sessions(state, &authority, scope))
        }
        Request::RescopeWorkspaceProjects {
            authority,
            workspace,
            profile,
        } => require_workspace_migration(protocol)
            .and_then(|()| rescope_workspace_projects(state, &authority, workspace, profile)),
        Request::LookupCreation { idempotency_key } => {
            require_delegation(protocol).map(|()| Response::CreationLookup {
                instance_id: state.instance_id.clone(),
                lookup: lookup_creation(state, idempotency_key),
            })
        }
        Request::LookupOrigin { origin } => {
            if !protocol.session_origin {
                Err(failure(ErrorCode::CapabilityNotNegotiated))
            } else {
                let mut lookup = state.registry.lookup_origin(origin);
                lookup.live = state.projects.decorate(lookup.live);
                Ok(Response::OriginLookup {
                    instance_id: state.instance_id.clone(),
                    lookup,
                })
            }
        }
        Request::RetireCreation { idempotency_key } => require_delegation(protocol).map(|()| {
            // Retire under the registry lock, then answer with what the key
            // held at that moment: a late create is fenced from here on,
            // and a caller that finds the key live reattaches instead.
            let lookup = decorate_lookup(state, state.registry.retire_creation(idempotency_key));
            Response::CreationRetired {
                instance_id: state.instance_id.clone(),
                lookup,
            }
        }),
        Request::TerminateDelegated { session_id } => require_delegation(protocol).and_then(|()| {
            with_session(state, session_id, |session| {
                session.terminate_for_delegation().map_err(failure)?;
                Ok(Response::Ok)
            })
        }),
        Request::RequestControl {
            session_id,
            urgency,
        } => require_delegation(protocol).and_then(|()| {
            with_session(state, session_id, |session| {
                session
                    .request_control(urgency)
                    .map(|outcome| Response::ControlRequested { outcome })
                    .map_err(failure)
            })
        }),
        Request::ReleaseControl { session_id, lease } => {
            require_delegation(protocol).and_then(|()| {
                with_session(state, session_id, |session| {
                    session.release_control(lease).map_err(failure)?;
                    resources
                        .lock()
                        .unwrap_or_else(|held| held.into_inner())
                        .leases
                        .retain(|current| *current != (session_id, lease));
                    Ok(Response::Ok)
                })
            })
        }
        Request::ReturnControl { session_id, lease } => {
            require_delegation(protocol).and_then(|()| {
                with_session(state, session_id, |session| {
                    session.return_control(lease).map_err(failure)?;
                    resources
                        .lock()
                        .unwrap_or_else(|held| held.into_inner())
                        .leases
                        .retain(|current| *current != (session_id, lease));
                    Ok(Response::Ok)
                })
            })
        }
        Request::StopHost { authority } => {
            if authority == state.bootstrap.authority() || product == "koru-session-host-cli" {
                Ok(Response::Ok)
            } else {
                Err(failure(ErrorCode::Forbidden))
            }
        }
    };
    let stop = stop_requested && result.is_ok();
    let disconnect = disconnect_requested && result.is_ok();
    (result, stop, disconnect)
}

fn with_session(
    state: &HostState,
    id: SessionId,
    action: impl FnOnce(koru_session_runtime::SessionHandle) -> Result<Response, SessionFailure>,
) -> Result<Response, SessionFailure> {
    state.registry.get(id).map_err(failure).and_then(action)
}

/// Both halves of the project gate: the negotiated version and the negotiated
/// capability. A peer that has one without the other has neither.
fn projects_negotiated(protocol: &ConnectionProtocol) -> bool {
    protocol.version >= PROJECTS_PROTOCOL_VERSION && protocol.projects
}

fn require_projects(protocol: &ConnectionProtocol) -> Result<(), SessionFailure> {
    projects_negotiated(protocol)
        .then_some(())
        .ok_or_else(|| failure(ErrorCode::CapabilityNotNegotiated))
}

/// The project gate one version further on. The capability is the family's and
/// does not change; the version is what a v12 peer does not have.
fn require_project_scope(protocol: &ConnectionProtocol) -> Result<(), SessionFailure> {
    (protocol.version >= PROJECT_SCOPE_PROTOCOL_VERSION && protocol.projects)
        .then_some(())
        .ok_or_else(|| failure(ErrorCode::CapabilityNotNegotiated))
}

/// Both halves of the workspace-lifecycle gate, exactly as the project gate is
/// written: the negotiated version and the negotiated capability.
fn require_workspace_lifecycle(protocol: &ConnectionProtocol) -> Result<(), SessionFailure> {
    (protocol.version >= WORKSPACE_LIFECYCLE_PROTOCOL_VERSION && protocol.workspace_lifecycle)
        .then_some(())
        .ok_or_else(|| failure(ErrorCode::CapabilityNotNegotiated))
}

/// The same two halves one version further on. The capability is the family's
/// and does not change; the version is what a v11 peer does not have.
fn require_workspace_migration(protocol: &ConnectionProtocol) -> Result<(), SessionFailure> {
    (protocol.version >= WORKSPACE_MIGRATION_PROTOCOL_VERSION && protocol.workspace_lifecycle)
        .then_some(())
        .ok_or_else(|| failure(ErrorCode::CapabilityNotNegotiated))
}

/// Both halves of the delegation gate: the negotiated version and the
/// negotiated capability. A peer with one and not the other has neither, and
/// is answered with the stable code rather than a dropped connection.
fn models_negotiated(protocol: &ConnectionProtocol) -> bool {
    protocol.version >= koru_session_protocol::PROVIDER_MODELS_PROTOCOL_VERSION
        && protocol
            .capabilities
            .supports(koru_session_protocol::capability::PROVIDER_MODELS)
}

fn delegation_negotiated(protocol: &ConnectionProtocol) -> bool {
    protocol.version >= DELEGATION_PROTOCOL_VERSION && protocol.delegation
}

fn require_delegation(protocol: &ConnectionProtocol) -> Result<(), SessionFailure> {
    delegation_negotiated(protocol)
        .then_some(())
        .ok_or_else(|| failure(ErrorCode::CapabilityNotNegotiated))
}

/// The non-creating lookup (§7.6). The registry's ledger answers first; a
/// key it has forgotten but the replay ledger still holds is `KnownEnded`
/// from the retained response, because either memory may outlive the other
/// and both are evidence that the creation happened.
fn lookup_creation(
    state: &HostState,
    key: koru_session_protocol::IdempotencyKey,
) -> koru_session_protocol::CreationLookup {
    match state.registry.lookup_creation(key) {
        koru_session_protocol::CreationLookup::NoEvidence => {
            let retained = state
                .create_replays
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .iter()
                .find(|entry| entry.key == key)
                .map(|entry| entry.response.clone());
            match retained {
                Some(session) => koru_session_protocol::CreationLookup::KnownEnded { session },
                None => koru_session_protocol::CreationLookup::NoEvidence,
            }
        }
        lookup => decorate_lookup(state, lookup),
    }
}

fn decorate_one(
    state: &HostState,
    session: koru_session_protocol::SessionSummary,
) -> koru_session_protocol::SessionSummary {
    state
        .projects
        .decorate(vec![session])
        .pop()
        .expect("one row in, one row out")
}

/// Placement is the project registry's answer, stamped on the way out here
/// exactly as it is on a catalogue row.
fn decorate_lookup(
    state: &HostState,
    lookup: koru_session_protocol::CreationLookup,
) -> koru_session_protocol::CreationLookup {
    use koru_session_protocol::CreationLookup;
    let decorate = |session| decorate_one(state, session);
    match lookup {
        CreationLookup::Known { session } => CreationLookup::Known {
            session: decorate(session),
        },
        CreationLookup::KnownEnded { session } => CreationLookup::KnownEnded {
            session: decorate(session),
        },
        other => other,
    }
}

/// Close every live session one Koru workspace owns.
///
/// **Two gates, and the capability is the weaker one.** Negotiating
/// `workspace-lifecycle` says a peer can *speak* this frame — anybody can ask
/// for a capability. What decides is the launch-scoped lifecycle authority,
/// which reaches a process only down an inherited stdin pipe from the Agent
/// that started it, and which is deliberately a different secret from the one
/// `StopHost` takes: the Gateway may end a workspace's terminals and may never
/// end the host.
///
/// An unset authority never matches. A supervisor that minted none leaves the
/// field empty, and an empty presented authority is refused with the same
/// `Forbidden` as a wrong one — a host with no secret must not be a host that
/// accepts every caller who also sends nothing.
///
/// **Idempotent, and it reports what it did rather than what it tried.**
/// Sessions are collected under the project registry's lock and terminated
/// after it is released; a session that exited in between is skipped, so the
/// answer names the terminals this call actually closed. Closing none is `Ok`.
fn terminate_workspace_sessions(
    state: &Arc<HostState>,
    authority: &str,
    scope: koru_session_protocol::WorkspaceScope,
) -> Result<Response, SessionFailure> {
    let expected = state.bootstrap.lifecycle_authority();
    if expected.is_empty() || authority != expected {
        return Err(failure(ErrorCode::Forbidden));
    }
    let affected = state
        .projects
        .sessions_in_scope(scope, &state.registry.list());
    let mut terminated = Vec::new();
    for session_id in affected {
        let Ok(session) = state.registry.get(session_id) else {
            continue;
        };
        if session.terminate_for_lifecycle().unwrap_or(false) {
            terminated.push(session_id);
        }
    }
    Ok(Response::WorkspaceSessionsTerminated { terminated })
}

/// Make every project that names a workspace name the profile that now owns it.
///
/// **The same authority as closing its terminals**, and deliberately so: both
/// are the migration operation acting on one workspace's session-domain state,
/// and a caller that could rescope projects without being able to close
/// terminals could move somebody's projects between personas underneath a
/// running session — which is the thing R8's ordering exists to prevent.
///
/// **Idempotent by shape rather than by bookkeeping.** The request names the
/// state to be in, so a repair after a crash, a repair after the host was down
/// for the move, and a repair nobody needed are the same call. It reports the
/// projects it changed; changing none is `Ok` with an empty list.
fn rescope_workspace_projects(
    state: &Arc<HostState>,
    authority: &str,
    workspace: koru_session_protocol::CatalogWorkspaceId,
    profile: koru_session_protocol::CatalogProfileId,
) -> Result<Response, SessionFailure> {
    let expected = state.bootstrap.lifecycle_authority();
    if expected.is_empty() || authority != expected {
        return Err(failure(ErrorCode::Forbidden));
    }
    state
        .projects
        .rescope_workspace(workspace, profile)
        .map(|rescoped| Response::WorkspaceProjectsRescoped { rescoped })
        .map_err(failure)
}

fn create_profile_session(
    state: &HostState,
    request: koru_session_wire::CreateProfileSession,
    protocol: &ConnectionProtocol,
) -> Result<koru_session_protocol::SessionSummary, SessionFailure> {
    // Keep the bounded replay ledger locked across spawn. Creation is already
    // admitted to the host's fixed blocking lane; serializing this rare edge
    // is what makes two concurrent retries unable to launch two processes.
    let mut replays = state
        .create_replays
        .lock()
        .unwrap_or_else(|held| held.into_inner());
    // The fence comes before the memory: a key a fresh attempt retired is
    // refused whatever the replay ledger remembers about it, because the
    // caller that retired it has already decided nothing may start under it.
    if state.registry.is_creation_retired(request.idempotency_key) {
        return Err(failure(ErrorCode::CreationRetired));
    }
    if let Some(previous) = replays
        .iter()
        .find(|entry| entry.key == request.idempotency_key)
    {
        return if previous.request == request {
            Ok(previous.response.clone())
        } else {
            Err(failure(ErrorCode::IdempotencyConflict))
        };
    }

    // The registry's creation ledger is what the bounded replay ledger above
    // is not: a memory of the key that survives eviction and exists *before*
    // the spawn. A key it already knows is answered without spawning; a key a
    // fresh attempt retired is refused; only a free key is reserved. Still a
    // map insert, still under the same admission mutex.
    let reservation = match state.registry.admit_creation(
        request.idempotency_key,
        request.origin,
        protocol.ai_control,
    ) {
        koru_session_runtime::CreationAdmission::Reserved(reservation) => reservation,
        koru_session_runtime::CreationAdmission::Known(session) => {
            return Ok(decorate_one(state, session.summary()));
        }
        koru_session_runtime::CreationAdmission::Ended(summary) => {
            return Ok(decorate_one(state, *summary));
        }
        koru_session_runtime::CreationAdmission::InFlight => {
            return Err(failure(ErrorCode::Backpressure));
        }
        koru_session_runtime::CreationAdmission::Retired => {
            return Err(failure(ErrorCode::CreationRetired));
        }
        koru_session_runtime::CreationAdmission::StaleAuthority => {
            return Err(failure(ErrorCode::StaleAuthority));
        }
        koru_session_runtime::CreationAdmission::Forbidden => {
            return Err(failure(ErrorCode::Forbidden));
        }
        koru_session_runtime::CreationAdmission::OriginOwned => {
            return Err(failure(ErrorCode::OriginOwned));
        }
    };

    let profile = koru_session_profiles::resolve(&request.profile).map_err(failure)?;
    let display_name = profile.fallback_display_name().to_owned();
    let create = match &request.mode {
        koru_session_protocol::SessionMode::RawPty => {
            if request.delegation.is_some() {
                // Raw PTY is never delegatable (D4): a run whose completion
                // is a guess breaks the feature's only promise.
                return Err(failure(ErrorCode::InvalidRequest));
            }
            let mut attachment_delivery = profile.attachment_delivery;
            if matches!(
                attachment_delivery,
                koru_session_protocol::AttachmentDeliveryPolicy::QuotedPath { .. }
            ) && !protocol.quoted_paths
            {
                attachment_delivery = koru_session_protocol::AttachmentDeliveryPolicy::Unsupported;
            }
            // A tab's agent gets Koru's tools the way a chat's does, in its
            // CLI's per-process form; a shell, a custom profile and a CLI
            // with no such form get exactly their own launch (ADR-470).
            let bridge = (profile.kind == koru_session_protocol::ProfileKind::Agent)
                .then(|| tab_bridge_for(state, profile.label))
                .flatten();
            // **Resume is the agent's own continue form, appended by the host**:
            // a client asks for it by flag and never names an argument. A
            // profile with no such form refuses rather than start fresh under
            // a request that said otherwise.
            let resume: &[&str] = if request.resume {
                if profile.kind != koru_session_protocol::ProfileKind::Agent {
                    return Err(failure(ErrorCode::ProfileIncompatible));
                }
                koru_session_profiles::resume_arguments(profile.label)
                    .ok_or_else(|| failure(ErrorCode::ProfileIncompatible))?
            } else {
                &[]
            };
            let (argv, environment) = raw_launch(&profile, bridge, resume)?;
            SessionCreate {
                profile: profile.id,
                parent_session_id: request.parent_session_id,
                profile_kind: profile.kind,
                display_name,
                working_directory: request.working_directory.clone(),
                attachment_delivery,
                capabilities: protocol.capabilities.clone(),
                launch: SessionLaunch::Raw(SpawnRequest {
                    argv,
                    cwd: request.working_directory.clone(),
                    environment,
                    size: request.size,
                }),
                created_at_ms: crate::now_millis(),
                origin: request.origin,
            }
        }
        koru_session_protocol::SessionMode::Managed { provider } => {
            if profile.provider.as_ref() != Some(provider) {
                return Err(failure(ErrorCode::ProfileIncompatible));
            }
            let executable = koru_session_profiles::managed_provider(provider)
                .map(|registration| registration.executable)
                .or_else(|| profile.argv.first().map(String::as_str))
                .and_then(koru_session_profiles::resolve_executable)
                .ok_or_else(|| failure(ErrorCode::ProfileMissing))?;
            SessionCreate {
                profile: profile.id,
                parent_session_id: request.parent_session_id,
                profile_kind: profile.kind,
                display_name,
                working_directory: request.working_directory.clone(),
                attachment_delivery:
                    koru_session_protocol::AttachmentDeliveryPolicy::NativeStructured,
                capabilities: protocol.capabilities.clone(),
                launch: SessionLaunch::Managed {
                    provider: provider.clone(),
                    executable: executable.into_os_string(),
                    delegation: request.delegation.clone(),
                    selection: request.selection.clone(),
                    // A chat's agent gets Koru's own tools through the
                    // desktop bridge; a terminal's or a run's does not.
                    mcp_servers: match request.origin {
                        koru_session_protocol::SessionOrigin::Chat { .. } => state
                            .bridge
                            .iter()
                            .map(|bridge| bridge_for(bridge, provider.as_str()))
                            .collect(),
                        _ => Vec::new(),
                    },
                },
                created_at_ms: crate::now_millis(),
                origin: request.origin,
            }
        }
    };
    // Placement is decided and held open across the spawn, for the same
    // reason the replay ledger above is: creating the session and recording
    // where it belongs must be one step, or a retry races the placement and
    // every other window sees a session that is briefly in no project.
    let placement = state
        .projects
        .place_at_create(request.project_id, request.parent_session_id, &|| {
            state.registry.list()
        })
        .map_err(failure)?;
    let response = state
        .registry
        .create_reserved(reservation, create)
        .map(|session| session.summary().with_project(placement.project()))?;
    placement.commit(response.id);
    replays.push_back(CreateReplay {
        key: request.idempotency_key,
        request,
        response: response.clone(),
    });
    if replays.len() > CREATE_REPLAY_CAPACITY {
        replays.pop_front();
    }
    Ok(response)
}

fn map_create(
    mut create: koru_session_wire::CreateSession,
    path_delivery_negotiated: bool,
    negotiated_capabilities: &koru_session_protocol::CapabilitySet,
) -> Result<SessionCreate, SessionFailure> {
    if !matches!(create.mode, koru_session_protocol::SessionMode::RawPty) {
        return Err(failure(ErrorCode::CapabilityNotNegotiated));
    }
    create.capabilities = create.capabilities.intersection(negotiated_capabilities);
    if matches!(
        create.attachment_delivery,
        koru_session_protocol::AttachmentDeliveryPolicy::QuotedPath { .. }
    ) && !path_delivery_negotiated
    {
        create.attachment_delivery = koru_session_protocol::AttachmentDeliveryPolicy::Unsupported;
    }
    let environment = match create.environment {
        koru_session_wire::EnvironmentSpec::Inherit => EnvironmentPolicy::Inherit,
        koru_session_wire::EnvironmentSpec::Clean => EnvironmentPolicy::Clean,
        koru_session_wire::EnvironmentSpec::InheritWith { set, remove } => {
            EnvironmentPolicy::InheritWith {
                set: set
                    .into_iter()
                    .map(|(key, value)| (OsString::from(key), OsString::from(value)))
                    .collect::<BTreeMap<_, _>>(),
                remove: remove
                    .into_iter()
                    .map(OsString::from)
                    .collect::<BTreeSet<_>>(),
            }
        }
    };
    let cwd = create.working_directory;
    let spawn = SpawnRequest {
        argv: create.argv.into_iter().map(OsString::from).collect(),
        cwd: cwd.clone(),
        environment,
        size: create.size,
    };
    Ok(SessionCreate {
        profile: create.profile,
        parent_session_id: create.parent_session_id,
        profile_kind: create.profile_kind,
        display_name: create.display_name,
        working_directory: cwd,
        attachment_delivery: create.attachment_delivery,
        capabilities: create.capabilities,
        launch: SessionLaunch::Raw(spawn),
        created_at_ms: crate::now_millis(),
        origin: Default::default(),
    })
}

fn next_subscription(state: &HostState) -> Result<SubscriptionId, SessionFailure> {
    SubscriptionId::new(state.next_subscription.fetch_add(1, Ordering::Relaxed))
        .ok_or_else(|| failure(ErrorCode::Internal))
}

fn forward_render(
    id: SubscriptionId,
    stream: koru_session_runtime::BoundedStream<koru_session_protocol::RenderUpdate>,
    send: mpsc::Sender<FromHost>,
) {
    forward(id, send, move || match stream.recv() {
        Ok(update) => Some(SubscriptionUpdate::Render { update }),
        Err(StreamReadError::Empty) => unreachable!("blocking receive cannot be empty"),
        Err(StreamReadError::Closed) => Some(SubscriptionUpdate::Closed { error: None }),
    });
}

/// Forward a session's lifecycle events, skipping any tag the peer cannot
/// read. An unknown tag would not cost the peer one event; it would end
/// its connection, because the frame fails to parse. Sequence numbers are
/// left as they are: a subscriber resumes from the last sequence it saw and
/// never assumes the numbers it received were contiguous.
fn forward_events(
    id: SubscriptionId,
    stream: koru_session_runtime::BoundedStream<koru_session_runtime::SessionEventUpdate>,
    send: mpsc::Sender<FromHost>,
    protocol_version: u32,
) {
    forward(id, send, move || {
        loop {
            match stream.recv() {
                Ok(koru_session_runtime::SessionEventUpdate::Event(event)) => {
                    if event.event.minimum_protocol_version() > protocol_version {
                        continue;
                    }
                    return Some(SubscriptionUpdate::Event { event });
                }
                Ok(koru_session_runtime::SessionEventUpdate::Gap { expected, actual }) => {
                    return Some(SubscriptionUpdate::EventGap { expected, actual });
                }
                Err(StreamReadError::Empty) => unreachable!("blocking receive cannot be empty"),
                Err(StreamReadError::Closed) => {
                    return Some(SubscriptionUpdate::Closed { error: None });
                }
            }
        }
    });
}

fn forward_output(
    id: SubscriptionId,
    stream: koru_session_runtime::BoundedStream<RawOutputUpdate>,
    send: mpsc::Sender<FromHost>,
) {
    forward(id, send, move || match stream.recv() {
        Ok(RawOutputUpdate::Chunk(chunk)) => Some(SubscriptionUpdate::Output {
            update: OutputUpdate::Chunk { chunk },
        }),
        Ok(RawOutputUpdate::Gap { expected, actual }) => Some(SubscriptionUpdate::Output {
            update: OutputUpdate::Gap { expected, actual },
        }),
        Err(StreamReadError::Empty) => unreachable!("blocking receive cannot be empty"),
        Err(StreamReadError::Closed) => Some(SubscriptionUpdate::Closed { error: None }),
    });
}

fn forward_catalog(
    id: SubscriptionId,
    stream: std::sync::mpsc::Receiver<koru_session_protocol::CatalogEvent>,
    send: mpsc::Sender<FromHost>,
) {
    // The registry drops this receiver's sender when the connection releases
    // its subscription, which is what ends the blocking receive below.
    forward(id, send, move || match stream.recv() {
        Ok(event) => Some(SubscriptionUpdate::Catalog { event }),
        Err(_) => Some(SubscriptionUpdate::Closed { error: None }),
    });
}

fn forward_agent_events(
    id: SubscriptionId,
    stream: koru_session_runtime::BoundedStream<AgentEventUpdate>,
    send: mpsc::Sender<FromHost>,
) {
    forward(id, send, move || match stream.recv() {
        Ok(AgentEventUpdate::Event(event)) => Some(SubscriptionUpdate::AgentEvent { event }),
        Ok(AgentEventUpdate::Gap { expected, actual }) => {
            Some(SubscriptionUpdate::AgentEventGap { expected, actual })
        }
        Err(StreamReadError::Empty) => unreachable!("blocking receive cannot be empty"),
        Err(StreamReadError::Closed) => Some(SubscriptionUpdate::Closed { error: None }),
    });
}

fn forward(
    id: SubscriptionId,
    send: mpsc::Sender<FromHost>,
    mut receive: impl FnMut() -> Option<SubscriptionUpdate> + Send + 'static,
) {
    let _ = std::thread::Builder::new()
        .name("koru-session subscription".into())
        .stack_size(128 * 1024)
        .spawn(move || {
            loop {
                let Some(update) = receive() else {
                    break;
                };
                let closed = matches!(update, SubscriptionUpdate::Closed { .. });
                if send
                    .blocking_send(FromHost::Subscription {
                        frame: SubscriptionFrame {
                            subscription_id: id,
                            update,
                        },
                    })
                    .is_err()
                    || closed
                {
                    break;
                }
            }
        });
}

async fn writer_loop(
    mut writer: impl AsyncWrite + Unpin,
    mut control: mpsc::Receiver<FromHost>,
    mut subscriptions: mpsc::Receiver<FromHost>,
    dynamic_approvals: bool,
) {
    loop {
        let next = tokio::select! {
            biased;
            value = control.recv() => value,
            value = subscriptions.recv() => value,
        };
        let Some(mut frame) = next else {
            break;
        };
        if !dynamic_approvals {
            strip_dynamic_approvals(&mut frame);
        }
        if write_frame(&mut writer, &frame).await.is_err() {
            break;
        }
    }
}

async fn read_frame<R, T>(reader: &mut R) -> std::io::Result<(u8, T)>
where
    R: AsyncRead + Unpin,
    T: for<'de> serde::Deserialize<'de>,
{
    let mut header = [0; koru_ipc::frame::HEADER_LEN];
    reader.read_exact(&mut header).await?;
    let header = koru_ipc::frame::parse_header(&header)?;
    let mut body = vec![0; header.body_len];
    reader.read_exact(&mut body).await?;
    Ok((header.version, koru_ipc::frame::decode(&body)?))
}

async fn write_frame<W, T>(writer: &mut W, value: &T) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: serde::Serialize,
{
    let bytes = koru_ipc::frame::encode(FRAME_VERSION, value)?;
    writer.write_all(&bytes).await?;
    writer.flush().await
}

fn failure(code: ErrorCode) -> SessionFailure {
    SessionFailure {
        message: code.as_str().to_owned(),
        code,
    }
}

fn attachment_failure(error: koru_session_attachments::AttachmentStoreError) -> SessionFailure {
    let message = error.message().to_owned();
    SessionFailure {
        code: error.code,
        message,
    }
}

/// The desktop MCP bridge as a chat's agent will start it: `koru-mcp serve
/// --stdio`, from beside this host's own executable, under the same root.
/// `None`, with one warning, when the bridge is not installed there — the
/// chat then runs without Koru's tools rather than not at all.
fn chat_bridge(
    explicit_root: Option<&Path>,
) -> Option<koru_session_runtime::providers::McpServerLaunch> {
    let host = std::env::current_exe().ok()?;
    let command = host.with_file_name(if cfg!(windows) {
        "koru-mcp.exe"
    } else {
        "koru-mcp"
    });
    if !command.is_file() {
        tracing::warn!(
            event = "chat.bridge.missing",
            "koru-mcp is not beside this host; chats run their agents without Koru's tools"
        );
        return None;
    }
    let mut args: Vec<std::ffi::OsString> = vec!["serve".into(), "--stdio".into()];
    if let Some(root) = explicit_root {
        args.push("--root".into());
        args.push(root.as_os_str().to_owned());
    }
    Some(koru_session_runtime::providers::McpServerLaunch {
        name: "koru".into(),
        command: command.into_os_string(),
        args,
        env: Vec::new(),
    })
}

/// The agents measured to follow `tools/list`'s `nextCursor` through the
/// bridge (ADR-433, amended 2026-09-26). Kimi 2.0.0 and Codex 0.156.1 read
/// page one only and Antigravity is unmeasured, so they get the whole list
/// in one reply. Paging is what makes Grok 1.0.34 read the list reliably.
const PAGED_BRIDGE_PROVIDERS: &[&str] = &["claude", "grok", "opencode"];

/// The bridge as `program`'s session starts it — a chat's provider kind or a
/// tab's program, the same names: paged for the agents that follow the
/// cursor, the build's default for every other.
fn bridge_for(
    bridge: &koru_session_runtime::providers::McpServerLaunch,
    program: &str,
) -> koru_session_runtime::providers::McpServerLaunch {
    let mut bridge = bridge.clone();
    if PAGED_BRIDGE_PROVIDERS.contains(&program) {
        bridge.args.push("--tool-pages".into());
        bridge.args.push("on".into());
    }
    bridge
}

/// What a raw tab of `label` adds for the bridge, when this host has the
/// bridge, could write the files, and the label's CLI has a per-process form.
fn tab_bridge_for(state: &HostState, label: ProfileLabel) -> Option<tab_bridge::TabBridge> {
    let bridge = state.bridge.as_ref()?;
    let files = state.tab_bridge_files.as_ref()?;
    let program = tab_bridge::program(label)?;
    tab_bridge::launch(label, &bridge_for(bridge, program), files)
}

/// A profile's own argv followed by its resume form: `codex` becomes
/// `codex resume --last`, `claude` becomes `claude --continue`.
fn with_resume(argv: &[String], resume: &[&str]) -> Vec<String> {
    let mut own = argv.to_vec();
    own.extend(resume.iter().map(|argument| (*argument).to_owned()));
    own
}

/// A raw launch's resolved argv and environment: the profile's own, plus the
/// tab's bridge. A bridge the platform boundary refuses — an argument a
/// Windows batch shim cannot be trusted with — costs the tab its tools,
/// never its launch.
fn raw_launch(
    profile: &koru_session_profiles::LaunchProfile,
    bridge: Option<tab_bridge::TabBridge>,
    resume: &[&str],
) -> Result<(Vec<OsString>, EnvironmentPolicy), SessionFailure> {
    // The profile's own argv, then the resume form, then whatever the bridge
    // adds: `codex resume --last -c mcp_servers…`, `claude --continue
    // --mcp-config …`.
    let own = with_resume(&profile.argv, resume);
    if let Some(bridge) = bridge {
        let argv = tab_bridge::argv(&own, Some(&bridge));
        if let Some(argv) = koru_session_profiles::resolve_launch_argv(&argv) {
            let environment = if bridge.env.is_empty() {
                EnvironmentPolicy::Inherit
            } else {
                EnvironmentPolicy::InheritWith {
                    set: bridge.env.into_iter().collect(),
                    remove: BTreeSet::new(),
                }
            };
            return Ok((argv, environment));
        }
        tracing::warn!(
            event = "tab.bridge.refused",
            profile = ?profile.label,
            "the launch boundary refused the bridge's arguments; the tab runs without Koru's tools"
        );
    }
    let argv = koru_session_profiles::resolve_launch_argv(&own)
        .ok_or_else(|| failure(ErrorCode::ProfileMissing))?;
    Ok((argv, EnvironmentPolicy::Inherit))
}

fn require_attachments(protocol_version: u32, negotiated: bool) -> Result<(), SessionFailure> {
    if protocol_version < 4 {
        Err(failure(ErrorCode::ProtocolIncompatible))
    } else if !negotiated {
        Err(failure(ErrorCode::CapabilityNotNegotiated))
    } else {
        Ok(())
    }
}

mod tab_bridge;

#[cfg(test)]
mod resume_tests {
    use super::*;

    #[test]
    fn a_resume_is_the_agents_own_continue_form_after_its_own_arguments() {
        assert_eq!(
            with_resume(&["claude".into()], &["--continue"]),
            ["claude", "--continue"]
        );
        assert_eq!(
            with_resume(&["codex".into()], &["resume", "--last"]),
            ["codex", "resume", "--last"]
        );
        assert_eq!(with_resume(&["grok".into()], &[]), ["grok"]);
    }

    #[test]
    fn every_agent_with_a_per_launch_bridge_or_a_registration_can_continue() {
        use koru_session_profiles::{ProfileLabel, resume_arguments};
        for label in [
            ProfileLabel::Claude,
            ProfileLabel::CodexEmbedded,
            ProfileLabel::CodexCli,
            ProfileLabel::Copilot,
            ProfileLabel::QwenCode,
            ProfileLabel::OpenCode,
            ProfileLabel::Grok,
            ProfileLabel::Kimi,
            ProfileLabel::Antigravity,
        ] {
            assert!(resume_arguments(label).is_some(), "{label:?}");
        }
        for label in [
            ProfileLabel::PlatformShell,
            ProfileLabel::PowerShell,
            ProfileLabel::Bash,
            ProfileLabel::Muse,
            ProfileLabel::Custom,
        ] {
            assert!(resume_arguments(label).is_none(), "{label:?}");
        }
    }
}

#[cfg(test)]
mod readiness_tests;

#[cfg(test)]
mod bridge_pages_tests {
    use super::*;

    #[test]
    fn only_the_agents_that_follow_the_cursor_get_a_paged_bridge() {
        let bridge = koru_session_runtime::providers::McpServerLaunch {
            name: "koru".into(),
            command: "koru-mcp".into(),
            args: vec!["serve".into(), "--stdio".into()],
            env: Vec::new(),
        };
        for provider in ["claude", "grok", "opencode"] {
            let args = bridge_for(&bridge, provider).args;
            assert_eq!(&args[2..], &["--tool-pages", "on"], "{provider}");
        }
        for provider in ["kimi", "codex", "antigravity"] {
            let args = bridge_for(&bridge, provider).args;
            assert_eq!(
                args.len(),
                2,
                "{provider} reads page one only or is unmeasured"
            );
        }
    }
}

/// Filter at the connection boundary, never in the shared retained history.
/// This covers live events, retained replay, and the atomic observation used
/// to recover a managed transcript.
fn strip_dynamic_approvals(frame: &mut FromHost) {
    fn strip(event: &mut koru_session_protocol::AgentEvent) {
        match event {
            koru_session_protocol::AgentEvent::ApprovalRequired { choices, .. } => choices.clear(),
            koru_session_protocol::AgentEvent::ApprovalResolved { choice, .. } => *choice = None,
            _ => {}
        }
    }
    match frame {
        FromHost::Subscription {
            frame:
                SubscriptionFrame {
                    update: SubscriptionUpdate::AgentEvent { event },
                    ..
                },
        } => strip(&mut event.event),
        FromHost::Response {
            frame:
                koru_session_wire::ResponseFrame {
                    result: Ok(Response::AgentEventsObserved { observation }),
                    ..
                },
        } => {
            for event in &mut observation.events {
                strip(&mut event.event);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
