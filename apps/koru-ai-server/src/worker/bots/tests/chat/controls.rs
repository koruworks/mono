// SPDX-License-Identifier: MPL-2.0
use super::*;
mod settings;

struct Interactive {
    native: Native,
    approval: s::ApprovalId,
    choice: s::ApprovalChoiceId,
    questions: bool,
    request: s::QuestionRequestId,
    accepted: AtomicBool,
    publish: AtomicBool,
    complete: AtomicBool,
    fail_complete: bool,
    gate: Mutex<Option<(SyncSender<()>, Receiver<()>)>>,
    steer_inputs: Mutex<Vec<s::PendingInput>>,
    lose_steer_reply: AtomicBool,
}
impl Interactive {
    fn new() -> Self {
        Self {
            native: Native::default(),
            approval: s::ApprovalId::mint(),
            choice: s::ApprovalChoiceId::mint(),
            questions: false,
            request: s::QuestionRequestId::mint(),
            accepted: AtomicBool::new(false),
            publish: AtomicBool::new(false),
            complete: AtomicBool::new(false),
            fail_complete: false,
            gate: Mutex::new(None),
            steer_inputs: Mutex::new(Vec::new()),
            lose_steer_reply: AtomicBool::new(false),
        }
    }
}
impl BotDelegatePort for Interactive {
    fn execute(&self, command: D) -> Result<BotReply, koru_delegate_api::Failure> {
        match command {
            D::Observe { fence, after } if fence.turn.is_some() => {
                assert_eq!(
                    self.native.state.lock().unwrap().fence.as_ref(),
                    Some(&fence)
                );
                let turn_id = fence.turn.unwrap();
                let mut native = vec![
                    s::AgentEvent::TurnStarted { turn_id },
                    s::AgentEvent::ApprovalRequired {
                        turn_id,
                        approval_id: self.approval,
                        summary: "Permission fixture".into(),
                        choices: vec![s::ApprovalChoice {
                            id: self.choice,
                            intent: s::ApprovalIntent::Reject,
                            label: "Reject".into(),
                        }],
                    },
                ];
                if self.questions {
                    native[1] = s::AgentEvent::QuestionsRequired {
                        turn_id,
                        request_id: self.request,
                        blocking: true,
                        delivery: s::QuestionsDelivery::InTurn,
                        questions: vec![s::AgentQuestion {
                            id: "question".into(),
                            header: String::new(),
                            question: "Choice?".into(),
                            options: vec![s::QuestionOption {
                                label: "Exact choice".into(),
                                description: String::new(),
                            }],
                            allow_other: false,
                            secret: false,
                        }],
                    };
                }
                if self.publish.load(Ordering::SeqCst) {
                    native.push(s::AgentEvent::ApprovalResolved {
                        turn_id,
                        approval_id: self.approval,
                        decision: s::ApprovalDecision::Reject,
                        choice: Some(self.choice),
                    });
                }
                if self.complete.load(Ordering::SeqCst) {
                    native.push(s::AgentEvent::MessageDelta {
                        turn_id,
                        text: "NATIVE-REPLY".into(),
                    });
                    native.push(if self.fail_complete {
                        s::AgentEvent::TurnFailed {
                            turn_id,
                            error: s::SessionFailure {
                                code: s::ErrorCode::ProviderUnavailable,
                                message: "Fixture failure".into(),
                            },
                        }
                    } else {
                        s::AgentEvent::TurnCompleted { turn_id }
                    });
                }
                let cursor =
                    s::StreamSequence::new(s::AGENT_EVENT_INITIAL_CURSOR + native.len() as u64)
                        .unwrap();
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
                Ok(BotReply::Observation {
                    observation: BotObservation {
                        fence,
                        observation: s::AgentEventObservation {
                            events,
                            cursor,
                            earlier_truncated: false,
                            commands: None,
                            session_cursor: None,
                        },
                    },
                })
            }
            D::AnswerApproval {
                fence,
                request: _,
                approval,
                decision,
                choice,
            } => {
                assert_eq!(
                    std::thread::current().name(),
                    Some("koru-ai-provider-control")
                );
                assert_eq!(
                    self.native.state.lock().unwrap().fence.as_ref(),
                    Some(&fence)
                );
                assert_eq!(approval, self.approval);
                assert_eq!(decision, s::ApprovalDecision::Reject);
                assert_eq!(choice, Some(self.choice));
                if let Some((entered, release)) = self.gate.lock().unwrap().take() {
                    entered.send(()).unwrap();
                    release.recv().unwrap();
                }
                self.accepted.store(true, Ordering::SeqCst);
                Ok(BotReply::Applied)
            }
            D::AnswerQuestions {
                fence,
                request,
                answers,
            } => {
                assert!(self.questions);
                assert_eq!(
                    std::thread::current().name(),
                    Some("koru-ai-provider-control")
                );
                assert_eq!(
                    self.native.state.lock().unwrap().fence.as_ref(),
                    Some(&fence)
                );
                assert_eq!(request, self.request);
                assert_eq!(
                    answers,
                    vec![s::QuestionAnswer {
                        question_id: "question".into(),
                        value: s::QuestionAnswerValue::Option {
                            label: "Exact choice".into()
                        }
                    }]
                );
                self.accepted.store(true, Ordering::SeqCst);
                Ok(BotReply::Applied)
            }
            D::SkipQuestions { fence, request } => {
                assert!(self.questions);
                assert_eq!(
                    std::thread::current().name(),
                    Some("koru-ai-provider-control")
                );
                assert_eq!(
                    self.native.state.lock().unwrap().fence.as_ref(),
                    Some(&fence)
                );
                assert_eq!(request, self.request);
                self.accepted.store(true, Ordering::SeqCst);
                Ok(BotReply::Applied)
            }
            D::Steer { fence, input } => {
                assert_eq!(
                    std::thread::current().name(),
                    Some("koru-ai-provider-control")
                );
                assert_eq!(
                    self.native.state.lock().unwrap().fence.as_ref(),
                    Some(&fence)
                );
                input.validate_conditional_steering().unwrap();
                self.steer_inputs.lock().unwrap().push(input);
                if let Some((entered, release)) = self.gate.lock().unwrap().take() {
                    entered.send(()).unwrap();
                    release.recv().unwrap();
                }
                if self.lose_steer_reply.swap(false, Ordering::SeqCst) {
                    return Err(koru_delegate_api::Failure {
                        code: "bot_unavailable".into(),
                        message: "Lost reply fixture".into(),
                        retryable: true,
                        details: Default::default(),
                    });
                }
                Ok(BotReply::Turn {
                    turn: fence.turn.unwrap(),
                })
            }
            other => self.native.execute(other),
        }
    }
}

fn start(
    native: Arc<Interactive>,
) -> (
    WorkerHandle,
    AiWorkerClient,
    Arc<Authority>,
    CommandContext,
    ToolInvocationId,
) {
    start_with_return(native, None)
}
fn start_with_return(
    native: Arc<Interactive>,
    returned: Option<SyncSender<Repository>>,
) -> (
    WorkerHandle,
    AiWorkerClient,
    Arc<Authority>,
    CommandContext,
    ToolInvocationId,
) {
    start_in(
        native,
        returned,
        PathBuf::from("unused-in-memory-root"),
        |_, _| {},
    )
}
/// The same, on `root`, with `seed` writing earlier history first.
fn start_in(
    native: Arc<Interactive>,
    returned: Option<SyncSender<Repository>>,
    root: PathBuf,
    seed: impl FnOnce(&mut Repository, &Bot),
) -> (
    WorkerHandle,
    AiWorkerClient,
    Arc<Authority>,
    CommandContext,
    ToolInvocationId,
) {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    seed(&mut repository, &bot);
    let chat = repository
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    let context = proof(authority.clone(), &bot, chat);
    let mut config = WorkerConfig::new(root, "internal-debug", ProviderRegistry::default())
        .with_bot_authority(authority.clone())
        .with_bot_delegate(native);
    config.test_repository = Some(repository);
    config.test_repository_return = returned;
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::SendTurn {
            context: context.clone(),
            request: request(&bot),
            reply,
        })
        .unwrap();
    let stream = response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    let invocation = loop {
        let event = stream
            .events
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .event;
        if let PublicAiEvent::ToolActivity { activity, .. } = event {
            if activity.status == ToolActivityStatus::ApprovalRequired {
                break activity.id;
            }
        }
    };
    (worker, client, authority, context, invocation)
}

#[test]
fn native_choice_is_exact_and_ack_does_not_settle_the_permission_row() {
    let native = Arc::new(Interactive::new());
    let (worker, client, _authority, _context, invocation) = start(native.clone());
    assert!(
        client
            .deny_tool_call(
                super::super::context(true),
                invocation,
                Some(s::ApprovalChoiceId::mint().to_string())
            )
            .is_err()
    );
    assert!(!native.accepted.load(Ordering::SeqCst));
    let response = client
        .deny_tool_call(
            super::super::context(true),
            invocation,
            Some(native.choice.to_string()),
        )
        .unwrap();
    assert!(native.accepted.load(Ordering::SeqCst));
    assert_eq!(response.activity.approval, ToolApproval::Pending);
    assert_eq!(
        response.activity.status,
        ToolActivityStatus::ApprovalRequired
    );
    native.publish.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let transcript = client
            .read_chat(super::super::context(true), response.activity.chat_id)
            .unwrap();
        if transcript
            .tool_activities
            .iter()
            .any(|row| row.id == invocation && row.approval == ToolApproval::Denied)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native resolution was not observed"
        );
        std::thread::yield_now();
    }
    assert!(
        client
            .deny_tool_call(
                super::super::context(true),
                invocation,
                Some(native.choice.to_string())
            )
            .is_err()
    );
    worker.shutdown();
    let _ = worker.join();
}

#[test]
fn authority_revoked_during_native_answer_refuses_completion_content() {
    let native = Arc::new(Interactive::new());
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    *native.gate.lock().unwrap() = Some((entered_tx, release));
    let (worker, client, authority, context, invocation) = start(native.clone());
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::DenyToolCall {
            context,
            tool: invocation,
            choice: Some(native.choice.to_string()),
            reply,
        })
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    authority.allowed.store(false, Ordering::SeqCst);
    release_tx.send(()).unwrap();
    assert!(
        response
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err()
    );
    assert!(native.accepted.load(Ordering::SeqCst));
    worker.shutdown();
    let _ = worker.join();
}

#[test]
fn native_question_answers_validate_exact_definition_and_skip_has_its_own_command() {
    for skip in [false, true] {
        let mut interactive = Interactive::new();
        interactive.questions = true;
        let native = Arc::new(interactive);
        let (worker, client, _authority, _context, invocation) = start(native.clone());
        if skip {
            let response = client
                .deny_tool_call(super::super::context(true), invocation, None)
                .unwrap();
            assert_eq!(response.activity.approval, ToolApproval::Pending);
        } else {
            for value in [
                koru_ai_core::AgentAnswerValue::Withheld,
                koru_ai_core::AgentAnswerValue::Option {
                    label: "Not offered".into(),
                },
            ] {
                assert!(
                    client
                        .approve_tool_call(
                            super::super::context(true),
                            invocation,
                            koru_ai_wire::ToolDecisionRequest {
                                choice: None,
                                answers: Some(vec![koru_ai_core::AgentAnswer {
                                    question_id: "question".into(),
                                    value
                                }]),
                            }
                        )
                        .is_err()
                );
                assert!(!native.accepted.load(Ordering::SeqCst));
            }
            let response = client
                .approve_tool_call(
                    super::super::context(true),
                    invocation,
                    koru_ai_wire::ToolDecisionRequest {
                        choice: None,
                        answers: Some(vec![koru_ai_core::AgentAnswer {
                            question_id: "question".into(),
                            value: koru_ai_core::AgentAnswerValue::Option {
                                label: "Exact choice".into(),
                            },
                        }]),
                    },
                )
                .unwrap();
            assert_eq!(response.activity.approval, ToolApproval::Pending);
        }
        assert!(native.accepted.load(Ordering::SeqCst));
        worker.shutdown();
        let _ = worker.join();
    }
}

fn steer_request() -> koru_ai_wire::SteerAttemptRequest {
    koru_ai_wire::SteerAttemptRequest {
        operation_id: Uuid::new_v4(),
        markdown: "Additional direction".into(),
    }
}
fn native_attempt(native: &Interactive) -> InferenceAttemptId {
    InferenceAttemptId::parse(
        &native
            .native
            .state
            .lock()
            .unwrap()
            .submission
            .as_ref()
            .unwrap()
            .provenance
            .ai_attempt_id
            .unwrap()
            .to_string(),
    )
    .unwrap()
}

#[test]
fn uncertain_steer_retries_only_its_live_key_and_persists_after_ack() {
    let native = Arc::new(Interactive::new());
    native.lose_steer_reply.store(true, Ordering::SeqCst);
    let (worker, client, _authority, context, _) = start(native.clone());
    let attempt = native_attempt(&native);
    let chat = context.prepared.as_ref().unwrap().chats[0].1;
    let request = steer_request();
    let failure = client
        .steer_attempt(super::super::context(true), attempt, request.clone())
        .unwrap_err();
    assert_eq!(failure.code, "steer_failed");
    assert!(
        !failure.retryable,
        "uncertainty must not request an automatic new send"
    );
    assert!(
        !client
            .read_chat(super::super::context(true), chat)
            .unwrap()
            .messages
            .iter()
            .any(|row| row.id == MessageId::from_uuid(request.operation_id))
    );
    let accepted = client
        .steer_attempt(super::super::context(true), attempt, request.clone())
        .unwrap();
    assert!(!accepted.replayed);
    assert_eq!(
        accepted.user_message_id,
        MessageId::from_uuid(request.operation_id)
    );
    let replay = client
        .steer_attempt(super::super::context(true), attempt, request.clone())
        .unwrap();
    assert!(replay.replayed);
    let sent = native.steer_inputs.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[0], sent[1],
        "lost acknowledgment retry preserves exact native key/input"
    );
    assert_eq!(sent[0].idempotency_key.as_uuid(), request.operation_id);
    drop(sent);
    let mut conflicting = request.clone();
    conflicting.markdown.push_str(" changed");
    assert_eq!(
        client
            .steer_attempt(super::super::context(true), attempt, conflicting)
            .unwrap_err()
            .code,
        "idempotency_conflict"
    );
    let transcript = client.read_chat(super::super::context(true), chat).unwrap();
    assert_eq!(
        transcript
            .messages
            .iter()
            .filter(|row| row.id == accepted.user_message_id)
            .count(),
        1
    );
    assert_eq!(
        transcript.messages.last().unwrap().markdown,
        request.markdown
    );
    worker.shutdown();
    let _ = worker.join();
}

#[test]
fn native_acceptance_then_worker_exit_does_not_invent_saved_success_or_resend_after_restart() {
    let native = Arc::new(Interactive::new());
    let (returned_tx, returned) = sync_channel(1);
    let (worker, client, authority, context, _) =
        start_with_return(native.clone(), Some(returned_tx));
    let attempt = native_attempt(&native);
    let request = steer_request();
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    *native.gate.lock().unwrap() = Some((entered_tx, release));
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::SteerAttempt {
            context,
            attempt,
            request: request.clone(),
            reply,
        })
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        client
            .steer_attempt(super::super::context(true), attempt, request.clone())
            .unwrap_err()
            .code,
        "idempotency_conflict"
    );
    assert_eq!(native.steer_inputs.lock().unwrap().len(), 1);
    // Native has accepted the exact input, but the AI writer exits before the
    // acknowledgment can cause its SQL insert. This is O20's explicit gap.
    worker.shutdown();
    release_tx.send(()).unwrap();
    let _ = worker.join();
    assert!(!matches!(
        response.recv_timeout(Duration::from_secs(5)),
        Ok(Ok(_))
    ));
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        repository
            .message(MessageId::from_uuid(request.operation_id))
            .unwrap()
            .is_none()
    );
    let begin = native.native.state.lock().unwrap().begin;
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-in-memory-root"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(native.clone());
    config.test_repository = Some(repository);
    let restarted = AiWorker::new(config).spawn().unwrap();
    let failure = restarted
        .client()
        .steer_attempt(super::super::context(true), attempt, request)
        .unwrap_err();
    assert_eq!(failure.code, "steer_failed");
    assert!(!failure.retryable);
    assert_eq!(native.steer_inputs.lock().unwrap().len(), 1);
    assert_eq!(native.native.state.lock().unwrap().begin, begin);
    restarted.shutdown();
    let _ = restarted.join();
}

#[test]
fn access_lost_after_native_steer_ack_reports_uncertainty_without_saved_success() {
    let native = Arc::new(Interactive::new());
    let (returned_tx, returned) = sync_channel(1);
    let (worker, client, authority, context, _) =
        start_with_return(native.clone(), Some(returned_tx));
    let attempt = native_attempt(&native);
    let request = steer_request();
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    *native.gate.lock().unwrap() = Some((entered_tx, release));
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::SteerAttempt {
            context,
            attempt,
            request: request.clone(),
            reply,
        })
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    authority.allowed.store(false, Ordering::SeqCst);
    release_tx.send(()).unwrap();
    let error = response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, "steer_failed");
    assert!(!error.retryable);
    worker.shutdown();
    let _ = worker.join();
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        repository
            .message(MessageId::from_uuid(request.operation_id))
            .unwrap()
            .is_none()
    );
    assert_eq!(native.steer_inputs.lock().unwrap().len(), 1);
}

#[test]
fn delayed_ack_history_stays_with_its_terminal_reply_and_fences_new_tasks() {
    for retry in [false, true] {
        let native = Arc::new(Interactive {
            fail_complete: retry,
            ..Interactive::new()
        });
        let (worker, client, authority, context, _) = start(native.clone());
        let attempt = native_attempt(&native);
        let chat = context
            .prepared
            .as_ref()
            .unwrap()
            .chat_snapshot
            .as_ref()
            .unwrap()
            .id;
        let assistant = MessageId::from_uuid(
            native
                .native
                .state
                .lock()
                .unwrap()
                .submission
                .as_ref()
                .unwrap()
                .provenance
                .assistant_message_id
                .unwrap(),
        );
        let steer = steer_request();
        let (entered_tx, entered) = sync_channel(1);
        let (release_tx, release) = sync_channel(1);
        *authority.inspect_gate.lock().unwrap() = Some((entered_tx, release));
        let (reply, response) = sync_channel(1);
        client
            .send(WorkerCommand::SteerAttempt {
                context: context.clone(),
                attempt,
                request: steer.clone(),
                reply,
            })
            .unwrap();
        // The native acknowledgment has arrived; fresh authority work is held
        // off the writer while native observations may still commit.
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let (second_reply, second_response) = sync_channel(1);
        client
            .send(WorkerCommand::SteerAttempt {
                context: context.clone(),
                attempt,
                request: steer_request(),
                reply: second_reply,
            })
            .unwrap();
        assert!(
            matches!(second_response.recv_timeout(Duration::from_secs(5)).unwrap(),
            Err(error) if error.code == "chat_busy")
        );
        assert_eq!(native.steer_inputs.lock().unwrap().len(), 1);
        native.publish.store(true, Ordering::SeqCst);
        native.complete.store(true, Ordering::SeqCst);
        let read = || {
            let (reply, response) = sync_channel(1);
            client
                .send(WorkerCommand::ReadChat {
                    context: context.clone(),
                    chat,
                    reply,
                })
                .unwrap();
            response
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap()
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if read()
                .messages
                .iter()
                .any(|row| row.id == assistant && row.state.is_terminal())
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "terminal observation did not commit"
            );
            std::thread::yield_now();
        }
        let operation = Uuid::new_v4();
        let next = || {
            let (reply, response) = sync_channel(1);
            let command = if retry {
                WorkerCommand::RetryTurn {
                    context: context.clone(),
                    chat,
                    request: RetryTurnRequest {
                        destination_workspace: Some(authority.workspace),
                        operation_id: operation,
                        message_id: assistant,
                        surface: AiSurface::Standalone,
                        explicit_refs: vec![],
                        reasoning_override: None,
                        web_search: false,
                        replace_pinned_documents: false,
                    },
                    reply,
                }
            } else {
                WorkerCommand::SendTurn {
                    context: context.clone(),
                    request: SendTurnRequest {
                        destination_workspace: Some(authority.workspace),
                        operation_id: operation,
                        chat_id: chat,
                        surface: AiSurface::Standalone,
                        markdown: "CURRENT-TASK".into(),
                        explicit_refs: vec![],
                        web_search: false,
                        reasoning_override: None,
                        system_prompt_override: None,
                        replace_pinned_documents: false,
                        attachment_ids: vec![],
                        answers: None,
                        skills: vec![],
                    },
                    reply,
                }
            };
            client.send(command).unwrap();
            response.recv_timeout(Duration::from_secs(5)).unwrap()
        };
        assert!(matches!(next(), Err(error) if error.code == "chat_busy"));
        assert_eq!(native.native.state.lock().unwrap().begin, 1);
        assert!(
            !read()
                .messages
                .iter()
                .any(|row| row.id == MessageId::from_uuid(steer.operation_id))
        );
        release_tx.send(()).unwrap();
        let accepted = response
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        let transcript = read();
        assert_eq!(
            transcript.messages.last().unwrap().id,
            accepted.user_message_id
        );
        let prefix = koru_ai_core::read_through(&transcript.messages, assistant).unwrap();
        assert_eq!(
            prefix
                .iter()
                .map(|row| row.markdown.as_str())
                .collect::<Vec<_>>(),
            ["CURRENT-TASK", "Additional direction", "NATIVE-REPLY"]
        );
        assert_eq!(native.steer_inputs.lock().unwrap().len(), 1);
        assert_eq!(native.native.state.lock().unwrap().begin, 1);
        // The same refused task identity can now be accepted; the guard did
        // not persist a partial turn or manufacture a different operation.
        assert!(next().is_ok());
        worker.shutdown();
        let _ = worker.join();
    }
}

#[test]
fn bot_branch_waits_for_delayed_steer_history_then_copies_it_in_reading_order() {
    let native = Arc::new(Interactive::new());
    let (worker, client, authority, context, _) = start(native.clone());
    let attempt = native_attempt(&native);
    let prepared = context.prepared.as_ref().unwrap();
    let (bot, chat, revision, policy) = prepared.chats[0];
    let assistant = MessageId::from_uuid(
        native
            .native
            .state
            .lock()
            .unwrap()
            .submission
            .as_ref()
            .unwrap()
            .provenance
            .assistant_message_id
            .unwrap(),
    );
    let destination = ChatId::new();
    // The proof a prepared Branch carries: the source and its unused destination.
    let branch_context = CommandContext {
        memory_for_fixture: None,
        request: super::super::context(true),
        prepared: Some(Arc::new(PreparedBotCommand {
            authority: prepared.authority.clone(),
            chats: vec![
                (bot, chat, revision, policy),
                (bot, destination, revision, policy),
            ],
            runtime: Ok(()),
            pins: vec![],
            library_sources: None,
            chat_snapshot: None,
            settings: None,
            terminal_question: None,
        })),
    };
    let branch = || {
        let (reply, response) = sync_channel(1);
        client
            .send(WorkerCommand::BranchChat {
                context: branch_context.clone(),
                chat,
                request: BranchChatRequest {
                    branch_chat_id: destination,
                    through_message_id: assistant,
                    title: "Branch".into(),
                },
                reply,
            })
            .unwrap();
        response.recv_timeout(Duration::from_secs(5)).unwrap()
    };
    let steer = steer_request();
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    *authority.inspect_gate.lock().unwrap() = Some((entered_tx, release));
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::SteerAttempt {
            context: context.clone(),
            attempt,
            request: steer.clone(),
            reply,
        })
        .unwrap();
    // Native accepted the steer; its history row waits on fresh authority.
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    native.publish.store(true, Ordering::SeqCst);
    native.complete.store(true, Ordering::SeqCst);
    // Reads carry their proofs: fresh preparation would queue behind the held steer.
    let read = |context: &CommandContext, chat| {
        let (reply, response) = sync_channel(1);
        client
            .send(WorkerCommand::ReadChat {
                context: context.clone(),
                chat,
                reply,
            })
            .unwrap();
        response.recv_timeout(Duration::from_secs(5)).unwrap()
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !read(&context, chat)
        .unwrap()
        .messages
        .iter()
        .any(|row| row.id == assistant && row.state.is_terminal())
    {
        assert!(
            Instant::now() < deadline,
            "terminal observation did not commit"
        );
        std::thread::yield_now();
    }
    // Terminal is not settled: a branch now would lose the acknowledged steer.
    assert!(matches!(branch(), Err(error) if error.code == "chat_busy"));
    assert!(read(&branch_context, destination).is_err());
    release_tx.send(()).unwrap();
    response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    let branched = branch().unwrap().transcript;
    assert_eq!(
        koru_ai_core::reading_order(&branched.messages)
            .iter()
            .map(|row| row.markdown.as_str())
            .collect::<Vec<_>>(),
        ["CURRENT-TASK", "Additional direction", "NATIVE-REPLY"]
    );
    assert_eq!(
        branched.messages.last().unwrap().origin_message_id,
        Some(MessageId::from_uuid(steer.operation_id))
    );
    // Branching started nothing: the native owner saw one task only.
    assert_eq!(native.native.state.lock().unwrap().begin, 1);
    assert_eq!(native.steer_inputs.lock().unwrap().len(), 1);
    worker.shutdown();
    let _ = worker.join();
}

#[test]
fn a_steer_sent_while_branch_files_copy_refuses_the_commit_and_the_branch_follows_once_settled() {
    let root = tempfile::tempdir().unwrap();
    let native = Arc::new(Interactive::new());
    let seeded = Mutex::new(None);
    // A finished exchange with a stored picture comes before the running task.
    let (worker, client, authority, context, _) = start_in(
        native.clone(),
        None,
        root.path().to_owned(),
        |repository, bot| {
            let picture = super::super::branch_attachment(bot.main_chat);
            let file = root
                .path()
                .join("attachments")
                .join(bot.main_chat.to_storage())
                .join(picture.attachment.id.to_storage())
                .join("file");
            std::fs::create_dir_all(&file).unwrap();
            std::fs::write(file.join("fixture.png"), b"picture").unwrap();
            repository.stage_attachment(&picture).unwrap();
            let mut finished = turn(bot.main_chat, Uuid::new_v4(), 1);
            finished.attachment_ids = vec![picture.attachment.id];
            repository.begin_turn(&finished).unwrap();
            repository
                .apply_event(
                    &AiEvent::Completed {
                        attempt_id: finished.attempt.id,
                        stop_reason: None,
                        at: Timestamp::from_millis(2),
                    },
                    &[],
                )
                .unwrap();
            *seeded.lock().unwrap() = Some(finished.assistant_message.id);
        },
    );
    let through = seeded.lock().unwrap().unwrap();
    let attempt = native_attempt(&native);
    let prepared = context.prepared.as_ref().unwrap();
    let (bot, chat, revision, policy) = prepared.chats[0];
    let destination = ChatId::new();
    let branch_context = CommandContext {
        memory_for_fixture: None,
        request: super::super::context(true),
        prepared: Some(Arc::new(PreparedBotCommand {
            authority: prepared.authority.clone(),
            chats: vec![
                (bot, chat, revision, policy),
                (bot, destination, revision, policy),
            ],
            runtime: Ok(()),
            pins: vec![],
            library_sources: None,
            chat_snapshot: None,
            settings: None,
            terminal_question: None,
        })),
    };
    let branch = || {
        let (reply, response) = sync_channel(1);
        client
            .send(WorkerCommand::BranchChat {
                context: branch_context.clone(),
                chat,
                request: BranchChatRequest {
                    branch_chat_id: destination,
                    through_message_id: through,
                    title: "Branch".into(),
                },
                reply,
            })
            .unwrap();
        response
    };
    let copied = || {
        std::fs::read_dir(
            root.path()
                .join("attachments")
                .join(destination.to_storage()),
        )
        .map(|entries| entries.count())
        .unwrap_or(0)
    };
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    *authority.inspect_gate.lock().unwrap() = Some((entered_tx, release));
    let first = branch();
    // The steering guard passed and the files are copied; the commit waits
    // for its fresh owner inspection.
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(copied(), 1);
    let steer = steer_request();
    let (reply, steered) = sync_channel(1);
    client
        .send(WorkerCommand::SteerAttempt {
            context: context.clone(),
            attempt,
            request: steer.clone(),
            reply,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while native.steer_inputs.lock().unwrap().is_empty() {
        assert!(
            Instant::now() < deadline,
            "the steer never reached the owner"
        );
        std::thread::yield_now();
    }
    release_tx.send(()).unwrap();
    let refused = first
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap_err();
    assert_eq!(refused.code, "chat_busy");
    assert!(refused.retryable);
    steered
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while copied() != 0 {
        assert!(
            Instant::now() < deadline,
            "the refused copy was left behind"
        );
        std::thread::yield_now();
    }
    // Once the acknowledgement settled, the same identity branches once.
    let branched = branch()
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .transcript;
    assert_eq!(branched.attachments.len(), 1);
    assert_eq!(copied(), 1);
    assert!(
        branched
            .messages
            .iter()
            .all(|row| row.origin_message_id != Some(MessageId::from_uuid(steer.operation_id)))
    );
    worker.shutdown();
    let _ = worker.join();
}

/// `m8_admission_inventory`, row 16 (M8 piece W): steering a Run that is
/// already running is allowed under a team pause. It reaches the live
/// session and starts no attempt. A steer whose turn has ended is refused
/// and starts nothing either.
#[test]
fn m8_admission_inventory_row_16_a_steer_reaches_a_running_run_under_a_team_pause() {
    let native = Arc::new(Interactive::new());
    let (worker, client, _authority, _context, _) = start(native.clone());
    let attempt = native_attempt(&native);
    super::team_pause::team(&client, true, 0);
    let begun = native.native.state.lock().unwrap().begin;
    let request = steer_request();
    let accepted = client
        .steer_attempt(super::super::context(true), attempt, request.clone())
        .unwrap();
    assert!(!accepted.replayed);
    assert_eq!(native.steer_inputs.lock().unwrap().len(), 1);
    assert_eq!(
        native.native.state.lock().unwrap().begin,
        begun,
        "a steer starts no attempt"
    );
    // The Run ends. A steer of that turn is refused and starts nothing.
    client
        .cancel_attempt(super::super::context(true), attempt)
        .unwrap();
    let ended = Instant::now() + Duration::from_secs(10);
    let refused = loop {
        match client.steer_attempt(super::super::context(true), attempt, steer_request()) {
            Err(refused) => break refused,
            Ok(_) => {
                assert!(Instant::now() < ended, "the stopped Run kept taking steers");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    assert!(!refused.retryable, "{}", refused.code);
    assert_eq!(native.native.state.lock().unwrap().begin, begun);
    worker.shutdown();
    let _ = worker.join();
}
