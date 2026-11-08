// SPDX-License-Identifier: MPL-2.0

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use koru_events::{Draft, TerminalSessionOutcome};
use koru_session_observability::{
    Counter, SessionEndedFact, SessionReattachedFact, SessionStartedFact, SessionStats,
    SessionTrace, metric_mode, metric_profile_kind, metric_provider_kind,
};
use koru_session_protocol::{
    AgentEvent, ApprovalDecision, ApprovalId, Attachment, AttachmentId, AttachmentStatus,
    AttentionReason, AttentionState, CancelReason, CapabilitySet, ControlRequestOutcome,
    ControllerLease, ControllerLeaseToken, ControllerState, DelegationLaunch, ErrorCode, GridSize,
    HandoverState, IdempotencyKey, LaunchSelection, ProcessState, ProfileKind, ProviderKind,
    RawCommand, RawOutputChunk, RenderUpdate, RowRange, ScrollbackPage, SequencedSessionEvent,
    SessionCommand, SessionEvent, SessionFailure, SessionId, SessionMode, SessionOrigin,
    SessionProfileKey, SessionSummary, StreamSequence, TakeoverUrgency, TerminalGeneration,
    ViewportGeneration, ViewportRequest,
};
use koru_terminal_emulator::{
    EmulatorEvent, MAX_PAGE_ROWS, TerminalEmulator, encode_focus, encode_key, encode_mouse,
    encode_paste,
};

mod agent_stream;
mod control;
mod delivery;
mod managed;
mod render;

pub use agent_stream::AgentEventUpdate;
use agent_stream::AgentStreamState;
pub use delivery::{AttachmentPathResolver, NoopAttachmentPathResolver, quote_path};
use delivery::{PendingSubmission, ReportedOutcome, ResolvedDelivery, resolve_delivery};

use control::{
    ControlQueue, control_loop, control_queue, reader_loop, release_start, wait_for_start,
};
use managed::{ManagedHandle, command_queue as managed_command_queue, spawn_worker};
use render::{
    apply_emulator_update, map_emulator, micros, publish_event_locked, publish_output, snapshot,
    validate_viewport,
};

use crate::providers::{AgentStart, ManagedProviderRegistry, ManagedProviderRequest};
use crate::stream::Queue;
use crate::{BoundedStream, PtyReader, PtyTransport, PtyWriter, SpawnRequest, TransportError};

const RENDER_QUEUE_CAPACITY: usize = 8;
const EVENT_QUEUE_CAPACITY: usize = 64;
const OUTPUT_QUEUE_CAPACITY: usize = 8;
const IDEMPOTENCY_HISTORY: usize = 1_024;

pub trait SessionLifecycleSink: Send + Sync + 'static {
    /// Must return without performing journal or network I/O on the caller.
    fn publish(&self, event: Draft);
}

/// Non-blocking lifecycle edge for session-owned external resources.
///
/// Implementations may update memory inline, but cleanup that touches disk must
/// be queued to a worker before returning.
pub trait SessionResourceLifecycle: Send + Sync + 'static {
    fn session_started(&self, session_id: SessionId);
    fn session_ended(&self, session_id: SessionId);
}

#[derive(Default)]
pub struct NoopLifecycleSink;

impl SessionLifecycleSink for NoopLifecycleSink {
    fn publish(&self, _event: Draft) {}
}

#[derive(Default)]
pub struct NoopResourceLifecycle;

impl SessionResourceLifecycle for NoopResourceLifecycle {
    fn session_started(&self, _session_id: SessionId) {}
    fn session_ended(&self, _session_id: SessionId) {}
}

#[derive(Clone)]
pub struct SessionRuntimeConfig {
    pub stats_enabled: bool,
    pub trace: SessionTrace,
    pub lifecycle: Arc<dyn SessionLifecycleSink>,
    pub resources: Arc<dyn SessionResourceLifecycle>,
    pub attachment_paths: Arc<dyn AttachmentPathResolver>,
    pub managed_providers: Arc<ManagedProviderRegistry>,
}

impl Default for SessionRuntimeConfig {
    fn default() -> Self {
        Self {
            stats_enabled: false,
            trace: SessionTrace::new(false),
            lifecycle: Arc::new(NoopLifecycleSink),
            resources: Arc::new(NoopResourceLifecycle),
            attachment_paths: Arc::new(NoopAttachmentPathResolver),
            managed_providers: Arc::new(ManagedProviderRegistry::default()),
        }
    }
}

impl SessionRuntimeConfig {
    pub fn from_env(lifecycle: Arc<dyn SessionLifecycleSink>) -> Self {
        Self {
            stats_enabled: SessionStats::from_env().enabled(),
            trace: SessionTrace::from_env(),
            lifecycle,
            resources: Arc::new(NoopResourceLifecycle),
            attachment_paths: Arc::new(NoopAttachmentPathResolver),
            managed_providers: Arc::new(ManagedProviderRegistry::default()),
        }
    }

    pub fn with_resources(mut self, resources: Arc<dyn SessionResourceLifecycle>) -> Self {
        self.resources = resources;
        self
    }

    pub fn with_attachment_paths(mut self, paths: Arc<dyn AttachmentPathResolver>) -> Self {
        self.attachment_paths = paths;
        self
    }

    pub fn with_managed_providers(mut self, providers: Arc<ManagedProviderRegistry>) -> Self {
        self.managed_providers = providers;
        self
    }
}

#[derive(Clone)]
pub enum SessionLaunch {
    Raw(SpawnRequest),
    Managed {
        provider: ProviderKind,
        executable: OsString,
        /// Present on a delegated launch; the adapter renders it privately.
        /// `None` is a manual managed profile, untouched by delegation.
        delegation: Option<DelegationLaunch>,
        /// An explicit model and effort for a manual managed session
        /// (protocol v16). The adapter acknowledges it before the first
        /// prompt or refuses; it never silently runs the default instead.
        selection: Option<LaunchSelection>,
        /// MCP servers the agent starts beside itself (a chat's desktop
        /// bridge); empty for every other launch.
        mcp_servers: Vec<crate::providers::McpServerLaunch>,
        /// The creator draws an agent's questions (protocol v21, koru-ai
        /// D108); every other launch's agent declines them.
        questions: bool,
    },
}

impl SessionLaunch {
    pub fn mode(&self) -> SessionMode {
        match self {
            Self::Raw(_) => SessionMode::RawPty,
            Self::Managed { provider, .. } => SessionMode::Managed {
                provider: provider.clone(),
            },
        }
    }
}

impl std::fmt::Debug for SessionLaunch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Raw(_) => formatter.write_str("SessionLaunch::Raw(<redacted>)"),
            Self::Managed { provider, .. } => formatter
                .debug_struct("SessionLaunch::Managed")
                .field("provider", provider)
                .finish_non_exhaustive(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionCreate {
    pub profile: SessionProfileKey,
    pub parent_session_id: Option<SessionId>,
    pub profile_kind: ProfileKind,
    pub display_name: String,
    pub working_directory: PathBuf,
    pub attachment_delivery: koru_session_protocol::AttachmentDeliveryPolicy,
    pub capabilities: CapabilitySet,
    pub launch: SessionLaunch,
    pub created_at_ms: i64,
    /// What creates this session (protocol v16). Stamped on the summary and
    /// on nothing else here: ownership is the registry's bookkeeping.
    pub origin: SessionOrigin,
}

impl SessionCreate {
    pub fn validate(&self) -> Result<(), ErrorCode> {
        let launch_valid = match &self.launch {
            SessionLaunch::Raw(spawn) => {
                self.working_directory == spawn.cwd && spawn.validate().is_ok()
            }
            SessionLaunch::Managed { executable, .. } => {
                self.working_directory.is_absolute() && !executable.is_empty()
            }
        };
        if self.display_name.is_empty() || self.display_name.chars().count() > 200 || !launch_valid
        {
            return Err(ErrorCode::InvalidRequest);
        }
        Ok(())
    }
}

pub struct ControllerAttachment {
    pub lease: ControllerLease,
    pub render: BoundedStream<RenderUpdate>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionTerminalState {
    pub terminal_generation: TerminalGeneration,
    pub retained: koru_terminal_emulator::RetainedRange,
    pub visible: RowRange,
    pub size: GridSize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RawOutputUpdate {
    Chunk(RawOutputChunk),
    Gap {
        expected: StreamSequence,
        actual: StreamSequence,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEventUpdate {
    Event(SequencedSessionEvent),
    Gap {
        expected: StreamSequence,
        actual: StreamSequence,
    },
}

struct RenderSubscriber {
    request: ViewportRequest,
    queue: Queue<RenderUpdate>,
    gapped: bool,
}

struct Controller {
    lease: ControllerLease,
    last_viewport_generation: ViewportGeneration,
    render: Option<RenderSubscriber>,
}

struct OutputSubscriber {
    queue: Queue<RawOutputUpdate>,
    gapped: bool,
}

struct EventSubscriber {
    queue: Queue<SessionEventUpdate>,
    gapped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SessionMutation {
    Raw(RawCommand),
    CancelTurn {
        turn_id: koru_session_protocol::AgentTurnId,
        reason: CancelReason,
    },
    Approval {
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        choice: Option<koru_session_protocol::ApprovalChoiceId>,
    },
    /// An answer to an agent's questions, or a skip (protocol v21).
    Questions {
        request_id: koru_session_protocol::QuestionRequestId,
        answers: Option<Vec<koru_session_protocol::QuestionAnswer>>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MutationState {
    InFlight,
    Succeeded,
}

struct MutationRecord {
    key: IdempotencyKey,
    mutation: SessionMutation,
    state: MutationState,
}

struct TerminalState {
    emulator: TerminalEmulator,
    terminal_generation: TerminalGeneration,
    render_sequence: StreamSequence,
}

#[derive(Clone, Copy)]
struct ManagedAttention {
    approval_id: ApprovalId,
    sequence: koru_session_protocol::StreamSequence,
}

struct State {
    summary: SessionSummary,
    terminal: Option<TerminalState>,
    event_sequence: StreamSequence,
    output_sequence: StreamSequence,
    agent_stream: AgentStreamState,
    agent_commands: Option<Vec<koru_session_protocol::AgentCommand>>,
    managed_attention: Option<ManagedAttention>,
    controller: Option<Controller>,
    had_controller: bool,
    /// Why each active turn is being cancelled, recorded when the cancel is
    /// accepted and stamped on the `TurnCancelled` the adapter reports,
    /// which knows nothing of reasons.
    cancel_reasons: BTreeMap<koru_session_protocol::AgentTurnId, CancelReason>,
    event_subscribers: Vec<EventSubscriber>,
    output_subscribers: Vec<OutputSubscriber>,
    idempotency: VecDeque<MutationRecord>,
    pending_inputs: BTreeMap<IdempotencyKey, PendingSubmission>,
    attachments: BTreeMap<AttachmentId, Attachment>,
}

impl State {
    fn raw_terminal(&self) -> Result<&TerminalState, ErrorCode> {
        self.terminal
            .as_ref()
            .ok_or(ErrorCode::CapabilityNotNegotiated)
    }

    fn raw_terminal_mut(&mut self) -> Result<&mut TerminalState, ErrorCode> {
        self.terminal
            .as_mut()
            .ok_or(ErrorCode::CapabilityNotNegotiated)
    }
}

enum SessionIo {
    Raw {
        writer: PtyWriter,
        control: ControlQueue,
    },
    Managed(ManagedHandle),
}

struct Shared {
    id: SessionId,
    state: Mutex<State>,
    /// Whether this session exists because a delegated run created it.
    /// A manual managed profile is not delegated, and neither is a raw
    /// terminal; the one rule this decides is who may end the session
    /// without holding its controller lease.
    delegated: bool,
    io: SessionIo,
    stats: SessionStats,
    trace: SessionTrace,
    lifecycle: Arc<dyn SessionLifecycleSink>,
    resources: Arc<dyn SessionResourceLifecycle>,
    attachment_paths: Arc<dyn AttachmentPathResolver>,
    attachment_delivery: ResolvedDelivery,
    started: Instant,
    ended: AtomicBool,
}

pub struct SessionHandle {
    shared: Arc<Shared>,
}

impl Clone for SessionHandle {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

pub(crate) struct SessionOwner {
    handle: SessionHandle,
    reader: Option<JoinHandle<()>>,
    control: Option<JoinHandle<()>>,
    managed: Option<JoinHandle<()>>,
}

impl SessionOwner {
    pub(crate) fn spawn(
        create: SessionCreate,
        config: SessionRuntimeConfig,
    ) -> Result<Self, SessionFailure> {
        create.validate().map_err(failure)?;
        let started = Instant::now();
        match &create.launch {
            SessionLaunch::Raw(spawn) => {
                let (transport, reader) = PtyTransport::spawn(spawn.clone()).map_err(|error| {
                    log_raw_launch_failure(&create, &error);
                    spawn_failure(error)
                })?;
                Self::start(create, config, transport, reader, started)
            }
            SessionLaunch::Managed {
                provider,
                executable,
                delegation,
                selection,
                mcp_servers,
                questions,
            } => Self::start_managed(
                create.clone(),
                config,
                provider.clone(),
                executable.clone(),
                delegation.clone(),
                selection.clone(),
                mcp_servers.clone(),
                *questions,
                started,
            ),
        }
    }

    fn start(
        create: SessionCreate,
        config: SessionRuntimeConfig,
        mut transport: PtyTransport,
        reader: PtyReader,
        started: Instant,
    ) -> Result<Self, SessionFailure> {
        let SessionLaunch::Raw(spawn) = &create.launch else {
            return Err(failure(ErrorCode::InvalidRequest));
        };
        let id = SessionId::mint();
        let emulator =
            TerminalEmulator::new(spawn.size).map_err(|_| failure(ErrorCode::InvalidSize))?;
        let writer = transport.writer();
        let mode = SessionMode::RawPty;
        let attachment_delivery =
            resolve_delivery(create.attachment_delivery, &mode, &create.capabilities);
        let (control_queue, control_receiver) = control_queue();
        let exit_queue = control_queue.clone();
        transport.set_exit_callback(Arc::new(move |result| exit_queue.exited(result)));
        let summary = SessionSummary {
            id,
            parent_session_id: create.parent_session_id,
            profile: create.profile,
            display_name: create.display_name,
            working_directory: Some(create.working_directory.to_string_lossy().into_owned()),
            mode: mode.clone(),
            process: ProcessState::Running,
            controller: ControllerState::Detached,
            managed: None,
            active_turn_id: None,
            attention: AttentionState::default(),
            pending_attachment_count: 0,
            capabilities: create.capabilities,
            // The runtime owns a process, never a folder: placement is the
            // host's project registry's answer and is stamped on the way out.
            project_id: None,
            handover: HandoverState::Idle,
            model: None,
            origin: create.origin,
            created_at_ms: create.created_at_ms,
        };
        let shared = Arc::new(Shared {
            delegated: false,
            id,
            state: Mutex::new(State {
                summary,
                terminal: Some(TerminalState {
                    emulator,
                    terminal_generation: counter(1),
                    render_sequence: counter(1),
                }),
                event_sequence: counter(1),
                output_sequence: counter(1),
                agent_stream: AgentStreamState::new(counter(1)),
                agent_commands: None,
                managed_attention: None,
                controller: None,
                had_controller: false,
                cancel_reasons: BTreeMap::new(),
                event_subscribers: Vec::new(),
                output_subscribers: Vec::new(),
                idempotency: VecDeque::with_capacity(IDEMPOTENCY_HISTORY),
                pending_inputs: BTreeMap::new(),
                attachments: BTreeMap::new(),
            }),
            io: SessionIo::Raw {
                writer,
                control: control_queue,
            },
            stats: SessionStats::new(config.stats_enabled),
            trace: config.trace,
            lifecycle: config.lifecycle,
            resources: config.resources,
            attachment_paths: config.attachment_paths,
            attachment_delivery,
            started,
            ended: AtomicBool::new(false),
        });

        let start_gate = Arc::new((Mutex::new(false), Condvar::new()));
        let reader_shared = Arc::clone(&shared);
        let reader_gate = Arc::clone(&start_gate);
        let reader_thread = std::thread::Builder::new()
            .name(format!("koru-session-reader-{id}"))
            .stack_size(512 * 1024)
            .spawn(move || {
                wait_for_start(&reader_gate);
                reader_loop(reader, reader_shared);
            })
            .map_err(|_| failure(ErrorCode::Internal))?;
        let control_shared = Arc::clone(&shared);
        let control_gate = Arc::clone(&start_gate);
        let control_thread = match std::thread::Builder::new()
            .name(format!("koru-session-control-{id}"))
            .stack_size(128 * 1024)
            .spawn(move || {
                wait_for_start(&control_gate);
                control_loop(transport, control_receiver, control_shared);
            }) {
            Ok(thread) => thread,
            Err(_) => {
                shared.terminate_backend();
                release_start(&start_gate);
                let _ = reader_thread.join();
                return Err(failure(ErrorCode::Internal));
            }
        };

        announce_started(&shared, &mode, create.profile_kind, started);
        release_start(&start_gate);

        Ok(Self {
            handle: SessionHandle {
                shared: Arc::clone(&shared),
            },
            reader: Some(reader_thread),
            control: Some(control_thread),
            managed: None,
        })
    }

    // The managed launch's own fields, each passed once from the one caller
    // that destructures `SessionLaunch::Managed`; a struct for them would be
    // a second copy of that variant.
    #[allow(clippy::too_many_arguments)]
    fn start_managed(
        create: SessionCreate,
        config: SessionRuntimeConfig,
        provider: ProviderKind,
        executable: OsString,
        delegation: Option<DelegationLaunch>,
        selection: Option<LaunchSelection>,
        mcp_servers: Vec<crate::providers::McpServerLaunch>,
        questions: bool,
        started: Instant,
    ) -> Result<Self, SessionFailure> {
        let id = SessionId::mint();
        let delegated = delegation.is_some();
        let resume = delegation.as_ref().and_then(|launch| launch.resume.clone());
        let mut managed = config
            .managed_providers
            .create(ManagedProviderRequest {
                provider: provider.clone(),
                executable,
                working_directory: create.working_directory.clone(),
                delegation,
                selection,
                mcp_servers,
                questions,
            })
            .map_err(|failure| {
                log_provider_launch_failure(&provider, &failure);
                provider_failure(failure)
            })?;
        let negotiated = managed
            .control
            .start(AgentStart {
                session_id: id,
                parent_session_id: create.parent_session_id,
                resume,
            })
            .map_err(|failure| {
                log_provider_launch_failure(&provider, &failure);
                provider_failure(failure)
            })?;
        if negotiated.provider != provider {
            let _ = managed.control.shutdown();
            return Err(failure(ErrorCode::ProviderIncompatible));
        }
        let model = Some(negotiated.delegation.economics.model.clone());
        let capabilities = negotiated.capabilities.intersection(&create.capabilities);
        let mode = SessionMode::Managed {
            provider: provider.clone(),
        };
        let attachment_delivery =
            resolve_delivery(create.attachment_delivery, &mode, &capabilities);
        let (managed_handle, managed_receiver) = managed_command_queue(managed.wake);
        let summary = SessionSummary {
            id,
            parent_session_id: create.parent_session_id,
            profile: create.profile,
            display_name: create.display_name,
            working_directory: Some(create.working_directory.to_string_lossy().into_owned()),
            mode: mode.clone(),
            process: ProcessState::Running,
            controller: ControllerState::Detached,
            managed: Some(koru_session_protocol::ManagedState::Idle),
            active_turn_id: None,
            attention: AttentionState::default(),
            pending_attachment_count: 0,
            capabilities,
            project_id: None,
            handover: HandoverState::Idle,
            model,
            origin: create.origin,
            created_at_ms: create.created_at_ms,
        };
        let shared = Arc::new(Shared {
            delegated,
            id,
            state: Mutex::new(State {
                summary,
                terminal: None,
                event_sequence: counter(1),
                output_sequence: counter(1),
                agent_stream: AgentStreamState::new(counter(1)),
                agent_commands: None,
                managed_attention: None,
                controller: None,
                had_controller: false,
                cancel_reasons: BTreeMap::new(),
                event_subscribers: Vec::new(),
                output_subscribers: Vec::new(),
                idempotency: VecDeque::with_capacity(IDEMPOTENCY_HISTORY),
                pending_inputs: BTreeMap::new(),
                attachments: BTreeMap::new(),
            }),
            io: SessionIo::Managed(managed_handle),
            stats: SessionStats::new(config.stats_enabled),
            trace: config.trace,
            lifecycle: config.lifecycle,
            resources: config.resources,
            attachment_paths: config.attachment_paths,
            attachment_delivery,
            started,
            ended: AtomicBool::new(false),
        });
        let start_gate = Arc::new((Mutex::new(false), Condvar::new()));
        let managed_thread = match spawn_worker(
            managed.control,
            managed_receiver,
            Arc::clone(&shared),
            Arc::clone(&start_gate),
        ) {
            Ok(thread) => thread,
            Err(_) => {
                shared.terminate_backend();
                release_start(&start_gate);
                return Err(failure(ErrorCode::Internal));
            }
        };
        announce_started(&shared, &mode, create.profile_kind, started);
        release_start(&start_gate);
        Ok(Self {
            handle: SessionHandle { shared },
            reader: None,
            control: None,
            managed: Some(managed_thread),
        })
    }

    pub(crate) fn handle(&self) -> SessionHandle {
        self.handle.clone()
    }

    pub(crate) fn shutdown(&mut self) {
        self.handle.shared.terminate_backend();
        if let Some(control) = self.control.take() {
            let _ = control.join();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(managed) = self.managed.take() {
            let _ = managed.join();
        }
    }
}

fn announce_started(
    shared: &Arc<Shared>,
    mode: &SessionMode,
    profile_kind: ProfileKind,
    started: Instant,
) {
    let provider = match mode {
        SessionMode::RawPty => None,
        SessionMode::Managed { provider } => Some(provider),
    };
    shared.resources.session_started(shared.id);
    let event = SessionStartedFact {
        session_id: shared.id,
        mode: metric_mode(mode),
        profile_kind: metric_profile_kind(profile_kind),
        provider_kind: metric_provider_kind(provider),
        elapsed_ms: millis(started.elapsed()),
    }
    .commit(shared.trace);
    shared.lifecycle.publish(event.into());
    shared.publish_event(SessionEvent::Started);
}

impl Drop for SessionOwner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl SessionHandle {
    pub fn id(&self) -> SessionId {
        self.shared.id
    }

    pub fn summary(&self) -> SessionSummary {
        self.shared.lock().summary.clone()
    }

    pub fn stats(&self) -> Option<koru_session_observability::StatsSnapshot> {
        self.shared.stats.snapshot()
    }

    pub fn terminal_state(&self) -> Result<SessionTerminalState, ErrorCode> {
        let state = self.shared.lock();
        let terminal = state
            .terminal
            .as_ref()
            .ok_or(ErrorCode::CapabilityNotNegotiated)?;
        Ok(SessionTerminalState {
            terminal_generation: terminal.terminal_generation,
            retained: terminal.emulator.retained_range(),
            visible: terminal.emulator.visible_range(),
            size: terminal.emulator.size(),
        })
    }

    pub fn is_ended(&self) -> bool {
        self.shared.ended.load(Ordering::Acquire)
    }

    pub fn attach(
        &self,
        request: ViewportRequest,
        granted_at_ms: i64,
    ) -> Result<ControllerAttachment, ErrorCode> {
        let mut state = self.shared.lock();
        if state.summary.process != ProcessState::Running {
            return Err(ErrorCode::SessionExited);
        }
        if state.controller.is_some() {
            return Err(ErrorCode::ControllerConflict);
        }
        let raw = state.terminal.is_some();
        if raw {
            validate_viewport(&state, request, false)?;
        } else {
            request.validate()?;
        }
        let (queue, stream) = Queue::new(RENDER_QUEUE_CAPACITY);
        if raw {
            let snapshot = snapshot(&mut state, request, self.shared.id, &self.shared.stats)?;
            let _ = queue.try_push(RenderUpdate::Snapshot { snapshot });
        }
        let lease = ControllerLease {
            token: ControllerLeaseToken::mint(),
            granted_at_ms,
        };
        let reattached = state.had_controller;
        state.had_controller = true;
        state.controller = Some(Controller {
            lease: lease.clone(),
            last_viewport_generation: request.viewport_generation,
            render: Some(RenderSubscriber {
                request,
                queue,
                gapped: false,
            }),
        });
        let cleared_attention = raw && state.summary.attention != AttentionState::default();
        state.summary.controller = ControllerState::Controlled;
        // A lease granted after a release is the handover happening; one
        // granted after a return is the original holder reclaiming. Any other
        // attach is not part of a handover and moves nothing.
        match state.summary.handover {
            HandoverState::Released => {
                move_handover(
                    &mut state,
                    self.shared.id,
                    HandoverState::Taken,
                    &self.shared.stats,
                );
            }
            HandoverState::Returned => {
                move_handover(
                    &mut state,
                    self.shared.id,
                    HandoverState::Idle,
                    &self.shared.stats,
                );
            }
            HandoverState::Idle | HandoverState::Requested | HandoverState::Taken => {}
        }
        if cleared_attention {
            state.summary.attention = AttentionState::default();
            publish_event_locked(
                &mut state,
                self.shared.id,
                SessionEvent::AttentionChanged {
                    attention: AttentionState::default(),
                },
                &self.shared.stats,
            );
        }
        drop(state);
        if reattached {
            let event = SessionReattachedFact {
                session_id: self.shared.id,
            }
            .commit(self.shared.trace);
            self.shared.lifecycle.publish(event.into());
        }
        Ok(ControllerAttachment {
            lease,
            render: stream,
        })
    }

    pub fn attach_live(
        &self,
        viewport_generation: ViewportGeneration,
        columns: u16,
        overscan_rows: u16,
        granted_at_ms: i64,
    ) -> Result<ControllerAttachment, ErrorCode> {
        let request = match self.terminal_state() {
            Ok(terminal) => ViewportRequest {
                terminal_generation: terminal.terminal_generation,
                viewport_generation,
                rows: terminal.visible,
                // A detached PTY keeps its last grid. A new window is free to
                // be wider, but it cannot request columns the emulator does
                // not own until its controller has delivered Resize. Attach
                // to the current bounded grid first; the ordinary resize path
                // expands it immediately afterward.
                columns: columns.min(terminal.size.columns),
                overscan_rows,
            },
            Err(ErrorCode::CapabilityNotNegotiated) => ViewportRequest {
                terminal_generation: counter(1),
                viewport_generation,
                rows: RowRange {
                    first: koru_session_protocol::RowIndex(0),
                    count: 1,
                },
                columns,
                overscan_rows: 0,
            },
            Err(error) => return Err(error),
        };
        self.attach(request, granted_at_ms)
    }

    /// Return one bounded grid observation without taking controller
    /// ownership or changing a controller's live viewport.
    pub fn snapshot(
        &self,
        viewport_generation: ViewportGeneration,
        range: RowRange,
        columns: u16,
    ) -> Result<koru_session_protocol::RenderSnapshot, ErrorCode> {
        let mut state = self.shared.lock();
        if state.terminal.is_none() {
            return Err(ErrorCode::CapabilityNotNegotiated);
        }
        let request = ViewportRequest {
            terminal_generation: state
                .terminal
                .as_ref()
                .expect("checked terminal")
                .terminal_generation,
            viewport_generation,
            rows: range,
            columns,
            overscan_rows: 0,
        };
        validate_viewport(&state, request, false)?;
        snapshot(&mut state, request, self.shared.id, &self.shared.stats)
    }

    /// Snapshot the bounded grid and both public-stream cursors under the same
    /// state lock. This closes the recovery race between observing content and
    /// learning where a replacement subscription must begin.
    pub fn observe_snapshot(
        &self,
        viewport_generation: ViewportGeneration,
        range: RowRange,
        columns: u16,
    ) -> Result<koru_session_protocol::SessionObservation, ErrorCode> {
        let mut state = self.shared.lock();
        if state.terminal.is_none() {
            return Err(ErrorCode::CapabilityNotNegotiated);
        }
        let request = ViewportRequest {
            terminal_generation: state
                .terminal
                .as_ref()
                .expect("checked terminal")
                .terminal_generation,
            viewport_generation,
            rows: range,
            columns,
            overscan_rows: 0,
        };
        validate_viewport(&state, request, false)?;
        let event_sequence = state.event_sequence;
        let output_sequence = state.output_sequence;
        let snapshot = snapshot(&mut state, request, self.shared.id, &self.shared.stats)?;
        Ok(koru_session_protocol::SessionObservation {
            snapshot,
            event_sequence,
            output_sequence,
        })
    }

    pub fn detach(&self, lease: ControllerLeaseToken) -> Result<(), ErrorCode> {
        let mut state = self.shared.lock();
        require_lease(&state, lease)?;
        drop_controller(&mut state);
        // A holder that goes away mid-request has released, and a person
        // whose window goes away has returned: the two crash shortcuts of
        // §7.4, taken by whichever detach the connection loss produces.
        match state.summary.handover {
            HandoverState::Requested => {
                move_handover(
                    &mut state,
                    self.shared.id,
                    HandoverState::Released,
                    &self.shared.stats,
                );
            }
            HandoverState::Taken => {
                move_handover(
                    &mut state,
                    self.shared.id,
                    HandoverState::Returned,
                    &self.shared.stats,
                );
            }
            HandoverState::Idle | HandoverState::Released | HandoverState::Returned => {}
        }
        Ok(())
    }

    /// Ask the controller's holder for the lease (§7.4). Release, not
    /// preemption: with no holder the caller may attach now; with one, the
    /// session reads `Requested`, the holder learns it from the event
    /// stream, and the caller waits for `Released`. `Now` ends the active
    /// managed turn with a takeover reason so the boundary arrives sooner.
    pub fn request_control(
        &self,
        urgency: TakeoverUrgency,
    ) -> Result<ControlRequestOutcome, ErrorCode> {
        let mut state = self.shared.lock();
        if state.summary.process != ProcessState::Running {
            return Err(ErrorCode::SessionExited);
        }
        if state.controller.is_none() {
            return Ok(ControlRequestOutcome::Available);
        }
        if state.summary.handover == HandoverState::Idle {
            move_handover(
                &mut state,
                self.shared.id,
                HandoverState::Requested,
                &self.shared.stats,
            );
        }
        let interrupt = match (urgency, state.summary.active_turn_id) {
            (TakeoverUrgency::Now, Some(turn_id)) => {
                record_cancel_reason(&mut state, turn_id, CancelReason::TakeoverBoundary);
                Some(turn_id)
            }
            _ => None,
        };
        let managed = match &self.shared.io {
            SessionIo::Managed(managed) if interrupt.is_some() => Some(managed.clone()),
            _ => None,
        };
        drop(state);
        if let (Some(turn_id), Some(managed)) = (interrupt, managed) {
            // The provider may have finished the turn already; a completion
            // that wins is the boundary arriving on its own.
            match managed.cancel(turn_id) {
                Ok(()) | Err(ErrorCode::TurnNotFound) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(ControlRequestOutcome::Waiting)
    }

    /// The holder gives its lease up for a handover. Like a detach, but the
    /// session reads `Released` whatever it read before, so the requester
    /// can tell a handover from a window closing.
    pub fn release_control(&self, lease: ControllerLeaseToken) -> Result<(), ErrorCode> {
        let mut state = self.shared.lock();
        require_lease(&state, lease)?;
        drop_controller(&mut state);
        // A person who releases the lease they were handed is returning it,
        // whatever verb they used; the cycle has one way forward from there.
        let next = match state.summary.handover {
            HandoverState::Taken => HandoverState::Returned,
            _ => HandoverState::Released,
        };
        move_handover(&mut state, self.shared.id, next, &self.shared.stats);
        Ok(())
    }

    /// The person gives the lease back. Only meaningful for a lease that
    /// was taken through a handover; anything else is an ordinary detach.
    pub fn return_control(&self, lease: ControllerLeaseToken) -> Result<(), ErrorCode> {
        let mut state = self.shared.lock();
        require_lease(&state, lease)?;
        if state.summary.handover != HandoverState::Taken {
            return Err(ErrorCode::InvalidTransition);
        }
        drop_controller(&mut state);
        move_handover(
            &mut state,
            self.shared.id,
            HandoverState::Returned,
            &self.shared.stats,
        );
        Ok(())
    }

    /// Keep controller ownership while suppressing all grid serialization.
    pub fn suspend_render(&self, lease: ControllerLeaseToken) -> Result<(), ErrorCode> {
        let mut state = self.shared.lock();
        let controller = require_lease_mut(&mut state, lease)?;
        if let Some(render) = controller.render.take() {
            render.queue.close();
        }
        Ok(())
    }

    pub fn set_viewport(
        &self,
        lease: ControllerLeaseToken,
        request: ViewportRequest,
    ) -> Result<BoundedStream<RenderUpdate>, ErrorCode> {
        let mut state = self.shared.lock();
        if state.terminal.is_none() {
            return Err(ErrorCode::CapabilityNotNegotiated);
        }
        require_lease(&state, lease)?;
        let previous = state
            .controller
            .as_ref()
            .map(|controller| controller.last_viewport_generation)
            .ok_or(ErrorCode::ControllerRequired)?;
        validate_viewport(&state, request, true)?;
        if request.viewport_generation <= previous {
            return Err(ErrorCode::StaleGeneration);
        }
        let (queue, stream) = Queue::new(RENDER_QUEUE_CAPACITY);
        let snapshot = snapshot(&mut state, request, self.shared.id, &self.shared.stats)?;
        let _ = queue.try_push(RenderUpdate::Snapshot { snapshot });
        let controller = require_lease_mut(&mut state, lease)?;
        controller.last_viewport_generation = request.viewport_generation;
        if let Some(old) = controller.render.replace(RenderSubscriber {
            request,
            queue,
            gapped: false,
        }) {
            old.queue.close();
        }
        Ok(stream)
    }

    pub fn recover_snapshot(&self, lease: ControllerLeaseToken) -> Result<(), ErrorCode> {
        let mut state = self.shared.lock();
        if state.terminal.is_none() {
            return Err(ErrorCode::CapabilityNotNegotiated);
        }
        require_lease(&state, lease)?;
        let request = state
            .controller
            .as_ref()
            .and_then(|controller| controller.render.as_ref())
            .map(|render| render.request)
            .ok_or(ErrorCode::InvalidRequest)?;
        let snapshot = snapshot(&mut state, request, self.shared.id, &self.shared.stats)?;
        let render = require_lease_mut(&mut state, lease)?
            .render
            .as_mut()
            .ok_or(ErrorCode::InvalidRequest)?;
        render.gapped = false;
        render.queue.clear();
        render
            .queue
            .try_push(RenderUpdate::Snapshot { snapshot })
            .map_err(|_| ErrorCode::Backpressure)?;
        Ok(())
    }

    /// End this delegated session because the run that created it was
    /// cancelled — **including while a person holds the controller lease**
    /// (delegation §7.6: *cancellation reaches the session-owned process
    /// tree including while a person holds control*).
    ///
    /// The lease belongs to whoever borrowed a step of the run; the session
    /// belongs to the run that created it, and ending the job is not the
    /// borrower's decision. A person who took control and walked away must
    /// not be able to hold a provider process open by keeping the lease.
    /// This is the same argument [`terminate_for_lifecycle`] makes, one step
    /// narrower: only a session Koru created for a run, and only the end.
    ///
    /// A session nobody delegated is refused with `Forbidden` — a manual
    /// managed profile and a raw terminal keep the lease rule exactly as
    /// they have it — and, like the lifecycle verb, this is idempotent: a
    /// session that has already exited answers `Ok(false)`.
    ///
    /// [`terminate_for_lifecycle`]: Self::terminate_for_lifecycle
    pub fn terminate_for_delegation(&self) -> Result<bool, ErrorCode> {
        if !self.shared.delegated {
            return Err(ErrorCode::Forbidden);
        }
        self.terminate_for_lifecycle()
    }

    /// End this session because the workspace it belongs to is going away.
    ///
    /// **The one mutation that does not take the controller lease**, and the
    /// reason the supervised lifecycle verb exists at all: the lease belongs to
    /// the window showing the terminal, and that window is exactly the party
    /// the operation is closing. Asking it to consent would be asking the thing
    /// being removed for permission.
    ///
    /// It is not reachable from the wire on its own — only through the session
    /// host's launch-authority-gated workspace verb — and it is idempotent: a
    /// session that has already exited answers `Ok(false)`, so a retry closes
    /// nothing and fails at nothing.
    pub fn terminate_for_lifecycle(&self) -> Result<bool, ErrorCode> {
        let state = self.shared.lock();
        if state.summary.process != ProcessState::Running {
            return Ok(false);
        }
        match &self.shared.io {
            SessionIo::Raw { control, .. } => {
                drop(state);
                control.terminate();
            }
            SessionIo::Managed(managed) => {
                let managed = managed.clone();
                drop(state);
                managed.terminate();
            }
        }
        Ok(true)
    }

    pub fn command(&self, command: SessionCommand) -> Result<(), ErrorCode> {
        if command.session_id != self.shared.id {
            return Err(ErrorCode::SessionNotFound);
        }
        command.action.validate()?;
        let mut state = self.shared.lock();
        require_lease(&state, command.controller_lease)?;
        if state.summary.process != ProcessState::Running {
            return Err(ErrorCode::SessionExited);
        }
        if begin_mutation(
            &mut state,
            command.idempotency_key,
            SessionMutation::Raw(command.action.clone()),
        )? {
            return Ok(());
        }

        let result = match &self.shared.io {
            SessionIo::Raw { writer, control } => {
                let modes = state
                    .terminal
                    .as_ref()
                    .expect("raw session owns terminal state")
                    .emulator
                    .modes();
                drop(state);
                let result = match &command.action {
                    RawCommand::WriteBytes { bytes } => writer.write_bytes(bytes),
                    RawCommand::WriteText { text } => writer.write_text(text),
                    RawCommand::SendKey { key } => {
                        let bytes = encode_key(key, modes)?;
                        writer.write_key(&bytes)
                    }
                    RawCommand::SendMouse { event } => match encode_mouse(*event, modes)? {
                        Some(bytes) => writer.write_key(&bytes),
                        None => Ok(()),
                    },
                    RawCommand::SetFocus { focused } => match encode_focus(*focused, modes) {
                        Some(bytes) => writer.write_key(bytes),
                        None => Ok(()),
                    },
                    RawCommand::PasteText { text } => {
                        let bytes = encode_paste(text, modes);
                        writer.write_bytes(&bytes)
                    }
                    RawCommand::Interrupt => writer.interrupt(),
                    RawCommand::Resize { size } => {
                        control.resize(*size);
                        Ok(())
                    }
                    RawCommand::Terminate => {
                        control.terminate();
                        Ok(())
                    }
                }
                .map_err(map_transport);
                if result.is_ok() {
                    self.shared.stats.add(
                        Counter::InputBytes,
                        input_bytes(&command.action).try_into().unwrap_or(u64::MAX),
                    );
                    self.shared.stats.observe_high_water(
                        Counter::QueueHighWater,
                        writer.queued_bytes().try_into().unwrap_or(u64::MAX),
                    );
                }
                result
            }
            SessionIo::Managed(managed) => {
                let operation = match command.action {
                    RawCommand::Interrupt => {
                        if !state
                            .summary
                            .capabilities
                            .supports(koru_session_protocol::capability::MANAGED_CANCEL)
                        {
                            Err(ErrorCode::CapabilityNotNegotiated)
                        } else {
                            state
                                .summary
                                .active_turn_id
                                .ok_or(ErrorCode::TurnNotFound)
                                .map(|turn_id| (managed.clone(), Some(turn_id)))
                        }
                    }
                    RawCommand::Terminate => Ok((managed.clone(), None)),
                    _ => Err(ErrorCode::CapabilityNotNegotiated),
                };
                drop(state);
                match operation {
                    Ok((managed, Some(turn_id))) => managed.cancel(turn_id),
                    Ok((managed, None)) => {
                        managed.terminate();
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
        };
        let mut state = self.shared.lock();
        finish_mutation(&mut state, command.idempotency_key, result.is_ok());
        result
    }

    pub fn answer_approval(
        &self,
        lease: ControllerLeaseToken,
        idempotency_key: IdempotencyKey,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        choice: Option<koru_session_protocol::ApprovalChoiceId>,
    ) -> Result<(), ErrorCode> {
        let mut state = self.shared.lock();
        require_lease(&state, lease)?;
        if state.summary.process != ProcessState::Running {
            return Err(ErrorCode::SessionExited);
        }
        if !state
            .summary
            .capabilities
            .supports(koru_session_protocol::capability::MANAGED_APPROVAL)
        {
            return Err(ErrorCode::CapabilityNotNegotiated);
        }
        if begin_mutation(
            &mut state,
            idempotency_key,
            SessionMutation::Approval {
                approval_id,
                decision,
                choice,
            },
        )? {
            return Ok(());
        }
        let SessionIo::Managed(managed) = &self.shared.io else {
            finish_mutation(&mut state, idempotency_key, false);
            return Err(ErrorCode::CapabilityNotNegotiated);
        };
        let managed = managed.clone();
        drop(state);
        let result = managed.answer_approval(approval_id, decision, choice);
        let mut state = self.shared.lock();
        finish_mutation(&mut state, idempotency_key, result.is_ok());
        result
    }

    /// **Answer the questions an agent asked, or skip them** (protocol v21,
    /// koru-ai D108): the controller's, once per key, like an approval.
    pub fn answer_questions(
        &self,
        lease: ControllerLeaseToken,
        idempotency_key: IdempotencyKey,
        request_id: koru_session_protocol::QuestionRequestId,
        answers: Option<Vec<koru_session_protocol::QuestionAnswer>>,
    ) -> Result<(), ErrorCode> {
        let mut state = self.shared.lock();
        require_lease(&state, lease)?;
        if state.summary.process != ProcessState::Running {
            return Err(ErrorCode::SessionExited);
        }
        if begin_mutation(
            &mut state,
            idempotency_key,
            SessionMutation::Questions {
                request_id,
                answers: answers.clone(),
            },
        )? {
            return Ok(());
        }
        let SessionIo::Managed(managed) = &self.shared.io else {
            finish_mutation(&mut state, idempotency_key, false);
            return Err(ErrorCode::CapabilityNotNegotiated);
        };
        let managed = managed.clone();
        drop(state);
        let result = managed.answer_questions(request_id, answers);
        let mut state = self.shared.lock();
        finish_mutation(&mut state, idempotency_key, result.is_ok());
        result
    }

    pub fn cancel_turn(
        &self,
        lease: ControllerLeaseToken,
        idempotency_key: IdempotencyKey,
        turn_id: koru_session_protocol::AgentTurnId,
        reason: CancelReason,
    ) -> Result<(), ErrorCode> {
        let mut state = self.shared.lock();
        require_lease(&state, lease)?;
        if state.summary.process != ProcessState::Running {
            return Err(ErrorCode::SessionExited);
        }
        if !state
            .summary
            .capabilities
            .supports(koru_session_protocol::capability::MANAGED_CANCEL)
        {
            return Err(ErrorCode::CapabilityNotNegotiated);
        }
        if state.summary.active_turn_id != Some(turn_id) {
            return Err(ErrorCode::TurnNotFound);
        }
        if begin_mutation(
            &mut state,
            idempotency_key,
            SessionMutation::CancelTurn { turn_id, reason },
        )? {
            return Ok(());
        }
        let SessionIo::Managed(managed) = &self.shared.io else {
            finish_mutation(&mut state, idempotency_key, false);
            return Err(ErrorCode::CapabilityNotNegotiated);
        };
        record_cancel_reason(&mut state, turn_id, reason);
        let managed = managed.clone();
        drop(state);
        let result = managed.cancel(turn_id);
        let mut state = self.shared.lock();
        finish_mutation(&mut state, idempotency_key, result.is_ok());
        result
    }

    /// Clear only managed approval attention that was raised no later than the
    /// normalized event the controller has actually presented.
    pub fn acknowledge_attention(
        &self,
        lease: ControllerLeaseToken,
        through: StreamSequence,
    ) -> Result<(), ErrorCode> {
        let mut state = self.shared.lock();
        require_lease(&state, lease)?;
        if !matches!(state.summary.mode, SessionMode::Managed { .. }) {
            return Err(ErrorCode::CapabilityNotNegotiated);
        }
        let Some(attention) = state.managed_attention else {
            return Ok(());
        };
        if attention.sequence > through {
            return Ok(());
        }
        state.managed_attention = None;
        if state.summary.attention.reason == Some(AttentionReason::ApprovalRequired) {
            state.summary.attention = AttentionState::default();
            publish_event_locked(
                &mut state,
                self.shared.id,
                SessionEvent::AttentionChanged {
                    attention: AttentionState::default(),
                },
                &self.shared.stats,
            );
        }
        Ok(())
    }

    /// Check controller ownership before a host starts expensive ingestion.
    pub fn can_ingest_attachment(&self, lease: ControllerLeaseToken) -> Result<(), ErrorCode> {
        let state = self.shared.lock();
        require_lease(&state, lease)?;
        if state.summary.process != ProcessState::Running {
            return Err(ErrorCode::SessionExited);
        }
        Ok(())
    }

    /// Admit metadata only after validation and atomic storage completed.
    pub fn add_attachment(
        &self,
        lease: ControllerLeaseToken,
        attachment: Attachment,
    ) -> Result<Attachment, ErrorCode> {
        if attachment.session_id != self.shared.id || attachment.status != AttachmentStatus::Pending
        {
            return Err(ErrorCode::InvalidRequest);
        }
        let mut state = self.shared.lock();
        require_lease(&state, lease)?;
        if state.summary.process != ProcessState::Running {
            return Err(ErrorCode::SessionExited);
        }
        if state.attachments.contains_key(&attachment.id) {
            return Err(ErrorCode::InvalidTransition);
        }
        let mut attachment = attachment;
        attachment.delivery_mode = self.shared.attachment_delivery.mode();
        state.attachments.insert(attachment.id, attachment.clone());
        refresh_pending_attachment_count(&mut state);
        Ok(attachment)
    }

    pub fn attachments(&self) -> Vec<Attachment> {
        self.shared.lock().attachments.values().cloned().collect()
    }

    pub fn remove_attachment(
        &self,
        lease: ControllerLeaseToken,
        attachment_id: AttachmentId,
    ) -> Result<Attachment, ErrorCode> {
        let mut state = self.shared.lock();
        require_lease(&state, lease)?;
        let mut attachment = state
            .attachments
            .remove(&attachment_id)
            .ok_or(ErrorCode::AttachmentNotFound)?;
        attachment.status = AttachmentStatus::Removed;
        attachment.preview_available = false;
        refresh_pending_attachment_count(&mut state);
        Ok(attachment)
    }

    pub fn scrollback_page(
        &self,
        lease: ControllerLeaseToken,
        terminal_generation: TerminalGeneration,
        viewport_generation: ViewportGeneration,
        range: RowRange,
    ) -> Result<ScrollbackPage, ErrorCode> {
        {
            let state = self.shared.lock();
            require_lease(&state, lease)?;
        }
        self.scrollback_page_inner(terminal_generation, viewport_generation, range)
    }

    fn scrollback_page_inner(
        &self,
        terminal_generation: TerminalGeneration,
        viewport_generation: ViewportGeneration,
        range: RowRange,
    ) -> Result<ScrollbackPage, ErrorCode> {
        let started = Instant::now();
        let state = self.shared.lock();
        let terminal = state.raw_terminal()?;
        if terminal_generation != terminal.terminal_generation {
            return Err(ErrorCode::StaleGeneration);
        }
        if range.count > MAX_PAGE_ROWS {
            return Err(ErrorCode::InvalidSize);
        }
        let window = terminal
            .emulator
            .scrollback_page(range)
            .map_err(map_emulator)?;
        let has_earlier = window.has_earlier();
        let has_later = window.has_later();
        let rows = window.rows;
        self.shared
            .stats
            .add(Counter::SnapshotPageRows, rows.len() as u64);
        self.shared.stats.record_timing_micros(
            koru_session_observability::Timing::ScrollbackPage,
            micros(started.elapsed()),
        );
        Ok(ScrollbackPage {
            session_id: self.shared.id,
            terminal_generation: terminal.terminal_generation,
            viewport_generation,
            sequence: terminal.render_sequence,
            range,
            rows,
            has_earlier,
            has_later,
        })
    }

    /// Read-only paging for observers. Controller paging retains its lease
    /// gate because it is coupled to that controller's viewport generation;
    /// this path is an independent bounded observation.
    pub fn observe_scrollback_page(
        &self,
        terminal_generation: TerminalGeneration,
        viewport_generation: ViewportGeneration,
        range: RowRange,
    ) -> Result<ScrollbackPage, ErrorCode> {
        self.scrollback_page_inner(terminal_generation, viewport_generation, range)
    }

    pub fn subscribe_events(&self) -> BoundedStream<SessionEventUpdate> {
        self.subscribe_events_from(None)
    }

    /// Install an event observer and compare its recovery cursor while holding
    /// the same state lock. A missed interval becomes an immediate explicit
    /// gap even if the session is quiet afterward.
    pub fn subscribe_events_from(
        &self,
        after: Option<StreamSequence>,
    ) -> BoundedStream<SessionEventUpdate> {
        let (queue, stream) = Queue::new(EVENT_QUEUE_CAPACITY);
        let mut state = self.shared.lock();
        let actual = state.event_sequence;
        let gapped = after.is_some_and(|after| after != actual);
        if let Some(after) = after.filter(|after| *after != actual) {
            let _ = queue.try_push(SessionEventUpdate::Gap {
                expected: after.next().unwrap_or(after),
                actual,
            });
        }
        state
            .event_subscribers
            .push(EventSubscriber { queue, gapped });
        stream
    }

    pub fn subscribe_output(&self) -> BoundedStream<RawOutputUpdate> {
        self.subscribe_output_from(None)
    }

    /// Output equivalent of [`Self::subscribe_events_from`].
    pub fn subscribe_output_from(
        &self,
        after: Option<StreamSequence>,
    ) -> BoundedStream<RawOutputUpdate> {
        let (queue, stream) = Queue::new(OUTPUT_QUEUE_CAPACITY);
        let mut state = self.shared.lock();
        let actual = state.output_sequence;
        let gapped = after.is_some_and(|after| after != actual);
        if let Some(after) = after.filter(|after| *after != actual) {
            let _ = queue.try_push(RawOutputUpdate::Gap {
                expected: after.next().unwrap_or(after),
                actual,
            });
        }
        state
            .output_subscribers
            .push(OutputSubscriber { queue, gapped });
        stream
    }

    pub fn subscribe_agent_events(&self) -> BoundedStream<AgentEventUpdate> {
        self.subscribe_agent_events_from(None)
    }

    /// Replay retained normalized events after `after`, then install the live
    /// subscriber under the same lock. A cursor older than the bounded ring is
    /// an explicit gap; detach never stops provider draining.
    pub fn subscribe_agent_events_from(
        &self,
        after: Option<StreamSequence>,
    ) -> BoundedStream<AgentEventUpdate> {
        let mut state = self.shared.lock();
        state.agent_stream.subscribe(after, &self.shared.stats)
    }

    pub fn observe_agent_events(
        &self,
    ) -> Result<koru_session_protocol::AgentEventObservation, ErrorCode> {
        let state = self.shared.lock();
        if !matches!(state.summary.mode, SessionMode::Managed { .. }) {
            return Err(ErrorCode::CapabilityNotNegotiated);
        }
        let mut observation = state.agent_stream.observe();
        observation.commands = state.agent_commands.clone();
        observation.session_cursor = Some(state.event_sequence);
        Ok(observation)
    }

    pub fn agent_event_sequence(&self) -> StreamSequence {
        self.shared.lock().agent_stream.sequence()
    }

    pub fn terminate(&self, lease: ControllerLeaseToken) -> Result<(), ErrorCode> {
        let state = self.shared.lock();
        require_lease(&state, lease)?;
        drop(state);
        self.shared.terminate_backend();
        Ok(())
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|held| held.into_inner())
    }

    fn parse(&self, bytes: &[u8]) {
        self.stats.add(Counter::OutputBytes, bytes.len() as u64);
        self.stats.add(Counter::ParsedBatches, 1);
        let mut state = self.lock();
        publish_output(&mut state, self.id, bytes, &self.stats);
        let (old_visible, update) = {
            let terminal = state
                .raw_terminal_mut()
                .expect("a PTY reader requires raw terminal state");
            let old_visible = terminal.emulator.visible_range();
            let update = terminal.emulator.parse(bytes);
            (old_visible, update)
        };
        self.stats.add(
            Counter::EmulatorDamagedRows,
            update.damaged_rows.len() as u64,
        );
        let writer = self.raw_writer();
        for event in &update.events {
            if let EmulatorEvent::PtyWrite(bytes) = event {
                let _ = writer.write_protocol(bytes);
            }
        }
        apply_emulator_update(&mut state, self.id, old_visible, update, false, &self.stats);
    }

    fn raw_writer(&self) -> &PtyWriter {
        match &self.io {
            SessionIo::Raw { writer, .. } => writer,
            SessionIo::Managed(_) => panic!("managed session has no PTY writer"),
        }
    }

    fn raw_control(&self) -> &ControlQueue {
        match &self.io {
            SessionIo::Raw { control, .. } => control,
            SessionIo::Managed(_) => panic!("managed session has no PTY control queue"),
        }
    }

    fn terminate_backend(&self) {
        match &self.io {
            SessionIo::Raw { control, .. } => control.terminate(),
            SessionIo::Managed(managed) => managed.terminate(),
        }
    }

    fn apply_agent_event(&self, event: AgentEvent) {
        let mut state = self.lock();
        let event = stamp_cancel_reason(&mut state, event);
        let mut delivery_report = None;
        let approval_required = match &event {
            AgentEvent::ApprovalRequired { approval_id, .. } => Some(*approval_id),
            _ => None,
        };
        let approval_finished = match &event {
            AgentEvent::ApprovalResolved { approval_id, .. }
            | AgentEvent::ApprovalExpired { approval_id, .. } => Some(*approval_id),
            _ => None,
        };
        match &event {
            AgentEvent::CommandsUpdated { commands } => {
                state.agent_commands = Some(commands.clone());
                publish_event_locked(
                    &mut state,
                    self.id,
                    SessionEvent::CommandsUpdated {
                        commands: commands.clone(),
                    },
                    &self.stats,
                );
            }
            AgentEvent::TurnStarted { turn_id } => {
                state.summary.active_turn_id = Some(*turn_id);
                state.summary.managed = Some(koru_session_protocol::ManagedState::Running);
            }
            AgentEvent::WaitingForInput { .. } => {
                state.summary.managed = Some(koru_session_protocol::ManagedState::WaitingForInput);
            }
            AgentEvent::ApprovalRequired { .. } => {
                state.summary.managed =
                    Some(koru_session_protocol::ManagedState::WaitingForApproval);
            }
            // A set the agent waits on is the person's to answer; one it
            // does not wait on leaves the turn running (D108).
            AgentEvent::QuestionsRequired { blocking: true, .. } => {
                state.summary.managed = Some(koru_session_protocol::ManagedState::WaitingForInput);
            }
            AgentEvent::AttachmentAccepted {
                turn_id,
                attachment_id,
            } => {
                if let Some(attachment) = state.attachments.get_mut(attachment_id)
                    && attachment.turn_id == Some(*turn_id)
                {
                    let next =
                        koru_session_protocol::DeliveryState::Delivered { turn_id: *turn_id };
                    if attachment.delivery.can_transition_to(&next) {
                        attachment.delivery = next;
                        delivery_report = Some((
                            *turn_id,
                            *attachment_id,
                            attachment.delivery_mode,
                            ReportedOutcome::Delivered,
                        ));
                        publish_event_locked(
                            &mut state,
                            self.id,
                            SessionEvent::AttachmentDelivered {
                                attachment_id: *attachment_id,
                                turn_id: *turn_id,
                            },
                            &self.stats,
                        );
                    }
                }
                refresh_pending_attachment_count(&mut state);
            }
            AgentEvent::AttachmentRejected {
                turn_id,
                attachment_id,
                code,
            } => {
                if let Some(attachment) = state.attachments.get_mut(attachment_id)
                    && attachment.turn_id == Some(*turn_id)
                {
                    let next = koru_session_protocol::DeliveryState::Failed {
                        turn_id: *turn_id,
                        error: failure(code.clone()),
                    };
                    if attachment.delivery.can_transition_to(&next) {
                        attachment.delivery = next;
                        delivery_report = Some((
                            *turn_id,
                            *attachment_id,
                            attachment.delivery_mode,
                            ReportedOutcome::Failed,
                        ));
                    }
                }
                refresh_pending_attachment_count(&mut state);
            }
            AgentEvent::TurnCompleted { turn_id } => {
                clear_active_turn(&mut state, *turn_id);
                state.cancel_reasons.remove(turn_id);
                state.summary.managed = Some(koru_session_protocol::ManagedState::Completed);
            }
            AgentEvent::TurnCancelled { turn_id, .. } => {
                clear_active_turn(&mut state, *turn_id);
                state.summary.managed = Some(koru_session_protocol::ManagedState::Cancelled);
            }
            AgentEvent::TurnFailed { turn_id, .. } => {
                clear_active_turn(&mut state, *turn_id);
                state.cancel_reasons.remove(turn_id);
                state.summary.managed = Some(koru_session_protocol::ManagedState::Failed);
            }
            AgentEvent::ProviderState { state: next, .. } => {
                state.summary.managed = Some(*next);
            }
            AgentEvent::Usage { usage, .. } => {
                // The last model the provider named is the session's
                // reported model; the requested one moves only with a turn
                // that chose another (protocol v22).
                if let Some(model) = &usage.model {
                    state
                        .summary
                        .model
                        .get_or_insert_with(Default::default)
                        .reported = Some(model.clone());
                }
            }
            AgentEvent::MessageDelta { .. }
            | AgentEvent::ToolStarted { .. }
            | AgentEvent::ToolCompleted { .. }
            | AgentEvent::ApprovalResolved { .. }
            | AgentEvent::ApprovalExpired { .. }
            | AgentEvent::ChildSessionCreated { .. }
            | AgentEvent::ModeChanged { .. }
            | AgentEvent::Notice { .. }
            | AgentEvent::QuestionsRequired { .. }
            | AgentEvent::QuestionsResolved { .. } => {}
        }
        if let Some((turn_id, attachment_id, _, outcome)) = delivery_report
            && !delivery::mark_reported(&mut state, turn_id, attachment_id, outcome)
        {
            delivery_report = None;
        }
        let sequence = state.agent_stream.publish(self.id, event, &self.stats);
        if let Some(approval_id) = approval_required {
            state.managed_attention = Some(ManagedAttention {
                approval_id,
                sequence,
            });
            state.summary.attention = AttentionState {
                unread_count: state.summary.attention.unread_count.saturating_add(1),
                requires_user: true,
                reason: Some(AttentionReason::ApprovalRequired),
            };
            let attention = state.summary.attention.clone();
            publish_event_locked(
                &mut state,
                self.id,
                SessionEvent::AttentionChanged { attention },
                &self.stats,
            );
        } else if let Some(approval_id) = approval_finished
            && state
                .managed_attention
                .is_some_and(|attention| attention.approval_id == approval_id)
        {
            state.managed_attention = None;
            if state.summary.attention.reason == Some(AttentionReason::ApprovalRequired) {
                state.summary.attention = AttentionState::default();
                publish_event_locked(
                    &mut state,
                    self.id,
                    SessionEvent::AttentionChanged {
                        attention: AttentionState::default(),
                    },
                    &self.stats,
                );
            }
        }
        drop(state);
        if let Some((_, attachment_id, mode, outcome)) = delivery_report {
            delivery::publish_delivery_fact(self, attachment_id, mode, outcome);
        }
    }

    fn publish_event(&self, event: SessionEvent) {
        let mut state = self.lock();
        publish_event_locked(&mut state, self.id, event, &self.stats);
    }

    fn finish(&self, process: ProcessState, outcome: TerminalSessionOutcome) {
        if self.ended.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut state = self.lock();
        if !state.summary.process.can_transition_to(&process) {
            return;
        }
        state.summary.process = process.clone();
        state.summary.controller = ControllerState::Detached;
        state.summary.attention = AttentionState {
            unread_count: state.summary.attention.unread_count.saturating_add(1),
            requires_user: true,
            reason: Some(match process {
                ProcessState::Failed { .. } => AttentionReason::Failed,
                _ => AttentionReason::Exited,
            }),
        };
        state.attachments.clear();
        state.summary.pending_attachment_count = 0;
        let event = match process {
            ProcessState::Exited { code } => SessionEvent::Exited { code },
            ProcessState::Failed { error } => SessionEvent::Failed { error },
            _ => return,
        };
        publish_event_locked(&mut state, self.id, event, &self.stats);
        if let Some(controller) = state.controller.take()
            && let Some(render) = controller.render
        {
            render.queue.close();
        }
        for subscriber in &state.event_subscribers {
            subscriber.queue.close();
        }
        for subscriber in &state.output_subscribers {
            subscriber.queue.close();
        }
        state.agent_stream.close();
        drop(state);
        let event = SessionEndedFact {
            session_id: self.id,
            outcome,
            elapsed_ms: millis(self.started.elapsed()),
        }
        .commit(self.trace);
        self.lifecycle.publish(event.into());
        self.resources.session_ended(self.id);
    }
}

fn clear_active_turn(state: &mut State, turn_id: koru_session_protocol::AgentTurnId) {
    if state.summary.active_turn_id == Some(turn_id) {
        state.summary.active_turn_id = None;
    }
}

/// Remember why a turn is being cancelled. A run cancellation outranks a
/// takeover boundary recorded for the same turn, never the reverse.
fn record_cancel_reason(
    state: &mut State,
    turn_id: koru_session_protocol::AgentTurnId,
    reason: CancelReason,
) {
    let recorded = state.cancel_reasons.entry(turn_id).or_insert(reason);
    *recorded = recorded.strongest(reason);
}

/// The adapter reports that a turn was cancelled and knows no reason; the
/// worker does, because it accepted the cancel, and what it recorded
/// replaces whatever default the adapter wrote. A cancellation nobody asked
/// this worker for — the provider's own — keeps the adapter's `Run`.
fn stamp_cancel_reason(state: &mut State, event: AgentEvent) -> AgentEvent {
    match event {
        AgentEvent::TurnCancelled { turn_id, reason } => AgentEvent::TurnCancelled {
            turn_id,
            reason: state.cancel_reasons.remove(&turn_id).unwrap_or(reason),
        },
        other => other,
    }
}

fn drop_controller(state: &mut State) {
    if let Some(mut controller) = state.controller.take()
        && let Some(render) = controller.render.take()
    {
        render.queue.close();
    }
    state.summary.controller = ControllerState::Detached;
}

/// Move the handover and tell every event subscriber. A move the cycle
/// forbids is a bug in this file, not a state a peer can request, so it is
/// asserted rather than reported.
fn move_handover(
    state: &mut State,
    session_id: SessionId,
    next: HandoverState,
    stats: &SessionStats,
) {
    debug_assert!(
        state.summary.handover.can_transition_to(next),
        "handover {:?} -> {next:?}",
        state.summary.handover
    );
    if state.summary.handover == next {
        return;
    }
    state.summary.handover = next;
    publish_event_locked(
        state,
        session_id,
        SessionEvent::HandoverChanged { handover: next },
        stats,
    );
}

/// Returns `true` for an already-completed identical replay.
fn begin_mutation(
    state: &mut State,
    key: IdempotencyKey,
    mutation: SessionMutation,
) -> Result<bool, ErrorCode> {
    if let Some(previous) = state.idempotency.iter().find(|entry| entry.key == key) {
        if previous.mutation != mutation {
            return Err(ErrorCode::IdempotencyConflict);
        }
        return match previous.state {
            MutationState::Succeeded => Ok(true),
            MutationState::InFlight => Err(ErrorCode::Backpressure),
        };
    }
    if state.idempotency.len() >= IDEMPOTENCY_HISTORY {
        let Some(position) = state
            .idempotency
            .iter()
            .position(|entry| entry.state == MutationState::Succeeded)
        else {
            return Err(ErrorCode::Backpressure);
        };
        state.idempotency.remove(position);
    }
    state.idempotency.push_back(MutationRecord {
        key,
        mutation,
        state: MutationState::InFlight,
    });
    Ok(false)
}

fn finish_mutation(state: &mut State, key: IdempotencyKey, succeeded: bool) {
    let Some(position) = state.idempotency.iter().position(|entry| entry.key == key) else {
        return;
    };
    if succeeded {
        state.idempotency[position].state = MutationState::Succeeded;
    } else {
        state.idempotency.remove(position);
    }
}

fn require_lease(state: &State, token: ControllerLeaseToken) -> Result<(), ErrorCode> {
    match &state.controller {
        None => Err(ErrorCode::ControllerRequired),
        Some(controller) if controller.lease.token != token => Err(ErrorCode::StaleLease),
        Some(_) => Ok(()),
    }
}

fn refresh_pending_attachment_count(state: &mut State) {
    state.summary.pending_attachment_count = state
        .attachments
        .values()
        .filter(|attachment| {
            !matches!(
                attachment.delivery,
                koru_session_protocol::DeliveryState::Delivered { .. }
            )
        })
        .count()
        .try_into()
        .unwrap_or(u32::MAX);
}

fn require_lease_mut(
    state: &mut State,
    token: ControllerLeaseToken,
) -> Result<&mut Controller, ErrorCode> {
    require_lease(state, token)?;
    Ok(state.controller.as_mut().expect("lease proved controller"))
}

fn counter<T>(value: u64) -> T
where
    T: PositiveCounter,
{
    T::make(value)
}

trait PositiveCounter {
    fn make(value: u64) -> Self;
}

macro_rules! positive {
    ($($type:ty),+ $(,)?) => {$ (
        impl PositiveCounter for $type {
            fn make(value: u64) -> Self { <$type>::new(value).expect("positive constant") }
        }
    )+ };
}

positive!(TerminalGeneration, StreamSequence);

fn input_bytes(action: &RawCommand) -> usize {
    match action {
        RawCommand::WriteBytes { bytes } => bytes.len(),
        RawCommand::WriteText { text } | RawCommand::PasteText { text } => text.len(),
        RawCommand::SendKey { .. }
        | RawCommand::SendMouse { .. }
        | RawCommand::SetFocus { .. }
        | RawCommand::Interrupt => 1,
        RawCommand::Resize { .. } | RawCommand::Terminate => 0,
    }
}

fn map_transport(error: TransportError) -> ErrorCode {
    match error.stage {
        crate::TransportStage::InvalidRequest => ErrorCode::InvalidRequest,
        crate::TransportStage::Write => ErrorCode::Backpressure,
        _ => ErrorCode::Internal,
    }
}

fn failure(code: ErrorCode) -> SessionFailure {
    SessionFailure {
        message: code.as_str().to_owned(),
        code,
    }
}

fn failed(code: ErrorCode, message: &str) -> ProcessState {
    ProcessState::Failed {
        error: SessionFailure {
            code,
            message: message.to_owned(),
        },
    }
}

fn spawn_failure(_error: TransportError) -> SessionFailure {
    failure(ErrorCode::SpawnFailed)
}

fn log_raw_launch_failure(create: &SessionCreate, error: &TransportError) {
    let profile = match &create.profile {
        SessionProfileKey::BuiltIn(profile) => profile.as_str(),
        SessionProfileKey::Custom(_) => "custom",
    };
    tracing::info!(
        target: "koru_session",
        event = "terminal.session.launch_failed",
        mode = "raw",
        profile,
        profile_kind = metric_profile_kind(create.profile_kind).token(),
        code = ErrorCode::SpawnFailed.as_str(),
        stage = ?error.stage,
        os_code = error.os_code.unwrap_or(0),
        "terminal session launch failed"
    );
}

fn log_provider_launch_failure(
    provider: &ProviderKind,
    failure: &crate::providers::ProviderFailure,
) {
    tracing::info!(
        target: "koru_session",
        event = "terminal.session.launch_failed",
        mode = "managed",
        provider = provider.as_str(),
        code = failure.code.as_str(),
        stage = ?failure.stage,
        diagnostic = ?failure.diagnostic,
        os_code = failure.os_code.unwrap_or(0),
        "managed terminal session launch failed"
    );
}

/// The failure a client is told. Its message is the code's token, except
/// for a refused reasoning level, which says so (`reasoning_rejected`): the
/// code is the refused model's, and the chat says which it was (D19).
fn provider_failure(failure: crate::providers::ProviderFailure) -> SessionFailure {
    let mut told = self::failure(failure.code);
    if failure.diagnostic == koru_session_protocol::ProviderDiagnostic::ReasoningRejected {
        told.message = koru_session_protocol::REASONING_REJECTED.to_owned();
    }
    told
}

fn millis(duration: std::time::Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
