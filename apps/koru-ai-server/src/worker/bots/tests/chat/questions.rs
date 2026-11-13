// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::{
    AgentAnswer, AgentAnswerValue, AnswersRecord, NextMessageAnswers, QuestionsRecord,
};
use std::sync::atomic::AtomicUsize;

struct TerminalNative {
    native: Native,
    request: s::QuestionRequestId,
    extra_requests: Vec<s::QuestionRequestId>,
    secret: bool,
    fail_second: bool,
    resolve_last: bool,
    fail_asking: bool,
    native_answers: AtomicUsize,
}
impl TerminalNative {
    fn new(secret: bool, cards: usize, resolve_last: bool, fail_asking: bool) -> Self {
        Self {
            native: Native {
                allow_answer_tasks: true,
                fail_first: true,
                ..Native::default()
            },
            request: s::QuestionRequestId::mint(),
            extra_requests: (1..cards).map(|_| s::QuestionRequestId::mint()).collect(),
            secret,
            fail_second: secret,
            resolve_last,
            fail_asking,
            native_answers: AtomicUsize::new(0),
        }
    }
}
impl BotDelegatePort for TerminalNative {
    fn execute(&self, command: D) -> Result<BotReply, koru_delegate_api::Failure> {
        if matches!(
            &command,
            D::AnswerQuestions { .. } | D::SkipQuestions { .. }
        ) {
            self.native_answers.fetch_add(1, Ordering::SeqCst);
            panic!("a terminal native prompt must never be answered");
        }
        if let D::Observe { fence, after } = &command {
            let state = self.native.state.lock().unwrap();
            if self.fail_second && state.begin == 2 && fence.turn.is_some() {
                let turn_id = fence.turn.unwrap();
                let native = vec![
                    s::AgentEvent::TurnStarted { turn_id },
                    s::AgentEvent::TurnFailed {
                        turn_id,
                        error: s::SessionFailure {
                            code: s::ErrorCode::ProviderUnavailable,
                            message: "fixture answer turn failed".into(),
                        },
                    },
                ];
                let cursor = s::AGENT_EVENT_INITIAL_CURSOR + native.len() as u64;
                let events = native
                    .into_iter()
                    .enumerate()
                    .filter(|(n, _)| {
                        after.is_none_or(|cursor| {
                            *n as u64 + s::AGENT_EVENT_FIRST_SEQUENCE > cursor.get()
                        })
                    })
                    .map(|(n, event)| s::SequencedAgentEvent {
                        session_id: fence.session,
                        sequence: s::StreamSequence::new(n as u64 + s::AGENT_EVENT_FIRST_SEQUENCE)
                            .unwrap(),
                        event,
                    })
                    .collect();
                return Ok(BotReply::Observation {
                    observation: BotObservation {
                        fence: fence.clone(),
                        observation: s::AgentEventObservation {
                            events,
                            cursor: s::StreamSequence::new(cursor).unwrap(),
                            earlier_truncated: false,
                            commands: None,
                            session_cursor: None,
                        },
                    },
                });
            }
            if state.begin == 1 && fence.turn.is_some() {
                assert_eq!(state.fence.as_ref(), Some(fence));
                let turn_id = fence.turn.unwrap();
                let mut native = vec![s::AgentEvent::TurnStarted { turn_id }];
                for request in
                    std::iter::once(self.request).chain(self.extra_requests.iter().copied())
                {
                    native.push(s::AgentEvent::QuestionsRequired {
                        turn_id,
                        request_id: request,
                        blocking: false,
                        delivery: s::QuestionsDelivery::NextMessage,
                        questions: vec![s::AgentQuestion {
                            id: "question".into(),
                            header: "Fixture".into(),
                            question: "Give the complete value".into(),
                            options: Vec::new(),
                            allow_other: true,
                            secret: self.secret,
                        }],
                    });
                }
                if self.resolve_last {
                    native.push(s::AgentEvent::QuestionsResolved {
                        turn_id,
                        request_id: self.extra_requests.last().copied().unwrap_or(self.request),
                        outcome: s::QuestionsOutcome::Expired,
                    });
                }
                native.push(s::AgentEvent::MessageDelta {
                    turn_id,
                    text: "ASKING-REPLY".into(),
                });
                native.push(if self.fail_asking {
                    s::AgentEvent::TurnFailed {
                        turn_id,
                        error: s::SessionFailure {
                            code: s::ErrorCode::ProviderUnavailable,
                            message: "fixture asking reply failed".into(),
                        },
                    }
                } else {
                    s::AgentEvent::TurnCompleted { turn_id }
                });
                let sequence = s::AGENT_EVENT_INITIAL_CURSOR + native.len() as u64;
                let events = native
                    .into_iter()
                    .enumerate()
                    .filter(|(n, _)| {
                        after.is_none_or(|cursor| {
                            *n as u64 + s::AGENT_EVENT_FIRST_SEQUENCE > cursor.get()
                        })
                    })
                    .map(|(n, event)| s::SequencedAgentEvent {
                        session_id: fence.session,
                        sequence: s::StreamSequence::new(n as u64 + s::AGENT_EVENT_FIRST_SEQUENCE)
                            .unwrap(),
                        event,
                    })
                    .collect();
                return Ok(BotReply::Observation {
                    observation: BotObservation {
                        fence: fence.clone(),
                        observation: s::AgentEventObservation {
                            events,
                            cursor: s::StreamSequence::new(sequence).unwrap(),
                            earlier_truncated: false,
                            commands: None,
                            session_cursor: None,
                        },
                    },
                });
            }
        }
        self.native.execute(command)
    }
}

struct Fixture {
    worker: WorkerHandle,
    client: AiWorkerClient,
    context: CommandContext,
    authority: Arc<Authority>,
    bot: Bot,
    native: Arc<TerminalNative>,
    row: ToolActivity,
    first_request: SendTurnRequest,
    returned: Receiver<Repository>,
}
fn start(secret: bool) -> Fixture {
    start_with_cards(secret, 1)
}
fn start_with_cards(secret: bool, cards: usize) -> Fixture {
    start_with_resolution(secret, cards, false)
}
fn start_with_resolution(secret: bool, cards: usize, resolve_last: bool) -> Fixture {
    start_with_options(secret, cards, resolve_last, false)
}
fn start_with_options(
    secret: bool,
    cards: usize,
    resolve_last: bool,
    fail_asking: bool,
) -> Fixture {
    start_with_context_options(secret, cards, resolve_last, fail_asking, false, false)
}
fn start_with_context_options(
    secret: bool,
    cards: usize,
    resolve_last: bool,
    fail_asking: bool,
    task_document: bool,
    fail_answer: bool,
) -> Fixture {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let chat = repository
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    let mut context = proof(authority.clone(), &bot, chat);
    let task = task_document.then(DocumentId::new);
    if let Some(document) = task {
        let prepared = Arc::get_mut(context.prepared.as_mut().unwrap()).unwrap();
        prepared.pins.push(d::RunContextItem::Document {
            workspace: bot.home_workspace,
            document,
            version: d::DocumentVersionId::new(),
        });
        prepared
            .library_sources
            .as_mut()
            .unwrap()
            .task_documents
            .push(BotPage {
                workspace: bot.home_workspace,
                document,
            });
    }
    let mut native = TerminalNative::new(secret, cards, resolve_last, fail_asking);
    native.fail_second |= fail_answer;
    let native = Arc::new(native);
    let (tx, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-in-memory-root"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority.clone())
    .with_bot_delegate(native.clone());
    config.test_repository = Some(repository);
    config.test_repository_return = Some(tx);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    let mut first_request = request(&bot);
    if let Some(document_id) = task {
        first_request
            .explicit_refs
            .push(ContextRef::Document { document_id });
    }
    let stream = send(&client, context.clone(), first_request.clone()).unwrap();
    finish_as(stream, !fail_asking);
    settled(&native, 1);
    let row = client
        .read_chat(super::super::context(true), bot.main_chat)
        .unwrap()
        .tool_activities
        .into_iter()
        .find(|row| {
            !row.status.is_terminal() && {
                row.agent_input
                    .as_deref()
                    .and_then(QuestionsRecord::decode)
                    .is_some_and(|r| r.next_message)
            }
        })
        .unwrap();
    assert_eq!(row.approval, ToolApproval::Pending);
    Fixture {
        worker,
        client,
        context,
        authority,
        bot,
        native,
        row,
        first_request,
        returned,
    }
}
fn send(
    client: &AiWorkerClient,
    context: CommandContext,
    request: SendTurnRequest,
) -> ApiResult<AiResponseStream> {
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::SendTurn {
            context,
            request,
            reply,
        })
        .unwrap();
    response.recv_timeout(Duration::from_secs(5)).unwrap()
}
fn finish(stream: AiResponseStream) {
    finish_as(stream, true)
}
fn finish_as(stream: AiResponseStream, completed: bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "response never settled");
        let event = stream
            .events
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .event;
        match event {
            PublicAiEvent::Completed { .. } => {
                assert!(completed, "fixture failure unexpectedly completed");
                break;
            }
            PublicAiEvent::Failed { .. } => {
                assert!(!completed, "Bot response failed");
                break;
            }
            _ => {}
        }
    }
}
fn settled(native: &TerminalNative, begin: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state = native.native.state.lock().unwrap();
        // A failed native turn finishes through automatic Interrupt.
        if state.begin == begin && (state.delivered || state.stopped || state.interrupt_settled) {
            break;
        }
        drop(state);
        assert!(Instant::now() < deadline, "delivery was not acknowledged");
        std::thread::yield_now();
    }
}
fn answer_request(f: &Fixture, text: &str) -> SendTurnRequest {
    let mut request = request(&f.bot);
    request.markdown.clear();
    request.answers = Some(NextMessageAnswers {
        invocation: f.row.id,
        answers: vec![AgentAnswer {
            question_id: "question".into(),
            value: AgentAnswerValue::Text { text: text.into() },
        }],
    });
    request
}
/// Events in a one-card asking turn: TurnStarted, QuestionsRequired, the
/// reply delta and the terminal event.
const ONE_CARD_ASKING_EVENTS: u64 = 4;
/// Replay cursor after a one-card asking turn has been fully committed.
const ONE_CARD_ASKING_CURSOR: u64 = s::AGENT_EVENT_INITIAL_CURSOR + ONE_CARD_ASKING_EVENTS;
fn answer_context(f: &Fixture) -> CommandContext {
    answer_context_at(f, ONE_CARD_ASKING_CURSOR)
}
fn answer_context_at(f: &Fixture, sequence: u64) -> CommandContext {
    let old = f.context.prepared.as_ref().unwrap();
    let state = f.native.native.state.lock().unwrap();
    let submission = state.submission.as_ref().unwrap();
    let p = &submission.provenance;
    CommandContext {
        #[cfg(test)]
        memory_for_fixture: None,
        request: f.context.request.clone(),
        prepared: Some(Arc::new(PreparedBotCommand {
            authority: old.authority.clone(),
            chats: old.chats.clone(),
            runtime: Ok(()),
            pins: old.pins.clone(),
            library_sources: old.library_sources.clone(),
            chat_snapshot: old.chat_snapshot.clone(),
            settings: None,
            terminal_question: Some(crate::worker::bots::chat::TerminalQuestionProof::fixture(
                f.row.attempt_id,
                f.row.id,
                koru_ai_core::bots::replay::BotToolSource {
                    kind: koru_ai_core::bots::replay::BotToolSourceKind::Questions,
                    id: f.native.request.to_string(),
                },
                state.fence.clone().unwrap(),
                d::BotSubmissionKey {
                    owner: p.owner,
                    bot: p.bot,
                    request: p.submission,
                },
                koru_ai_core::bots::BotSubmissionIntent {
                    version: 1,
                    submission: submission.clone(),
                },
                blake3::hash(&serde_json::to_vec(&("send", &f.first_request)).unwrap())
                    .to_hex()
                    .to_string(),
                koru_ai_core::bots::replay::BotReplayState {
                    fence: state.fence.clone(),
                    last_sequence: Some(sequence),
                    stop_reason: None,
                },
                if f.native.fail_asking {
                    AttemptStatus::Failed
                } else {
                    AttemptStatus::Complete
                },
                state.delivery.clone(),
            )),
        })),
    }
}

#[test]
fn terminal_answers_create_one_linked_task_and_keep_lossless_history_on_exact_replay() {
    let f = start(false);
    let original_run = f
        .native
        .native
        .state
        .lock()
        .unwrap()
        .submission
        .as_ref()
        .unwrap()
        .request
        .run;
    let text = "long accepted answer ".repeat(50) + "complete";
    let request = answer_request(&f, &text);
    let context = answer_context(&f);
    let stream = send(&f.client, context.clone(), request.clone()).unwrap();
    let accepted = stream.accepted.clone();
    finish(stream);
    settled(&f.native, 2);
    let transcript = f
        .client
        .read_chat(super::super::context(true), f.bot.main_chat)
        .unwrap();
    assert_eq!(transcript.messages.len(), 3, "answers add no user bubble");
    let answered = transcript
        .tool_activities
        .iter()
        .find(|row| row.id == f.row.id)
        .unwrap();
    let saved = AnswersRecord::decode(answered.agent_output.as_deref().unwrap()).unwrap();
    assert_eq!(saved.answers, request.answers.as_ref().unwrap().answers);
    assert_eq!(answered.status, ToolActivityStatus::Complete);
    let state = f.native.native.state.lock().unwrap();
    let submission = state.submission.as_ref().unwrap();
    assert_eq!(submission.provenance.continued_from, Some(original_run));
    assert_ne!(submission.request.run, original_run);
    assert!(state.input.as_ref().unwrap().text.contains(&text));
    drop(state);
    let replay = send(&f.client, context.clone(), request.clone()).unwrap();
    assert!(replay.accepted.replayed);
    assert_eq!(replay.accepted.attempt_id, accepted.attempt_id);
    assert_eq!(replay.accepted.user_message_id, accepted.user_message_id);
    let mut changed = request.clone();
    changed.answers.as_mut().unwrap().answers[0].value = AgentAnswerValue::Text {
        text: "changed under the same operation".into(),
    };
    assert_eq!(
        send(&f.client, context.clone(), changed)
            .err()
            .unwrap()
            .code,
        "request_conflict"
    );
    assert!(send(&f.client, context, answer_request(&f, "competing answer")).is_err());
    assert_eq!(f.native.native.state.lock().unwrap().begin, 2);
    let stream = send(&f.client, f.context.clone(), super::request(&f.bot)).unwrap();
    finish(stream);
    settled(&f.native, 3);
    assert!(
        f.native
            .native
            .state
            .lock()
            .unwrap()
            .input
            .as_ref()
            .unwrap()
            .text
            .contains(&text)
    );
    let later_replay = send(&f.client, f.context.clone(), request).unwrap();
    assert!(later_replay.accepted.replayed);
    assert_eq!(
        later_replay.accepted.user_message_id,
        accepted.user_message_id
    );
    assert_eq!(later_replay.accepted.attempt_id, accepted.attempt_id);
    assert_eq!(f.native.native.state.lock().unwrap().begin, 3);
    assert_eq!(f.native.native_answers.load(Ordering::SeqCst), 0);
    f.worker.shutdown();
    let _ = f.worker.join();
}

#[test]
fn task_document_context_survives_answer_then_failed_answer_retry() {
    let f = start_with_context_options(false, 1, false, false, true, true);
    let stream = send(
        &f.client,
        answer_context(&f),
        answer_request(&f, "accepted answer"),
    )
    .unwrap();
    let answered = stream.accepted.clone();
    finish_as(stream, false);
    settled(&f.native, 2);
    let retry = RetryTurnRequest {
        destination_workspace: Some(f.bot.home_workspace),
        operation_id: Uuid::new_v4(),
        message_id: answered.assistant_message_id,
        surface: AiSurface::Standalone,
        explicit_refs: vec![],
        reasoning_override: None,
        web_search: false,
        replace_pinned_documents: false,
    };
    let (reply, response) = sync_channel(1);
    f.client
        .send(WorkerCommand::RetryTurn {
            context: f.context.clone(),
            chat: f.bot.main_chat,
            request: retry,
            reply,
        })
        .unwrap();
    let stream = response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    let retried = stream.accepted.clone();
    finish(stream);
    settled(&f.native, 3);
    assert_eq!(f.native.native.state.lock().unwrap().begin, 3);
    f.worker.shutdown();
    assert_eq!(f.worker.join(), WorkerExit::Clean);
    let repo = f.returned.recv_timeout(Duration::from_secs(5)).unwrap();
    for attempt in [f.row.attempt_id, answered.attempt_id, retried.attempt_id] {
        assert_eq!(
            repo.attempt(attempt)
                .unwrap()
                .unwrap()
                .context_document_count,
            1
        );
        let receipt = repo.bot_submission_for_attempt(attempt).unwrap().unwrap();
        assert_eq!(receipt.intent.submission.request.context.len(), 2);
        assert!(receipt.intent.submission.policy.selected_pages.is_empty());
    }
    assert_eq!(repo.pinned_documents(f.bot.main_chat).unwrap().len(), 1);
    assert_eq!(
        repo.messages(f.bot.main_chat)
            .unwrap()
            .iter()
            .filter(|message| message.role == ChatRole::User)
            .count(),
        1
    );
}

#[test]
fn terminal_secret_reaches_only_transient_input_and_saved_retry_refuses_placeholders() {
    let f = start(true);
    let secret = " exact secret value ";
    let stream = send(&f.client, answer_context(&f), answer_request(&f, secret)).unwrap();
    let assistant = stream.accepted.assistant_message_id;
    finish_as(stream, false);
    settled(&f.native, 2);
    let transcript = f
        .client
        .read_chat(super::super::context(true), f.bot.main_chat)
        .unwrap();
    let row = transcript
        .tool_activities
        .iter()
        .find(|r| r.id == f.row.id)
        .unwrap();
    assert!(
        QuestionsRecord::decode(row.agent_input.as_deref().unwrap())
            .unwrap()
            .questions[0]
            .secret
    );
    assert_eq!(
        AnswersRecord::decode(row.agent_output.as_deref().unwrap())
            .unwrap()
            .answers[0]
            .value,
        AgentAnswerValue::Withheld
    );
    let state = f.native.native.state.lock().unwrap();
    assert!(state.input.as_ref().unwrap().text.contains(secret));
    assert!(
        !state
            .submission
            .as_ref()
            .unwrap()
            .input
            .text
            .contains(secret)
    );
    drop(state);
    let retry = RetryTurnRequest {
        destination_workspace: Some(f.bot.home_workspace),
        operation_id: Uuid::new_v4(),
        message_id: assistant,
        surface: AiSurface::Standalone,
        explicit_refs: Vec::new(),
        reasoning_override: None,
        web_search: false,
        replace_pinned_documents: false,
    };
    let (reply, response) = sync_channel(1);
    f.client
        .send(WorkerCommand::RetryTurn {
            context: f.context.clone(),
            chat: f.bot.main_chat,
            request: retry,
            reply,
        })
        .unwrap();
    assert_eq!(
        response
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .err()
            .unwrap()
            .code,
        "questions_closed"
    );
    finish(send(&f.client, f.context.clone(), super::request(&f.bot)).unwrap());
    settled(&f.native, 3);
    assert!(
        !f.native
            .native
            .state
            .lock()
            .unwrap()
            .input
            .as_ref()
            .unwrap()
            .text
            .contains(secret)
    );
    assert_eq!(f.native.native_answers.load(Ordering::SeqCst), 0);
    f.worker.shutdown();
    let _ = f.worker.join();
}

#[test]
fn terminal_skip_is_local_and_missing_live_definitions_refuse_after_restart() {
    let f = start(false);
    let skipped = f
        .client
        .deny_tool_call(super::super::context(true), f.row.id, None)
        .unwrap();
    assert_eq!(skipped.activity.status, ToolActivityStatus::Denied);
    assert_eq!(f.native.native_answers.load(Ordering::SeqCst), 0);
    assert_eq!(f.native.native.state.lock().unwrap().begin, 1);
    f.worker.shutdown();
    let _ = f.worker.join();

    let f = start(false);
    let request = answer_request(&f, "new input");
    f.worker.shutdown();
    let _ = f.worker.join();
    let repository = f.returned.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-in-memory-root"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(f.authority)
    .with_bot_delegate(f.native.clone());
    config.test_repository = Some(repository);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    assert_eq!(
        send(&client, super::super::context(true).into(), request)
            .err()
            .unwrap()
            .code,
        "questions_closed"
    );
    assert!(
        client
            .deny_tool_call(super::super::context(true), f.row.id, None)
            .is_err()
    );
    assert_eq!(f.native.native_answers.load(Ordering::SeqCst), 0);
    assert_eq!(f.native.native.state.lock().unwrap().begin, 1);
    worker.shutdown();
    let _ = worker.join();
}

#[test]
fn stale_proof_invalid_and_oversized_answers_leave_terminal_card_open() {
    let f = start(false);
    let request = answer_request(&f, "valid text");
    assert_eq!(
        send(
            &f.client,
            answer_context_at(&f, ONE_CARD_ASKING_CURSOR - 1),
            request.clone()
        )
        .err()
        .unwrap()
        .code,
        "questions_closed"
    );
    let mut invalid = request.clone();
    invalid.operation_id = Uuid::new_v4();
    invalid.answers.as_mut().unwrap().answers[0].question_id = "not-offered".into();
    assert!(send(&f.client, answer_context(&f), invalid).is_err());
    assert!(
        send(
            &f.client,
            answer_context(&f),
            answer_request(&f, &"large".repeat(10_000))
        )
        .is_err()
    );
    assert!(send(&f.client, super::super::context(false).into(), request).is_err());
    let transcript = f
        .client
        .read_chat(super::super::context(true), f.bot.main_chat)
        .unwrap();
    assert_eq!(transcript.messages.len(), 2);
    assert_eq!(
        transcript
            .tool_activities
            .iter()
            .find(|row| row.id == f.row.id)
            .unwrap()
            .approval,
        ToolApproval::Pending
    );
    assert_eq!(f.native.native.state.lock().unwrap().begin, 1);
    assert_eq!(f.native.native_answers.load(Ordering::SeqCst), 0);
    f.worker.shutdown();
    let _ = f.worker.join();
}

#[test]
fn retained_card_bound_closes_excess_as_unavailable_without_fabricating_user_skip() {
    let f = start_with_cards(false, 65);
    let transcript = f
        .client
        .read_chat(super::super::context(true), f.bot.main_chat)
        .unwrap();
    assert_eq!(transcript.tool_activities.len(), 65);
    assert_eq!(
        transcript
            .tool_activities
            .iter()
            .filter(|row| !row.status.is_terminal())
            .count(),
        64
    );
    let expired = transcript
        .tool_activities
        .iter()
        .find(|row| row.status.is_terminal())
        .unwrap();
    assert_eq!(expired.status, ToolActivityStatus::Cancelled);
    assert_eq!(expired.error_code, Some(ToolErrorCode::Cancelled));
    assert!(expired.live.is_none());
    assert!(
        transcript
            .tool_activities
            .iter()
            .all(|row| row.status != ToolActivityStatus::Denied)
    );
    assert_eq!(f.native.native_answers.load(Ordering::SeqCst), 0);
    f.worker.shutdown();
    let _ = f.worker.join();
}

/// The previous task's owner does not answer within the preparation's bound
/// (`LocalBotDelegate::execute_within` fails retryable, as `LocalDelegate`
/// does when its socket times out): the answer says the owner did not
/// answer, not that the task has not settled.
#[test]
fn an_owner_that_does_not_answer_the_settled_check_is_named_as_such() {
    struct Silent;
    impl BotDelegatePort for Silent {
        fn execute(&self, _: D) -> Result<BotReply, koru_delegate_api::Failure> {
            Err(koru_delegate_api::Failure::unavailable())
        }
    }
    let f = start(false);
    let question = answer_context(&f)
        .prepared
        .as_ref()
        .unwrap()
        .terminal_question
        .clone()
        .unwrap();
    let port: Arc<dyn BotDelegatePort> = Arc::new(Silent);
    let refused = question.verify(Some(&port)).unwrap_err();
    assert_eq!(refused.code, "bot_unavailable");
    assert!(refused.retryable);
    f.worker.shutdown();
    let _ = f.worker.join();
}

#[test]
fn pending_prior_settlement_and_revoked_preparation_authority_refuse_new_answer_task() {
    let f = start(false);
    let context = answer_context(&f);
    let question = context
        .prepared
        .as_ref()
        .unwrap()
        .terminal_question
        .clone()
        .unwrap();
    f.native.native.state.lock().unwrap().delivered = false;
    let port: Arc<dyn BotDelegatePort> = f.native.clone();
    let result = std::thread::Builder::new()
        .name("koru-ai-provider-fixture".into())
        .spawn(move || question.verify(Some(&port)))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(result.unwrap_err().code, "chat_busy");
    f.native.native.state.lock().unwrap().delivered = true;
    f.authority.allowed.store(false, Ordering::SeqCst);
    assert!(
        send(
            &f.client,
            context,
            answer_request(&f, "answer after revocation")
        )
        .is_err()
    );
    f.authority.allowed.store(true, Ordering::SeqCst);
    let transcript = f
        .client
        .read_chat(super::super::context(true), f.bot.main_chat)
        .unwrap();
    assert_eq!(transcript.messages.len(), 2);
    assert_eq!(
        transcript
            .tool_activities
            .iter()
            .find(|row| row.id == f.row.id)
            .unwrap()
            .approval,
        ToolApproval::Pending
    );
    assert_eq!(f.native.native.state.lock().unwrap().begin, 1);
    assert_eq!(f.native.native_answers.load(Ordering::SeqCst), 0);
    f.worker.shutdown();
    let _ = f.worker.join();
}

#[test]
fn active_native_resolution_near_retention_cap_remains_authoritative_before_terminal() {
    let f = start_with_resolution(false, 65, true);
    let transcript = f
        .client
        .read_chat(super::super::context(true), f.bot.main_chat)
        .unwrap();
    assert_eq!(transcript.tool_activities.len(), 65);
    assert_eq!(
        transcript
            .tool_activities
            .iter()
            .filter(|row| !row.status.is_terminal())
            .count(),
        64
    );
    assert_eq!(
        transcript
            .tool_activities
            .iter()
            .filter(|row| row.status == ToolActivityStatus::Cancelled)
            .count(),
        1
    );
    assert_eq!(f.native.native_answers.load(Ordering::SeqCst), 0);
    assert_eq!(f.native.native.state.lock().unwrap().begin, 1);
    f.worker.shutdown();
    let _ = f.worker.join();
}

#[test]
fn accepted_answers_to_a_failed_asking_reply_survive_in_later_task_history() {
    let f = start_with_options(false, 1, false, true);
    let text = "answer retained after the asking reply failed";
    finish(send(&f.client, answer_context(&f), answer_request(&f, text)).unwrap());
    settled(&f.native, 2);
    finish(send(&f.client, f.context.clone(), request(&f.bot)).unwrap());
    settled(&f.native, 3);
    let state = f.native.native.state.lock().unwrap();
    assert!(state.input.as_ref().unwrap().text.contains(text));
    assert_eq!(state.begin, 3);
    drop(state);
    assert_eq!(f.native.native_answers.load(Ordering::SeqCst), 0);
    f.worker.shutdown();
    let _ = f.worker.join();
}
