// SPDX-License-Identifier: MPL-2.0
use super::*;

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
