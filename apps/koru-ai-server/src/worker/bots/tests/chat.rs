// SPDX-License-Identifier: MPL-2.0
//! Full AI dispatch using an in-memory repository and fake delegate owner.
use super::*;
use koru_delegate_api::bots::{BotCommand as D, BotDelegatePort, BotObservation, BotReply};
use koru_delegate_core as d;
use koru_session_protocol as s;
use std::sync::Mutex;
mod controls;
mod questions;

#[derive(Default)]
struct Native {
    fail_first: bool,
    allow_answer_tasks: bool,
    state: Mutex<NativeState>,
    submit_gate: Mutex<Option<(SyncSender<()>, Receiver<()>)>>,
    stop_reports: Mutex<Option<SyncSender<()>>>,
    settled_reports: Mutex<Option<SyncSender<()>>>,
    delivery_reports: Mutex<Option<SyncSender<()>>>,
}
#[derive(Default)]
struct NativeState {
    submission: Option<d::BotSubmission>,
    fence: Option<d::BotExecutionFence>,
    begin: usize,
    observed_before_begin: bool,
    delivered: bool,
    delivery: Option<d::BotDeliveryReceipt>,
    input: Option<s::PendingInput>,
    stopped: bool,
    stop_calls: usize,
}
impl BotDelegatePort for Native {
    fn execute(&self, command: D) -> Result<BotReply, koru_delegate_api::Failure> {
        assert!(
            std::thread::current()
                .name()
                .unwrap_or_default()
                .starts_with("koru-ai-provider")
        );
        if matches!(&command, D::PendingNativeEffects { .. }) {
            return Err(koru_delegate_api::Failure::new(
                "fixture_pending",
                "fixture recovery stays pending",
                true,
            ));
        }
        if matches!(&command, D::Submit { .. }) {
            if let Some((entered, release)) = self.submit_gate.lock().unwrap().take() {
                entered.send(()).unwrap();
                let _ = release.recv();
            }
        }
        let mut state = self.state.lock().unwrap();
        match command {
            D::Submit { submission } => {
                assert!(
                    submission.input.text == "CURRENT-TASK"
                        || (self.allow_answer_tasks
                            && koru_ai_core::AnswersRecord::decode(&submission.input.text)
                                .is_some())
                );
                assert!(submission.provenance.ai_attempt_id.is_some());
                let fence = d::BotExecutionFence {
                    owner: submission.provenance.owner,
                    bot: submission.provenance.bot,
                    run: submission.request.run,
                    attempt: d::RunAttemptId::mint(),
                    generation: d::AttemptGeneration::from_raw(1).unwrap(),
                    session: s::SessionId::mint(),
                    turn: None,
                };
                state.submission = Some(submission);
                state.fence = Some(fence);
                state.stopped = false;
                state.delivered = false;
                state.observed_before_begin = false;
                Ok(admission(&state))
            }
            D::Reconcile { key } => {
                if let Some(submission) = &state.submission {
                    if key.request != submission.provenance.submission {
                        return Ok(BotReply::Missing);
                    }
                    if state.stopped {
                        if let Some(tx) = self.settled_reports.lock().unwrap().as_ref() {
                            let _ = tx.try_send(());
                        }
                    }
                    Ok(admission(&state))
                } else {
                    Ok(BotReply::Missing)
                }
            }
            D::Observe { fence, after } => {
                assert_eq!(Some(&fence), state.fence.as_ref());
                let events = if let Some(turn_id) = fence.turn {
                    let native = vec![
                        s::AgentEvent::TurnStarted { turn_id },
                        s::AgentEvent::MessageDelta {
                            turn_id,
                            text: "NATIVE-REPLY".into(),
                        },
                        if self.fail_first && state.begin == 1 {
                            s::AgentEvent::TurnFailed {
                                turn_id,
                                error: s::SessionFailure {
                                    code: s::ErrorCode::ProviderUnavailable,
                                    message: "fixture response failed".into(),
                                },
                            }
                        } else {
                            s::AgentEvent::TurnCompleted { turn_id }
                        },
                    ];
                    native
                        .into_iter()
                        .enumerate()
                        .filter(|(n, _)| after.is_none_or(|cursor| (*n as u64) + 1 > cursor.get()))
                        .map(|(n, event)| s::SequencedAgentEvent {
                            session_id: fence.session,
                            sequence: s::StreamSequence::new(n as u64 + 1).unwrap(),
                            event,
                        })
                        .collect()
                } else {
                    state.observed_before_begin = true;
                    Vec::new()
                };
                Ok(BotReply::Observation {
                    observation: BotObservation {
                        fence,
                        observation: s::AgentEventObservation {
                            events,
                            cursor: s::StreamSequence::new(if state.begin > 0 { 3 } else { 1 })
                                .unwrap(),
                            earlier_truncated: false,
                            commands: None,
                            session_cursor: None,
                        },
                    },
                })
            }
            D::BeginTurn { fence, input } => {
                assert!(state.observed_before_begin);
                assert_eq!(Some(&fence), state.fence.as_ref());
                state.begin += 1;
                state.input = Some(input);
                let turn = s::AgentTurnId::mint();
                state.fence.as_mut().unwrap().turn = Some(turn);
                Ok(BotReply::Turn { turn })
            }
            D::AcknowledgeDelivery { key, receipt } => {
                assert_eq!(key.run, state.submission.as_ref().unwrap().request.run);
                assert_eq!(
                    receipt.ai_attempt_id,
                    state
                        .submission
                        .as_ref()
                        .unwrap()
                        .provenance
                        .ai_attempt_id
                        .unwrap()
                );
                state.delivered = true;
                state.delivery = Some(receipt);
                if let Some(report) = self.delivery_reports.lock().unwrap().as_ref() {
                    let _ = report.try_send(());
                }
                Ok(BotReply::Applied)
            }
            D::Stop { key, fence } => {
                assert_eq!(key.run, state.submission.as_ref().unwrap().request.run);
                assert_eq!(fence, state.fence);
                state.stopped = true;
                state.stop_calls += 1;
                if let Some(tx) = self.stop_reports.lock().unwrap().as_ref() {
                    let _ = tx.try_send(());
                }
                if self.fail_first {
                    return Ok(BotReply::Applied);
                }
                Err(koru_delegate_api::Failure {
                    code: "bot_stop_unconfirmed".into(),
                    message: "fixture lost stop reply".into(),
                    retryable: true,
                    details: Default::default(),
                })
            }
            _ => panic!("unexpected owner command"),
        }
    }
}
fn admission(state: &NativeState) -> BotReply {
    let submission = state.submission.as_ref().unwrap();
    BotReply::Admission {
        admission: d::BotAdmissionView {
            key: d::BotSubmissionKey {
                owner: submission.provenance.owner,
                bot: submission.provenance.bot,
                request: submission.provenance.submission,
            },
            run: submission.request.run,
            state: if state.delivered || state.stopped {
                d::BotAdmissionState::Settled
            } else {
                d::BotAdmissionState::Executing
            },
            queue_sequence: 1,
            fence: state.fence.clone(),
        },
    }
}
fn proof(authority: Arc<Authority>, bot: &Bot, chat: Chat) -> CommandContext {
    CommandContext {
        request: context(true),
        prepared: Some(Arc::new(PreparedBotCommand {
            authority,
            chats: vec![(bot.id, bot.main_chat, bot.revision, bot.policy_revision)],
            runtime: Ok(()),
            pins: vec![d::RunContextItem::Document {
                workspace: bot.home_workspace,
                document: bot.config.role.document,
                version: d::DocumentVersionId::new(),
            }],
            chat_snapshot: Some(chat),
            settings: None,
            terminal_question: None,
        })),
    }
}
fn request(bot: &Bot) -> SendTurnRequest {
    SendTurnRequest {
        destination_workspace: Some(bot.home_workspace),
        operation_id: Uuid::new_v4(),
        chat_id: bot.main_chat,
        surface: AiSurface::Standalone,
        markdown: "CURRENT-TASK".into(),
        explicit_refs: Vec::new(),
        web_search: false,
        reasoning_override: None,
        system_prompt_override: None,
        replace_pinned_documents: false,
        attachment_ids: Vec::new(),
        answers: None,
        skills: Vec::new(),
    }
}

#[test]
fn applied_delivery_with_full_recovery_queue_keeps_its_durable_outbox_for_retry() {
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
    let (delivery_tx, deliveries) = sync_channel(4);
    let native = Arc::new(Native {
        delivery_reports: Mutex::new(Some(delivery_tx)),
        ..Native::default()
    });
    let gate = crate::worker::native_recovery::tests::saturated_gate(d::BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    });
    let memory =
        koru_store_server::memory::MemoryStore::open("delivery-saturation-memory").unwrap();
    let (return_tx, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-delivery-saturation"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(native)
    .with_native_effect_recovery(memory.client(), gate);
    config.test_repository = Some(repository);
    config.test_repository_return = Some(return_tx);
    let worker = AiWorker::new(config).spawn().unwrap();
    let (reply, response) = sync_channel(1);
    worker
        .client()
        .send(WorkerCommand::SendTurn {
            context,
            request: request(&bot),
            reply,
        })
        .unwrap();
    let stream = response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    // A second exact delivery proves the first Applied reply was processed and
    // retained for retry rather than acknowledging/removing its outbox entry.
    deliveries.recv_timeout(Duration::from_secs(10)).unwrap();
    deliveries.recv_timeout(Duration::from_secs(10)).unwrap();
    worker.shutdown();
    let _ = worker.join();
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    let pending = repository
        .pending_bot_deliveries(
            BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            },
            None,
            16,
        )
        .unwrap();
    assert_eq!(pending.receipts.len(), 1);
    assert_eq!(
        pending.receipts[0].ai_attempt_id,
        stream.accepted.attempt_id.as_uuid()
    );
}

#[test]
fn shared_bot_send_reaches_only_delegate_and_commits_native_response() {
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
    let native = Arc::new(Native::default());
    // No ordinary provider is registered; a fallback cannot produce a reply.
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-in-memory-root"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(native.clone());
    config.test_repository = Some(repository);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    let request = request(&bot);
    for destination in [None, Some(WorkspaceId::new())] {
        let mut invalid = request.clone();
        invalid.operation_id = Uuid::new_v4();
        invalid.destination_workspace = destination;
        let (reply, response) = sync_channel(1);
        client
            .send(WorkerCommand::SendTurn {
                context: context.clone(),
                request: invalid,
                reply,
            })
            .unwrap();
        assert!(
            response
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .is_err()
        );
    }
    assert_eq!(native.state.lock().unwrap().begin, 0);
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::SendTurn {
            context: context.clone(),
            request: request.clone(),
            reply,
        })
        .unwrap();
    let stream = response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    let mut text = String::new();
    loop {
        let event = stream
            .events
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .event;
        match event {
            PublicAiEvent::TextDelta { text: delta, .. } => text.push_str(&delta),
            PublicAiEvent::Completed { .. } => break,
            event if event.is_terminal() => panic!("unexpected terminal: {event:?}"),
            _ => {}
        }
    }
    assert_eq!(text, "NATIVE-REPLY");
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::ReadChat {
            context: context.clone(),
            chat: bot.main_chat,
            reply,
        })
        .unwrap();
    let transcript = response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert_eq!(transcript.messages.len(), 2);
    assert_eq!(transcript.messages[1].markdown, "NATIVE-REPLY");
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::SendTurn {
            context: context.clone(),
            request: request.clone(),
            reply,
        })
        .unwrap();
    let replay = response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert!(replay.accepted.replayed);
    assert_eq!(replay.accepted.attempt_id, stream.accepted.attempt_id);
    assert_eq!(native.state.lock().unwrap().begin, 1);
    let mut changed = request;
    changed.markdown.push_str(" changed");
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::SendTurn {
            context,
            request: changed,
            reply,
        })
        .unwrap();
    assert!(
        response
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err()
    );
    worker.shutdown();
    let _ = worker.join();
}

#[test]
fn shared_bot_retry_uses_original_task_once_and_exact_replay_keeps_identity() {
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
    let (stopped_tx, stopped) = sync_channel(1);
    let native = Arc::new(Native {
        fail_first: true,
        stop_reports: Mutex::new(Some(stopped_tx)),
        ..Native::default()
    });
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-in-memory-root"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(native.clone());
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
    let first = response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    loop {
        let event = first.events.recv_timeout(Duration::from_secs(5)).unwrap();
        if event.event.is_terminal() {
            assert!(matches!(event.event, PublicAiEvent::Failed { .. }));
            break;
        }
    }
    stopped.recv_timeout(Duration::from_secs(5)).unwrap();
    let request = RetryTurnRequest {
        destination_workspace: Some(bot.home_workspace),
        operation_id: Uuid::new_v4(),
        message_id: first.accepted.assistant_message_id,
        surface: AiSurface::Standalone,
        explicit_refs: Vec::new(),
        reasoning_override: None,
        web_search: false,
        replace_pinned_documents: false,
    };
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::RetryTurn {
            context: context.clone(),
            chat: bot.main_chat,
            request: request.clone(),
            reply,
        })
        .unwrap();
    let retry = response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    loop {
        let event = retry.events.recv_timeout(Duration::from_secs(5)).unwrap();
        if event.event.is_terminal() {
            assert!(matches!(event.event, PublicAiEvent::Completed { .. }));
            break;
        }
    }
    assert_eq!(
        native.state.lock().unwrap().input.as_ref().unwrap().text,
        "CURRENT-TASK"
    );
    assert_eq!(
        retry.accepted.user_message_id,
        first.accepted.user_message_id
    );
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::RetryTurn {
            context,
            chat: bot.main_chat,
            request,
            reply,
        })
        .unwrap();
    let replay = response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert!(replay.accepted.replayed);
    assert_eq!(replay.accepted.attempt_id, retry.accepted.attempt_id);
    assert_eq!(
        replay.accepted.user_message_id,
        retry.accepted.user_message_id
    );
    assert_eq!(native.state.lock().unwrap().begin, 2);
    worker.shutdown();
    let _ = worker.join();
}

#[test]
fn cancel_during_submit_and_lost_stop_reply_recovers_after_writer_restart() {
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
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    let (stop_tx, stopped) = sync_channel(4);
    let (settled_tx, settled) = sync_channel(4);
    let native = Arc::new(Native {
        submit_gate: Mutex::new(Some((entered_tx, release))),
        stop_reports: Mutex::new(Some(stop_tx)),
        settled_reports: Mutex::new(Some(settled_tx)),
        ..Native::default()
    });
    let (return_tx, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-in-memory-root"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority.clone())
    .with_bot_delegate(native.clone());
    config.test_repository = Some(repository);
    config.test_repository_return = Some(return_tx);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    struct Release(Option<SyncSender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.try_send(());
            }
        }
    }
    let release = Release(Some(release_tx));
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
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::CancelAttempt {
            context,
            attempt: stream.accepted.attempt_id,
            reply,
        })
        .unwrap();
    assert_eq!(
        response
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .status,
        AttemptStatus::Cancelled
    );
    drop(release);
    stopped.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.shutdown();
    let _ = worker.join();
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        repository
            .attempt(stream.accepted.attempt_id)
            .unwrap()
            .unwrap()
            .status,
        AttemptStatus::Cancelled
    );
    assert_eq!(native.state.lock().unwrap().begin, 0);
    for _ in settled.try_iter() {}
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-in-memory-root"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(native.clone());
    config.test_repository = Some(repository);
    let restarted = AiWorker::new(config).spawn().unwrap();
    settled.recv_timeout(Duration::from_secs(5)).unwrap();
    restarted.shutdown();
    let _ = restarted.join();
    let state = native.state.lock().unwrap();
    assert_eq!(state.begin, 0);
    assert!(state.stop_calls >= 1);
}
