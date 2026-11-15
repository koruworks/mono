// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn routine_live_chat_proof_jobs_use_exact_linked_owner_and_return_writer_snapshot() {
    for phase in [Phase::RoutineAdmission, Phase::RoutineBegin] {
        let (repository, submission, authority, port) = policy::dispatch_job_fixture();
        let proof = policy::dispatch::RoutineDispatch::snapshot(&repository, &submission).unwrap();
        let attempt =
            InferenceAttemptId::parse(&submission.provenance.ai_attempt_id.unwrap().to_string())
                .unwrap();
        // The production job performs only the three finite authenticated
        // proof calls; the strict peer panics on a provider/Begin/Submit call.
        let result = StepJob {
            attempt,
            phase,
            command: DelegateCommand::Reconcile {
                key: BotSubmissionKey {
                    owner: submission.provenance.owner,
                    bot: submission.provenance.bot,
                    request: submission.provenance.submission,
                },
            },
            ingest: None,
            administrative: false,
            port,
            routine: Some((proof, authority)),
        }
        .run();
        assert!(matches!(result.result, Ok(BotReply::Applied)));
        assert_eq!(result.attempt, attempt);
        let returned = result
            .routine
            .expect("completion must recheck this snapshot");
        returned.recheck(&repository).unwrap();
        repository
            .fixture_execute_batch(&format!(
                "UPDATE bots SET revision=revision+1 WHERE bot_id='{}'",
                submission.provenance.bot,
            ))
            .unwrap();
        assert!(returned.recheck(&repository).is_err());
    }
}

struct Blocked {
    entered: SyncSender<()>,
    release: Mutex<Receiver<()>>,
}
impl BotDelegatePort for Blocked {
    fn execute(&self, _: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        self.entered.send(()).unwrap();
        let _ = self.release.lock().unwrap().recv();
        Ok(BotReply::Missing)
    }
}
struct Administrative;
impl BotDelegatePort for Administrative {
    fn execute(&self, _: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        assert_eq!(
            std::thread::current().name(),
            Some("koru-ai-provider-control")
        );
        Ok(BotReply::Applied)
    }
}

fn control_job(port: Arc<dyn BotDelegatePort>) -> ProviderAdminJob {
    ProviderAdminJob::BotChatControl(StepJob {
        routine: None,
        attempt: InferenceAttemptId::new(),
        phase: Phase::Stop,
        administrative: true,
        ingest: None,
        command: DelegateCommand::Stop {
            key: koru_delegate_core::BotRunKey {
                owner: koru_delegate_core::BotOwner {
                    user_scope: UserScopeId::new(),
                    device: DeviceId::from_uuid(Uuid::new_v4()),
                },
                bot: BotId::mint(),
                run: koru_delegate_core::RunId::mint(),
            },
            fence: None,
        },
        port,
    })
}

#[test]
fn full_control_queue_returns_retryable_job_and_accepts_retry_after_progress() {
    let (output, events) = sync_channel(32);
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut admin = ProviderAdminPool::spawn(None, None, output, shutdown).unwrap();
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    admin
        .try_submit(control_job(Arc::new(Blocked {
            entered: entered_tx,
            release: Mutex::new(release),
        })))
        .unwrap_or_else(|_| panic!("empty control queue"));
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    for _ in 0..PROVIDER_ADMIN_QUEUE_CAPACITY {
        admin
            .try_submit(control_job(Arc::new(Administrative)))
            .unwrap_or_else(|_| panic!("reserved capacity"));
    }
    let rejected = match admin.try_submit(control_job(Arc::new(Administrative))) {
        Err(error) => error,
        Ok(()) => panic!("control queue must be bounded"),
    };
    assert!(rejected.failure.retryable);
    assert_eq!(rejected.failure.code, "ai_worker_saturated");
    release_tx.send(()).unwrap();
    // The second completed call proves a queued slot was consumed.
    events.recv_timeout(Duration::from_secs(5)).unwrap();
    events.recv_timeout(Duration::from_secs(5)).unwrap();
    admin
        .try_submit(rejected.job)
        .unwrap_or_else(|_| panic!("retry after progress"));
    for _ in 0..PROVIDER_ADMIN_QUEUE_CAPACITY {
        events.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    admin.stop();
    assert!(
        admin
            .try_submit(control_job(Arc::new(Administrative)))
            .is_err()
    );
}

#[test]
fn control_shutdown_discards_queued_work_and_joins_after_inflight_call_returns() {
    struct Count(Arc<std::sync::atomic::AtomicUsize>);
    impl BotDelegatePort for Count {
        fn execute(&self, _: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(BotReply::Applied)
        }
    }
    let (output, _events) = sync_channel(4);
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut admin = ProviderAdminPool::spawn(None, None, output, shutdown.clone()).unwrap();
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    admin
        .try_submit(control_job(Arc::new(Blocked {
            entered: entered_tx,
            release: Mutex::new(release),
        })))
        .unwrap_or_else(|_| panic!("empty control queue"));
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    admin
        .try_submit(control_job(Arc::new(Count(calls.clone()))))
        .unwrap_or_else(|_| panic!("queue has room"));
    admin
        .try_submit(ProviderAdminJob::BotInteraction(
            super::super::controls::ControlJob::fixture(Arc::new(Count(calls.clone()))),
        ))
        .unwrap_or_else(|_| panic!("interactive queue has room"));
    shutdown.store(true, Ordering::Release);
    release_tx.send(()).unwrap();
    admin.stop();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[test]
fn native_stop_runs_while_both_providers_and_normal_admin_are_blocked() {
    let (output, events) = sync_channel(8);
    let shutdown = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered) = sync_channel(2);
    let (release_tx, release) = sync_channel(2);
    let (admin_entered_tx, admin_entered) = sync_channel(1);
    let (admin_release_tx, admin_release) = sync_channel(1);
    let blocked = Arc::new(Blocked {
        entered: entered_tx,
        release: Mutex::new(release),
    });
    struct Release {
        tx: SyncSender<()>,
        admin: SyncSender<()>,
        shutdown: Arc<AtomicBool>,
    }
    impl Drop for Release {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Release);
            let _ = self.tx.try_send(());
            let _ = self.tx.try_send(());
            let _ = self.admin.try_send(());
        }
    }
    let release = Release {
        tx: release_tx,
        admin: admin_release_tx,
        shutdown: shutdown.clone(),
    };
    let mut provider = ProviderPool::spawn(
        Arc::new(ProviderRegistry::default()),
        output.clone(),
        shutdown.clone(),
        2,
    )
    .unwrap();
    let mut admin = ProviderAdminPool::spawn(None, None, output, shutdown.clone()).unwrap();
    admin
        .try_submit(ProviderAdminJob::BlockForFixture {
            entered: admin_entered_tx,
            release: admin_release,
        })
        .unwrap_or_else(|_| panic!("empty normal admin queue"));
    admin_entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let key = BotSubmissionKey {
        owner: koru_delegate_core::BotOwner {
            user_scope: UserScopeId::new(),
            device: DeviceId::from_uuid(Uuid::new_v4()),
        },
        bot: BotId::mint(),
        request: BotRequestId::mint(),
    };
    for _ in 0..2 {
        assert!(provider.try_submit_bot(StepJob {
            routine: None,
            attempt: InferenceAttemptId::new(),
            phase: Phase::Admission,
            command: DelegateCommand::Reconcile { key: key.clone() },
            ingest: None,
            administrative: false,
            port: blocked.clone()
        }));
    }
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let expected = InferenceAttemptId::new();
    admin
        .try_submit(ProviderAdminJob::BotChatControl(StepJob {
            routine: None,
            attempt: expected,
            phase: Phase::Stop,
            administrative: true,
            ingest: None,
            command: DelegateCommand::Stop {
                key: koru_delegate_core::BotRunKey {
                    owner: key.owner,
                    bot: key.bot,
                    run: koru_delegate_core::RunId::mint(),
                },
                fence: None,
            },
            port: Arc::new(Administrative),
        }))
        .unwrap_or_else(|_| panic!("admin saturated"));
    match events.recv_timeout(Duration::from_secs(5)).unwrap() {
        Envelope::BotChatStep(result) => {
            assert_eq!(result.attempt, expected);
            assert!(result.administrative);
            assert!(matches!(result.result, Ok(BotReply::Applied)));
        }
        _ => panic!("unexpected result"),
    }
    admin
        .try_submit(ProviderAdminJob::BotInteraction(
            super::super::controls::ControlJob::fixture(Arc::new(Administrative)),
        ))
        .unwrap_or_else(|_| panic!("interactive queue has room"));
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(5)).unwrap(),
        Envelope::BotInteraction(_)
    ));
    drop(release);
    provider.stop();
    admin.stop();
}

#[test]
fn stop_precedes_a_full_interactive_queue_and_rejected_interaction_can_retry() {
    let (output, events) = sync_channel(32);
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut admin = ProviderAdminPool::spawn(None, None, output, shutdown).unwrap();
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    let interactive =
        |port| ProviderAdminJob::BotInteraction(super::super::controls::ControlJob::fixture(port));
    admin
        .try_submit(interactive(Arc::new(Blocked {
            entered: entered_tx,
            release: Mutex::new(release),
        })))
        .unwrap_or_else(|_| panic!("first interaction"));
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    for _ in 0..PROVIDER_ADMIN_QUEUE_CAPACITY {
        admin
            .try_submit(interactive(Arc::new(Administrative)))
            .unwrap_or_else(|_| panic!("reserved capacity"));
    }
    let rejected = match admin.try_submit(interactive(Arc::new(Administrative))) {
        Err(error) => error,
        Ok(()) => panic!("interactive queue must be bounded"),
    };
    assert_eq!(rejected.failure.code, "ai_worker_saturated");
    assert!(rejected.failure.retryable);
    admin
        .try_submit(control_job(Arc::new(Administrative)))
        .unwrap_or_else(|_| panic!("Stop has independent capacity"));
    release_tx.send(()).unwrap();
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(5)).unwrap(),
        Envelope::BotInteraction(_)
    ));
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(5)).unwrap(),
        Envelope::BotChatStep(_)
    ));
    events.recv_timeout(Duration::from_secs(5)).unwrap();
    admin
        .try_submit(rejected.job)
        .unwrap_or_else(|_| panic!("retry after progress"));
    for _ in 0..PROVIDER_ADMIN_QUEUE_CAPACITY {
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(5)).unwrap(),
            Envelope::BotInteraction(_)
        ));
    }
    admin.stop();
    assert!(
        admin
            .try_submit(interactive(Arc::new(Administrative)))
            .is_err()
    );
}

#[test]
fn finishing_backoff_doubles_from_the_poll_interval_to_the_owner_cap() {
    assert_eq!(finishing_backoff(0), Duration::from_millis(250));
    assert_eq!(finishing_backoff(1), Duration::from_millis(500));
    assert_eq!(finishing_backoff(4), Duration::from_secs(4));
    assert_eq!(
        finishing_backoff(FINISH_BACKOFF_MAX_ROUND),
        Duration::from_secs(32)
    );
    assert_eq!(finishing_backoff(40), Duration::from_secs(32));
}

/// Task Stops (`stop_task`) have a lane of their own: with eight of them
/// sent while the execution owner holds the first, the ones past its queue
/// are answered `ai_worker_saturated` at once, and a person's approval
/// answer and conversation Stop on the Stop lane are each accepted and
/// answered meanwhile (the lane review found task Stops on the Stop lane's
/// thread, where a stuck one held the person's other Stops).
#[test]
fn task_stops_have_their_own_lane_and_never_make_an_approval_or_a_stop_wait() {
    let (output, events) = sync_channel(32);
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut admin = ProviderAdminPool::spawn(None, None, output, shutdown.clone()).unwrap();
    let (entered_tx, entered) = sync_channel(8);
    let (release_tx, release) = sync_channel(8);
    let stuck: Arc<dyn BotDelegatePort> = Arc::new(Blocked {
        entered: entered_tx,
        release: Mutex::new(release),
    });
    let far = Instant::now() + Duration::from_secs(60);
    let mut answers = Vec::new();
    let mut saturated = 0;
    for _ in 0..8 {
        let (reply, answer) = sync_channel(1);
        let job = crate::worker::bots::BotTaskStopJob::fixture(stuck.clone(), far, reply);
        match admin.try_submit(ProviderAdminJob::BotTaskStop(job)) {
            Ok(()) => answers.push(answer),
            Err(error) => {
                assert_eq!(error.failure.code, "ai_worker_saturated");
                saturated += 1;
            }
        }
        if answers.len() == 1 && saturated == 0 {
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    }
    assert_eq!(
        answers.len(),
        1 + crate::worker::bots::TASK_STOP_QUEUE_CAPACITY
    );
    assert_eq!(saturated, 8 - answers.len());
    // The person's approval answer and conversation Stop are not behind them.
    let started = Instant::now();
    admin
        .try_submit(ProviderAdminJob::BotInteraction(
            super::super::controls::ControlJob::fixture(Arc::new(Administrative)),
        ))
        .unwrap_or_else(|_| panic!("the interactive queue has room"));
    admin
        .try_submit(control_job(Arc::new(Administrative)))
        .unwrap_or_else(|_| panic!("the Stop lane has room"));
    let mut seen = (false, false);
    while !(seen.0 && seen.1) {
        match events.recv_timeout(Duration::from_secs(5)).unwrap() {
            Envelope::BotInteraction(_) => seen.0 = true,
            Envelope::BotChatStep(result) => {
                assert!(matches!(result.result, Ok(BotReply::Applied)));
                seen.1 = true;
            }
            _ => {}
        }
    }
    assert!(started.elapsed() < Duration::from_secs(2));
    shutdown.store(true, Ordering::Release);
    for _ in 0..8 {
        let _ = release_tx.try_send(());
    }
    admin.stop();
    drop(answers);
}
