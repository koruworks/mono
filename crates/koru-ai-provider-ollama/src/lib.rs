// SPDX-License-Identifier: MPL-2.0

//! Native Ollama adapter for chat, discovery, inspection, and model pulls.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use koru_ai_core::{
    AiError, AiErrorClass, AiEvent, AiProvider, AiProviderId, AiRequest, AiToolCall, AiUsage,
    Cancellation, ModelDescriptor, ModelPullProgress, ProviderStream, ReasoningChoice, Timestamp,
    UsageSource, conversion, validate_reasoning,
};
use koru_ai_transport::{FrameError, NdjsonDecoder, blocking_runtime};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

mod metadata;

pub use metadata::ollama_model;

const OLLAMA_ENDPOINT: &str = "http://localhost:11434/";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PULL_START_TIMEOUT: Duration = Duration::from_secs(15);
const CHAT_START_TIMEOUT: Duration = Duration::from_secs(180);
const STREAM_POLL: Duration = Duration::from_millis(50);

pub type ModelPullStream = Box<dyn Iterator<Item = Result<ModelPullProgress, AiError>> + Send>;

#[derive(Clone)]
pub struct OllamaService {
    endpoint: Url,
    client: Client,
}

impl Default for OllamaService {
    fn default() -> Self {
        Self::new(Url::parse(OLLAMA_ENDPOINT).expect("Ollama endpoint is a URL"))
    }
}

impl OllamaService {
    pub fn new(endpoint: Url) -> Self {
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .expect("reqwest client configuration is valid");
        Self {
            endpoint: normalized_endpoint(endpoint),
            client,
        }
    }

    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    pub fn list_models(&self) -> Result<Vec<String>, AiError> {
        let runtime = runtime("Ollama")?;
        let transport = TransportWait::new(&runtime, None, PROBE_TIMEOUT);
        let response = transport.wait(self.client.get(self.url("api/tags")?).send())?;
        ensure_success(response.status())?;
        let body: TagsResponse = transport.wait(response.json())?;
        let mut models = body
            .models
            .into_iter()
            .filter_map(|model| model.model.or(model.name))
            .filter(|model| !model.trim().is_empty())
            .collect::<Vec<_>>();
        models.sort();
        models.dedup();
        Ok(models)
    }

    pub fn inspect_model(&self, model: &str) -> Result<ModelDescriptor, AiError> {
        self.inspect_model_with_cancellation(model, None)
    }

    fn inspect_model_with_cancellation(
        &self,
        model: &str,
        cancellation: Option<&Cancellation>,
    ) -> Result<ModelDescriptor, AiError> {
        validate_model_name(model)?;
        let runtime = runtime("Ollama")?;
        let transport = TransportWait::new(&runtime, cancellation, PROBE_TIMEOUT);
        let response = transport.wait(
            self.client
                .post(self.url("api/show")?)
                .json(&json!({ "model": model }))
                .send(),
        )?;
        ensure_success(response.status())?;
        let body: ShowResponse = transport.wait(response.json())?;
        Ok(ollama_model(model, &body.capabilities))
    }

    pub fn pull_model(
        &self,
        model: &str,
        cancellation: Cancellation,
    ) -> Result<ModelPullStream, AiError> {
        validate_model_name(model)?;
        if cancellation.is_cancelled() {
            return Err(cancelled_pull());
        }
        let runtime = runtime("Ollama")?;
        let transport = TransportWait::new(&runtime, Some(&cancellation), PULL_START_TIMEOUT);
        let response = transport
            .wait(
                self.client
                    .post(self.url("api/pull")?)
                    .json(&json!({ "model": model, "stream": true }))
                    .send(),
            )
            .map_err(|error| {
                if error.class == AiErrorClass::Cancelled {
                    cancelled_pull()
                } else {
                    error
                }
            })?;
        ensure_success(response.status())?;
        Ok(Box::new(PullStream {
            runtime,
            bytes: Box::pin(response.bytes_stream()),
            decoder: NdjsonDecoder::default(),
            model: model.to_owned(),
            cancellation,
            pending: VecDeque::new(),
            done: false,
        }))
    }

    fn url(&self, path: &str) -> Result<Url, AiError> {
        self.endpoint.join(path).map_err(|_| {
            AiError::new(
                AiErrorClass::Configuration,
                "The Ollama endpoint is invalid.",
                false,
            )
        })
    }
}

#[derive(Clone)]
pub struct OllamaProvider {
    service: OllamaService,
}

impl OllamaProvider {
    pub fn new(endpoint: Url) -> Self {
        Self {
            service: OllamaService::new(endpoint),
        }
    }

    pub fn from_service(service: OllamaService) -> Self {
        Self { service }
    }

    pub fn service(&self) -> &OllamaService {
        &self.service
    }
}

impl AiProvider for OllamaProvider {
    fn start(
        &self,
        request: AiRequest,
        cancellation: Cancellation,
    ) -> Result<ProviderStream, AiError> {
        if request.provider.as_str() != AiProviderId::OLLAMA {
            return Err(AiError::new(
                AiErrorClass::Configuration,
                "The Ollama adapter received a request for another provider.",
                false,
            ));
        }
        if !request.options.stream || request.options.provider_storage {
            return Err(AiError::new(
                AiErrorClass::Configuration,
                "Ollama requests must stream with provider storage disabled.",
                false,
            ));
        }
        if cancellation.is_cancelled() {
            return Ok(Box::new(std::iter::once(AiEvent::Cancelled {
                attempt_id: request.attempt_id,
                at: Timestamp::now(),
            })));
        }

        let descriptor = match self
            .service
            .inspect_model_with_cancellation(&request.model, Some(&cancellation))
        {
            Ok(descriptor) => descriptor,
            Err(error) if error.class == AiErrorClass::Cancelled => {
                return Ok(Box::new(std::iter::once(AiEvent::Cancelled {
                    attempt_id: request.attempt_id,
                    at: Timestamp::now(),
                })));
            }
            Err(error) => return Err(error),
        };
        let effective = validate_reasoning(&descriptor.capabilities.reasoning, request.reasoning)?;
        let converted = conversion::convert(&request)?;
        let mut messages = Vec::with_capacity(converted.messages.len() + 1);
        messages.push(json!({ "role": "system", "content": converted.system }));
        messages.extend(converted.messages.into_iter().map(|message| {
            let role = match message.role {
                koru_ai_core::ChatRole::User => "user",
                koru_ai_core::ChatRole::Assistant => "assistant",
            };
            json!({ "role": role, "content": message.markdown })
        }));
        append_tool_history(&mut messages, &request)?;
        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "stream": true
        });
        if !request.tools.is_empty() {
            body["tools"] = json!(
                request
                    .tools
                    .iter()
                    .map(|tool| json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.input_schema,
                        }
                    }))
                    .collect::<Vec<_>>()
            );
        }
        if let Some(think) = ollama_think(request.reasoning) {
            body["think"] = think;
        }

        let runtime = runtime("Ollama")?;
        let transport = TransportWait::new(&runtime, Some(&cancellation), CHAT_START_TIMEOUT);
        let response = match transport.wait(
            self.service
                .client
                .post(self.service.url("api/chat")?)
                .json(&body)
                .send(),
        ) {
            Ok(response) => response,
            Err(error) if error.class == AiErrorClass::Cancelled => {
                return Ok(Box::new(std::iter::once(AiEvent::Cancelled {
                    attempt_id: request.attempt_id,
                    at: Timestamp::now(),
                })));
            }
            Err(error) => return Err(error),
        };
        ensure_success(response.status())?;
        let mut pending = VecDeque::new();
        pending.push_back(AiEvent::Started {
            attempt_id: request.attempt_id,
            provider_response_id: None,
            request_payload_bytes: None,
            resolved_model: descriptor.resolved_model,
            effective_reasoning: effective,
            at: Timestamp::now(),
        });
        Ok(Box::new(OllamaChatStream {
            runtime,
            bytes: Box::pin(response.bytes_stream()),
            decoder: NdjsonDecoder::default(),
            attempt_id: request.attempt_id,
            cancellation,
            pending,
            tool_calls: Vec::new(),
            terminal: false,
        }))
    }
}

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

struct OllamaChatStream {
    runtime: tokio::runtime::Runtime,
    bytes: ByteStream,
    decoder: NdjsonDecoder,
    attempt_id: koru_ai_core::InferenceAttemptId,
    cancellation: Cancellation,
    pending: VecDeque<AiEvent>,
    tool_calls: Vec<AiToolCall>,
    terminal: bool,
}

impl Iterator for OllamaChatStream {
    type Item = AiEvent;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(event);
            }
            if self.terminal {
                return None;
            }
            if self.cancellation.is_cancelled() {
                self.terminal = true;
                return Some(AiEvent::Cancelled {
                    attempt_id: self.attempt_id,
                    at: Timestamp::now(),
                });
            }
            match self
                .runtime
                .block_on(async { tokio::time::timeout(STREAM_POLL, self.bytes.next()).await })
            {
                Err(_) => continue,
                Ok(Some(Ok(bytes))) => match self.decoder.push(&bytes) {
                    Ok(lines) => {
                        for line in lines {
                            self.handle_line(line);
                        }
                    }
                    Err(error) => self.fail(frame_error(error)),
                },
                Ok(Some(Err(error))) => self.fail(map_transport(error)),
                Ok(None) => match self.decoder.finish() {
                    Ok(Some(line)) => {
                        self.handle_line(line);
                        if !self.terminal {
                            self.fail(unexpected_end());
                        }
                    }
                    Ok(None) => self.fail(unexpected_end()),
                    Err(error) => self.fail(frame_error(error)),
                },
            }
        }
    }
}

impl OllamaChatStream {
    fn handle_line(&mut self, line: Value) {
        if self.terminal {
            return;
        }
        if line.get("error").and_then(Value::as_str).is_some() {
            self.fail(AiError::new(
                AiErrorClass::Provider,
                "Ollama could not complete the request.",
                false,
            ));
            return;
        }
        if let Some(text) = line
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            self.pending.push_back(AiEvent::TextDelta {
                attempt_id: self.attempt_id,
                text: text.to_owned(),
                at: Timestamp::now(),
            });
        }
        if let Some(calls) = line
            .get("message")
            .and_then(|message| message.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for call in calls {
                let Some(function) = call.get("function") else {
                    continue;
                };
                let Some(name) = function.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let arguments = function
                    .get("arguments")
                    .cloned()
                    .and_then(|arguments| match arguments {
                        Value::String(encoded) => serde_json::from_str(&encoded).ok(),
                        value => Some(value),
                    })
                    .unwrap_or_else(|| json!({}));
                let call_id = call
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        format!("ollama-{}-{}", self.attempt_id, self.tool_calls.len())
                    });
                self.tool_calls.push(AiToolCall {
                    call_id,
                    name: name.to_owned(),
                    arguments,
                    thought_signatures: Vec::new(),
                });
            }
        }
        if line.get("done").and_then(Value::as_bool) == Some(true) {
            let usage = ollama_usage(&line);
            if usage.source == UsageSource::ProviderReported {
                self.pending.push_back(AiEvent::Usage {
                    attempt_id: self.attempt_id,
                    usage,
                });
            }
            let stop_reason = line
                .get("done_reason")
                .and_then(Value::as_str)
                .map(str::to_owned);
            self.pending.push_back(if self.tool_calls.is_empty() {
                AiEvent::Completed {
                    attempt_id: self.attempt_id,
                    stop_reason,
                    at: Timestamp::now(),
                }
            } else {
                AiEvent::ToolCalls {
                    attempt_id: self.attempt_id,
                    calls: std::mem::take(&mut self.tool_calls),
                    stop_reason,
                    at: Timestamp::now(),
                }
            });
            self.terminal = true;
        }
    }

    fn fail(&mut self, error: AiError) {
        if !self.terminal {
            self.pending.push_back(AiEvent::Failed {
                attempt_id: self.attempt_id,
                error,
                at: Timestamp::now(),
            });
            self.terminal = true;
        }
    }
}

fn append_tool_history(messages: &mut Vec<Value>, request: &AiRequest) -> Result<(), AiError> {
    for turn in &request.tool_history {
        messages.push(json!({
            "role": "assistant",
            "content": "",
            "tool_calls": turn.calls.iter().map(|call| json!({
                "id": call.call_id,
                "function": {
                    "name": call.name,
                    "arguments": call.arguments,
                }
            })).collect::<Vec<_>>()
        }));
        for result in &turn.results {
            let content = serde_json::to_string(&json!({
                "ok": !result.is_error,
                "result": result.content,
            }))
            .map_err(|_| {
                AiError::new(
                    AiErrorClass::Provider,
                    "The AI tool result could not be encoded.",
                    false,
                )
            })?;
            messages.push(json!({
                "role": "tool",
                "content": content,
                "tool_name": result.name,
            }));
        }
    }
    Ok(())
}

struct PullStream {
    runtime: tokio::runtime::Runtime,
    bytes: ByteStream,
    decoder: NdjsonDecoder,
    model: String,
    cancellation: Cancellation,
    pending: VecDeque<Result<ModelPullProgress, AiError>>,
    done: bool,
}

impl Iterator for PullStream {
    type Item = Result<ModelPullProgress, AiError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Some(item);
            }
            if self.done {
                return None;
            }
            if self.cancellation.is_cancelled() {
                self.done = true;
                return Some(Err(cancelled_pull()));
            }
            match self
                .runtime
                .block_on(async { tokio::time::timeout(STREAM_POLL, self.bytes.next()).await })
            {
                Err(_) => continue,
                Ok(Some(Ok(bytes))) => match self.decoder.push(&bytes) {
                    Ok(lines) => {
                        for line in lines {
                            self.handle_line(line);
                        }
                    }
                    Err(error) => self.finish_error(frame_error(error)),
                },
                Ok(Some(Err(error))) => self.finish_error(map_transport(error)),
                Ok(None) => {
                    match self.decoder.finish() {
                        Ok(Some(line)) => self.handle_line(line),
                        Ok(None) => {}
                        Err(error) => self.finish_error(frame_error(error)),
                    }
                    self.done = true;
                }
            }
        }
    }
}

impl PullStream {
    fn handle_line(&mut self, line: Value) {
        if line.get("error").and_then(Value::as_str).is_some() {
            self.finish_error(AiError::new(
                AiErrorClass::Provider,
                "Ollama could not pull the selected model.",
                true,
            ));
            return;
        }
        let status = line
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pulling")
            .to_owned();
        self.pending.push_back(Ok(ModelPullProgress {
            model: self.model.clone(),
            status: status.clone(),
            completed_bytes: line.get("completed").and_then(Value::as_u64),
            total_bytes: line.get("total").and_then(Value::as_u64),
        }));
        if status == "success" {
            self.done = true;
        }
    }

    fn finish_error(&mut self, error: AiError) {
        if !self.done {
            self.pending.push_back(Err(error));
            self.done = true;
        }
    }
}

fn frame_error(_: FrameError) -> AiError {
    AiError::new(
        AiErrorClass::Provider,
        "Ollama returned an invalid streaming response.",
        false,
    )
}

fn ollama_usage(line: &Value) -> AiUsage {
    let prompt_tokens = line.get("prompt_eval_count").and_then(Value::as_u64);
    let completion_tokens = line.get("eval_count").and_then(Value::as_u64);
    let total_tokens = match (prompt_tokens, completion_tokens) {
        (Some(prompt), Some(completion)) => prompt.checked_add(completion),
        _ => None,
    };
    let available = prompt_tokens.is_some() || completion_tokens.is_some();
    AiUsage {
        prompt_tokens,
        cached_prompt_tokens: None,
        completion_tokens,
        reasoning_tokens: None,
        total_tokens,
        source: if available {
            UsageSource::ProviderReported
        } else {
            UsageSource::Unavailable
        },
    }
}

fn ollama_think(choice: ReasoningChoice) -> Option<Value> {
    match choice {
        ReasoningChoice::Default => None,
        ReasoningChoice::Off => Some(Value::Bool(false)),
        ReasoningChoice::On => Some(Value::Bool(true)),
        ReasoningChoice::Level(level) => Some(Value::String(level.as_token().to_owned())),
    }
}

fn validate_model_name(model: &str) -> Result<(), AiError> {
    if model.trim().is_empty() || model.len() > 500 || model.chars().any(char::is_control) {
        return Err(AiError::new(
            AiErrorClass::Configuration,
            "The Ollama model name is invalid.",
            false,
        ));
    }
    Ok(())
}

fn normalized_endpoint(mut endpoint: Url) -> Url {
    if !endpoint.path().ends_with('/') {
        endpoint.set_path(&format!("{}/", endpoint.path()));
    }
    endpoint
}

fn runtime(provider: &str) -> Result<tokio::runtime::Runtime, AiError> {
    blocking_runtime().map_err(|_| {
        AiError::new(
            AiErrorClass::Provider,
            format!("The {provider} runtime could not start."),
            true,
        )
    })
}

struct TransportWait<'a> {
    runtime: &'a tokio::runtime::Runtime,
    cancellation: Option<&'a Cancellation>,
    deadline: tokio::time::Instant,
}

impl<'a> TransportWait<'a> {
    fn new(
        runtime: &'a tokio::runtime::Runtime,
        cancellation: Option<&'a Cancellation>,
        timeout: Duration,
    ) -> Self {
        Self {
            runtime,
            cancellation,
            deadline: tokio::time::Instant::now() + timeout,
        }
    }

    fn wait<T>(
        &self,
        future: impl Future<Output = Result<T, reqwest::Error>>,
    ) -> Result<T, AiError> {
        self.runtime.block_on(async {
            let mut future = Box::pin(future);
            loop {
                if self.cancellation.is_some_and(Cancellation::is_cancelled) {
                    return Err(cancelled_response());
                }
                let remaining = self
                    .deadline
                    .saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    return Err(AiError::new(
                        AiErrorClass::Network,
                        "Ollama did not respond in time.",
                        true,
                    ));
                }
                let poll = if self.cancellation.is_some() {
                    STREAM_POLL.min(remaining)
                } else {
                    remaining
                };
                match tokio::time::timeout(poll, future.as_mut()).await {
                    Ok(Ok(value)) => return Ok(value),
                    Ok(Err(error)) => return Err(map_transport(error)),
                    Err(_) => {}
                }
            }
        })
    }
}

fn ensure_success(status: StatusCode) -> Result<(), AiError> {
    if status.is_success() {
        return Ok(());
    }
    Err(match status.as_u16() {
        404 => AiError::new(
            AiErrorClass::ModelUnavailable,
            "The selected Ollama model is not installed.",
            false,
        ),
        408 | 500..=599 => AiError::new(
            AiErrorClass::Network,
            "Ollama is temporarily unavailable.",
            true,
        ),
        _ => AiError::new(
            AiErrorClass::Provider,
            "Ollama rejected the request.",
            false,
        ),
    })
}

fn map_transport(error: reqwest::Error) -> AiError {
    if error.is_connect() || error.is_timeout() || error.is_request() {
        AiError::new(AiErrorClass::Network, "Ollama could not be reached.", true)
    } else {
        AiError::new(
            AiErrorClass::Provider,
            "Ollama returned an invalid response.",
            false,
        )
    }
}

fn unexpected_end() -> AiError {
    AiError::new(
        AiErrorClass::Network,
        "The Ollama response ended unexpectedly.",
        true,
    )
}

fn cancelled_pull() -> AiError {
    AiError::new(
        AiErrorClass::Cancelled,
        "The Ollama model pull was cancelled.",
        false,
    )
}

fn cancelled_response() -> AiError {
    AiError::new(
        AiErrorClass::Cancelled,
        "The Ollama response was cancelled.",
        false,
    )
}

#[derive(Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: Option<String>,
    model: Option<String>,
}

#[derive(Deserialize)]
struct ShowResponse {
    #[serde(default)]
    capabilities: Vec<String>,
}

#[cfg(test)]
mod tests {
    use koru_ai_core::{
        AiProviderId, AiRequestMessage, AiRequestOptions, AiToolDefinition, AiToolSafety, ChatRole,
        InferenceAttemptId,
    };

    use koru_ai_transport::test_server::{ResponseSpec, TestServer};

    use super::*;

    fn request(reasoning: ReasoningChoice) -> AiRequest {
        AiRequest {
            attempt_id: InferenceAttemptId::new(),
            provider: AiProviderId::parse(AiProviderId::OLLAMA).unwrap(),
            model: "gpt-oss:20b".into(),
            reasoning,
            prompt_template_version: "chat-v1".into(),
            system: "You are Koru.".into(),
            messages: vec![AiRequestMessage {
                role: ChatRole::User,
                markdown: "Hello".into(),
                attachments: Vec::new(),
            }],
            context: Vec::new(),
            tools: Vec::new(),
            tool_history: Vec::new(),
            options: AiRequestOptions::default(),
        }
    }

    fn read_document_tool() -> AiToolDefinition {
        AiToolDefinition {
            name: "read_document".into(),
            title: "Read document".into(),
            description: "Read one authorized Koru document".into(),
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

    #[test]
    fn decoder_preserves_unicode_split_across_transport_chunks() {
        let encoded = serde_json::to_vec(&json!({
            "message": { "content": "Kōru 🌿" },
            "done": false
        }))
        .unwrap();
        let split = encoded
            .windows(4)
            .position(|window| window == "🌿".as_bytes())
            .unwrap()
            + 2;
        let mut decoder = NdjsonDecoder::default();
        assert!(decoder.push(&encoded[..split]).unwrap().is_empty());
        let mut tail = encoded[split..].to_vec();
        tail.push(b'\n');
        let lines = decoder.push(&tail).unwrap();
        assert_eq!(lines[0]["message"]["content"], "Kōru 🌿");
    }

    #[test]
    fn usage_is_partial_without_inventing_missing_totals() {
        let partial = ollama_usage(&json!({ "prompt_eval_count": 12 }));
        assert_eq!(partial.prompt_tokens, Some(12));
        assert_eq!(partial.completion_tokens, None);
        assert_eq!(partial.total_tokens, None);
        assert_eq!(partial.source, UsageSource::ProviderReported);

        let unavailable = ollama_usage(&json!({ "done": true }));
        assert_eq!(unavailable.source, UsageSource::Unavailable);
    }

    #[test]
    fn thinking_control_uses_ollamas_native_shapes() {
        assert_eq!(ollama_think(ReasoningChoice::Default), None);
        assert_eq!(ollama_think(ReasoningChoice::Off), Some(json!(false)));
        assert_eq!(ollama_think(ReasoningChoice::On), Some(json!(true)));
        assert_eq!(
            ollama_think(ReasoningChoice::Level(koru_ai_core::ReasoningLevel::High)),
            Some(json!("high"))
        );
    }

    #[test]
    fn discovery_inspection_and_chat_use_native_ollama_protocol() {
        let server = TestServer::start(vec![
            ResponseSpec::json(
                200,
                json!({
                    "models": [
                        { "name": "qwen3:4b" },
                        { "model": "gpt-oss:20b" }
                    ]
                }),
            ),
            ResponseSpec::json(200, json!({ "capabilities": ["thinking"] })),
            ResponseSpec::json(200, json!({ "capabilities": ["thinking"] })),
            ResponseSpec::ndjson(vec![
                json!({ "message": { "role": "assistant", "content": "Hi 🌿" }, "done": false }),
                json!({
                    "message": { "role": "assistant", "content": "" },
                    "done": true,
                    "done_reason": "stop",
                    "prompt_eval_count": 7,
                    "eval_count": 3
                }),
            ]),
        ]);
        let service = OllamaService::new(server.url());
        assert_eq!(
            service.list_models().unwrap(),
            vec!["gpt-oss:20b", "qwen3:4b"]
        );
        let descriptor = service.inspect_model("gpt-oss:20b").unwrap();
        assert_eq!(descriptor.resolved_model.as_deref(), Some("gpt-oss:20b"));

        let provider = OllamaProvider::from_service(service);
        let events = provider
            .start(
                request(ReasoningChoice::Level(koru_ai_core::ReasoningLevel::High)),
                Cancellation::default(),
            )
            .unwrap()
            .collect::<Vec<_>>();
        assert!(matches!(events[0], AiEvent::Started { .. }));
        assert!(matches!(&events[1], AiEvent::TextDelta { text, .. } if text == "Hi 🌿"));
        assert!(matches!(
            &events[2],
            AiEvent::Usage { usage, .. }
                if usage.prompt_tokens == Some(7)
                    && usage.completion_tokens == Some(3)
                    && usage.total_tokens == Some(10)
        ));
        assert!(
            matches!(&events[3], AiEvent::Completed { stop_reason, .. } if stop_reason.as_deref() == Some("stop"))
        );

        let requests = server.finish();
        assert_eq!(requests[0].path, "/api/tags");
        assert_eq!(requests[1].path, "/api/show");
        assert_eq!(requests[2].path, "/api/show");
        assert_eq!(requests[3].path, "/api/chat");
        let chat: Value = serde_json::from_slice(&requests[3].body).unwrap();
        assert_eq!(chat["model"], "gpt-oss:20b");
        assert_eq!(chat["stream"], true);
        assert_eq!(chat["think"], "high");
        assert_eq!(chat["messages"][0]["role"], "system");
        assert_eq!(chat["messages"][1]["role"], "user");
    }

    #[test]
    fn chat_uses_native_tools_and_returns_normalized_calls() {
        let server = TestServer::start(vec![
            ResponseSpec::json(200, json!({ "capabilities": ["tools"] })),
            ResponseSpec::ndjson(vec![json!({
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "function": {
                            "name": "read_document",
                            "arguments": { "document_id": "document-a" }
                        }
                    }]
                },
                "done": true,
                "done_reason": "stop"
            })]),
        ]);
        let mut request = request(ReasoningChoice::Default);
        request.tools.push(read_document_tool());
        let events = OllamaProvider::new(server.url())
            .start(request, Cancellation::default())
            .unwrap()
            .collect::<Vec<_>>();
        assert!(matches!(
            events.last(),
            Some(AiEvent::ToolCalls { calls, .. })
                if calls.len() == 1
                    && calls[0].name == "read_document"
                    && calls[0].arguments == json!({ "document_id": "document-a" })
        ));

        let requests = server.finish();
        let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "read_document");
    }

    #[test]
    fn model_pull_reports_progress_and_success() {
        let server = TestServer::start(vec![ResponseSpec::ndjson(vec![
            json!({ "status": "downloading", "completed": 20, "total": 100 }),
            json!({ "status": "success" }),
        ])]);
        let service = OllamaService::new(server.url());
        let progress = service
            .pull_model("qwen3:4b", Cancellation::default())
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(progress.len(), 2);
        assert_eq!(progress[0].completed_bytes, Some(20));
        assert_eq!(progress[0].total_bytes, Some(100));
        assert_eq!(progress[1].status, "success");
        let requests = server.finish();
        assert_eq!(requests[0].path, "/api/pull");
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body, json!({ "model": "qwen3:4b", "stream": true }));
    }

    #[test]
    fn cancelled_pull_stops_before_reading_the_transport() {
        let server = TestServer::start(vec![ResponseSpec::ndjson(vec![json!({
            "status": "success"
        })])]);
        let service = OllamaService::new(server.url());
        let cancellation = Cancellation::default();
        let mut stream = service
            .pull_model("qwen3:4b", cancellation.clone())
            .unwrap();
        cancellation.cancel();
        assert_eq!(
            stream.next().unwrap().unwrap_err().class,
            AiErrorClass::Cancelled
        );
        drop(stream);
        let _ = server.finish();
    }

    #[test]
    fn pre_cancelled_chat_does_not_probe_or_send() {
        let server = TestServer::start(Vec::new());
        let cancellation = Cancellation::default();
        cancellation.cancel();
        let events = OllamaProvider::new(server.url())
            .start(request(ReasoningChoice::Default), cancellation)
            .unwrap()
            .collect::<Vec<_>>();
        assert!(matches!(events.as_slice(), [AiEvent::Cancelled { .. }]));
        assert!(server.finish().is_empty());
    }

    #[test]
    fn cancellation_interrupts_model_preflight_waiting_for_headers() {
        let (phase_ready, phase_wait) = std::sync::mpsc::channel();
        let server = TestServer::start(vec![
            ResponseSpec::json(200, json!({ "capabilities": ["thinking"] }))
                .with_delay_before_headers(Duration::from_millis(600))
                .notify_before_headers(phase_ready),
        ]);
        let cancellation = Cancellation::default();
        let cancel = cancellation.clone();
        let canceller = std::thread::spawn(move || {
            phase_wait
                .recv_timeout(Duration::from_secs(5))
                .expect("preflight request never arrived");
            let cancelled_at = std::time::Instant::now();
            cancel.cancel();
            cancelled_at
        });

        let events = OllamaProvider::new(server.url())
            .start(request(ReasoningChoice::Default), cancellation)
            .unwrap()
            .collect::<Vec<_>>();
        let finished = std::time::Instant::now();
        let elapsed = finished.duration_since(canceller.join().unwrap());

        assert!(matches!(events.as_slice(), [AiEvent::Cancelled { .. }]));
        assert!(elapsed < Duration::from_millis(400), "elapsed: {elapsed:?}");
        assert_eq!(server.finish()[0].path, "/api/show");
    }

    #[test]
    fn cancellation_interrupts_model_preflight_waiting_for_body() {
        let (phase_ready, phase_wait) = std::sync::mpsc::channel();
        let server = TestServer::start(vec![
            ResponseSpec::json(200, json!({ "capabilities": ["thinking"] }))
                .with_delay_before_body(Duration::from_millis(600))
                .notify_before_body(phase_ready),
        ]);
        let cancellation = Cancellation::default();
        let cancel = cancellation.clone();
        let canceller = std::thread::spawn(move || {
            phase_wait
                .recv_timeout(Duration::from_secs(5))
                .expect("preflight headers never arrived");
            let cancelled_at = std::time::Instant::now();
            cancel.cancel();
            cancelled_at
        });

        let events = OllamaProvider::new(server.url())
            .start(request(ReasoningChoice::Default), cancellation)
            .unwrap()
            .collect::<Vec<_>>();
        let finished = std::time::Instant::now();
        let elapsed = finished.duration_since(canceller.join().unwrap());

        assert!(matches!(events.as_slice(), [AiEvent::Cancelled { .. }]));
        assert!(elapsed < Duration::from_millis(400), "elapsed: {elapsed:?}");
        assert_eq!(server.finish()[0].path, "/api/show");
    }

    #[test]
    fn cancellation_interrupts_model_pull_waiting_for_headers() {
        let (phase_ready, phase_wait) = std::sync::mpsc::channel();
        let server = TestServer::start(vec![
            ResponseSpec::ndjson(vec![json!({ "status": "success" })])
                .with_delay_before_headers(Duration::from_millis(600))
                .notify_before_headers(phase_ready),
        ]);
        let service = OllamaService::new(server.url());
        let cancellation = Cancellation::default();
        let cancel = cancellation.clone();
        let canceller = std::thread::spawn(move || {
            phase_wait
                .recv_timeout(Duration::from_secs(5))
                .expect("pull request never arrived");
            let cancelled_at = std::time::Instant::now();
            cancel.cancel();
            cancelled_at
        });

        let error = match service.pull_model("qwen3:4b", cancellation) {
            Ok(_) => panic!("cancelled pull unexpectedly returned a stream"),
            Err(error) => error,
        };
        let finished = std::time::Instant::now();
        let elapsed = finished.duration_since(canceller.join().unwrap());

        assert_eq!(error.class, AiErrorClass::Cancelled);
        assert!(elapsed < Duration::from_millis(400), "elapsed: {elapsed:?}");
        assert_eq!(server.finish()[0].path, "/api/pull");
    }

    #[test]
    fn cancellation_is_observed_while_a_cold_model_waits_for_headers() {
        let (phase_ready, phase_wait) = std::sync::mpsc::channel();
        let server = TestServer::start(vec![
            // Exercise a preflight longer than the former 50 ms cancellation
            // guess: cancellation must still wait for the chat request.
            ResponseSpec::json(200, json!({ "capabilities": ["thinking"] }))
                .with_delay_before_body(Duration::from_millis(100)),
            ResponseSpec::ndjson(vec![json!({
                "message": { "role": "assistant", "content": "too late" },
                "done": true
            })])
            .with_delay_before_headers(Duration::from_millis(300))
            .notify_before_headers(phase_ready),
        ]);
        let cancellation = Cancellation::default();
        let cancel = cancellation.clone();
        let canceller = std::thread::spawn(move || {
            phase_wait
                .recv_timeout(Duration::from_secs(5))
                .expect("chat request never arrived");
            let cancelled_at = std::time::Instant::now();
            cancel.cancel();
            cancelled_at
        });
        let events = OllamaProvider::new(server.url())
            .start(request(ReasoningChoice::Default), cancellation)
            .unwrap()
            .collect::<Vec<_>>();
        let finished = std::time::Instant::now();
        let elapsed = finished.duration_since(canceller.join().unwrap());
        assert!(matches!(events.as_slice(), [AiEvent::Cancelled { .. }]));
        assert!(elapsed < Duration::from_millis(250), "elapsed: {elapsed:?}");
        assert_eq!(
            server
                .finish()
                .into_iter()
                .map(|request| request.path)
                .collect::<Vec<_>>(),
            ["/api/show", "/api/chat"]
        );
    }

    #[test]
    #[ignore = "requires a running local Ollama with at least one installed model"]
    fn live_local_catalog_stream_and_in_flight_cancellation() {
        let service = OllamaService::default();
        let models = service.list_models().expect("read live Ollama catalog");
        let model = models
            .iter()
            .find(|model| model.contains(":4b"))
            .or_else(|| models.first())
            .expect("the live release gate needs one installed model")
            .clone();
        let descriptor = service.inspect_model(&model).expect("inspect live model");
        assert_eq!(descriptor.resolved_model.as_deref(), Some(model.as_str()));

        let provider = OllamaProvider::from_service(service);
        let mut complete = request(ReasoningChoice::Default);
        complete.model = model.clone();
        complete.messages[0].markdown = "Reply with only the word OK.".into();
        let events = provider
            .start(complete, Cancellation::default())
            .expect("start live Ollama response")
            .collect::<Vec<_>>();
        assert!(matches!(events.first(), Some(AiEvent::Started { .. })));
        assert!(
            events.iter().any(|event| {
                matches!(event, AiEvent::TextDelta { text, .. } if !text.is_empty())
            })
        );
        assert!(matches!(events.last(), Some(AiEvent::Completed { .. })));

        let cancellation = Cancellation::default();
        let mut cancelled = request(ReasoningChoice::Default);
        cancelled.model = model;
        cancelled.messages[0].markdown =
            "Write a numbered list containing two hundred short items.".into();
        let mut stream = provider
            .start(cancelled, cancellation.clone())
            .expect("start cancellable live Ollama response");
        loop {
            match stream.next().expect("live response ended before text") {
                AiEvent::TextDelta { text, .. } if !text.is_empty() => break,
                event if event.is_terminal() => {
                    panic!("live response became terminal before cancellation: {event:?}")
                }
                _ => {}
            }
        }
        let cancelled_at = std::time::Instant::now();
        cancellation.cancel();
        loop {
            match stream
                .next()
                .expect("cancelled live response lost its terminal event")
            {
                AiEvent::Cancelled { .. } => break,
                event if event.is_terminal() => {
                    panic!("live cancellation produced the wrong terminal event: {event:?}")
                }
                _ => {}
            }
        }
        assert!(
            cancelled_at.elapsed() < Duration::from_secs(2),
            "live Ollama cancellation was not prompt"
        );
    }
}
