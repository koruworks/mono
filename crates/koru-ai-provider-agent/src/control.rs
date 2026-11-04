// SPDX-License-Identifier: MPL-2.0

//! The session-control thread: the AI worker's one `ai_control` connection.
//!
//! Every chat-session create, lookup and terminate travels on this one
//! connection, and the host processes a connection's requests strictly in
//! order — which is the barrier that makes a drain's lookup see every
//! earlier create, even one its sender gave up on. The generation the host
//! handed this connection in its hello is what admits chat creates at all;
//! reconnecting takes a new one and makes everything the old connection
//! sent stale (protocol v16).
//!
//! The thread answers three questions: *find or start this chat's session*,
//! *end everything this chat has*, and *how ready is this agent*. Nothing on
//! it draws, and the dispatcher that asks never waits on the wire — it
//! hands a reply channel over and carries on.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::{Duration, Instant};

use koru_ai_core::{AiError, AiErrorClass, AiProvider, AiProviderId, ChatId};
use koru_session_client::{ClientError, SessionClient, Subscription};
use koru_session_protocol::{
    CapabilityName, CapabilitySet, ClientRole, ErrorCode, ExecutableAvailability, GridSize,
    IdempotencyKey, LaunchSelection, MAX_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION, ModelIdentity,
    ProtocolOffer, ProtocolRange, ProviderKind, ProviderModels, ProviderReadiness, RawCommand,
    SessionCommand, SessionId, SessionMode, SessionOrigin, ViewportGeneration, capability,
};
use koru_session_wire::SubscriptionUpdate;

use crate::{
    AgentProvider, AgentProviderKind, AgentReadiness, COLUMNS, ENSURE_DEADLINE, LiveTurns, PRODUCT,
    ROWS, agent_provider_kinds, chat_origin,
};

/// How long a delete waits for a chat's sessions to end before it gives the
/// row back: the host's own cancel deadline plus a terminate.
const TEARDOWN_DEADLINE: Duration = Duration::from_secs(20);
/// How long a readiness question may take: the answer is a cached snapshot.
const READINESS_DEADLINE: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(250);
const COMMAND_CAPACITY: usize = 64;

/// The chat's session, found or started.
#[derive(Clone, Debug)]
pub struct EnsuredSession {
    pub session: SessionId,
    /// Whether this call started it. A fresh session has none of the
    /// conversation, so the turn hands it the prefix and says so.
    pub fresh: bool,
    pub model: Option<ModelIdentity>,
    /// A turn the host reports as still running from before: the turn
    /// cancels it before submitting, never runs two on one session.
    pub active_turn: Option<koru_session_protocol::AgentTurnId>,
}

/// What ending a chat's sessions came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndOutcome {
    /// Nothing of the chat is live or in flight on the host.
    Ended,
    /// Something could not be ended; the chat keeps its row and the person
    /// tries again. The code is closed and content-free.
    Failed { code: &'static str },
}

enum Command {
    Ensure {
        origin: SessionOrigin,
        kind: AgentProviderKind,
        selection: Option<LaunchSelection>,
        /// The chat's chosen folder; `None` is the home folder.
        cwd: Option<std::path::PathBuf>,
        reply: SyncSender<Result<EnsuredSession, AiError>>,
    },
    End {
        origin: SessionOrigin,
        on_done: Box<dyn FnOnce(EndOutcome) + Send>,
    },
    Readiness {
        kind: ProviderKind,
        reply: SyncSender<Result<AgentReadiness, AiError>>,
    },
}

/// The door to the control thread. Cheap to clone; every clone shares the
/// thread, the connection and the live turn table.
#[derive(Clone)]
pub struct AgentSessions {
    commands: SyncSender<Command>,
    endpoint: koru_ipc::Endpoint,
    live: Arc<LiveTurns>,
    /// The manual refreshes under way, one per agent: a second joins the
    /// first rather than asking the host to re-probe again.
    refreshes: Arc<crate::joined::Joined<ProviderKind, Result<AgentCatalog, ErrorCode>>>,
}

/// What an agent offers a picker: its models and, beside them, its approval
/// modes (koru-ai D7), `None` when it has none.
#[derive(Clone, Debug)]
pub struct AgentCatalog {
    pub models: ProviderModels,
    pub modes: Option<koru_session_protocol::AgentModes>,
    /// The session host takes a level with each message (protocol v20), so
    /// a begun chat's level can change from its next message (koru-ai
    /// D70). `false` against an older host.
    pub levels_per_turn: bool,
}

/// Where a chat's agent works until a folder is chosen (I8): the person's
/// home folder, which every agent accepts.
pub(crate) fn default_working_directory() -> std::path::PathBuf {
    std::env::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."))
}

impl AgentSessions {
    /// The models an agent lists, on a connection of its own so a slow
    /// catalog never holds the control connection. An agent whose route
    /// lists nothing answers with an empty catalog. Nothing is kept here:
    /// the session host's catalog is the one cache, and a copy here would
    /// hide every refresh it makes (agent-models D2).
    ///
    /// With `refresh` — the manual refresh (D13) — the host is first asked
    /// to re-probe the agent, which drops its list, and the answer is the
    /// list the re-probe rebuilt; the host's catalog waits for it within
    /// the adapter's budget. A refresh already under way for the agent is
    /// joined.
    pub fn discover_models(
        &self,
        kind: &AgentProviderKind,
        refresh: bool,
    ) -> Result<AgentCatalog, ErrorCode> {
        if refresh {
            return self
                .refreshes
                .run(kind.kind.clone(), || self.list_models(kind, true));
        }
        self.list_models(kind, false)
    }

    fn list_models(
        &self,
        kind: &AgentProviderKind,
        refresh: bool,
    ) -> Result<AgentCatalog, ErrorCode> {
        let started = Instant::now();
        let mut levels_per_turn = false;
        let listed = SessionClient::connect(&self.endpoint, "koru-ai-agent-models", offer())
            .and_then(|client| {
                levels_per_turn = client.supports_turn_reasoning();
                if refresh {
                    client.refresh_provider(kind.kind.clone())?;
                    tracing::info!(
                        target: "koru_ai::agent",
                        provider = %kind.kind,
                        "agent models refresh requested"
                    );
                }
                client.discover_provider_catalog(kind.kind.clone(), default_working_directory())
            });
        let (models, modes) = match listed {
            Ok(listed) => listed,
            Err(ClientError::Session(failure))
                if failure.code == ErrorCode::CapabilityNotNegotiated =>
            {
                (
                    ProviderModels::try_from(Vec::new()).expect("an empty catalog is valid"),
                    None,
                )
            }
            Err(error) => {
                tracing::warn!(
                    target: "koru_ai::agent",
                    provider = %kind.kind,
                    error = %error,
                    "listing an agent's models failed"
                );
                return Err(client_code(&error));
            }
        };
        tracing::info!(
            target: "koru_ai::agent",
            provider = %kind.kind,
            count = models.models().len(),
            modes = modes.as_ref().map_or(0, |modes| modes.modes().len()),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "agent models listed"
        );
        Ok(AgentCatalog {
            models,
            modes,
            levels_per_turn,
        })
    }

    /// Start the control thread for the session host at `endpoint`. The
    /// connection is made on first use, so a host that is not up yet costs
    /// nothing until a person picks an agent.
    pub fn spawn(endpoint: koru_ipc::Endpoint) -> Result<Self, String> {
        let (commands, receiver) = sync_channel(COMMAND_CAPACITY);
        let thread_endpoint = endpoint.clone();
        std::thread::Builder::new()
            .name("koru-ai-agent-control".into())
            .spawn(move || Control::new(thread_endpoint).run(receiver))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            commands,
            endpoint,
            live: Arc::new(LiveTurns::default()),
            refreshes: Arc::new(crate::joined::Joined::new()),
        })
    }

    /// One chat provider per managed agent kind the host registers.
    pub fn providers(&self) -> Vec<(AiProviderId, Arc<dyn AiProvider>)> {
        agent_provider_kinds()
            .into_iter()
            .map(|kind| {
                let id = kind.id.clone();
                let provider: Arc<dyn AiProvider> = Arc::new(AgentProvider {
                    kind,
                    sessions: self.clone(),
                    endpoint: self.endpoint.clone(),
                    live: Arc::clone(&self.live),
                });
                (id, provider)
            })
            .collect()
    }

    pub fn live(&self) -> &Arc<LiveTurns> {
        &self.live
    }

    /// How ready one agent is, from the host's cached snapshot.
    pub fn readiness(&self, kind: &ProviderKind) -> Result<AgentReadiness, AiError> {
        let (reply, answer) = sync_channel(1);
        self.commands
            .try_send(Command::Readiness {
                kind: kind.clone(),
                reply,
            })
            .map_err(|_| control_busy())?;
        answer
            .recv_timeout(READINESS_DEADLINE)
            .map_err(|_| control_busy())?
    }

    /// Find or start the chat's session. Blocks the caller — a provider job
    /// thread, never the dispatcher — for at most [`ENSURE_DEADLINE`].
    pub(crate) fn ensure(
        &self,
        origin: SessionOrigin,
        kind: &AgentProviderKind,
        selection: Option<LaunchSelection>,
        cwd: Option<std::path::PathBuf>,
    ) -> Result<EnsuredSession, AiError> {
        let (reply, answer) = sync_channel(1);
        self.commands
            .try_send(Command::Ensure {
                origin,
                kind: kind.clone(),
                selection,
                cwd,
                reply,
            })
            .map_err(|_| control_busy())?;
        answer.recv_timeout(ENSURE_DEADLINE).map_err(|_| {
            AiError::new(
                AiErrorClass::Provider,
                "The agent did not start in time. Try again.",
                true,
            )
        })?
    }

    /// End everything the chat has on the host. `on_done` runs on the control
    /// thread when the host has confirmed nothing of the chat runs, or when
    /// something could not be ended; the caller keeps the chat's row until
    /// then, and answers its own request from wherever `on_done` reports.
    pub fn end_chat_sessions(
        &self,
        chat: ChatId,
        on_done: Box<dyn FnOnce(EndOutcome) + Send>,
    ) -> Result<(), AiError> {
        let origin = chat_origin(chat)?;
        self.commands
            .try_send(Command::End { origin, on_done })
            .map_err(|_| control_busy())
    }
}

fn control_busy() -> AiError {
    AiError::new(
        AiErrorClass::Provider,
        "The agent service is busy. Try again.",
        true,
    )
}

/// What this crate negotiates: the session-origin family and every managed
/// capability a chat turn drives a provider through.
pub(crate) fn offer() -> ProtocolOffer {
    let capabilities = [
        capability::CONTROLLER_LEASES,
        capability::RENDER_DELTAS,
        capability::INTERRUPT,
        capability::TERMINATE,
        capability::PROFILE_LAUNCH,
        capability::OBSERVER_OUTPUT,
        capability::OBSERVATION_CURSORS,
        capability::MANAGED_SESSIONS,
        capability::MANAGED_EVENTS,
        capability::MANAGED_CANCEL,
        capability::MANAGED_APPROVAL,
        capability::DYNAMIC_APPROVALS,
        capability::MANAGED_USAGE,
        capability::PROVIDER_READINESS,
        capability::PROVIDER_MODELS,
        capability::PROJECTS,
        capability::SESSION_ORIGIN,
        capability::LAUNCH_SELECTION,
        // The chat draws an agent's questions and answers them (koru-ai
        // D108): its sessions' agents may ask.
        capability::MANAGED_QUESTIONS,
        // A turn is `SendPendingInput`, which the host answers only to a peer
        // that negotiated attachments (protocol v4); the first live turn
        // failed for its absence here.
        capability::ATTACHMENTS,
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

/// A session client error as the chat's own error, content-free.
pub(crate) fn session_error(operation: &'static str, error: ClientError) -> AiError {
    let (class, retryable, message) = match &error {
        ClientError::Session(failure) => match failure.code {
            ErrorCode::CapabilityNotNegotiated | ErrorCode::ProtocolIncompatible => (
                AiErrorClass::Configuration,
                false,
                "Update Koru to chat with a local agent.",
            ),
            ErrorCode::ProfileMissing | ErrorCode::ProfileNotFound => (
                AiErrorClass::Configuration,
                false,
                "This agent is not installed.",
            ),
            ErrorCode::ProviderUnavailable
            | ErrorCode::ProviderIncompatible
            | ErrorCode::ProviderFailed
            | ErrorCode::SpawnFailed => (
                AiErrorClass::ModelUnavailable,
                true,
                "The agent could not be started.",
            ),
            ErrorCode::ProviderModelUnavailable
                if failure.message == koru_session_protocol::REASONING_REJECTED =>
            {
                (
                    AiErrorClass::ModelUnavailable,
                    false,
                    koru_ai_core::AGENT_REASONING_REJECTED,
                )
            }
            ErrorCode::ProviderModelUnavailable => (
                AiErrorClass::ModelUnavailable,
                false,
                "The agent refused that model.",
            ),
            ErrorCode::HostUnavailable | ErrorCode::Backpressure => (
                AiErrorClass::Network,
                true,
                "The agent service is not available right now.",
            ),
            _ => (AiErrorClass::Provider, true, "The agent session failed."),
        },
        ClientError::Disconnected | ClientError::TimedOut | ClientError::Ipc(_) => (
            AiErrorClass::Network,
            true,
            "The agent service is not answering.",
        ),
        ClientError::Handshake(_) | ClientError::Protocol(_) => (
            AiErrorClass::Configuration,
            false,
            "Update Koru to chat with a local agent.",
        ),
        ClientError::UnexpectedResponse => {
            (AiErrorClass::Provider, false, "The agent session failed.")
        }
    };
    tracing::debug!(
        target: "koru_ai::agent",
        operation,
        error = %error,
        "agent session call failed"
    );
    AiError::new(class, message, retryable)
}

/// What a refused start refused, as the refresh event says it: the level
/// (D19) or the model (D7).
fn refusal_reason(error: &ClientError) -> &'static str {
    match error {
        ClientError::Session(failure)
            if failure.message == koru_session_protocol::REASONING_REJECTED =>
        {
            "reasoning"
        }
        _ => "model",
    }
}

fn is_code(error: &ClientError, code: ErrorCode) -> bool {
    matches!(error, ClientError::Session(failure) if failure.code == code)
}

fn is_transport(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Disconnected | ClientError::Ipc(_) | ClientError::TimedOut
    )
}

struct Control {
    endpoint: koru_ipc::Endpoint,
    client: Option<SessionClient>,
    readiness: Vec<ProviderReadiness>,
    readiness_subscription: Option<Subscription>,
    /// When each kind was last asked to re-probe, so a row that keeps
    /// failing is re-probed once per window, not on every picker open.
    refreshed: HashMap<ProviderKind, Instant>,
}

impl Control {
    fn new(endpoint: koru_ipc::Endpoint) -> Self {
        Self {
            endpoint,
            client: None,
            readiness: Vec::new(),
            readiness_subscription: None,
            refreshed: HashMap::new(),
        }
    }

    fn run(mut self, receiver: Receiver<Command>) {
        loop {
            match receiver.recv_timeout(Duration::from_secs(1)) {
                Ok(Command::Ensure {
                    origin,
                    kind,
                    selection,
                    cwd,
                    reply,
                }) => {
                    let _ = reply.send(self.ensure(origin, &kind, selection, cwd));
                }
                Ok(Command::End { origin, on_done }) => {
                    on_done(self.end(origin));
                }
                Ok(Command::Readiness { kind, reply }) => {
                    let _ = reply.send(self.readiness(&kind));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// The control connection, made or remade. A reconnect is a new
    /// `ai_control` hello and therefore a new generation: whatever the old
    /// connection had in flight is stale at the host from now on.
    fn client(&mut self) -> Result<&SessionClient, AiError> {
        if self
            .client
            .as_ref()
            .is_some_and(|client| client.state() == koru_session_client::ConnectionState::Connected)
        {
            return Ok(self.client.as_ref().expect("checked"));
        }
        self.readiness.clear();
        self.readiness_subscription = None;
        let client = SessionClient::connect_as(
            &self.endpoint,
            PRODUCT,
            offer(),
            Some(ClientRole::AiControl),
        )
        .map_err(|error| session_error("connect", error))?;
        if !client.supports_session_origin() || !client.supports_launch_selection() {
            return Err(AiError::new(
                AiErrorClass::Configuration,
                "Update Koru to chat with a local agent.",
                false,
            ));
        }
        tracing::info!(target: "koru_ai::agent", "agent control connection established");
        Ok(self.client.insert(client))
    }

    fn drop_client(&mut self) {
        self.client = None;
        self.readiness.clear();
        self.readiness_subscription = None;
    }

    fn ensure(
        &mut self,
        origin: SessionOrigin,
        kind: &AgentProviderKind,
        selection: Option<LaunchSelection>,
        cwd: Option<std::path::PathBuf>,
    ) -> Result<EnsuredSession, AiError> {
        let deadline = Instant::now() + ENSURE_DEADLINE - Duration::from_secs(5);
        let mut reconnected = false;
        loop {
            let client = self.client()?;
            let (_, lookup) = match client.lookup_origin(origin) {
                Ok(answer) => answer,
                Err(error) if is_transport(&error) && !reconnected => {
                    reconnected = true;
                    self.drop_client();
                    continue;
                }
                Err(error) => return Err(session_error("lookup_origin", error)),
            };
            match lookup.live.as_slice() {
                [session] => {
                    return Ok(EnsuredSession {
                        session: session.id,
                        fresh: false,
                        model: session.model.clone(),
                        active_turn: session.active_turn_id,
                    });
                }
                [] => {}
                _ => {
                    tracing::warn!(
                        target: "koru_ai::agent",
                        live = lookup.live.len(),
                        "a chat has more than one agent conversation; refusing until it is deleted"
                    );
                    return Err(AiError::new(
                        AiErrorClass::Configuration,
                        "This chat has more than one agent conversation. Delete it to end them.",
                        false,
                    ));
                }
            }
            if lookup.creating > 0 {
                if Instant::now() >= deadline {
                    return Err(AiError::new(
                        AiErrorClass::Provider,
                        "The agent is still starting. Try again.",
                        true,
                    ));
                }
                std::thread::sleep(POLL);
                continue;
            }
            let cwd = cwd.clone().unwrap_or_else(default_working_directory);
            let size = GridSize::new(COLUMNS, ROWS).expect("a positive grid");
            match client.create_profile_origin(
                IdempotencyKey::mint(),
                kind.profile.clone(),
                SessionMode::Managed {
                    provider: kind.kind.clone(),
                },
                origin,
                selection.clone(),
                cwd,
                size,
            ) {
                Ok(session) => {
                    tracing::info!(
                        target: "koru_ai::agent",
                        session = %session.id,
                        provider = %kind.kind,
                        "agent session started for a chat"
                    );
                    return Ok(EnsuredSession {
                        session: session.id,
                        fresh: true,
                        model: session.model,
                        active_turn: None,
                    });
                }
                // Somebody's create landed between the lookup and ours: the
                // next lookup finds it, and this turn reattaches to it.
                Err(error) if is_code(&error, ErrorCode::OriginOwned) => continue,
                Err(error) if is_code(&error, ErrorCode::StaleAuthority) && !reconnected => {
                    reconnected = true;
                    self.drop_client();
                    continue;
                }
                Err(error) if is_transport(&error) && !reconnected => {
                    reconnected = true;
                    self.drop_client();
                    continue;
                }
                Err(error) => {
                    if is_code(&error, ErrorCode::ProviderModelUnavailable) {
                        self.refresh_after_refusal(&kind.kind, refusal_reason(&error));
                    }
                    return Err(session_error("create", error));
                }
            }
        }
    }

    /// Drain a chat: terminate what is live, wait out what is in flight,
    /// until the host reports nothing under the origin or the deadline
    /// passes. Nothing is fenced: this thread is the sole creator for
    /// chats, it is busy right here, and the caller refuses new sends for
    /// the chat meanwhile.
    fn end(&mut self, origin: SessionOrigin) -> EndOutcome {
        let deadline = Instant::now() + TEARDOWN_DEADLINE;
        loop {
            let client = match self.client() {
                Ok(client) => client,
                Err(_) => {
                    return EndOutcome::Failed {
                        code: "host_unreachable",
                    };
                }
            };
            let lookup = match client.lookup_origin(origin) {
                Ok((_, lookup)) => lookup,
                Err(error) if is_transport(&error) => {
                    self.drop_client();
                    return EndOutcome::Failed {
                        code: "host_unreachable",
                    };
                }
                Err(_) => {
                    return EndOutcome::Failed {
                        code: "lookup_failed",
                    };
                }
            };
            if lookup.is_empty() {
                return EndOutcome::Ended;
            }
            for session in &lookup.live {
                if let Err(error) = terminate(client, session.id) {
                    tracing::info!(
                        target: "koru_ai::agent",
                        session = %session.id,
                        error = %error,
                        "an agent session could not be ended yet"
                    );
                }
            }
            if Instant::now() >= deadline {
                tracing::warn!(
                    target: "koru_ai::agent",
                    live = lookup.live.len(),
                    creating = lookup.creating,
                    "a chat's agent sessions did not end in time"
                );
                return EndOutcome::Failed { code: "deadline" };
            }
            std::thread::sleep(POLL);
        }
    }

    fn readiness(&mut self, kind: &ProviderKind) -> Result<AgentReadiness, AiError> {
        self.client()?;
        if self.readiness_subscription.is_none() {
            let client = self.client.as_ref().expect("connected above");
            let (rows, subscription) = client
                .subscribe_providers()
                .map_err(|error| session_error("subscribe_providers", error))?;
            self.readiness = rows;
            self.readiness_subscription = Some(subscription);
        }
        let mut lost = false;
        if let Some(subscription) = &self.readiness_subscription {
            loop {
                match subscription.recv_timeout(Duration::ZERO) {
                    Ok(SubscriptionUpdate::Providers { provider }) => {
                        if let Some(row) = self
                            .readiness
                            .iter_mut()
                            .find(|row| row.kind == provider.kind)
                        {
                            *row = provider;
                        } else {
                            self.readiness.push(provider);
                        }
                    }
                    Err(ClientError::TimedOut) => break,
                    Ok(SubscriptionUpdate::Closed { .. }) | Err(_) => {
                        lost = true;
                        break;
                    }
                    Ok(_) => {}
                }
            }
        }
        if lost {
            self.readiness_subscription = None;
            self.readiness.clear();
            return Err(AiError::new(
                AiErrorClass::Network,
                "The agent service is not answering.",
                true,
            ));
        }
        let row = self.readiness.iter().find(|row| &row.kind == kind).cloned();
        // A failed check is a moment, not a verdict — a probe that timed out
        // under load says nothing about the agent. Ask the host to look
        // again, once per window, and report the check as running.
        if row
            .as_ref()
            .is_some_and(|row| matches!(row.availability, ExecutableAvailability::Failed { .. }))
            && refresh_due(&self.refreshed, kind, Instant::now())
        {
            let client = self.client.as_ref().expect("connected above");
            match client.refresh_provider(kind.clone()) {
                Ok(()) => {
                    self.refreshed.insert(kind.clone(), Instant::now());
                    tracing::info!(target: "koru_ai::agent", provider = %kind, "agent readiness re-probe requested");
                    return Ok(row.map(|mut row| {
                        row.availability = ExecutableAvailability::Checking;
                        row
                    }));
                }
                Err(error) => {
                    tracing::warn!(target: "koru_ai::agent", provider = %kind, error = %error, "agent readiness re-probe refused");
                }
            }
        }
        Ok(row)
    }
}

impl Control {
    /// A model the agent refused at the start of a chat's session says its
    /// list may be out of date — an account disconnected, a model withdrawn
    /// — so the host is asked to re-probe the agent, which rebuilds its
    /// list, at most once per window (agent-models D7). The turn still
    /// fails visibly; the next open of the picker shows the list as it is.
    fn refresh_after_refusal(&mut self, kind: &ProviderKind, reason: &'static str) {
        if !refresh_due(&self.refreshed, kind, Instant::now()) {
            return;
        }
        let Some(client) = self.client.as_ref() else {
            return;
        };
        match client.refresh_provider(kind.clone()) {
            Ok(()) => {
                self.refreshed.insert(kind.clone(), Instant::now());
                tracing::info!(target: "koru_ai::agent", event = "agent.model_refused.refresh",
                    provider = %kind, reason, "agent refused a model or level; its list is being rebuilt");
            }
            Err(error) => {
                tracing::warn!(target: "koru_ai::agent", provider = %kind, error = %error,
                    "agent refused a model and its list could not be refreshed");
            }
        }
    }
}

/// How long a failed readiness row stands before a look is asked for again,
/// and how long after one re-probe a refused model may ask for another.
const REFRESH_WINDOW: Duration = Duration::from_secs(30);

/// Whether `kind` may be re-probed again: never asked, or asked longer than
/// a window ago.
fn refresh_due(
    refreshed: &HashMap<ProviderKind, Instant>,
    kind: &ProviderKind,
    now: Instant,
) -> bool {
    refreshed
        .get(kind)
        .is_none_or(|at| now.saturating_duration_since(*at) >= REFRESH_WINDOW)
}

/// End one session through the one lease this connection may hold: take
/// it, say terminate, give it back. A session that already exited answers
/// with its absence, which is the outcome wanted.
fn terminate(client: &SessionClient, session: SessionId) -> Result<(), ClientError> {
    let generation = ViewportGeneration::new(1).expect("one");
    let (lease, render) = match client.attach_live(session, generation, COLUMNS, 1) {
        Ok(attached) => attached,
        Err(error) if is_code(&error, ErrorCode::SessionNotFound) => return Ok(()),
        Err(error) if is_code(&error, ErrorCode::SessionExited) => return Ok(()),
        Err(error) => return Err(error),
    };
    drop(render);
    let result = client.command(SessionCommand {
        idempotency_key: IdempotencyKey::mint(),
        session_id: session,
        controller_lease: lease.token,
        action: RawCommand::Terminate,
    });
    let _ = client.detach(session, lease.token);
    match result {
        Err(error) if is_code(&error, ErrorCode::SessionExited) => Ok(()),
        other => other,
    }
}

/// The host's error code behind a client error, or the nearest one.
fn client_code(error: &ClientError) -> ErrorCode {
    match error {
        ClientError::Session(failure) => failure.code.clone(),
        ClientError::TimedOut => ErrorCode::Backpressure,
        _ => ErrorCode::ProviderUnavailable,
    }
}

#[cfg(test)]
mod tests;
