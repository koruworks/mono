// SPDX-License-Identifier: MPL-2.0

//! Deterministic ACP peer on Koru's real transport seam. No process or I/O.
//! Raw payloads remain private and Debug reports only counters.
use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol_schema::v1 as acp;
use koru_session_protocol::ErrorCode;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::process::{JsonRpcTransport, TransportReceive};
use super::{JsonLineReader, ProviderDiagnostic, ProviderFailure, ProviderStage, write_json_line};

pub(super) const SESSION: &str = "<session>";
pub(super) const PERMISSION_ID: &str = "<permission-request>";

/// Configuration intentionally has no Debug: option names are display content.
#[derive(Default)]
pub(super) struct FakeAcpConfig {
    pub fail_at: Option<ProviderStage>,
    pub permission_options: Option<Vec<acp::PermissionOption>>,
    pub ignore_cancel: bool,
    /// Generated lazily, so an infinite flood takes constant fixture memory.
    pub flood_after_cancel: bool,
    pub exit_after_prompt: bool,
    pub stop_reason: Option<acp::StopReason>,
    /// The session's `configOptions`, answered by `session/new` and kept:
    /// `session/set_config_option` sets an option's `currentValue` and
    /// answers with them all.
    pub config_options: Vec<Value>,
    /// A model's own `thought_level` option, put in place when the model is
    /// set; a model not named here has none.
    pub thought_by_model: Vec<(String, Value)>,
    /// The level is taken but not applied: `currentValue` stays as it was.
    pub ignore_level: bool,
}

#[derive(Default)]
pub(super) struct FakeAcpState {
    sent: Vec<Value>,
    pub shutdowns: usize,
    /// Shutdowns that ended the tree without waiting for an EOF exit.
    pub terminations: usize,
    pub receives: usize,
    pub prompts: usize,
    pub cancels: usize,
    pub permission_pending: bool,
}

impl FakeAcpState {
    /// Explicit private-wire inspection for adapter assertions; never Debug.
    pub fn sent(&self) -> &[Value] {
        &self.sent
    }
}

impl fmt::Debug for FakeAcpState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeAcpState")
            .field("sent_count", &self.sent.len())
            .field("shutdowns", &self.shutdowns)
            .field("terminations", &self.terminations)
            .field("receives", &self.receives)
            .field("prompts", &self.prompts)
            .field("cancels", &self.cancels)
            .field("permission_pending", &self.permission_pending)
            .finish()
    }
}

pub(super) struct FakeAcpPeer {
    config: FakeAcpConfig,
    state: Arc<Mutex<FakeAcpState>>,
    inbound: VecDeque<Value>,
    prompt_id: Option<Value>,
    cancelled: bool,
    exited: bool,
}

impl FakeAcpPeer {
    pub fn new(config: FakeAcpConfig) -> (Self, Arc<Mutex<FakeAcpState>>) {
        let state = Arc::new(Mutex::new(FakeAcpState::default()));
        (
            Self {
                config,
                state: Arc::clone(&state),
                inbound: VecDeque::new(),
                prompt_id: None,
                cancelled: false,
                exited: false,
            },
            state,
        )
    }

    /// Discovery/version/spawn occur before a transport exists. Their fakes
    /// call this same fault seam rather than launching a real CLI.
    pub fn check_stage(&self, stage: ProviderStage) -> Result<(), ProviderFailure> {
        if self.config.fail_at == Some(stage) {
            Err(failure(stage, ProviderDiagnostic::Io))
        } else {
            Ok(())
        }
    }

    /// Adds scripted extension traffic. Standard messages are type-checked;
    /// unknown methods deliberately remain possible for forward-compat tests.
    pub fn queue(&mut self, frame: Value) -> Result<(), ProviderFailure> {
        validate_server(&frame)?;
        self.inbound.push_back(roundtrip(&frame)?);
        Ok(())
    }

    pub fn exit(&mut self) {
        self.exited = true;
    }

    fn reply(&mut self, id: Value, result: Value) -> Result<(), ProviderFailure> {
        self.queue(json!({"jsonrpc":"2.0", "id":id, "result":result}))
    }

    fn finish(&mut self, stop: acp::StopReason) -> Result<(), ProviderFailure> {
        if let Some(id) = self.prompt_id.take() {
            let result = serde_json::to_value(acp::PromptResponse::new(stop))
                .map_err(|_| failure(ProviderStage::Read, ProviderDiagnostic::InvalidInput))?;
            self.reply(id, result)?;
        }
        Ok(())
    }
}

impl fmt::Debug for FakeAcpPeer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeAcpPeer")
            .field("queued", &self.inbound.len())
            .field("cancelled", &self.cancelled)
            .field("exited", &self.exited)
            .finish()
    }
}

impl JsonRpcTransport for FakeAcpPeer {
    fn send(&mut self, message: &Value) -> Result<(), ProviderFailure> {
        self.check_stage(ProviderStage::Write)?;
        let message = roundtrip(message)?;
        let params = &message["params"];
        let id = message["id"].clone();
        match message["method"].as_str() {
            Some("initialize") => {
                self.check_stage(ProviderStage::Initialize)?;
                decode::<acp::InitializeRequest>(params)?;
                let result = json!({"protocolVersion":1,"agentCapabilities":{
                    "mcpCapabilities":{"http":true,"sse":false},
                    "promptCapabilities":{"image":true}
                },"authMethods":[]});
                decode::<acp::InitializeResponse>(&result)?;
                self.reply(id, result)?;
            }
            Some("authenticate") => {
                self.check_stage(ProviderStage::Initialize)?;
                decode::<acp::AuthenticateRequest>(params)?;
                let result = json!({});
                decode::<acp::AuthenticateResponse>(&result)?;
                self.reply(id, result)?;
            }
            Some("session/set_config_option") => {
                let config = params["configId"].as_str().unwrap_or_default().to_owned();
                let value = params["value"].clone();
                let options = &mut self.config.config_options;
                let Some(option) = options.iter_mut().find(|option| option["id"] == config) else {
                    return Err(failure(
                        ProviderStage::Write,
                        ProviderDiagnostic::InvalidInput,
                    ));
                };
                let thought = option["category"] == "thought_level";
                if !(thought && self.config.ignore_level) {
                    option["currentValue"] = value.clone();
                }
                if config == "model" {
                    options.retain(|option| option["category"] != "thought_level");
                    if let Some((_, level)) = self
                        .config
                        .thought_by_model
                        .iter()
                        .find(|(model, _)| value.as_str() == Some(model.as_str()))
                    {
                        options.push(level.clone());
                    }
                }
                let result = json!({"configOptions": options.clone()});
                self.reply(id, result)?;
            }
            Some("session/new") => {
                self.check_stage(ProviderStage::Initialize)?;
                decode::<acp::NewSessionRequest>(params)?;
                let result = if self.config.config_options.is_empty() {
                    json!({"sessionId":SESSION})
                } else {
                    json!({"sessionId":SESSION, "configOptions": self.config.config_options.clone()})
                };
                decode::<acp::NewSessionResponse>(&result)?;
                self.reply(id, result)?;
            }
            Some("session/prompt") => {
                self.check_stage(ProviderStage::Submit)?;
                decode::<acp::PromptRequest>(params)?;
                self.state.lock().unwrap().prompts += 1;
                self.prompt_id = Some(id);
                if self.config.exit_after_prompt {
                    self.exit();
                } else if let Some(options) = &self.config.permission_options {
                    let params = json!({"sessionId":SESSION,
                        "toolCall":{"toolCallId":"<tool>","title":"<summary>"},
                        "options":options});
                    self.queue(json!({"jsonrpc":"2.0","id":PERMISSION_ID,
                        "method":"session/request_permission","params":params}))?;
                    self.state.lock().unwrap().permission_pending = true;
                } else if let Some(stop) = self.config.stop_reason {
                    self.finish(stop)?;
                }
            }
            Some("session/cancel") => {
                self.check_stage(ProviderStage::Cancel)?;
                decode::<acp::CancelNotification>(params)?;
                self.state.lock().unwrap().cancels += 1;
                self.cancelled = true;
                if !self.config.ignore_cancel {
                    self.finish(acp::StopReason::Cancelled)?;
                }
            }
            None if id == PERMISSION_ID => {
                self.check_stage(ProviderStage::Approval)?;
                let answer = decode::<acp::RequestPermissionResponse>(&message["result"])?;
                if !self.state.lock().unwrap().permission_pending {
                    return Err(failure(
                        ProviderStage::Approval,
                        ProviderDiagnostic::InvalidState,
                    ));
                }
                if let acp::RequestPermissionOutcome::Selected(selected) = &answer.outcome {
                    let offered = self
                        .config
                        .permission_options
                        .as_ref()
                        .is_some_and(|options| {
                            options
                                .iter()
                                .any(|option| option.option_id == selected.option_id)
                        });
                    if !offered {
                        return Err(failure(
                            ProviderStage::Approval,
                            ProviderDiagnostic::InvalidInput,
                        ));
                    }
                }
                self.state.lock().unwrap().permission_pending = false;
            }
            // Extension replies are observable by the test through sent().
            None if message.get("result").is_some() || message.get("error").is_some() => {}
            _ => {
                return Err(failure(
                    ProviderStage::Write,
                    ProviderDiagnostic::InvalidInput,
                ));
            }
        }
        self.state.lock().unwrap().sent.push(message);
        Ok(())
    }

    fn receive(&mut self, _timeout: Duration) -> Result<TransportReceive, ProviderFailure> {
        self.check_stage(ProviderStage::Read)?;
        self.state.lock().unwrap().receives += 1;
        if let Some(frame) = self.inbound.pop_front() {
            return Ok(TransportReceive::Frame(frame));
        }
        if self.exited {
            return Err(failure(
                ProviderStage::Read,
                ProviderDiagnostic::UnexpectedExit,
            ));
        }
        if self.cancelled && self.config.flood_after_cancel {
            let update = late_update();
            validate_server(&update)?;
            return Ok(TransportReceive::Frame(roundtrip(&update)?));
        }
        Ok(TransportReceive::Timeout)
    }

    fn shutdown(&mut self, _timeout: Duration) -> Result<(), ProviderFailure> {
        self.state.lock().unwrap().shutdowns += 1;
        self.exited = true;
        self.check_stage(ProviderStage::Shutdown)
    }

    fn terminate_tree(&mut self) -> Result<(), ProviderFailure> {
        self.state.lock().unwrap().terminations += 1;
        self.exited = true;
        self.check_stage(ProviderStage::Shutdown)
    }
}

pub(super) fn late_update() -> Value {
    json!({"jsonrpc":"2.0","method":"session/update","params":{
        "sessionId":SESSION,"update":{"sessionUpdate":"agent_message_chunk",
        "content":{"type":"text","text":"<response>"}}}})
}

/// Exercise exactly the production JSONL framing without a socket or disk.
fn roundtrip(value: &Value) -> Result<Value, ProviderFailure> {
    let mut bytes = Vec::new();
    // Koru's prompt frames are bounded by the outbound limit; the fake is
    // the agent reading them, so it reads at that bound too.
    write_json_line(
        &mut bytes,
        value,
        super::DEFAULT_PROVIDER_PROMPT_FRAME_BYTES,
    )?;
    JsonLineReader::new(
        std::io::Cursor::new(bytes),
        super::DEFAULT_PROVIDER_PROMPT_FRAME_BYTES,
    )?
    .next_frame()?
    .map(|frame| frame.into_value())
    .ok_or_else(|| failure(ProviderStage::Read, ProviderDiagnostic::MalformedFrame))
}

fn decode<T: DeserializeOwned>(value: &Value) -> Result<T, ProviderFailure> {
    serde_json::from_value(value.clone())
        .map_err(|_| failure(ProviderStage::Read, ProviderDiagnostic::MalformedFrame))
}

fn validate_server(frame: &Value) -> Result<(), ProviderFailure> {
    match frame["method"].as_str() {
        Some("session/update") => {
            decode::<acp::SessionNotification>(&frame["params"])?;
        }
        Some("session/request_permission") => {
            decode::<acp::RequestPermissionRequest>(&frame["params"])?;
        }
        _ => {}
    }
    Ok(())
}

fn failure(stage: ProviderStage, diagnostic: ProviderDiagnostic) -> ProviderFailure {
    ProviderFailure::new(ErrorCode::ProviderFailed, stage, diagnostic)
}

#[cfg(test)]
mod tests;
