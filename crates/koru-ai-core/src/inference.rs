// SPDX-License-Identifier: MPL-2.0

//! Provider-neutral inference requests, events, usage, and safe errors.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::{
    AiProviderId, ChatRole, ContextItem, InferenceAttemptId, ReasoningChoice, Timestamp,
    ToolActivity,
};

pub const DEFAULT_MAX_TOOL_STEPS: u16 = 8;
pub const DEFAULT_MAX_TOOL_CALLS: u16 = 32;
pub const DEFAULT_MAX_TOOL_RESULT_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AiRequest {
    pub attempt_id: InferenceAttemptId,
    pub provider: AiProviderId,
    pub model: String,
    pub reasoning: ReasoningChoice,
    pub prompt_template_version: String,
    pub system: String,
    pub messages: Vec<AiRequestMessage>,
    pub context: Vec<ContextItem>,
    #[serde(default)]
    pub tools: Vec<AiToolDefinition>,
    #[serde(default)]
    pub tool_history: Vec<AiToolTurn>,
    pub options: AiRequestOptions,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AiRequestMessage {
    pub role: ChatRole,
    pub markdown: String,
    /// The pictures and files sent with this message, in order. Only the
    /// local agent provider reads them; every other provider is refused
    /// attachments before a request is built.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<crate::AttachmentInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AiToolDefinition {
    pub name: String,
    pub title: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub safety: AiToolSafety,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AiToolSafety {
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub open_world: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AiToolCall {
    /// Provider-issued correlation id. It is opaque and scoped to one attempt.
    pub call_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    /// Provider-specific reasoning continuity data, never displayed or executed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thought_signatures: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AiToolResult {
    pub call_id: String,
    pub name: String,
    pub content: serde_json::Value,
    pub is_error: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AiToolTurn {
    pub calls: Vec<AiToolCall>,
    pub results: Vec<AiToolResult>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AiToolLoopLimits {
    pub max_steps: u16,
    pub max_calls: u16,
    pub max_result_bytes: u64,
}

impl Default for AiToolLoopLimits {
    fn default() -> Self {
        Self {
            max_steps: DEFAULT_MAX_TOOL_STEPS,
            max_calls: DEFAULT_MAX_TOOL_CALLS,
            max_result_bytes: DEFAULT_MAX_TOOL_RESULT_BYTES,
        }
    }
}

/// Bounded provider/tool conversation state for one durable inference attempt.
#[derive(Clone, Debug)]
pub struct AiToolLoop {
    request: AiRequest,
    limits: AiToolLoopLimits,
    steps: u16,
    calls: u16,
    result_bytes: u64,
}

impl AiToolLoop {
    pub fn new(request: AiRequest, limits: AiToolLoopLimits) -> Result<Self, AiError> {
        let loop_state = Self {
            steps: request.tool_history.len().try_into().unwrap_or(u16::MAX),
            calls: request
                .tool_history
                .iter()
                .map(|turn| u16::try_from(turn.calls.len()).unwrap_or(u16::MAX))
                .fold(0_u16, u16::saturating_add),
            result_bytes: request
                .tool_history
                .iter()
                .flat_map(|turn| &turn.results)
                .map(tool_result_bytes)
                .fold(0_u64, u64::saturating_add),
            request,
            limits,
        };
        loop_state.validate_history()?;
        loop_state.ensure_within_limits()?;
        Ok(loop_state)
    }

    pub fn request(&self) -> &AiRequest {
        &self.request
    }

    pub fn provider_request(&self) -> AiRequest {
        self.request.clone()
    }

    pub fn accept_calls(&self, calls: &[AiToolCall]) -> Result<(), AiError> {
        if calls.is_empty() {
            return Err(tool_protocol_error(
                "The AI provider returned an empty tool call batch.",
            ));
        }
        if self.steps >= self.limits.max_steps
            || self
                .calls
                .saturating_add(calls.len().try_into().unwrap_or(u16::MAX))
                > self.limits.max_calls
        {
            return Err(AiError::new(
                AiErrorClass::Provider,
                "The AI tool step limit was reached.",
                false,
            ));
        }
        let existing_ids = self
            .request
            .tool_history
            .iter()
            .flat_map(|turn| turn.calls.iter().map(|call| call.call_id.as_str()))
            .collect::<HashSet<_>>();
        let mut ids = HashSet::new();
        for call in calls {
            if call.call_id.trim().is_empty()
                || call.name.trim().is_empty()
                || !call.arguments.is_object()
                || !ids.insert(call.call_id.as_str())
                || existing_ids.contains(call.call_id.as_str())
                || !self.request.tools.iter().any(|tool| tool.name == call.name)
            {
                return Err(tool_protocol_error(
                    "The AI provider returned an invalid tool call.",
                ));
            }
        }
        Ok(())
    }

    pub fn append_results(
        &mut self,
        calls: Vec<AiToolCall>,
        results: Vec<AiToolResult>,
    ) -> Result<(), AiError> {
        self.accept_calls(&calls)?;
        if calls.len() != results.len()
            || calls
                .iter()
                .zip(&results)
                .any(|(call, result)| call.call_id != result.call_id || call.name != result.name)
        {
            return Err(tool_protocol_error(
                "The AI tool results did not match the requested calls.",
            ));
        }
        let added_bytes = results
            .iter()
            .map(tool_result_bytes)
            .fold(0_u64, u64::saturating_add);
        if self.result_bytes.saturating_add(added_bytes) > self.limits.max_result_bytes {
            return Err(AiError::new(
                AiErrorClass::ContextTooLarge,
                "The AI tool results are too large to continue.",
                false,
            ));
        }
        self.steps = self.steps.saturating_add(1);
        self.calls = self
            .calls
            .saturating_add(calls.len().try_into().unwrap_or(u16::MAX));
        self.result_bytes = self.result_bytes.saturating_add(added_bytes);
        self.request
            .tool_history
            .push(AiToolTurn { calls, results });
        Ok(())
    }

    fn ensure_within_limits(&self) -> Result<(), AiError> {
        if self.steps > self.limits.max_steps
            || self.calls > self.limits.max_calls
            || self.result_bytes > self.limits.max_result_bytes
        {
            Err(AiError::new(
                AiErrorClass::ContextTooLarge,
                "The AI tool history exceeds its execution limits.",
                false,
            ))
        } else {
            Ok(())
        }
    }

    fn validate_history(&self) -> Result<(), AiError> {
        let mut ids = HashSet::new();
        for turn in &self.request.tool_history {
            if turn.calls.is_empty()
                || turn.calls.len() != turn.results.len()
                || turn.calls.iter().zip(&turn.results).any(|(call, result)| {
                    call.call_id.trim().is_empty()
                        || call.name.trim().is_empty()
                        || !call.arguments.is_object()
                        || !ids.insert(call.call_id.as_str())
                        || !self.request.tools.iter().any(|tool| tool.name == call.name)
                        || call.call_id != result.call_id
                        || call.name != result.name
                })
            {
                return Err(tool_protocol_error(
                    "The AI tool history is invalid or incomplete.",
                ));
            }
        }
        Ok(())
    }
}

fn tool_result_bytes(result: &AiToolResult) -> u64 {
    let content = serde_json::to_vec(&result.content)
        .map(|content| content.len())
        .unwrap_or(usize::MAX);
    result
        .call_id
        .len()
        .saturating_add(result.name.len())
        .saturating_add(content)
        .try_into()
        .unwrap_or(u64::MAX)
}

fn tool_protocol_error(message: &'static str) -> AiError {
    AiError::new(AiErrorClass::Provider, message, false)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AiRequestOptions {
    pub stream: bool,
    pub capture_usage: bool,
    pub provider_storage: bool,
    #[serde(default)]
    pub web_search: bool,
    /// The chat this request belongs to. A local agent's session is owned
    /// under the chat's id, so its provider needs the chat to find or start
    /// the conversation; the API providers ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<crate::ChatId>,
    /// The folder a local agent works in for this chat, when one was chosen;
    /// the API providers ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_folder: Option<String>,
    /// The approval mode a local agent runs this message in (koru-ai D7),
    /// the agent's own id; `None` leaves the agent's default. The API
    /// providers ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_mode: Option<String>,
    /// Whether a local agent runs this message in its model's Fast mode
    /// (koru-ai D11); `None` leaves the model's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_fast: Option<bool>,
}

impl Default for AiRequestOptions {
    fn default() -> Self {
        Self {
            stream: true,
            capture_usage: true,
            provider_storage: false,
            web_search: false,
            chat_id: None,
            working_folder: None,
            agent_mode: None,
            agent_fast: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RequestMeasurements {
    pub message_count: u32,
    pub context_document_count: u32,
    pub context_message_block_count: u32,
    pub context_payload_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AiEvent {
    Started {
        attempt_id: InferenceAttemptId,
        provider_response_id: Option<String>,
        #[serde(default)]
        request_payload_bytes: Option<u64>,
        resolved_model: Option<String>,
        effective_reasoning: Option<ReasoningChoice>,
        at: Timestamp,
    },
    TextDelta {
        attempt_id: InferenceAttemptId,
        text: String,
        at: Timestamp,
    },
    ReasoningDelta {
        attempt_id: InferenceAttemptId,
        text: String,
        at: Timestamp,
    },
    Usage {
        attempt_id: InferenceAttemptId,
        usage: AiUsage,
    },
    ToolCalls {
        attempt_id: InferenceAttemptId,
        calls: Vec<AiToolCall>,
        stop_reason: Option<String>,
        at: Timestamp,
    },
    ToolActivity {
        attempt_id: InferenceAttemptId,
        activity: ToolActivity,
    },
    /// A local agent's status line beside its reply, as kept (koru-ai
    /// D69). The worker publishes it once the row is written.
    AgentNotice {
        attempt_id: InferenceAttemptId,
        notice: crate::AgentNotice,
    },
    /// Worker-internal, like `ToolCalls`: a local agent's tool or permission
    /// fact, which the worker turns into a durable row write and a public
    /// `ToolActivity` event. It never reaches the public stream itself.
    AgentActivity {
        attempt_id: InferenceAttemptId,
        activity: crate::AgentActivityEvent,
        at: Timestamp,
    },
    /// A one-line fact about the attempt the surface shows and nothing
    /// stores.
    Notice {
        attempt_id: InferenceAttemptId,
        notice: crate::AttemptNotice,
        at: Timestamp,
    },
    WebSearch {
        attempt_id: InferenceAttemptId,
        call_id: String,
        phase: crate::WebSearchPhase,
        at: Timestamp,
    },
    Citation {
        attempt_id: InferenceAttemptId,
        source: crate::SourceLink,
        at: Timestamp,
    },
    Completed {
        attempt_id: InferenceAttemptId,
        stop_reason: Option<String>,
        at: Timestamp,
    },
    Failed {
        attempt_id: InferenceAttemptId,
        error: AiError,
        at: Timestamp,
    },
    Cancelled {
        attempt_id: InferenceAttemptId,
        at: Timestamp,
    },
}

impl AiEvent {
    pub const fn attempt_id(&self) -> InferenceAttemptId {
        match self {
            Self::Started { attempt_id, .. }
            | Self::TextDelta { attempt_id, .. }
            | Self::ReasoningDelta { attempt_id, .. }
            | Self::Usage { attempt_id, .. }
            | Self::ToolCalls { attempt_id, .. }
            | Self::ToolActivity { attempt_id, .. }
            | Self::AgentNotice { attempt_id, .. }
            | Self::AgentActivity { attempt_id, .. }
            | Self::Notice { attempt_id, .. }
            | Self::WebSearch { attempt_id, .. }
            | Self::Citation { attempt_id, .. }
            | Self::Completed { attempt_id, .. }
            | Self::Failed { attempt_id, .. }
            | Self::Cancelled { attempt_id, .. } => *attempt_id,
        }
    }

    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Failed { .. } | Self::Cancelled { .. }
        )
    }

    pub const fn ends_provider_step(&self) -> bool {
        self.is_terminal() || matches!(self, Self::ToolCalls { .. })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AiUsage {
    pub prompt_tokens: Option<u64>,
    pub cached_prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub source: UsageSource,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    ProviderReported,
    #[default]
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AiError {
    pub class: AiErrorClass,
    pub public_message: String,
    pub retryable: bool,
}

impl AiError {
    pub fn new(class: AiErrorClass, public_message: impl Into<String>, retryable: bool) -> Self {
        Self {
            class,
            public_message: public_message.into(),
            retryable,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AiErrorClass {
    Configuration,
    Authentication,
    RateLimit,
    ContextTooLarge,
    ModelUnavailable,
    Network,
    Provider,
    Cancelled,
    Storage,
}

/// Provider streams are blocking iterators consumed only by bounded provider
/// executors. No provider task runs on a gateway or UI thread.
pub type ProviderStream = Box<dyn Iterator<Item = AiEvent> + Send + 'static>;

/// Transport-neutral cooperative cancellation shared with one provider call.
#[derive(Clone, Debug, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool_definition() -> AiToolDefinition {
        AiToolDefinition {
            name: "read_document".into(),
            title: "Read document".into(),
            description: "Read one document".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "document_id": { "type": "string" } },
                "required": ["document_id"],
                "additionalProperties": false
            }),
            output_schema: json!({ "type": "object" }),
            safety: AiToolSafety {
                read_only: true,
                destructive: false,
                idempotent: true,
                open_world: false,
            },
        }
    }

    fn request() -> AiRequest {
        AiRequest {
            attempt_id: InferenceAttemptId::new(),
            provider: AiProviderId::parse("mock").unwrap(),
            model: "deterministic-v1".into(),
            reasoning: ReasoningChoice::Default,
            prompt_template_version: "chat-v1".into(),
            system: "system".into(),
            messages: Vec::new(),
            context: Vec::new(),
            tools: vec![tool_definition()],
            tool_history: Vec::new(),
            options: AiRequestOptions::default(),
        }
    }

    fn call(id: &str) -> AiToolCall {
        AiToolCall {
            call_id: id.into(),
            name: "read_document".into(),
            arguments: json!({ "document_id": "document-a" }),
            thought_signatures: Vec::new(),
        }
    }

    fn result(id: &str) -> AiToolResult {
        AiToolResult {
            call_id: id.into(),
            name: "read_document".into(),
            content: json!({ "markdown": "# A" }),
            is_error: false,
        }
    }

    #[test]
    fn provider_storage_is_off_by_default() {
        let options = AiRequestOptions::default();
        assert!(options.stream);
        assert!(options.capture_usage);
        assert!(!options.provider_storage);
    }

    #[test]
    fn cancellation_is_shared_without_a_runtime_type() {
        let first = Cancellation::default();
        let second = first.clone();
        first.cancel();
        assert!(second.is_cancelled());
    }

    #[test]
    fn tool_loop_appends_correlated_results_to_one_provider_request() {
        let mut loop_state = AiToolLoop::new(request(), AiToolLoopLimits::default()).unwrap();
        loop_state
            .append_results(vec![call("call-a")], vec![result("call-a")])
            .unwrap();

        let continued = loop_state.provider_request();
        assert_eq!(continued.tool_history.len(), 1);
        assert_eq!(continued.tool_history[0].calls[0].call_id, "call-a");
        assert_eq!(continued.tool_history[0].results[0].call_id, "call-a");
    }

    #[test]
    fn tool_loop_rejects_unknown_tools_and_reused_provider_ids() {
        let mut loop_state = AiToolLoop::new(request(), AiToolLoopLimits::default()).unwrap();
        loop_state
            .append_results(vec![call("call-a")], vec![result("call-a")])
            .unwrap();

        assert!(loop_state.accept_calls(&[call("call-a")]).is_err());
        let mut unknown = call("call-b");
        unknown.name = "not_authorized".into();
        assert!(loop_state.accept_calls(&[unknown]).is_err());
    }

    #[test]
    fn tool_loop_enforces_step_and_result_limits() {
        let limits = AiToolLoopLimits {
            max_steps: 1,
            max_calls: 1,
            max_result_bytes: 64,
        };
        let mut loop_state = AiToolLoop::new(request(), limits).unwrap();
        loop_state
            .append_results(vec![call("a")], vec![result("a")])
            .unwrap();
        assert!(loop_state.accept_calls(&[call("b")]).is_err());

        let mut too_large = AiToolLoop::new(
            request(),
            AiToolLoopLimits {
                max_result_bytes: 1,
                ..AiToolLoopLimits::default()
            },
        )
        .unwrap();
        assert!(
            too_large
                .append_results(vec![call("large")], vec![result("large")])
                .is_err()
        );
    }
}
