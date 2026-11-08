// SPDX-License-Identifier: MPL-2.0

//! Native OpenAI-compatible Chat Completions adapter for hosted Koru AI.

use std::time::Duration;

use koru_ai_core::{
    AiError, AiErrorClass, AiEvent, AiProvider, AiProviderId, AiRequest, Cancellation,
    ProviderStream, Timestamp, validate_reasoning,
};
use koru_ai_transport::blocking_runtime;
use serde::Deserialize;
use url::Url;
use zeroize::Zeroizing;

mod metadata;
mod request;
mod stream;
mod think;

pub use metadata::{KORU_MODEL, KORU_MODEL_LABEL, koru_model};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const START_TIMEOUT: Duration = Duration::from_secs(15);

pub struct KoruProvider {
    api_key: Zeroizing<String>,
    endpoint: Url,
    client: reqwest::Client,
}

impl KoruProvider {
    pub fn from_env() -> Result<Self, AiError> {
        let key_configured = koru_env::value(koru_env::name::KORU_AI_HOST_KEY).is_some();
        let endpoint = match koru_env::value(koru_env::name::KORU_AI_HOST_ENDPOINT) {
            Some(endpoint) => endpoint,
            None => {
                tracing::info!(
                    target: "koru_ai::provider",
                    endpoint_configured = false,
                    key_configured,
                    "hosted Koru AI env is missing KORU_AI_HOST_ENDPOINT"
                );
                return Err(AiError::new(
                    AiErrorClass::Configuration,
                    "Set KORU_AI_HOST_ENDPOINT to configure the hosted provider.",
                    false,
                ));
            }
        };
        let endpoint = match Url::parse(&endpoint) {
            Ok(endpoint) => endpoint,
            Err(_) => {
                tracing::warn!(
                    target: "koru_ai::provider",
                    endpoint_configured = true,
                    key_configured,
                    "hosted Koru AI env is not a valid URL"
                );
                return Err(AiError::new(
                    AiErrorClass::Configuration,
                    "KORU_AI_HOST_ENDPOINT is not a valid URL.",
                    false,
                ));
            }
        };
        tracing::info!(
            target: "koru_ai::provider",
            endpoint_configured = true,
            key_configured,
            scheme = endpoint.scheme(),
            host = endpoint.host_str().unwrap_or(""),
            "hosted Koru AI adapter constructed from env"
        );
        let key = koru_env::value(koru_env::name::KORU_AI_HOST_KEY).unwrap_or_default();
        Ok(Self::with_endpoint(key, endpoint))
    }

    pub fn with_endpoint(api_key: String, endpoint: Url) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .expect("reqwest client configuration is valid");
        Self {
            api_key: Zeroizing::new(api_key),
            endpoint: normalized_endpoint(endpoint),
            client,
        }
    }

    pub fn list_models(&self) -> Result<Vec<String>, AiError> {
        let started = std::time::Instant::now();
        tracing::info!(
            target: "koru_ai::provider",
            host = self.endpoint.host_str().unwrap_or(""),
            "hosted Koru AI listing models"
        );
        let runtime = runtime()?;
        let url = self.url("models")?;
        let response = match runtime.block_on(async {
            tokio::time::timeout(
                START_TIMEOUT,
                self.client.get(url).authorized_by(self).send(),
            )
            .await
        }) {
            Err(_) => {
                tracing::warn!(
                    target: "koru_ai::provider",
                    stage = "timeout",
                    elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                    "hosted Koru AI models request timed out"
                );
                return Err(temporary());
            }
            Ok(Err(_)) => {
                tracing::warn!(
                    target: "koru_ai::provider",
                    stage = "transport",
                    elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                    "hosted Koru AI models request did not connect"
                );
                return Err(temporary());
            }
            Ok(Ok(response)) => response,
        };
        let status = response.status().as_u16();
        if !response.status().is_success() {
            tracing::warn!(
                target: "koru_ai::provider",
                status,
                elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                "hosted Koru AI models request was rejected"
            );
            return Err(status_error(status));
        }
        let mut models = runtime
            .block_on(response.json::<ModelList>())
            .map_err(|_| {
                tracing::warn!(
                    target: "koru_ai::provider",
                    stage = "decode",
                    "hosted Koru AI models response was not JSON"
                );
                provider_error()
            })?
            .data
            .into_iter()
            .map(|model| model.id)
            .filter(|model| !model.trim().is_empty())
            .collect::<Vec<_>>();
        models.sort();
        models.dedup();
        tracing::info!(
            target: "koru_ai::provider",
            model_count = models.len(),
            has_hosted_model = models.iter().any(|model| model == KORU_MODEL),
            elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
            "hosted Koru AI listed models"
        );
        Ok(models)
    }

    pub fn validate_hosted_model(&self) -> Result<koru_ai_core::ModelDescriptor, AiError> {
        if !self
            .list_models()?
            .iter()
            .any(|candidate| candidate == KORU_MODEL)
        {
            return Err(AiError::new(
                AiErrorClass::ModelUnavailable,
                "The hosted Koru AI model is not available.",
                false,
            ));
        }
        Ok(koru_model())
    }

    fn url(&self, path: &str) -> Result<Url, AiError> {
        self.endpoint.join(path).map_err(|_| {
            AiError::new(
                AiErrorClass::Configuration,
                "The hosted Koru AI endpoint is invalid.",
                false,
            )
        })
    }
}

impl AiProvider for KoruProvider {
    fn start(
        &self,
        request: AiRequest,
        cancellation: Cancellation,
    ) -> Result<ProviderStream, AiError> {
        if request.provider.as_str() != AiProviderId::KORU {
            return Err(AiError::new(
                AiErrorClass::Configuration,
                "The Koru AI adapter received a request for another provider.",
                false,
            ));
        }
        if !request.options.stream || request.options.provider_storage {
            return Err(AiError::new(
                AiErrorClass::Configuration,
                "Koru AI requests must stream with provider storage disabled.",
                false,
            ));
        }
        if cancellation.is_cancelled() {
            return Ok(Box::new(std::iter::once(AiEvent::Cancelled {
                attempt_id: request.attempt_id,
                at: Timestamp::now(),
            })));
        }
        let descriptor = koru_model();
        let effective = validate_reasoning(&descriptor.capabilities.reasoning, request.reasoning)?;
        if request.options.web_search {
            return Err(AiError::new(
                AiErrorClass::Configuration,
                "Web search is not supported by the hosted Koru AI model.",
                false,
            ));
        }
        if request.model != KORU_MODEL {
            return Err(AiError::new(
                AiErrorClass::ModelUnavailable,
                "The hosted Koru AI model is not available.",
                false,
            ));
        }
        let has_tools = !request.tools.is_empty();
        let body = request::encode(&request)?;
        let mut payload_bytes = body.len().try_into().unwrap_or(u64::MAX);
        tracing::info!(
            target: "koru_ai::provider",
            payload_len = body.len(),
            "sending request to hosted Koru AI"
        );
        let runtime = runtime()?;
        let url = self.url("chat/completions")?;
        let mut response = runtime
            .block_on(async {
                tokio::time::timeout(
                    START_TIMEOUT,
                    self.client
                        .post(url.clone())
                        .authorized_by(self)
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .body(body)
                        .send(),
                )
                .await
            })
            .map_err(|_| temporary())?
            .map_err(|_| temporary())?;
        if !response.status().is_success() {
            let status = response.status();
            let body_bytes = runtime
                .block_on(async { response.bytes().await })
                .unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body_bytes);

            // If the server rejected tool choice because vLLM lacks tool-call-parser flags
            // or the tool definitions caused the prompt to exceed the model's context length,
            // retry once without tools so regular chat and reasoning continue to work.
            let is_tool_choice_error =
                body_str.contains("tool choice requires") || body_str.contains("tool_choice");
            let is_context_length_error = body_str.contains("maximum context length")
                || body_str.contains("context_length_exceeded")
                || body_str.contains("input prompt or the number of requested output tokens");
            if status.as_u16() == 400
                && has_tools
                && (is_tool_choice_error || is_context_length_error)
            {
                tracing::warn!(
                    target: "koru_ai::provider",
                    "hosted Koru AI endpoint rejected tools or context length exceeded with tools; retrying without tools"
                );
                let body_no_tools = request::encode_without_tools(&request)?;
                payload_bytes = body_no_tools.len().try_into().unwrap_or(u64::MAX);
                let retry_res = runtime
                    .block_on(async {
                        tokio::time::timeout(
                            START_TIMEOUT,
                            self.client
                                .post(url)
                                .authorized_by(self)
                                .header(reqwest::header::CONTENT_TYPE, "application/json")
                                .body(body_no_tools)
                                .send(),
                        )
                        .await
                    })
                    .map_err(|_| temporary())?
                    .map_err(|_| temporary())?;

                if retry_res.status().is_success() {
                    response = retry_res;
                } else {
                    let retry_status = retry_res.status();
                    let retry_bytes = runtime
                        .block_on(async { retry_res.bytes().await })
                        .unwrap_or_default();
                    let shape = UpstreamError::read(&retry_bytes);
                    tracing::warn!(
                        target: "koru_ai::provider",
                        status = retry_status.as_u16(),
                        body_bytes = retry_bytes.len(),
                        error_type = shape.kind.as_deref().unwrap_or(""),
                        error_code = shape.code.as_deref().unwrap_or(""),
                        "hosted Koru AI retry without tools also returned non-success response"
                    );
                    let mut error = status_error(retry_status.as_u16());
                    if let Some(message) = shape.message {
                        error.public_message = message;
                    }
                    return Ok(Box::new(std::iter::once(AiEvent::Failed {
                        attempt_id: request.attempt_id,
                        error,
                        at: Timestamp::now(),
                    })));
                }
            } else {
                let shape = UpstreamError::read(&body_bytes);
                tracing::warn!(
                    target: "koru_ai::provider",
                    status = status.as_u16(),
                    body_bytes = body_bytes.len(),
                    error_type = shape.kind.as_deref().unwrap_or(""),
                    error_code = shape.code.as_deref().unwrap_or(""),
                    "hosted Koru AI request returned non-success response"
                );
                let mut error = status_error(status.as_u16());
                if let Some(message) = shape.message {
                    error.public_message = message;
                }
                return Ok(Box::new(std::iter::once(AiEvent::Failed {
                    attempt_id: request.attempt_id,
                    error,
                    at: Timestamp::now(),
                })));
            }
        }
        Ok(Box::new(stream::KoruStream::new(
            runtime,
            response,
            request.attempt_id,
            request.model,
            effective,
            payload_bytes,
            cancellation,
        )))
    }
}

#[derive(Deserialize)]
struct ModelList {
    #[serde(default)]
    data: Vec<Model>,
}

#[derive(Deserialize)]
struct Model {
    id: String,
}

fn runtime() -> Result<tokio::runtime::Runtime, AiError> {
    blocking_runtime().map_err(|_| {
        AiError::new(
            AiErrorClass::Provider,
            "The Koru AI runtime could not start.",
            true,
        )
    })
}

fn normalized_endpoint(mut endpoint: Url) -> Url {
    let trimmed = endpoint.path().trim_end_matches('/');
    if trimmed.is_empty() {
        endpoint.set_path("/v1/");
    } else if trimmed.ends_with("/v1") || trimmed == "v1" {
        endpoint.set_path(&format!("{trimmed}/"));
    } else {
        endpoint.set_path(&format!("{trimmed}/v1/"));
    }
    endpoint
}

/// What an OpenAI-compatible error body says about itself: its `type` and
/// `code`, which are safe to log, and its `message`, which is shown to the
/// person and never logged. A body that is not that shape — an HTML page from
/// a proxy, a validation error echoing the request — yields nothing: the
/// status's own wording stands, and the body reaches neither the log nor the
/// status bar. Logging the body was how prompt text could end up in ordinary
/// logs, since a 400 that echoes `messages[...]` is a prompt.
#[derive(Default)]
struct UpstreamError {
    kind: Option<String>,
    code: Option<String>,
    message: Option<String>,
}

impl UpstreamError {
    fn read(body: &[u8]) -> Self {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
            return Self::default();
        };
        let error = value.get("error").unwrap_or(&value);
        let text = |key: &str| {
            error
                .get(key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
        };
        Self {
            kind: text("type"),
            code: text("code").or_else(|| error.get("code").map(|c| c.to_string())),
            message: text("message"),
        }
    }
}

/// The bearer, when there is one. An endpoint that needs no key (a local
/// vLLM) gets no `Authorization` header rather than `Bearer ` with nothing
/// after it, which is a malformed header some servers refuse outright.
trait Authorized {
    fn authorized_by(self, provider: &KoruProvider) -> Self;
}

impl Authorized for reqwest::RequestBuilder {
    fn authorized_by(self, provider: &KoruProvider) -> Self {
        if provider.api_key.is_empty() {
            self
        } else {
            self.bearer_auth(provider.api_key.as_str())
        }
    }
}

fn status_error(status: u16) -> AiError {
    match status {
        401 | 403 => AiError::new(
            AiErrorClass::Authentication,
            "The hosted Koru AI service rejected its credentials.",
            false,
        ),
        404 => AiError::new(
            AiErrorClass::ModelUnavailable,
            "The hosted Koru AI model is not available.",
            false,
        ),
        413 => AiError::new(
            AiErrorClass::ContextTooLarge,
            "The request is too large for the hosted Koru AI model.",
            false,
        ),
        429 => AiError::new(
            AiErrorClass::RateLimit,
            "The hosted Koru AI service is busy. Try again later.",
            true,
        ),
        408 | 500..=599 => temporary(),
        _ => provider_error(),
    }
}

fn temporary() -> AiError {
    AiError::new(
        AiErrorClass::Network,
        "The hosted Koru AI service is unreachable. Check your connection or try again.",
        true,
    )
}

fn provider_error() -> AiError {
    AiError::new(
        AiErrorClass::Provider,
        "The hosted Koru AI service could not complete the request.",
        true,
    )
}

#[cfg(test)]
mod tests {
    use koru_ai_core::{
        AiProvider, AiRequestMessage, AiRequestOptions, AiToolDefinition, AiToolSafety, ChatRole,
        InferenceAttemptId, ReasoningChoice,
    };
    use koru_ai_transport::test_server::{ResponseSpec, TestServer};
    use serde_json::{Value, json};

    use super::*;

    fn request() -> AiRequest {
        AiRequest {
            attempt_id: InferenceAttemptId::new(),
            provider: AiProviderId::parse(AiProviderId::KORU).unwrap(),
            model: KORU_MODEL.into(),
            reasoning: ReasoningChoice::Default,
            prompt_template_version: "chat-v1".into(),
            system: "You are Koru.".into(),
            messages: vec![AiRequestMessage {
                role: ChatRole::User,
                markdown: "Reply with pong".into(),
                attachments: Vec::new(),
            }],
            context: Vec::new(),
            tools: Vec::new(),
            tool_history: Vec::new(),
            options: AiRequestOptions::default(),
        }
    }

    #[test]
    fn thinking_off_is_encoded_and_text_stream_completes() {
        let server = TestServer::start(vec![ResponseSpec::sse(vec![
            json!({
                "id": "chatcmpl-1",
                "choices": [{ "delta": { "content": "pong" }, "finish_reason": null }]
            }),
            json!({
                "id": "chatcmpl-1",
                "choices": [{ "delta": {}, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4 }
            }),
        ])]);
        let mut request = request();
        request.reasoning = ReasoningChoice::Off;
        let provider = KoruProvider::with_endpoint("test-key".into(), server.url());
        let events = provider
            .start(request, Cancellation::default())
            .unwrap()
            .collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(
            event,
            AiEvent::TextDelta { text, .. } if text == "pong"
        )));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AiEvent::Completed { .. }))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AiEvent::ReasoningDelta { .. }))
        );
        let captured = server.finish();
        let body: Value = serde_json::from_slice(&captured[0].body).unwrap();
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!(captured[0].headers["authorization"], "Bearer test-key");
        assert_eq!(captured[0].path, "/v1/chat/completions");
    }

    #[test]
    fn think_tags_and_reasoning_content_become_reasoning_deltas() {
        let server = TestServer::start(vec![ResponseSpec::sse(vec![
            json!({
                "id": "chatcmpl-2",
                "choices": [{
                    "delta": {
                        "reasoning": "vllm-plan",
                    },
                    "finish_reason": null
                }]
            }),
            json!({
                "id": "chatcmpl-2",
                "choices": [{
                    "delta": {
                        "reasoning_content": "plan",
                        "content": "<think>hidden</think>visible"
                    },
                    "finish_reason": "stop"
                }]
            }),
        ])]);
        let provider = KoruProvider::with_endpoint("test-key".into(), server.url());
        let events = provider
            .start(request(), Cancellation::default())
            .unwrap()
            .collect::<Vec<_>>();
        let reasoning = events
            .iter()
            .filter_map(|event| match event {
                AiEvent::ReasoningDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(reasoning.contains(&"vllm-plan"));
        assert!(reasoning.contains(&"plan"));
        assert!(reasoning.contains(&"hidden"));
        assert!(events.iter().any(|event| matches!(
            event,
            AiEvent::TextDelta { text, .. } if text == "visible"
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            AiEvent::TextDelta { text, .. } if text.contains("think") || text.contains("hidden")
        )));
        let _ = server.finish();
    }

    #[test]
    fn tool_call_argument_fragments_accumulate_by_index() {
        let server = TestServer::start(vec![ResponseSpec::sse(vec![
            json!({
                "id": "chatcmpl-3",
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_1",
                            "function": { "name": "read_document", "arguments": "{\"document" }
                        }]
                    }
                }]
            }),
            json!({
                "id": "chatcmpl-3",
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": { "arguments": "_id\":\"doc-1\"}" }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            }),
        ])]);
        let mut request = request();
        request.tools = vec![AiToolDefinition {
            name: "read_document".into(),
            title: "Read document".into(),
            description: "Read one authorized document".into(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "object" }),
            safety: AiToolSafety {
                read_only: true,
                destructive: false,
                idempotent: true,
                open_world: false,
            },
        }];
        let provider = KoruProvider::with_endpoint("test-key".into(), server.url());
        let events = provider
            .start(request, Cancellation::default())
            .unwrap()
            .collect::<Vec<_>>();
        assert!(matches!(
            events.last(),
            Some(AiEvent::ToolCalls { calls, .. })
                if calls.len() == 1
                    && calls[0].name == "read_document"
                    && calls[0].arguments == json!({ "document_id": "doc-1" })
        ));
        let _ = server.finish();
    }

    #[test]
    fn thinking_on_is_the_default_encode() {
        let encoded = request::encode(&request()).unwrap();
        let body: Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
        assert_eq!(body["model"], KORU_MODEL);
    }

    #[test]
    fn normalized_endpoint_normalizes_v1_and_trailing_slashes() {
        for (input, expected) in [
            ("https://example.com", "https://example.com/v1/"),
            ("https://example.com/", "https://example.com/v1/"),
            ("https://example.com/v1", "https://example.com/v1/"),
            ("https://example.com/v1/", "https://example.com/v1/"),
            ("https://example.com/proxy", "https://example.com/proxy/v1/"),
            (
                "https://example.com/proxy/v1",
                "https://example.com/proxy/v1/",
            ),
            (
                "https://example.com/proxy/v1/",
                "https://example.com/proxy/v1/",
            ),
        ] {
            let parsed = Url::parse(input).unwrap();
            assert_eq!(
                normalized_endpoint(parsed).as_str(),
                expected,
                "failed for input: {input}"
            );
        }
    }

    #[test]
    fn list_models_and_validate_hosted_model_succeed() {
        let response = || {
            ResponseSpec::json(
                200,
                json!({
                    "object": "list",
                    "data": [{ "id": KORU_MODEL }]
                }),
            )
        };
        let server = TestServer::start(vec![response(), response()]);
        let provider = KoruProvider::with_endpoint("test-key".into(), server.url());
        let models = provider.list_models().unwrap();
        assert_eq!(models, vec![KORU_MODEL]);
        let descriptor = provider.validate_hosted_model().unwrap();
        assert_eq!(descriptor.selection.requested_model, KORU_MODEL);
        let captured = server.finish();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].path, "/v1/models");
        assert_eq!(captured[1].path, "/v1/models");
    }

    #[test]
    fn an_endpoint_without_a_key_gets_no_authorization_header() {
        let server = TestServer::start(vec![ResponseSpec::sse(vec![
            json!({
                "id": "chatcmpl-nokey",
                "choices": [{ "delta": { "content": "ok" }, "finish_reason": null }]
            }),
            json!({
                "id": "chatcmpl-nokey",
                "choices": [{ "delta": {}, "finish_reason": "stop" }]
            }),
        ])]);
        let provider = KoruProvider::with_endpoint(String::new(), server.url());
        let _ = provider
            .start(request(), Cancellation::default())
            .unwrap()
            .collect::<Vec<_>>();
        let captured = server.finish();
        assert!(
            !captured[0].headers.contains_key("authorization"),
            "an empty key must not become `Bearer ` with nothing after it"
        );
    }

    /// A body that is not an OpenAI-style error — a proxy's HTML page, a
    /// validation error echoing the request — reaches neither the person
    /// nor the log. Only the status's own wording does.
    #[test]
    fn a_non_json_error_body_is_neither_shown_nor_logged() {
        let server = TestServer::start(vec![ResponseSpec::json(
            502,
            json!("<html>Bad gateway: the prompt was \"tell me everything\"</html>"),
        )]);
        let provider = KoruProvider::with_endpoint("test-key".into(), server.url());
        let events = provider
            .start(request(), Cancellation::default())
            .unwrap()
            .collect::<Vec<_>>();
        let failed = events
            .iter()
            .find_map(|event| match event {
                AiEvent::Failed { error, .. } => Some(error.clone()),
                _ => None,
            })
            .expect("a failure event");
        assert!(!failed.public_message.contains("tell me everything"));
        assert_eq!(failed.public_message, status_error(502).public_message);
        server.finish();
    }

    /// An OpenAI-style error keeps its `message` for the person; its `type`
    /// and `code` are what the log carries.
    #[test]
    fn an_upstream_error_is_read_for_its_shape() {
        let shape = UpstreamError::read(
            br#"{"error":{"message":"model not found","type":"NotFoundError","code":404}}"#,
        );
        assert_eq!(shape.message.as_deref(), Some("model not found"));
        assert_eq!(shape.kind.as_deref(), Some("NotFoundError"));
        assert_eq!(shape.code.as_deref(), Some("404"));
        let flat =
            UpstreamError::read(br#"{"message":"quota exceeded","code":"insufficient_quota"}"#);
        assert_eq!(flat.message.as_deref(), Some("quota exceeded"));
        assert_eq!(flat.code.as_deref(), Some("insufficient_quota"));
        let none = UpstreamError::read(b"<html>nope</html>");
        assert!(none.message.is_none() && none.kind.is_none() && none.code.is_none());
    }

    #[test]
    fn rejected_tool_choice_retries_without_tools_and_succeeds() {
        let server = TestServer::start(vec![
            ResponseSpec::json(
                400,
                json!({
                    "error": {
                        "message": "\"auto\" tool choice requires --enable-auto-tool-choice and --tool-call-parser to be set",
                        "type": "BadRequestError"
                    }
                }),
            ),
            ResponseSpec::sse(vec![
                json!({
                    "id": "chatcmpl-retry-1",
                    "choices": [{ "delta": { "content": "pong without tools" }, "finish_reason": null }]
                }),
                json!({
                    "id": "chatcmpl-retry-1",
                    "choices": [{ "delta": {}, "finish_reason": "stop" }],
                    "usage": { "prompt_tokens": 3, "completion_tokens": 3, "total_tokens": 6 }
                }),
            ]),
        ]);
        let mut request = request();
        request.tools = vec![AiToolDefinition {
            name: "read_document".into(),
            title: "Read document".into(),
            description: "Read one authorized document".into(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "object" }),
            safety: AiToolSafety {
                read_only: true,
                destructive: false,
                idempotent: true,
                open_world: false,
            },
        }];
        let provider = KoruProvider::with_endpoint("test-key".into(), server.url());
        let events = provider
            .start(request, Cancellation::default())
            .unwrap()
            .collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(
            event,
            AiEvent::TextDelta { text, .. } if text == "pong without tools"
        )));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AiEvent::Completed { .. }))
        );

        let captured = server.finish();
        assert_eq!(captured.len(), 2);
        // First request had tools
        let first_body: Value = serde_json::from_slice(&captured[0].body).unwrap();
        assert!(first_body.get("tools").is_some());
        // Second (retried) request omitted tools
        let second_body: Value = serde_json::from_slice(&captured[1].body).unwrap();
        assert!(second_body.get("tools").is_none());
    }

    #[test]
    fn context_length_exceeded_retries_without_tools_and_succeeds() {
        let server = TestServer::start(vec![
            ResponseSpec::json(
                400,
                json!({
                    "error": {
                        "message": "This model's maximum context length is 8192 tokens. However, you requested 0 output tokens and your prompt contains at least 8193 input tokens, for a total of at least 8193 tokens. Please reduce the length of the input prompt or the number of requested output tokens. (parameter=input_tokens, value=8193)",
                        "type": "BadRequestError"
                    }
                }),
            ),
            ResponseSpec::sse(vec![
                json!({
                    "id": "chatcmpl-retry-ctx",
                    "choices": [{ "delta": { "content": "hello" }, "finish_reason": null }]
                }),
                json!({
                    "id": "chatcmpl-retry-ctx",
                    "choices": [{ "delta": {}, "finish_reason": "stop" }],
                    "usage": { "prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11 }
                }),
            ]),
        ]);
        let mut request = request();
        request.tools = vec![AiToolDefinition {
            name: "read_document".into(),
            title: "Read document".into(),
            description: "Read one authorized document".into(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "object" }),
            safety: AiToolSafety {
                read_only: true,
                destructive: false,
                idempotent: true,
                open_world: false,
            },
        }];
        let provider = KoruProvider::with_endpoint("test-key".into(), server.url());
        let events = provider
            .start(request, Cancellation::default())
            .unwrap()
            .collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(
            event,
            AiEvent::TextDelta { text, .. } if text == "hello"
        )));
        let captured = server.finish();
        assert_eq!(captured.len(), 2);
        let second_body: Value = serde_json::from_slice(&captured[1].body).unwrap();
        assert!(second_body.get("tools").is_none());
    }
}
