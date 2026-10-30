// SPDX-License-Identifier: MPL-2.0

mod antigravity;
mod claude;
mod config;
mod diagnostics;
mod events;
mod grok;
mod input;
mod models;
mod probe;
mod routes;

pub use config::{AcpConfig, AcpRoute};
pub use probe::{AcpCompatibilityProbe, AcpFactory};

use std::collections::{BTreeMap, VecDeque};
use std::str::FromStr;
use std::time::{Duration, Instant};

use agent_client_protocol_schema::v1 as acp;
use koru_session_protocol::{
    AgentEvent, AgentTurnId, ApprovalDecision, ApprovalId, CapabilityName, CapabilitySet,
    ErrorCode, ManagedState, SessionFailure, SessionId, ToolCallId, ToolKind, capability,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::process::{JsonRpcTransport, StdioJsonRpc, TransportReceive};
use super::*;

/// Independent of the pinned Rust schema package version.
pub const ACP_PROTOCOL_VERSION: u64 = 1;

/// One process, one ACP session. No provider-native identity leaves this owner.
pub struct AcpControl {
    config: AcpConfig,
    transport: Option<Box<dyn JsonRpcTransport>>,
    capabilities: Option<AgentCapabilities>,
    session_id: Option<SessionId>,
    native_session: Option<String>,
    mcp_sse: bool,
    wrapper_auth: Option<claude::WrapperAuth>,
    grok_inventory: Option<String>,
    /// What the session advertised under its `model` config option, for
    /// discovery; empty where the route advertises none.
    model_options: Vec<koru_session_protocol::ProviderModel>,
    next_request_id: u64,
    turns: BTreeMap<AgentTurnId, Turn>,
    active: Option<AgentTurnId>,
    tools: BTreeMap<String, Tool>,
    approvals: BTreeMap<ApprovalId, PendingApproval>,
    events: VecDeque<AgentEvent>,
    diagnostics: ProviderDiagnostics,
    log: diagnostics::SessionLog,
    unusable: Option<ProviderFailure>,
    /// Separate from the bounded content queue: a cancellation deadline must
    /// deliver its terminal event even with every content slot occupied.
    forced_terminal: Option<AgentEvent>,
    shut_down: bool,
    image_reader: Box<dyn input::ImageReader>,
}

struct Turn {
    request_id: u64,
    fingerprint: blake3::Hash,
    terminal: bool,
    cancelling: Option<Cancelling>,
}

#[derive(Clone, Copy)]
struct Cancelling {
    deadline: Instant,
    unrepresentable: bool,
}

struct Tool {
    id: ToolCallId,
    kind: ToolKind,
    completed: bool,
    /// The latest of what the tool produced, as its updates carried it;
    /// the completion takes it (UC17).
    output: Option<String>,
}

#[derive(Clone)]
struct PendingApproval {
    request_id: Value,
    turn_id: AgentTurnId,
    choices: Vec<koru_session_protocol::ApprovalChoice>,
    native_options: BTreeMap<koru_session_protocol::ApprovalChoiceId, String>,
}

impl AcpControl {
    pub fn new(config: AcpConfig) -> Self {
        Self::with_transport(config, None)
    }

    fn with_transport(config: AcpConfig, transport: Option<Box<dyn JsonRpcTransport>>) -> Self {
        Self {
            config,
            transport,
            capabilities: None,
            session_id: None,
            native_session: None,
            mcp_sse: false,
            wrapper_auth: None,
            grok_inventory: None,
            model_options: Vec::new(),
            next_request_id: 1,
            turns: BTreeMap::new(),
            active: None,
            tools: BTreeMap::new(),
            approvals: BTreeMap::new(),
            events: VecDeque::new(),
            diagnostics: ProviderDiagnostics::default(),
            log: diagnostics::SessionLog::default(),
            unusable: None,
            forced_terminal: None,
            shut_down: false,
            image_reader: Box::new(input::FileImageReader),
        }
    }

    fn transport(&mut self) -> Result<&mut (dyn JsonRpcTransport + '_), ProviderFailure> {
        match self.transport.as_mut() {
            Some(transport) => Ok(transport.as_mut()),
            None => Err(invalid_state(ProviderStage::Read)),
        }
    }

    /// The models the started session advertised; empty before start.
    pub(super) fn model_options(&self) -> &[koru_session_protocol::ProviderModel] {
        &self.model_options
    }

    fn ensure_transport(&mut self) -> Result<(), ProviderFailure> {
        if self.transport.is_none() {
            let mut environment = self.config.route.environment.clone();
            let scratch = if self.config.route.dialect.unpacks_into_temp() {
                let directory = private_temp()?;
                for name in ["TMP", "TEMP"] {
                    environment.push((name.into(), directory.clone().into_os_string()));
                }
                Some(directory)
            } else {
                None
            };
            let spawned = StdioJsonRpc::spawn(
                &self.config.executable,
                &self.config.route.arguments,
                &environment,
                &self.config.working_directory,
                self.config.maximum_frame_bytes,
                self.config.wake.clone(),
            );
            let mut transport = match spawned {
                Ok(transport) => transport,
                Err(error) => {
                    if let Some(directory) = scratch {
                        let _ = std::fs::remove_dir_all(directory);
                    }
                    return Err(error);
                }
            };
            if let Some(directory) = scratch {
                transport.own_scratch(directory);
            }
            self.transport = Some(Box::new(transport));
        }
        Ok(())
    }

    fn send_request(&mut self, method: &str, params: Value) -> Result<u64, ProviderFailure> {
        let id = self.next_request_id;
        self.next_request_id = id
            .checked_add(1)
            .ok_or_else(|| invalid_state(ProviderStage::Write))?;
        self.transport()?
            .send(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))?;
        Ok(id)
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, ProviderFailure> {
        let id = self.send_request(method, params)?;
        let deadline = Instant::now() + self.config.request_timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(failure(
                    ErrorCode::ProviderUnavailable,
                    ProviderStage::Initialize,
                    ProviderDiagnostic::Timeout,
                ));
            }
            match self.transport()?.receive(remaining)? {
                TransportReceive::Frame(frame) => {
                    self.diagnostics.frames_read = self.diagnostics.frames_read.saturating_add(1);
                    if frame.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                        return Err(handshake_rejected());
                    }
                    if frame.get("method").is_none()
                        && frame.get("id").and_then(Value::as_u64) == Some(id)
                    {
                        if frame.get("result").is_some() == frame.get("error").is_some() {
                            return Err(handshake_rejected());
                        }
                        if let Some(error) = frame.get("error") {
                            if error.get("code").and_then(Value::as_i64).is_none()
                                || error.get("message").and_then(Value::as_str).is_none()
                            {
                                return Err(handshake_rejected());
                            }
                            return Err(acp_probe_failure(error, self.config.route.is_claude()));
                        }
                        return frame.get("result").cloned().ok_or_else(handshake_rejected);
                    }
                    self.handle_frame(frame)?;
                }
                TransportReceive::Timeout => {
                    return Err(failure(
                        ErrorCode::ProviderUnavailable,
                        ProviderStage::Initialize,
                        ProviderDiagnostic::Timeout,
                    ));
                }
                TransportReceive::Woken => {}
            }
        }
    }

    fn ensure_usable(&mut self, stage: ProviderStage) -> Result<(), ProviderFailure> {
        self.expire_cancel();
        if self.shut_down || self.unusable.is_some() {
            return Err(invalid_state(stage));
        }
        Ok(())
    }

    fn queue(&mut self, event: AgentEvent) -> Result<(), ProviderFailure> {
        if self.events.len() + self.approvals.len() >= self.config.event_capacity {
            return Err(failure(
                ErrorCode::Backpressure,
                ProviderStage::Read,
                ProviderDiagnostic::Backpressure,
            ));
        }
        self.log.observe(&event, self.diagnostics);
        self.events.push_back(event);
        self.diagnostics.maximum_queue_depth =
            self.diagnostics.maximum_queue_depth.max(self.events.len());
        Ok(())
    }

    fn mark_unknown(&mut self) {
        self.diagnostics.unknown_events = self.diagnostics.unknown_events.saturating_add(1);
    }

    fn cancel_active(
        &mut self,
        turn_id: AgentTurnId,
        unrepresentable: bool,
    ) -> Result<(), ProviderFailure> {
        let turn = self.turns.get(&turn_id).ok_or_else(|| {
            failure(
                ErrorCode::TurnNotFound,
                ProviderStage::Cancel,
                ProviderDiagnostic::InvalidState,
            )
        })?;
        if turn.terminal || turn.cancelling.is_some() {
            return Ok(());
        }
        // Record before sending so a failed write cannot leave an accepted
        // cancel without a deadline. The worker also shuts down on write error.
        self.turns.get_mut(&turn_id).unwrap().cancelling = Some(Cancelling {
            deadline: Instant::now() + self.config.cancel_timeout,
            unrepresentable,
        });
        self.log.cancel_requested(unrepresentable);
        self.expire_permissions(turn_id)?;
        let params = json!({"sessionId":self.native_session});
        self.transport()?
            .send(&json!({"jsonrpc":"2.0", "method":"session/cancel", "params":params}))
    }

    fn expire_cancel(&mut self) {
        let Some(turn_id) = self.active else { return };
        let Some(cancelling) = self.turns[&turn_id].cancelling else {
            return;
        };
        if Instant::now() < cancelling.deadline {
            return;
        }
        self.log.cancel_deadline();
        self.turns.get_mut(&turn_id).unwrap().terminal = true;
        self.active = None;
        self.tools.clear();
        self.approvals.clear();
        self.events.retain(|event| {
            !matches!(
                event,
                AgentEvent::MessageDelta { .. }
                    | AgentEvent::ToolStarted { .. }
                    | AgentEvent::ToolCompleted { .. }
                    | AgentEvent::ProviderState { .. }
            )
        });
        self.forced_terminal = Some(if cancelling.unrepresentable {
            failed_approval_event(turn_id)
        } else {
            AgentEvent::TurnCancelled {
                turn_id,
                reason: Default::default(),
            }
        });
        self.log
            .observe(self.forced_terminal.as_ref().unwrap(), self.diagnostics);
        self.unusable = Some(failure(
            ErrorCode::ProviderFailed,
            ProviderStage::Cancel,
            ProviderDiagnostic::Timeout,
        ));
    }

    fn fail_transport(&mut self, mut error: ProviderFailure) {
        if error.diagnostic == ProviderDiagnostic::UnexpectedExit {
            error.code = ErrorCode::SessionExited;
        }
        if error.diagnostic == ProviderDiagnostic::MalformedFrame {
            self.diagnostics.malformed_frames = self.diagnostics.malformed_frames.saturating_add(1);
        }
        if self.unusable.is_none() {
            self.log.failure("transport", &error);
        }
        if let Some(turn_id) = self.active.take() {
            self.turns.get_mut(&turn_id).unwrap().terminal = true;
            self.forced_terminal = Some(failed_event(turn_id, error.code.clone()));
            self.log
                .observe(self.forced_terminal.as_ref().unwrap(), self.diagnostics);
        }
        self.events.retain(|event| {
            !matches!(
                event,
                AgentEvent::MessageDelta { .. }
                    | AgentEvent::ToolStarted { .. }
                    | AgentEvent::ToolCompleted { .. }
                    | AgentEvent::ProviderState { .. }
            )
        });
        self.tools.clear();
        for (approval_id, pending) in std::mem::take(&mut self.approvals) {
            let event = AgentEvent::ApprovalExpired {
                turn_id: pending.turn_id,
                approval_id,
            };
            self.log.observe(&event, self.diagnostics);
            self.events.push_back(event);
        }
        self.unusable = Some(error);
    }

    fn finish(&mut self, stop: Option<&str>) -> Result<(), ProviderFailure> {
        let Some(turn_id) = self.active else {
            self.mark_unknown();
            return Ok(());
        };
        let unrepresentable = self.turns[&turn_id]
            .cancelling
            .is_some_and(|value| value.unrepresentable);
        self.expire_permissions(turn_id)?;
        let event = if unrepresentable {
            failed_approval_event(turn_id)
        } else {
            match stop {
                Some("end_turn") => AgentEvent::TurnCompleted { turn_id },
                Some("cancelled") => AgentEvent::TurnCancelled {
                    turn_id,
                    reason: Default::default(),
                },
                _ => failed_event(turn_id, ErrorCode::ProviderFailed),
            }
        };
        // Queue before retiring the active turn, so backpressure still has an
        // active turn to fail through the reserved terminal slot.
        self.queue(event)?;
        self.turns.get_mut(&turn_id).unwrap().terminal = true;
        self.active = None;
        self.tools.clear();
        Ok(())
    }

    fn start_session(&mut self, start: AgentStart) -> Result<AgentCapabilities, ProviderFailure> {
        self.ensure_usable(ProviderStage::Initialize)?;
        if let Some(capabilities) = &self.capabilities {
            return if self.session_id == Some(start.session_id) {
                Ok(capabilities.clone())
            } else {
                Err(invalid_state(ProviderStage::Initialize))
            };
        }
        if start.resume.is_some() {
            return Err(failure(
                ErrorCode::InvalidRequest,
                ProviderStage::Initialize,
                ProviderDiagnostic::InvalidInput,
            ));
        }
        if !self.config.route.supports(&self.config.executable_version) {
            return Err(failure(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Version,
                ProviderDiagnostic::VersionRejected,
            ));
        }
        if self.config.delegation.is_some()
            && self.config.route.child_sessions == ChildSessionPolicy::Uncontrolled
        {
            return Err(failure(
                ErrorCode::CapabilityNotNegotiated,
                ProviderStage::Initialize,
                ProviderDiagnostic::HandshakeRejected,
            ));
        }
        self.ensure_transport()?;
        let initialized = self.call(
            "initialize",
            json!({
                "protocolVersion":ACP_PROTOCOL_VERSION,
                "clientInfo":{"name":"koru","version":env!("CARGO_PKG_VERSION")},
                "clientCapabilities":{}
            }),
        )?;
        decode::<acp::InitializeResponse>(&initialized).map_err(|_| handshake_rejected())?;
        if initialized.get("protocolVersion").and_then(Value::as_u64) != Some(ACP_PROTOCOL_VERSION)
        {
            return Err(handshake_rejected());
        }
        if self.config.route.is_claude() {
            claude::validate_handshake(&initialized)?;
        }
        if self.config.route.is_antigravity() {
            antigravity::validate_handshake(&initialized, &self.config.executable_version)?;
        }
        // serde's optional-field fallback is not evidence of a capability.
        let http = initialized
            .pointer("/agentCapabilities/mcpCapabilities/http")
            .and_then(Value::as_bool)
            == Some(true);
        self.mcp_sse = initialized
            .pointer("/agentCapabilities/mcpCapabilities/sse")
            .and_then(Value::as_bool)
            == Some(true);
        let image = initialized
            .pointer("/agentCapabilities/promptCapabilities/image")
            .and_then(Value::as_bool)
            == Some(true);
        if self.config.delegation.is_some() && !http {
            return Err(failure(
                ErrorCode::CapabilityNotNegotiated,
                ProviderStage::Initialize,
                ProviderDiagnostic::HandshakeRejected,
            ));
        }
        if let Some(method) = self.config.route.cached_auth_method.clone() {
            let advertised = initialized
                .get("authMethods")
                .and_then(Value::as_array)
                .is_some_and(|methods| {
                    methods
                        .iter()
                        .any(|entry| entry["id"].as_str() == Some(&method))
                });
            if !advertised {
                return Err(handshake_rejected());
            }
            let mut auth = json!({"methodId":method});
            if let Some(meta) = &self.config.route.authenticate_meta {
                auth["_meta"] = meta.clone();
            }
            self.call("authenticate", auth)?;
        }
        let mut servers = Vec::new();
        if let Some(delegation) = &self.config.delegation {
            let headers = delegation
                .credential
                .as_ref()
                .map(|bearer| {
                    vec![json!({
                        "name":"Authorization", "value":format!("Bearer {}", bearer.as_str())
                    })]
                })
                .unwrap_or_default();
            servers.push(json!({"type":"http","name":KORU_MCP_SERVER_NAME,
                "url":delegation.mcp_endpoint.as_str(),"headers":headers}));
        }
        // A stdio server is ACP's untagged shape: name, command, args, env.
        for server in &self.config.mcp_servers {
            servers.push(json!({
                "name": server.name,
                "command": server.command.to_string_lossy(),
                "args": server.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>(),
                "env": server.env.iter().map(|(name, value)| json!({
                    "name": name.to_string_lossy(), "value": value.to_string_lossy()
                })).collect::<Vec<_>>(),
            }));
        }
        let cwd = self.config.working_directory.to_str().ok_or_else(|| {
            failure(
                ErrorCode::InvalidRequest,
                ProviderStage::Initialize,
                ProviderDiagnostic::InvalidInput,
            )
        })?;
        let mut params = json!({"cwd":cwd,"mcpServers":servers});
        if let Some(meta) = &self.config.route.session_new_meta {
            params["_meta"] = meta.clone();
        }
        let session = self.call("session/new", params)?;
        decode::<acp::NewSessionResponse>(&session).map_err(|_| handshake_rejected())?;
        let native_session = session["sessionId"]
            .as_str()
            .filter(|value| valid_native_id(value))
            .ok_or_else(handshake_rejected)?
            .to_owned();
        self.model_options = models::config_models(&session, self.config.route.provider.as_str());
        let requested_model = self.config.requested_model();
        if let Some(model) = &requested_model {
            // A model the session did not advertise fails the start, on any
            // route: ACP's `session/set_config_option` is the same verb for
            // every dialect, and the answer is checked.
            models::validate_selection(&session, model)?;
            let selected = self
                .call(
                    "session/set_config_option",
                    json!({
                        "sessionId":native_session, "configId":"model", "value":model.as_str()
                    }),
                )
                .map_err(|_| models::selection_unavailable())?;
            models::validate_applied(&selected, model)?;
            tracing::info!(target: "koru::acp", event = "acp.model.selected", session_id = %start.session_id, selected = true);
        }
        if self.config.route.is_claude() {
            self.await_wrapper_auth()?;
        }
        self.native_session = Some(native_session.clone());
        if self.config.route.is_grok() {
            self.await_grok_inventory()?;
        }
        let mut names = vec![
            capability::MANAGED_EVENTS,
            capability::MANAGED_CANCEL,
            capability::MANAGED_APPROVAL,
            capability::DYNAMIC_APPROVALS,
        ];
        if self.config.route.dialect == routes::AcpDialect::OpenCode
            || !self.model_options.is_empty()
        {
            names.push(capability::PROVIDER_MODELS);
        }
        if image {
            names.push(capability::NATIVE_ATTACHMENT_DELIVERY);
        }
        let mut delegation = DelegationFacts::undeclared();
        delegation.starts_in_cwd = true;
        delegation.distinguishes_outcomes = true;
        delegation.follows_up = true;
        delegation.reports_tool_activity = true;
        delegation.mcp = if http {
            McpSupport::Supported
        } else {
            McpSupport::Unsupported
        };
        delegation.child_sessions = self.config.route.child_sessions;
        delegation.economics.model.requested = requested_model;
        let capabilities = AgentCapabilities {
            provider: self.config.route.provider.clone(),
            executable_version: self.config.executable_version.clone(),
            protocol_version: ProviderVersion::from_str("1").expect("static version"),
            capabilities: CapabilitySet::new(
                names
                    .into_iter()
                    .map(|name| CapabilityName::from_str(name).expect("static capability")),
            ),
            delegation,
        };
        self.session_id = Some(start.session_id);
        self.native_session = Some(native_session);
        self.capabilities = Some(capabilities.clone());
        self.queue(AgentEvent::ProviderState {
            provider: self.config.route.provider.clone(),
            state: ManagedState::Idle,
        })?;
        Ok(capabilities)
    }
}

impl AgentControl for AcpControl {
    fn start(&mut self, start: AgentStart) -> Result<AgentCapabilities, ProviderFailure> {
        let first = self.capabilities.is_none();
        if first {
            self.log.start(
                &self.config.route,
                self.config.delegation.is_some(),
                start.session_id,
            );
        }
        let result = self.start_session(start);
        if first {
            match &result {
                Ok(capabilities) => self.log.ready(capabilities),
                Err(error) => self.log.failure("start", error),
            }
        }
        result
    }

    fn capabilities(&self) -> Option<&AgentCapabilities> {
        self.capabilities.as_ref()
    }

    fn submit(&mut self, input: AgentInput) -> Result<(), ProviderFailure> {
        self.ensure_usable(ProviderStage::Submit)?;
        input.validate(self.config.maximum_input_bytes)?;
        let fingerprint = input::fingerprint(&input);
        if let Some(turn) = self.turns.get(&input.turn_id) {
            return if turn.fingerprint == fingerprint {
                Ok(())
            } else {
                Err(failure(
                    ErrorCode::IdempotencyConflict,
                    ProviderStage::Submit,
                    ProviderDiagnostic::InvalidInput,
                ))
            };
        }
        if self.capabilities.is_none() || self.active.is_some() {
            return Err(invalid_state(ProviderStage::Submit));
        }
        // Reserve the start and attachment acknowledgements before accepting.
        if self.events.len() + 1 + input.attachments.len() > self.config.event_capacity {
            return Err(failure(
                ErrorCode::Backpressure,
                ProviderStage::Submit,
                ProviderDiagnostic::Backpressure,
            ));
        }
        let prompt = self.prompt(&input)?;
        let params = json!({"sessionId":self.native_session,"prompt":prompt});
        let request_id = match self.send_request("session/prompt", params) {
            Ok(id) => id,
            Err(error) => {
                self.fail_transport(error.clone());
                return Err(error);
            }
        };
        self.turns.insert(
            input.turn_id,
            Turn {
                request_id,
                fingerprint,
                terminal: false,
                cancelling: None,
            },
        );
        self.active = Some(input.turn_id);
        self.queue(AgentEvent::TurnStarted {
            turn_id: input.turn_id,
        })?;
        for attachment in input.attachments {
            self.queue(AgentEvent::AttachmentAccepted {
                turn_id: input.turn_id,
                attachment_id: attachment.id,
            })?;
        }
        Ok(())
    }

    fn cancel(&mut self, turn_id: AgentTurnId) -> Result<(), ProviderFailure> {
        self.ensure_usable(ProviderStage::Cancel)?;
        match self.cancel_active(turn_id, false) {
            Ok(()) => Ok(()),
            Err(error) if error.code == ErrorCode::TurnNotFound => Err(error),
            Err(error) => {
                self.fail_transport(error.clone());
                Err(error)
            }
        }
    }

    fn answer_approval(
        &mut self,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        choice: Option<koru_session_protocol::ApprovalChoiceId>,
    ) -> Result<(), ProviderFailure> {
        self.ensure_usable(ProviderStage::Approval)?;
        let pending = self.approvals.get(&approval_id).cloned().ok_or_else(|| {
            failure(
                ErrorCode::ApprovalStale,
                ProviderStage::Approval,
                ProviderDiagnostic::InvalidState,
            )
        })?;
        let selected = match koru_session_protocol::resolve_approval_choice(
            &pending.choices,
            decision,
            choice,
        ) {
            Ok(selected) => selected,
            Err(koru_session_protocol::ApprovalChoiceError::Stale) => {
                return Err(failure(
                    ErrorCode::ApprovalStale,
                    ProviderStage::Approval,
                    ProviderDiagnostic::InvalidState,
                ));
            }
            Err(koru_session_protocol::ApprovalChoiceError::Unrepresentable) => {
                self.reply_permission(pending.request_id, None)?;
                self.approvals.remove(&approval_id);
                self.queue(AgentEvent::ApprovalExpired {
                    turn_id: pending.turn_id,
                    approval_id,
                })?;
                return self.cancel_active(pending.turn_id, true);
            }
        };
        let decision = selected.intent.decision();
        let choice = Some(selected.id);
        self.reply_permission(
            pending.request_id,
            Some(&pending.native_options[&selected.id]),
        )?;
        self.approvals.remove(&approval_id);
        self.queue(AgentEvent::ApprovalResolved {
            turn_id: pending.turn_id,
            approval_id,
            decision,
            choice,
        })
    }

    fn next_event(&mut self, timeout: Duration) -> Result<Option<AgentEvent>, ProviderFailure> {
        let deadline = Instant::now().checked_add(timeout);
        for _ in 0..self.config.event_capacity {
            self.expire_cancel();
            if self.unusable.is_some()
                && let Some(event) = self.events.pop_front()
            {
                return Ok(Some(event));
            }
            if let Some(event) = self.forced_terminal.take() {
                return Ok(Some(event));
            }
            if let Some(error) = &self.unusable {
                return Err(error.clone());
            }
            if let Some(event) = self.events.pop_front() {
                return Ok(Some(event));
            }
            if self.shut_down {
                return Err(invalid_state(ProviderStage::Read));
            }
            let mut remaining = deadline
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::MAX);
            if let Some(cancel) = self.active.and_then(|id| self.turns[&id].cancelling) {
                remaining =
                    remaining.min(cancel.deadline.saturating_duration_since(Instant::now()));
            }
            match self.transport()?.receive(remaining) {
                Ok(TransportReceive::Frame(frame)) => {
                    self.diagnostics.frames_read = self.diagnostics.frames_read.saturating_add(1);
                    if let Err(error) = self.handle_frame(frame) {
                        self.fail_transport(error);
                    }
                }
                Ok(TransportReceive::Woken) => {
                    self.expire_cancel();
                    if self.forced_terminal.is_none() {
                        return Ok(None);
                    }
                }
                Ok(TransportReceive::Timeout) => {
                    self.expire_cancel();
                    if self.forced_terminal.is_none() {
                        return Ok(None);
                    }
                }
                Err(error) => self.fail_transport(error),
            }
        }
        // Check even on the bounded-loop edge; a flood is never a timer.
        self.expire_cancel();
        if self.unusable.is_some()
            && let Some(event) = self.events.pop_front()
        {
            return Ok(Some(event));
        }
        if let Some(event) = self.forced_terminal.take() {
            return Ok(Some(event));
        }
        if let Some(error) = &self.unusable {
            return Err(error.clone());
        }
        Ok(self.events.pop_front())
    }

    fn diagnostics(&self) -> ProviderDiagnostics {
        self.diagnostics
    }

    fn shutdown(&mut self) -> Result<(), ProviderFailure> {
        if self.shut_down {
            return Ok(());
        }
        self.shut_down = true;
        let result = match &mut self.transport {
            // A server that never exits on EOF would only spend the whole
            // timeout and then be terminated anyway, reported as a failure.
            Some(transport) if !self.config.route.dialect.exits_on_eof() => {
                transport.terminate_tree()
            }
            Some(transport) => transport.shutdown(self.config.shutdown_timeout),
            None => Ok(()),
        };
        self.log.shutdown(&result, self.diagnostics);
        result
    }
}

impl Drop for AcpControl {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// A fresh folder under the system temp for one launch's own temp files,
/// unique by this process's id and a sequence. A folder already carrying the
/// name was left by a dead process that had this id before, so it is stale.
fn private_temp() -> Result<std::path::PathBuf, ProviderFailure> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = std::env::temp_dir();
    sweep_abandoned_temp(&temp);
    let directory = temp.join(format!("koru-acp-{}-{sequence}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir(&directory)
        .map_err(|error| ProviderFailure::io(ProviderStage::Spawn, &error))?;
    Ok(directory)
}

/// **The folders of runtimes that died without shutting down.** A shutdown
/// removes its own folder (`StdioJsonRpc::release_scratch`); a runtime that
/// was killed cannot, and each one leaves a bundle unpacked in it — about
/// 300 MB a launch, which filled a disk. They are removed here, at the next
/// launch, once the process a folder is named after no longer runs. A process
/// id reused by another program keeps its folder: the sweep may leave one
/// behind, never remove a live session's.
fn sweep_abandoned_temp(temp: &std::path::Path) {
    let own = std::process::id();
    let Ok(entries) = std::fs::read_dir(temp) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(owner) = entry.file_name().to_str().and_then(temp_owner) else {
            continue;
        };
        if owner == own || super::process::process_exists(owner) {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// The process a `koru-acp-<pid>-<sequence>` folder belongs to.
fn temp_owner(name: &str) -> Option<u32> {
    let (pid, sequence) = name.strip_prefix("koru-acp-")?.split_once('-')?;
    sequence.parse::<u64>().ok()?;
    pid.parse().ok()
}

fn failure(
    code: ErrorCode,
    stage: ProviderStage,
    diagnostic: ProviderDiagnostic,
) -> ProviderFailure {
    ProviderFailure::new(code, stage, diagnostic)
}
fn invalid_state(stage: ProviderStage) -> ProviderFailure {
    failure(
        ErrorCode::ProviderFailed,
        stage,
        ProviderDiagnostic::InvalidState,
    )
}
fn handshake_rejected() -> ProviderFailure {
    failure(
        ErrorCode::ProviderIncompatible,
        ProviderStage::Initialize,
        ProviderDiagnostic::HandshakeRejected,
    )
}
fn malformed() -> ProviderFailure {
    failure(
        ErrorCode::ProviderIncompatible,
        ProviderStage::Read,
        ProviderDiagnostic::MalformedFrame,
    )
}
fn decode<T: DeserializeOwned>(value: &Value) -> Result<T, ProviderFailure> {
    serde_json::from_value(value.clone()).map_err(|_| malformed())
}
fn valid_native_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn failed_approval_event(turn_id: AgentTurnId) -> AgentEvent {
    AgentEvent::TurnFailed {
        turn_id,
        error: SessionFailure {
            code: ErrorCode::ProviderFailed,
            message: koru_session_protocol::APPROVAL_UNREPRESENTABLE_MESSAGE.to_owned(),
        },
    }
}
fn failed_event(turn_id: AgentTurnId, code: ErrorCode) -> AgentEvent {
    AgentEvent::TurnFailed {
        turn_id,
        error: SessionFailure {
            message: code.as_str().to_owned(),
            code,
        },
    }
}

#[cfg(test)]
mod tests;
