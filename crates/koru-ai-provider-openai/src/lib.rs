// SPDX-License-Identifier: MPL-2.0

//! Native OpenAI Responses adapter.

use std::time::Duration;

use koru_ai_core::{
    AiError, AiErrorClass, AiEvent, AiProvider, AiProviderId, AiRequest, Cancellation,
    ProviderStream, Timestamp, validate_reasoning,
};
use koru_ai_transport::blocking_runtime;
use serde::Deserialize;
use url::Url;
use zeroize::Zeroizing;

mod events;
mod metadata;
mod request;
mod stream;

pub use metadata::openai_model;

const OPENAI_ENDPOINT: &str = "https://api.openai.com/v1/";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const START_TIMEOUT: Duration = Duration::from_secs(15);

pub struct OpenAiProvider {
    api_key: Zeroizing<String>,
    endpoint: Url,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(api_key: String) -> Self {
        Self::with_endpoint(
            api_key,
            Url::parse(OPENAI_ENDPOINT).expect("OpenAI endpoint is a URL"),
        )
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
        let runtime = runtime()?;
        let url = self.url("models")?;
        let response = runtime
            .block_on(async {
                tokio::time::timeout(
                    START_TIMEOUT,
                    self.client
                        .get(url)
                        .bearer_auth(self.api_key.as_str())
                        .send(),
                )
                .await
            })
            .map_err(|_| temporary())?
            .map_err(|_| temporary())?;
        if !response.status().is_success() {
            return Err(status_error(response.status().as_u16()));
        }
        let mut models = runtime
            .block_on(response.json::<ModelList>())
            .map_err(|_| provider_error())?
            .data
            .into_iter()
            .map(|model| model.id)
            .filter(|model| !model.trim().is_empty())
            .collect::<Vec<_>>();
        models.sort();
        models.dedup();
        Ok(models)
    }

    pub fn validate_model(&self, model: &str) -> Result<koru_ai_core::ModelDescriptor, AiError> {
        if !self
            .list_models()?
            .iter()
            .any(|candidate| candidate == model)
        {
            return Err(AiError::new(
                AiErrorClass::ModelUnavailable,
                "The selected OpenAI model is not available to this project.",
                false,
            ));
        }
        Ok(openai_model(model))
    }

    fn url(&self, path: &str) -> Result<Url, AiError> {
        self.endpoint.join(path).map_err(|_| {
            AiError::new(
                AiErrorClass::Configuration,
                "The OpenAI endpoint is invalid.",
                false,
            )
        })
    }
}

impl AiProvider for OpenAiProvider {
    fn start(
        &self,
        request: AiRequest,
        cancellation: Cancellation,
    ) -> Result<ProviderStream, AiError> {
        if request.provider.as_str() != AiProviderId::OPENAI {
            return Err(AiError::new(
                AiErrorClass::Configuration,
                "The OpenAI adapter received a request for another provider.",
                false,
            ));
        }
        if !request.options.stream || request.options.provider_storage {
            return Err(AiError::new(
                AiErrorClass::Configuration,
                "OpenAI requests must stream with provider storage disabled.",
                false,
            ));
        }
        if cancellation.is_cancelled() {
            return Ok(Box::new(std::iter::once(AiEvent::Cancelled {
                attempt_id: request.attempt_id,
                at: Timestamp::now(),
            })));
        }
        let descriptor = openai_model(&request.model);
        let effective = validate_reasoning(&descriptor.capabilities.reasoning, request.reasoning)?;
        if request.options.web_search && !descriptor.capabilities.web_search {
            return Err(AiError::new(
                AiErrorClass::Configuration,
                "Web search is not supported by the selected model.",
                false,
            ));
        }
        let body = request::encode(&request)?;
        let payload_bytes = body.len().try_into().unwrap_or(u64::MAX);
        let runtime = runtime()?;
        let url = self.url("responses")?;
        let response = runtime
            .block_on(async {
                tokio::time::timeout(
                    START_TIMEOUT,
                    self.client
                        .post(url)
                        .bearer_auth(self.api_key.as_str())
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .body(body)
                        .send(),
                )
                .await
            })
            .map_err(|_| temporary())?
            .map_err(|_| temporary())?;
        if !response.status().is_success() {
            let error = status_error(response.status().as_u16());
            return Ok(Box::new(std::iter::once(AiEvent::Failed {
                attempt_id: request.attempt_id,
                error,
                at: Timestamp::now(),
            })));
        }
        Ok(Box::new(stream::OpenAiStream::new(
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
            "The OpenAI runtime could not start.",
            true,
        )
    })
}

fn normalized_endpoint(mut endpoint: Url) -> Url {
    if !endpoint.path().ends_with('/') {
        endpoint.set_path(&format!("{}/", endpoint.path()));
    }
    endpoint
}

fn status_error(status: u16) -> AiError {
    match status {
        401 | 403 => AiError::new(
            AiErrorClass::Authentication,
            "OpenAI rejected the saved project API key.",
            false,
        ),
        404 => AiError::new(
            AiErrorClass::ModelUnavailable,
            "The selected OpenAI model is not available.",
            false,
        ),
        413 => AiError::new(
            AiErrorClass::ContextTooLarge,
            "The request is too large for the selected OpenAI model.",
            false,
        ),
        429 => AiError::new(
            AiErrorClass::RateLimit,
            "OpenAI is rate limiting this project. Try again later.",
            true,
        ),
        408 | 500..=599 => temporary(),
        _ => provider_error(),
    }
}

fn temporary() -> AiError {
    AiError::new(
        AiErrorClass::Network,
        "OpenAI is temporarily unavailable.",
        true,
    )
}

fn provider_error() -> AiError {
    AiError::new(
        AiErrorClass::Provider,
        "OpenAI could not complete the request.",
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
            provider: AiProviderId::parse(AiProviderId::OPENAI).unwrap(),
            model: "gpt-5.4".into(),
            reasoning: ReasoningChoice::Default,
            prompt_template_version: "chat-v1".into(),
            system: "You are Koru.".into(),
            messages: vec![AiRequestMessage {
                role: ChatRole::User,
                markdown: "What changed today?".into(),
                attachments: Vec::new(),
            }],
            context: Vec::new(),
            tools: vec![AiToolDefinition {
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
            }],
            tool_history: Vec::new(),
            options: AiRequestOptions {
                web_search: true,
                ..AiRequestOptions::default()
            },
        }
    }

    #[test]
    fn native_response_stream_preserves_payload_and_normalizes_search_sources_and_calls() {
        let server = TestServer::start(vec![ResponseSpec::sse(vec![
            json!({ "type": "response.created", "response": { "id": "resp_123" } }),
            json!({ "type": "response.some_future_event", "future": true }),
            json!({ "type": "response.web_search_call.in_progress", "item_id": "search_1" }),
            json!({ "type": "response.output_text.delta", "delta": "Fresh answer" }),
            json!({
                "type": "response.output_text.annotation.added",
                "annotation": {
                    "type": "url_citation",
                    "url": "https://example.com/source",
                    "title": "Example source"
                }
            }),
            json!({ "type": "response.web_search_call.completed", "item_id": "search_1" }),
            json!({
                "type": "response.output_item.added",
                "output_index": 2,
                "item": { "type": "function_call", "call_id": "call_1", "name": "read_document" }
            }),
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 2,
                "delta": "{\"document_id\":\"doc-1\"}"
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_123",
                    "status": "completed",
                    "usage": { "input_tokens": 9, "output_tokens": 4, "total_tokens": 13 },
                    "output": []
                }
            }),
        ])]);
        let provider = OpenAiProvider::with_endpoint("project-key".into(), server.url());
        let request = request();
        let attempt_id = request.attempt_id;
        let events = provider
            .start(request, Cancellation::default())
            .unwrap()
            .collect::<Vec<_>>();

        assert!(matches!(
            &events[0],
            AiEvent::Started {
                provider_response_id: Some(id),
                request_payload_bytes: Some(bytes),
                ..
            } if id == "resp_123" && *bytes > 0
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            AiEvent::WebSearch {
                phase: koru_ai_core::WebSearchPhase::Started,
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AiEvent::Citation { source, .. }
                if source.url == "https://example.com/source" && source.title == "Example source"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AiEvent::Usage { usage, .. } if usage.total_tokens == Some(13)
        )));
        assert!(matches!(
            events.last(),
            Some(AiEvent::ToolCalls { calls, .. })
                if calls.len() == 1
                    && calls[0].name == "read_document"
                    && calls[0].arguments == json!({ "document_id": "doc-1" })
        ));

        let captured = server.finish();
        assert_eq!(captured[0].path, "/responses");
        assert_eq!(captured[0].headers["authorization"], "Bearer project-key");
        let body: Value = serde_json::from_slice(&captured[0].body).unwrap();
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["input"][0]["role"], "user");
        assert!(
            body["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["type"] == "web_search")
        );
        assert_eq!(
            events[0],
            AiEvent::Started {
                attempt_id,
                provider_response_id: Some("resp_123".into()),
                request_payload_bytes: Some(captured[0].body.len() as u64),
                resolved_model: Some("gpt-5.4".into()),
                effective_reasoning: Some(ReasoningChoice::Level(
                    koru_ai_core::ReasoningLevel::Medium,
                )),
                at: match &events[0] {
                    AiEvent::Started { at, .. } => *at,
                    _ => unreachable!(),
                },
            }
        );
    }

    #[test]
    fn model_catalog_and_http_statuses_use_native_endpoints_and_safe_errors() {
        let server = TestServer::start(vec![
            ResponseSpec::json(
                200,
                json!({ "data": [{"id": "gpt-5.4"}, {"id": "gpt-4.1-mini"}] }),
            ),
            ResponseSpec::json(
                401,
                json!({ "error": { "message": "secret upstream detail" } }),
            ),
        ]);
        let provider = OpenAiProvider::with_endpoint("project-key".into(), server.url());
        assert_eq!(provider.list_models().unwrap(), ["gpt-4.1-mini", "gpt-5.4"]);
        let error = provider.list_models().unwrap_err();
        assert_eq!(error.class, AiErrorClass::Authentication);
        assert!(!error.public_message.contains("secret upstream detail"));
        let requests = server.finish();
        assert_eq!(
            requests
                .iter()
                .map(|request| request.path.as_str())
                .collect::<Vec<_>>(),
            ["/models", "/models"]
        );
    }

    #[test]
    fn pre_cancelled_request_performs_no_network_io() {
        let server = TestServer::start(Vec::new());
        let cancellation = Cancellation::default();
        cancellation.cancel();
        let events = OpenAiProvider::with_endpoint("project-key".into(), server.url())
            .start(request(), cancellation)
            .unwrap()
            .collect::<Vec<_>>();
        assert!(matches!(events.as_slice(), [AiEvent::Cancelled { .. }]));
        assert!(server.finish().is_empty());
    }
}
