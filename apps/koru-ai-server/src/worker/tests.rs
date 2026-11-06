// SPDX-License-Identifier: MPL-2.0

use std::sync::Arc;
use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use koru_ai_api::{AiPrincipal, AiScope, AiServerPort, RequestContext};
use koru_ai_core::{
    AiError, AiErrorClass, AiProvider, AiProviderId, AiRequest, AiSurface, AiToolCall,
    AiToolDefinition, AiToolExecutionContext, AiToolExecutor, AiToolResult, AiToolSafety,
    AttemptStatus, Cancellation, ChatId, ChatRole, ContextItem, ContextRef, DocumentContext,
    DocumentDestination, DocumentId, FeedbackEventId, FeedbackValue, InferenceAttemptId,
    MessageState, ModelSelection, ProviderStream, ReasoningChoice, RecoveryCounts,
    StoreContextPort, StoreRevision, ToolActivityStatus, ToolApproval, ToolMode, WorkspaceId,
};
use koru_ai_provider::{
    CredentialStore, DeterministicMockProvider, ProviderRegistry, SecretString,
    SessionCredentialStore,
};
use koru_ai_wire::{
    BranchChatRequest, CreateChatRequest, FeedbackRequest, PublicAiEvent, RenameChatRequest,
    RetryTurnRequest, SelectProviderRequest, SendTurnRequest, UpdateChatPrivacyRequest,
    UpdateChatToolPolicyRequest, UpdateOllamaSettingsRequest, UpdateOpenAiSettingsRequest,
};
use serde_json::json;
use uuid::Uuid;

use super::*;

fn context() -> RequestContext {
    RequestContext {
        principal: AiPrincipal {
            label: "worker-test".into(),
            scopes: vec![AiScope::Chat],
        },
        trace_id: "trace-worker-test".into(),
        deadline_millis: None,
    }
}

fn provider_context() -> RequestContext {
    RequestContext {
        principal: AiPrincipal {
            label: "worker-provider-test".into(),
            scopes: vec![AiScope::ManageProviders],
        },
        trace_id: "trace-worker-provider-test".into(),
        deadline_millis: None,
    }
}

fn usage_context() -> RequestContext {
    RequestContext {
        principal: AiPrincipal {
            label: "worker-usage-test".into(),
            scopes: vec![AiScope::ReadUsage],
        },
        trace_id: "trace-worker-usage-test".into(),
        deadline_millis: None,
    }
}

fn model() -> koru_ai_core::ModelSelection {
    koru_ai_core::ModelSelection {
        provider: AiProviderId::parse("mock").unwrap(),
        requested_model: "deterministic-v1".into(),
        default_reasoning: ReasoningChoice::Default,
    }
}

fn create_chat(client: &AiWorkerClient) -> ChatId {
    let chat_id = ChatId::new();
    client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id,
                title: "Worker test".into(),
                profile_id: None,
                model: model(),
            },
        )
        .unwrap();
    chat_id
}

fn send(client: &AiWorkerClient, chat_id: ChatId, operation_id: Uuid) -> AiResponseStream {
    client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id,
                chat_id,
                surface: AiSurface::Standalone,
                markdown: "Question".into(),
                explicit_refs: Vec::new(),
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
                attachment_ids: Vec::new(),
                answers: None,
                skills: Vec::new(),
            },
        )
        .unwrap()
}

#[derive(Clone)]
struct CapturingProvider {
    requests: std::sync::mpsc::SyncSender<AiRequest>,
}

#[derive(Clone)]
struct WebSearchProvider {
    requests: std::sync::mpsc::SyncSender<AiRequest>,
}

impl AiProvider for WebSearchProvider {
    fn start(
        &self,
        request: AiRequest,
        _cancellation: Cancellation,
    ) -> Result<ProviderStream, AiError> {
        let attempt_id = request.attempt_id;
        self.requests.send(request).unwrap();
        let source = koru_ai_core::SourceLink {
            url: "https://example.com/source".into(),
            title: "Example source".into(),
        };
        Ok(Box::new(
            vec![
                AiEvent::Started {
                    attempt_id,
                    provider_response_id: Some("response-search".into()),
                    request_payload_bytes: Some(321),
                    resolved_model: Some("gpt-5.4".into()),
                    effective_reasoning: None,
                    at: Timestamp::now(),
                },
                AiEvent::WebSearch {
                    attempt_id,
                    call_id: "search-1".into(),
                    phase: koru_ai_core::WebSearchPhase::Started,
                    at: Timestamp::now(),
                },
                AiEvent::TextDelta {
                    attempt_id,
                    text: "Current answer".into(),
                    at: Timestamp::now(),
                },
                AiEvent::Citation {
                    attempt_id,
                    source: source.clone(),
                    at: Timestamp::now(),
                },
                AiEvent::Citation {
                    attempt_id,
                    source,
                    at: Timestamp::now(),
                },
                AiEvent::WebSearch {
                    attempt_id,
                    call_id: "search-1".into(),
                    phase: koru_ai_core::WebSearchPhase::Completed,
                    at: Timestamp::now(),
                },
                AiEvent::Usage {
                    attempt_id,
                    usage: koru_ai_core::AiUsage {
                        prompt_tokens: Some(10),
                        cached_prompt_tokens: None,
                        completion_tokens: Some(2),
                        reasoning_tokens: None,
                        total_tokens: Some(12),
                        source: koru_ai_core::UsageSource::ProviderReported,
                    },
                },
                AiEvent::Completed {
                    attempt_id,
                    stop_reason: Some("stop".into()),
                    at: Timestamp::now(),
                },
            ]
            .into_iter(),
        ))
    }
}

impl AiProvider for CapturingProvider {
    fn start(
        &self,
        request: AiRequest,
        _cancellation: Cancellation,
    ) -> Result<ProviderStream, AiError> {
        let attempt_id = request.attempt_id;
        self.requests.send(request).unwrap();
        Ok(Box::new(
            vec![
                AiEvent::Started {
                    attempt_id,
                    provider_response_id: None,
                    request_payload_bytes: None,
                    resolved_model: Some("capture-v1".into()),
                    effective_reasoning: None,
                    at: Timestamp::now(),
                },
                AiEvent::TextDelta {
                    attempt_id,
                    text: "Reusable answer.".into(),
                    at: Timestamp::now(),
                },
                AiEvent::Completed {
                    attempt_id,
                    stop_reason: Some("stop".into()),
                    at: Timestamp::now(),
                },
            ]
            .into_iter(),
        ))
    }
}

struct FixedStoreContext {
    document: DocumentId,
    failure: bool,
}

#[derive(Default)]
struct FixedToolExecutor;

impl AiToolExecutor for FixedToolExecutor {
    fn definitions(&self) -> Vec<AiToolDefinition> {
        vec![AiToolDefinition {
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
        }]
    }

    fn invoke(
        &self,
        _context: &AiToolExecutionContext,
        call: &AiToolCall,
        _cancellation: &Cancellation,
    ) -> Result<AiToolResult, AiError> {
        Ok(AiToolResult {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            content: json!({ "title": "Roadmap", "markdown": "# Roadmap" }),
            is_error: false,
        })
    }
}

#[derive(Default)]
struct MutationToolExecutor;

impl AiToolExecutor for MutationToolExecutor {
    fn definitions(&self) -> Vec<AiToolDefinition> {
        vec![AiToolDefinition {
            name: "update_document".into(),
            title: "Update document".into(),
            description: "Apply one semantic document update".into(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "object" }),
            safety: AiToolSafety {
                read_only: false,
                destructive: false,
                idempotent: false,
                open_world: false,
            },
        }]
    }

    fn invoke(
        &self,
        _context: &AiToolExecutionContext,
        call: &AiToolCall,
        _cancellation: &Cancellation,
    ) -> Result<AiToolResult, AiError> {
        Ok(AiToolResult {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            content: json!({ "updated": true }),
            is_error: false,
        })
    }
}

impl StoreContextPort for FixedStoreContext {
    fn resolve_document(
        &self,
        document: DocumentId,
        _reader: Option<koru_ai_core::ProfileId>,
    ) -> Result<DocumentContext, AiError> {
        if self.failure || document != self.document {
            return Err(AiError::new(
                AiErrorClass::Storage,
                "The selected document is unavailable.",
                false,
            ));
        }
        Ok(DocumentContext {
            document_id: document,
            display_title: "Roadmap".into(),
            source_revision: StoreRevision(vec![7, 8, 9]),
            markdown: "# Roadmap\n\nShip context.\n".into(),
        })
    }

    fn create_document(
        &self,
        _idempotency_key: Uuid,
        _destination: &DocumentDestination,
        _markdown: &str,
    ) -> Result<DocumentId, AiError> {
        unreachable!("not exercised by chat context tests")
    }
}

struct MultiStoreContext {
    documents: Vec<(DocumentId, &'static str, &'static str)>,
}

impl StoreContextPort for MultiStoreContext {
    fn resolve_document(
        &self,
        document: DocumentId,
        _reader: Option<koru_ai_core::ProfileId>,
    ) -> Result<DocumentContext, AiError> {
        let (_, title, markdown) = self
            .documents
            .iter()
            .find(|(id, _, _)| *id == document)
            .ok_or_else(|| {
                AiError::new(
                    AiErrorClass::Storage,
                    "The selected document is unavailable.",
                    false,
                )
            })?;
        Ok(DocumentContext {
            document_id: document,
            display_title: (*title).into(),
            source_revision: StoreRevision(vec![1, 2, 3]),
            markdown: (*markdown).into(),
        })
    }

    fn create_document(
        &self,
        _idempotency_key: Uuid,
        _destination: &DocumentDestination,
        _markdown: &str,
    ) -> Result<DocumentId, AiError> {
        unreachable!("not exercised by chat context tests")
    }
}

fn wait_for_terminal(client: &AiWorkerClient, attempt_id: InferenceAttemptId) -> AttemptStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = client
            .read_attempt(context(), attempt_id)
            .unwrap()
            .attempt
            .status;
        if status.is_terminal() {
            return status;
        }
        assert!(Instant::now() < deadline, "attempt never became terminal");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn worker_reports_readiness_only_after_schema_and_recovery_are_ready() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();

    let info = worker.info().unwrap();
    assert_eq!(info.api_version, AI_WIRE_VERSION);
    assert_eq!(info.schema_version, koru_ai_sqlite::SCHEMA_VERSION);
    assert_eq!(info.providers, vec!["mock"]);
    assert!(info.capabilities.contains(&"ai.chat".into()));
    assert_eq!(info.recovery, RecoveryCounts::default());
    assert_eq!(worker.status(), WorkerStatus::Ready(info));
    assert!(directory.path().join("ai.db").exists());
    worker.ping().unwrap();

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn ready_worker_without_a_provider_advertises_no_ai_capability() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        ProviderRegistry::default(),
    ))
    .spawn()
    .unwrap();

    let info = worker.info().unwrap();
    assert!(info.providers.is_empty());
    assert!(info.capabilities.is_empty());

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn client_minted_chat_identity_is_retry_safe_but_not_reusable() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let request = CreateChatRequest {
        chat_id: ChatId::new(),
        title: "Retry-safe".into(),
        profile_id: None,
        model: model(),
    };
    let first = client.create_chat(context(), request.clone()).unwrap();
    assert_eq!(client.create_chat(context(), request).unwrap(), first);

    let conflict = client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id: first.chat.id,
                title: "Different".into(),
                profile_id: None,
                model: model(),
            },
        )
        .unwrap_err();
    assert_eq!(conflict.code, "idempotency_conflict");

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn chat_titles_are_renamed_durably_through_the_worker() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat = create_chat(&client);

    let renamed = client
        .rename_chat(
            context(),
            chat,
            RenameChatRequest {
                title: "Renamed chat".into(),
            },
        )
        .unwrap();
    assert_eq!(renamed.chat.title, "Renamed chat");
    assert_eq!(
        client.read_chat(context(), chat).unwrap().chat,
        renamed.chat
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn chat_models_are_changed_durably_through_the_worker() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat = create_chat(&client);
    let selected = ModelSelection {
        provider: AiProviderId::parse("mock").unwrap(),
        requested_model: "deterministic-mock".into(),
        default_reasoning: ReasoningChoice::Default,
    };

    let updated = client
        .update_chat_model(
            context(),
            chat,
            UpdateChatModelRequest {
                model: selected.clone(),
            },
        )
        .unwrap();
    assert_eq!(updated.chat.current_model, selected);
    assert_eq!(
        client.read_chat(context(), chat).unwrap().chat,
        updated.chat
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn mock_provider_completes_through_bounded_worker_and_real_database() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let operation_id = Uuid::new_v4();
    let stream = send(&client, chat_id, operation_id);
    let accepted = stream.accepted.clone();
    let mut text = String::new();
    let mut text_events = 0;
    let mut last_sequence = 0;
    loop {
        let public = stream.events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(public.version, AI_STREAM_VERSION);
        assert!(public.sequence > last_sequence);
        last_sequence = public.sequence;
        match public.event {
            PublicAiEvent::TextDelta { text: delta, .. } => {
                text_events += 1;
                text.push_str(&delta);
            }
            event if event.is_terminal() => break,
            _ => {}
        }
    }
    assert_eq!(text, "Deterministic mock response.");
    assert!(
        text_events < text.chars().count(),
        "provider character chunks were not coalesced"
    );
    assert_eq!(
        client
            .read_attempt(context(), accepted.attempt_id)
            .unwrap()
            .attempt
            .status,
        AttemptStatus::Complete
    );
    let transcript = client.read_chat(context(), chat_id).unwrap();
    assert_eq!(transcript.messages[1].markdown, text);
    assert_eq!(transcript.messages[1].state, MessageState::Complete);
    assert_eq!(transcript.blocks.len(), 1);
    assert_eq!(transcript.blocks[0].plain_text, text);
    assert_eq!(transcript.usage.len(), 1);
    assert_eq!(
        transcript.usage[0].message_id,
        accepted.assistant_message_id
    );
    let persisted_attempt = client
        .read_attempt(context(), accepted.attempt_id)
        .unwrap()
        .attempt;
    assert_eq!(transcript.usage[0].usage, persisted_attempt.usage);

    let mut unavailable_duplicate = persisted_attempt.clone();
    unavailable_duplicate.usage = AiUsage::default();
    let mut latest_reported_duplicate = persisted_attempt.clone();
    latest_reported_duplicate.usage.completion_tokens = Some(999);
    let resolved = transcript_usage(
        &transcript.messages,
        vec![
            persisted_attempt,
            unavailable_duplicate,
            latest_reported_duplicate.clone(),
        ],
    );
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].usage, latest_reported_duplicate.usage);

    let replay = send(&client, chat_id, operation_id);
    assert!(replay.accepted.replayed);
    assert_eq!(replay.accepted.attempt_id, accepted.attempt_id);
    assert!(replay.events.recv().is_err());

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn armed_web_search_is_audited_and_citations_reach_transcript_and_usage() {
    let directory = tempfile::tempdir().unwrap();
    let (captured, requests) = sync_channel(2);
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse(AiProviderId::OPENAI).unwrap(),
        Arc::new(WebSearchProvider { requests: captured }),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = ChatId::new();
    client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id,
                title: "Search test".into(),
                profile_id: None,
                model: ModelSelection {
                    provider: AiProviderId::parse(AiProviderId::OPENAI).unwrap(),
                    requested_model: "gpt-5.4".into(),
                    default_reasoning: ReasoningChoice::Default,
                },
            },
        )
        .unwrap();
    let stream = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id,
                surface: AiSurface::Standalone,
                markdown: "What changed?".into(),
                explicit_refs: Vec::new(),
                web_search: true,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
                attachment_ids: Vec::new(),
                answers: None,
                skills: Vec::new(),
            },
        )
        .unwrap();
    assert!(
        requests
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .options
            .web_search
    );
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );

    let attempt = client
        .read_attempt(context(), stream.accepted.attempt_id)
        .unwrap()
        .attempt;
    assert!(attempt.web_search_requested);
    assert_eq!(attempt.web_search_calls, 1);
    assert_eq!(attempt.request_payload_bytes, Some(321));
    let transcript = client.read_chat(context(), chat_id).unwrap();
    assert_eq!(transcript.sources.len(), 1);
    assert_eq!(
        transcript.sources[0].sources.len(),
        1,
        "duplicate URLs are ignored"
    );
    assert_eq!(transcript.sources[0].sources[0].title, "Example source");
    let branch = client
        .branch_chat(
            context(),
            chat_id,
            BranchChatRequest {
                branch_chat_id: ChatId::new(),
                through_message_id: stream.accepted.assistant_message_id,
                title: "Search branch".into(),
            },
        )
        .unwrap()
        .transcript;
    assert_eq!(branch.sources.len(), 1);
    assert_eq!(
        branch.sources[0].sources[0].url,
        "https://example.com/source"
    );
    let usage = client.usage(usage_context(), Some(chat_id)).unwrap().report;
    assert_eq!(usage.current_chat.unwrap().web_search_calls, 1);

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn unsupported_web_search_is_rejected_before_an_attempt_or_provider_call() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let error = match client.send_turn(
        context(),
        SendTurnRequest {
            operation_id: Uuid::new_v4(),
            chat_id,
            surface: AiSurface::Standalone,
            markdown: "Search".into(),
            explicit_refs: Vec::new(),
            web_search: true,
            reasoning_override: None,
            system_prompt_override: None,
            replace_pinned_documents: false,
            attachment_ids: Vec::new(),
            answers: None,
            skills: Vec::new(),
        },
    ) {
        Ok(_) => panic!("unsupported web search unexpectedly started"),
        Err(error) => error,
    };
    assert_eq!(error.code, "invalid_request");
    assert_eq!(
        error.message,
        "Web search is not supported by the selected model."
    );
    assert!(
        client
            .read_chat(context(), chat_id)
            .unwrap()
            .messages
            .is_empty()
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn document_and_message_block_context_reach_provider_with_durable_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let document_id = DocumentId::new();
    let (captured, requests) = sync_channel(8);
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(CapturingProvider { requests: captured }),
    );
    let worker = AiWorker::new(
        WorkerConfig::new(directory.path().to_owned(), "internal-debug", providers)
            .with_store_context(Arc::new(FixedStoreContext {
                document: document_id,
                failure: false,
            })),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);

    let first = send(&client, chat_id, Uuid::new_v4());
    let first_request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(first_request.context.is_empty());
    assert_eq!(
        wait_for_terminal(&client, first.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let first_transcript = client.read_chat(context(), chat_id).unwrap();
    assert!(first_transcript.usage.is_empty());
    let source_block = first_transcript.blocks[0].id;

    let second = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id,
                surface: AiSurface::ContextPane,
                markdown: "Use both sources".into(),
                explicit_refs: vec![
                    ContextRef::Document { document_id },
                    ContextRef::MessageBlock {
                        block_id: source_block,
                    },
                ],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
                attachment_ids: Vec::new(),
                answers: None,
                skills: Vec::new(),
            },
        )
        .unwrap();
    let second_request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        second_request.context[0],
        ContextItem::Document(_)
    ));
    assert!(matches!(
        second_request.context[1],
        ContextItem::MessageBlock(_)
    ));
    assert_eq!(
        wait_for_terminal(&client, second.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let attempt = client
        .read_attempt(context(), second.accepted.attempt_id)
        .unwrap()
        .attempt;
    assert!(attempt.context_started_at.is_some());
    assert!(attempt.context_completed_at.is_some());
    assert_eq!(attempt.context_document_count, 1);
    assert_eq!(attempt.context_message_block_count, 1);
    assert!(attempt.context_payload_bytes > 0);
    let transcript = client.read_chat(context(), chat_id).unwrap();
    assert_eq!(transcript.pinned_documents.len(), 1);
    assert_eq!(transcript.pinned_documents[0].document_id, document_id);
    assert_eq!(transcript.pinned_documents[0].display_title, "Roadmap");

    let third = send(&client, chat_id, Uuid::new_v4());
    let third_request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        third_request.context.as_slice(),
        [ContextItem::Document(_)]
    ));
    assert_eq!(
        wait_for_terminal(&client, third.accepted.attempt_id),
        AttemptStatus::Complete
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn replace_pinned_documents_cleans_prior_context_for_context_pane() {
    let directory = tempfile::tempdir().unwrap();
    let first_document = DocumentId::new();
    let second_document = DocumentId::new();
    let (captured, requests) = sync_channel(8);
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(CapturingProvider { requests: captured }),
    );
    let worker = AiWorker::new(
        WorkerConfig::new(directory.path().to_owned(), "internal-debug", providers)
            .with_store_context(Arc::new(MultiStoreContext {
                documents: vec![
                    (
                        first_document,
                        "First document",
                        "# First\n\nFirst content.\n",
                    ),
                    (
                        second_document,
                        "Second document",
                        "# Second\n\nSecond content.\n",
                    ),
                ],
            })),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);

    let first = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id,
                surface: AiSurface::ContextPane,
                markdown: "Question about first".into(),
                explicit_refs: vec![ContextRef::Document {
                    document_id: first_document,
                }],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
                attachment_ids: Vec::new(),
                answers: None,
                skills: Vec::new(),
            },
        )
        .unwrap();
    let first_request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(first_request.context.len(), 1);
    assert_eq!(
        wait_for_terminal(&client, first.accepted.attempt_id),
        AttemptStatus::Complete
    );

    let second = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id,
                surface: AiSurface::ContextPane,
                markdown: "Question about second".into(),
                explicit_refs: vec![ContextRef::Document {
                    document_id: second_document,
                }],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: true,
                attachment_ids: Vec::new(),
                answers: None,
                skills: Vec::new(),
            },
        )
        .unwrap();
    let second_request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(second_request.context.len(), 1);
    assert!(matches!(
        second_request.context[0],
        ContextItem::Document(DocumentContext {
            document_id,
            ..
        }) if document_id == second_document
    ));
    assert_eq!(
        wait_for_terminal(&client, second.accepted.attempt_id),
        AttemptStatus::Complete
    );

    let transcript = client.read_chat(context(), chat_id).unwrap();
    assert_eq!(transcript.pinned_documents.len(), 1);
    assert_eq!(transcript.pinned_documents[0].document_id, second_document);

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn private_message_blocks_are_available_only_to_their_own_chat() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let source = create_chat(&client);
    let first = send(&client, source, Uuid::new_v4());
    assert_eq!(
        wait_for_terminal(&client, first.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let source_block = client.read_chat(context(), source).unwrap().blocks[0].id;

    let target = create_chat(&client);
    let public_reference = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id: target,
                surface: AiSurface::Standalone,
                markdown: "Use the public answer".into(),
                explicit_refs: vec![ContextRef::MessageBlock {
                    block_id: source_block,
                }],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
                attachment_ids: Vec::new(),
                answers: None,
                skills: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(
        wait_for_terminal(&client, public_reference.accepted.attempt_id),
        AttemptStatus::Complete
    );

    client
        .update_chat_privacy(
            context(),
            source,
            UpdateChatPrivacyRequest { private: true },
        )
        .unwrap();
    let own_reference = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id: source,
                surface: AiSurface::Standalone,
                markdown: "Use my private answer".into(),
                explicit_refs: vec![ContextRef::MessageBlock {
                    block_id: source_block,
                }],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
                attachment_ids: Vec::new(),
                answers: None,
                skills: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(
        wait_for_terminal(&client, own_reference.accepted.attempt_id),
        AttemptStatus::Complete
    );

    let foreign_reference = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id: target,
                surface: AiSurface::Standalone,
                markdown: "Try the now-private answer".into(),
                explicit_refs: vec![ContextRef::MessageBlock {
                    block_id: source_block,
                }],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
                attachment_ids: Vec::new(),
                answers: None,
                skills: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(
        wait_for_terminal(&client, foreign_reference.accepted.attempt_id),
        AttemptStatus::Failed
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn context_failure_is_durable_and_never_calls_the_provider() {
    let directory = tempfile::tempdir().unwrap();
    let document_id = DocumentId::new();
    let (captured, requests) = sync_channel(1);
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(CapturingProvider { requests: captured }),
    );
    let worker = AiWorker::new(
        WorkerConfig::new(directory.path().to_owned(), "internal-debug", providers)
            .with_store_context(Arc::new(FixedStoreContext {
                document: document_id,
                failure: true,
            })),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let response = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id,
                surface: AiSurface::Standalone,
                markdown: "Use unavailable context".into(),
                explicit_refs: vec![ContextRef::Document { document_id }],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
                attachment_ids: Vec::new(),
                answers: None,
                skills: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(
        wait_for_terminal(&client, response.accepted.attempt_id),
        AttemptStatus::Failed
    );
    assert!(requests.recv_timeout(Duration::from_millis(100)).is_err());
    let attempt = client
        .read_attempt(context(), response.accepted.attempt_id)
        .unwrap()
        .attempt;
    assert!(attempt.context_started_at.is_some());
    assert!(attempt.context_completed_at.is_some());
    assert!(
        client
            .read_chat(context(), chat_id)
            .unwrap()
            .pinned_documents
            .is_empty()
    );
    let retry = client
        .retry_turn(
            context(),
            chat_id,
            RetryTurnRequest {
                operation_id: Uuid::new_v4(),
                message_id: response.accepted.assistant_message_id,
                surface: AiSurface::Standalone,
                explicit_refs: vec![ContextRef::Document { document_id }],
                web_search: false,
                reasoning_override: None,
                replace_pinned_documents: false,
            },
        )
        .unwrap();
    assert_ne!(
        retry.accepted.assistant_message_id,
        response.accepted.assistant_message_id
    );
    assert_eq!(
        wait_for_terminal(&client, retry.accepted.attempt_id),
        AttemptStatus::Failed
    );
    let history = client.read_chat(context(), chat_id).unwrap().messages;
    assert_eq!(history.len(), 3);
    assert_eq!(history[1].state, MessageState::Failed);
    assert_eq!(history[2].origin_message_id, Some(history[1].id));

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn dropped_stream_does_not_stop_persistence_or_authoritative_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = send(&client, chat_id, Uuid::new_v4());
    let attempt_id = stream.accepted.attempt_id;
    drop(stream);

    assert_eq!(
        wait_for_terminal(&client, attempt_id),
        AttemptStatus::Complete
    );
    let transcript = client.read_chat(context(), chat_id).unwrap();
    assert_eq!(
        transcript.messages[1].markdown,
        "Deterministic mock response."
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn provider_tool_calls_continue_within_one_durable_attempt() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(
            DeterministicMockProvider::new("Answer from workspace.")
                .with_tool_call("read_document", json!({ "document_id": "document-a" })),
        ),
    );
    let worker = AiWorker::new(
        WorkerConfig::new(directory.path().to_owned(), "internal-debug", providers)
            .with_tool_executor(Arc::new(FixedToolExecutor)),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = send(&client, chat_id, Uuid::new_v4());
    let attempt_id = stream.accepted.attempt_id;

    assert_eq!(
        wait_for_terminal(&client, attempt_id),
        AttemptStatus::Complete
    );
    let transcript = client.read_chat(context(), chat_id).unwrap();
    assert_eq!(transcript.messages.len(), 2);
    assert_eq!(transcript.messages[1].markdown, "Answer from workspace.");
    assert_eq!(
        client
            .read_attempt(context(), attempt_id)
            .unwrap()
            .attempt
            .status,
        AttemptStatus::Complete
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn provider_can_read_its_own_private_chat_without_reentering_the_worker() {
    let directory = tempfile::tempdir().unwrap();
    let chat_id = ChatId::new();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(
            DeterministicMockProvider::new("Read the private chat.").with_tool_call(
                "read_chat",
                json!({ "chat_id": chat_id.to_storage(), "page_size": 20 }),
            ),
        ),
    );
    let worker = AiWorker::new(
        WorkerConfig::new(directory.path().to_owned(), "internal-debug", providers)
            .with_tool_executor(Arc::new(crate::ChatToolAdapter::new(
                directory.path().to_owned(),
            ))),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id,
                title: "Private self-read".into(),
                profile_id: None,
                model: model(),
            },
        )
        .unwrap();
    client
        .update_chat_privacy(
            context(),
            chat_id,
            UpdateChatPrivacyRequest { private: true },
        )
        .unwrap();

    let attempt_id = send(&client, chat_id, Uuid::new_v4()).accepted.attempt_id;
    assert_eq!(
        wait_for_terminal(&client, attempt_id),
        AttemptStatus::Complete
    );
    let transcript = client.read_chat(context(), chat_id).unwrap();
    assert_eq!(transcript.messages[1].markdown, "Read the private chat.");
    assert_eq!(transcript.tool_activities.len(), 1);
    assert_eq!(
        transcript.tool_activities[0].status,
        ToolActivityStatus::Complete
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn manual_mode_pauses_mutation_until_idempotent_approval() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(
            DeterministicMockProvider::new("Mutation complete.")
                .with_tool_call("update_document", json!({ "change": "safe fixture" })),
        ),
    );
    let worker = AiWorker::new(
        WorkerConfig::new(directory.path().to_owned(), "internal-debug", providers)
            .with_tool_executor(Arc::new(MutationToolExecutor)),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = send(&client, chat_id, Uuid::new_v4());
    let attempt_id = stream.accepted.attempt_id;
    let invocation = loop {
        let event = stream.events.recv_timeout(Duration::from_secs(5)).unwrap();
        if let PublicAiEvent::ToolActivity { activity, .. } = event.event
            && activity.status == ToolActivityStatus::ApprovalRequired
        {
            break activity.id;
        }
    };
    assert_eq!(
        client
            .read_attempt(context(), attempt_id)
            .unwrap()
            .attempt
            .status,
        AttemptStatus::Streaming
    );

    let approved = client
        .approve_tool_call(context(), invocation, Default::default())
        .unwrap();
    assert_eq!(approved.activity.approval, ToolApproval::Approved);
    assert_eq!(
        client
            .approve_tool_call(context(), invocation, Default::default())
            .unwrap()
            .activity
            .approval,
        ToolApproval::Approved
    );
    assert_eq!(
        wait_for_terminal(&client, attempt_id),
        AttemptStatus::Complete
    );
    let activity = &client
        .read_chat(context(), chat_id)
        .unwrap()
        .tool_activities[0];
    assert_eq!(activity.status, ToolActivityStatus::Complete);
    assert_eq!(activity.approval, ToolApproval::Approved);

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn denial_returns_permission_result_and_auto_mode_does_not_pause() {
    for (mode, deny) in [(ToolMode::Manual, true), (ToolMode::Auto, false)] {
        let directory = tempfile::tempdir().unwrap();
        let mut providers = ProviderRegistry::default();
        providers.insert(
            AiProviderId::parse("mock").unwrap(),
            Arc::new(
                DeterministicMockProvider::new("Provider continued.")
                    .with_tool_call("update_document", json!({ "change": "fixture" })),
            ),
        );
        let worker = AiWorker::new(
            WorkerConfig::new(directory.path().to_owned(), "internal-debug", providers)
                .with_tool_executor(Arc::new(MutationToolExecutor)),
        )
        .spawn()
        .unwrap();
        let client = worker.client();
        let chat_id = create_chat(&client);
        client
            .update_chat_tool_policy(
                context(),
                chat_id,
                UpdateChatToolPolicyRequest {
                    mode,
                    workspace_ids: vec![WorkspaceId::new()],
                },
            )
            .unwrap();
        let stream = send(&client, chat_id, Uuid::new_v4());
        let attempt_id = stream.accepted.attempt_id;
        if deny {
            let invocation = loop {
                let event = stream.events.recv_timeout(Duration::from_secs(5)).unwrap();
                if let PublicAiEvent::ToolActivity { activity, .. } = event.event
                    && activity.status == ToolActivityStatus::ApprovalRequired
                {
                    break activity.id;
                }
            };
            let denied = client.deny_tool_call(context(), invocation, None).unwrap();
            assert_eq!(denied.activity.status, ToolActivityStatus::Denied);
            assert_eq!(
                client
                    .deny_tool_call(context(), invocation, None)
                    .unwrap()
                    .activity
                    .status,
                ToolActivityStatus::Denied
            );
        }
        assert_eq!(
            wait_for_terminal(&client, attempt_id),
            AttemptStatus::Complete
        );
        let transcript = client.read_chat(context(), chat_id).unwrap();
        assert_eq!(transcript.messages[1].markdown, "Provider continued.");
        assert_eq!(
            transcript.tool_activities[0].status,
            if deny {
                ToolActivityStatus::Denied
            } else {
                ToolActivityStatus::Complete
            }
        );

        worker.shutdown();
        assert_eq!(worker.join(), WorkerExit::Clean);
    }
}

#[test]
fn full_stream_buffer_detaches_while_authoritative_completion_continues() {
    let directory = tempfile::tempdir().unwrap();
    let response = "x".repeat(COALESCE_BYTES * (STREAM_CAPACITY + 8));
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(DeterministicMockProvider::new(response.clone())),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = send(&client, chat_id, Uuid::new_v4());
    let attempt_id = stream.accepted.attempt_id;
    // Keep the receiver alive without draining it. The bounded buffer fills,
    // then the worker detaches this subscriber and continues durable work.
    assert_eq!(
        wait_for_terminal(&client, attempt_id),
        AttemptStatus::Complete
    );
    assert_eq!(
        client.read_chat(context(), chat_id).unwrap().messages[1].markdown,
        response
    );
    worker.ping().unwrap();
    drop(stream);

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn cancellation_race_is_idempotent_and_terminal() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        // 32 characters at 200 ms: the cancel lands while the answer is still
        // streaming, and a cancelled stream stops at the next character, so
        // the length costs nothing. At 5 ms the whole answer took 160 ms and a
        // loaded runner finished it first (Complete, not Cancelled); 50 ms
        // still lost one run in 45 on four loaded cores.
        Arc::new(
            DeterministicMockProvider::new("a response long enough to cancel")
                .with_chunk_delay(Duration::from_millis(200)),
        ),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = send(&client, chat_id, Uuid::new_v4());
    let attempt_id = stream.accepted.attempt_id;
    while !matches!(
        stream
            .events
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .event,
        PublicAiEvent::Started { .. }
    ) {}

    let first = client.cancel_attempt(context(), attempt_id).unwrap();
    let second = client.cancel_attempt(context(), attempt_id).unwrap();
    assert_eq!(first.status, AttemptStatus::Cancelled);
    assert_eq!(second, first);
    assert_eq!(
        client
            .read_attempt(context(), attempt_id)
            .unwrap()
            .attempt
            .status,
        AttemptStatus::Cancelled
    );
    assert_eq!(
        client.read_chat(context(), chat_id).unwrap().messages[1].state,
        MessageState::Interrupted
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn command_queue_is_bounded_and_shutdown_cannot_be_starved_by_it() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let (entered_tx, entered_rx) = sync_channel(1);
    let (release_tx, release_rx) = sync_channel(1);
    worker
        .sender
        .try_send(Envelope::Command(WorkerCommand::Hold {
            entered: entered_tx,
            release: release_rx,
        }))
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    for _ in 0..WORKER_COMMAND_CAPACITY {
        let (reply, _response) = sync_channel(1);
        worker
            .sender
            .try_send(Envelope::Command(WorkerCommand::Ping(reply)))
            .unwrap();
    }
    assert_eq!(worker.ping().unwrap_err(), WorkerError::Saturated);
    worker.shutdown();
    release_tx.send(()).unwrap();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn provider_queue_saturation_fails_excess_work_without_blocking_the_worker() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        // The one provider thread is held for about seven seconds, so the
        // queue behind it stays full for as long as the test needs: with
        // "slow" it was free again after two, and on a loaded runner the
        // excess attempt could arrive after that and simply be queued.
        Arc::new(
            DeterministicMockProvider::new("a slow response")
                .with_chunk_delay(Duration::from_millis(500)),
        ),
    );
    let worker = AiWorker::new(
        WorkerConfig::new(directory.path().to_owned(), "internal-debug", providers)
            .with_provider_threads(1),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    let mut attempts = Vec::new();
    for _ in 0..PROVIDER_QUEUE_CAPACITY + 2 {
        let chat_id = create_chat(&client);
        attempts.push(send(&client, chat_id, Uuid::new_v4()).accepted.attempt_id);
    }

    // Each attempt reaches the provider queue only after its context is
    // resolved on another thread, so the refusal is waited for, not read
    // the instant the last send returns.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !attempts.iter().any(|attempt_id| {
        client
            .read_attempt(context(), *attempt_id)
            .unwrap()
            .attempt
            .status
            == AttemptStatus::Failed
    }) {
        assert!(
            Instant::now() < deadline,
            "no excess attempt was refused by the full provider queue"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    worker.ping().unwrap();

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn one_command_panic_is_contained_without_stopping_the_worker() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let (entered_tx, entered_rx) = sync_channel(1);
    worker
        .sender
        .try_send(Envelope::Command(WorkerCommand::Panic(entered_tx)))
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.ping().unwrap();

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

struct PanickingProvider;

impl AiProvider for PanickingProvider {
    fn start(&self, _: AiRequest, _: Cancellation) -> Result<ProviderStream, AiError> {
        panic!("requested provider panic")
    }
}

#[test]
fn provider_panic_becomes_a_safe_failed_attempt_and_worker_survives() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(PanickingProvider),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = send(&client, chat_id, Uuid::new_v4());
    let attempt_id = stream.accepted.attempt_id;
    assert_eq!(
        wait_for_terminal(&client, attempt_id),
        AttemptStatus::Failed
    );
    let failed = client.read_attempt(context(), attempt_id).unwrap().attempt;
    assert_eq!(failed.error_class, Some(AiErrorClass::Provider));
    worker.ping().unwrap();

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn unauthorized_or_expired_internal_context_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let request = CreateChatRequest {
        chat_id: ChatId::new(),
        title: "Denied".into(),
        profile_id: None,
        model: model(),
    };
    let mut denied = context();
    denied.principal.scopes.clear();
    assert_eq!(
        client
            .create_chat(denied, request.clone())
            .unwrap_err()
            .code,
        "forbidden"
    );
    let mut expired = context();
    expired.deadline_millis = Some(0);
    assert_eq!(
        client.create_chat(expired, request).unwrap_err().code,
        "deadline_exceeded"
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn startup_failure_never_reports_ready() {
    let directory = tempfile::tempdir().unwrap();
    let root_file = directory.path().join("not-a-directory");
    std::fs::write(&root_file, b"occupied").unwrap();
    let error = AiWorker::new(WorkerConfig::deterministic_mock(
        root_file,
        "internal-debug",
    ))
    .spawn()
    .unwrap_err();
    assert!(matches!(error, WorkerError::Startup(_)));
}

#[test]
fn startup_panic_is_observed_as_failure_not_readiness() {
    let directory = tempfile::tempdir().unwrap();
    let mut config =
        WorkerConfig::deterministic_mock(directory.path().to_owned(), "internal-debug");
    config.panic_after_open = true;
    assert_eq!(
        AiWorker::new(config).spawn().unwrap_err(),
        WorkerError::Startup("worker panicked".into())
    );
}

#[test]
fn shutdown_cancels_active_attempt_before_database_close() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(
            DeterministicMockProvider::new("slow response")
                .with_chunk_delay(Duration::from_millis(20)),
        ),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = send(&client, chat_id, Uuid::new_v4());
    let attempt_id = stream.accepted.attempt_id;
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);

    let repository = Repository::open(&directory.path().join("ai.db")).unwrap();
    let attempt = repository.attempt(attempt_id).unwrap().unwrap();
    assert_eq!(attempt.status, AttemptStatus::Cancelled, "{attempt:?}");
}

#[test]
fn provider_settings_are_scoped_persisted_and_rehydrated() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_owned();
    let credentials = Arc::new(SessionCredentialStore::default());
    let config = WorkerConfig::production_with_credentials(
        root.clone(),
        "internal-debug",
        credentials,
        "koru-internal-debug",
    )
    .unwrap();
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();

    let forbidden = client.provider_settings(context()).unwrap_err();
    assert_eq!(forbidden.code, "forbidden");

    let defaults = client.provider_settings(provider_context()).unwrap();
    assert_eq!(defaults.selected_provider, None);
    assert_eq!(defaults.ollama.endpoint, "http://localhost:11434/");
    assert!(!defaults.openai.credential_configured);

    let ollama = client
        .update_ollama_settings(
            provider_context(),
            UpdateOllamaSettingsRequest {
                endpoint: "http://127.0.0.1:11434".into(),
                model: Some("gpt-oss:20b".into()),
                reasoning: ReasoningChoice::Level(koru_ai_core::ReasoningLevel::High),
            },
        )
        .unwrap();
    assert_eq!(ollama.endpoint, "http://127.0.0.1:11434/");
    assert_eq!(ollama.model.as_deref(), Some("gpt-oss:20b"));

    let openai = client
        .update_openai_settings(
            provider_context(),
            UpdateOpenAiSettingsRequest {
                model: Some("gpt-5.6".into()),
                reasoning: ReasoningChoice::Level(koru_ai_core::ReasoningLevel::Medium),
            },
        )
        .unwrap();
    assert_eq!(openai.model.as_deref(), Some("gpt-5.6"));
    assert!(!openai.credential_configured);

    let selected = client
        .select_provider(
            provider_context(),
            SelectProviderRequest {
                provider: AiProviderId::parse("ollama").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(
        selected
            .selected_provider
            .as_ref()
            .map(AiProviderId::as_str),
        Some("ollama")
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);

    let credentials = Arc::new(SessionCredentialStore::default());
    let worker = AiWorker::new(
        WorkerConfig::production_with_credentials(
            root,
            "internal-debug",
            credentials,
            "koru-internal-debug",
        )
        .unwrap(),
    )
    .spawn()
    .unwrap();
    let saved = worker
        .client()
        .provider_settings(provider_context())
        .unwrap();
    assert_eq!(
        saved.selected_provider.as_ref().map(AiProviderId::as_str),
        Some("ollama")
    );
    assert_eq!(saved.openai.model.as_deref(), Some("gpt-5.6"));
    assert_eq!(saved.ollama.model.as_deref(), Some("gpt-oss:20b"));
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn select_provider_accepts_koru_and_rejects_unknown_tokens() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let credentials = Arc::new(SessionCredentialStore::default());
    let worker = AiWorker::new(
        WorkerConfig::production_with_credentials(
            root,
            "internal-debug",
            credentials,
            "koru-internal-debug",
        )
        .unwrap(),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    let selected = client
        .select_provider(
            provider_context(),
            SelectProviderRequest {
                provider: AiProviderId::parse("koru").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(
        selected
            .selected_provider
            .as_ref()
            .map(AiProviderId::as_str),
        Some("koru")
    );
    let rejected = client
        .select_provider(
            provider_context(),
            SelectProviderRequest {
                provider: AiProviderId::parse("anthropic").unwrap(),
            },
        )
        .unwrap_err();
    assert_eq!(rejected.code, "invalid_request");
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn openai_settings_record_presence_only_after_the_host_saved_the_vault_secret() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_owned();
    let credentials = Arc::new(SessionCredentialStore::default());
    credentials
        .save_openai(
            "koru-internal-debug",
            SecretString::new("sk-test-never-persist").unwrap(),
        )
        .unwrap();
    let worker = AiWorker::new(
        WorkerConfig::production_with_credentials(
            root.clone(),
            "internal-debug",
            credentials.clone(),
            "koru-internal-debug",
        )
        .unwrap(),
    )
    .spawn()
    .unwrap();

    let settings = worker
        .client()
        .update_openai_settings(
            provider_context(),
            UpdateOpenAiSettingsRequest {
                model: Some("gpt-5.6".into()),
                reasoning: ReasoningChoice::Default,
            },
        )
        .unwrap();
    assert!(settings.credential_configured);
    let persisted = worker
        .client()
        .provider_settings(provider_context())
        .unwrap();
    assert!(persisted.openai.credential_configured);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);

    let repository = Repository::open(&root.join("ai.db")).unwrap();
    assert_eq!(
        repository.settings().unwrap().unwrap().openai.key_account,
        None
    );
    drop(repository);

    let worker = AiWorker::new(
        WorkerConfig::production_with_credentials(
            root,
            "internal-debug",
            credentials,
            "koru-internal-debug",
        )
        .unwrap(),
    )
    .spawn()
    .unwrap();
    let rehydrated = worker
        .client()
        .provider_settings(provider_context())
        .unwrap();
    assert!(rehydrated.openai.credential_configured);
    assert_eq!(rehydrated.openai.model.as_deref(), Some("gpt-5.6"));
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn chat_listing_is_scoped_and_returns_durable_rows() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat = create_chat(&client);
    assert_eq!(
        client.list_chats(context(), None).unwrap().chats[0].id,
        chat
    );
    assert_eq!(
        client
            .list_chats(provider_context(), None)
            .unwrap_err()
            .code,
        "forbidden"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn completed_responses_support_toggle_feedback_and_identity_preserving_branches() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat = create_chat(&client);
    let stream = send(&client, chat, Uuid::new_v4());
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let transcript = client.read_chat(context(), chat).unwrap();
    let response = transcript
        .messages
        .iter()
        .find(|message| message.role == ChatRole::Assistant)
        .unwrap();
    assert!(!transcript.blocks.is_empty());
    client
        .update_chat_privacy(context(), chat, UpdateChatPrivacyRequest { private: true })
        .unwrap();

    client
        .feedback(
            context(),
            response.id,
            FeedbackRequest {
                event_id: FeedbackEventId::new(),
                value: FeedbackValue::Good,
                surface: AiSurface::Standalone,
            },
        )
        .unwrap();
    assert_eq!(
        client.read_chat(context(), chat).unwrap().feedback[0].value,
        FeedbackValue::Good
    );

    let branch = ChatId::new();
    let branched = client
        .branch_chat(
            context(),
            chat,
            BranchChatRequest {
                branch_chat_id: branch,
                through_message_id: response.id,
                title: "Worker test — branch".into(),
            },
        )
        .unwrap()
        .transcript;
    assert_eq!(branched.chat.id, branch);
    assert!(branched.chat.private);
    assert_eq!(branched.messages.len(), 2);
    assert!(
        branched
            .messages
            .iter()
            .all(|message| message.origin_message_id.is_some())
    );
    assert_ne!(branched.messages[1].id, response.id);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn usage_is_separately_scoped_and_history_deletion_anonymizes_before_clear() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat = create_chat(&client);
    let stream = send(&client, chat, Uuid::new_v4());
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let transcript = client.read_chat(context(), chat).unwrap();
    let response = transcript
        .messages
        .iter()
        .find(|message| message.role == ChatRole::Assistant)
        .unwrap();
    client
        .feedback(
            context(),
            response.id,
            FeedbackRequest {
                event_id: FeedbackEventId::new(),
                value: FeedbackValue::Good,
                surface: AiSurface::Standalone,
            },
        )
        .unwrap();

    assert_eq!(
        client.usage(context(), Some(chat)).unwrap_err().code,
        "forbidden"
    );
    let before = client.usage(usage_context(), Some(chat)).unwrap().report;
    assert_eq!(before.current_chat.unwrap().attempts, 1);
    assert_eq!(before.groups[0].totals.good_feedback, 1);

    assert!(client.delete_chat(context(), chat).unwrap().deleted);
    assert!(client.list_chats(context(), None).unwrap().chats.is_empty());
    let retained = client.usage(usage_context(), None).unwrap().report;
    assert_eq!(retained.groups.len(), 1);
    assert_eq!(retained.groups[0].totals.good_feedback, 1);

    let cleared = client.clear_usage(usage_context()).unwrap();
    assert_eq!(cleared.attempts_deleted, 1);
    assert_eq!(cleared.feedback_events_deleted, 1);
    assert!(
        client
            .usage(usage_context(), None)
            .unwrap()
            .report
            .groups
            .is_empty()
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn clear_history_keeps_usage_and_is_idempotent() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let first = create_chat(&client);
    let stream = send(&client, first, Uuid::new_v4());
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    create_chat(&client);
    assert_eq!(client.clear_history(context()).unwrap().chats_deleted, 2);
    assert_eq!(client.clear_history(context()).unwrap().chats_deleted, 0);
    assert_eq!(
        client
            .usage(usage_context(), None)
            .unwrap()
            .report
            .groups
            .len(),
        1
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn debug_lifecycle_logs_exclude_chat_content_titles_and_paths() {
    const TITLE: &str = "PRIVATE-TITLE-SENTINEL";
    const CONTENT: &str = "PRIVATE-PROMPT-RESPONSE-SECRET-SENTINEL";
    const PATH_PART: &str = "PRIVATE-PATH-SENTINEL";

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join(PATH_PART);
    std::fs::create_dir_all(&root).unwrap();
    let logging = koru_log::init_file(
        &root,
        "koru-ai-safety.log",
        "koru_ai=debug,koru_ai_server=debug,koru_ai_sqlite=debug",
    );
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        root.clone(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = ChatId::new();
    client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id,
                title: TITLE.into(),
                profile_id: None,
                model: model(),
            },
        )
        .unwrap();
    let stream = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id,
                surface: AiSurface::Standalone,
                markdown: CONTENT.into(),
                explicit_refs: Vec::new(),
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
                attachment_ids: Vec::new(),
                answers: None,
                skills: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    drop(logging);

    let logs = std::fs::read_dir(root.join(koru_log::LOG_DIR))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(!logs.contains(TITLE), "chat title reached lifecycle logs");
    assert!(
        !logs.contains(CONTENT),
        "chat content reached lifecycle logs"
    );
    assert!(
        !logs.contains(PATH_PART),
        "filesystem path reached lifecycle logs"
    );
    assert!(logs.contains("AI attempt timing summary"));
    assert!(logs.contains("AI provider stream coalescing summary"));
}

#[test]
fn payload_debug_logs_one_marked_request_and_terminal_response() {
    const CHILD: &str = koru_env::name::KORU_AI_PAYLOAD_TEST_CHILD;
    const CONTENT: &str = "PRIVATE-PAYLOAD-DEBUG-SENTINEL";
    const RESPONSE: &str = "Deterministic mock response.";

    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("worker::tests::payload_debug_logs_one_marked_request_and_terminal_response")
            .arg("--exact")
            .arg("--nocapture")
            .env(CHILD, "1")
            .env(koru_env::name::KORU_AI_PAYLOAD_DEBUG, "1")
            .output()
            .expect("run isolated payload-debug test");
        assert!(
            output.status.success(),
            "isolated payload-debug test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    assert!(koru_env::switch(koru_env::name::KORU_AI_PAYLOAD_DEBUG));
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("payload-debug");
    std::fs::create_dir_all(&root).unwrap();
    let logging = koru_log::init_file(
        &root,
        "koru-ai-payload-test.log",
        "koru_ai=debug,koru_ai_server=debug,koru_ai_sqlite=debug",
    );
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        root.clone(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id,
                surface: AiSurface::Standalone,
                markdown: CONTENT.into(),
                explicit_refs: Vec::new(),
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
                attachment_ids: Vec::new(),
                answers: None,
                skills: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    drop(logging);

    let logs = std::fs::read_dir(root.join(koru_log::LOG_DIR))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    let requests = logs
        .lines()
        .filter(|line| line.contains("KORU_AI_PAYLOAD:"))
        .collect::<Vec<_>>();
    let responses = logs
        .lines()
        .filter(|line| line.contains("KORU_AI_RESPONSE:"))
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 1, "request payload records: {requests:?}");
    assert_eq!(
        responses.len(),
        1,
        "response payload records: {responses:?}"
    );
    assert!(requests[0].contains(CONTENT));
    assert!(responses[0].contains(RESPONSE));
    assert!(
        logs.lines()
            .filter(|line| line.contains(CONTENT))
            .all(|line| { line.contains("KORU_AI_PAYLOAD:") })
    );
    assert!(
        logs.lines()
            .filter(|line| line.contains(RESPONSE))
            .all(|line| { line.contains("KORU_AI_RESPONSE:") })
    );
}

#[test]
fn provider_management_is_unavailable_for_test_only_registries() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let error = worker
        .client()
        .provider_settings(provider_context())
        .unwrap_err();
    assert_eq!(error.code, "ai_provider_management_unavailable");
    let error = worker
        .client()
        .update_ollama_settings(
            provider_context(),
            UpdateOllamaSettingsRequest {
                endpoint: "http://localhost:11434".into(),
                model: None,
                reasoning: ReasoningChoice::Default,
            },
        )
        .unwrap_err();
    assert_eq!(error.code, "ai_provider_management_unavailable");
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn ollama_endpoint_validation_is_a_public_request_error() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(
        WorkerConfig::production_with_credentials(
            directory.path().to_owned(),
            "internal-debug",
            Arc::new(SessionCredentialStore::default()),
            "koru-internal-debug",
        )
        .unwrap(),
    )
    .spawn()
    .unwrap();
    for endpoint in [
        "ftp://localhost:11434",
        "http://user@localhost:11434",
        "http://localhost:11434?secret=no",
        "http://localhost:11434/#fragment",
    ] {
        let error = worker
            .client()
            .update_ollama_settings(
                provider_context(),
                UpdateOllamaSettingsRequest {
                    endpoint: endpoint.into(),
                    model: None,
                    reasoning: ReasoningChoice::Default,
                },
            )
            .unwrap_err();
        assert_eq!(error.code, "invalid_request", "{endpoint}");
    }
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn ollama_pull_that_ends_without_success_gets_a_terminal_failure() {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let _ = socket.read(&mut request).unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    });
    let mut settings = AiSettings::unconfigured(Timestamp::now());
    settings.ollama.endpoint = url::Url::parse(&format!("http://{address}")).unwrap();
    let runtime = ProviderRuntime::new(
        Some(settings),
        Arc::new(SessionCredentialStore::default()),
        "koru-pull-eof-test",
    )
    .unwrap();
    let (events, receiver) = sync_channel(2);
    run_model_pull(
        &runtime,
        "test-model",
        Cancellation::default(),
        &events,
        &AtomicBool::new(false),
    );
    let event = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        matches!(event, ModelPullEvent::Failed { ref failure } if failure.code == "ai_network")
    );
    assert!(receiver.try_recv().is_err());
    server.join().unwrap();
}

#[test]
fn tool_activity_extracts_only_stable_target_ids() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let call = AiToolCall {
        call_id: "call".into(),
        name: "read_document".into(),
        arguments: json!({
            "workspace_id": workspace,
            "document_id": document,
            "query": "must not be retained"
        }),
        thought_signatures: Vec::new(),
    };

    assert_eq!(
        tool_activity_targets(&call),
        (Some(workspace), Some(document))
    );
}

#[test]
fn tool_failure_summary_is_bounded_and_public() {
    let result = AiToolResult {
        call_id: "call".into(),
        name: "edit_document".into(),
        content: json!({ "error": { "message": "x".repeat(600) } }),
        is_error: true,
    };

    let summary = tool_result_error_message(&result);
    assert_eq!(summary.chars().count(), 513);
    assert!(summary.ends_with('…'));
}

#[test]
fn a_working_folder_is_only_for_an_empty_agent_chat_and_rides_the_request() {
    use koru_ai_wire::UpdateChatFolderRequest;
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path().to_string_lossy().into_owned();
    let (captured, requests) = sync_channel(8);
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(DeterministicMockProvider::new("Answer.")),
    );
    providers.insert(
        AiProviderId::parse("agent-codex").unwrap(),
        Arc::new(CapturingProvider { requests: captured }),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();

    // A chat without a local agent has no folder to choose.
    let plain = create_chat(&client);
    let refused = client
        .update_chat_folder(
            context(),
            plain,
            UpdateChatFolderRequest {
                folder: Some(folder.clone()),
            },
        )
        .unwrap_err();
    assert_eq!(refused.code, "invalid_request");

    let agent = ChatId::new();
    client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id: agent,
                title: "Agent chat".into(),
                profile_id: None,
                model: koru_ai_core::ModelSelection {
                    provider: AiProviderId::parse("agent-codex").unwrap(),
                    requested_model: koru_ai_core::AGENT_DEFAULT_MODEL.into(),
                    default_reasoning: ReasoningChoice::Default,
                },
            },
        )
        .unwrap();
    // A folder that is not there is refused; an existing one is kept.
    let missing = client
        .update_chat_folder(
            context(),
            agent,
            UpdateChatFolderRequest {
                folder: Some(
                    directory
                        .path()
                        .join("nowhere")
                        .to_string_lossy()
                        .into_owned(),
                ),
            },
        )
        .unwrap_err();
    assert_eq!(missing.code, "invalid_request");
    let chosen = client
        .update_chat_folder(
            context(),
            agent,
            UpdateChatFolderRequest {
                folder: Some(folder.clone()),
            },
        )
        .unwrap();
    assert_eq!(
        chosen.chat.agent_working_folder.as_deref(),
        Some(folder.as_str())
    );
    assert_eq!(
        client
            .read_chat(context(), agent)
            .unwrap()
            .chat
            .agent_working_folder
            .as_deref(),
        Some(folder.as_str())
    );

    // The first turn carries the folder to the provider, and locks it.
    let stream = send(&client, agent, Uuid::new_v4());
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        request.options.working_folder.as_deref(),
        Some(folder.as_str())
    );
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let locked = client
        .update_chat_folder(context(), agent, UpdateChatFolderRequest { folder: None })
        .unwrap_err();
    assert_eq!(locked.code, "folder_locked");

    // A branch keeps the folder its conversation happened in.
    let branch = ChatId::new();
    client
        .branch_chat(
            context(),
            agent,
            BranchChatRequest {
                branch_chat_id: branch,
                through_message_id: stream.accepted.assistant_message_id,
                title: "Branch".into(),
            },
        )
        .unwrap();
    assert_eq!(
        client
            .read_chat(context(), branch)
            .unwrap()
            .chat
            .agent_working_folder
            .as_deref(),
        Some(folder.as_str())
    );
}

/// **The skills a message names ride its request to a local agent**
/// (koru-ai D121), by name, with its `$name` text; a chat with no local
/// agent, or a skill named twice, is refused before anything is recorded.
#[test]
fn picked_skills_ride_an_agent_message_and_nothing_else() {
    let directory = tempfile::tempdir().unwrap();
    let (captured, requests) = sync_channel(8);
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(DeterministicMockProvider::new("Answer.")),
    );
    providers.insert(
        AiProviderId::parse("agent-codex").unwrap(),
        Arc::new(CapturingProvider { requests: captured }),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let with_skills = |chat_id: ChatId, skills: &[&str]| SendTurnRequest {
        operation_id: Uuid::new_v4(),
        chat_id,
        surface: AiSurface::Standalone,
        markdown: "Use $review-pr".into(),
        explicit_refs: Vec::new(),
        web_search: false,
        reasoning_override: None,
        system_prompt_override: None,
        replace_pinned_documents: false,
        attachment_ids: Vec::new(),
        answers: None,
        skills: skills.iter().map(|name| (*name).to_owned()).collect(),
    };

    let plain = create_chat(&client);
    let refused = client
        .send_turn(context(), with_skills(plain, &["review-pr"]))
        .map(|_| ())
        .unwrap_err();
    assert_eq!(refused.code, "invalid_request", "no local agent");

    let agent = ChatId::new();
    client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id: agent,
                title: "Agent chat".into(),
                profile_id: None,
                model: koru_ai_core::ModelSelection {
                    provider: AiProviderId::parse("agent-codex").unwrap(),
                    requested_model: koru_ai_core::AGENT_DEFAULT_MODEL.into(),
                    default_reasoning: ReasoningChoice::Default,
                },
            },
        )
        .unwrap();
    let doubled = client
        .send_turn(context(), with_skills(agent, &["review-pr", "review-pr"]))
        .map(|_| ())
        .unwrap_err();
    assert_eq!(doubled.code, "invalid_request", "a skill named twice");

    let stream = client
        .send_turn(context(), with_skills(agent, &["review-pr"]))
        .unwrap();
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(request.options.agent_skills, vec!["review-pr".to_owned()]);
    assert_eq!(
        request.messages.last().unwrap().markdown,
        "Use $review-pr",
        "the message keeps its $name text"
    );
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
}

mod attachments;
mod profiles;

/// D14: an agent model's levels become the chat's effort control — Koru's
/// order, the stated default when there is one, *off* when offered — and a
/// listing's answer fills the index the transcript's control is read from.
#[test]
fn an_agent_models_levels_become_its_effort_control() {
    use koru_session_protocol::{LaunchReasoning, ModelReasoning};
    let control = super::agents::reasoning_control(&ModelReasoning {
        levels: vec![LaunchReasoning::Low, LaunchReasoning::XHigh],
        default: Some(LaunchReasoning::XHigh),
        off: true,
    })
    .unwrap();
    assert_eq!(
        control,
        koru_ai_core::ReasoningControl::Levels {
            values: vec![
                koru_ai_core::ReasoningLevel::Low,
                koru_ai_core::ReasoningLevel::XHigh
            ],
            default: Some(koru_ai_core::ReasoningLevel::XHigh),
            supports_off: true,
        }
    );
    let response = koru_ai_wire::AgentModelsResponse {
        provider: AiProviderId::parse("agent-claude").unwrap(),
        modes: Vec::new(),
        default_mode: None,
        levels_per_turn: false,
        models_per_turn: false,
        models: vec![
            koru_ai_wire::AgentModelView {
                id: "opus".into(),
                group: "claude".into(),
                name: None,
                description: None,
                group_name: None,
                legacy: false,
                reasoning: Some(control.clone()),
                context: None,
                fast: None,
            },
            koru_ai_wire::AgentModelView {
                id: "haiku".into(),
                group: "claude".into(),
                name: None,
                description: None,
                group_name: None,
                legacy: false,
                reasoning: None,
                context: None,
                fast: None,
            },
        ],
    };
    assert_eq!(
        super::agents::agent_controls(&response),
        vec![("opus".to_owned(), control)]
    );
}

/// An agent that leaves plan mode during its reply (koru-ai D7).
static STALE_START: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Clone)]
struct ModeMovingProvider {
    requests: std::sync::mpsc::SyncSender<AiRequest>,
}

impl AiProvider for ModeMovingProvider {
    fn start(
        &self,
        request: AiRequest,
        _cancellation: Cancellation,
    ) -> Result<ProviderStream, AiError> {
        let attempt_id = request.attempt_id;
        // As if the person had chosen another mode after this was sent.
        let started_in = if STALE_START.load(std::sync::atomic::Ordering::SeqCst) {
            Some("plan".to_owned())
        } else {
            request.options.agent_mode.clone()
        };
        self.requests.send(request).unwrap();
        Ok(Box::new(
            vec![
                AiEvent::Started {
                    attempt_id,
                    provider_response_id: None,
                    request_payload_bytes: None,
                    resolved_model: None,
                    effective_reasoning: None,
                    at: Timestamp::now(),
                },
                AiEvent::AgentActivity {
                    attempt_id,
                    activity: koru_ai_core::AgentActivityEvent::ModeChanged {
                        started_in,
                        mode: "default".into(),
                    },
                    at: Timestamp::now(),
                },
                AiEvent::Completed {
                    attempt_id,
                    stop_reason: Some("stop".into()),
                    at: Timestamp::now(),
                },
            ]
            .into_iter(),
        ))
    }
}

/// An agent whose reply runs until the test lets it end.
struct HeldProvider {
    requests: std::sync::mpsc::SyncSender<AiRequest>,
    release: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>,
}

impl AiProvider for HeldProvider {
    fn start(
        &self,
        request: AiRequest,
        _cancellation: Cancellation,
    ) -> Result<ProviderStream, AiError> {
        let attempt_id = request.attempt_id;
        self.requests.send(request).unwrap();
        let release = Arc::clone(&self.release);
        let mut step = 0;
        Ok(Box::new(std::iter::from_fn(move || {
            step += 1;
            match step {
                1 => Some(AiEvent::Started {
                    attempt_id,
                    provider_response_id: None,
                    request_payload_bytes: None,
                    resolved_model: None,
                    effective_reasoning: None,
                    at: Timestamp::now(),
                }),
                2 => {
                    let _ = release
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(10));
                    Some(AiEvent::Completed {
                        attempt_id,
                        stop_reason: Some("stop".into()),
                        at: Timestamp::now(),
                    })
                }
                _ => None,
            }
        })))
    }
}

/// **A begun agent chat's model and level change while a reply runs**,
/// from its next message (koru-ai D70): the running reply took its own
/// when it was sent. A change of provider still waits for the reply.
#[test]
fn an_agent_chats_model_changes_while_a_reply_runs_from_its_next_message() {
    let directory = tempfile::tempdir().unwrap();
    let (captured, requests) = sync_channel(8);
    let (release, released) = sync_channel(8);
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(DeterministicMockProvider::new("Answer.")),
    );
    providers.insert(
        AiProviderId::parse("agent-claude").unwrap(),
        Arc::new(HeldProvider {
            requests: captured,
            release: Arc::new(std::sync::Mutex::new(released)),
        }),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let agent = agent_chat(&client, "agent-claude");
    let model = |provider: &str, requested: &str| UpdateChatModelRequest {
        model: koru_ai_core::ModelSelection {
            provider: AiProviderId::parse(provider).unwrap(),
            requested_model: requested.into(),
            default_reasoning: ReasoningChoice::Level(koru_ai_core::ReasoningLevel::High),
        },
    };

    let stream = send(&client, agent, Uuid::new_v4());
    let first = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(first.model, koru_ai_core::AGENT_DEFAULT_MODEL);
    let moved = client
        .update_chat_model(context(), agent, model("agent-claude", "sonnet"))
        .unwrap();
    assert_eq!(moved.chat.current_model.requested_model, "sonnet");
    let refused = client
        .update_chat_model(context(), agent, model("mock", "mock-model"))
        .unwrap_err();
    assert_eq!(refused.code, "ai_attempt_active");
    release.send(()).unwrap();
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );

    let stream = send(&client, agent, Uuid::new_v4());
    let next = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(next.model, "sonnet");
    assert_eq!(
        next.reasoning,
        ReasoningChoice::Level(koru_ai_core::ReasoningLevel::High)
    );
    release.send(()).unwrap();
    wait_for_terminal(&client, stream.accepted.attempt_id);
    let messages = client.read_chat(context(), agent).unwrap().messages;
    let models: Vec<_> = messages
        .iter()
        .filter(|message| message.role == ChatRole::Assistant)
        .map(|message| message.requested_model.as_deref())
        .collect();
    assert_eq!(
        models,
        [Some(koru_ai_core::AGENT_DEFAULT_MODEL), Some("sonnet")],
        "each reply keeps the model it ran on"
    );
}

fn agent_chat(client: &AiWorkerClient, provider: &str) -> ChatId {
    let chat = ChatId::new();
    client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id: chat,
                title: "Agent chat".into(),
                profile_id: None,
                model: koru_ai_core::ModelSelection {
                    provider: AiProviderId::parse(provider).unwrap(),
                    requested_model: koru_ai_core::AGENT_DEFAULT_MODEL.into(),
                    default_reasoning: ReasoningChoice::Default,
                },
            },
        )
        .unwrap();
    chat
}

/// **A chat's approval mode** (koru-ai D7) is kept on the chat, rides each
/// message to its agent, and is only for an agent chat. The agent leaving a
/// mode during the reply moves the chat with it — unless the person chose
/// another mode meanwhile, which is the one the next message takes.
#[test]
fn an_agent_chats_mode_rides_each_message_and_follows_the_agent() {
    use koru_ai_wire::UpdateChatAgentSettingsRequest;
    let directory = tempfile::tempdir().unwrap();
    let (captured, requests) = sync_channel(8);
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(DeterministicMockProvider::new("Answer.")),
    );
    providers.insert(
        AiProviderId::parse("agent-claude").unwrap(),
        Arc::new(ModeMovingProvider { requests: captured }),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let mode = |mode: &str| UpdateChatAgentSettingsRequest {
        mode: Some(mode.into()),
        fast: None,
    };

    let plain = create_chat(&client);
    let refused = client
        .update_chat_agent_settings(context(), plain, mode("plan"))
        .unwrap_err();
    assert_eq!(refused.code, "invalid_request");

    let agent = agent_chat(&client, "agent-claude");
    let bad = client
        .update_chat_agent_settings(context(), agent, mode("two words"))
        .unwrap_err();
    assert_eq!(bad.code, "invalid_request");
    let chosen = client
        .update_chat_agent_settings(context(), agent, mode("plan"))
        .unwrap();
    assert_eq!(chosen.chat.agent_mode.as_deref(), Some("plan"));
    // A request that names nothing changes nothing.
    let kept = client
        .update_chat_agent_settings(context(), agent, UpdateChatAgentSettingsRequest::default())
        .unwrap();
    assert_eq!(kept.chat.agent_mode.as_deref(), Some("plan"));

    // Fast mode (D11) beside it, kept with the mode.
    let fast = client
        .update_chat_agent_settings(
            context(),
            agent,
            UpdateChatAgentSettingsRequest {
                mode: None,
                fast: Some(true),
            },
        )
        .unwrap();
    assert_eq!(fast.chat.agent_fast, Some(true));
    assert_eq!(fast.chat.agent_mode.as_deref(), Some("plan"));

    // The message carries the mode and Fast mode; the agent leaves plan
    // mode, and the chat follows it.
    let stream = send(&client, agent, Uuid::new_v4());
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(request.options.agent_mode.as_deref(), Some("plan"));
    assert_eq!(request.options.agent_fast, Some(true));
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let after = client.read_chat(context(), agent).unwrap().chat;
    assert_eq!(after.agent_mode.as_deref(), Some("default"));

    // The person chose another mode while the agent moved — the reply was
    // sent in plan mode, the chat is in Accept edits now: theirs is kept.
    client
        .update_chat_agent_settings(context(), agent, mode("acceptEdits"))
        .unwrap();
    STALE_START.store(true, std::sync::atomic::Ordering::SeqCst);
    let stream = send(&client, agent, Uuid::new_v4());
    requests.recv_timeout(Duration::from_secs(5)).unwrap();
    wait_for_terminal(&client, stream.accepted.attempt_id);
    let after = client.read_chat(context(), agent).unwrap().chat;
    assert_eq!(after.agent_mode.as_deref(), Some("acceptEdits"));
}

/// **A pin is the chat's** (AI schema 12): listed with the chat, kept at
/// its first pin time when pinned again, cleared by unpinning, and never
/// moving the chat in the list.
#[test]
fn a_chat_is_pinned_and_unpinned_in_place() {
    use koru_ai_wire::PinChatRequest;
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(DeterministicMockProvider::new("Answer.")),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat = create_chat(&client);
    let before = client.read_chat(context(), chat).unwrap().chat;
    assert_eq!(before.pinned_at, None);
    let pinned = client
        .pin_chat(context(), chat, PinChatRequest { pinned: true })
        .unwrap()
        .chat;
    let at = pinned.pinned_at.expect("pinned");
    assert_eq!(pinned.updated_at, before.updated_at, "not activity");
    let again = client
        .pin_chat(context(), chat, PinChatRequest { pinned: true })
        .unwrap()
        .chat;
    assert_eq!(again.pinned_at, Some(at), "the first pin time is kept");
    let listed = client.list_chats(context(), None).unwrap().chats;
    assert_eq!(listed[0].pinned_at, Some(at));
    let unpinned = client
        .pin_chat(context(), chat, PinChatRequest { pinned: false })
        .unwrap()
        .chat;
    assert_eq!(unpinned.pinned_at, None);
    assert_eq!(
        client
            .pin_chat(context(), ChatId::new(), PinChatRequest { pinned: true })
            .unwrap_err()
            .code,
        "not_found"
    );
}

/// An agent that says a status line before its reply and one within it
/// (koru-ai D69).
struct NoticeProvider;

impl AiProvider for NoticeProvider {
    fn start(
        &self,
        request: AiRequest,
        _cancellation: Cancellation,
    ) -> Result<ProviderStream, AiError> {
        let attempt_id = request.attempt_id;
        let notice = |text: &str| AiEvent::AgentActivity {
            attempt_id,
            activity: koru_ai_core::AgentActivityEvent::Notice { text: text.into() },
            at: Timestamp::now(),
        };
        Ok(Box::new(
            vec![
                AiEvent::Started {
                    attempt_id,
                    provider_response_id: None,
                    request_payload_bytes: None,
                    resolved_model: None,
                    effective_reasoning: None,
                    at: Timestamp::now(),
                },
                notice("Hook failed: exited with code 127"),
                AiEvent::TextDelta {
                    attempt_id,
                    text: "Hello.".into(),
                    at: Timestamp::now(),
                },
                notice("Indexing"),
                AiEvent::Completed {
                    attempt_id,
                    stop_reason: Some("stop".into()),
                    at: Timestamp::now(),
                },
            ]
            .into_iter(),
        ))
    }
}

/// **An agent's notice is kept beside its reply, not in it** (koru-ai
/// D69): published as it comes, read back with the transcript in the order
/// it came and where the reply stood, and the reply's text is the reply's.
#[test]
fn an_agent_notice_is_kept_beside_its_reply() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("agent-devin").unwrap(),
        Arc::new(NoticeProvider),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat = agent_chat(&client, "agent-devin");
    let stream = send(&client, chat, Uuid::new_v4());
    let mut live = Vec::new();
    loop {
        let public = stream.events.recv_timeout(Duration::from_secs(5)).unwrap();
        match public.event {
            PublicAiEvent::AgentNotice { notice, .. } => live.push(notice.text),
            event if event.is_terminal() => break,
            _ => {}
        }
    }
    assert_eq!(live, ["Hook failed: exited with code 127", "Indexing"]);
    let transcript = client.read_chat(context(), chat).unwrap();
    let reply = transcript.messages.last().unwrap();
    assert_eq!(reply.markdown, "Hello.");
    let kept: Vec<_> = transcript
        .notices
        .iter()
        .map(|notice| {
            (
                notice.text.as_str(),
                notice.text_offset,
                notice.assistant_message_id,
            )
        })
        .collect();
    assert_eq!(
        kept,
        [
            ("Hook failed: exited with code 127", Some(0), reply.id),
            ("Indexing", Some("Hello.".len() as u32), reply.id),
        ]
    );
}

/// **A steer reaches only a live local agent turn.** A reply that is not an
/// agent's, an attempt that is not running, or a steer with no text is
/// refused before anything is handed over or recorded — `steer_unavailable`
/// tells the client to send the message after the reply instead.
#[test]
fn a_steer_is_refused_unless_a_local_agent_turn_is_running() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(
            DeterministicMockProvider::new("a reply that streams for a while")
                .with_chunk_delay(Duration::from_millis(200)),
        ),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = send(&client, chat_id, Uuid::new_v4());
    let attempt_id = stream.accepted.attempt_id;
    let steer = |markdown: &str| koru_ai_wire::SteerAttemptRequest {
        operation_id: Uuid::new_v4(),
        markdown: markdown.into(),
    };

    let not_an_agent = client
        .steer_attempt(context(), attempt_id, steer("also this"))
        .unwrap_err();
    assert_eq!(not_an_agent.code, "steer_unavailable");
    let empty = client
        .steer_attempt(context(), attempt_id, steer("  "))
        .unwrap_err();
    assert_eq!(empty.code, "invalid_request");
    let unknown = client
        .steer_attempt(context(), InferenceAttemptId::new(), steer("x"))
        .unwrap_err();
    assert_eq!(unknown.code, "not_found");
    let messages = client.read_chat(context(), chat_id).unwrap().messages;
    assert_eq!(messages.len(), 2, "nothing was recorded");

    client.cancel_attempt(context(), attempt_id).unwrap();
    let ended = client
        .steer_attempt(context(), attempt_id, steer("too late"))
        .unwrap_err();
    assert_eq!(ended.code, "steer_unavailable");
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// **An export reads as the chat does** (D99): a message steered into a
/// reply is written under its question, before the reply, though it was
/// stored after it; the export through that reply carries it, and a later
/// turn follows.
#[test]
fn an_export_carries_a_steer_under_its_question() {
    let chat = ChatId::new();
    let row = |ordinal: u32, role: ChatRole, markdown: &str| Message {
        id: MessageId::new(),
        chat_id: chat,
        ordinal,
        role,
        state: MessageState::Complete,
        markdown: markdown.into(),
        origin_message_id: None,
        provider: None,
        requested_model: None,
        resolved_model: None,
        error_class: None,
        created_at: Timestamp::from_millis(i64::from(ordinal) + 1),
        completed_at: Some(Timestamp::from_millis(i64::from(ordinal) + 1)),
    };
    let messages = vec![
        row(0, ChatRole::User, "Today's news?"),
        row(1, ChatRole::Assistant, "The headlines"),
        row(2, ChatRole::User, "In the US"),
        row(3, ChatRole::User, "Next"),
        row(4, ChatRole::Assistant, "Next reply"),
    ];
    let block = MessageBlockId::new();

    let through_reply = export_markdown(&messages, messages[1].id, chat, block);
    assert!(
        through_reply.starts_with(
            "## You\n\nToday's news?\n\n## You\n\nIn the US\n\n## Koru AI\n\nThe headlines\n\n"
        ),
        "{through_reply}"
    );
    assert!(!through_reply.contains("Next"));

    let through_next = export_markdown(&messages, messages[4].id, chat, block);
    let order: Vec<_> = [
        "Today's news?",
        "In the US",
        "The headlines",
        "Next",
        "Next reply",
    ]
    .iter()
    .map(|text| through_next.find(text).unwrap())
    .collect();
    assert!(order.is_sorted(), "{through_next}");
}

/// An agent that asks three sets of questions in one reply (koru-ai D108):
/// the first answered — a secret among the answers, withheld — the second
/// skipped, the third left open when the reply ends.
struct QuestionsProvider;

impl QuestionsProvider {
    fn set(id: &str) -> Vec<koru_ai_core::AgentQuestion> {
        vec![
            koru_ai_core::AgentQuestion {
                id: format!("{id}-pick"),
                header: format!("Header {id}"),
                question: "Which one?".into(),
                options: vec![koru_ai_core::AgentQuestionOption {
                    label: "Recommended".into(),
                    description: "The agent's first".into(),
                }],
                allow_other: true,
                secret: false,
            },
            koru_ai_core::AgentQuestion {
                id: format!("{id}-token"),
                header: String::new(),
                question: "The token?".into(),
                options: Vec::new(),
                allow_other: false,
                secret: true,
            },
        ]
    }
}

impl AiProvider for QuestionsProvider {
    fn start(
        &self,
        request: AiRequest,
        _cancellation: Cancellation,
    ) -> Result<ProviderStream, AiError> {
        use koru_ai_core::{AgentActivityEvent, AgentAnswer, AgentAnswerValue};
        let attempt_id = request.attempt_id;
        let activity = |activity: AgentActivityEvent| AiEvent::AgentActivity {
            attempt_id,
            activity,
            at: Timestamp::now(),
        };
        let [answered, skipped, open] = [
            ToolInvocationId::new(),
            ToolInvocationId::new(),
            ToolInvocationId::new(),
        ];
        Ok(Box::new(
            vec![
                AiEvent::Started {
                    attempt_id,
                    provider_response_id: None,
                    request_payload_bytes: None,
                    resolved_model: None,
                    effective_reasoning: None,
                    at: Timestamp::now(),
                },
                activity(AgentActivityEvent::QuestionsAsked {
                    invocation: answered,
                    questions: Self::set("a"),
                    blocking: true,
                    next_message: false,
                }),
                activity(AgentActivityEvent::QuestionsAnswered {
                    invocation: answered,
                    answers: Some(vec![
                        AgentAnswer {
                            question_id: "a-pick".into(),
                            value: AgentAnswerValue::Option {
                                label: "Recommended".into(),
                            },
                        },
                        AgentAnswer {
                            question_id: "a-token".into(),
                            value: AgentAnswerValue::Withheld,
                        },
                    ]),
                }),
                activity(AgentActivityEvent::QuestionsAsked {
                    invocation: skipped,
                    questions: Self::set("b"),
                    blocking: false,
                    next_message: false,
                }),
                activity(AgentActivityEvent::QuestionsAnswered {
                    invocation: skipped,
                    answers: None,
                }),
                activity(AgentActivityEvent::QuestionsAsked {
                    invocation: open,
                    questions: Self::set("c"),
                    blocking: true,
                    next_message: false,
                }),
                AiEvent::TextDelta {
                    attempt_id,
                    text: "Thanks.".into(),
                    at: Timestamp::now(),
                },
                AiEvent::Completed {
                    attempt_id,
                    stop_reason: Some("stop".into()),
                    at: Timestamp::now(),
                },
            ]
            .into_iter(),
        ))
    }
}

/// **An agent's questions are a permission request's row** (koru-ai D108):
/// a set the agent waits on asks for the person, live with its questions,
/// one it does not wait on runs; answered it is allowed with what was said
/// — a secret withheld — skipped it is denied, and one its reply left open
/// is cancelled. The questions and the answers stay on the row, so the set
/// reads the same reopened, and an older build reads a permission request.
#[test]
fn an_agents_questions_are_a_permission_rows_and_read_back_the_same() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("agent-codex").unwrap(),
        Arc::new(QuestionsProvider),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat = agent_chat(&client, "agent-codex");
    let stream = send(&client, chat, Uuid::new_v4());
    let mut asked = Vec::new();
    loop {
        let public = stream.events.recv_timeout(Duration::from_secs(5)).unwrap();
        match public.event {
            PublicAiEvent::ToolActivity { activity, .. }
                if activity
                    .live
                    .as_ref()
                    .is_some_and(|live| !live.questions.is_empty()) =>
            {
                asked.push((activity.status, activity.live.unwrap().questions));
            }
            event if event.is_terminal() => break,
            _ => {}
        }
    }
    assert_eq!(asked.len(), 3);
    assert_eq!(asked[0].0, ToolActivityStatus::ApprovalRequired);
    assert_eq!(asked[1].0, ToolActivityStatus::Running, "not waited on");
    assert_eq!(asked[0].1[0].options[0].label, "Recommended");

    let transcript = client.read_chat(context(), chat).unwrap();
    let rows = &transcript.tool_activities;
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row.tool_name == "agent.approval"));
    assert!(
        rows.iter().all(|row| row.live.is_none()),
        "nothing live after"
    );
    // The rows share a timestamp; each set is found by its own header.
    let set = |header: &str| {
        rows.iter()
            .find(|row| {
                row.agent_summary
                    .as_deref()
                    .is_some_and(|summary| summary.starts_with(header))
            })
            .unwrap()
    };
    let answered = set("Header a");
    assert_eq!(answered.status, ToolActivityStatus::Complete);
    assert_eq!(answered.approval, ToolApproval::Approved);
    assert_eq!(
        answered.agent_summary.as_deref(),
        Some("Header a · The token?")
    );
    let questions =
        koru_ai_core::QuestionsRecord::decode(answered.agent_input.as_deref().unwrap()).unwrap();
    assert_eq!(questions.questions[0].question, "Which one?");
    assert!(questions.questions[1].secret);
    let answers =
        koru_ai_core::AnswersRecord::decode(answered.agent_output.as_deref().unwrap()).unwrap();
    assert_eq!(
        answers.answers[1].value,
        koru_ai_core::AgentAnswerValue::Withheld
    );
    assert_eq!(set("Header b").status, ToolActivityStatus::Denied);
    assert_eq!(set("Header b").approval, ToolApproval::Denied);
    assert_eq!(set("Header c").status, ToolActivityStatus::Cancelled);
    assert!(set("Header c").agent_output.is_none());

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// An agent that asks, in each reply, one set answered by the next message
/// and moves on (koru-ai D108): Codex's `request_user_input_async`.
struct NextMessageQuestionsProvider;

impl AiProvider for NextMessageQuestionsProvider {
    fn start(
        &self,
        request: AiRequest,
        _cancellation: Cancellation,
    ) -> Result<ProviderStream, AiError> {
        use koru_ai_core::AgentActivityEvent;
        let attempt_id = request.attempt_id;
        Ok(Box::new(
            vec![
                AiEvent::Started {
                    attempt_id,
                    provider_response_id: None,
                    request_payload_bytes: None,
                    resolved_model: None,
                    effective_reasoning: None,
                    at: Timestamp::now(),
                },
                AiEvent::AgentActivity {
                    attempt_id,
                    activity: AgentActivityEvent::QuestionsAsked {
                        invocation: ToolInvocationId::new(),
                        questions: vec![koru_ai_core::AgentQuestion {
                            id: "[\"request_user_input_async\",\"call\",0]".into(),
                            header: String::new(),
                            question: "Which database?".into(),
                            options: vec![koru_ai_core::AgentQuestionOption {
                                label: "SQLite".into(),
                                description: String::new(),
                            }],
                            allow_other: true,
                            secret: false,
                        }],
                        blocking: false,
                        next_message: true,
                    },
                    at: Timestamp::now(),
                },
                AiEvent::Completed {
                    attempt_id,
                    stop_reason: Some("stop".into()),
                    at: Timestamp::now(),
                },
            ]
            .into_iter(),
        ))
    }
}

/// The set a reply asked, read back from the stream: its row's id.
fn asked_set(stream: &AiResponseStream) -> ToolInvocationId {
    let mut asked = None;
    loop {
        let public = stream.events.recv_timeout(Duration::from_secs(5)).unwrap();
        match public.event {
            PublicAiEvent::ToolActivity { activity, .. }
                if activity
                    .live
                    .as_ref()
                    .is_some_and(|live| !live.questions.is_empty()) =>
            {
                asked = Some(activity.id);
            }
            event if event.is_terminal() => break,
            _ => {}
        }
    }
    asked.expect("a set was asked")
}

/// **A set answered by the next message outlives its reply** (koru-ai
/// D108): it stays open and live — its questions on the transcript — after
/// the reply that asked ends, says so in its record, and is answered by a
/// message of its own, never by approving the row once its reply ended.
/// Another message passes it over (cancelled, on that message's reply), and
/// a skip closes it with nothing sent.
#[test]
fn a_set_answered_by_the_next_message_outlives_its_reply_until_passed_over() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("agent-codex").unwrap(),
        Arc::new(NextMessageQuestionsProvider),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat = agent_chat(&client, "agent-codex");
    let first = asked_set(&send(&client, chat, Uuid::new_v4()));

    let row = |id: ToolInvocationId| {
        client
            .read_chat(context(), chat)
            .unwrap()
            .tool_activities
            .into_iter()
            .find(|row| row.id == id)
            .unwrap()
    };
    let open = row(first);
    assert_eq!(open.status, ToolActivityStatus::Running);
    assert_eq!(open.approval, ToolApproval::Pending);
    assert_eq!(
        open.live.as_ref().unwrap().questions[0].options[0].label,
        "SQLite",
        "still answerable after its reply"
    );
    let record =
        koru_ai_core::QuestionsRecord::decode(open.agent_input.as_deref().unwrap()).unwrap();
    assert!(record.next_message);
    let refused = client
        .approve_tool_call(
            context(),
            first,
            koru_ai_wire::ToolDecisionRequest {
                choice: None,
                answers: Some(vec![koru_ai_core::AgentAnswer {
                    question_id: "[\"request_user_input_async\",\"call\",0]".into(),
                    value: koru_ai_core::AgentAnswerValue::Option {
                        label: "SQLite".into(),
                    },
                }]),
            },
        )
        .unwrap_err();
    assert_eq!(refused.code, "answer_as_message");

    // Another message passes it over, on that message's own reply.
    let next = send(&client, chat, Uuid::new_v4());
    let mut passed = None;
    let mut second = None;
    loop {
        let public = next.events.recv_timeout(Duration::from_secs(5)).unwrap();
        match public.event {
            PublicAiEvent::ToolActivity { activity, .. } if activity.id == first => {
                passed = Some(activity.status);
            }
            PublicAiEvent::ToolActivity { activity, .. } if activity.live.is_some() => {
                second = Some(activity.id);
            }
            event if event.is_terminal() => break,
            _ => {}
        }
    }
    assert_eq!(passed, Some(ToolActivityStatus::Cancelled));
    let closed = row(first);
    assert_eq!(closed.status, ToolActivityStatus::Cancelled);
    assert!(closed.live.is_none());

    // The second reply's set is skipped: denied, and no reply runs for it.
    let second = second.expect("the second reply asked again");
    let skipped = client.deny_tool_call(context(), second, None).unwrap();
    assert_eq!(skipped.activity.status, ToolActivityStatus::Denied);
    assert_eq!(skipped.activity.approval, ToolApproval::Denied);
    assert!(row(second).live.is_none());
    let replies = client
        .read_chat(context(), chat)
        .unwrap()
        .messages
        .iter()
        .filter(|message| message.role == ChatRole::Assistant)
        .count();
    assert_eq!(replies, 2, "a skip starts no reply");

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// **Answers go only to a set answered by the next message**, on their own:
/// with words of their own, or for a set that is not one, they are refused
/// and no reply starts.
#[test]
fn answers_as_a_message_are_refused_for_anything_but_an_open_set() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("agent-codex").unwrap(),
        Arc::new(NextMessageQuestionsProvider),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat = agent_chat(&client, "agent-codex");
    let set = asked_set(&send(&client, chat, Uuid::new_v4()));
    let answers = |invocation: ToolInvocationId, markdown: &str| SendTurnRequest {
        operation_id: Uuid::new_v4(),
        chat_id: chat,
        surface: AiSurface::Standalone,
        markdown: markdown.into(),
        explicit_refs: Vec::new(),
        web_search: false,
        reasoning_override: None,
        system_prompt_override: None,
        replace_pinned_documents: false,
        attachment_ids: Vec::new(),
        answers: Some(koru_ai_core::NextMessageAnswers {
            invocation,
            answers: vec![koru_ai_core::AgentAnswer {
                question_id: "[\"request_user_input_async\",\"call\",0]".into(),
                value: koru_ai_core::AgentAnswerValue::Option {
                    label: "SQLite".into(),
                },
            }],
        }),
        skills: Vec::new(),
    };
    let worded = client
        .send_turn(context(), answers(set, "and words"))
        .err()
        .unwrap();
    assert_eq!(worded.code, "invalid_request");
    let stranger = client
        .send_turn(context(), answers(ToolInvocationId::new(), ""))
        .err()
        .unwrap();
    assert_eq!(stranger.code, "not_found");
    let messages = client.read_chat(context(), chat).unwrap().messages;
    assert_eq!(messages.len(), 2, "nothing was written");

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// **A chat says a missing adapter is a missing adapter**, as Settings does:
/// the host's `Missing` with the agent's own program present is
/// `agent_adapter_missing`; without it, or with no row, the agent is not
/// installed — and only `Missing` searches for the program.
#[test]
fn a_missing_route_with_the_agent_present_is_a_missing_adapter() {
    use super::agents::readiness_status as status;
    use koru_session_protocol::ExecutableAvailability as A;
    assert_eq!(
        status(Some(A::Missing), || true),
        (
            ProviderValidationStatus::Unavailable,
            Some("agent_adapter_missing")
        )
    );
    assert_eq!(
        status(Some(A::Missing), || false),
        (
            ProviderValidationStatus::Unavailable,
            Some("agent_not_installed")
        )
    );
    assert_eq!(
        status(None, || panic!("no row is not a search")),
        (
            ProviderValidationStatus::Unavailable,
            Some("agent_not_installed")
        )
    );
    assert_eq!(
        status(Some(A::Available { version: None }), || panic!("not asked")),
        (ProviderValidationStatus::Ready, None)
    );
    assert_eq!(
        status(Some(A::Checking), || panic!("not asked")).1,
        Some("agent_checking")
    );
}
