// SPDX-License-Identifier: MPL-2.0

mod config;
mod events;
mod launch;
mod models;
mod normalize;
mod probe;

pub use config::CodexConfig;
pub use launch::{KORU_MCP_SERVER_NAME, mcp_server_overrides};
pub use probe::{CodexCompatibilityProbe, CodexFactory};

use std::collections::{BTreeMap, VecDeque};
use std::str::FromStr;
use std::time::{Duration, Instant};

use koru_session_protocol::{
    AgentEvent, AgentTurnId, ApprovalDecision, ApprovalId, AttachmentId, AttachmentKind,
    CachedInputRelationship, CapabilityName, CapabilitySet, ErrorCode, ModelIdentity, ModelName,
    ProviderKind, SessionId, ToolCallId, capability,
};
use serde_json::{Value, json};

use super::{
    AgentCapabilities, AgentControl, AgentInput, AgentStart, ChildSessionPolicy, DelegationFacts,
    EconomicsDeclaration, McpSupport, ProviderDiagnostic, ProviderDiagnostics, ProviderFailure,
    ProviderStage, ProviderVersion, ResumeSupport, UsageReporting,
};

use super::process::{JsonRpcTransport, StdioJsonRpc, TransportReceive};

pub const CODEX_PROVIDER: &str = koru_session_profiles::CODEX_REGISTRATION.kind;
pub const CODEX_PROTOCOL_VERSION: &str = "v2";
/// The reviewed allowlist. 0.156.1 (2026-09-25), 0.158.0 (2026-09-29) and
/// 0.163.0 (2026-10-27) were admitted on the maintainers' instruction with
/// schema evidence and a no-prompt handshake, and without a recorded turn —
/// see each version's `tests/fixtures/codex-app-server/<version>/README.md`.
pub const SUPPORTED_CODEX_VERSIONS: &[&str] = &[
    "0.150.1", "0.153.4", "0.154.0", "0.156.1", "0.158.0", "0.163.0", "0.165.0",
];

/// One Codex app-server process, one ephemeral Codex thread, and one Koru
/// managed session. Provider ids and JSON values never leave this adapter.
pub struct CodexControl {
    config: CodexConfig,
    transport: Option<Box<dyn JsonRpcTransport>>,
    capabilities: Option<AgentCapabilities>,
    session_id: Option<SessionId>,
    thread_id: Option<String>,
    next_request_id: u64,
    turns: BTreeMap<AgentTurnId, NativeTurn>,
    native_turns: BTreeMap<String, AgentTurnId>,
    tools: BTreeMap<(AgentTurnId, String), ToolCallId>,
    approvals: BTreeMap<ApprovalId, PendingApproval>,
    attachment_paths: BTreeMap<(AgentTurnId, String), AttachmentId>,
    events: VecDeque<AgentEvent>,
    diagnostics: ProviderDiagnostics,
    /// The thread's cumulative usage as last reported. An ephemeral thread
    /// starts at zero, which is what makes the first turn's baseline true.
    thread_usage: normalize::UsageTotals,
    /// Each turn's baseline: the thread total when the turn was submitted.
    /// A turn's usage is the thread total now minus this, so a missed
    /// intermediate update loses nothing (§17.2).
    usage_baselines: BTreeMap<AgentTurnId, normalize::UsageTotals>,
    /// The model the thread reported at start (0.153.4 names it on the
    /// `thread/start` result); `None` where the provider said nothing.
    thread_model: Option<ModelName>,
    /// Turns the provider rerouted to another model (`model/rerouted`).
    rerouted_models: BTreeMap<AgentTurnId, ModelName>,
    shut_down: bool,
}

#[derive(Clone)]
struct NativeTurn {
    native_id: Option<String>,
    fingerprint: blake3::Hash,
    terminal: bool,
}

#[derive(Clone)]
struct PendingApproval {
    request_id: RpcId,
    turn_id: AgentTurnId,
    kind: ApprovalKind,
    choices: Vec<koru_session_protocol::ApprovalChoice>,
}

#[derive(Clone)]
enum ApprovalKind {
    Command,
    FileChange,
    Permissions { requested: Value },
}

#[derive(Clone, PartialEq, Eq)]
enum RpcId {
    Number(u64),
    String(String),
}

impl RpcId {
    fn parse(value: &Value) -> Option<Self> {
        if let Some(value) = value.as_u64() {
            Some(Self::Number(value))
        } else {
            value.as_str().map(|value| Self::String(value.to_owned()))
        }
    }

    fn value(&self) -> Value {
        match self {
            Self::Number(value) => json!(value),
            Self::String(value) => json!(value),
        }
    }
}

impl CodexControl {
    pub fn new(config: CodexConfig) -> Self {
        Self::with_transport(config, None)
    }

    fn with_transport(config: CodexConfig, transport: Option<Box<dyn JsonRpcTransport>>) -> Self {
        Self {
            config,
            transport,
            capabilities: None,
            session_id: None,
            thread_id: None,
            next_request_id: 1,
            turns: BTreeMap::new(),
            native_turns: BTreeMap::new(),
            tools: BTreeMap::new(),
            approvals: BTreeMap::new(),
            attachment_paths: BTreeMap::new(),
            events: VecDeque::new(),
            diagnostics: ProviderDiagnostics::default(),
            thread_usage: normalize::UsageTotals::ZERO,
            usage_baselines: BTreeMap::new(),
            thread_model: None,
            rerouted_models: BTreeMap::new(),
            shut_down: false,
        }
    }

    /// Spawn the app-server if needed and complete its handshake.
    fn initialize(&mut self) -> Result<(), ProviderFailure> {
        self.ensure_transport()?;
        let initialized = self.call(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "koru_terminal",
                    "title": "Koru Terminal",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": { "experimentalApi": false },
            }),
            ProviderStage::Initialize,
        )?;
        validate_initialize(&initialized, &self.config.executable_version)?;
        self.notify("initialized", json!({}))?;
        Ok(())
    }

    fn ensure_transport(&mut self) -> Result<(), ProviderFailure> {
        if self.transport.is_none() {
            let argv = launch::render_argv_with(
                self.config.delegation.as_ref(),
                &self.config.mcp_servers,
                &self.config.executable_version,
            );
            let env = launch::child_env(self.config.delegation.as_ref());
            self.transport = Some(Box::new(StdioJsonRpc::spawn(
                &self.config.executable,
                &argv,
                &env,
                &self.config.working_directory,
                self.config.maximum_frame_bytes,
                self.config.wake.clone(),
            )?));
        }
        Ok(())
    }

    /// What this adapter declares about being delegated to, given the
    /// model the handshake selected. Every fact here is asserted by the
    /// offline replay in `tests.rs` and recorded in `docs/features/terminal/providers/codex.md`.
    fn delegation_facts(&self, requested_model: &str) -> DelegationFacts {
        DelegationFacts {
            starts_in_cwd: true,
            distinguishes_outcomes: true,
            follows_up: true,
            reports_tool_activity: true,
            // Threads are ephemeral, so nothing survives to resume; the
            // orchestrator re-submits (§7.6). `thread/resume` exists behind
            // a persisted thread and is a later decision, not a gap here.
            resume: ResumeSupport::Unsupported,
            mcp: match self.config.delegation {
                Some(_) => McpSupport::Supported,
                // Rendering is a property of the adapter, not of one launch;
                // a manual profile is simply not asked to.
                None => McpSupport::Supported,
            },
            child_sessions: ChildSessionPolicy::Disabled,
            economics: EconomicsDeclaration {
                usage: UsageReporting::RunningTotal,
                cached_input: CachedInputRelationship::Included,
                model: ModelIdentity {
                    requested: ModelName::from_str(requested_model).ok(),
                    reported: self.thread_model.clone(),
                },
            },
        }
    }

    fn transport(&mut self) -> Result<&mut (dyn JsonRpcTransport + '_), ProviderFailure> {
        match self.transport.as_mut() {
            Some(transport) => Ok(transport.as_mut()),
            None => Err(failure(
                ErrorCode::ProviderFailed,
                ProviderStage::Read,
                ProviderDiagnostic::InvalidState,
            )),
        }
    }

    fn call(
        &mut self,
        method: &str,
        params: Value,
        stage: ProviderStage,
    ) -> Result<Value, ProviderFailure> {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.checked_add(1).ok_or_else(|| {
            failure(
                ErrorCode::ProviderFailed,
                stage,
                ProviderDiagnostic::InvalidState,
            )
        })?;
        self.transport()?.send(&json!({
            "method": method,
            "id": id,
            "params": params,
        }))?;
        let deadline = Instant::now() + self.config.request_timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(failure(
                    ErrorCode::ProviderUnavailable,
                    stage,
                    ProviderDiagnostic::Timeout,
                ));
            }
            let frame = match self.transport()?.receive(remaining)? {
                TransportReceive::Frame(frame) => frame,
                TransportReceive::Woken => continue,
                TransportReceive::Timeout => {
                    return Err(failure(
                        ErrorCode::ProviderUnavailable,
                        stage,
                        ProviderDiagnostic::Timeout,
                    ));
                }
            };
            self.diagnostics.frames_read = self.diagnostics.frames_read.saturating_add(1);
            if frame.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(result) = frame.get("result") {
                    return Ok(result.clone());
                }
                return Err(failure(
                    if stage == ProviderStage::Initialize {
                        ErrorCode::ProviderIncompatible
                    } else {
                        ErrorCode::ProviderFailed
                    },
                    stage,
                    if stage == ProviderStage::Initialize {
                        ProviderDiagnostic::HandshakeRejected
                    } else {
                        ProviderDiagnostic::InvalidState
                    },
                ));
            }
            self.handle_frame(frame)?;
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), ProviderFailure> {
        self.transport()?
            .send(&json!({ "method": method, "params": params }))
    }
}

impl AgentControl for CodexControl {
    fn start(&mut self, start: AgentStart) -> Result<AgentCapabilities, ProviderFailure> {
        if let Some(capabilities) = &self.capabilities {
            return if self.session_id == Some(start.session_id) {
                Ok(capabilities.clone())
            } else {
                Err(failure(
                    ErrorCode::InvalidTransition,
                    ProviderStage::Initialize,
                    ProviderDiagnostic::InvalidState,
                ))
            };
        }
        if self.shut_down
            || !SUPPORTED_CODEX_VERSIONS.contains(&self.config.executable_version.as_str())
        {
            return Err(failure(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Version,
                ProviderDiagnostic::VersionRejected,
            ));
        }
        if start.resume.is_some() {
            // No native resume is declared, so no token was ever minted; one
            // arriving here is a caller ignoring the declaration.
            return Err(failure(
                ErrorCode::InvalidRequest,
                ProviderStage::Initialize,
                ProviderDiagnostic::InvalidInput,
            ));
        }
        self.initialize()?;
        let requested = self
            .config
            .selection
            .as_ref()
            .and_then(|selection| selection.model.clone());
        let model = self.select_model(requested.as_ref())?;
        let cwd = self.config.working_directory.to_str().ok_or_else(|| {
            failure(
                ErrorCode::InvalidRequest,
                ProviderStage::Initialize,
                ProviderDiagnostic::InvalidInput,
            )
        })?;
        let thread = self.call(
            "thread/start",
            json!({ "cwd": cwd, "ephemeral": true, "model": model.model }),
            ProviderStage::Initialize,
        )?;
        let thread_id = thread
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .filter(|value| valid_native_id(value))
            .ok_or_else(|| {
                failure(
                    ErrorCode::ProviderIncompatible,
                    ProviderStage::Initialize,
                    ProviderDiagnostic::HandshakeRejected,
                )
            })?
            .to_owned();
        // 0.153.4 names the model the thread runs on its result; 0.150.1 does
        // not, and an absent name is an honest `None`, never the request.
        self.thread_model = normalize::model_name(thread.pointer("/thread/model"));
        let mut names = vec![
            capability::MANAGED_EVENTS,
            capability::MANAGED_CANCEL,
            capability::MANAGED_APPROVAL,
            capability::DYNAMIC_APPROVALS,
            capability::MANAGED_USAGE,
        ];
        if model.image {
            names.push(capability::NATIVE_ATTACHMENT_DELIVERY);
        }
        let capabilities = AgentCapabilities {
            provider: provider_kind(),
            executable_version: self.config.executable_version.clone(),
            protocol_version: ProviderVersion::from_str(CODEX_PROTOCOL_VERSION)
                .expect("static protocol version is valid"),
            capabilities: CapabilitySet::new(names.into_iter().map(|name| {
                CapabilityName::from_str(name).expect("registered capability is valid")
            })),
            delegation: self.delegation_facts(&model.model),
        };
        self.session_id = Some(start.session_id);
        self.thread_id = Some(thread_id);
        self.capabilities = Some(capabilities.clone());
        self.queue(AgentEvent::ProviderState {
            provider: provider_kind(),
            state: koru_session_protocol::ManagedState::Idle,
        })?;
        Ok(capabilities)
    }

    fn capabilities(&self) -> Option<&AgentCapabilities> {
        self.capabilities.as_ref()
    }

    fn submit(&mut self, input: AgentInput) -> Result<(), ProviderFailure> {
        input.validate(self.config.maximum_input_bytes)?;
        let fingerprint = input_fingerprint(&input);
        if let Some(existing) = self.turns.get(&input.turn_id) {
            if existing.fingerprint != fingerprint {
                return Err(failure(
                    ErrorCode::IdempotencyConflict,
                    ProviderStage::Submit,
                    ProviderDiagnostic::InvalidInput,
                ));
            }
            return if existing.native_id.is_some() {
                Ok(())
            } else {
                Err(unknown_submit_outcome())
            };
        }
        let capabilities = self.capabilities.as_ref().ok_or_else(|| {
            failure(
                ErrorCode::ProviderFailed,
                ProviderStage::Submit,
                ProviderDiagnostic::InvalidState,
            )
        })?;
        let session_id = self.session_id.expect("started adapter has a session id");
        let mut provider_input = Vec::new();
        let mut resolved_attachments = Vec::new();
        if !input.text.is_empty() {
            provider_input.push(json!({ "type": "text", "text": input.text }));
        }
        for attachment in &input.attachments {
            if attachment.kind != AttachmentKind::Image
                || !attachment.media_type.starts_with("image/")
                || !capabilities
                    .capabilities
                    .supports(capability::NATIVE_ATTACHMENT_DELIVERY)
            {
                return Err(failure(
                    ErrorCode::UnsupportedMedia,
                    ProviderStage::Submit,
                    ProviderDiagnostic::InvalidInput,
                ));
            }
            let path = self
                .config
                .attachment_paths
                .resolve(session_id, attachment.id)
                .map_err(|code| {
                    failure(
                        code,
                        ProviderStage::Submit,
                        ProviderDiagnostic::InvalidInput,
                    )
                })?;
            if !path.is_absolute() {
                return Err(failure(
                    ErrorCode::DeliveryFailed,
                    ProviderStage::Submit,
                    ProviderDiagnostic::InvalidInput,
                ));
            }
            let path = path.to_str().ok_or_else(|| {
                failure(
                    ErrorCode::DeliveryFailed,
                    ProviderStage::Submit,
                    ProviderDiagnostic::InvalidInput,
                )
            })?;
            if resolved_attachments
                .iter()
                .any(|(resolved, _)| resolved == path)
            {
                return Err(failure(
                    ErrorCode::DeliveryFailed,
                    ProviderStage::Submit,
                    ProviderDiagnostic::InvalidInput,
                ));
            }
            resolved_attachments.push((path.to_owned(), attachment.id));
            provider_input.push(json!({ "type": "localImage", "path": path, "detail": "low" }));
        }
        let thread_id = self.thread_id.clone().ok_or_else(|| {
            failure(
                ErrorCode::ProviderFailed,
                ProviderStage::Submit,
                ProviderDiagnostic::InvalidState,
            )
        })?;
        self.turns.insert(
            input.turn_id,
            NativeTurn {
                native_id: None,
                fingerprint,
                terminal: false,
            },
        );
        // The turn's usage is measured from the thread total as it stands
        // now; a missed update in between costs nothing.
        self.usage_baselines
            .insert(input.turn_id, self.thread_usage);
        self.attachment_paths.extend(
            resolved_attachments
                .into_iter()
                .map(|(path, attachment_id)| ((input.turn_id, path), attachment_id)),
        );
        let response = self.call(
            "turn/start",
            json!({ "threadId": thread_id, "input": provider_input }),
            ProviderStage::Submit,
        )?;
        let native_id = response
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .filter(|value| valid_native_id(value))
            .ok_or_else(malformed_submit)?
            .to_owned();
        self.native_turns.insert(native_id.clone(), input.turn_id);
        self.turns
            .get_mut(&input.turn_id)
            .expect("pending turn exists after the provider call")
            .native_id = Some(native_id);
        self.queue(AgentEvent::TurnStarted {
            turn_id: input.turn_id,
        })?;
        self.queue(AgentEvent::ProviderState {
            provider: provider_kind(),
            state: koru_session_protocol::ManagedState::Running,
        })
    }

    fn steer(&mut self, input: AgentInput) -> Result<(), ProviderFailure> {
        input.validate(self.config.maximum_input_bytes)?;
        let invalid_state = || {
            failure(
                ErrorCode::InvalidTransition,
                ProviderStage::Submit,
                ProviderDiagnostic::InvalidState,
            )
        };
        if !input.attachments.is_empty() {
            return Err(failure(
                ErrorCode::DeliveryUnsupported,
                ProviderStage::Submit,
                ProviderDiagnostic::InvalidInput,
            ));
        }
        // Only a turn this adapter started and has not seen finish can be
        // steered; the provider also checks the id it is given.
        let native_id = self
            .turns
            .get(&input.turn_id)
            .filter(|turn| !turn.terminal)
            .and_then(|turn| turn.native_id.clone())
            .ok_or_else(invalid_state)?;
        let thread_id = self.thread_id.clone().ok_or_else(invalid_state)?;
        self.call(
            "turn/steer",
            json!({
                "threadId": thread_id,
                "expectedTurnId": native_id,
                "input": [{ "type": "text", "text": input.text }],
            }),
            ProviderStage::Submit,
        )?;
        Ok(())
    }

    fn cancel(&mut self, turn_id: AgentTurnId) -> Result<(), ProviderFailure> {
        let turn = self.turns.get(&turn_id).cloned().ok_or_else(|| {
            failure(
                ErrorCode::TurnNotFound,
                ProviderStage::Cancel,
                ProviderDiagnostic::InvalidState,
            )
        })?;
        if turn.terminal {
            return Ok(());
        }
        let native_id = turn.native_id.ok_or_else(unknown_submit_outcome)?;
        let thread_id = self.thread_id.clone().ok_or_else(|| {
            failure(
                ErrorCode::ProviderFailed,
                ProviderStage::Cancel,
                ProviderDiagnostic::InvalidState,
            )
        })?;
        match self.call(
            "turn/interrupt",
            json!({ "threadId": thread_id, "turnId": native_id }),
            ProviderStage::Cancel,
        ) {
            Ok(_) => Ok(()),
            Err(_) if self.turns.get(&turn_id).is_some_and(|turn| turn.terminal) => Ok(()),
            Err(failure) => Err(failure),
        }
    }

    fn answer_approval(
        &mut self,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        choice: Option<koru_session_protocol::ApprovalChoiceId>,
    ) -> Result<(), ProviderFailure> {
        let pending = self.approvals.get(&approval_id).cloned().ok_or_else(|| {
            failure(
                ErrorCode::ApprovalStale,
                ProviderStage::Approval,
                ProviderDiagnostic::InvalidState,
            )
        })?;
        let selected =
            koru_session_protocol::resolve_approval_choice(&pending.choices, decision, choice)
                .map_err(|_| {
                    failure(
                        ErrorCode::ApprovalStale,
                        ProviderStage::Approval,
                        ProviderDiagnostic::InvalidState,
                    )
                })?;
        let decision = selected.intent.decision();
        let choice = Some(selected.id);
        let result = match (&pending.kind, decision) {
            (ApprovalKind::Command | ApprovalKind::FileChange, ApprovalDecision::Approve) => {
                json!({ "decision": "accept" })
            }
            (ApprovalKind::Command | ApprovalKind::FileChange, ApprovalDecision::Reject) => {
                json!({ "decision": "decline" })
            }
            (ApprovalKind::Permissions { requested }, ApprovalDecision::Approve) => {
                json!({ "permissions": requested, "scope": "turn" })
            }
            (ApprovalKind::Permissions { .. }, ApprovalDecision::Reject) => {
                json!({ "permissions": {}, "scope": "turn" })
            }
        };
        self.transport()?.send(&json!({
            "id": pending.request_id.value(),
            "result": result,
        }))?;
        self.approvals.remove(&approval_id);
        self.queue(AgentEvent::ApprovalResolved {
            turn_id: pending.turn_id,
            approval_id,
            decision,
            choice,
        })?;
        if self.approvals.is_empty() {
            self.queue(AgentEvent::ProviderState {
                provider: provider_kind(),
                state: koru_session_protocol::ManagedState::Running,
            })?;
        }
        Ok(())
    }

    fn next_event(&mut self, timeout: Duration) -> Result<Option<AgentEvent>, ProviderFailure> {
        if let Some(event) = self.events.pop_front() {
            return Ok(Some(event));
        }
        let deadline = Instant::now().checked_add(timeout);
        for _ in 0..self.config.event_capacity {
            let remaining = deadline
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::MAX);
            let frame = match self.transport()?.receive(remaining)? {
                TransportReceive::Frame(frame) => frame,
                TransportReceive::Woken | TransportReceive::Timeout => return Ok(None),
            };
            self.diagnostics.frames_read = self.diagnostics.frames_read.saturating_add(1);
            self.handle_frame(frame)?;
            if let Some(event) = self.events.pop_front() {
                return Ok(Some(event));
            }
        }
        Ok(None)
    }

    fn diagnostics(&self) -> ProviderDiagnostics {
        self.diagnostics
    }

    fn shutdown(&mut self) -> Result<(), ProviderFailure> {
        if self.shut_down {
            return Ok(());
        }
        self.shut_down = true;
        let timeout = self.config.shutdown_timeout;
        self.transport()?.shutdown(timeout)
    }
}

impl Drop for CodexControl {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

pub(super) fn provider_kind() -> ProviderKind {
    ProviderKind::from_str(CODEX_PROVIDER).expect("static provider is valid")
}

pub(super) fn valid_native_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn validate_initialize(
    initialized: &Value,
    executable_version: &ProviderVersion,
) -> Result<(), ProviderFailure> {
    let user_agent = initialized
        .get("userAgent")
        .and_then(Value::as_str)
        .ok_or_else(handshake_rejected)?;
    let expected = format!("koru_terminal/{}", executable_version.as_str());
    let user_agent_matches = user_agent == expected
        || user_agent
            .strip_prefix(&expected)
            .is_some_and(|suffix| suffix.starts_with(" ("));
    let codex_home = initialized.get("codexHome").and_then(Value::as_str);
    let platform_family = initialized.get("platformFamily").and_then(Value::as_str);
    let platform_os = initialized.get("platformOs").and_then(Value::as_str);
    if !user_agent_matches
        || !codex_home.is_some_and(|value| valid_bounded_value(value, 32 * 1024))
        || !platform_family.is_some_and(|value| valid_bounded_value(value, 64))
        || !platform_os.is_some_and(|value| valid_bounded_value(value, 64))
    {
        return Err(handshake_rejected());
    }
    Ok(())
}

fn valid_bounded_value(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= maximum_bytes && !value.chars().any(char::is_control)
}

fn handshake_rejected() -> ProviderFailure {
    failure(
        ErrorCode::ProviderIncompatible,
        ProviderStage::Initialize,
        ProviderDiagnostic::HandshakeRejected,
    )
}

pub(super) fn approval_summary(method: &str) -> &'static str {
    match method {
        "item/commandExecution/requestApproval" => "command_execution",
        "item/fileChange/requestApproval" => "file_change",
        "item/permissions/requestApproval" => "permissions",
        _ => "provider_action",
    }
}

fn input_fingerprint(input: &AgentInput) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"koru-codex-input-v1\0");
    hasher.update(&(input.text.len() as u64).to_le_bytes());
    hasher.update(input.text.as_bytes());
    for attachment in &input.attachments {
        hasher.update(attachment.id.as_uuid().as_bytes());
        hasher.update(&[match attachment.kind {
            AttachmentKind::Image => 1,
            AttachmentKind::File => 2,
        }]);
        hasher.update(&(attachment.media_type.len() as u64).to_le_bytes());
        hasher.update(attachment.media_type.as_bytes());
    }
    hasher.finalize()
}

fn unknown_submit_outcome() -> ProviderFailure {
    failure(
        ErrorCode::ProviderUnavailable,
        ProviderStage::Submit,
        ProviderDiagnostic::Timeout,
    )
}

pub(super) fn malformed_read() -> ProviderFailure {
    failure(
        ErrorCode::ProviderIncompatible,
        ProviderStage::Read,
        ProviderDiagnostic::MalformedFrame,
    )
}

fn malformed_submit() -> ProviderFailure {
    failure(
        ErrorCode::ProviderIncompatible,
        ProviderStage::Submit,
        ProviderDiagnostic::MalformedFrame,
    )
}

pub(super) fn failure(
    code: ErrorCode,
    stage: ProviderStage,
    diagnostic: ProviderDiagnostic,
) -> ProviderFailure {
    ProviderFailure::new(code, stage, diagnostic)
}

#[cfg(test)]
mod tests;
