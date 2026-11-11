// SPDX-License-Identifier: MPL-2.0
//! Archive and Unarchive through the full worker (P1.7 S4), with the fake
//! delegate owner the conversation tests use, extended with the admission
//! barrier, the policy barrier and the Run read Archive needs.
use super::*;
use koru_ai_wire::bot_lifecycle_codes::TRASHED;
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
struct Archiving {
    native: Native,
    /// The Run read reports a person holding the session.
    holding: AtomicBool,
    /// The next policy installs answer Pending (effects still in flight).
    policy_pending: Mutex<usize>,
    barriers: Mutex<Vec<d::BotAdmissionBarrier>>,
    policies: Mutex<Vec<d::BotPolicyBarrier>>,
    reads: AtomicUsize,
    stops: AtomicUsize,
}
impl BotDelegatePort for Archiving {
    fn execute(&self, command: D) -> Result<BotReply, koru_delegate_api::Failure> {
        assert!(
            std::thread::current()
                .name()
                .unwrap_or_default()
                .starts_with("koru-ai-provider"),
            "an owner call ran on the writer"
        );
        match &command {
            D::Barrier { barrier } => {
                self.barriers.lock().unwrap().push(barrier.clone());
                Ok(BotReply::Barrier {
                    barrier: barrier.clone(),
                })
            }
            D::SynchronizePolicy { barrier } => {
                self.policies.lock().unwrap().push(barrier.clone());
                let pending = {
                    let mut left = self.policy_pending.lock().unwrap();
                    let pending = *left > 0;
                    *left = left.saturating_sub(1);
                    pending
                };
                Ok(BotReply::PolicyBarrier {
                    receipt: d::BotPolicyBarrierReceipt {
                        barrier: barrier.clone(),
                        state: if pending {
                            d::BotPolicyBarrierState::Pending
                        } else {
                            d::BotPolicyBarrierState::Effective
                        },
                    },
                })
            }
            D::Read { key, .. } => {
                self.reads.fetch_add(1, Ordering::SeqCst);
                let state = self.native.state.lock().unwrap();
                let submission = state.submission.clone().unwrap();
                assert_eq!(key.run, submission.request.run);
                let device = submission.request.executing_device;
                let mut header = d::RunHeader::from_request(submission.request, 0);
                header.status = d::RunStatus::Running {
                    attention: Default::default(),
                    control: if self.holding.load(Ordering::SeqCst) {
                        d::ControlState::Human {
                            objective_satisfied: false,
                        }
                    } else {
                        d::ControlState::Orchestrator
                    },
                };
                Ok(BotReply::Run {
                    view: koru_delegate_api::wire::RunView {
                        document: koru_delegate_api::wire::DocumentId::new(),
                        header,
                        execution: koru_delegate_api::wire::RunExecution {
                            device,
                            session: None,
                            run_workspace: None,
                        },
                        context: vec![],
                        timeline: koru_delegate_api::wire::TimelineWindow {
                            total: 0,
                            from: 0,
                            items: vec![],
                        },
                    },
                })
            }
            D::Stop { .. } => {
                self.stops.fetch_add(1, Ordering::SeqCst);
                drop(self.native.execute(command));
                Ok(BotReply::Applied)
            }
            _ => self.native.execute(command),
        }
    }
}

struct Running {
    worker: WorkerHandle,
    client: AiWorkerClient,
    returned: Receiver<Repository>,
    port: Arc<Archiving>,
    bot: Bot,
    attempt: InferenceAttemptId,
}

fn spawn(
    repository: Repository,
    authority: Arc<Authority>,
    port: Arc<Archiving>,
) -> (WorkerHandle, Receiver<Repository>) {
    let (return_tx, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-lifecycle"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(port);
    config.test_repository = Some(repository);
    config.test_repository_return = Some(return_tx);
    (AiWorker::new(config).spawn().unwrap(), returned)
}

/// A Bot whose task is running natively: its turn began and never ends.
fn running(holding: bool) -> Running {
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
    let (begin_tx, begins) = sync_channel(4);
    let port = Arc::new(Archiving {
        native: Native {
            hold_observation: true,
            begin_reports: Mutex::new(Some(begin_tx)),
            ..Native::default()
        },
        holding: AtomicBool::new(holding),
        ..Archiving::default()
    });
    let (worker, returned) = spawn(repository, authority, port.clone());
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
    begins.recv_timeout(Duration::from_secs(10)).unwrap();
    let client = worker.client();
    Running {
        worker,
        client,
        returned,
        port,
        bot,
        attempt: stream.accepted.attempt_id,
    }
}

fn archive(client: &AiWorkerClient, bot: &Bot, request: BotRequestId) -> BotLifecycleProgress {
    let BotResponse::Lifecycle { operation } = client
        .bots(
            context(true),
            BotCommand::Archive {
                request_id: request,
                bot_id: bot.id,
                expected_revision: bot.revision,
            },
        )
        .unwrap()
    else {
        panic!("lifecycle progress")
    };
    operation
}

/// Read the operation until it stops moving.
fn settled(client: &AiWorkerClient, bot: BotId, request: BotRequestId) -> BotLifecycleProgress {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let Ok(BotResponse::Lifecycle { operation }) = client.bots(
            context(true),
            BotCommand::ReadLifecycleOperation {
                request_id: request,
                bot_id: bot,
            },
        ) else {
            panic!("lifecycle read")
        };
        if matches!(
            operation.stage,
            BotLifecycleStage::Completed | BotLifecycleStage::Incomplete
        ) {
            return operation;
        }
        assert!(Instant::now() < deadline, "still {:?}", operation.stage);
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn archive_stops_the_running_task_then_archives_and_unarchive_returns_it_paused() {
    let Running {
        worker,
        client,
        returned,
        port,
        bot,
        attempt,
    } = running(false);
    let archive_id = BotRequestId::mint();
    let captured = archive(&client, &bot, archive_id);
    assert_eq!(captured.kind, BotLifecycleKind::Archive);
    // An exact repeat answers the same operation; a changed one conflicts.
    assert_eq!(archive(&client, &bot, archive_id).request_id, archive_id);
    let done = settled(&client, bot.id, archive_id);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert!(
        done.members
            .iter()
            .all(|m| m.stage == BotLifecycleMemberStage::Acknowledged)
    );
    // The existing pause barrier, then effects off at the paused revision.
    let barriers = port.barriers.lock().unwrap().clone();
    assert!(barriers.iter().all(|b| b.paused && b.bot == bot.id));
    assert!(!barriers.is_empty());
    let policies = port.policies.lock().unwrap().clone();
    assert!(!policies.is_empty());
    assert!(
        policies
            .iter()
            .all(|p| !p.effects_allowed && p.config_revision == bot.revision + 1)
    );
    // The session was read before anything was stopped, then stopped once
    // as the person's Stop stops it; its owner settled it.
    assert!(port.reads.load(Ordering::SeqCst) >= 1);
    assert!(port.stops.load(Ordering::SeqCst) >= 1);
    // Out of the roster, in the Archived filter, its history still readable.
    let Ok(BotResponse::List { bots }) = client.bots(
        context(true),
        BotCommand::ListLifecycle {
            workspace: bot.home_workspace,
            lifecycle: BotLifecycle::Archived,
        },
    ) else {
        panic!("archived listing")
    };
    assert_eq!(bots.iter().map(|b| b.id).collect::<Vec<_>>(), vec![bot.id]);
    let transcript = client.read_chat(context(true), bot.main_chat).unwrap();
    assert!(!transcript.messages.is_empty());
    // A change to an archived Bot's conversation reads as missing.
    assert_eq!(
        client
            .send_turn(context(true), request(&bot))
            .map(|_| ())
            .unwrap_err()
            .code,
        "not_found"
    );
    // Unarchive: active again, still paused, nothing resumed.
    let stops = port.stops.load(Ordering::SeqCst);
    let BotResponse::Lifecycle { operation } = client
        .bots(
            context(true),
            BotCommand::Restore {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                expected_revision: bot.revision + 2,
                source_request_id: archive_id,
            },
        )
        .unwrap()
    else {
        panic!("restore progress")
    };
    assert_eq!(operation.stage, BotLifecycleStage::Completed);
    assert_eq!(operation.source_request_id, Some(archive_id));
    let BotResponse::Bot { bot: restored } = client
        .bots(context(true), BotCommand::Read { bot_id: bot.id })
        .unwrap()
    else {
        panic!("Bot")
    };
    assert!(restored.paused);
    assert_eq!(restored.revision, bot.revision + 3);
    assert_eq!(port.stops.load(Ordering::SeqCst), stops);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        repository.attempt(attempt).unwrap().unwrap().status,
        AttemptStatus::Cancelled
    );
}

#[test]
fn a_person_holding_the_session_parks_archive_without_stopping_and_a_repeat_resumes() {
    let Running {
        worker,
        client,
        returned,
        port,
        bot,
        attempt,
    } = running(true);
    let request = BotRequestId::mint();
    archive(&client, &bot, request);
    let parked = settled(&client, bot.id, request);
    assert_eq!(parked.stage, BotLifecycleStage::Incomplete);
    assert_eq!(parked.refusal, Some(BotLifecycleRefusal::PersonHolding));
    // O27: nothing the person holds was ended.
    assert_eq!(port.stops.load(Ordering::SeqCst), 0);
    assert_eq!(port.native.state.lock().unwrap().stop_calls, 0);
    // Parked keeps the barrier: the Bot stays held for changes.
    assert_eq!(
        client
            .bots(
                context(true),
                BotCommand::CreateChat {
                    request_id: BotRequestId::mint(),
                    bot_id: bot.id,
                },
            )
            .unwrap_err()
            .code,
        koru_ai_wire::bot_lifecycle_codes::PENDING
    );
    // The person hands back; the same request resumes and finishes.
    port.holding.store(false, Ordering::SeqCst);
    archive(&client, &bot, request);
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert!(port.stops.load(Ordering::SeqCst) >= 1);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        repository.attempt(attempt).unwrap().unwrap().status,
        AttemptStatus::Cancelled
    );
}

#[test]
fn a_restart_resumes_a_captured_archive_from_the_journal_and_waits_for_the_barrier() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    // Captured by an earlier worker that stopped before any owner call.
    let request = BotRequestId::mint();
    repository
        .capture_bot_lifecycle(&koru_ai_sqlite::LifecycleCapture {
            operation: request,
            owner: BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            },
            bot: bot.id,
            kind: BotLifecycleKind::Archive,
            chat: None,
            fingerprint: "c".repeat(64),
            expected_revision: bot.revision,
            source: None,
            members: vec![],
            now: 1,
        })
        .unwrap();
    let port = Arc::new(Archiving {
        // Two installs see native effects still in flight.
        policy_pending: Mutex::new(2),
        ..Archiving::default()
    });
    let (worker, returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    // The pause went out through its receipt; the barrier was re-asked until
    // effective; nothing was stopped because nothing ran.
    assert!(!port.barriers.lock().unwrap().is_empty());
    assert!(port.policies.lock().unwrap().len() >= 3);
    assert_eq!(port.stops.load(Ordering::SeqCst), 0);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        repository.bot_any_lifecycle(bot.id).unwrap().unwrap().1,
        BotLifecycle::Archived
    );
}

#[test]
fn start_on_a_trashed_bot_reads_its_stable_trash_code() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    repository
        .fixture_set_bot_lifecycle(bot.id, BotLifecycle::Trashed)
        .unwrap();
    let (worker, _returned) = spawn(repository, authority, Arc::new(Archiving::default()));
    let code = worker
        .client()
        .bots(
            context(true),
            BotCommand::Start {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                chat_id: bot.main_chat,
                expected_revision: bot.revision,
                destination: bot.home_workspace,
                task: BotTask {
                    text: "CURRENT-TASK".into(),
                    documents: vec![],
                },
            },
        )
        .unwrap_err()
        .code;
    assert_eq!(code, TRASHED);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
