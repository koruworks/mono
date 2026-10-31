// SPDX-License-Identifier: MPL-2.0

//! Deterministic provider used to prove the service architecture without I/O.

use std::collections::VecDeque;
use std::time::Duration;

use koru_ai_core::{
    AiError, AiEvent, AiProvider, AiRequest, AiToolCall, AiUsage, Cancellation, ProviderStream,
    SourceLink, Timestamp, UsageSource, WebSearchPhase,
};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct DeterministicMockProvider {
    response: String,
    chunk_delay: Duration,
    panic_on_start: bool,
    tool_call: Option<(String, Value)>,
    web_sources: Vec<SourceLink>,
}

impl DeterministicMockProvider {
    pub fn new(response: impl Into<String>) -> Self {
        Self {
            response: response.into(),
            chunk_delay: Duration::ZERO,
            panic_on_start: false,
            tool_call: None,
            web_sources: Vec::new(),
        }
    }

    pub fn with_chunk_delay(mut self, delay: Duration) -> Self {
        self.chunk_delay = delay;
        self
    }

    pub fn with_tool_call(mut self, name: impl Into<String>, arguments: Value) -> Self {
        self.tool_call = Some((name.into(), arguments));
        self
    }

    pub fn with_web_search(mut self, sources: Vec<SourceLink>) -> Self {
        self.web_sources = sources;
        self
    }

    #[cfg(test)]
    pub(crate) fn panicking(response: impl Into<String>) -> Self {
        Self {
            response: response.into(),
            chunk_delay: Duration::ZERO,
            panic_on_start: true,
            tool_call: None,
            web_sources: Vec::new(),
        }
    }
}

impl Default for DeterministicMockProvider {
    fn default() -> Self {
        Self::new("Deterministic mock response.")
    }
}

impl AiProvider for DeterministicMockProvider {
    fn start(
        &self,
        request: AiRequest,
        cancellation: Cancellation,
    ) -> Result<ProviderStream, AiError> {
        assert!(!self.panic_on_start, "requested mock provider panic");
        let prompt_tokens = request.messages.len() as u64;
        let tool_call = request
            .tool_history
            .is_empty()
            .then(|| self.tool_call.clone())
            .flatten()
            .map(|(name, arguments)| AiToolCall {
                call_id: format!("mock-tool-{}", request.attempt_id),
                name,
                arguments,
                thought_signatures: Vec::new(),
            });
        let completion_tokens = if tool_call.is_some() {
            0
        } else {
            self.response.chars().count() as u64
        };
        Ok(Box::new(MockStream {
            attempt_id: request.attempt_id,
            model: request.model,
            reasoning: request.reasoning,
            response_id: format!("mock-{}", request.attempt_id),
            chunks: self.response.chars().map(String::from).collect(),
            chunk_delay: self.chunk_delay,
            cancellation,
            prompt_tokens,
            completion_tokens,
            tool_call,
            web_sources: request
                .options
                .web_search
                .then(|| self.web_sources.clone().into())
                .unwrap_or_default(),
            stage: Stage::Started,
            chunk: 0,
        }))
    }
}

struct MockStream {
    attempt_id: koru_ai_core::InferenceAttemptId,
    model: String,
    reasoning: koru_ai_core::ReasoningChoice,
    response_id: String,
    chunks: Vec<String>,
    chunk_delay: Duration,
    cancellation: Cancellation,
    prompt_tokens: u64,
    completion_tokens: u64,
    tool_call: Option<AiToolCall>,
    web_sources: VecDeque<SourceLink>,
    stage: Stage,
    chunk: usize,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Stage {
    Started,
    WebStart,
    WebCitation,
    WebComplete,
    Text,
    Usage,
    Completed,
    Done,
}

impl Iterator for MockStream {
    type Item = AiEvent;

    fn next(&mut self) -> Option<Self::Item> {
        if self.stage == Stage::Done {
            return None;
        }
        if self.cancellation.is_cancelled() {
            self.stage = Stage::Done;
            return Some(AiEvent::Cancelled {
                attempt_id: self.attempt_id,
                at: Timestamp::now(),
            });
        }
        if self.stage != Stage::Started && !self.chunk_delay.is_zero() {
            std::thread::sleep(self.chunk_delay);
            if self.cancellation.is_cancelled() {
                self.stage = Stage::Done;
                return Some(AiEvent::Cancelled {
                    attempt_id: self.attempt_id,
                    at: Timestamp::now(),
                });
            }
        }

        match self.stage {
            Stage::Started => {
                self.stage = if self.tool_call.is_some() {
                    Stage::Usage
                } else {
                    Stage::Text
                };
                Some(AiEvent::Started {
                    attempt_id: self.attempt_id,
                    provider_response_id: Some(self.response_id.clone()),
                    request_payload_bytes: None,
                    resolved_model: Some(self.model.clone()),
                    effective_reasoning: Some(self.reasoning),
                    at: Timestamp::now(),
                })
            }
            Stage::WebStart => {
                self.stage = Stage::WebCitation;
                Some(AiEvent::WebSearch {
                    attempt_id: self.attempt_id,
                    call_id: format!("mock-search-{}", self.attempt_id),
                    phase: WebSearchPhase::Started,
                    at: Timestamp::now(),
                })
            }
            Stage::WebCitation => {
                let source = self.web_sources.pop_front().expect("search source exists");
                self.stage = if self.web_sources.is_empty() {
                    Stage::WebComplete
                } else {
                    Stage::WebCitation
                };
                Some(AiEvent::Citation {
                    attempt_id: self.attempt_id,
                    source,
                    at: Timestamp::now(),
                })
            }
            Stage::WebComplete => {
                self.stage = if self.tool_call.is_some() {
                    Stage::Usage
                } else if self.chunks.is_empty() {
                    Stage::Usage
                } else {
                    Stage::Text
                };
                Some(AiEvent::WebSearch {
                    attempt_id: self.attempt_id,
                    call_id: format!("mock-search-{}", self.attempt_id),
                    phase: WebSearchPhase::Completed,
                    at: Timestamp::now(),
                })
            }
            Stage::Text if self.chunk < self.chunks.len() => {
                let text = self.chunks[self.chunk].clone();
                self.chunk += 1;
                Some(AiEvent::TextDelta {
                    attempt_id: self.attempt_id,
                    text,
                    at: Timestamp::now(),
                })
            }
            Stage::Text => {
                self.stage = if self.web_sources.is_empty() {
                    Stage::Usage
                } else {
                    Stage::WebStart
                };
                self.next()
            }
            Stage::Usage => {
                self.stage = Stage::Completed;
                Some(AiEvent::Usage {
                    attempt_id: self.attempt_id,
                    usage: AiUsage {
                        prompt_tokens: Some(self.prompt_tokens),
                        cached_prompt_tokens: Some(0),
                        completion_tokens: Some(self.completion_tokens),
                        reasoning_tokens: None,
                        total_tokens: Some(self.prompt_tokens + self.completion_tokens),
                        source: UsageSource::ProviderReported,
                    },
                })
            }
            Stage::Completed => {
                self.stage = Stage::Done;
                Some(match self.tool_call.take() {
                    Some(call) => AiEvent::ToolCalls {
                        attempt_id: self.attempt_id,
                        calls: vec![call],
                        stop_reason: Some("tool_calls".into()),
                        at: Timestamp::now(),
                    },
                    None => AiEvent::Completed {
                        attempt_id: self.attempt_id,
                        stop_reason: Some("stop".into()),
                        at: Timestamp::now(),
                    },
                })
            }
            Stage::Done => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use koru_ai_core::{
        AiProviderId, AiRequestMessage, AiRequestOptions, ChatRole, InferenceAttemptId,
        ReasoningChoice,
    };

    use super::*;

    fn request() -> AiRequest {
        AiRequest {
            attempt_id: InferenceAttemptId::new(),
            provider: AiProviderId::parse("mock").unwrap(),
            model: "deterministic-v1".into(),
            reasoning: ReasoningChoice::Default,
            prompt_template_version: "mock-v1".into(),
            system: "Static test instruction".into(),
            messages: vec![AiRequestMessage {
                role: ChatRole::User,
                markdown: "Question".into(),
                attachments: Vec::new(),
            }],
            context: Vec::new(),
            tools: Vec::new(),
            tool_history: Vec::new(),
            options: AiRequestOptions::default(),
        }
    }

    #[test]
    fn mock_stream_is_deterministic_and_provider_reported() {
        let events: Vec<_> = DeterministicMockProvider::new("abc")
            .start(request(), Cancellation::default())
            .unwrap()
            .collect();
        assert!(matches!(events[0], AiEvent::Started { .. }));
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match event {
                    AiEvent::TextDelta { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>(),
            "abc"
        );
        assert!(matches!(
            events[4],
            AiEvent::Usage {
                usage: AiUsage {
                    source: UsageSource::ProviderReported,
                    ..
                },
                ..
            }
        ));
        assert!(matches!(events[5], AiEvent::Completed { .. }));
    }

    #[test]
    fn cancellation_is_observed_before_more_text() {
        let cancellation = Cancellation::default();
        let mut stream = DeterministicMockProvider::new("abc")
            .start(request(), cancellation.clone())
            .unwrap();
        assert!(matches!(stream.next(), Some(AiEvent::Started { .. })));
        cancellation.cancel();
        assert!(matches!(stream.next(), Some(AiEvent::Cancelled { .. })));
        assert!(stream.next().is_none());
    }

    #[test]
    #[should_panic(expected = "requested mock provider panic")]
    fn panic_mode_exercises_executor_containment() {
        let _ =
            DeterministicMockProvider::panicking("never").start(request(), Cancellation::default());
    }
}
