// SPDX-License-Identifier: MPL-2.0

//! Direct, surface-neutral client for `koru-session-host`.
//!
//! Each instance owns one connection, its request table, bounded subscription
//! queues, reconnect state, and at most one controller lease. It contains no
//! HTTP behavior, service launching, or window callbacks.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

use base64::Engine as _;
use koru_session_protocol::{
    AgentEventObservation, AgentTurnId, ApprovalDecision, ApprovalId, Attachment, AttachmentId,
    AttachmentSource, ControllerLease, ControllerLeaseToken, ErrorCode, GridSize, IdempotencyKey,
    ImageDimensions, PendingInput, ProjectId, ProjectSummary, ProtocolOffer, RenderSnapshot,
    RowRange, ScrollbackPage, SendPendingInputResult, SessionCommand, SessionFailure, SessionId,
    SessionMode, SessionObservation, SessionProfileKey, SessionSummary, TerminalGeneration,
    ViewportGeneration, ViewportRequest, WorkspaceScope,
};
use koru_session_wire::{
    AttachmentUpload, ClientHello, CreateProfileSession, CreateProject, CreateSession,
    FRAME_VERSION, FromHost, Greeting, HostHealth, Request, RequestFrame, Response, SessionCatalog,
    SubscriptionFrame, SubscriptionId, SubscriptionUpdate,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const SUBSCRIPTION_CAPACITY: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Disconnected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentPreview {
    pub attachment_id: AttachmentId,
    pub dimensions: ImageDimensions,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub enum ClientError {
    Ipc(String),
    Handshake(String),
    Protocol(koru_session_protocol::ProtocolRefusal),
    Session(SessionFailure),
    Disconnected,
    TimedOut,
    UnexpectedResponse,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ipc(value) | Self::Handshake(value) => f.write_str(value),
            Self::Protocol(value) => write!(f, "{}", value.error.code),
            Self::Session(value) => write!(f, "{}", value.code),
            Self::Disconnected => f.write_str("session host disconnected"),
            Self::TimedOut => f.write_str("session host request timed out"),
            Self::UnexpectedResponse => f.write_str("session host returned an unexpected response"),
        }
    }
}

impl std::error::Error for ClientError {}

struct QueueState {
    updates: VecDeque<SubscriptionUpdate>,
    closed: bool,
}

struct Queue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

impl Queue {
    fn new() -> Self {
        Self {
            state: Mutex::new(QueueState {
                updates: VecDeque::with_capacity(SUBSCRIPTION_CAPACITY),
                closed: false,
            }),
            ready: Condvar::new(),
        }
    }

    fn push(&self, update: SubscriptionUpdate) {
        let mut state = self.state.lock().unwrap_or_else(|held| held.into_inner());
        if state.closed {
            return;
        }
        if state.updates.len() == SUBSCRIPTION_CAPACITY {
            state.updates.clear();
            state.updates.push_back(SubscriptionUpdate::Closed {
                error: Some(failure(ErrorCode::StreamGap)),
            });
            state.closed = true;
        } else {
            let closes = matches!(update, SubscriptionUpdate::Closed { .. });
            state.updates.push_back(update);
            state.closed = closes;
        }
        drop(state);
        self.ready.notify_one();
    }

    fn close(&self) {
        self.state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .closed = true;
        self.ready.notify_all();
    }
}

pub struct Subscription {
    id: SubscriptionId,
    queue: Arc<Queue>,
}

impl Subscription {
    pub fn id(&self) -> SubscriptionId {
        self.id
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<SubscriptionUpdate, ClientError> {
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        if state.updates.is_empty() && !state.closed {
            let (next, waited) = self
                .queue
                .ready
                .wait_timeout_while(state, timeout, |state| {
                    state.updates.is_empty() && !state.closed
                })
                .unwrap_or_else(|held| held.into_inner());
            state = next;
            if waited.timed_out() && state.updates.is_empty() && !state.closed {
                return Err(ClientError::TimedOut);
            }
        }
        state.updates.pop_front().ok_or(ClientError::Disconnected)
    }

    /// Wait for an update or connection close without an idle polling timer.
    pub fn recv(&self) -> Result<SubscriptionUpdate, ClientError> {
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        while state.updates.is_empty() && !state.closed {
            state = self
                .queue
                .ready
                .wait(state)
                .unwrap_or_else(|held| held.into_inner());
        }
        state.updates.pop_front().ok_or(ClientError::Disconnected)
    }
}

struct Inner {
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    pending: Mutex<HashMap<u64, mpsc::SyncSender<Result<Response, SessionFailure>>>>,
    subscriptions: Mutex<BTreeMap<SubscriptionId, Arc<Queue>>>,
    orphaned: Mutex<BTreeMap<SubscriptionId, VecDeque<SubscriptionUpdate>>>,
    next_request: AtomicU64,
    state: Mutex<ConnectionState>,
    controller: Mutex<Option<(SessionId, ControllerLease)>>,
}

pub struct SessionClient {
    inner: Arc<Inner>,
    host: koru_session_wire::HostHello,
}

impl SessionClient {
    pub fn connect(
        endpoint: &koru_ipc::Endpoint,
        product: impl Into<String>,
        offer: ProtocolOffer,
    ) -> Result<Self, ClientError> {
        Self::connect_as(endpoint, product, offer, None)
    }

    /// Connect and say what this connection is. `AiControl` is the AI
    /// worker's one control connection: the host hands it the next creation
    /// generation, which is what admits chat-origin creates (protocol v16).
    /// A host that did not negotiate `session-origin` refuses the role in
    /// its greeting, so a caller never proceeds believing it may create.
    pub fn connect_as(
        endpoint: &koru_ipc::Endpoint,
        product: impl Into<String>,
        offer: ProtocolOffer,
        role: Option<koru_session_protocol::ClientRole>,
    ) -> Result<Self, ClientError> {
        let mut stream = koru_ipc::connect_timed(
            endpoint,
            Duration::from_millis(koru_session_wire::HANDSHAKE_TIMEOUT_MS),
        )
        .map_err(|error| ClientError::Ipc(error.to_string()))?;
        let hello = ClientHello {
            product: product.into(),
            product_version: env!("CARGO_PKG_VERSION").into(),
            offer,
            role,
        };
        koru_ipc::frame::write_frame(&mut stream, FRAME_VERSION, &hello)
            .map_err(|error| ClientError::Handshake(error.to_string()))?;
        let (version, greeting): (u8, Greeting) = koru_ipc::frame::read_frame(&mut stream)
            .map_err(|error| ClientError::Handshake(error.to_string()))?;
        if version != FRAME_VERSION {
            return Err(ClientError::Handshake(
                "unsupported session frame version".into(),
            ));
        }
        let host = match greeting {
            Greeting::Welcome { hello } => hello,
            Greeting::Refused { refusal } => return Err(ClientError::Protocol(refusal)),
        };
        koru_ipc::set_timeouts(&stream, None);
        let (reader, writer) = koru_ipc::split(stream);
        Self::from_io(host, reader, writer)
    }

    /// A negotiated, in-memory peer for dependent-crate regression tests.
    #[cfg(feature = "test-transport")]
    pub fn from_test_transport(
        host: koru_session_wire::HostHello,
        reader: impl std::io::Read + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> Result<Self, ClientError> {
        Self::from_io(host, reader, writer)
    }

    fn from_io(
        host: koru_session_wire::HostHello,
        reader: impl std::io::Read + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> Result<Self, ClientError> {
        let inner = Arc::new(Inner {
            writer: Mutex::new(Some(Box::new(writer))),
            pending: Mutex::new(HashMap::new()),
            subscriptions: Mutex::new(BTreeMap::new()),
            orphaned: Mutex::new(BTreeMap::new()),
            next_request: AtomicU64::new(1),
            state: Mutex::new(ConnectionState::Connected),
            controller: Mutex::new(None),
        });
        let reader_inner = Arc::clone(&inner);
        std::thread::Builder::new()
            .name("koru-session-client reader".into())
            .stack_size(128 * 1024)
            .spawn(move || reader_loop(reader, &reader_inner))
            .map_err(|error| ClientError::Ipc(error.to_string()))?;
        Ok(Self { inner, host })
    }

    pub fn state(&self) -> ConnectionState {
        *self
            .inner
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
    }

    pub fn host(&self) -> &koru_session_wire::HostHello {
        &self.host
    }

    pub fn health(&self) -> Result<HostHealth, ClientError> {
        match self.request(Request::Health)? {
            Response::Health { health } => Ok(health),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn list(&self) -> Result<Vec<SessionSummary>, ClientError> {
        match self.request(Request::ListSessions)? {
            Response::Sessions { sessions } => Ok(sessions),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn catalog(&self) -> Result<SessionCatalog, ClientError> {
        match self.request(Request::Catalog)? {
            Response::Catalog { catalog } => Ok(catalog),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn prepare_shutdown(
        &self,
        authority: String,
        observed_generation: koru_session_protocol::SessionCatalogGeneration,
    ) -> Result<SessionCatalog, ClientError> {
        match self.request(Request::PrepareShutdown {
            authority,
            observed_generation,
        })? {
            Response::Catalog { catalog } => Ok(catalog),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn cancel_shutdown(&self, authority: String) -> Result<(), ClientError> {
        expect_ok(self.request(Request::CancelShutdown { authority })?)
    }

    pub fn create(&self, create: CreateSession) -> Result<SessionSummary, ClientError> {
        match self.request(Request::CreateSession { create })? {
            Response::Created { session } => Ok(session),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn create_profile(
        &self,
        idempotency_key: IdempotencyKey,
        profile: SessionProfileKey,
        parent_session_id: Option<SessionId>,
        working_directory: PathBuf,
        size: GridSize,
    ) -> Result<SessionSummary, ClientError> {
        self.create_profile_mode(
            idempotency_key,
            profile,
            parent_session_id,
            SessionMode::RawPty,
            working_directory,
            size,
        )
    }

    pub fn create_profile_mode(
        &self,
        idempotency_key: IdempotencyKey,
        profile: SessionProfileKey,
        parent_session_id: Option<SessionId>,
        mode: SessionMode,
        working_directory: PathBuf,
        size: GridSize,
    ) -> Result<SessionSummary, ClientError> {
        self.create_profile_in(
            idempotency_key,
            profile,
            parent_session_id,
            mode,
            None,
            working_directory,
            size,
        )
    }

    /// Create a session already placed in a project.
    ///
    /// Placement travels inside creation rather than in a follow-up assign:
    /// create-then-assign would make a transient unplaced session visible to
    /// every other window, and a retried assign would race the create.
    #[allow(clippy::too_many_arguments)]
    pub fn create_profile_in(
        &self,
        idempotency_key: IdempotencyKey,
        profile: SessionProfileKey,
        parent_session_id: Option<SessionId>,
        mode: SessionMode,
        project_id: Option<ProjectId>,
        working_directory: PathBuf,
        size: GridSize,
    ) -> Result<SessionSummary, ClientError> {
        match self.request(Request::CreateProfileSession {
            create: CreateProfileSession {
                idempotency_key,
                profile,
                parent_session_id,
                mode,
                project_id,
                delegation: None,
                origin: Default::default(),
                selection: None,
                resume: false,
                working_directory,
                size,
            },
        })? {
            Response::Created { session } => Ok(session),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Whether this connection negotiated `launch-resume`. A resume is never
    /// sent to a host without it: the host would ignore the flag and start a
    /// new conversation under a request that asked to continue.
    pub fn supports_launch_resume(&self) -> bool {
        self.host
            .agreement
            .capabilities
            .supports(koru_session_protocol::capability::LAUNCH_RESUME)
    }

    /// Create a raw agent session that continues its most recent conversation
    /// in `working_directory` (`launch-resume`). Refused locally, before
    /// anything reaches the host, when the connection did not negotiate it.
    pub fn create_profile_resumed(
        &self,
        idempotency_key: IdempotencyKey,
        profile: SessionProfileKey,
        project_id: Option<ProjectId>,
        working_directory: PathBuf,
        size: GridSize,
    ) -> Result<SessionSummary, ClientError> {
        if !self.supports_launch_resume() {
            return Err(ClientError::Session(failure(
                ErrorCode::CapabilityNotNegotiated,
            )));
        }
        match self.request(Request::CreateProfileSession {
            create: CreateProfileSession {
                idempotency_key,
                profile,
                parent_session_id: None,
                mode: SessionMode::RawPty,
                project_id,
                delegation: None,
                origin: Default::default(),
                selection: None,
                resume: true,
                working_directory,
                size,
            },
        })? {
            Response::Created { session } => Ok(session),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn list_projects(&self) -> Result<Vec<ProjectSummary>, ClientError> {
        match self.request(Request::ListProjects)? {
            Response::Projects { projects } => Ok(projects),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Create a project under a client-minted id. Replaying the same id with
    /// the same content returns the same project rather than a second one.
    pub fn create_project(
        &self,
        project_id: ProjectId,
        name: impl Into<String>,
        root_directory: Option<String>,
        workspace_scope: Option<WorkspaceScope>,
    ) -> Result<ProjectSummary, ClientError> {
        self.one_project(Request::CreateProject {
            create: CreateProject {
                project_id,
                name: name.into(),
                root_directory,
                workspace_scope,
            },
        })
    }

    pub fn rename_project(
        &self,
        project_id: ProjectId,
        name: impl Into<String>,
    ) -> Result<ProjectSummary, ClientError> {
        self.one_project(Request::RenameProject {
            project_id,
            name: name.into(),
        })
    }

    /// Set or clear a project's folder. `None` clears it.
    pub fn set_project_root(
        &self,
        project_id: ProjectId,
        root_directory: Option<String>,
    ) -> Result<ProjectSummary, ClientError> {
        self.one_project(Request::SetProjectRoot {
            project_id,
            root_directory,
        })
    }

    /// Move one project to a workspace, or out of every workspace with `None`.
    ///
    /// The orphan area's second resolution: a project whose workspace is gone
    /// keeps its scope and its identity, and this is what gives it a new one.
    /// **Idempotent because it names the destination** — sending the same pair
    /// twice is the same project in the same place, and there is no "from" for
    /// a replay to disagree with.
    pub fn set_project_scope(
        &self,
        project_id: ProjectId,
        workspace_scope: Option<WorkspaceScope>,
    ) -> Result<ProjectSummary, ClientError> {
        self.one_project(Request::SetProjectScope {
            project_id,
            workspace_scope,
        })
    }

    /// Delete a project. Refused while it still has running sessions.
    pub fn delete_project(&self, project_id: ProjectId) -> Result<(), ClientError> {
        expect_ok(self.request(Request::DeleteProject { project_id })?)
    }

    /// Move a session — and everything launched under it — into a project, or
    /// out of every project with `None`.
    pub fn assign_project(
        &self,
        session_id: SessionId,
        project_id: Option<ProjectId>,
    ) -> Result<(), ClientError> {
        expect_ok(self.request(Request::AssignProject {
            session_id,
            project_id,
        })?)
    }

    /// Hold the named catalog subscription. Each update says only that the
    /// project catalogue changed; the holder re-reads [`Self::list_projects`].
    pub fn subscribe_projects(&self) -> Result<Subscription, ClientError> {
        match self.request(Request::SubscribeProjects)? {
            Response::Subscribed { subscription_id } => {
                Ok(self.register_subscription(subscription_id))
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    fn one_project(&self, request: Request) -> Result<ProjectSummary, ClientError> {
        match self.request(request)? {
            Response::Project { project } => Ok(project),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn ingest_attachment(
        &self,
        session_id: SessionId,
        controller_lease: ControllerLeaseToken,
        attachment_id: AttachmentId,
        source: AttachmentSource,
        bytes: &[u8],
    ) -> Result<Attachment, ClientError> {
        let upload = AttachmentUpload {
            session_id,
            controller_lease,
            attachment_id,
            source,
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        };
        match self.request(Request::IngestAttachment { upload })? {
            Response::AttachmentIngested { attachment } => Ok(attachment),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Forward an already bounded public upload without decoding and encoding
    /// the same bytes again at the Gateway boundary.
    pub fn ingest_attachment_base64(
        &self,
        session_id: SessionId,
        controller_lease: ControllerLeaseToken,
        attachment_id: AttachmentId,
        source: AttachmentSource,
        data_base64: String,
    ) -> Result<Attachment, ClientError> {
        let upload = AttachmentUpload {
            session_id,
            controller_lease,
            attachment_id,
            source,
            data_base64,
        };
        match self.request(Request::IngestAttachment { upload })? {
            Response::AttachmentIngested { attachment } => Ok(attachment),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn attachments(&self, session_id: SessionId) -> Result<Vec<Attachment>, ClientError> {
        match self.request(Request::ListAttachments { session_id })? {
            Response::Attachments { attachments } => Ok(attachments),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn attachment_preview(
        &self,
        session_id: SessionId,
        attachment_id: AttachmentId,
        max_edge: u32,
    ) -> Result<AttachmentPreview, ClientError> {
        match self.request(Request::GetAttachmentPreview {
            session_id,
            attachment_id,
            max_edge,
        })? {
            Response::AttachmentPreview { preview } => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(preview.data_base64)
                    .map_err(|_| ClientError::Ipc("invalid attachment preview encoding".into()))?;
                Ok(AttachmentPreview {
                    attachment_id: preview.attachment_id,
                    dimensions: preview.dimensions,
                    media_type: preview.media_type,
                    bytes,
                })
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn remove_attachment(
        &self,
        session_id: SessionId,
        controller_lease: ControllerLeaseToken,
        attachment_id: AttachmentId,
    ) -> Result<Attachment, ClientError> {
        match self.request(Request::RemoveAttachment {
            session_id,
            controller_lease,
            attachment_id,
        })? {
            Response::AttachmentRemoved { attachment } => Ok(attachment),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn send_pending_input(
        &self,
        session_id: SessionId,
        controller_lease: ControllerLeaseToken,
        input: PendingInput,
    ) -> Result<SendPendingInputResult, ClientError> {
        match self.request(Request::SendPendingInput {
            session_id,
            controller_lease,
            input,
        })? {
            Response::PendingInputSent { result } => Ok(result),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn supports_provider_models(&self) -> bool {
        self.host.agreement.version >= koru_session_protocol::PROVIDER_MODELS_PROTOCOL_VERSION
            && self
                .host
                .agreement
                .capabilities
                .supports(koru_session_protocol::capability::PROVIDER_MODELS)
    }

    pub fn discover_provider_models(
        &self,
        provider: koru_session_protocol::ProviderKind,
        working_directory: PathBuf,
    ) -> Result<koru_session_protocol::ProviderModels, ClientError> {
        if !self.supports_provider_models() {
            return Err(ClientError::Session(failure(
                ErrorCode::CapabilityNotNegotiated,
            )));
        }
        match self.request_with_timeout(
            Request::DiscoverProviderModels {
                provider,
                working_directory,
            },
            Duration::from_secs(30),
        )? {
            Response::ProviderModels { models } => Ok(models),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn supports_provider_readiness(&self) -> bool {
        self.host.agreement.version >= 8
            && self
                .host
                .agreement
                .capabilities
                .supports(koru_session_protocol::capability::PROVIDER_READINESS)
    }

    pub fn subscribe_providers(
        &self,
    ) -> Result<(Vec<koru_session_protocol::ProviderReadiness>, Subscription), ClientError> {
        if !self.supports_provider_readiness() {
            return Err(ClientError::Session(failure(
                ErrorCode::CapabilityNotNegotiated,
            )));
        }
        match self.request(Request::SubscribeProviders)? {
            Response::ProvidersSubscribed {
                subscription_id,
                providers,
            } => Ok((providers, self.register_subscription(subscription_id))),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn refresh_provider(
        &self,
        kind: koru_session_protocol::ProviderKind,
    ) -> Result<(), ClientError> {
        if !self.supports_provider_readiness() {
            return Err(ClientError::Session(failure(
                ErrorCode::CapabilityNotNegotiated,
            )));
        }
        expect_ok(self.request(Request::RefreshProvider { kind })?)
    }

    pub fn answer_approval(
        &self,
        session_id: SessionId,
        controller_lease: ControllerLeaseToken,
        idempotency_key: IdempotencyKey,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        choice: Option<koru_session_protocol::ApprovalChoiceId>,
    ) -> Result<(), ClientError> {
        // A choice cannot be downgraded to its legacy projection: it may be a
        // remembered permission or one of several equally projected options.
        if choice.is_some() && !self.supports_dynamic_approvals() {
            return Err(ClientError::Session(failure(
                ErrorCode::CapabilityNotNegotiated,
            )));
        }
        expect_ok(self.request(Request::AnswerApproval {
            session_id,
            controller_lease,
            idempotency_key,
            approval_id,
            decision,
            choice,
        })?)
    }

    pub fn supports_dynamic_approvals(&self) -> bool {
        self.host.agreement.version >= 8
            && self
                .host
                .agreement
                .capabilities
                .supports(koru_session_protocol::capability::MANAGED_APPROVAL)
            && self
                .host
                .agreement
                .capabilities
                .supports(koru_session_protocol::capability::DYNAMIC_APPROVALS)
    }

    pub fn acknowledge_attention(
        &self,
        session_id: SessionId,
        controller_lease: ControllerLeaseToken,
        through: koru_session_protocol::StreamSequence,
    ) -> Result<(), ClientError> {
        expect_ok(self.request(Request::AcknowledgeAttention {
            session_id,
            controller_lease,
            through,
        })?)
    }

    /// Cancel the run's turn: the meaning every cancellation had before a
    /// takeover could end a step without ending the job.
    pub fn cancel_turn(
        &self,
        session_id: SessionId,
        controller_lease: ControllerLeaseToken,
        idempotency_key: IdempotencyKey,
        turn_id: AgentTurnId,
    ) -> Result<(), ClientError> {
        self.cancel_turn_for(
            session_id,
            controller_lease,
            idempotency_key,
            turn_id,
            koru_session_protocol::CancelReason::Run,
        )
    }

    /// Protocol v14. Cancel a turn with a stated reason; a takeover boundary
    /// ends the turn for a handover and never the run (D36).
    pub fn cancel_turn_for(
        &self,
        session_id: SessionId,
        controller_lease: ControllerLeaseToken,
        idempotency_key: IdempotencyKey,
        turn_id: AgentTurnId,
        reason: koru_session_protocol::CancelReason,
    ) -> Result<(), ClientError> {
        expect_ok(self.request(Request::CancelTurn {
            session_id,
            controller_lease,
            idempotency_key,
            turn_id,
            reason,
        })?)
    }

    /// Whether this connection negotiated session origin ownership
    /// (protocol v16). The AI worker checks this before starting any agent
    /// chat: a host that cannot own a chat's sessions cannot be asked to
    /// end them, so nothing is created against it.
    pub fn supports_session_origin(&self) -> bool {
        self.host.agreement.version >= koru_session_protocol::SESSION_ORIGIN_PROTOCOL_VERSION
            && self
                .host
                .agreement
                .capabilities
                .supports(koru_session_protocol::capability::SESSION_ORIGIN)
    }

    /// Whether this connection negotiated explicit launch selection
    /// (protocol v16). An explicit model or effort is never sent to a host
    /// without it: the host would ignore the field and run the default.
    pub fn supports_launch_selection(&self) -> bool {
        self.host.agreement.version >= koru_session_protocol::SESSION_ORIGIN_PROTOCOL_VERSION
            && self
                .host
                .agreement
                .capabilities
                .supports(koru_session_protocol::capability::LAUNCH_SELECTION)
    }

    /// Create a managed session under an origin, with an optional explicit
    /// selection (protocol v16). Refused locally, before anything reaches
    /// the host, when the connection did not negotiate what the request
    /// needs — the same rule `create_delegated` keeps for delegation.
    #[allow(clippy::too_many_arguments)]
    pub fn create_profile_origin(
        &self,
        idempotency_key: IdempotencyKey,
        profile: SessionProfileKey,
        mode: SessionMode,
        origin: koru_session_protocol::SessionOrigin,
        selection: Option<koru_session_protocol::LaunchSelection>,
        working_directory: PathBuf,
        size: GridSize,
    ) -> Result<SessionSummary, ClientError> {
        if origin != koru_session_protocol::SessionOrigin::Manual && !self.supports_session_origin()
        {
            return Err(ClientError::Session(failure(
                ErrorCode::CapabilityNotNegotiated,
            )));
        }
        if selection
            .as_ref()
            .is_some_and(koru_session_protocol::LaunchSelection::is_explicit)
            && !self.supports_launch_selection()
        {
            return Err(ClientError::Session(failure(
                ErrorCode::CapabilityNotNegotiated,
            )));
        }
        match self.request(Request::CreateProfileSession {
            create: CreateProfileSession {
                idempotency_key,
                profile,
                parent_session_id: None,
                mode,
                project_id: None,
                delegation: None,
                origin,
                selection,
                resume: false,
                working_directory,
                size,
            },
        })? {
            Response::Created { session } => Ok(session),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// What the host holds under an origin right now (protocol v16): live
    /// sessions and in-flight reservations, from the live registry only.
    pub fn lookup_origin(
        &self,
        origin: koru_session_protocol::SessionOrigin,
    ) -> Result<(String, koru_session_protocol::OriginLookup), ClientError> {
        if !self.supports_session_origin() {
            return Err(ClientError::Session(failure(
                ErrorCode::CapabilityNotNegotiated,
            )));
        }
        match self.request(Request::LookupOrigin { origin })? {
            Response::OriginLookup {
                instance_id,
                lookup,
            } => Ok((instance_id, lookup)),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Whether this connection negotiated the delegation family. The
    /// orchestrator checks this before creating a delegated session: a host
    /// that cannot be asked "did it start?" cannot be delegated to.
    pub fn supports_delegation(&self) -> bool {
        self.host.agreement.version >= koru_session_protocol::DELEGATION_PROTOCOL_VERSION
            && self
                .host
                .agreement
                .capabilities
                .supports(koru_session_protocol::capability::DELEGATION)
    }

    /// Protocol v14. Create a delegated managed session: a managed profile
    /// launched with Koru's MCP endpoint and tool allowlist, and optionally
    /// resuming a provider session.
    ///
    /// **Refused locally on a connection without the capability**, before
    /// any frame is written. A v13 host would read the create, ignore the
    /// field it does not know, and spawn a provider that never reaches its
    /// tools — which is the one outcome worse than not spawning.
    #[allow(clippy::too_many_arguments)]
    pub fn create_delegated(
        &self,
        idempotency_key: IdempotencyKey,
        profile: SessionProfileKey,
        provider: koru_session_protocol::ProviderKind,
        project_id: Option<ProjectId>,
        delegation: koru_session_protocol::DelegationLaunch,
        working_directory: PathBuf,
        size: GridSize,
    ) -> Result<SessionSummary, ClientError> {
        if !self.supports_delegation()
            || (delegation.model.is_some() && !self.supports_provider_models())
        {
            return Err(ClientError::Session(failure(
                ErrorCode::CapabilityNotNegotiated,
            )));
        }
        match self.request(Request::CreateProfileSession {
            create: CreateProfileSession {
                idempotency_key,
                profile,
                parent_session_id: None,
                mode: SessionMode::Managed { provider },
                project_id,
                delegation: Some(delegation),
                origin: Default::default(),
                selection: None,
                resume: false,
                working_directory,
                size,
            },
        })? {
            Response::Created { session } => Ok(session),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// The delegated create **without** the local capability check, so a
    /// test can prove the host's own gate refuses the frame. A real caller
    /// uses [`Self::create_delegated`], whose refusal happens before any
    /// frame is written.
    pub fn create_profile_delegated_unchecked(
        &self,
        idempotency_key: IdempotencyKey,
        profile: SessionProfileKey,
        provider: koru_session_protocol::ProviderKind,
        delegation: koru_session_protocol::DelegationLaunch,
        working_directory: PathBuf,
        size: GridSize,
    ) -> Result<SessionSummary, ClientError> {
        let mode = if matches!(profile, SessionProfileKey::BuiltIn(ref key) if key.as_str() == "platform-shell")
        {
            SessionMode::RawPty
        } else {
            SessionMode::Managed { provider }
        };
        match self.request(Request::CreateProfileSession {
            create: CreateProfileSession {
                idempotency_key,
                profile,
                parent_session_id: None,
                mode,
                project_id: None,
                delegation: Some(delegation),
                origin: Default::default(),
                selection: None,
                resume: false,
                working_directory,
                size,
            },
        })? {
            Response::Created { session } => Ok(session),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Protocol v14. Ask whether a creation happened without causing one.
    /// Answers the lookup and the host incarnation that gave it.
    pub fn lookup_creation(
        &self,
        idempotency_key: IdempotencyKey,
    ) -> Result<(String, koru_session_protocol::CreationLookup), ClientError> {
        match self.request(Request::LookupCreation { idempotency_key })? {
            Response::CreationLookup {
                instance_id,
                lookup,
            } => Ok((instance_id, lookup)),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Protocol v14. Fence a key before starting a fresh attempt; answers
    /// what the key held at the moment of retirement.
    pub fn retire_creation(
        &self,
        idempotency_key: IdempotencyKey,
    ) -> Result<(String, koru_session_protocol::CreationLookup), ClientError> {
        match self.request(Request::RetireCreation { idempotency_key })? {
            Response::CreationRetired {
                instance_id,
                lookup,
            } => Ok((instance_id, lookup)),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Protocol v14. End a delegated session because its run was cancelled,
    /// without taking the controller lease — the run's own end reaches the
    /// process tree even while a person holds control (§7.6). Refused with
    /// `forbidden` for a session no delegated launch created.
    pub fn terminate_delegated(&self, session_id: SessionId) -> Result<(), ClientError> {
        expect_ok(self.request(Request::TerminateDelegated { session_id })?)
    }

    /// Protocol v14. Ask the controller's holder for the lease.
    pub fn request_control(
        &self,
        session_id: SessionId,
        urgency: koru_session_protocol::TakeoverUrgency,
    ) -> Result<koru_session_protocol::ControlRequestOutcome, ClientError> {
        match self.request(Request::RequestControl {
            session_id,
            urgency,
        })? {
            Response::ControlRequested { outcome } => Ok(outcome),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Protocol v14. Give the lease up for a handover. Like `detach`, this
    /// forgets the connection's lease locally as well.
    pub fn release_control(
        &self,
        session_id: SessionId,
        lease: ControllerLeaseToken,
    ) -> Result<(), ClientError> {
        let result = expect_ok(self.request(Request::ReleaseControl { session_id, lease })?);
        self.forget_lease(session_id, lease);
        result
    }

    /// Protocol v14. Give the lease back after a handover.
    pub fn return_control(
        &self,
        session_id: SessionId,
        lease: ControllerLeaseToken,
    ) -> Result<(), ClientError> {
        let result = expect_ok(self.request(Request::ReturnControl { session_id, lease })?);
        self.forget_lease(session_id, lease);
        result
    }

    pub fn observe_agent_events(
        &self,
        session_id: SessionId,
    ) -> Result<AgentEventObservation, ClientError> {
        match self.request(Request::ObserveAgentEvents { session_id })? {
            Response::AgentEventsObserved { observation } => Ok(observation),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn attach_live(
        &self,
        session_id: SessionId,
        viewport_generation: ViewportGeneration,
        columns: u16,
        overscan_rows: u16,
    ) -> Result<(ControllerLease, Subscription), ClientError> {
        if self
            .inner
            .controller
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .is_some()
        {
            return Err(ClientError::Session(failure(ErrorCode::ControllerConflict)));
        }
        match self.request(Request::Attach {
            session_id,
            viewport_generation,
            columns,
            overscan_rows,
        })? {
            Response::Attached {
                lease,
                subscription_id,
            } => {
                let subscription = self.register_subscription(subscription_id);
                *self
                    .inner
                    .controller
                    .lock()
                    .unwrap_or_else(|held| held.into_inner()) = Some((session_id, lease.clone()));
                Ok((lease, subscription))
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn detach(
        &self,
        session_id: SessionId,
        lease: ControllerLeaseToken,
    ) -> Result<(), ClientError> {
        let result = self
            .request(Request::Detach { session_id, lease })
            .and_then(expect_ok);
        self.forget_lease(session_id, lease);
        result
    }

    /// Local ownership ends when this client asks to release the exact
    /// lease it holds, even if the process won the race and the Host now
    /// answers SessionNotFound/SessionExited. Retaining that dead lease
    /// poisons every later attach on this otherwise healthy connection as
    /// a local ControllerConflict.
    fn forget_lease(&self, session_id: SessionId, lease: ControllerLeaseToken) {
        let mut controller = self
            .inner
            .controller
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        if controller
            .as_ref()
            .is_some_and(|(held_session, held)| *held_session == session_id && held.token == lease)
        {
            *controller = None;
        }
    }

    pub fn command(&self, command: SessionCommand) -> Result<(), ClientError> {
        expect_ok(self.request(Request::Command { command })?)
    }

    pub fn suspend_render(
        &self,
        session_id: SessionId,
        lease: ControllerLeaseToken,
    ) -> Result<(), ClientError> {
        expect_ok(self.request(Request::SuspendRender { session_id, lease })?)
    }

    pub fn set_viewport(
        &self,
        session_id: SessionId,
        lease: ControllerLeaseToken,
        viewport: ViewportRequest,
    ) -> Result<Subscription, ClientError> {
        match self.request(Request::SetViewport {
            session_id,
            lease,
            viewport,
        })? {
            Response::Subscribed { subscription_id } => {
                Ok(self.register_subscription(subscription_id))
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn recover_snapshot(
        &self,
        session_id: SessionId,
        lease: ControllerLeaseToken,
    ) -> Result<(), ClientError> {
        expect_ok(self.request(Request::RecoverSnapshot { session_id, lease })?)
    }

    pub fn snapshot(
        &self,
        session_id: SessionId,
        viewport_generation: ViewportGeneration,
        range: RowRange,
        columns: u16,
    ) -> Result<RenderSnapshot, ClientError> {
        match self.request(Request::Snapshot {
            session_id,
            viewport_generation,
            range,
            columns,
        })? {
            Response::Snapshot { snapshot } => Ok(snapshot),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn observe_snapshot(
        &self,
        session_id: SessionId,
        viewport_generation: ViewportGeneration,
        range: RowRange,
        columns: u16,
    ) -> Result<SessionObservation, ClientError> {
        match self.request(Request::ObserveSnapshot {
            session_id,
            viewport_generation,
            range,
            columns,
        })? {
            Response::ObservedSnapshot { observation } => Ok(observation),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn scrollback_page(
        &self,
        session_id: SessionId,
        lease: ControllerLeaseToken,
        terminal_generation: TerminalGeneration,
        viewport_generation: ViewportGeneration,
        range: RowRange,
    ) -> Result<ScrollbackPage, ClientError> {
        match self.request(Request::ScrollbackPage {
            session_id,
            lease,
            terminal_generation,
            viewport_generation,
            range,
        })? {
            Response::ScrollbackPage { page } => Ok(page),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn observe_scrollback_page(
        &self,
        session_id: SessionId,
        terminal_generation: TerminalGeneration,
        viewport_generation: ViewportGeneration,
        range: RowRange,
    ) -> Result<ScrollbackPage, ClientError> {
        match self.request(Request::ObserveScrollbackPage {
            session_id,
            terminal_generation,
            viewport_generation,
            range,
        })? {
            Response::ScrollbackPage { page } => Ok(page),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn subscribe_events(&self, session_id: SessionId) -> Result<Subscription, ClientError> {
        match self.request(Request::SubscribeEvents { session_id })? {
            Response::Subscribed { subscription_id } => {
                Ok(self.register_subscription(subscription_id))
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn subscribe_events_from(
        &self,
        session_id: SessionId,
        after: Option<koru_session_protocol::StreamSequence>,
    ) -> Result<Subscription, ClientError> {
        match self.request(Request::SubscribeEventsFrom { session_id, after })? {
            Response::Subscribed { subscription_id } => {
                Ok(self.register_subscription(subscription_id))
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn subscribe_output(&self, session_id: SessionId) -> Result<Subscription, ClientError> {
        match self.request(Request::SubscribeOutput { session_id })? {
            Response::Subscribed { subscription_id } => {
                Ok(self.register_subscription(subscription_id))
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn subscribe_output_from(
        &self,
        session_id: SessionId,
        after: Option<koru_session_protocol::StreamSequence>,
    ) -> Result<Subscription, ClientError> {
        match self.request(Request::SubscribeOutputFrom { session_id, after })? {
            Response::Subscribed { subscription_id } => {
                Ok(self.register_subscription(subscription_id))
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn subscribe_agent_events_from(
        &self,
        session_id: SessionId,
        after: Option<koru_session_protocol::StreamSequence>,
    ) -> Result<Subscription, ClientError> {
        match self.request(Request::SubscribeAgentEventsFrom { session_id, after })? {
            Response::Subscribed { subscription_id } => {
                Ok(self.register_subscription(subscription_id))
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Close every live session one Koru workspace owns.
    ///
    /// The supervised lifecycle verb, not a terminal command: it ends sessions
    /// whose controller is somebody else, because the workspace they belong to
    /// is being archived or deleted. It takes the launch-scoped lifecycle
    /// authority the Agent hands its supervised children, and answers with the
    /// sessions it actually closed — an empty list on a retry.
    pub fn terminate_workspace_sessions(
        &self,
        authority: String,
        scope: koru_session_protocol::WorkspaceScope,
    ) -> Result<Vec<SessionId>, ClientError> {
        match self.request(Request::TerminateWorkspaceSessions { authority, scope })? {
            Response::WorkspaceSessionsTerminated { terminated } => Ok(terminated),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Make every project that names a workspace name the profile that now
    /// owns it.
    ///
    /// The repair half of a workspace moving between profiles. It names the
    /// destination rather than the move, so calling it twice is calling it
    /// once, and calling it late repairs whatever is left. Answers with the
    /// projects it changed — an empty list when everything was already right.
    pub fn rescope_workspace_projects(
        &self,
        authority: String,
        workspace: koru_session_protocol::CatalogWorkspaceId,
        profile: koru_session_protocol::CatalogProfileId,
    ) -> Result<Vec<koru_session_protocol::ProjectId>, ClientError> {
        match self.request(Request::RescopeWorkspaceProjects {
            authority,
            workspace,
            profile,
        })? {
            Response::WorkspaceProjectsRescoped { rescoped } => Ok(rescoped),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn stop_host(&self, authority: String) -> Result<(), ClientError> {
        expect_ok(self.request(Request::StopHost { authority })?)
    }

    pub fn controller(&self) -> Option<(SessionId, ControllerLease)> {
        self.inner
            .controller
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }

    /// End this private connection now. Long-lived public stream adapters use
    /// it to release idle subscription readers when the HTTP peer disappears.
    pub fn disconnect(&self) {
        disconnect(&self.inner, true);
    }

    fn request(&self, request: Request) -> Result<Response, ClientError> {
        self.request_with_timeout(request, REQUEST_TIMEOUT)
    }

    fn request_with_timeout(
        &self,
        request: Request,
        timeout: Duration,
    ) -> Result<Response, ClientError> {
        if self.state() != ConnectionState::Connected {
            return Err(ClientError::Disconnected);
        }
        let id = self.inner.next_request.fetch_add(1, Ordering::Relaxed);
        if id == 0 {
            return Err(ClientError::Disconnected);
        }
        let (send, recv) = mpsc::sync_channel(1);
        self.inner
            .pending
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .insert(id, send);
        let write = {
            let mut writer = self
                .inner
                .writer
                .lock()
                .unwrap_or_else(|held| held.into_inner());
            match writer.as_mut() {
                Some(writer) => koru_ipc::frame::write_frame(
                    writer,
                    FRAME_VERSION,
                    &RequestFrame { id, request },
                ),
                None => return Err(ClientError::Disconnected),
            }
        };
        if let Err(error) = write {
            self.inner
                .pending
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .remove(&id);
            disconnect(&self.inner, false);
            return Err(ClientError::Ipc(error.to_string()));
        }
        match recv.recv_timeout(timeout) {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(error)) => Err(ClientError::Session(error)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.inner
                    .pending
                    .lock()
                    .unwrap_or_else(|held| held.into_inner())
                    .remove(&id);
                Err(ClientError::TimedOut)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ClientError::Disconnected),
        }
    }

    fn register_subscription(&self, id: SubscriptionId) -> Subscription {
        let queue = Arc::new(Queue::new());
        self.inner
            .subscriptions
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .insert(id, Arc::clone(&queue));
        if let Some(mut updates) = self
            .inner
            .orphaned
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .remove(&id)
        {
            while let Some(update) = updates.pop_front() {
                queue.push(update);
            }
        }
        Subscription { id, queue }
    }
}

impl Drop for SessionClient {
    fn drop(&mut self) {
        disconnect(&self.inner, true);
    }
}

fn reader_loop(mut reader: impl std::io::Read, inner: &Arc<Inner>) {
    loop {
        let Ok((version, frame)) = koru_ipc::frame::read_frame::<_, FromHost>(&mut reader) else {
            break;
        };
        if version != FRAME_VERSION {
            break;
        }
        match frame {
            FromHost::Response { frame } => {
                if let Some(sender) = inner
                    .pending
                    .lock()
                    .unwrap_or_else(|held| held.into_inner())
                    .remove(&frame.id)
                {
                    let _ = sender.send(frame.result);
                }
            }
            FromHost::Subscription {
                frame:
                    SubscriptionFrame {
                        subscription_id,
                        update,
                    },
            } => {
                let queue = inner
                    .subscriptions
                    .lock()
                    .unwrap_or_else(|held| held.into_inner())
                    .get(&subscription_id)
                    .cloned();
                if let Some(queue) = queue {
                    queue.push(update);
                } else {
                    let mut orphaned = inner
                        .orphaned
                        .lock()
                        .unwrap_or_else(|held| held.into_inner());
                    let updates = orphaned.entry(subscription_id).or_default();
                    if updates.len() < SUBSCRIPTION_CAPACITY {
                        updates.push_back(update);
                    }
                }
            }
        }
    }
    disconnect(inner, false);
}

fn disconnect(inner: &Inner, polite: bool) {
    if polite
        && *inner.state.lock().unwrap_or_else(|held| held.into_inner())
            == ConnectionState::Connected
        && let Some(writer) = inner
            .writer
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .as_mut()
    {
        let _ = koru_ipc::frame::write_frame(
            writer,
            FRAME_VERSION,
            &RequestFrame {
                id: u64::MAX,
                request: Request::Disconnect,
            },
        );
    }
    *inner.state.lock().unwrap_or_else(|held| held.into_inner()) = ConnectionState::Disconnected;
    inner
        .writer
        .lock()
        .unwrap_or_else(|held| held.into_inner())
        .take();
    inner
        .pending
        .lock()
        .unwrap_or_else(|held| held.into_inner())
        .clear();
    for queue in inner
        .subscriptions
        .lock()
        .unwrap_or_else(|held| held.into_inner())
        .values()
    {
        queue.close();
    }
}

fn expect_ok(response: Response) -> Result<(), ClientError> {
    matches!(response, Response::Ok)
        .then_some(())
        .ok_or(ClientError::UnexpectedResponse)
}

fn failure(code: ErrorCode) -> SessionFailure {
    SessionFailure {
        message: code.as_str().to_owned(),
        code,
    }
}

#[cfg(test)]
mod tests;
