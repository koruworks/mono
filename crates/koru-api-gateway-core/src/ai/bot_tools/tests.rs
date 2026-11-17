// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::bots::*;
use koru_ai_core::{AttemptStatus, DocumentId, MessageId, ModelSelection, ReasoningChoice};
use std::sync::Mutex;
use uuid::Uuid;

fn bot() -> Bot {
    let home_workspace = WorkspaceId::new();
    Bot {
        may_ask: true,
        id: BotId::mint(),
        home_workspace,
        user_scope: UserScopeId::new(),
        owner_device: DeviceId::from_uuid(Uuid::new_v4()),
        config: BotConfig {
            name: "Fixture".into(),
            role: BotPage {
                workspace: home_workspace,
                document: DocumentId::new(),
            },
            working_folder: "PRIVATE-FOLDER".into(),
            model: ModelSelection {
                provider: koru_ai_core::AiProviderId::parse("agent-codex").unwrap(),
                requested_model: "fixture".into(),
                default_reasoning: ReasoningChoice::Default,
            },
            mode: None,
            fast: None,
            avatar: None,
        },
        access: BotAccess::Default,
        selected_pages: Vec::new(),
        revision: 1,
        policy_revision: 1,
        main_chat: ChatId::new(),
        paused: false,
        folder_lock_request: None,
        folder_frozen_at_ms: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        lifecycle: BotLifecycle::Active,
        open_operation: None,
        held_by: None,
        pending_change: None,
        connections: Default::default(),
    }
}
fn context() -> RequestContext {
    owner_context(RequestContext {
        principal: AiPrincipal {
            label: "caller-selected".into(),
            scopes: vec![AiScope::ReadBots, AiScope::RunBots],
        },
        trace_id: "test".into(),
        deadline_millis: None,
    })
}

struct Port {
    bot: Bot,
    sent: Mutex<Vec<SendTurnRequest>>,
    accepted: SendTurnAccepted,
    answer: BotTaskAnswer,
    cancelled: Mutex<Vec<InferenceAttemptId>>,
}
impl Port {
    fn new() -> Self {
        let accepted = SendTurnAccepted {
            attempt_id: InferenceAttemptId::new(),
            user_message_id: MessageId::new(),
            assistant_message_id: MessageId::new(),
            replayed: false,
        };
        Self {
            bot: bot(),
            sent: Mutex::new(Vec::new()),
            cancelled: Mutex::new(Vec::new()),
            answer: BotTaskAnswer {
                status: AttemptStatus::Pending,
                message_id: accepted.assistant_message_id,
                run_id: None,
                markdown: None,
                next_offset: None,
            },
            accepted,
        }
    }
    fn start(&self, request_id: Uuid) -> StartBotTaskRequest {
        StartBotTaskRequest {
            workspace: self.bot.home_workspace,
            bot_id: self.bot.id,
            request_id,
            text: "Find potential VCs".into(),
            document_ids: Vec::new(),
        }
    }
}
impl TaskPort for Port {
    fn bots(&self, context: RequestContext, command: BotCommand) -> Result<BotResponse, Failure> {
        assert_eq!(
            context.principal.scopes,
            vec![AiScope::ManageBots, AiScope::Chat]
        );
        match command {
            BotCommand::Read { bot_id } if bot_id == self.bot.id => Ok(BotResponse::Bot {
                bot: self.bot.clone(),
            }),
            BotCommand::ListTaskBots {
                workspace,
                after: None,
                limit: 1,
            } if workspace == self.bot.home_workspace => Ok(BotResponse::TaskBots {
                page: BotSummaryPage {
                    bots: vec![BotSummary {
                        bot_id: self.bot.id,
                        name: self.bot.config.name.clone(),
                        main_chat: self.bot.main_chat,
                        revision: self.bot.revision,
                        paused: self.bot.paused,
                        lifecycle: self.bot.lifecycle,
                    }],
                    next: None,
                },
            }),
            BotCommand::ReadTaskAnswer {
                bot_id,
                chat_id,
                attempt_id,
                operation_id,
                offset,
            } => {
                let sent = self.sent.lock().unwrap();
                if bot_id != self.bot.id
                    || chat_id != self.bot.main_chat
                    || attempt_id != self.accepted.attempt_id
                    || sent.first().is_none_or(|r| r.operation_id != operation_id)
                    || offset != 0
                {
                    return Err(missing());
                }
                Ok(BotResponse::TaskAnswer {
                    answer: self.answer.clone(),
                })
            }
            _ => Err(missing()),
        }
    }
    fn send_turn(
        &self,
        context: RequestContext,
        request: SendTurnRequest,
    ) -> Result<koru_ai_api::AiResponseStream, Failure> {
        assert_eq!(
            context.principal.scopes,
            vec![AiScope::ManageBots, AiScope::Chat]
        );
        self.sent.lock().unwrap().push(request);
        let (_, events) = std::sync::mpsc::channel();
        Ok(koru_ai_api::AiResponseStream {
            accepted: self.accepted.clone(),
            events,
        })
    }
    fn cancel_attempt(
        &self,
        _: RequestContext,
        attempt: InferenceAttemptId,
    ) -> Result<CancelAttemptResponse, Failure> {
        assert_eq!(attempt, self.accepted.attempt_id);
        self.cancelled.lock().unwrap().push(attempt);
        Ok(CancelAttemptResponse {
            attempt_id: attempt,
            status: AttemptStatus::Cancelled,
        })
    }
}

#[test]
fn bot_stop_checks_exact_task_before_shared_cancel() {
    let port = Port::new();
    let key = Uuid::new_v4();
    start(&port, context(), port.start(key)).unwrap();
    let mut request = StopBotTaskRequest {
        workspace: port.bot.home_workspace,
        bot_id: port.bot.id,
        request_id: Uuid::new_v4(),
        attempt_id: port.accepted.attempt_id,
    };
    assert_eq!(
        stop(&port, context(), request.clone()).unwrap_err().code,
        "not_found"
    );
    assert!(port.cancelled.lock().unwrap().is_empty());
    request.request_id = key;
    assert_eq!(
        stop(&port, context(), request.clone()).unwrap().status,
        AttemptStatus::Cancelled
    );
    request.attempt_id = InferenceAttemptId::new();
    assert_eq!(
        stop(&port, context(), request).unwrap_err().code,
        "not_found"
    );
    assert_eq!(
        port.cancelled.lock().unwrap().as_slice(),
        &[port.accepted.attempt_id]
    );
}

#[test]
fn bot_results_include_only_confirmed_current_workspace_references() {
    let workspace = WorkspaceId::new();
    let saved = koru_delegate_core::NativeResultReference {
        version_id: None,
        operation_id: Uuid::new_v4(),
        workspace,
        document: DocumentId::new(),
        version: vec![1, 2, 3],
        title: "VC research".into(),
        confirmed_at_ms: 11,
        kind: koru_delegate_core::NativeResultKind::CreatedPage,
    };
    let mut foreign = saved.clone();
    foreign.workspace = WorkspaceId::new();
    let mut invalid = saved.clone();
    invalid.version.clear();
    let result = confirmed_results(workspace, vec![foreign, invalid, saved.clone()]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].document_id, saved.document);
    assert_eq!(STANDARD.decode(&result[0].version).unwrap(), saved.version);
    assert_eq!(result[0].title, "VC research");
    let json = serde_json::to_value(&result[0]).unwrap();
    for private in [
        "session",
        "run_id",
        "reasoning",
        "tool_payload",
        "operation_id",
    ] {
        assert!(json.get(private).is_none());
    }
}

#[test]
fn bot_tasks_bind_retry_to_owner_and_payload() {
    let port = Port::new();
    let key = Uuid::new_v4();
    let request = port.start(key);
    start(&port, context(), request.clone()).unwrap();
    let mut renamed = context();
    renamed.principal.label = "a different caller label".into();
    start(&port, renamed, request).unwrap();
    let sent = port.sent.lock().unwrap();
    assert_eq!(
        sent[0], sent[1],
        "uncertain retry reaches the shared durable fingerprint unchanged"
    );
    assert_eq!(sent[0].chat_id, port.bot.main_chat);
    assert_eq!(sent[0].destination_workspace, Some(port.bot.home_workspace));
    assert!(sent[0].system_prompt_override.is_none());
    assert!(sent[0].skills.is_empty());
    let original = operation_id(&port.bot, key).unwrap();
    let mut other = port.bot.clone();
    other.owner_device = DeviceId::from_uuid(Uuid::new_v4());
    assert_ne!(original, operation_id(&other, key).unwrap());
    other = port.bot.clone();
    other.user_scope = UserScopeId::new();
    assert_ne!(original, operation_id(&other, key).unwrap());
    other = port.bot.clone();
    other.id = BotId::mint();
    assert_eq!(
        original,
        operation_id(&other, key).unwrap(),
        "owner-global UUID reuse conflicts across Bots through shared SendTurn"
    );
    assert!(operation_id(&port.bot, Uuid::nil()).is_err());
    let mut malformed = *key.as_bytes();
    malformed[8] = 0;
    assert!(operation_id(&port.bot, Uuid::from_bytes(malformed)).is_err());
}

#[test]
fn bot_tasks_reject_other_workspace_attempt_and_documents() {
    let port = Port::new();
    let principal = GatewayPrincipal {
        label: "untrusted-label".into(),
        scopes: vec!["bots.run".into()],
    };
    let mut selected = port.start(Uuid::new_v4());
    assert!(selected_documents_allowed(&principal, &selected).is_ok());
    selected.document_ids.push(DocumentId::new());
    assert_eq!(
        selected_documents_allowed(&principal, &selected)
            .unwrap_err()
            .code,
        "forbidden"
    );
    let mut authorized = principal;
    authorized.scopes.push("store.read_documents".into());
    assert!(selected_documents_allowed(&authorized, &selected).is_ok());
    let mut request = port.start(Uuid::new_v4());
    request.workspace = WorkspaceId::new();
    assert_eq!(
        start(&port, context(), request).unwrap_err().code,
        "not_found"
    );
    let mut request = port.start(Uuid::new_v4());
    request.document_ids = (0..17).map(|_| DocumentId::new()).collect();
    assert_eq!(
        start(&port, context(), request).unwrap_err().code,
        "invalid_request"
    );
    assert!(port.sent.lock().unwrap().is_empty());
    let key = Uuid::new_v4();
    start(&port, context(), port.start(key)).unwrap();
    let read_request = ReadBotTaskRequest {
        workspace: port.bot.home_workspace,
        bot_id: port.bot.id,
        request_id: key,
        attempt_id: InferenceAttemptId::new(),
        cursor: None,
    };
    assert_eq!(
        read(&port, context(), read_request).unwrap_err().code,
        "not_found"
    );
    let request = ReadBotTaskRequest {
        workspace: port.bot.home_workspace,
        bot_id: port.bot.id,
        request_id: key,
        attempt_id: port.accepted.attempt_id,
        cursor: Some(
            encode(&TaskCursor {
                attempt: InferenceAttemptId::new(),
                operation: operation_id(&port.bot, key).unwrap(),
                answer: 0,
                results: 0,
            })
            .unwrap(),
        ),
    };
    assert_eq!(
        read(&port, context(), request).unwrap_err().code,
        "invalid_request"
    );
}

#[test]
fn bot_tools_preserve_preparing_and_actual_chat_only_answer() {
    let mut port = Port::new();
    let key = Uuid::new_v4();
    start(&port, context(), port.start(key)).unwrap();
    let request = ReadBotTaskRequest {
        workspace: port.bot.home_workspace,
        bot_id: port.bot.id,
        request_id: key,
        attempt_id: port.accepted.attempt_id,
        cursor: None,
    };
    let pending = read(&port, context(), request.clone()).unwrap();
    assert_eq!(pending.status, AttemptStatus::Pending);
    assert!(matches!(pending.results_state, BotResultsState::Preparing));
    assert_eq!(pending.answer, None);
    port.answer.status = AttemptStatus::Complete;
    port.answer.markdown = Some("Three potential investors".into());
    let done = read(&port, context(), request).unwrap();
    assert_eq!(done.answer.as_deref(), Some("Three potential investors"));
    assert!(matches!(done.results_state, BotResultsState::Unavailable));
    let listed = serde_json::to_string(
        &list(
            &port,
            context(),
            ListBotsRequest {
                workspace: port.bot.home_workspace,
                cursor: None,
                limit: 1,
            },
        )
        .unwrap(),
    )
    .unwrap();
    assert!(!listed.contains("PRIVATE-FOLDER"));
    assert!(!listed.contains("user_scope"));
}

#[test]
fn bot_tool_pages_are_scoped_and_bounded() {
    let attempt = InferenceAttemptId::new();
    let operation = Uuid::new_v4();
    let answer = BotTaskAnswer {
        status: AttemptStatus::Complete,
        message_id: MessageId::new(),
        run_id: None,
        markdown: Some("\u{1}".repeat(16 * 1024)),
        next_offset: Some(16 * 1024),
    };
    let result = BotTaskResult {
        document_id: DocumentId::new(),
        workspace: WorkspaceId::new(),
        title: "Saved result".into(),
        version: STANDARD.encode(vec![7; 65536]),
        confirmed_at_ms: 1,
    };
    let cursor = TaskCursor {
        attempt,
        operation,
        answer: 0,
        results: 0,
    };
    let first = page(
        attempt,
        operation,
        answer,
        BotResultsState::Available,
        vec![result.clone()],
        cursor,
    )
    .unwrap();
    assert!(serde_json::to_vec(&first).unwrap().len() <= PAGE_BYTES);
    assert!(
        first.results.is_empty(),
        "escaped text and large version cannot share a page"
    );
    let cursor: TaskCursor = decode(first.next_cursor.as_deref().unwrap()).unwrap();
    assert_eq!(cursor.answer, 16 * 1024);
    assert_eq!(cursor.results, 0);
    let answer = BotTaskAnswer {
        status: AttemptStatus::Complete,
        message_id: MessageId::new(),
        run_id: None,
        markdown: Some(String::new()),
        next_offset: None,
    };
    let second = page(
        attempt,
        operation,
        answer.clone(),
        BotResultsState::Available,
        vec![result],
        cursor,
    )
    .unwrap();
    assert_eq!(second.results.len(), 1);
    assert_eq!(second.next_cursor, None);
    assert!(serde_json::to_vec(&second).unwrap().len() <= PAGE_BYTES);
    let small = BotTaskResult {
        document_id: DocumentId::new(),
        workspace: WorkspaceId::new(),
        title: "page".into(),
        version: "AQ==".into(),
        confirmed_at_ms: 1,
    };
    let many = page(
        attempt,
        operation,
        answer.clone(),
        BotResultsState::Available,
        vec![small.clone(); 21],
        TaskCursor {
            attempt,
            operation,
            answer: 0,
            results: 0,
        },
    )
    .unwrap();
    assert_eq!(many.results.len(), 20);
    assert!(many.next_cursor.is_some());
    let cursor: TaskCursor = decode(many.next_cursor.as_deref().unwrap()).unwrap();
    let mut appended = vec![small; 21];
    let last_id = DocumentId::new();
    appended.push(BotTaskResult {
        document_id: last_id,
        workspace: WorkspaceId::new(),
        title: "Appended".into(),
        version: "AQ==".into(),
        confirmed_at_ms: 2,
    });
    let last = page(
        attempt,
        operation,
        answer,
        BotResultsState::Available,
        appended,
        cursor,
    )
    .unwrap();
    assert_eq!(last.results.len(), 2);
    assert_eq!(last.results[1].document_id, last_id);
    assert_eq!(last.next_cursor, None);
}
