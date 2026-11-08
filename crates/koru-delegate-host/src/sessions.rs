// SPDX-License-Identifier: MPL-2.0

//! The session host at protocol v15, as the orchestrator's `Sessions` port.
//!
//! One unary connection answers creation, the non-creating lookup, the
//! retirement and the observation; **one connection per controlled
//! session** holds that session's lease, because a connection holds one
//! lease and the orchestrator may drive several runs; and **one connection
//! per stream** reads the agent events and the session events on two
//! threads into one channel, so the orchestrator reads one order.
//!
//! The provider a profile launches is the profile registry's answer
//! (`koru-session-profiles`), never a second table here.

use std::{
    collections::HashMap,
    path::Path,
    str::FromStr,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use koru_delegate_core::{AgentTurnId, HostInstanceId, IdempotencyKey, SessionId};
use koru_delegate_runtime::{
    CreatedSession, Port, PortError, PortResult, SessionEvents, SessionLaunch, SessionUpdate,
    Sessions,
};
use koru_session_client::{ClientError, SessionClient, Subscription};
use koru_session_profiles::{LaunchIntent, MANAGED_PROVIDER_REGISTRATIONS, managed_launch_mode};
use koru_session_protocol::{
    AgentEventObservation, CancelReason, CapabilityName, CapabilitySet, ControllerLeaseToken,
    CreationLookup, GridSize, MAX_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION, PendingInput,
    ProtocolOffer, ProtocolRange, ProviderReadiness, ProviderResumeToken, RawCommand,
    SessionCommand, SessionProfileKey, SessionSummary, StreamSequence, ViewportGeneration,
    capability,
};
use koru_session_wire::SubscriptionUpdate;

const PRODUCT: &str = "koru-delegate";
/// The grid a delegated session is created with. Nobody looks at it until
/// a person takes control, and a window resizes it then.
const COLUMNS: u16 = 120;
const ROWS: u16 = 40;

/// The session host's endpoint, root-qualified under an explicit root —
/// the same rule `koru_session_host::endpoint` states.
pub fn endpoint(
    segment: &str,
    explicit_root: Option<&Path>,
) -> Result<koru_ipc::Endpoint, koru_ipc::IpcError> {
    match explicit_root {
        Some(root) => {
            koru_ipc::Endpoint::for_service_channel_root(koru_ipc::SESSION_HOST, segment, root)
        }
        None => koru_ipc::Endpoint::for_service(koru_ipc::SESSION_HOST, segment),
    }
}

/// What the orchestrator negotiates: the delegation family and every
/// managed capability it drives a provider through.
pub fn offer() -> ProtocolOffer {
    let capabilities = [
        capability::CONTROLLER_LEASES,
        capability::RENDER_DELTAS,
        capability::INTERRUPT,
        capability::TERMINATE,
        capability::ATTACHMENTS,
        capability::PROFILE_LAUNCH,
        capability::OBSERVER_OUTPUT,
        capability::OBSERVATION_CURSORS,
        capability::MANAGED_SESSIONS,
        capability::NATIVE_ATTACHMENT_DELIVERY,
        capability::MANAGED_EVENTS,
        capability::MANAGED_CANCEL,
        capability::MANAGED_APPROVAL,
        capability::DYNAMIC_APPROVALS,
        capability::MANAGED_CHILD_SESSIONS,
        capability::MANAGED_USAGE,
        capability::PROJECTS,
        capability::DELEGATION,
        capability::PROVIDER_MODELS,
        capability::PROVIDER_READINESS,
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

fn connect(endpoint: &koru_ipc::Endpoint) -> Result<SessionClient, ClientError> {
    SessionClient::connect(endpoint, PRODUCT, offer())
}

fn session_error(operation: &'static str, error: ClientError) -> PortError {
    let code = match &error {
        ClientError::Session(failure) => failure.code.as_str().to_owned(),
        ClientError::Disconnected => "disconnected".to_owned(),
        ClientError::TimedOut => "timed_out".to_owned(),
        _ => "unavailable".to_owned(),
    };
    PortError::new(Port::Sessions, code, format!("{operation}: {error}"))
}

fn parse_instance(instance_id: &str) -> PortResult<HostInstanceId> {
    uuid::Uuid::parse_str(instance_id)
        .map(HostInstanceId::from_uuid)
        .map_err(|_| PortError::new(Port::Sessions, "instance_id", "not a UUID"))
}

/// A controlled session: the connection that holds its lease.
struct Controller {
    client: SessionClient,
    lease: ControllerLeaseToken,
}

pub struct HostSessions {
    endpoint: koru_ipc::Endpoint,
    unary: SessionClient,
    controllers: HashMap<SessionId, Controller>,
    provider_readiness: Vec<ProviderReadiness>,
    provider_subscription: Option<Subscription>,
}

impl HostSessions {
    /// A second unary connection for the dedicated schedule-preparation
    /// thread. Startup already proved the host reachable; later disconnects
    /// use the ordinary reconnect path.
    pub fn for_scheduler(endpoint: koru_ipc::Endpoint) -> Result<Self, String> {
        let unary =
            connect(&endpoint).map_err(|error| format!("schedule provider readiness: {error}"))?;
        Ok(Self {
            endpoint,
            unary,
            controllers: HashMap::new(),
            provider_readiness: Vec::new(),
            provider_subscription: None,
        })
    }

    /// Connect to the host the Agent started before this worker, waiting
    /// for it to answer.
    pub(crate) fn connect(
        endpoint: koru_ipc::Endpoint,
        deadline: Instant,
        stop: &mpsc::Receiver<&'static str>,
    ) -> Result<Self, crate::Wait> {
        let mut last;
        loop {
            match connect(&endpoint) {
                Ok(unary) => {
                    return Ok(Self {
                        endpoint,
                        unary,
                        controllers: HashMap::new(),
                        provider_readiness: Vec::new(),
                        provider_subscription: None,
                    });
                }
                Err(error) => last = error.to_string(),
            }
            if Instant::now() >= deadline {
                return Err(crate::Wait::Failed(format!(
                    "the session host did not answer in time: {last}"
                )));
            }
            if stop.recv_timeout(Duration::from_millis(250)).is_ok() {
                return Err(crate::Wait::Stopped);
            }
        }
    }

    /// The unary connection, reconnected if the host restarted under it.
    fn unary(&mut self) -> PortResult<&SessionClient> {
        if self.unary.state() != koru_session_client::ConnectionState::Connected {
            self.unary =
                connect(&self.endpoint).map_err(|error| session_error("connect", error))?;
            self.provider_readiness.clear();
            self.provider_subscription = None;
        }
        Ok(&self.unary)
    }

    fn refresh_provider_readiness(&mut self) -> PortResult<()> {
        self.unary()?;
        if self.provider_subscription.is_none() {
            let (rows, subscription) = self
                .unary
                .subscribe_providers()
                .map_err(|error| session_error("provider readiness", error))?;
            self.provider_readiness = rows;
            self.provider_subscription = Some(subscription);
        }
        let mut lost = false;
        if let Some(subscription) = &self.provider_subscription {
            loop {
                match subscription.recv_timeout(Duration::ZERO) {
                    Ok(SubscriptionUpdate::Providers { provider }) => {
                        if let Some(row) = self
                            .provider_readiness
                            .iter_mut()
                            .find(|row| row.kind == provider.kind)
                        {
                            *row = provider;
                        } else {
                            self.provider_readiness.push(provider);
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
            self.provider_readiness.clear();
            self.provider_subscription = None;
            return Err(PortError::new(
                Port::Sessions,
                "provider_subscription_lost",
                "provider readiness subscription was lost",
            ));
        }
        Ok(())
    }

    fn controller(&self, session: SessionId) -> PortResult<&Controller> {
        self.controllers.get(&session).ok_or_else(|| {
            PortError::new(
                Port::Sessions,
                "no_controller",
                "this worker holds no lease",
            )
        })
    }
}

impl Sessions for HostSessions {
    fn supports_delegation(&self) -> bool {
        self.unary.supports_delegation()
    }

    fn profile_delegatable(&mut self, profile: &SessionProfileKey) -> PortResult<bool> {
        #[cfg(feature = "test-support")]
        if test_fixture_preflight_admitted(profile) {
            // Repository process tests compile provider probes out. This
            // admits only their copied Codex fixture at preflight; the real
            // delegated launch and its live handshake still decide execution.
            return Ok(true);
        }
        self.refresh_provider_readiness()?;
        let Some(profile) = profile_for_readiness(profile)? else {
            return Ok(false);
        };
        Ok(managed_launch_mode(
            &profile,
            LaunchIntent::Delegation,
            MANAGED_PROVIDER_REGISTRATIONS,
            Some(&self.provider_readiness),
        )
        .is_ok())
    }

    fn host_instance(&mut self) -> PortResult<HostInstanceId> {
        let client = self.unary()?;
        parse_instance(&client.host().instance_id)
    }

    fn create_delegated(&mut self, launch: SessionLaunch) -> PortResult<CreatedSession> {
        let profile = koru_session_profiles::resolve(&launch.profile)
            .map_err(|code| PortError::new(Port::Sessions, code.as_str(), "unknown profile"))?;
        let provider = profile.provider.ok_or_else(|| {
            PortError::new(
                Port::Sessions,
                "not_managed",
                "the profile launches no managed provider",
            )
        })?;
        let size = GridSize::new(COLUMNS, ROWS)
            .map_err(|code| PortError::new(Port::Sessions, code.as_str(), "grid"))?;
        let summary = self
            .unary()?
            .create_delegated(
                launch.key,
                launch.profile,
                provider.clone(),
                launch.project,
                launch.delegation,
                launch.working_directory,
                size,
            )
            .map_err(|error| session_error("create", error))?;
        Ok(CreatedSession {
            session: summary.id,
            provider,
            requested_model: summary.model.and_then(|model| model.requested),
        })
    }

    fn lookup(&mut self, key: IdempotencyKey) -> PortResult<(HostInstanceId, CreationLookup)> {
        let (instance, lookup) = self
            .unary()?
            .lookup_creation(key)
            .map_err(|error| session_error("lookup", error))?;
        Ok((parse_instance(&instance)?, lookup))
    }

    fn retire(&mut self, key: IdempotencyKey) -> PortResult<(HostInstanceId, CreationLookup)> {
        let (instance, lookup) = self
            .unary()?
            .retire_creation(key)
            .map_err(|error| session_error("retire", error))?;
        Ok((parse_instance(&instance)?, lookup))
    }

    fn summary(&mut self, session: SessionId) -> PortResult<Option<SessionSummary>> {
        let sessions = self
            .unary()?
            .list()
            .map_err(|error| session_error("list", error))?;
        Ok(sessions.into_iter().find(|summary| summary.id == session))
    }

    fn attach(&mut self, session: SessionId) -> PortResult<()> {
        if self.controllers.contains_key(&session) {
            return Ok(());
        }
        let client = connect(&self.endpoint).map_err(|error| session_error("connect", error))?;
        let generation = ViewportGeneration::new(1).expect("one");
        let (lease, render) = client
            .attach_live(session, generation, COLUMNS, 1)
            .map_err(|error| session_error("attach", error))?;
        // Nobody draws this session here; the render stream is silenced.
        let _ = client.suspend_render(session, lease.token);
        drop(render);
        self.controllers.insert(
            session,
            Controller {
                client,
                lease: lease.token,
            },
        );
        Ok(())
    }

    fn release_control(&mut self, session: SessionId) -> PortResult<()> {
        let Some(controller) = self.controllers.remove(&session) else {
            return Ok(());
        };
        controller
            .client
            .release_control(session, controller.lease)
            .map_err(|error| session_error("release", error))
    }

    fn submit(&mut self, session: SessionId, text: String) -> PortResult<AgentTurnId> {
        let controller = self.controller(session)?;
        let result = controller
            .client
            .send_pending_input(
                session,
                controller.lease,
                PendingInput {
                    model: None,
                    idempotency_key: IdempotencyKey::mint(),
                    text,
                    attachment_ids: Vec::new(),
                    mode: None,
                    fast: None,
                    reasoning: None,
                    skills: Vec::new(),
                },
            )
            .map_err(|error| session_error("submit", error))?;
        Ok(result.turn_id)
    }

    fn cancel_turn(
        &mut self,
        session: SessionId,
        turn: AgentTurnId,
        reason: CancelReason,
    ) -> PortResult<()> {
        let controller = self.controller(session)?;
        controller
            .client
            .cancel_turn_for(
                session,
                controller.lease,
                IdempotencyKey::mint(),
                turn,
                reason,
            )
            .map_err(|error| session_error("cancel", error))
    }

    fn terminate(&mut self, session: SessionId) -> PortResult<()> {
        if let Some(controller) = self.controllers.remove(&session) {
            return controller
                .client
                .command(SessionCommand {
                    idempotency_key: IdempotencyKey::mint(),
                    session_id: session,
                    controller_lease: controller.lease,
                    action: RawCommand::Terminate,
                })
                .map_err(|error| session_error("terminate", error));
        }
        // A session this worker holds no lease on. If a run created it, the
        // host ends it without one (§7.6: a cancellation reaches the
        // process tree *including while a person holds control*, and a
        // person who took control cannot be asked to consent to the run
        // they borrowed ending). Anything else — a child the provider
        // spawned (§7.3) — is ended the ordinary way, by taking the
        // controller for the one command.
        let client = connect(&self.endpoint).map_err(|error| session_error("connect", error))?;
        match client.terminate_delegated(session) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let refused = matches!(
                    &error,
                    ClientError::Session(failure)
                        if failure.code == koru_session_protocol::ErrorCode::Forbidden
                );
                if !refused {
                    return Err(session_error("terminate", error));
                }
            }
        }
        let (lease, _render) = client
            .attach_live(
                session,
                ViewportGeneration::new(1).expect("one"),
                COLUMNS,
                1,
            )
            .map_err(|error| session_error("attach", error))?;
        client
            .command(SessionCommand {
                idempotency_key: IdempotencyKey::mint(),
                session_id: session,
                controller_lease: lease.token,
                action: RawCommand::Terminate,
            })
            .map_err(|error| session_error("terminate", error))
    }

    fn resume_token(&mut self, _session: SessionId) -> PortResult<Option<ProviderResumeToken>> {
        // The adapter's resume seam (§7.6). A token is minted by an adapter
        // that declared `ResumeSupport::Native`, and it lives inside the
        // session worker's `AgentControl`; no adapter in this tree declares
        // it — Codex declares `Unsupported` — so no host verb carries one
        // out, and every resume takes the recorded-context fallback. When
        // one does, this answers with the token and nothing above changes.
        Ok(None)
    }

    fn observe(&mut self, session: SessionId) -> PortResult<AgentEventObservation> {
        self.unary()?
            .observe_agent_events(session)
            .map_err(|error| session_error("observe", error))
    }

    fn subscribe(
        &mut self,
        session: SessionId,
        after: Option<StreamSequence>,
    ) -> PortResult<Box<dyn SessionEvents>> {
        let client =
            Arc::new(connect(&self.endpoint).map_err(|error| session_error("connect", error))?);
        let agent = client
            .subscribe_agent_events_from(session, after)
            .map_err(|error| session_error("subscribe", error))?;
        let events = client
            .subscribe_events_from(session, None)
            .map_err(|error| session_error("subscribe", error))?;
        let (send, receive) = mpsc::channel();
        for (name, subscription) in [
            ("koru-delegate agent events", agent),
            ("koru-delegate session events", events),
        ] {
            let send = send.clone();
            std::thread::Builder::new()
                .name(name.into())
                .stack_size(128 * 1024)
                .spawn(move || {
                    loop {
                        let update = match subscription.recv() {
                            Ok(update) => update,
                            Err(_) => {
                                let _ = send.send(SessionUpdate::Closed { error: None });
                                return;
                            }
                        };
                        let update = match update {
                            SubscriptionUpdate::AgentEvent { event } => SessionUpdate::Agent(event),
                            SubscriptionUpdate::AgentEventGap { expected, actual } => {
                                SessionUpdate::AgentGap { expected, actual }
                            }
                            SubscriptionUpdate::Event { event } => SessionUpdate::Session(event),
                            SubscriptionUpdate::Closed { error } => {
                                let _ = send.send(SessionUpdate::Closed { error });
                                return;
                            }
                            _ => continue,
                        };
                        if send.send(update).is_err() {
                            return;
                        }
                    }
                })
                .map_err(|error| PortError::new(Port::Sessions, "thread", error.to_string()))?;
        }
        Ok(Box::new(LiveEvents {
            receive,
            _client: client,
        }))
    }
}

#[cfg(feature = "test-support")]
fn test_fixture_preflight_admitted(profile: &SessionProfileKey) -> bool {
    let fixture_on_path = std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| {
            directory
                .file_name()
                .is_some_and(|name| name == "codex-fixture-bin")
                && directory
                    .join(if cfg!(windows) { "codex.cmd" } else { "codex" })
                    .is_file()
                && (!cfg!(windows) || directory.join("codex-fixture.ps1").is_file())
        })
    });
    test_fixture_preflight_guards(
        profile,
        std::env::var_os("DELEGATE_PROCESS_TEST_CLOCK").is_some(),
        std::env::var("DELEGATE_PROCESS_TEST_WAKE_AUTHORITY")
            .is_ok_and(|authority| authority == "delegate-process-test"),
        std::env::var_os("CODEX_FIXTURE_TRACE").is_some(),
        fixture_on_path,
    )
}

#[cfg(feature = "test-support")]
fn test_fixture_preflight_guards(
    profile: &SessionProfileKey,
    clock: bool,
    authority: bool,
    trace: bool,
    command: bool,
) -> bool {
    matches!(profile, SessionProfileKey::BuiltIn(key) if key.as_str() == "codex")
        && clock
        && authority
        && trace
        && command
}

fn profile_for_readiness(
    profile: &SessionProfileKey,
) -> PortResult<Option<koru_session_profiles::LaunchProfile>> {
    match koru_session_profiles::resolve(profile) {
        Ok(profile) => Ok(Some(profile)),
        Err(koru_session_protocol::ErrorCode::ProfileNotFound) => Ok(None),
        Err(code) => Err(PortError::new(
            Port::Sessions,
            code.as_str(),
            "profile could not be resolved",
        )),
    }
}

/// A session's merged stream. Dropping it disconnects the connection the
/// two readers block on, which ends them.
struct LiveEvents {
    receive: mpsc::Receiver<SessionUpdate>,
    _client: Arc<SessionClient>,
}

impl SessionEvents for LiveEvents {
    fn next(&mut self, timeout: Duration) -> Option<SessionUpdate> {
        match self.receive.recv_timeout(timeout) {
            Ok(update) => Some(update),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Some(SessionUpdate::Closed { error: None })
            }
        }
    }
}

impl Drop for LiveEvents {
    fn drop(&mut self) {
        self._client.disconnect();
    }
}

#[cfg(test)]
mod tests;
