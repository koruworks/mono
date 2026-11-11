// SPDX-License-Identifier: MPL-2.0
//! Preparation's own lane (what lifecycle-6 left owed, P1.7 "Background work
//! leaves room for a person's request"). Every Bot command a person sends,
//! reads included, is prepared on `koru-ai-provider-prepare`; every
//! background job (lifecycle rounds, receipt recovery, probes, the restart
//! walk, branch, policy, provider work) stays on the shared
//! `koru-ai-provider-admin` lane. So an execution owner hung inside a
//! background job holds none of a person's Bot requests, and background work
//! that fills its own queue refuses none of them.
use super::probe::{PerWorkspace, bot_in_own_workspace, parked_with_nothing_owed};
use super::trash::api_task_in;
use super::*;

/// An execution owner hung inside one kind of call: while `held`, every call
/// `call_of` names `name` blocks until released (25 s at most), then fails
/// retryable, as a host that stops answering does (`LocalDelegate::call`
/// gives up with a retryable failure). Every other call is the lifecycle
/// fake's, which answers as the real owner's `orchestrator/bots.rs` does.
struct Hung {
    inner: Archiving,
    name: &'static str,
    held: AtomicBool,
    /// Each hung call: the thread it ran on and, for an unsettled-work read,
    /// its limit (a probe asks for one, a settling round for a page).
    entered: Mutex<Vec<(String, Option<u16>)>>,
}
impl Hung {
    fn on(name: &'static str, inner: Archiving) -> Arc<Self> {
        Arc::new(Self {
            inner,
            name,
            held: AtomicBool::new(true),
            entered: Mutex::new(Vec::new()),
        })
    }
    fn first(&self) -> Option<(String, Option<u16>)> {
        self.entered.lock().unwrap().first().cloned()
    }
}
impl BotDelegatePort for Hung {
    fn execute(&self, command: D) -> Result<BotReply, koru_delegate_api::Failure> {
        if call_of(&command).0 == self.name && self.held.load(Ordering::SeqCst) {
            let limit = match &command {
                D::UnsettledWork { limit, .. } => Some(*limit),
                _ => None,
            };
            let thread = std::thread::current().name().unwrap_or_default().to_owned();
            self.entered.lock().unwrap().push((thread, limit));
            let since = Instant::now();
            while self.held.load(Ordering::SeqCst) && since.elapsed() < Duration::from_secs(25) {
                std::thread::sleep(Duration::from_millis(10));
            }
            return Err(koru_delegate_api::Failure::new(
                "bot_unavailable",
                "fixture owner hung",
                true,
            ));
        }
        self.inner.execute(command)
    }
}

/// Releases a hung owner however the test ends, so no thread outlives it.
struct Release(Arc<Hung>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.held.store(false, Ordering::SeqCst);
    }
}

/// The owner's authority over every workspace its Bots live in.
fn reaching_all(template: &Authority) -> Arc<PerWorkspace> {
    Arc::new(PerWorkspace {
        owner: template.owner,
        profile: template.profile,
        lost: Mutex::new(Default::default()),
        lost_inspections: AtomicUsize::new(0),
    })
}

fn spawn_on(
    repository: Repository,
    authority: Arc<PerWorkspace>,
    port: Arc<dyn BotDelegatePort>,
) -> WorkerHandle {
    let (return_tx, _returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-lifecycle"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(port);
    config.test_repository = Some(repository);
    config.test_repository_return = Some(return_tx);
    // A settings save validates its model: discovery answers ready.
    config.test_bot_runtime_ready = true;
    AiWorker::new(config).spawn().unwrap()
}

/// An Archive whose pause the owner acknowledged, moving in `settling`: its
/// next round reads what of the Bot is unsettled (a page of it).
fn settling_with_nothing_owed(repository: &mut Repository, bot: &Bot) -> BotRequestId {
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let operation = captured_archive(repository, bot);
    let pause = repository
        .bot_lifecycle_operation(bot.user_scope, operation)
        .unwrap()
        .unwrap()
        .pause_request
        .unwrap();
    let Some(BotAdmissionReceipt::Pause(receipt)) = repository
        .bot_admission_receipt(owner, bot.id, pause)
        .unwrap()
    else {
        panic!("pause receipt")
    };
    repository
        .acknowledge_bot_pause(owner, bot.id, pause, &receipt.intent.barrier, 3)
        .unwrap();
    assert!(
        repository
            .advance_bot_lifecycle(
                bot.user_scope,
                operation,
                BotLifecycleStage::Captured,
                BotLifecycleStage::Settling,
                false,
                4,
            )
            .unwrap()
    );
    operation
}

/// Well inside the 25 s hang, and the caller's own 30 s.
const PROMPT: Duration = Duration::from_secs(3);

/// A person's own Bot read, a chat read of their Bot's conversation and a
/// settings save of it, each prepared first, are each answered promptly.
fn persons_requests_are_answered(client: &AiWorkerClient, mine: &Bot) {
    let started = Instant::now();
    let read = client.bots(context(true), BotCommand::Read { bot_id: mine.id });
    assert!(
        matches!(&read, Ok(BotResponse::Bot { bot }) if bot.id == mine.id),
        "the person's Bot read: {:?}",
        read.map(|_| ()).map_err(|error| error.code)
    );
    assert!(
        started.elapsed() < PROMPT,
        "the Bot read took {:?}",
        started.elapsed()
    );
    let started = Instant::now();
    let chat = client.read_chat(context(true), mine.main_chat);
    assert!(
        matches!(&chat, Ok(transcript) if transcript.chat.id == mine.main_chat),
        "the person's chat read: {:?}",
        chat.map(|_| ()).map_err(|error| error.code)
    );
    assert!(
        started.elapsed() < PROMPT,
        "the chat read took {:?}",
        started.elapsed()
    );
    let started = Instant::now();
    let saved = client.update_chat_model(
        context(true),
        mine.main_chat,
        UpdateChatModelRequest {
            model: mine.config.model.clone(),
        },
    );
    assert!(
        saved.is_ok(),
        "the person's settings save: {:?}",
        saved.map(|_| ()).map_err(|error| error.code)
    );
    assert!(
        started.elapsed() < PROMPT,
        "the settings save took {:?}",
        started.elapsed()
    );
}

/// A probe of a parked operation hung on its owner: the person's requests
/// do not wait behind it.
#[test]
fn a_probe_hung_on_its_owner_holds_no_persons_bot_request() {
    let (mut repository, template, _) = fixture();
    let parked = bot_in_own_workspace(&mut repository, &template);
    parked_with_nothing_owed(&mut repository, &parked);
    let mine = bot_in_own_workspace(&mut repository, &template);
    let port = Hung::on("unsettled", Archiving::default());
    let _release = Release(port.clone());
    let worker = spawn_on(repository, reaching_all(&template), port.clone());
    let client = worker.client();
    until("the probe is hung on its owner", || port.first().is_some());
    assert_eq!(
        port.first(),
        Some(("koru-ai-provider-admin".to_owned(), Some(1))),
        "a probe, on the shared lane"
    );
    persons_requests_are_answered(&client, &mine);
    worker.shutdown();
    drop(_release);
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// A lifecycle round (an Archive settling) hung on its owner.
#[test]
fn a_lifecycle_round_hung_on_its_owner_holds_no_persons_bot_request() {
    let (mut repository, template, _) = fixture();
    let moving = bot_in_own_workspace(&mut repository, &template);
    settling_with_nothing_owed(&mut repository, &moving);
    let mine = bot_in_own_workspace(&mut repository, &template);
    let port = Hung::on("unsettled", Archiving::default());
    let _release = Release(port.clone());
    let worker = spawn_on(repository, reaching_all(&template), port.clone());
    let client = worker.client();
    until("the settling round is hung on its owner", || {
        port.first().is_some()
    });
    assert_eq!(
        port.first(),
        Some(("koru-ai-provider-admin".to_owned(), Some(64))),
        "a settling round's page, on the shared lane"
    );
    persons_requests_are_answered(&client, &mine);
    worker.shutdown();
    drop(_release);
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// Receipt recovery's reconcile of a task still being submitted, hung on its
/// owner.
#[test]
fn a_recovery_reconcile_hung_on_its_owner_holds_no_persons_bot_request() {
    let (mut repository, template, _) = fixture();
    let starting = bot_in_own_workspace(&mut repository, &template);
    let task = api_task_in(
        &mut repository,
        &starting,
        starting.main_chat,
        d::BotAdmissionState::Queued,
        0,
    );
    let mine = bot_in_own_workspace(&mut repository, &template);
    let inner = Archiving::default();
    inner.api.lock().unwrap().push(task);
    let port = Hung::on("reconcile", inner);
    let _release = Release(port.clone());
    let worker = spawn_on(repository, reaching_all(&template), port.clone());
    let client = worker.client();
    until("the recovery reconcile is hung on its owner", || {
        port.first().is_some()
    });
    assert_eq!(
        port.first(),
        Some(("koru-ai-provider-admin".to_owned(), None)),
        "receipt recovery, on the shared lane"
    );
    persons_requests_are_answered(&client, &mine);
    worker.shutdown();
    drop(_release);
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// Nine Bots each mid-Archive at a restart, and an owner that is silent for
/// their settling reads: their rounds fill the shared lane (one running and
/// eight queued), and a person's Bot reads are still answered, never
/// `ai_worker_saturated`.
#[test]
fn nine_bots_mid_operation_after_a_restart_refuse_no_persons_bot_read() {
    let (mut repository, template, _) = fixture();
    let moving: Vec<Bot> = (0..9)
        .map(|_| {
            let bot = bot_in_own_workspace(&mut repository, &template);
            settling_with_nothing_owed(&mut repository, &bot);
            bot
        })
        .collect();
    let mine = bot_in_own_workspace(&mut repository, &template);
    let port = Hung::on("unsettled", Archiving::default());
    let _release = Release(port.clone());
    let worker = spawn_on(repository, reaching_all(&template), port.clone());
    let client = worker.client();
    until("the first settling round is hung on its owner", || {
        port.first().is_some()
    });
    let reads: Vec<_> = (0..2)
        .map(|_| {
            let client = worker.client();
            let bot = mine.id;
            std::thread::spawn(move || {
                let started = Instant::now();
                let read = client
                    .bots(context(true), BotCommand::Read { bot_id: bot })
                    .map(|_| ())
                    .map_err(|error| error.code);
                (read, started.elapsed())
            })
        })
        .collect();
    for read in reads {
        let (read, took) = read.join().unwrap();
        assert_eq!(read, Ok(()), "a person's Bot read");
        assert!(took < PROMPT, "a person's Bot read took {took:?}");
    }
    // A third, alone, after them.
    let read = client.bots(context(true), BotCommand::Read { bot_id: mine.id });
    assert!(read.is_ok(), "{:?}", read.map(|_| ()).map_err(|e| e.code));
    assert_eq!(moving.len(), 9);
    worker.shutdown();
    drop(_release);
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// Preparation's lane holds one job running and
/// `PREPARATION_QUEUE_CAPACITY` waiting; one more is answered
/// `ai_worker_saturated` at once. Neither lane's queue is the other's: a
/// full shared lane leaves preparation room, and a full preparation lane
/// leaves the shared lane room.
#[test]
fn a_full_preparation_lane_answers_at_once_and_shares_no_queue_with_background_work() {
    let (output, _events) = sync_channel(64);
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut admin = ProviderAdminPool::spawn(None, None, output, shutdown.clone()).unwrap();
    let mut releases = Vec::new();
    let mut block = |admin: &ProviderAdminPool, preparation: bool| {
        let (entered_tx, entered) = sync_channel(1);
        let (release_tx, release) = sync_channel(1);
        releases.push(release_tx);
        let job = if preparation {
            ProviderAdminJob::BlockPreparationForFixture {
                entered: entered_tx,
                release,
            }
        } else {
            ProviderAdminJob::BlockForFixture {
                entered: entered_tx,
                release,
            }
        };
        (
            admin.try_submit(job).map_err(|error| error.failure),
            entered,
        )
    };
    // The shared lane: one running, eight waiting, then full.
    let (submitted, entered) = block(&admin, false);
    assert!(submitted.is_ok());
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    for _ in 0..PROVIDER_ADMIN_QUEUE_CAPACITY {
        assert!(block(&admin, false).0.is_ok());
    }
    let full = block(&admin, false).0.unwrap_err();
    assert_eq!(full.code, "ai_worker_saturated");
    // Preparation still has its whole lane.
    let (submitted, entered) = block(&admin, true);
    assert!(submitted.is_ok());
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    for _ in 0..PREPARATION_QUEUE_CAPACITY {
        assert!(block(&admin, true).0.is_ok());
    }
    let started = Instant::now();
    let full = block(&admin, true).0.unwrap_err();
    assert_eq!(full.code, "ai_worker_saturated");
    assert!(full.retryable);
    assert!(started.elapsed() < Duration::from_millis(500));
    shutdown.store(true, Ordering::Release);
    drop(releases);
    admin.stop();
}

/// At shutdown every caller waiting on preparation is answered once the job
/// being prepared returns: that one, and every one queued behind it (their
/// jobs are dropped with the lane, so each reply is dropped and the caller
/// told `ai_unavailable`), well inside 2 s of the lane freeing and never
/// left to its own 30 s timeout. The job being prepared is not cut short:
/// shutdown waits for it, as for every lane.
#[test]
fn shutdown_answers_every_caller_waiting_on_preparation() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let (worker, _returned) = spawn(
        repository,
        authority.clone(),
        Arc::new(Archiving::default()),
    );
    // Once the worker answers a person, the next authority inspection is
    // the next person's preparation, and it holds the lane.
    read_bot(&worker.client(), bot.id);
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    *authority.inspect_gate.lock().unwrap() = Some((entered_tx, release));
    let read = |worker: &WorkerHandle| {
        let client = worker.client();
        let bot = bot.id;
        std::thread::spawn(move || {
            let read = client.bots(context(true), BotCommand::Read { bot_id: bot });
            (read.map(|_| ()).map_err(|error| error.code), Instant::now())
        })
    };
    // One read holds the lane inside its authority inspection; three wait.
    let mut readers = vec![read(&worker)];
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    readers.extend((0..3).map(|_| read(&worker)));
    std::thread::sleep(Duration::from_millis(200));
    worker.shutdown();
    std::thread::sleep(Duration::from_millis(100));
    let released = Instant::now();
    release_tx.send(()).unwrap();
    for reader in readers {
        let (read, at) = reader.join().unwrap();
        assert!(
            at.saturating_duration_since(released) < Duration::from_secs(2),
            "a caller was left waiting: {read:?}"
        );
        assert_ne!(read, Err("ai_worker_timeout".to_owned()));
    }
    assert_eq!(worker.join(), WorkerExit::Clean);
}
