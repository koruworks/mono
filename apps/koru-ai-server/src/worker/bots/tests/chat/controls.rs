// SPDX-License-Identifier: MPL-2.0
use super::*;

struct Interactive {
    native: Native,
    approval: s::ApprovalId,
    choice: s::ApprovalChoiceId,
    questions: bool,
    request: s::QuestionRequestId,
    accepted: AtomicBool,
    publish: AtomicBool,
    gate: Mutex<Option<(SyncSender<()>, Receiver<()>)>>,
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
            gate: Mutex::new(None),
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
                let cursor = s::StreamSequence::new(native.len() as u64).unwrap();
                let events = native
                    .into_iter()
                    .enumerate()
                    .filter(|(n, _)| after.is_none_or(|cursor| *n as u64 + 1 > cursor.get()))
                    .map(|(n, event)| s::SequencedAgentEvent {
                        session_id: fence.session,
                        sequence: s::StreamSequence::new(n as u64 + 1).unwrap(),
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
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let chat = repository
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    let context = proof(authority.clone(), &bot, chat);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-in-memory-root"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority.clone())
    .with_bot_delegate(native);
    config.test_repository = Some(repository);
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
