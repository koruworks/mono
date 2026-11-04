// SPDX-License-Identifier: MPL-2.0

mod antigravity;
mod claude;
mod config;
mod diagnostics;
mod elicitation;
mod events;
mod grok;
mod input;
mod models;
mod modes;
mod probe;
mod routes;
mod steering;

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
    /// The agent advertised the steering extension at initialize
    /// (`_meta.steering.supported`), so text can go into a running turn
    /// ([`steering`]).
    steering: bool,
    wrapper_auth: Option<claude::WrapperAuth>,
    grok_inventory: Option<String>,
    /// What the session advertised under its `model` config option, for
    /// discovery; empty where the route advertises none.
    model_options: Vec<koru_session_protocol::ProviderModel>,
    /// The approval modes the session advertised, with the one it is in
    /// (koru-ai D7); `None` where it advertised none.
    modes: Option<koru_session_protocol::AgentModes>,
    /// The `mode` config option's id, when the modes came with one.
    mode_config: Option<String>,
    /// A mode the agent moved to between turns, said with the next turn
    /// (`ModeChanged` belongs to a turn).
    unreported_mode: Option<koru_session_protocol::AgentModeId>,
    /// The session's model's Fast mode option, as the latest answer or
    /// update carried it; `None` for a model without one (D11).
    fast: Option<models::FastOption>,
    /// The session's `thought_level` option as the latest answer or update
    /// carried it — its id, its values and the level it is at — so a level
    /// chosen after the start can be set (koru-ai D70); `None` for a model
    /// without one.
    thought_level: Option<models::ThoughtLevel>,
    /// Notices the agent said while no turn ran, said with the next turn
    /// (koru-ai D69; `Notice` belongs to a turn).
    unsaid_notices: Vec<String>,
    next_request_id: u64,
    turns: BTreeMap<AgentTurnId, Turn>,
    active: Option<AgentTurnId>,
    tools: BTreeMap<String, Tool>,
    /// The native ids of calls whose start is held back, oldest first.
    unstarted: Vec<String>,
    approvals: BTreeMap<ApprovalId, PendingApproval>,
    /// Forms the agent asked (`elicitation/create`) and nobody has
    /// answered: each one a question set (D108).
    elicitations:
        BTreeMap<koru_session_protocol::QuestionRequestId, elicitation::PendingElicitation>,
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
    /// Whether an update has said what the call does. A call that arrives
    /// with nothing to say it — Claude announces a tool the moment the model
    /// starts writing its input, as *Terminal* with `rawInput: {}` — is held
    /// in `unstarted` until one does; a ready call starts once every call
    /// announced before it is ready too (`AcpControl::flush_starts`).
    ready: bool,
    /// The agent's latest title for the call, bounded.
    title: Option<String>,
    /// The latest of what the call was asked to do (`rawInput`), bounded.
    input: Option<String>,
    /// What the tool produced, as its updates carried it (UC17): the latest
    /// diff blocks, the latest result text, and the latest `rawOutput`. The
    /// completion takes the diff and the text, else the raw output.
    diff: Option<String>,
    text: Option<String>,
    raw_output: Option<String>,
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
            steering: false,
            wrapper_auth: None,
            grok_inventory: None,
            model_options: Vec::new(),
            modes: None,
            mode_config: None,
            unreported_mode: None,
            fast: None,
            thought_level: None,
            unsaid_notices: Vec::new(),
            next_request_id: 1,
            turns: BTreeMap::new(),
            active: None,
            tools: BTreeMap::new(),
            unstarted: Vec::new(),
            approvals: BTreeMap::new(),
            elicitations: BTreeMap::new(),
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

    /// The approval modes the session advertised, in the mode it is in now;
    /// `None` before start and where it advertised none (D7).
    pub(super) fn modes(&self) -> Option<&koru_session_protocol::AgentModes> {
        self.modes.as_ref()
    }

    /// Follow the mode an answer or an update reports: the session's modes
    /// with that one current. The new mode when it moved, `None` when it is
    /// the same, is not one of the session's, or the answer names none.
    fn follow_mode(
        &mut self,
        reported: Option<koru_session_protocol::AgentModeId>,
    ) -> Option<koru_session_protocol::AgentModeId> {
        let reported = reported?;
        let modes = self.modes.as_ref()?;
        if modes.current() == Some(&reported) {
            return None;
        }
        self.modes = Some(modes.with_current(&reported)?);
        Some(reported)
    }

    /// Say a mode the agent moved to by itself: on the running turn, or
    /// with the next one when none runs (D7).
    pub(super) fn report_mode(
        &mut self,
        moved: Option<koru_session_protocol::AgentModeId>,
    ) -> Result<(), ProviderFailure> {
        let Some(mode) = moved else {
            return Ok(());
        };
        match self.active {
            Some(turn_id) if self.turns[&turn_id].cancelling.is_none() => {
                self.unreported_mode = None;
                self.queue(AgentEvent::ModeChanged { turn_id, mode })
            }
            Some(_) => Ok(()),
            None => {
                self.unreported_mode = Some(mode);
                Ok(())
            }
        }
    }

    /// **Turn the model's Fast mode on or off** (koru-ai D11), between turns:
    /// through its `fast` option, verified from the answer like a level. A
    /// model with no Fast mode ignores the choice — it is offered only where
    /// the model has one. Claude's wrapper takes it live, so it may change
    /// with any message.
    fn apply_fast(
        &mut self,
        native_session: &str,
        on: bool,
        stage: ProviderStage,
    ) -> Result<(), ProviderFailure> {
        let rejected = || {
            tracing::warn!(target: "koru::acp", event = "acp.fast.rejected", on);
            failure(
                ErrorCode::InvalidRequest,
                stage,
                ProviderDiagnostic::InvalidInput,
            )
        };
        let Some(option) = &self.fast else {
            if on {
                tracing::info!(target: "koru::acp", event = "acp.fast.not_offered");
            }
            return Ok(());
        };
        if option.on == on {
            return Ok(());
        }
        let params =
            json!({"sessionId":native_session, "configId":option.id, "value":option.value(on)});
        let applied = self
            .call("session/set_config_option", params)
            .map_err(|_| rejected())?;
        let reported = models::fast_option(&applied).ok_or_else(rejected)?;
        if reported.on != on {
            return Err(rejected());
        }
        self.fast = Some(reported);
        tracing::info!(target: "koru::acp", event = "acp.fast.selected", on);
        Ok(())
    }

    /// **Move the session to `mode`** (D7), between turns. The mode must be
    /// one the session offers; it is set through the `mode` config option
    /// when the agent advertised one — the answer reports where the session
    /// is — else through `session/set_mode`, whose answer is empty. An agent
    /// may land on another of its modes than the one asked (Claude, asked
    /// for `auto` on a model without it, takes `acceptEdits`): the session
    /// follows it, and the next turn says so. A refused set, or one whose
    /// answer names no mode it offers, refuses the start or the turn —
    /// never runs it in a mode nobody chose.
    fn apply_mode(
        &mut self,
        native_session: &str,
        mode: &koru_session_protocol::AgentModeId,
        stage: ProviderStage,
    ) -> Result<(), ProviderFailure> {
        let rejected = || {
            tracing::warn!(target: "koru::acp", event = "acp.mode.rejected", mode = %mode);
            failure(
                ErrorCode::InvalidRequest,
                stage,
                ProviderDiagnostic::InvalidInput,
            )
        };
        let Some(modes) = &self.modes else {
            return Err(rejected());
        };
        if !modes.contains(mode) {
            return Err(rejected());
        }
        if modes.current() == Some(mode) {
            return Ok(());
        }
        let reported = match self.mode_config.clone() {
            Some(config_id) => {
                let applied = self
                    .call(
                        "session/set_config_option",
                        json!({"sessionId":native_session, "configId":config_id, "value":mode.as_str()}),
                    )
                    .map_err(|_| rejected())?;
                modes::current(&applied).ok_or_else(rejected)?
            }
            None => {
                self.call(
                    "session/set_mode",
                    json!({"sessionId":native_session, "modeId":mode.as_str()}),
                )
                .map_err(|_| rejected())?;
                mode.clone()
            }
        };
        if !self
            .modes
            .as_ref()
            .is_some_and(|modes| modes.contains(&reported))
        {
            return Err(rejected());
        }
        self.follow_mode(Some(reported.clone()));
        // What the agent said while setting it is the answer itself: only
        // a landing elsewhere than asked is news for the next turn.
        self.unreported_mode = (reported != *mode).then_some(reported.clone());
        tracing::info!(target: "koru::acp", event = "acp.mode.selected", mode = %reported, asked = %mode);
        Ok(())
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
                self.config.maximum_inbound_frame_bytes,
                self.config.maximum_prompt_frame_bytes,
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

    /// Queue `event`, after every call start still held back: nothing that
    /// follows a call in the stream may reach the chat before it, or the
    /// call's place in the reply's text and its order among the rows move.
    fn queue(&mut self, event: AgentEvent) -> Result<(), ProviderFailure> {
        self.flush_starts(usize::MAX)?;
        self.push_event(event)
    }

    /// Queue the held-back starts: all of them up to `upto` of the oldest,
    /// each with the title and input its updates have brought so far.
    fn flush_starts(&mut self, upto: usize) -> Result<(), ProviderFailure> {
        let count = upto.min(self.unstarted.len());
        for native_id in self.unstarted.drain(..count).collect::<Vec<_>>() {
            let Some(turn_id) = self.active else {
                break;
            };
            let Some(tool) = self.tools.get(&native_id) else {
                continue;
            };
            let event = AgentEvent::ToolStarted {
                turn_id,
                tool_call_id: tool.id,
                label: tool
                    .title
                    .clone()
                    .unwrap_or_else(|| tool.kind.as_str().to_owned()),
                input: tool.input.clone(),
            };
            self.push_event(event)?;
        }
        Ok(())
    }

    fn clear_tools(&mut self) {
        self.tools.clear();
        self.unstarted.clear();
    }

    fn push_event(&mut self, event: AgentEvent) -> Result<(), ProviderFailure> {
        if self.events.len() + self.approvals.len() + self.elicitations.len()
            >= self.config.event_capacity
        {
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
        self.expire_elicitations(turn_id)?;
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
        self.clear_tools();
        self.approvals.clear();
        self.elicitations.clear();
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
        self.clear_tools();
        for (approval_id, pending) in std::mem::take(&mut self.approvals) {
            let event = AgentEvent::ApprovalExpired {
                turn_id: pending.turn_id,
                approval_id,
            };
            self.log.observe(&event, self.diagnostics);
            self.events.push_back(event);
        }
        for (request_id, pending) in std::mem::take(&mut self.elicitations) {
            let event = AgentEvent::QuestionsResolved {
                turn_id: pending.turn_id,
                request_id,
                outcome: koru_session_protocol::QuestionsOutcome::Expired,
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
        self.expire_elicitations(turn_id)?;
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
        self.clear_tools();
        Ok(())
    }

    /// **Start the session at the chosen reasoning level** (agent-models
    /// D18): the `thought_level` option of the latest answer must offer it,
    /// `session/set_config_option` sets it, and the answer must report it
    /// applied — the same validate, set and verify the model gets. Anything
    /// short of that fails the start as a refused level (D19), never runs
    /// the agent at a level the person did not choose.
    fn apply_reasoning(
        &mut self,
        native_session: &str,
        options: &Value,
        level: koru_session_protocol::LaunchReasoning,
        session_id: SessionId,
    ) -> Result<(), ProviderFailure> {
        let rejected = || {
            tracing::warn!(target: "koru::acp", event = "acp.reasoning.rejected", session_id = %session_id);
            failure(
                ErrorCode::ProviderModelUnavailable,
                ProviderStage::Initialize,
                ProviderDiagnostic::ReasoningRejected,
            )
        };
        let Some(option) = models::thought_level(options) else {
            return Err(rejected());
        };
        let Some(value) = option.value_for(level).map(str::to_owned) else {
            return Err(rejected());
        };
        let applied = self
            .call(
                "session/set_config_option",
                json!({"sessionId":native_session, "configId":option.id, "value":value}),
            )
            .map_err(|_| rejected())?;
        let reported = models::thought_level(&applied);
        if reported
            .as_ref()
            .and_then(|option| option.current.as_deref())
            != Some(value.as_str())
        {
            return Err(rejected());
        }
        self.thought_level = reported;
        tracing::info!(target: "koru::acp", event = "acp.reasoning.selected", session_id = %session_id, applied = true);
        Ok(())
    }

    /// **Move the session to the turn's level** (koru-ai D70), between
    /// turns: through the same `thought_level` option the start sets,
    /// verified from the answer the same way, and only when the session is
    /// elsewhere. A session whose model has no such option, a level it does
    /// not offer, or a set it refuses refuses the turn as a refused level
    /// (D19) — never runs it at a level nobody chose.
    fn apply_turn_reasoning(
        &mut self,
        native_session: &str,
        level: koru_session_protocol::LaunchReasoning,
    ) -> Result<(), ProviderFailure> {
        let rejected = || {
            tracing::warn!(target: "koru::acp", event = "acp.reasoning.rejected", level = level.as_str(), stage = "submit");
            failure(
                ErrorCode::ProviderModelUnavailable,
                ProviderStage::Submit,
                ProviderDiagnostic::ReasoningRejected,
            )
        };
        let Some(option) = &self.thought_level else {
            return Err(rejected());
        };
        let Some(value) = option.value_for(level).map(str::to_owned) else {
            return Err(rejected());
        };
        if option.current.as_deref() == Some(value.as_str()) {
            return Ok(());
        }
        let id = option.id.clone();
        let applied = self
            .call(
                "session/set_config_option",
                json!({"sessionId":native_session, "configId":id, "value":value}),
            )
            .map_err(|_| rejected())?;
        let reported = models::thought_level(&applied);
        if reported
            .as_ref()
            .and_then(|option| option.current.as_deref())
            != Some(value.as_str())
        {
            return Err(rejected());
        }
        self.thought_level = reported;
        tracing::info!(target: "koru::acp", event = "acp.reasoning.selected", level = level.as_str(), stage = "submit");
        Ok(())
    }

    /// **Each model's levels, read by walking the models once** (D16): the
    /// discovery session's own model is set to each listed one in turn, and
    /// the `thought_level` option its answer carries is that model's. No
    /// prompt is sent, and only this session's model changes; a set that
    /// fails ends the walk, and the models already read keep their levels.
    pub(super) fn walk_levels(&mut self) -> Vec<koru_session_protocol::ProviderModel> {
        let mut models = self.model_options.clone();
        let Some(session) = self.native_session.clone() else {
            return models;
        };
        let mut dropped = 0;
        for model in &mut models {
            let Ok(answer) = self.call(
                "session/set_config_option",
                json!({"sessionId":session, "configId":"model", "value":model.id.as_str()}),
            ) else {
                break;
            };
            let level = models::thought_level(&answer);
            dropped += level.as_ref().map_or(0, |level| level.dropped);
            model.reasoning = level.and_then(|level| level.reasoning);
            // The answer that set the model says whether it has Fast mode.
            model.fast = models::model_fast(&answer);
        }
        models::levels_dropped(self.config.route.provider.as_str(), dropped);
        models
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
                "clientCapabilities":client_capabilities(self.config.questions)
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
        self.steering = steering::advertised(&initialized);
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
        if let Some(read) = modes::read(&session) {
            self.modes = Some(read.modes);
            self.mode_config = read.config_id;
        }
        self.fast = models::fast_option(&session);
        if self.config.route.is_claude() {
            self.model_options =
                claude::fold_context_siblings(std::mem::take(&mut self.model_options));
        }
        let requested_model = self.config.requested_model();
        // The latest answer's options: the model's levels arrive with the
        // answer that set the model (D18).
        let mut latest = None;
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
            // A model may not offer a mode the session was in (Claude's
            // `auto`), and the agent moves it; the answer says where. Its
            // Fast mode is the new model's (D11).
            self.follow_mode(modes::current(&selected));
            self.fast = models::fast_option(&selected);
            latest = Some(selected);
        }
        // The session's level option, as the model now in it has it (D70).
        self.thought_level = models::thought_level(latest.as_ref().unwrap_or(&session));
        if let Some(level) = self.config.requested_reasoning() {
            let options = latest.as_ref().unwrap_or(&session);
            self.apply_reasoning(&native_session, options, level, start.session_id)?;
        }
        if let Some(mode) = self.config.requested_mode() {
            self.apply_mode(&native_session, &mode, ProviderStage::Initialize)?;
        }
        if let Some(on) = self.config.requested_fast() {
            self.apply_fast(&native_session, on, ProviderStage::Initialize)?;
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
        // Reserve the start, the notices said before it and the attachment
        // acknowledgements before accepting.
        if self.events.len() + 1 + self.unsaid_notices.len() + input.attachments.len()
            > self.config.event_capacity
        {
            return Err(failure(
                ErrorCode::Backpressure,
                ProviderStage::Submit,
                ProviderDiagnostic::Backpressure,
            ));
        }
        let prompt = self.prompt(&input)?;
        // The turn's mode is set before its prompt, between turns (D7).
        if let (Some(mode), Some(native_session)) = (&input.mode, self.native_session.clone()) {
            self.apply_mode(&native_session, mode, ProviderStage::Submit)?;
        }
        if let (Some(on), Some(native_session)) = (input.fast, self.native_session.clone()) {
            self.apply_fast(&native_session, on, ProviderStage::Submit)?;
        }
        // A level chosen after the start (D70), likewise.
        if let (Some(level), Some(native_session)) = (input.reasoning, self.native_session.clone())
        {
            self.apply_turn_reasoning(&native_session, level)?;
        }
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
        if let Some(mode) = self.unreported_mode.take() {
            self.queue(AgentEvent::ModeChanged {
                turn_id: input.turn_id,
                mode,
            })?;
        }
        // What the agent said before the turn is said first in it (D69).
        for text in std::mem::take(&mut self.unsaid_notices) {
            self.queue(AgentEvent::Notice {
                turn_id: input.turn_id,
                text,
            })?;
        }
        for attachment in input.attachments {
            self.queue(AgentEvent::AttachmentAccepted {
                turn_id: input.turn_id,
                attachment_id: attachment.id,
            })?;
        }
        Ok(())
    }

    fn steer(&mut self, input: AgentInput) -> Result<(), ProviderFailure> {
        self.steer_turn(input)
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

    fn answer_questions(
        &mut self,
        request_id: koru_session_protocol::QuestionRequestId,
        answers: Option<Vec<koru_session_protocol::QuestionAnswer>>,
    ) -> Result<(), ProviderFailure> {
        self.answer_elicitation(request_id, answers)
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

/// **What Koru tells the agent it can do** at `initialize`: form
/// elicitation — an agent's questions — only for a session whose creator
/// draws them (D108). Nothing else is advertised.
fn client_capabilities(questions: bool) -> Value {
    if questions {
        json!({"elicitation":{"form":{}}})
    } else {
        json!({})
    }
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
