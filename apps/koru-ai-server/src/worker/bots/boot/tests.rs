// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::ProfileId;
use koru_delegate_core::BotBootCursor;
use std::sync::Mutex;

type Call = (D, SyncSender<Result<BotReply, koru_delegate_api::Failure>>);
struct ScriptedDelegate(SyncSender<Call>);
impl BotDelegatePort for ScriptedDelegate {
    fn execute(&self, command: D) -> Result<BotReply, koru_delegate_api::Failure> {
        if !matches!(
            command,
            D::Hello { .. } | D::SynchronizeBootBot { .. } | D::PendingNativeEffects { .. }
        ) {
            return Err(unavailable());
        }
        let (reply, result) = sync_channel(1);
        self.0.send((command, reply)).unwrap();
        result.recv_timeout(Duration::from_secs(5)).unwrap()
    }
}
fn next(calls: &Receiver<Call>) -> Call {
    calls.recv_timeout(Duration::from_secs(5)).unwrap()
}
fn entry(bot: &Bot, sequence: u64, admission: BotBootAdmission) -> BotBootEntry {
    BotBootEntry {
        identity: koru_delegate_core::BotBootIdentity {
            owner: delegate_owner(bot),
            bot: bot.id,
            run: koru_delegate_core::RunId::mint(),
            sequence,
        },
        admission,
    }
}
fn hello(
    reply: SyncSender<Result<BotReply, koru_delegate_api::Failure>>,
    boot: DelegateBootId,
    entries: Vec<BotBootEntry>,
    next: Option<BotBootCursor>,
) {
    reply
        .send(Ok(BotReply::Hello {
            hello: koru_delegate_core::BotHello {
                boot,
                entries,
                next,
            },
        }))
        .unwrap();
}
fn synchronized(command: D, state: BotBootState) -> BotReply {
    let D::SynchronizeBootBot {
        boot,
        barrier,
        policy,
    } = command
    else {
        panic!("expected replay")
    };
    BotReply::BootSynchronized {
        receipt: koru_delegate_core::BotBootReceipt {
            boot,
            barrier,
            policy: policy.key,
            config_revision: policy.config_revision,
            policy_revision: policy.policy_revision,
            state,
        },
    }
}
fn worker() -> (WorkerHandle, Bot, Receiver<Call>, DelegateBootId) {
    let (mut repository, authority, command) = super::super::tests::fixture();
    let bot = super::super::tests::created(&mut repository, authority.clone(), command);
    let (send, calls) = sync_channel(8);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-boot-replay"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(Arc::new(ScriptedDelegate(send)));
    config.test_repository = Some(repository);
    // Configuration tests exercise the replay hook without an installed agent.
    config.test_bot_runtime_ready = true;
    let worker = AiWorker::new(config).spawn().unwrap();
    let boot = DelegateBootId::mint();
    let (command, reply) = next(&calls);
    assert!(matches!(
        command,
        D::Hello {
            scan: BotBootScan::Probe {},
            ..
        }
    ));
    hello(reply, boot, vec![], None);
    (worker, bot, calls, boot)
}

#[test]
fn worker_skips_unavailable_and_out_of_range_entries_and_reaches_next_page() {
    let (worker, bot, calls, boot) = worker();
    let (command, reply) = next(&calls);
    assert!(matches!(
        command,
        D::Hello {
            scan: BotBootScan::Start {},
            ..
        }
    ));
    let cursor = BotBootCursor {
        boot,
        owner: delegate_owner(&bot),
        after: 3,
        through: 4,
    };
    hello(
        reply,
        boot,
        vec![
            entry(&bot, 1, BotBootAdmission::Unavailable {}),
            entry(&bot, u64::MAX, BotBootAdmission::Missing {}),
            entry(&bot, 3, BotBootAdmission::Missing {}),
        ],
        Some(cursor.clone()),
    );
    let (command, reply) = next(&calls);
    reply
        .send(Ok(synchronized(command, BotBootState::Ready)))
        .unwrap();
    let (command, reply) = next(&calls);
    assert!(
        matches!(command,D::Hello {scan:BotBootScan::Continue {cursor:ref actual},..} if *actual == cursor)
    );
    hello(
        reply,
        boot,
        vec![
            entry(&bot, 3, BotBootAdmission::Missing {}),
            entry(&bot, 5, BotBootAdmission::Missing {}),
            entry(&bot, 4, BotBootAdmission::Missing {}),
        ],
        None,
    );
    let (command, reply) = next(&calls);
    reply
        .send(Ok(synchronized(command, BotBootState::Ready)))
        .unwrap();
    // Invalid identities remain owed, but never monopolize the cursor.
    let (command, reply) = next(&calls);
    assert!(matches!(
        command,
        D::Hello {
            scan: BotBootScan::Start {},
            ..
        }
    ));
    hello(reply, boot, vec![], None);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn worker_unknown_timeout_retries_and_ignores_late_old_serial() {
    let (worker, bot, calls, boot) = worker();
    let (_, reply) = next(&calls);
    hello(
        reply,
        boot,
        vec![entry(&bot, 1, BotBootAdmission::Missing {})],
        None,
    );
    let (command, reply) = next(&calls);
    let D::SynchronizeBootBot {
        boot: sent_boot,
        barrier,
        policy,
    } = command
    else {
        panic!("replay")
    };
    reply
        .send(Err(koru_delegate_api::Failure::new(
            "timeout",
            "execution unknown",
            true,
        )))
        .unwrap();
    let (command, reply) = next(&calls);
    assert!(matches!(
        command,
        D::Hello {
            scan: BotBootScan::Start {},
            ..
        }
    ));
    // The old call may finish after its socket deadline; its callback must not
    // clear the fresh in-flight job or overwrite the new boot observation.
    worker
        .client()
        .sender
        .send(Envelope::BotBootChecked(BotBootResult {
            routine_ready: true,
            serial: 3,
            work: Work::Replay {
                routine: Ok(None),
                boot: sent_boot,
                bot: bot.clone(),
                admission: barrier.clone(),
                policy: policy.clone(),
            },
            result: Ok((
                delegate_owner(&bot),
                BotReply::BootSynchronized {
                    receipt: koru_delegate_core::BotBootReceipt {
                        boot: sent_boot,
                        barrier,
                        policy: policy.key,
                        config_revision: policy.config_revision,
                        policy_revision: policy.policy_revision,
                        state: BotBootState::Ready,
                    },
                },
            )),
        }))
        .unwrap();
    hello(
        reply,
        boot,
        vec![entry(&bot, 1, BotBootAdmission::Missing {})],
        None,
    );
    let (command, reply) = next(&calls);
    reply
        .send(Ok(synchronized(command, BotBootState::Ready)))
        .unwrap();
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

fn owner_context() -> RequestContext {
    RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "fixture".into(),
            scopes: vec![AiScope::Chat, AiScope::ManageBots],
        },
        trace_id: "boot-fixture".into(),
        deadline_millis: None,
    }
}

#[test]
fn worker_same_boot_config_change_keeps_cursor_then_replays_followup_inventory() {
    for new_sequence in [1, 6] {
        let (worker, bot, calls, boot) = worker();
        let (_, reply) = next(&calls);
        let cursor = BotBootCursor {
            boot,
            owner: delegate_owner(&bot),
            after: 2,
            through: 5,
        };
        hello(
            reply,
            boot,
            vec![entry(&bot, 2, BotBootAdmission::Missing {})],
            Some(cursor.clone()),
        );
        let (command, reply) = next(&calls);
        let old_barrier = match &command {
            D::SynchronizeBootBot { barrier, .. } => barrier.clone(),
            _ => panic!("replay"),
        };
        reply
            .send(Ok(synchronized(command, BotBootState::Ready)))
            .unwrap();
        let (command, reply) = next(&calls);
        assert!(
            matches!(command,D::Hello {scan:BotBootScan::Continue {cursor:ref actual},..} if *actual==cursor)
        );
        let mut config = bot.config.clone();
        config.name = "changed while inventory is in flight".into();
        let response = worker
            .client()
            .bots(
                owner_context(),
                BotCommand::Update {
                    request_id: BotRequestId::mint(),
                    bot_id: bot.id,
                    expected_revision: bot.revision,
                    config,
                },
            )
            .unwrap();
        assert!(matches!(response,BotResponse::Bot {bot:ref current} if current.revision==2));
        hello(reply, boot, vec![], None);
        // A mutation can advance metadata Probe; it never rewinds Continue.
        let reply = loop {
            let (command, reply) = next(&calls);
            match command {
                D::Hello {
                    scan: BotBootScan::Probe {},
                    ..
                } => hello(reply, boot, vec![], None),
                D::Hello {
                    scan: BotBootScan::Start {},
                    ..
                } => break reply,
                other => panic!("unexpected {other:?}"),
            }
        };
        // Both a row beyond the old high water and a reused sequence are
        // eligible on the coalesced pass, even though delegate proof was Ready.
        hello(
            reply,
            boot,
            vec![entry(
                &bot,
                new_sequence,
                BotBootAdmission::Present {
                    barrier: old_barrier,
                },
            )],
            None,
        );
        let (command, reply) = next(&calls);
        assert!(
            matches!(&command,D::SynchronizeBootBot {barrier,policy,..} if barrier.epoch==2 && policy.config_revision==2)
        );
        reply
            .send(Ok(synchronized(command, BotBootState::Ready)))
            .unwrap();
        worker.shutdown();
        assert_eq!(worker.join(), WorkerExit::Clean);
    }
}

#[test]
fn worker_delegate_restart_discards_old_cursor_and_captures_new_boot() {
    let (worker, bot, calls, boot) = worker();
    let (_, reply) = next(&calls);
    let cursor = BotBootCursor {
        boot,
        owner: delegate_owner(&bot),
        after: 1,
        through: 9,
    };
    hello(reply, boot, vec![], Some(cursor));
    let (command, reply) = next(&calls);
    assert!(matches!(
        command,
        D::Hello {
            scan: BotBootScan::Continue { .. },
            ..
        }
    ));
    reply
        .send(Err(koru_delegate_api::Failure::new(
            "bot_boot_cursor_stale",
            "delegate restarted",
            false,
        )))
        .unwrap();
    let new_boot = DelegateBootId::mint();
    let (command, reply) = next(&calls);
    assert!(matches!(
        command,
        D::Hello {
            scan: BotBootScan::Probe {},
            ..
        }
    ));
    hello(reply, new_boot, vec![], None);
    let (command, reply) = next(&calls);
    assert!(matches!(
        command,
        D::Hello {
            scan: BotBootScan::Start {},
            ..
        }
    ));
    hello(
        reply,
        new_boot,
        vec![entry(&bot, 1, BotBootAdmission::Missing {})],
        None,
    );
    let (command, reply) = next(&calls);
    assert!(matches!(&command,D::SynchronizeBootBot {boot:actual,..} if *actual==new_boot));
    reply
        .send(Ok(synchronized(command, BotBootState::Ready)))
        .unwrap();
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

struct ResumeDelegate(ScriptedDelegate);
impl BotDelegatePort for ResumeDelegate {
    fn execute(&self, command: D) -> Result<BotReply, koru_delegate_api::Failure> {
        match command {
            D::Barrier { barrier } => Ok(BotReply::Barrier { barrier }),
            other => self.0.execute(other),
        }
    }
}
#[test]
fn worker_same_boot_resume_replays_existing_queued_work_without_another_submit() {
    let (mut repository, authority, command) = super::super::tests::fixture();
    let bot = super::super::tests::created(&mut repository, authority.clone(), command);
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let pause = repository
        .prepare_bot_pause(
            owner,
            bot.id,
            BotRequestId::mint(),
            1,
            true,
            &"a".repeat(64),
            2,
        )
        .unwrap();
    repository
        .acknowledge_bot_pause(
            owner,
            bot.id,
            pause.intent.barrier.request,
            &pause.intent.barrier,
            3,
        )
        .unwrap();
    let (send, calls) = sync_channel(8);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-boot-resume"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(Arc::new(ResumeDelegate(ScriptedDelegate(send))));
    config.test_repository = Some(repository);
    let worker = AiWorker::new(config).spawn().unwrap();
    let boot = DelegateBootId::mint();
    let (_, reply) = next(&calls);
    hello(reply, boot, vec![], None);
    let (_, reply) = next(&calls);
    hello(
        reply,
        boot,
        vec![entry(
            &bot,
            1,
            BotBootAdmission::Present {
                barrier: pause.intent.barrier.clone(),
            },
        )],
        None,
    );
    let (command, reply) = next(&calls);
    assert!(
        matches!(&command,D::SynchronizeBootBot {barrier,..} if barrier.paused && barrier.epoch==2)
    );
    reply
        .send(Ok(synchronized(command, BotBootState::Ready)))
        .unwrap();
    // Let the startup follow-up finish, so only Resume can request fresh work.
    let (_, reply) = next(&calls);
    hello(reply, boot, vec![], None);
    worker
        .client()
        .bots(
            owner_context(),
            BotCommand::Resume {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                expected_revision: 2,
            },
        )
        .unwrap();
    loop {
        let (command, reply) = next(&calls);
        match command {
            D::Hello {
                scan: BotBootScan::Probe {},
                ..
            } => hello(reply, boot, vec![], None),
            D::Hello { .. } => hello(
                reply,
                boot,
                vec![entry(
                    &bot,
                    1,
                    BotBootAdmission::Present {
                        barrier: pause.intent.barrier.clone(),
                    },
                )],
                None,
            ),
            command @ D::SynchronizeBootBot { .. } => {
                assert!(
                    matches!(&command,D::SynchronizeBootBot {barrier,policy,..} if !barrier.paused && barrier.epoch==3 && policy.config_revision==3)
                );
                reply
                    .send(Ok(synchronized(command, BotBootState::Ready)))
                    .unwrap();
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

struct DeletedPages {
    owner: BotOwner,
    workspace: WorkspaceId,
    profile: ProfileId,
}

struct SwitchingAuthority(Mutex<koru_ai_core::bots::BotAuthoritySnapshot>);
impl BotAuthority for SwitchingAuthority {
    fn owner(&self) -> Result<BotOwner, BotError> {
        Ok(self.0.lock().unwrap().owner)
    }
    fn workspace(&self, id: WorkspaceId) -> Result<ProfileId, BotError> {
        self.0.lock().unwrap().workspace(id)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.0.lock().unwrap().page(page)
    }
}
#[test]
fn worker_owner_change_discards_old_page_before_another_delegate_replay() {
    let (mut repository, original, command) = super::super::tests::fixture();
    let bot = super::super::tests::created(&mut repository, original.clone(), command);
    let authority = Arc::new(SwitchingAuthority(Mutex::new(
        koru_ai_core::bots::BotAuthoritySnapshot {
            owner: original.owner().unwrap(),
            workspaces: std::collections::BTreeMap::from([(
                bot.home_workspace,
                original.workspace(bot.home_workspace),
            )]),
            pages: std::collections::BTreeMap::from([(bot.config.role.clone(), Ok(()))]),
        },
    )));
    let (send, calls) = sync_channel(8);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-boot-owner-change"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority.clone())
    .with_bot_delegate(Arc::new(ScriptedDelegate(send)));
    config.test_repository = Some(repository);
    let worker = AiWorker::new(config).spawn().unwrap();
    let boot = DelegateBootId::mint();
    let (_, reply) = next(&calls);
    hello(reply, boot, vec![], None);
    let (_, reply) = next(&calls);
    hello(
        reply,
        boot,
        vec![
            entry(&bot, 1, BotBootAdmission::Missing {}),
            entry(&bot, 2, BotBootAdmission::Missing {}),
        ],
        None,
    );
    let (command, reply) = next(&calls);
    let new_owner = BotOwner {
        user_scope: UserScopeId::new(),
        device: DeviceId::new(),
    };
    authority.0.lock().unwrap().owner = new_owner;
    reply
        .send(Ok(synchronized(command, BotBootState::Ready)))
        .unwrap();
    let (command, reply) = next(&calls);
    assert!(
        matches!(command,D::Hello {owner,scan:BotBootScan::Probe {}} if owner.user_scope==new_owner.user_scope && owner.device==new_owner.device)
    );
    hello(reply, boot, vec![], None);
    let (command, reply) = next(&calls);
    assert!(
        matches!(command,D::Hello {owner,scan:BotBootScan::Start {}} if owner.user_scope==new_owner.user_scope)
    );
    hello(reply, boot, vec![], None);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
impl BotAuthority for DeletedPages {
    fn owner(&self) -> Result<BotOwner, BotError> {
        Ok(self.owner)
    }
    fn workspace(&self, id: WorkspaceId) -> Result<ProfileId, BotError> {
        if id == self.workspace {
            Ok(self.profile)
        } else {
            Err(BotError::Forbidden)
        }
    }
    fn page(&self, _: &BotPage) -> Result<(), BotError> {
        Err(BotError::Forbidden)
    }
}
struct RecordingDelegate(Mutex<Vec<D>>);
impl BotDelegatePort for RecordingDelegate {
    fn execute(&self, command: D) -> Result<BotReply, koru_delegate_api::Failure> {
        self.0.lock().unwrap().push(command.clone());
        Ok(synchronized(command, BotBootState::Ready))
    }
}
#[test]
fn failed_routine_snapshot_preserves_successful_global_boot_receipt() {
    let (mut repo, authority, command) = super::super::tests::fixture();
    let bot = super::super::tests::created(&mut repo, authority.clone(), command);
    let snapshot = repo
        .bot_boot_snapshot(
            BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            },
            bot.id,
            &BotBootAdmission::Missing {},
        )
        .unwrap();
    let port = Arc::new(RecordingDelegate(Mutex::new(vec![])));
    let result = BotBootJob {
        serial: 1,
        work: Work::Replay {
            boot: DelegateBootId::mint(),
            admission: snapshot.admission,
            policy: policy::barrier(&bot).unwrap(),
            bot,
            routine: Err(unavailable()),
        },
        authority,
        delegate: port.clone(),
    }
    .run();
    assert!(
        matches!(result.result,Ok((_,BotReply::BootSynchronized{receipt})) if receipt.state==BotBootState::Ready)
    );
    assert!(!result.routine_ready);
    assert_eq!(port.0.lock().unwrap().len(), 1);
}
#[test]
fn effects_off_replay_requires_owner_home_but_not_deleted_role_or_selected_page() {
    let (mut repo, authority, command) = super::super::tests::fixture();
    let bot = super::super::tests::created(&mut repo, authority, command);
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let snapshot = repo
        .bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Missing {})
        .unwrap();
    let port = Arc::new(RecordingDelegate(Mutex::new(vec![])));
    let authority = Arc::new(DeletedPages {
        owner,
        workspace: bot.home_workspace,
        profile: ProfileId::new(),
    });
    let mut policy = policy::barrier(&bot).unwrap();
    for effects_allowed in [false, true] {
        policy.effects_allowed = effects_allowed;
        let result = BotBootJob {
            serial: 1,
            work: Work::Replay {
                routine: Ok(None),
                boot: DelegateBootId::mint(),
                bot: bot.clone(),
                admission: snapshot.admission.clone(),
                policy: policy.clone(),
            },
            authority: authority.clone(),
            delegate: port.clone(),
        }
        .run();
        assert_eq!(result.result.is_ok(), !effects_allowed);
    }
    assert_eq!(port.0.lock().unwrap().len(), 1);
    let foreign = Arc::new(DeletedPages {
        owner: BotOwner {
            device: DeviceId::new(),
            ..owner
        },
        workspace: bot.home_workspace,
        profile: ProfileId::new(),
    });
    policy.effects_allowed = false;
    assert!(
        BotBootJob {
            serial: 2,
            work: Work::Replay {
                routine: Ok(None),
                boot: DelegateBootId::mint(),
                bot,
                admission: snapshot.admission,
                policy
            },
            authority: foreign,
            delegate: port.clone()
        }
        .run()
        .result
        .is_err()
    );
    assert_eq!(port.0.lock().unwrap().len(), 1);
}

#[test]
fn mutation_wakes_coalesce_without_rewinding_current_cursor() {
    let mut state = BootRecovery::default();
    state.begin();
    let cursor = BotBootCursor {
        boot: DelegateBootId::mint(),
        owner: DelegateOwner {
            user_scope: UserScopeId::new(),
            device: DeviceId::new(),
        },
        after: 17,
        through: 70,
    };
    state.next = Some(BotBootScan::Continue {
        cursor: cursor.clone(),
    });
    for _ in 0..100 {
        state.request();
    }
    assert_eq!(state.next, Some(BotBootScan::Continue { cursor }));
    state.next = None;
    state.finish(Instant::now());
    assert_eq!(state.next, Some(BotBootScan::Start {}));
    assert!(!state.followup);
    state.next = None;
    state.finish(Instant::now());
    assert!(!state.walking);
}

#[test]
fn failed_pass_retains_fresh_submit_wake_and_retry_backoff_is_bounded() {
    let now = Instant::now();
    let mut state = BootRecovery::default();
    state.begin();
    state.request();
    state.retry_needed = true;
    state.next = None;
    state.finish(now);
    assert!(state.followup);
    assert_eq!(state.next, Some(BotBootScan::Start {}));
    assert_eq!(state.due, now + Duration::from_millis(250));
    for _ in 0..100 {
        state.defer(now);
    }
    assert_eq!(state.due, now + Duration::from_secs(32));
    state.finish(now + Duration::from_secs(32));
    assert!(!state.followup);
    assert_eq!(state.next, Some(BotBootScan::Start {}));
}

#[test]
fn slow_successful_probe_with_repeated_wakes_leaves_inventory_a_turn() {
    let mut state = BootRecovery::default();
    state.begin();
    let launched = Instant::now();
    state.probe_not_before = launched + Duration::from_secs(1);
    for _ in 0..50 {
        state.request();
    }
    let completed = launched + Duration::from_secs(8);
    state.probed(completed);
    assert_eq!(state.probe_at, completed + Duration::from_secs(1));
    assert_eq!(state.next, Some(BotBootScan::Start {}));
    assert!(state.followup);
    state.request();
    assert_eq!(state.probe_at, completed + Duration::from_secs(1));
    state.defer(completed);
    assert_eq!(state.due, completed + Duration::from_millis(250));
}

#[test]
fn worker_pending_policy_drains_native_work_then_replays_without_completing_pause() {
    use crate::worker::native_recovery::NativeMutationGate;
    use koru_store_server::memory::MemoryStore;
    let memory = MemoryStore::open("boot-policy-drain-memory").unwrap();
    let (mut repository, authority, command) = super::super::tests::fixture();
    let bot = super::super::tests::created(&mut repository, authority.clone(), command);
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let pause_request = BotRequestId::mint();
    repository
        .prepare_bot_pause(owner, bot.id, pause_request, 1, true, &"a".repeat(64), 2)
        .unwrap();
    let (send, calls) = sync_channel(8);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-boot-pending-policy"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(Arc::new(ScriptedDelegate(send)))
    .with_native_effect_recovery(memory.client(), Arc::new(NativeMutationGate::closed()));
    config.test_repository = Some(repository);
    let worker = AiWorker::new(config).spawn().unwrap();
    let boot = DelegateBootId::mint();
    let mut replay_count = 0;
    let mut scoped_drain = false;
    let mut startup_drain = false;
    loop {
        let (command, reply) = next(&calls);
        match command {
            D::Hello {
                scan: BotBootScan::Probe {},
                ..
            } => hello(reply, boot, vec![], None),
            D::Hello {
                scan: BotBootScan::Start {},
                ..
            } => hello(
                reply,
                boot,
                vec![entry(&bot, 1, BotBootAdmission::Missing {})],
                Some(BotBootCursor {
                    boot,
                    owner: delegate_owner(&bot),
                    after: 1,
                    through: 2,
                }),
            ),
            D::Hello { .. } => hello(reply, boot, vec![], None),
            D::PendingNativeEffects { bot: scope, .. } => {
                if scope == Some(bot.id) {
                    scoped_drain = true
                } else {
                    startup_drain = true
                }
                reply
                    .send(Ok(BotReply::NativePendingEffects {
                        page: koru_delegate_core::NativePendingEffects {
                            targets: vec![],
                            next: None,
                        },
                    }))
                    .unwrap();
            }
            command @ D::SynchronizeBootBot { .. } => {
                replay_count += 1;
                let state = if !scoped_drain {
                    BotBootState::PendingPolicy
                } else {
                    BotBootState::Ready
                };
                reply.send(Ok(synchronized(command, state))).unwrap();
                if replay_count > 1 && scoped_drain && startup_drain {
                    break;
                }
            }
            _ => unreachable!(),
        }
    }
    // The next page can be scheduled only after the Ready callback clears
    // boot.busy. Hold that known job while checking the durable pause receipt.
    let final_reply = loop {
        let (command, reply) = next(&calls);
        match command {
            D::Hello { .. } => break reply,
            D::PendingNativeEffects { .. } => reply
                .send(Ok(BotReply::NativePendingEffects {
                    page: koru_delegate_core::NativePendingEffects {
                        targets: vec![],
                        next: None,
                    },
                }))
                .unwrap(),
            other => panic!("unexpected after Ready: {other:?}"),
        }
    };
    let response = worker
        .client()
        .bots(
            owner_context(),
            BotCommand::ReadOperation {
                request_id: pause_request,
                bot_id: bot.id,
            },
        )
        .unwrap();
    let BotResponse::Operation { operation } = response else {
        panic!("operation")
    };
    assert_eq!(operation.status, BotOperationStatus::Pending);
    worker.shutdown();
    // Release the known in-flight call only after shutdown prevents another
    // scripted admin call from starting without a test owner to answer it.
    hello(final_reply, boot, vec![], None);
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn worker_completed_archive_replays_effects_off_after_role_deletion() {
    use koru_ai_sqlite::{LifecycleCapture, LifecycleMemberDraft};
    let (mut repository, authority, command) = super::super::tests::fixture();
    let bot = super::super::tests::created(&mut repository, authority, command);
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let operation = repository
        .capture_bot_lifecycle(&LifecycleCapture {
            operation: BotRequestId::mint(),
            owner,
            bot: bot.id,
            kind: BotLifecycleKind::Archive,
            chat: None,
            fingerprint: "a".repeat(64),
            expected_revision: 1,
            source: None,
            now: 10,
            members: vec![
                LifecycleMemberDraft {
                    subject: BotLifecycleSubject::Bot,
                    reason: BotLifecycleReason::Bot,
                    prior_state: BotLifecyclePriorState::Live,
                    intended: BotLifecycleIntended::Hide,
                    retained: None,
                },
                LifecycleMemberDraft {
                    subject: BotLifecycleSubject::Chat {
                        chat_id: bot.main_chat,
                    },
                    reason: BotLifecycleReason::OwnedConversation,
                    prior_state: BotLifecyclePriorState::Live,
                    intended: BotLifecycleIntended::Hide,
                    retained: None,
                },
            ],
        })
        .unwrap();
    repository
        .advance_bot_lifecycle(
            owner.user_scope,
            operation.operation,
            BotLifecycleStage::Captured,
            BotLifecycleStage::Settling,
            true,
            11,
        )
        .unwrap();
    repository
        .complete_bot_archive(owner.user_scope, operation.operation, 12)
        .unwrap();
    let authority = Arc::new(DeletedPages {
        owner,
        workspace: bot.home_workspace,
        profile: ProfileId::new(),
    });
    let (send, calls) = sync_channel(8);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-boot-archived"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(Arc::new(ScriptedDelegate(send)));
    config.test_repository = Some(repository);
    let worker = AiWorker::new(config).spawn().unwrap();
    let boot = DelegateBootId::mint();
    let (_, reply) = next(&calls);
    hello(reply, boot, vec![], None);
    let (_, reply) = next(&calls);
    hello(
        reply,
        boot,
        vec![entry(&bot, 1, BotBootAdmission::Missing {})],
        None,
    );
    let (command, reply) = next(&calls);
    assert!(
        matches!(&command,D::SynchronizeBootBot {barrier,policy,..} if barrier.epoch==3 && barrier.paused && !policy.effects_allowed && policy.config_revision==3)
    );
    reply
        .send(Ok(synchronized(command, BotBootState::Ready)))
        .unwrap();
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
