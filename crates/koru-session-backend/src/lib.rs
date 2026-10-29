// SPDX-License-Identifier: MPL-2.0

//! The session client backend: one named command thread owning the
//! `SessionClient` connection, a bounded command queue in front of it, and the
//! subscription reader threads behind it.
//!
//! **Surface-neutral by contract** (docs/features/terminal/ARCHITECTURE.md). The
//! backend knows sessions, leases, viewports and attachments; it does not know
//! windows, tabs, or any UI toolkit. A host hands [`spawn`] a [`BackendSink`]
//! and receives every answer as a [`BackendEvent`]; the host (koru.exe) maps those
//! onto its own event enum, and a later host does the same without this crate
//! changing. A module test pins the dependency direction.
//!
//! **Nothing here touches a host's thread.** The command queue is a bounded
//! 256-slot `sync_channel` whose sender raises an overflow flag rather than
//! blocking a key press; each subscription is its own small named thread; and
//! the render reader publishes back *into the command thread*, so the command
//! owner stays the only publisher of a frame.

mod attachments;
mod handover;
mod models;
mod providers;
mod sink;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use koru_session_client::{ClientError, ConnectionState, SessionClient, Subscription};
use koru_session_profiles::{LaunchIntent, LaunchProfile, managed_launch_mode, probe};
use koru_session_protocol::{
    CapabilityName, CapabilitySet, ControllerLease, ErrorCode, GridSize, IdempotencyKey,
    MAX_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION, ProjectId, ProtocolOffer, ProtocolRange,
    RenderUpdate, RowRange, SessionCommand, SessionFailure, SessionId, SessionMode,
    SessionProfileKey, SessionSummary, TerminalGeneration, ViewportGeneration, ViewportRequest,
    WorkspaceScope, capability,
};
use koru_session_view::{
    ManagedIntent, PendingInputIntent, SessionSurfaceIntent, SessionSurfaceInteraction,
    TerminalIntent,
};
use koru_session_wire::{CreateSession, EnvironmentSpec, SubscriptionUpdate};

pub use sink::{BackendEvent, BackendSink};

const COMMAND_CAPACITY: usize = 256;
const CONNECT_RETRY: Duration = Duration::from_millis(250);
const SUBSCRIPTION_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone)]
pub struct InteractionSender {
    commands: SyncSender<BackendCommand>,
    overflowed: Arc<AtomicBool>,
}

impl InteractionSender {
    fn try_command(&self, command: BackendCommand) -> bool {
        match self.commands.try_send(command) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_)) => {
                self.overflowed.store(true, Ordering::Release);
                false
            }
            Err(mpsc::TrySendError::Disconnected(_)) => false,
        }
    }

    /// **Launch, placed.** `project_id` travels inside the creation request
    /// rather than in a follow-up assign (ADR-264): placement is atomic at
    /// create, so no other window ever sees the session briefly unplaced and a
    /// retried create cannot race an assign.
    ///
    /// `display_name` is the host's presentation of the profile — localized
    /// where the host localizes — because a name a person reads is surface
    /// knowledge this crate deliberately does not hold.
    /// Explicit retry. Reading the window cache never calls this.
    pub fn discover_provider_models(
        &self,
        request: IdempotencyKey,
        provider: koru_session_protocol::ProviderKind,
        working_directory: PathBuf,
    ) -> bool {
        self.try_command(BackendCommand::DiscoverProviderModels {
            request,
            provider,
            working_directory,
        })
    }

    pub fn refresh_provider(&self, kind: koru_session_protocol::ProviderKind) {
        self.try_command(BackendCommand::RefreshProvider { kind });
    }

    pub fn launch(
        &self,
        profile: LaunchProfile,
        display_name: String,
        size: GridSize,
        intent: LaunchIntent,
        project_id: Option<ProjectId>,
    ) {
        self.launch_in(profile, display_name, size, intent, project_id, None);
    }

    /// [`launch`](Self::launch), with the session's working directory chosen
    /// by the host. `None` keeps the historical behaviour — the backend
    /// process's own current directory — which is right for a terminal
    /// launched from a console and wrong for a windowed host whose current
    /// directory is wherever the OS started it; such a host names a directory
    /// (its home, a project root) instead.
    pub fn launch_in(
        &self,
        profile: LaunchProfile,
        display_name: String,
        size: GridSize,
        intent: LaunchIntent,
        project_id: Option<ProjectId>,
        working_directory: Option<PathBuf>,
    ) {
        self.try_command(BackendCommand::Launch {
            profile,
            display_name,
            size,
            intent,
            project_id,
            working_directory,
            resume: false,
        });
    }

    /// [`launch_in`](Self::launch_in) for a **raw agent that continues its most
    /// recent conversation** in `working_directory` (`launch-resume`). Always a
    /// raw terminal: a managed chat has no such continuation.
    pub fn launch_resumed(
        &self,
        profile: LaunchProfile,
        display_name: String,
        size: GridSize,
        project_id: Option<ProjectId>,
        working_directory: PathBuf,
    ) {
        self.try_command(BackendCommand::Launch {
            profile,
            display_name,
            size,
            intent: LaunchIntent::Raw,
            project_id,
            working_directory: Some(working_directory),
            resume: true,
        });
    }

    // ---------------------------------------------------------- the projects
    //
    // Every one of these is a typed verb on the session client, sent and
    // forgotten: the answer that matters is `ProjectsChanged`, which every
    // window holds and which re-reads `ListProjects`. Nothing here writes a
    // local row and hopes the host agrees — that is what makes two windows
    // converge without a second sync mechanism.

    pub fn create_project(
        &self,
        project_id: ProjectId,
        name: String,
        workspace_scope: Option<WorkspaceScope>,
    ) {
        self.try_command(BackendCommand::CreateProject {
            project_id,
            name,
            workspace_scope,
        });
    }

    pub fn rename_project(&self, project_id: ProjectId, name: String) {
        self.try_command(BackendCommand::RenameProject { project_id, name });
    }

    pub fn set_project_root(&self, project_id: ProjectId, root_directory: Option<String>) {
        self.try_command(BackendCommand::SetProjectRoot {
            project_id,
            root_directory,
        });
    }

    /// The orphan area's second resolution (`SetProjectScope`, protocol v13).
    /// Idempotent on the wire because it names the destination, so a retry
    /// after a lost answer is the same move rather than a second one.
    pub fn set_project_scope(
        &self,
        project_id: ProjectId,
        workspace_scope: Option<WorkspaceScope>,
    ) {
        self.try_command(BackendCommand::SetProjectScope {
            project_id,
            workspace_scope,
        });
    }

    pub fn delete_project(&self, project_id: ProjectId) {
        self.try_command(BackendCommand::DeleteProject { project_id });
    }

    pub fn assign_project(&self, session_id: SessionId, project_id: Option<ProjectId>) {
        self.try_command(BackendCommand::AssignProject {
            session_id,
            project_id,
        });
    }

    pub fn select(&self, session_id: SessionId, columns: u16) {
        self.try_command(BackendCommand::Select {
            session_id,
            columns,
        });
    }

    pub fn detach(&self) {
        self.try_command(BackendCommand::Detach);
    }

    /// **Watch a session without taking it.**
    ///
    /// A delegated run's session is being driven by the orchestrator, which
    /// holds its controller lease; a window that wanted to *see* what the
    /// agent was doing had only one way in — take the lease off it. So a
    /// second connection is opened that asks for no lease at all and reads
    /// the retained managed-event tail and the live stream after it
    /// (protocol v8/v9's `observe_agent_events` and
    /// `subscribe_agent_events_from`, neither of which has ever required
    /// one). The controller keeps the session throughout, and nothing about
    /// the lease rule moves.
    ///
    /// Managed sessions only. A raw PTY's picture *is* its controller's
    /// negotiated viewport, and there is no such thing as delegating to one
    /// (ROADMAP §15).
    pub fn observe(&self, session_id: SessionId) {
        self.try_command(BackendCommand::Observe { session_id });
    }

    /// Stop watching. The observing connection is closed, which ends its
    /// reader; the session and its controller are untouched.
    pub fn stop_observing(&self) {
        self.try_command(BackendCommand::StopObserving);
    }

    /// End this connection's thread — for a terminal's own lane, once it is no
    /// longer on screen. The session it held keeps running.
    pub fn close(&self) {
        self.try_command(BackendCommand::Close);
    }

    /// §7.4: ask the session's holder for the controller. The answer is not
    /// a lease — nothing preempts — it is either *nobody holds it, attach*
    /// or *the holder has been told*. Either way the window learns where
    /// the handover stands from the session's own `handover`, which every
    /// refresh carries.
    ///
    /// `urgency` is D36's recoverable distinction and nothing more:
    /// `AtTurnBoundary` waits for the agent to finish the step,
    /// `Now` ends that step with `CancelReason::TakeoverBoundary` so the
    /// boundary arrives sooner. **Neither ends the run.**
    pub fn request_control(
        &self,
        session_id: SessionId,
        urgency: koru_session_protocol::TakeoverUrgency,
    ) {
        self.try_command(BackendCommand::RequestControl {
            session_id,
            urgency,
        });
    }

    /// §7.4: give the controller back. The orchestrator reacquires it and
    /// carries on with the objective; the run was never interrupted.
    pub fn return_control(&self) {
        self.try_command(BackendCommand::ReturnControl);
    }

    pub fn terminate(&self) {
        self.try_command(BackendCommand::Terminate);
    }

    pub fn resize(&self, size: GridSize) {
        self.try_command(BackendCommand::Intent(TerminalIntent::Raw(
            koru_session_protocol::RawCommand::Resize { size },
        )));
    }

    pub fn take_overflow(&self) -> bool {
        self.overflowed.swap(false, Ordering::AcqRel)
    }

    pub fn terminal_clipboard(&self, action: koru_session_view::ClipboardAction) {
        self.try_command(BackendCommand::Clipboard(action));
    }

    pub fn internal_sender(&self) -> SyncSender<BackendCommand> {
        self.commands.clone()
    }
}

impl SessionSurfaceInteraction for InteractionSender {
    fn dispatch(&mut self, intent: SessionSurfaceIntent) {
        self.try_command(match intent {
            SessionSurfaceIntent::Terminal(intent) => BackendCommand::Intent(intent),
            SessionSurfaceIntent::PendingInput(intent) => BackendCommand::PendingInput(intent),
            SessionSurfaceIntent::Managed(intent) => BackendCommand::ManagedIntent(intent),
        });
    }
}

pub fn channel() -> (InteractionSender, mpsc::Receiver<BackendCommand>) {
    let (send, receive) = mpsc::sync_channel(COMMAND_CAPACITY);
    (
        InteractionSender {
            commands: send,
            overflowed: Arc::new(AtomicBool::new(false)),
        },
        receive,
    )
}

pub fn spawn<S: BackendSink>(
    sink: S,
    root: Option<PathBuf>,
    profiles: Vec<LaunchProfile>,
    commands: mpsc::Receiver<BackendCommand>,
    internal: SyncSender<BackendCommand>,
) {
    let _ = std::thread::Builder::new()
        .name("koru-session-backend session client".into())
        .stack_size(256 * 1024)
        .spawn(move || run(sink, root, profiles, commands, internal, false));
}

/// **One terminal's own connection.** The same backend, started for a single
/// visible terminal: the window drives one session per connection (the client
/// holds one controller lease), so a second terminal on screen gets a second
/// connection rather than a blank pane. It skips the work only the window's
/// main connection needs — probing the profiles and reading the whole catalog
/// at start — and is ended with [`InteractionSender::close`].
pub fn spawn_lane<S: BackendSink>(
    sink: S,
    root: Option<PathBuf>,
    commands: mpsc::Receiver<BackendCommand>,
    internal: SyncSender<BackendCommand>,
) {
    let _ = std::thread::Builder::new()
        .name("koru-session-backend terminal lane".into())
        .stack_size(256 * 1024)
        .spawn(move || run(sink, root, Vec::new(), commands, internal, true));
}

#[derive(Debug)]
pub enum BackendCommand {
    DiscoverProviderModels {
        request: IdempotencyKey,
        provider: koru_session_protocol::ProviderKind,
        working_directory: PathBuf,
    },
    ModelsDiscovered {
        generation: u64,
        request: IdempotencyKey,
        result: Result<koru_session_protocol::ProviderModels, ErrorCode>,
    },
    RefreshProvider {
        kind: koru_session_protocol::ProviderKind,
    },
    ProviderChanged {
        generation: u64,
        provider: koru_session_protocol::ProviderReadiness,
    },
    ProvidersLost {
        generation: u64,
    },
    Launch {
        profile: LaunchProfile,
        display_name: String,
        size: GridSize,
        intent: LaunchIntent,
        /// Where this session lands, atomically at create.
        project_id: Option<ProjectId>,
        /// Where the session's process starts. `None` falls back to the
        /// backend process's own current directory — see
        /// [`InteractionSender::launch_in`].
        working_directory: Option<PathBuf>,
        /// Continue the agent's most recent conversation in that directory
        /// ([`InteractionSender::launch_resumed`]); raw agents only.
        resume: bool,
    },
    Select {
        session_id: SessionId,
        columns: u16,
    },
    CreateProject {
        project_id: ProjectId,
        name: String,
        workspace_scope: Option<WorkspaceScope>,
    },
    RenameProject {
        project_id: ProjectId,
        name: String,
    },
    SetProjectRoot {
        project_id: ProjectId,
        root_directory: Option<String>,
    },
    SetProjectScope {
        project_id: ProjectId,
        workspace_scope: Option<WorkspaceScope>,
    },
    DeleteProject {
        project_id: ProjectId,
    },
    AssignProject {
        session_id: SessionId,
        project_id: Option<ProjectId>,
    },
    Intent(TerminalIntent),
    ManagedIntent(ManagedIntent),
    PendingInput(PendingInputIntent),
    Clipboard(koru_session_view::ClipboardAction),
    ObservedRender {
        epoch: u64,
        update: RenderUpdate,
    },
    /// A reader thread observed the transport die under its subscription.
    ///
    /// Stamped with the connection generation the reader was spawned for, so
    /// the command thread — the only owner of recovery — can tell late news
    /// about an already-replaced connection from news about the current one.
    /// A superseded stamp is dropped; nothing else is ever done with it.
    ConnectionLost {
        generation: u64,
    },
    Refresh,
    Detach,
    Terminate,
    /// Watch a managed session's event stream, taking no controller lease.
    /// See [`InteractionSender::observe`].
    Observe {
        session_id: SessionId,
    },
    /// Stop watching, closing the observing connection.
    StopObserving,
    /// End this connection (a terminal's own lane): detach, disconnect, stop.
    Close,
    /// Protocol v14, §7.4. Ask the holder for the controller.
    RequestControl {
        session_id: SessionId,
        urgency: koru_session_protocol::TakeoverUrgency,
    },
    /// Protocol v14, §7.4. Hand the controller back to the orchestrator.
    ReturnControl,
}

struct Active {
    session_id: SessionId,
    lease: ControllerLease,
    terminal_generation: Option<TerminalGeneration>,
    viewport_generation: ViewportGeneration,
    pending_viewport: Option<(RowRange, u16, u16)>,
    resize_in_flight: Option<GridSize>,
    mode: SessionMode,
    epoch: u64,
    /// The connection generation this attachment was taken under. Every render
    /// reader spawned for it — the attach one and each viewport replacement —
    /// carries this stamp for its loss signal.
    generation: u64,
    /// The last width the surface asked for, kept so a same-instance recovery
    /// can re-attach at the size the person was looking at.
    columns: u16,
    managed_client: Option<Arc<SessionClient>>,
}

/// One session this window is **watching**.
///
/// A second `SessionClient` of its own, so that closing it ends the watch
/// without touching the connection that holds the controller — and so that
/// the watch is visibly not a lease: there is no `ControllerLease` in this
/// struct, and there is nowhere for one to be put.
///
/// The epoch a watch is opened at is not kept here: it is stamped onto every
/// event the watch delivers and checked by the surface that owns the
/// session (ADR-263's late-frame guard), which is where it is needed.
struct Observing {
    session_id: SessionId,
    /// The observing connection. Dropping the watch disconnects it, which
    /// closes its subscription and lets its reader thread end.
    client: Arc<SessionClient>,
}

struct LaunchRequest {
    profile: LaunchProfile,
    display_name: String,
    size: GridSize,
    intent: LaunchIntent,
    project_id: Option<ProjectId>,
    working_directory: Option<PathBuf>,
    resume: bool,
    readiness: Vec<koru_session_protocol::ProviderReadiness>,
}

impl BackendCommand {
    fn operation(&self) -> &'static str {
        match self {
            Self::DiscoverProviderModels { .. } => "discover_provider_models",
            Self::ModelsDiscovered { .. } => "models_discovered",
            Self::RefreshProvider { .. } => "refresh_provider",
            Self::ProviderChanged { .. } => "provider_changed",
            Self::ProvidersLost { .. } => "providers_lost",
            Self::Launch { .. } => "launch",
            Self::Select { .. } => "attach",
            Self::CreateProject { .. } => "create_project",
            Self::RenameProject { .. } => "rename_project",
            Self::SetProjectRoot { .. } => "set_project_root",
            Self::SetProjectScope { .. } => "set_project_scope",
            Self::DeleteProject { .. } => "delete_project",
            Self::AssignProject { .. } => "assign_project",
            Self::Intent(_) => "terminal_input",
            Self::ManagedIntent(_) => "managed_input",
            Self::PendingInput(_) => "pending_input",
            Self::Clipboard(_) => "clipboard",
            Self::ObservedRender { .. } => "observe_render",
            Self::ConnectionLost { .. } => "connection_lost",
            Self::Refresh => "refresh",
            Self::Detach => "detach",
            Self::Terminate => "terminate",
            Self::Observe { .. } => "observe",
            Self::StopObserving => "stop_observing",
            Self::Close => "close",
            Self::RequestControl { .. } => "request_control",
            Self::ReturnControl => "return_control",
        }
    }

    fn session_id(&self, active: &Option<Active>) -> Option<SessionId> {
        match self {
            Self::DiscoverProviderModels { .. }
            | Self::ModelsDiscovered { .. }
            | Self::RefreshProvider { .. }
            | Self::ProviderChanged { .. }
            | Self::ProvidersLost { .. }
            | Self::Launch { .. }
            | Self::Refresh
            | Self::Close
            | Self::ConnectionLost { .. }
            | Self::CreateProject { .. }
            | Self::RenameProject { .. }
            | Self::SetProjectRoot { .. }
            | Self::SetProjectScope { .. }
            | Self::DeleteProject { .. } => None,
            Self::Select { session_id, .. }
            | Self::AssignProject { session_id, .. }
            | Self::Observe { session_id }
            | Self::RequestControl { session_id, .. } => Some(*session_id),
            Self::ObservedRender { update, .. } => Some(render_session_id(update)),
            _ => active.as_ref().map(|current| current.session_id),
        }
    }
}

struct AttachContext<'a, S: BackendSink> {
    endpoint: &'a koru_ipc::Endpoint,
    sink: &'a S,
    internal: &'a SyncSender<BackendCommand>,
    attachment_worker: &'a attachments::Worker,
    /// The connection generation this attach happens under; it stamps the
    /// [`Active`] record and every reader spawned for it.
    generation: u64,
}

// Derives would demand `S: Copy`; the context is all references, so it is
// copyable whatever the sink is.
impl<S: BackendSink> Clone for AttachContext<'_, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S: BackendSink> Copy for AttachContext<'_, S> {}

/// Everything owned per connection: the command client, the attachment
/// worker (with its own multiplexed connection), and — spawned as a side
/// effect — the projects reader. Replaced wholesale by recovery; nothing in
/// it survives a connection loss.
struct Connection {
    providers: providers::Catalog,
    model_worker: models::Worker,
    client: Arc<SessionClient>,
    attachment_worker: attachments::Worker,
}

/// Build the per-connection machinery around a freshly connected client.
///
/// `generation` stamps the projects reader it spawns, so a reader left over
/// from a superseded connection can be told apart from the current one.
fn establish<S: BackendSink>(
    client: Arc<SessionClient>,
    generation: u64,
    endpoint: &koru_ipc::Endpoint,
    sink: &S,
    internal: &SyncSender<BackendCommand>,
) -> Connection {
    // Attachment validation and preview work can take disk/decoder latency.
    // Give it a second multiplexed connection so the server-side request loop
    // for keystrokes, resize, and viewport commands remains available.
    let attachment_client = SessionClient::connect(
        endpoint,
        "koru-session-backend-attachments",
        protocol_offer(),
    )
    .map(Arc::new)
    .unwrap_or_else(|_| Arc::clone(&client));
    let attachment_worker = attachments::Worker::spawn(attachment_client, sink.clone());
    // Held for the connection's life. Taken *before* the first refresh, so a
    // mutation another window makes between the two is not the one this window
    // misses.
    if let Ok(subscription) = client.subscribe_projects() {
        spawn_projects_reader(
            Arc::clone(&client),
            sink.clone(),
            internal.clone(),
            generation,
            subscription,
        );
    }
    let providers = providers::subscribe(&client, generation, sink, internal);
    Connection {
        model_worker: models::Worker::spawn(endpoint.clone(), generation, internal.clone()),
        providers,
        client,
        attachment_worker,
    }
}

fn run<S: BackendSink>(
    sink: S,
    root: Option<PathBuf>,
    profiles: Vec<LaunchProfile>,
    commands: mpsc::Receiver<BackendCommand>,
    internal: SyncSender<BackendCommand>,
    lane: bool,
) {
    if !lane {
        send(&sink, BackendEvent::Profiles(probe(profiles)));
    }
    let endpoint = match direct_endpoint(root.as_deref()) {
        Ok(endpoint) => endpoint,
        Err(_) => {
            send(
                &sink,
                BackendEvent::Failure(failure(ErrorCode::HostUnavailable)),
            );
            tracing::warn!(target: "koru_session_backend", operation = "resolve_endpoint", code = %ErrorCode::HostUnavailable.as_str(), "session operation failed");
            return;
        }
    };
    let Some(client) = await_session_host(
        &commands,
        CONNECT_RETRY,
        || {
            SessionClient::connect(&endpoint, "koru-session-backend", protocol_offer())
                .map(Arc::new)
        },
        |failure| {
            log_failure("connect", None, failure);
            send(&sink, BackendEvent::Providers(providers::unavailable()));
            send(&sink, BackendEvent::Disconnected);
        },
        |command| {
            if let BackendCommand::DiscoverProviderModels { request, .. } = &command {
                send(
                    &sink,
                    BackendEvent::ProviderModels {
                        request: *request,
                        result: Err(ErrorCode::HostUnavailable),
                    },
                );
            }
            report_failure(
                &sink,
                command.operation(),
                command.session_id(&None),
                ClientError::Session(failure(ErrorCode::HostUnavailable)),
            );
        },
    ) else {
        return;
    };
    // The connection generation (W3b). Owned here, bumped by `recover` for
    // every connection it establishes; readers carry the value they were
    // spawned with, and anything stamped with a superseded value is late news
    // about a connection that no longer exists.
    let mut generation = 1_u64;
    let mut connection = establish(client, generation, &endpoint, &sink, &internal);
    let mut subscribed = BTreeSet::new();
    let mut active = None;
    // The session this window is *watching* — a second connection holding no
    // lease. Independent of `active` on purpose: watching a run's session
    // and driving one are two different relationships, and a window may be
    // in the second while another process is in the first.
    let mut observing: Option<Observing> = None;
    let mut next_epoch = 1_u64;
    if !lane {
        refresh(
            &connection.client,
            &sink,
            &internal,
            generation,
            &mut subscribed,
            true,
        );
    }

    while let Ok(command) = commands.recv() {
        // The window is done with this connection: give the session back (the
        // process keeps running) and end the thread.
        if matches!(command, BackendCommand::Close) {
            let _ = detach(&connection.client, &mut active);
            connection.client.disconnect();
            return;
        }
        if let BackendCommand::ConnectionLost {
            generation: observed,
        } = command
        {
            // A reader whose generation is superseded reports a loss that was
            // already recovered from; its signal is dropped, never presented.
            if observed == generation
                && !recover(
                    &sink,
                    &endpoint,
                    &commands,
                    &internal,
                    &mut connection,
                    &mut generation,
                    &mut active,
                    &mut observing,
                    &mut next_epoch,
                    &mut subscribed,
                )
            {
                return;
            }
            continue;
        }
        let operation = command.operation();
        let session_id = command.session_id(&active);
        let result = match command {
            BackendCommand::DiscoverProviderModels {
                request,
                provider,
                working_directory,
            } => {
                if !connection
                    .model_worker
                    .discover(request, provider, working_directory)
                {
                    send(
                        &sink,
                        BackendEvent::ProviderModels {
                            request,
                            result: Err(ErrorCode::Backpressure),
                        },
                    );
                }
                Ok(())
            }
            BackendCommand::ModelsDiscovered {
                generation: observed,
                request,
                result,
            } => {
                if observed == generation {
                    send(&sink, BackendEvent::ProviderModels { request, result });
                }
                Ok(())
            }
            BackendCommand::ProviderChanged {
                generation: observed,
                provider,
            } => {
                if connection.providers.accept(generation, observed, provider) {
                    send(
                        &sink,
                        BackendEvent::Providers(connection.providers.snapshot()),
                    );
                }
                Ok(())
            }
            BackendCommand::ProvidersLost {
                generation: observed,
            } => {
                if observed == generation {
                    connection.providers =
                        providers::subscribe(&connection.client, generation, &sink, &internal);
                }
                Ok(())
            }
            BackendCommand::RefreshProvider { kind } => connection.client.refresh_provider(kind),
            BackendCommand::Launch {
                profile,
                display_name,
                size,
                intent,
                project_id,
                working_directory,
                resume,
            } => launch(
                &connection.client,
                AttachContext {
                    endpoint: &endpoint,
                    sink: &sink,
                    internal: &internal,
                    attachment_worker: &connection.attachment_worker,
                    generation,
                },
                &mut active,
                &mut next_epoch,
                LaunchRequest {
                    profile,
                    display_name,
                    size,
                    intent,
                    project_id,
                    working_directory,
                    resume,
                    readiness: connection.providers.snapshot(),
                },
            ),
            // **Every project mutation answers with a fresh catalogue**, so a
            // window sees its own change without waiting for the round trip
            // through `ProjectsChanged` — and every *other* window sees it
            // through that stream, which is the only mechanism.
            BackendCommand::CreateProject {
                project_id,
                name,
                workspace_scope,
            } => connection
                .client
                .create_project(project_id, name, None, workspace_scope)
                .map(|_| ())
                .and_then(|()| publish_projects(&connection.client, &sink)),
            BackendCommand::RenameProject { project_id, name } => connection
                .client
                .rename_project(project_id, name)
                .map(|_| ())
                .and_then(|()| publish_projects(&connection.client, &sink)),
            BackendCommand::SetProjectRoot {
                project_id,
                root_directory,
            } => connection
                .client
                .set_project_root(project_id, root_directory)
                .map(|_| ())
                .and_then(|()| publish_projects(&connection.client, &sink)),
            BackendCommand::SetProjectScope {
                project_id,
                workspace_scope,
            } => connection
                .client
                .set_project_scope(project_id, workspace_scope)
                .map(|_| ())
                .and_then(|()| publish_projects(&connection.client, &sink)),
            BackendCommand::DeleteProject { project_id } => connection
                .client
                .delete_project(project_id)
                .and_then(|()| publish_projects(&connection.client, &sink)),
            BackendCommand::AssignProject {
                session_id,
                project_id,
            } => connection
                .client
                .assign_project(session_id, project_id)
                .and_then(|()| {
                    // A move changes a *session's* placement, so the session list
                    // is what moved as well as the project catalogue.
                    send(&sink, BackendEvent::Sessions(connection.client.list()?));
                    publish_projects(&connection.client, &sink)
                }),
            BackendCommand::Select {
                session_id,
                columns,
            } => attach(
                &connection.client,
                AttachContext {
                    endpoint: &endpoint,
                    sink: &sink,
                    internal: &internal,
                    attachment_worker: &connection.attachment_worker,
                    generation,
                },
                &mut active,
                &mut next_epoch,
                session_id,
                columns,
            ),
            BackendCommand::Intent(intent) => {
                route_intent(&connection.client, &sink, &internal, &mut active, intent)
            }
            BackendCommand::ManagedIntent(intent) => route_managed_intent(&sink, &active, intent),
            BackendCommand::PendingInput(intent) => {
                route_pending_input(&sink, &active, &connection.attachment_worker, intent)
            }
            BackendCommand::Clipboard(action) => {
                route_clipboard(&sink, &active, &connection.attachment_worker, action)
            }
            BackendCommand::ObservedRender { epoch, update } => observe_render(
                &connection.client,
                &sink,
                &internal,
                &mut active,
                epoch,
                &update,
            ),
            BackendCommand::ConnectionLost { .. } | BackendCommand::Close => {
                unreachable!("handled before the match")
            }
            BackendCommand::Refresh => {
                refresh(
                    &connection.client,
                    &sink,
                    &internal,
                    generation,
                    &mut subscribed,
                    false,
                );
                Ok(())
            }
            // §7.4. `Available` means no controller is held, so the
            // handover this window is waiting for has already happened —
            // it says `Released` to itself and attaches through the same
            // path a granted release takes, rather than having two.
            BackendCommand::RequestControl {
                session_id,
                urgency,
            } => handover::request(
                &connection.client,
                &sink,
                &internal,
                generation,
                &mut subscribed,
                session_id,
                urgency,
            ),
            // Giving it back is `return_control` on the lease this window
            // holds, which is also a detach: the connection stops being the
            // controller, and the orchestrator's next attach reclaims it.
            BackendCommand::ReturnControl => match active.take() {
                Some(current) => connection
                    .client
                    .return_control(current.session_id, current.lease.token),
                None => Ok(()),
            },
            BackendCommand::Observe { session_id } => observe(
                &connection.client,
                &endpoint,
                &sink,
                &mut observing,
                &mut next_epoch,
                session_id,
            ),
            BackendCommand::StopObserving => {
                stop_observing(&mut observing);
                Ok(())
            }
            BackendCommand::Detach => detach(&connection.client, &mut active),
            BackendCommand::Terminate => terminate(&connection.client, &mut active).map(|()| {
                if let Some(session_id) = session_id {
                    send(&sink, BackendEvent::Terminated { session_id });
                }
            }),
        };
        if let Err(error) = result {
            // Reported first, recovered second: the failure *is* the
            // unknown-outcome presentation for the command whose response was
            // lost, and recovery never re-sends it — the owning surface offers
            // the retry, which idempotency keys make safe.
            report_failure(&sink, operation, session_id, error);
            if connection.client.state() == ConnectionState::Disconnected
                && !recover(
                    &sink,
                    &endpoint,
                    &commands,
                    &internal,
                    &mut connection,
                    &mut generation,
                    &mut active,
                    &mut observing,
                    &mut next_epoch,
                    &mut subscribed,
                )
            {
                return;
            }
        }
        if connection.attachment_worker.take_overflow() {
            send(&sink, BackendEvent::Backpressure);
        }
    }
    stop_observing(&mut observing);
    let _ = detach(&connection.client, &mut active);
}

fn await_session_host<T>(
    commands: &mpsc::Receiver<BackendCommand>,
    retry_every: Duration,
    mut connect: impl FnMut() -> Result<T, ClientError>,
    mut unavailable: impl FnMut(&SessionFailure),
    mut rejected: impl FnMut(&BackendCommand),
) -> Option<T> {
    let mut reported = false;
    loop {
        match connect() {
            Ok(client) => {
                if reported {
                    tracing::info!(target: "koru_session_backend", "session host connection recovered");
                }
                return Some(client);
            }
            Err(error) => {
                if !reported {
                    let failure = client_failure(error);
                    unavailable(&failure);
                    reported = true;
                }
            }
        }
        match commands.recv_timeout(retry_every) {
            Ok(BackendCommand::Detach | BackendCommand::Close)
            | Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            Ok(command) => rejected(&command),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

/// Connection-generation recovery (W3b).
///
/// Runs on the backend's command thread once the session-host connection is
/// observed lost — by a reader thread's [`BackendCommand::ConnectionLost`]
/// signal, or by a command failing while the client reports itself
/// disconnected. In order:
///
/// 1. **The fence rises first.** The generation is bumped before anything is
///    rebuilt, so every reader and every signal stamped with the old value is
///    superseded from that line on; render frames are additionally fenced by
///    the per-attach epoch, which is cleared here and never reused.
/// 2. **Nothing crosses.** The lease, the pending presentation state
///    (viewport, in-flight resize) and the subscription registrations are
///    cleared; the command and attachment clients and the attachment worker
///    are retired and rebuilt. No old lease token or subscription handle is
///    carried into the new connection.
/// 3. **Reconnect is bounded retry**, the same cadence as the startup wait,
///    reporting [`BackendEvent::Disconnected`] once per outage — news, not
///    activity. Unlike the startup wait, `Detach` here means *the person left
///    the session* (so nothing is re-attached), never *stop the backend*: the
///    window that owns this thread is still open.
/// 4. **Host identity comes from the handshake**, `HostHello.instance_id` and
///    `supervisor_launch_id`, never from the catalog — the catalog answers
///    only whether a given session still exists. The same instance means the
///    host survived a transport drop: resume quietly ([`BackendEvent::Recovered`],
///    and re-attach what was attached under a fresh lease and epoch). A
///    different instance means a restarted, empty host: present the truth
///    ([`BackendEvent::SessionsLost`]) — even when nothing was attached,
///    because an empty catalog on a new instance is loss, not "nothing
///    changed".
/// 5. **No replay, ever.** A command whose response was lost was already
///    reported as failed before recovery began; recovery re-establishes the
///    connection and subscriptions and sends no input, no launch, and no
///    attachment delivery. The owning surface presents the unknown outcome
///    and a person retries — idempotency keys make that retry safe. The old
///    attachment worker's queued jobs fail fast against its retired client,
///    which is exactly that unknown-outcome presentation.
///
/// Answers `false` only when the host application is closing (the command
/// channel disconnected), which ends the backend thread.
#[allow(clippy::too_many_arguments)]
fn recover<S: BackendSink>(
    sink: &S,
    endpoint: &koru_ipc::Endpoint,
    commands: &mpsc::Receiver<BackendCommand>,
    internal: &SyncSender<BackendCommand>,
    connection: &mut Connection,
    generation: &mut u64,
    active: &mut Option<Active>,
    observing: &mut Option<Observing>,
    next_epoch: &mut u64,
    subscribed: &mut BTreeSet<SessionId>,
) -> bool {
    *generation += 1;
    let previous = connection.client.host().clone();
    let mut resume = active.take().map(|current| {
        if let Some(managed) = current.managed_client {
            managed.disconnect();
        }
        (current.session_id, current.columns)
    });
    // A watch's connection died with the host's; the session it was watching
    // is what a same-instance recovery re-establishes it on.
    let mut rewatch = observing.as_ref().map(|watch| watch.session_id);
    stop_observing(observing);
    subscribed.clear();
    // Closes every old subscription queue, so each superseded reader wakes,
    // signals with its old stamp (dropped), and exits quietly. The old
    // attachment worker goes when `connection` is replaced below.
    connection.client.disconnect();
    tracing::warn!(target: "koru_session_backend", operation = "recover", "session host connection lost");
    send(sink, BackendEvent::Providers(providers::unavailable()));
    send(sink, BackendEvent::Disconnected);
    loop {
        let client = loop {
            if let Ok(client) =
                SessionClient::connect(endpoint, "koru-session-backend", protocol_offer())
            {
                break Arc::new(client);
            }
            match commands.recv_timeout(CONNECT_RETRY) {
                // The person left the session while the host was away; the
                // reconnect continues, but nothing is re-attached for them.
                Ok(BackendCommand::Detach) => resume = None,
                // The person left the session they were watching.
                Ok(BackendCommand::StopObserving) => rewatch = None,
                Ok(BackendCommand::Close) => return false,
                // Late traffic from superseded readers, not a person's ask.
                Ok(
                    BackendCommand::ConnectionLost { .. }
                    | BackendCommand::ProviderChanged { .. }
                    | BackendCommand::ProvidersLost { .. }
                    | BackendCommand::ModelsDiscovered { .. }
                    | BackendCommand::ObservedRender { .. }
                    | BackendCommand::Refresh,
                ) => {}
                Ok(BackendCommand::DiscoverProviderModels { request, .. }) => {
                    send(
                        sink,
                        BackendEvent::ProviderModels {
                            request,
                            result: Err(ErrorCode::HostUnavailable),
                        },
                    );
                }
                Ok(command) => report_failure(
                    sink,
                    command.operation(),
                    command.session_id(&None),
                    ClientError::Session(failure(ErrorCode::HostUnavailable)),
                ),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return false,
            }
        };
        let same_instance = {
            let hello = client.host();
            hello.instance_id == previous.instance_id
                && hello.supervisor_launch_id == previous.supervisor_launch_id
        };
        *connection = establish(client, *generation, endpoint, sink, internal);
        let sessions = match connection.client.list() {
            Ok(sessions) => sessions,
            Err(_) => {
                // The host went away again mid-rebuild: still the same outage
                // from the person's view, so no second report — but this
                // attempt's readers are superseded before the next one.
                connection.client.disconnect();
                *generation += 1;
                continue;
            }
        };
        subscribe_new(
            &connection.client,
            internal,
            *generation,
            subscribed,
            &sessions,
        );
        let _ = publish_projects(&connection.client, sink);
        tracing::info!(
            target: "koru_session_backend",
            same_instance,
            "session host connection recovered"
        );
        if same_instance {
            send(sink, BackendEvent::Recovered { sessions });
            if let Some((session_id, columns)) = resume
                && let Err(error) = attach(
                    &connection.client,
                    AttachContext {
                        endpoint,
                        sink,
                        internal,
                        attachment_worker: &connection.attachment_worker,
                        generation: *generation,
                    },
                    active,
                    next_epoch,
                    session_id,
                    columns,
                )
            {
                // The session may have ended during the outage; the catalog
                // above is the truth and the failure is presented, not hidden.
                report_failure(sink, "attach", Some(session_id), error);
            }
            if let Some(session_id) = rewatch
                && let Err(error) = observe(
                    &connection.client,
                    endpoint,
                    sink,
                    observing,
                    next_epoch,
                    session_id,
                )
            {
                report_failure(sink, "observe", Some(session_id), error);
            }
        } else {
            send(sink, BackendEvent::SessionsLost { sessions });
        }
        return true;
    }
}

fn launch<S: BackendSink>(
    client: &Arc<SessionClient>,
    context: AttachContext<'_, S>,
    active: &mut Option<Active>,
    next_epoch: &mut u64,
    request: LaunchRequest,
) -> Result<(), ClientError> {
    detach(client, active)?;
    let working_directory = match request.working_directory {
        Some(directory) => directory,
        None => std::env::current_dir().map_err(|error| ClientError::Ipc(error.to_string()))?,
    };
    let project_id = request.project_id;
    if request.resume {
        // A continuation is a raw agent in a folder: no managed attempt and no
        // fallback, because falling back to a fresh conversation is exactly
        // what the person declined by pressing *Resume*.
        let summary = client.create_profile_resumed(
            IdempotencyKey::mint(),
            request.profile.id.clone(),
            project_id,
            working_directory,
            request.size,
        )?;
        send(context.sink, BackendEvent::Sessions(client.list()?));
        return attach(
            client,
            context,
            active,
            next_epoch,
            summary.id,
            request.size.columns,
        );
    }
    let raw = || {
        create_raw_profile(
            client,
            &request.profile,
            request.display_name.clone(),
            working_directory.clone(),
            request.size,
            project_id,
        )
    };
    let (summary, fallback) = create_with_raw_fallback(
        &request.profile,
        request.intent,
        &request.readiness,
        |mode| {
            client.create_profile_in(
                IdempotencyKey::mint(),
                request.profile.id.clone(),
                None,
                mode,
                project_id,
                working_directory.clone(),
                request.size,
            )
        },
        raw,
    )?;
    if let Some(code) = fallback.as_ref() {
        tracing::info!(
            target: "koru_session_backend",
            operation = "launch_managed",
            code = %closed_error_code(code),
            fallback = "raw_pty",
            "managed launch used raw fallback"
        );
    }
    send(context.sink, BackendEvent::Sessions(client.list()?));
    attach(
        client,
        context,
        active,
        next_epoch,
        summary.id,
        request.size.columns,
    )?;
    if fallback.is_some() {
        send(
            context.sink,
            BackendEvent::RawFallback {
                session_id: summary.id,
            },
        );
    }
    Ok(())
}

fn create_raw_profile(
    client: &SessionClient,
    profile: &LaunchProfile,
    display_name: String,
    working_directory: PathBuf,
    size: GridSize,
    project_id: Option<ProjectId>,
) -> Result<SessionSummary, ClientError> {
    match &profile.id {
        SessionProfileKey::BuiltIn(_) => match client.create_profile_in(
            IdempotencyKey::mint(),
            profile.id.clone(),
            None,
            SessionMode::RawPty,
            project_id,
            working_directory.clone(),
            size,
        ) {
            Err(ClientError::Session(session_failure))
                if session_failure.code == ErrorCode::ProfileNotFound =>
            {
                // Built-in keys are append-only, so a freshly built Terminal
                // can temporarily meet an older, session-owning host. Preserve
                // those live PTYs: use this build's shared resolver and the
                // legacy concrete-argv request only for that closed mismatch.
                let argv = koru_session_profiles::resolve_launch_strings(&profile.argv)
                    .ok_or_else(|| ClientError::Session(failure(ErrorCode::ProfileMissing)))?;
                create_direct_raw(
                    client,
                    profile,
                    display_name,
                    working_directory,
                    size,
                    argv,
                    project_id,
                )
            }
            result => result,
        },
        SessionProfileKey::Custom(_) => create_direct_raw(
            client,
            profile,
            display_name,
            working_directory,
            size,
            profile.argv.clone(),
            project_id,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn create_direct_raw(
    client: &SessionClient,
    profile: &LaunchProfile,
    display_name: String,
    working_directory: PathBuf,
    size: GridSize,
    argv: Vec<String>,
    project_id: Option<ProjectId>,
) -> Result<SessionSummary, ClientError> {
    client.create(CreateSession {
        profile: profile.id.clone(),
        parent_session_id: None,
        profile_kind: profile.kind,
        display_name,
        working_directory,
        mode: SessionMode::RawPty,
        attachment_delivery: profile.attachment_delivery,
        capabilities: protocol_offer().capabilities,
        // **Both creation requests carry it** (ADR-264). The concrete-argv
        // frame is the legacy path for an older host, and a placement that
        // travelled on one request and not the other would put a session in no
        // project exactly when the peer was oldest.
        project_id,
        argv,
        environment: EnvironmentSpec::Inherit,
        size,
    })
}

fn create_with_raw_fallback<T>(
    profile: &LaunchProfile,
    intent: LaunchIntent,
    readiness: &[koru_session_protocol::ProviderReadiness],
    managed: impl FnOnce(SessionMode) -> Result<T, ClientError>,
    raw: impl FnOnce() -> Result<T, ClientError>,
) -> Result<(T, Option<ErrorCode>), ClientError> {
    let mode = match managed_launch_mode(
        profile,
        intent,
        koru_session_profiles::MANAGED_PROVIDER_REGISTRATIONS,
        Some(readiness),
    ) {
        Ok(SessionMode::RawPty) => return raw().map(|created| (created, None)),
        Ok(mode) => mode,
        Err(code) if intent == LaunchIntent::Ordinary && managed_raw_fallback_code(&code) => {
            return raw().map(|created| (created, Some(code)));
        }
        Err(code) => return Err(ClientError::Session(failure(code))),
    };
    match managed(mode) {
        Ok(created) => Ok((created, None)),
        Err(error) => {
            let failure = client_failure(error);
            if intent == LaunchIntent::Ordinary && managed_raw_fallback_code(&failure.code) {
                raw().map(|created| (created, Some(failure.code)))
            } else {
                Err(ClientError::Session(failure))
            }
        }
    }
}

fn managed_raw_fallback_code(code: &ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::ProviderUnavailable
            | ErrorCode::ProviderIncompatible
            | ErrorCode::ProfileIncompatible
            | ErrorCode::CapabilityNotNegotiated
            | ErrorCode::ProtocolIncompatible
    )
}

fn attach<S: BackendSink>(
    client: &Arc<SessionClient>,
    context: AttachContext<'_, S>,
    active: &mut Option<Active>,
    next_epoch: &mut u64,
    session_id: SessionId,
    columns: u16,
) -> Result<(), ClientError> {
    detach(client, active)?;
    let summary = client
        .list()?
        .into_iter()
        .find(|summary| summary.id == session_id)
        .ok_or_else(|| ClientError::Session(failure(ErrorCode::SessionNotFound)))?;
    let epoch = *next_epoch;
    *next_epoch = next_epoch.saturating_add(1).max(1);
    let viewport_generation = ViewportGeneration::new(1).expect("positive generation");
    let (lease, subscription, managed_start) = match &summary.mode {
        SessionMode::RawPty => {
            let (lease, subscription) =
                attach_raw(client, session_id, viewport_generation, columns, 2)?;
            (lease, Some(subscription), None)
        }
        SessionMode::Managed { .. } => {
            let stream_client = Arc::new(SessionClient::connect(
                context.endpoint,
                "koru-session-backend-managed",
                protocol_offer(),
            )?);
            let (lease, _unused_render) =
                stream_client.attach_live(session_id, viewport_generation, columns, 2)?;
            let observation = stream_client.observe_agent_events(session_id)?;
            let events =
                stream_client.subscribe_agent_events_from(session_id, Some(observation.cursor))?;
            (lease, None, Some((stream_client, observation, events)))
        }
    };
    let managed_client = managed_start
        .as_ref()
        .map(|(client, _, _)| Arc::clone(client));
    *active = Some(Active {
        session_id,
        lease,
        terminal_generation: None,
        viewport_generation,
        pending_viewport: None,
        resize_in_flight: None,
        mode: summary.mode.clone(),
        epoch,
        generation: context.generation,
        columns,
        managed_client,
    });
    // Publish controller identity before a render reader can publish its first
    // snapshot. Starting the reader first made this ordering nondeterministic:
    // a fast shell could deliver its snapshot while the window still had no
    // pending attachment, leaving presentation staging (and all input) stuck.
    send(
        context.sink,
        BackendEvent::Attached {
            session_id,
            mode: summary.mode,
            epoch,
        },
    );
    if let Some(subscription) = subscription {
        spawn_render_reader(
            Arc::clone(client),
            context.internal.clone(),
            context.generation,
            session_id,
            epoch,
            subscription,
        );
    }
    if let Some((stream_client, observation, events)) = managed_start {
        send(
            context.sink,
            BackendEvent::ManagedBatch {
                session_id,
                epoch,
                events: observation.events,
                earlier_truncated: observation.earlier_truncated,
            },
        );
        spawn_managed_reader(
            stream_client,
            context.sink.clone(),
            session_id,
            epoch,
            events,
        );
    }
    if !context.attachment_worker.load(attachments::Target {
        session_id,
        lease: active
            .as_ref()
            .expect("active session was installed")
            .lease
            .token,
    }) {
        send(context.sink, BackendEvent::Backpressure);
    }
    Ok(())
}

/// **Watch a managed session, taking no controller lease.**
///
/// The two verbs used here — `observe_agent_events` for the retained tail
/// and its resume cursor, and `subscribe_agent_events_from` for everything
/// after it — are protocol v8/v9 observer reads and have never required a
/// lease (`koru-session-runtime`'s `Session::observe_agent_events` takes the
/// state lock and reads; nothing calls `require_lease`). So this is not a
/// second way to hold a session: it is the *absence* of holding one, and the
/// orchestrator that does hold the lease keeps it throughout.
///
/// Only a managed session can be watched. A raw PTY's picture is the grid
/// its controller negotiated, so watching one would mean asking for a second
/// viewport — and there is no such thing as a delegated raw session anyway
/// (ROADMAP §15: "No raw-PTY delegation, ever").
fn observe<S: BackendSink>(
    catalog: &SessionClient,
    endpoint: &koru_ipc::Endpoint,
    sink: &S,
    observing: &mut Option<Observing>,
    next_epoch: &mut u64,
    session_id: SessionId,
) -> Result<(), ClientError> {
    if observing
        .as_ref()
        .is_some_and(|watch| watch.session_id == session_id)
    {
        return Ok(());
    }
    stop_observing(observing);
    let summary = catalog
        .list()?
        .into_iter()
        .find(|summary| summary.id == session_id)
        .ok_or_else(|| ClientError::Session(failure(ErrorCode::SessionNotFound)))?;
    if !matches!(summary.mode, SessionMode::Managed { .. }) {
        return Err(ClientError::Session(failure(
            ErrorCode::CapabilityNotNegotiated,
        )));
    }
    let epoch = *next_epoch;
    *next_epoch = next_epoch.saturating_add(1).max(1);
    let watcher = Arc::new(SessionClient::connect(
        endpoint,
        "koru-session-backend-observer",
        protocol_offer(),
    )?);
    let observation = watcher.observe_agent_events(session_id)?;
    let events = watcher.subscribe_agent_events_from(session_id, Some(observation.cursor))?;
    *observing = Some(Observing {
        session_id,
        client: Arc::clone(&watcher),
    });
    // The identity and epoch first, the way an attach publishes `Attached`
    // before its reader can deliver a frame: a batch that arrived before the
    // host knew which watch it belonged to would be dropped by its own guard.
    send(sink, BackendEvent::Observing { session_id, epoch });
    send(
        sink,
        BackendEvent::ManagedBatch {
            session_id,
            epoch,
            events: observation.events,
            earlier_truncated: observation.earlier_truncated,
        },
    );
    spawn_managed_reader(watcher, sink.clone(), session_id, epoch, events);
    Ok(())
}

/// End a watch. Nothing is detached and no lease is released, because none
/// was ever taken: the connection is closed, its subscription with it, and
/// its reader thread wakes and exits.
fn stop_observing(observing: &mut Option<Observing>) {
    if let Some(watch) = observing.take() {
        watch.client.disconnect();
    }
}

fn route_pending_input<S: BackendSink>(
    sink: &S,
    active: &Option<Active>,
    worker: &attachments::Worker,
    intent: PendingInputIntent,
) -> Result<(), ClientError> {
    let Some(current) = active.as_ref() else {
        return Ok(());
    };
    let target = attachments::Target {
        session_id: current.session_id,
        lease: current.lease.token,
    };
    let queued = match intent {
        PendingInputIntent::AcquireClipboardImage => worker.clipboard_image(target),
        PendingInputIntent::PasteClipboard => worker.pending_paste(target),
        PendingInputIntent::AddFiles { paths } => worker.files(target, paths),
        PendingInputIntent::RemoveAttachment { attachment_id } => {
            worker.remove(target, attachment_id)
        }
        PendingInputIntent::Send { input } | PendingInputIntent::Retry { input } => {
            worker.send(target, input)
        }
    };
    if !queued {
        send(sink, BackendEvent::Backpressure);
    }
    Ok(())
}

fn route_clipboard<S: BackendSink>(
    sink: &S,
    active: &Option<Active>,
    worker: &attachments::Worker,
    action: koru_session_view::ClipboardAction,
) -> Result<(), ClientError> {
    let queued = match action {
        koru_session_view::ClipboardAction::Copy(text) => worker.copy_text(text),
        koru_session_view::ClipboardAction::Paste => {
            let Some(current) = active.as_ref() else {
                return Ok(());
            };
            worker.terminal_paste(attachments::Target {
                session_id: current.session_id,
                lease: current.lease.token,
            })
        }
    };
    if !queued {
        send(sink, BackendEvent::Backpressure);
    }
    Ok(())
}

fn detach(client: &SessionClient, active: &mut Option<Active>) -> Result<(), ClientError> {
    let Some(current) = active.take() else {
        return Ok(());
    };
    let result = if let Some(managed) = current.managed_client {
        let result = managed.detach(current.session_id, current.lease.token);
        managed.disconnect();
        result
    } else {
        client.detach(current.session_id, current.lease.token)
    };
    match result {
        Err(ClientError::Session(failure))
            if matches!(
                failure.code,
                ErrorCode::SessionNotFound
                    | ErrorCode::SessionExited
                    | ErrorCode::ControllerRequired
                    | ErrorCode::StaleLease
            ) =>
        {
            Ok(())
        }
        result => result,
    }
}

fn terminate(client: &SessionClient, active: &mut Option<Active>) -> Result<(), ClientError> {
    let current = active
        .as_ref()
        .ok_or_else(|| ClientError::Session(failure(ErrorCode::ControllerRequired)))?;
    let session_id = current.session_id;
    let lease = current.lease.token;
    let managed = current.managed_client.as_ref().map(Arc::clone);
    let command_client = managed.as_deref().unwrap_or(client);
    command_client.command(SessionCommand {
        idempotency_key: IdempotencyKey::mint(),
        session_id,
        controller_lease: lease,
        action: koru_session_protocol::RawCommand::Terminate,
    })?;
    let _ = active.take().expect("active session was checked above");
    // A Terminal window owns only the controller. Releasing it immediately
    // makes the row and a subsequent window deterministic while the host
    // observes the asynchronous process exit.
    if let Err(error) = command_client.detach(session_id, lease) {
        let failure = client_failure(error);
        if !matches!(
            failure.code,
            ErrorCode::SessionNotFound
                | ErrorCode::SessionExited
                | ErrorCode::ControllerRequired
                | ErrorCode::StaleLease
        ) {
            log_failure("detach_after_terminate", Some(session_id), &failure);
        }
    }
    if let Some(managed) = managed {
        managed.disconnect();
    }
    Ok(())
}

fn attach_raw(
    client: &SessionClient,
    session_id: SessionId,
    viewport_generation: ViewportGeneration,
    columns: u16,
    overscan_rows: u16,
) -> Result<(ControllerLease, Subscription), ClientError> {
    client.attach_live(session_id, viewport_generation, columns, overscan_rows)
}

fn send_raw(
    client: &SessionClient,
    session_id: SessionId,
    lease: koru_session_protocol::ControllerLeaseToken,
    action: koru_session_protocol::RawCommand,
) -> Result<(), ClientError> {
    client.command(SessionCommand {
        idempotency_key: IdempotencyKey::mint(),
        session_id,
        controller_lease: lease,
        action,
    })
}

fn route_intent<S: BackendSink>(
    client: &Arc<SessionClient>,
    sink: &S,
    internal: &SyncSender<BackendCommand>,
    active: &mut Option<Active>,
    intent: TerminalIntent,
) -> Result<(), ClientError> {
    let Some(current) = active.as_mut() else {
        return Ok(());
    };
    if !matches!(current.mode, SessionMode::RawPty) {
        return Ok(());
    }
    match intent {
        TerminalIntent::Raw(action) => {
            if let koru_session_protocol::RawCommand::Resize { size } = action {
                current.columns = size.columns;
                current.resize_in_flight = Some(size);
                let result = send_raw(
                    client,
                    current.session_id,
                    current.lease.token,
                    koru_session_protocol::RawCommand::Resize { size },
                );
                if result.is_err() && current.resize_in_flight == Some(size) {
                    current.resize_in_flight = None;
                }
                return result;
            }
            send_raw(client, current.session_id, current.lease.token, action)
        }
        TerminalIntent::SetViewport {
            rows,
            columns,
            overscan_rows,
        } => {
            current.columns = columns;
            current.pending_viewport = Some((rows, columns, overscan_rows));
            retry_pending_viewport(client, internal, current)
        }
        TerminalIntent::RequestScrollback { range } => {
            let Some(terminal_generation) = current.terminal_generation else {
                return Ok(());
            };
            let page = client.scrollback_page(
                current.session_id,
                current.lease.token,
                terminal_generation,
                current.viewport_generation,
                range,
            )?;
            send(sink, BackendEvent::Scrollback(page));
            Ok(())
        }
        TerminalIntent::RecoverSnapshot => {
            client.recover_snapshot(current.session_id, current.lease.token)
        }
    }
}

fn route_managed_intent<S: BackendSink>(
    sink: &S,
    active: &Option<Active>,
    intent: ManagedIntent,
) -> Result<(), ClientError> {
    let Some(current) = active.as_ref() else {
        return Ok(());
    };
    let Some(client) = current.managed_client.as_ref() else {
        // A managed intent is never translated into raw terminal bytes.
        return Ok(());
    };
    let result = match intent {
        ManagedIntent::AnswerApproval {
            approval_id,
            decision,
            choice,
        } => client
            .answer_approval(
                current.session_id,
                current.lease.token,
                IdempotencyKey::mint(),
                approval_id,
                decision,
                choice,
            )
            .map_err(|error| (Some(approval_id), error)),
        ManagedIntent::CancelTurn { turn_id } => client
            .cancel_turn(
                current.session_id,
                current.lease.token,
                IdempotencyKey::mint(),
                turn_id,
            )
            .map_err(|error| (None, error)),
        ManagedIntent::AttentionPresented { through } => client
            .acknowledge_attention(current.session_id, current.lease.token, through)
            .map_err(|error| (None, error)),
    };
    match result {
        Ok(()) => Ok(()),
        Err((approval_id, error)) => {
            let failure = client_failure(error);
            log_failure("managed_input", Some(current.session_id), &failure);
            send(
                sink,
                BackendEvent::ManagedCommandFailure {
                    session_id: current.session_id,
                    epoch: current.epoch,
                    approval_id,
                    code: failure.code,
                },
            );
            Ok(())
        }
    }
}

fn observe_render<S: BackendSink>(
    client: &Arc<SessionClient>,
    sink: &S,
    internal: &SyncSender<BackendCommand>,
    active: &mut Option<Active>,
    epoch: u64,
    update: &RenderUpdate,
) -> Result<(), ClientError> {
    let Some(current) = active.as_mut() else {
        return Ok(());
    };
    if current.epoch != epoch {
        return Ok(());
    }
    let (session_id, terminal_generation, viewport_generation) = match update {
        RenderUpdate::Snapshot { snapshot } => (
            snapshot.session_id,
            snapshot.terminal_generation,
            snapshot.viewport_generation,
        ),
        RenderUpdate::Delta { delta } => (
            delta.session_id,
            delta.terminal_generation,
            delta.viewport_generation,
        ),
        RenderUpdate::Gap { gap } => (
            gap.session_id,
            gap.terminal_generation,
            gap.viewport_generation,
        ),
    };
    if session_id != current.session_id {
        return Ok(());
    }
    // Replacing a viewport closes its old subscription, but an already queued
    // update can still reach this command owner. Never let that retired reader
    // move either generation backward: the next viewport would reuse an old
    // generation and the Host would correctly reject it as stale.
    if !advance_render_generations(
        &mut current.terminal_generation,
        &mut current.viewport_generation,
        terminal_generation,
        viewport_generation,
    ) {
        return Ok(());
    }
    if current
        .resize_in_flight
        .is_some_and(|expected| render_grid_size(update) == Some(expected))
    {
        current.resize_in_flight = None;
    }
    retry_pending_viewport(client, internal, current)?;
    // The command owner is the only place that knows which reader still owns
    // the active lease. Publishing here prevents a final update from a
    // detached reader repainting the newly selected session.
    send(sink, BackendEvent::Render(update.clone()));
    Ok(())
}

fn advance_render_generations(
    current_terminal: &mut Option<TerminalGeneration>,
    current_viewport: &mut ViewportGeneration,
    incoming_terminal: TerminalGeneration,
    incoming_viewport: ViewportGeneration,
) -> bool {
    if incoming_viewport < *current_viewport
        || current_terminal.is_some_and(|generation| incoming_terminal < generation)
    {
        return false;
    }
    *current_terminal = Some(incoming_terminal);
    *current_viewport = incoming_viewport;
    true
}

fn retry_pending_viewport(
    client: &Arc<SessionClient>,
    internal: &SyncSender<BackendCommand>,
    current: &mut Active,
) -> Result<(), ClientError> {
    let Some((rows, columns, overscan_rows)) = current.pending_viewport else {
        return Ok(());
    };
    if current.terminal_generation.is_none() {
        return Ok(());
    }
    if current.resize_in_flight.is_some() {
        return Ok(());
    }
    match set_viewport(client, internal, current, rows, columns, overscan_rows) {
        Ok(()) => {
            if current.pending_viewport == Some((rows, columns, overscan_rows)) {
                current.pending_viewport = None;
            }
            Ok(())
        }
        Err(error) if retryable_viewport_error(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

fn retryable_viewport_error(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Session(SessionFailure {
            code: ErrorCode::InvalidSize | ErrorCode::StaleGeneration,
            ..
        })
    )
}

fn set_viewport(
    client: &Arc<SessionClient>,
    internal: &SyncSender<BackendCommand>,
    current: &mut Active,
    rows: RowRange,
    columns: u16,
    overscan_rows: u16,
) -> Result<(), ClientError> {
    let Some(terminal_generation) = current.terminal_generation else {
        current.pending_viewport = Some((rows, columns, overscan_rows));
        return Ok(());
    };
    let subscription = subscribe_next_viewport(&mut current.viewport_generation, |generation| {
        client.set_viewport(
            current.session_id,
            current.lease.token,
            ViewportRequest {
                terminal_generation,
                viewport_generation: generation,
                rows,
                columns,
                overscan_rows,
            },
        )
    })?;
    spawn_render_reader(
        Arc::clone(client),
        internal.clone(),
        current.generation,
        current.session_id,
        current.epoch,
        subscription,
    );
    Ok(())
}

fn subscribe_next_viewport<T>(
    current: &mut ViewportGeneration,
    subscribe: impl FnOnce(ViewportGeneration) -> Result<T, ClientError>,
) -> Result<T, ClientError> {
    let next = current
        .next()
        .ok_or(ClientError::Session(failure(ErrorCode::InvalidRequest)))?;
    let subscription = subscribe(next)?;
    // The old render stream remains authoritative when the Host rejects a
    // replacement (for example, because ConPTY repainted during resize). Do
    // not retire its generation locally until the Host has committed the new
    // subscription; otherwise every valid old-stream update is discarded and
    // the surface appears frozen until reattach.
    *current = next;
    Ok(subscription)
}

/// Take a lifecycle subscription for every catalog session this connection
/// does not hold one for yet, stamped with the connection generation.
fn subscribe_new(
    client: &Arc<SessionClient>,
    internal: &SyncSender<BackendCommand>,
    generation: u64,
    subscribed: &mut BTreeSet<SessionId>,
    sessions: &[SessionSummary],
) {
    for session in sessions {
        let _ = subscribe_session(client, internal, generation, subscribed, session.id);
    }
}

fn subscribe_session(
    client: &Arc<SessionClient>,
    internal: &SyncSender<BackendCommand>,
    generation: u64,
    subscribed: &mut BTreeSet<SessionId>,
    session: SessionId,
) -> Result<(), ClientError> {
    if !subscribed.contains(&session) {
        let subscription = client.subscribe_events_from(session, None)?;
        spawn_event_reader(
            Arc::clone(client),
            internal.clone(),
            generation,
            session,
            subscription,
            None,
        );
        subscribed.insert(session);
    }
    Ok(())
}

fn refresh<S: BackendSink>(
    client: &Arc<SessionClient>,
    sink: &S,
    internal: &SyncSender<BackendCommand>,
    generation: u64,
    subscribed: &mut BTreeSet<SessionId>,
    initial: bool,
) {
    match client.list() {
        Ok(sessions) => {
            subscribe_new(client, internal, generation, subscribed, &sessions);
            send(
                sink,
                if initial {
                    BackendEvent::Connected(sessions)
                } else {
                    BackendEvent::Sessions(sessions)
                },
            );
            // The tree's catalogue, and Welcome's recents. A host that has not
            // negotiated `projects` answers an error, which is not a failure
            // worth a notice: the list is simply empty.
            let _ = publish_projects(client, sink);
        }
        Err(error) => {
            let failure = client_failure(error);
            log_failure("refresh", None, &failure);
            send(sink, BackendEvent::Failure(failure));
        }
    }
}

/// Re-read the project catalogue and push it at the window.
///
/// **`ListProjects`, never the event's payload.** `ProjectsChanged` is
/// invalidation and carries nothing (`CatalogEvent` is an enum with one arm and
/// no fields, deliberately), so the only thing a holder can do with one is ask
/// again. That is what makes the stream bounded: a queue that is already full
/// may drop a notification without a reader missing anything.
fn publish_projects<S: BackendSink>(client: &SessionClient, sink: &S) -> Result<(), ClientError> {
    let projects = client.list_projects()?;
    send(sink, BackendEvent::Projects(projects));
    Ok(())
}

/// **The named catalog subscription (ADR-264).**
///
/// One per window, held for the connection's life, and it is what makes two
/// windows agree about projects with no polling and no second sync mechanism: a
/// mutation lands at the session host, the host publishes, and every window
/// re-reads. It carries no lease and no session, so it costs one parked thread.
///
/// A host that has not negotiated `projects` refuses the subscribe, which is
/// not a failure worth reporting: that host has no projects to change.
fn spawn_projects_reader<S: BackendSink>(
    client: Arc<SessionClient>,
    sink: S,
    internal: SyncSender<BackendCommand>,
    generation: u64,
    subscription: Subscription,
) {
    let _ = std::thread::Builder::new()
        .name("koru-session-backend project catalog".into())
        .stack_size(128 * 1024)
        .spawn(move || {
            loop {
                match subscription.recv_timeout(SUBSCRIPTION_WAIT) {
                    Ok(SubscriptionUpdate::Catalog { .. }) => {
                        if publish_projects(&client, &sink).is_err() {
                            // A re-read that fails because the transport died
                            // is a connection loss, not a catalog answer.
                            signal_connection_lost(&client, &internal, generation);
                            break;
                        }
                    }
                    Ok(SubscriptionUpdate::Closed { error }) => {
                        if let Some(error) = error {
                            log_failure("projects_subscription", None, &error);
                        }
                        break;
                    }
                    Ok(_) => {}
                    Err(ClientError::TimedOut) => continue,
                    Err(_) => {
                        // This subscription lives for the connection's life, so
                        // its death *is* the connection's — which makes it the
                        // loss detector for a window with nothing attached.
                        signal_connection_lost(&client, &internal, generation);
                        break;
                    }
                }
            }
        });
}

/// A reader thread's only move on transport death: report the loss to the
/// command thread, stamped with the generation it was spawned for, and exit
/// quietly. The command thread owns recovery and drops superseded stamps; a
/// full internal queue is fine to fail on, because the commands ahead of the
/// signal will observe the same dead client themselves.
fn signal_connection_lost(
    client: &SessionClient,
    internal: &SyncSender<BackendCommand>,
    generation: u64,
) {
    if client.state() == ConnectionState::Disconnected {
        let _ = internal.try_send(BackendCommand::ConnectionLost { generation });
    }
}

fn spawn_render_reader(
    client: Arc<SessionClient>,
    internal: SyncSender<BackendCommand>,
    generation: u64,
    session_id: SessionId,
    epoch: u64,
    subscription: Subscription,
) {
    let _ = std::thread::Builder::new()
        .name("koru-session-backend render subscription".into())
        .stack_size(128 * 1024)
        .spawn(move || {
            loop {
                match subscription.recv_timeout(SUBSCRIPTION_WAIT) {
                    Ok(SubscriptionUpdate::Render { update }) => {
                        if internal
                            .send(BackendCommand::ObservedRender { epoch, update })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(SubscriptionUpdate::Closed { error }) => {
                        if let Some(error) = error {
                            log_failure("render_subscription", Some(session_id), &error);
                        }
                        let _ = internal.send(BackendCommand::Refresh);
                        break;
                    }
                    Ok(_) => {}
                    Err(ClientError::TimedOut) => continue,
                    Err(_) => {
                        // The transport died under this reader. The command
                        // thread owns recovery; the reader reports, stamped
                        // with its generation, and exits quietly.
                        signal_connection_lost(&client, &internal, generation);
                        break;
                    }
                }
            }
        });
}

/// **The lightweight per-session status subscription (R2).**
///
/// One of these per session, held whether or not its tab is the active one, and
/// it is what makes an inactive tab truthful: it takes no controller lease, no
/// render subscription and no output stream — `SubscribeEvents` is refused by
/// nothing but `SessionNotFound` and the host checks no lease for it. What it
/// carries is lifecycle and attention: started, exited, failed, bell, title,
/// approval required, attention changed. Everything an inactive tab shows.
///
/// **The numbers come from `ListSessions`, not from the event.** An event is an
/// invalidation token here: `AttentionChanged` is published only when the
/// *reason* changes, so an unread count that keeps climbing produces no further
/// events, and a reader that trusted the payload would freeze at two.
///
/// ## The gap, and why it is a resubscribe rather than a refresh
///
/// A host-side event subscriber that overflows is marked `gapped` and **is
/// never un-marked** — only the render subscriber's flag is reset, in
/// `recover_snapshot`. So a queue that fills once is retained and permanently
/// silent, and the tab it feeds stops updating with no error anywhere. Asking
/// for a `Refresh` (which is what this did) repaints once and then goes quiet
/// forever.
///
/// The recovery the protocol offers is a *new* subscription from the last
/// sequence seen, which is exactly what `SubscribeEventsFrom` exists for
/// (`worker.rs` compares the cursor under the same lock and pushes an immediate
/// `Gap`). So a gap ends this reader and asks the run loop for a fresh one.
fn spawn_event_reader(
    client: Arc<SessionClient>,
    internal: SyncSender<BackendCommand>,
    generation: u64,
    session_id: SessionId,
    subscription: Subscription,
    from: Option<koru_session_protocol::StreamSequence>,
) {
    let _ = std::thread::Builder::new()
        .name("koru-session-backend lifecycle subscription".into())
        .stack_size(96 * 1024)
        .spawn(move || {
            let _ = from;
            loop {
                match subscription.recv_timeout(SUBSCRIPTION_WAIT) {
                    Ok(SubscriptionUpdate::Event { event }) => {
                        if !matches!(
                            event.event,
                            koru_session_protocol::SessionEvent::Output { .. }
                        ) && internal.send(BackendCommand::Refresh).is_err()
                        {
                            break;
                        }
                    }
                    Ok(SubscriptionUpdate::EventGap { actual, .. }) => {
                        // The host has stopped publishing to this subscriber for
                        // good. Take a new one from **the sequence the gap
                        // names** — not from the last event this reader saw,
                        // which is behind it by however much was dropped —
                        // because without this the tab is silent for the rest
                        // of its life.
                        let resume = Some(actual);
                        let _ = internal.try_send(BackendCommand::Refresh);
                        match client.subscribe_events_from(session_id, resume) {
                            Ok(next) => {
                                spawn_event_reader(
                                    client, internal, generation, session_id, next, resume,
                                );
                            }
                            Err(error) => {
                                log_failure(
                                    "event_subscription",
                                    Some(session_id),
                                    &client_failure(error),
                                );
                            }
                        }
                        break;
                    }
                    // `Closed` is the host ending *this stream* (the session is
                    // gone); a dead transport is the *connection* ending, which
                    // the command thread recovers from.
                    Ok(SubscriptionUpdate::Closed { .. }) => break,
                    Err(ClientError::Disconnected) => {
                        signal_connection_lost(&client, &internal, generation);
                        break;
                    }
                    Err(ClientError::TimedOut) => continue,
                    Ok(_) | Err(_) => {}
                }
            }
        });
}

fn spawn_managed_reader<S: BackendSink>(
    client: Arc<SessionClient>,
    sink: S,
    session_id: SessionId,
    epoch: u64,
    subscription: Subscription,
) {
    let _ = std::thread::Builder::new()
        .name("koru-session-backend managed subscription".into())
        .stack_size(128 * 1024)
        .spawn(move || {
            'reader: while let Ok(update) = subscription.recv() {
                match update {
                    SubscriptionUpdate::AgentEvent { event } => {
                        // One window event carries a bounded burst. The shared
                        // view then coalesces adjacent deltas before shaping.
                        let mut events = Vec::with_capacity(16);
                        events.push(event);
                        let deadline = Instant::now() + Duration::from_millis(8);
                        while events.len() < 64 {
                            let Some(remaining) = deadline.checked_duration_since(Instant::now())
                            else {
                                break;
                            };
                            match subscription.recv_timeout(remaining) {
                                Ok(SubscriptionUpdate::AgentEvent { event }) => events.push(event),
                                Ok(SubscriptionUpdate::AgentEventGap { expected, actual }) => {
                                    let _ = sink.deliver(BackendEvent::ManagedBatch {
                                        session_id,
                                        epoch,
                                        events,
                                        earlier_truncated: false,
                                    });
                                    let _ = sink.deliver(BackendEvent::ManagedGap {
                                        session_id,
                                        epoch,
                                        expected,
                                        actual,
                                    });
                                    events = Vec::new();
                                    break;
                                }
                                Ok(SubscriptionUpdate::Closed { error }) => {
                                    if let Some(error) = error {
                                        log_failure(
                                            "managed_subscription",
                                            Some(session_id),
                                            &error,
                                        );
                                        let _ = sink.deliver(BackendEvent::ManagedStreamFailure {
                                            session_id,
                                            epoch,
                                            failure: error,
                                        });
                                    }
                                    break 'reader;
                                }
                                Err(ClientError::TimedOut) => break,
                                Ok(_) | Err(_) => break,
                            }
                        }
                        if !events.is_empty()
                            && !sink.deliver(BackendEvent::ManagedBatch {
                                session_id,
                                epoch,
                                events,
                                earlier_truncated: false,
                            })
                        {
                            break;
                        }
                    }
                    SubscriptionUpdate::AgentEventGap { expected, actual } => {
                        if !sink.deliver(BackendEvent::ManagedGap {
                            session_id,
                            epoch,
                            expected,
                            actual,
                        }) {
                            break;
                        }
                    }
                    SubscriptionUpdate::Closed { error } => {
                        if let Some(error) = error {
                            log_failure("managed_subscription", Some(session_id), &error);
                            let _ = sink.deliver(BackendEvent::ManagedStreamFailure {
                                session_id,
                                epoch,
                                failure: error,
                            });
                        }
                        break;
                    }
                    _ => {}
                }
            }
            client.disconnect();
        });
}

/// The one funnel every event leaves through. Its counterpart used to be a
/// `winit` proxy; the sink is what makes the crate surface-neutral.
fn send<S: BackendSink>(sink: &S, event: BackendEvent) {
    let _ = sink.deliver(event);
}

fn report_failure<S: BackendSink>(
    sink: &S,
    operation: &'static str,
    session_id: Option<SessionId>,
    error: ClientError,
) {
    let failure = client_failure(error);
    log_failure(operation, session_id, &failure);
    send(
        sink,
        BackendEvent::OperationFailure {
            operation,
            session_id,
            failure,
        },
    );
}

fn log_failure(operation: &'static str, session_id: Option<SessionId>, failure: &SessionFailure) {
    tracing::warn!(
        target: "koru_session_backend",
        operation,
        code = %closed_error_code(&failure.code),
        session_id = ?session_id,
        "session operation failed"
    );
}

pub(crate) fn closed_error_code(code: &ErrorCode) -> &str {
    match code {
        ErrorCode::Unknown(_) => "unknown",
        code => code.as_str(),
    }
}

fn render_session_id(update: &RenderUpdate) -> SessionId {
    match update {
        RenderUpdate::Snapshot { snapshot } => snapshot.session_id,
        RenderUpdate::Delta { delta } => delta.session_id,
        RenderUpdate::Gap { gap } => gap.session_id,
    }
}

fn render_grid_size(update: &RenderUpdate) -> Option<GridSize> {
    match update {
        RenderUpdate::Snapshot { snapshot } => Some(snapshot.size),
        RenderUpdate::Delta { .. } | RenderUpdate::Gap { .. } => None,
    }
}

pub(crate) fn client_failure(error: ClientError) -> SessionFailure {
    match error {
        ClientError::Session(failure) => failure,
        ClientError::Protocol(_) => failure(ErrorCode::ProtocolIncompatible),
        ClientError::Disconnected | ClientError::Ipc(_) | ClientError::Handshake(_) => {
            failure(ErrorCode::HostUnavailable)
        }
        ClientError::TimedOut => failure(ErrorCode::HostUnavailable),
        ClientError::UnexpectedResponse => failure(ErrorCode::InvalidRequest),
    }
}

fn failure(code: ErrorCode) -> SessionFailure {
    let message = code.as_str().to_owned();
    SessionFailure { code, message }
}

pub fn direct_endpoint(root: Option<&Path>) -> Result<koru_ipc::Endpoint, koru_ipc::IpcError> {
    match root {
        Some(root) => koru_ipc::Endpoint::for_service_channel_root(
            koru_ipc::SESSION_HOST,
            koru_channel::segment(),
            root,
        ),
        None => koru_ipc::Endpoint::for_service(koru_ipc::SESSION_HOST, koru_channel::segment()),
    }
}

pub fn protocol_offer() -> ProtocolOffer {
    let capabilities = [
        capability::CONTROLLER_LEASES,
        capability::RENDER_DELTAS,
        capability::SCROLLBACK_PAGES,
        capability::RAW_WRITE,
        capability::TERMINAL_KEYS,
        capability::RESIZE,
        capability::INTERRUPT,
        capability::TERMINATE,
        capability::ATTACHMENTS,
        capability::QUOTED_PATH_DELIVERY,
        capability::PROFILE_LAUNCH,
        capability::MANAGED_SESSIONS,
        capability::MANAGED_EVENTS,
        capability::MANAGED_CANCEL,
        capability::MANAGED_APPROVAL,
        capability::DYNAMIC_APPROVALS,
        capability::PRESENTED_ATTENTION,
        capability::MANAGED_USAGE,
        capability::PROVIDER_READINESS,
        capability::PROVIDER_MODELS,
        capability::NATIVE_ATTACHMENT_DELIVERY,
        // The tree, every mutation verb, the named catalog subscription and
        // the atomic placement on both creation requests all ride this one
        // negotiated capability (SP-11).
        capability::PROJECTS,
        // The cursor form of the event subscription, so a status stream that
        // overflows can be resumed instead of going permanently silent — see
        // `spawn_event_reader`.
        capability::OBSERVATION_CURSORS,
        // **The delegation family** (ADR-341, v14), offered now that a
        // window can take a delegated session and give it back: the run
        // views' `Take the session` and `Give the session back` are
        // `request_control` and `return_control`, and the host refuses both
        // to a peer that did not negotiate this. The window still creates
        // no delegated session — that is the orchestrator's — and manual
        // managed profiles are untouched by the offer.
        capability::DELEGATION,
        // **Continue the last conversation** (ADR-474): a saved agent tab
        // offers to resume, and the host refuses that to a peer that did not
        // negotiate it — `capability_not_negotiated`, the failure a window
        // that forgot to offer it shows as `Session failed`.
        capability::LAUNCH_RESUME,
    ]
    .into_iter()
    .map(|name| CapabilityName::from_str(name).expect("built-in capability"));
    ProtocolOffer {
        versions: ProtocolRange {
            min: MIN_PROTOCOL_VERSION,
            max: MAX_PROTOCOL_VERSION,
        },
        capabilities: CapabilitySet::new(capabilities),
    }
}

#[cfg(test)]
mod tests;
