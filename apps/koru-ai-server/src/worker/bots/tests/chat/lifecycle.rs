// SPDX-License-Identifier: MPL-2.0
//! Archive and Unarchive through the full worker (P1.7 S4, S4b), with the
//! fake delegate owner the conversation tests use, extended with what
//! Archive needs: the admission barrier, the policy barrier, and the per-Bot
//! unsettled-work read (private IPC 12) over the conversation task and any
//! tasks started through the public API, which have no conversation.
use super::*;
use koru_ai_wire::bot_lifecycle_codes::{ARCHIVED, PENDING, TRASHED};
use std::sync::atomic::AtomicUsize;

/// A task the fake owner runs that no conversation of this writer drives.
struct ApiTask {
    view: d::BotAdmissionView,
    /// Interrupts the owner still answers as pending before it settles.
    pending: usize,
    /// A person takes this task's session the moment an Interrupt reaches it.
    take_on_interrupt: bool,
    held: bool,
}

#[derive(Default)]
struct Archiving {
    native: Native,
    /// The session owner reports a person holding the conversation task's
    /// session (or asking for it).
    holding: AtomicBool,
    /// The next policy installs answer Pending (effects still in flight).
    policy_pending: Mutex<usize>,
    barriers: Mutex<Vec<d::BotAdmissionBarrier>>,
    policies: Mutex<Vec<d::BotPolicyBarrier>>,
    api: Mutex<Vec<ApiTask>>,
    /// Unsettled-work reads.
    reads: AtomicUsize,
    stops: AtomicUsize,
    /// Every Interrupt, by Run, whoever sent it.
    interrupts: Mutex<Vec<d::RunId>>,
}
impl Archiving {
    fn unsettled(&self, bot: d::BotId) -> Vec<d::BotUnsettledAdmission> {
        let mut admissions = Vec::new();
        {
            let state = self.native.state.lock().unwrap();
            if state
                .submission
                .as_ref()
                .is_some_and(|s| s.provenance.bot == bot)
                && let BotReply::Admission { admission } = admission(&state)
                && !matches!(
                    admission.state,
                    d::BotAdmissionState::Settled | d::BotAdmissionState::Refused
                )
            {
                admissions.push(d::BotUnsettledAdmission {
                    admission,
                    held: self.holding.load(Ordering::SeqCst),
                });
            }
        }
        for task in self.api.lock().unwrap().iter() {
            if task.view.key.bot == bot
                && !matches!(
                    task.view.state,
                    d::BotAdmissionState::Settled | d::BotAdmissionState::Refused
                )
            {
                admissions.push(d::BotUnsettledAdmission {
                    admission: task.view.clone(),
                    held: task.held,
                });
            }
        }
        admissions
    }
    fn interrupts_of(&self, run: d::RunId) -> usize {
        self.interrupts
            .lock()
            .unwrap()
            .iter()
            .filter(|r| **r == run)
            .count()
    }
}
fn pending() -> koru_delegate_api::Failure {
    koru_delegate_api::Failure::new("bot_interrupt_pending", "fixture cleanup pending", true)
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
            D::UnsettledWork { bot, limit, .. } => {
                assert!(*limit >= 1 && *limit <= 64);
                self.reads.fetch_add(1, Ordering::SeqCst);
                Ok(BotReply::UnsettledWork {
                    work: d::BotUnsettledWork {
                        admissions: self.unsettled(*bot),
                        more: false,
                    },
                })
            }
            D::Interrupt { key, fence } => {
                self.interrupts.lock().unwrap().push(key.run);
                let mut api = self.api.lock().unwrap();
                if let Some(task) = api.iter_mut().find(|t| t.view.run == key.run) {
                    // Only the owner's current fence is accepted.
                    assert_eq!(fence, &task.view.fence);
                    if task.take_on_interrupt {
                        // The person took the session after the driver's
                        // read: the owned cleanup refuses, nothing ends it.
                        task.take_on_interrupt = false;
                        task.held = true;
                    }
                    if task.held || task.pending > 0 {
                        task.pending = task.pending.saturating_sub(1);
                        return Err(pending());
                    }
                    task.view.state = d::BotAdmissionState::Settled;
                    return Ok(BotReply::Applied);
                }
                drop(api);
                self.native.execute(command)
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

/// A Bot whose conversation task is running natively: its turn began and
/// never ends.
fn running(holding: bool) -> Running {
    started_with(
        Native {
            hold_observation: true,
            ..Native::default()
        },
        holding,
    )
}

fn started_with(native: Native, holding: bool) -> Running {
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
    *native.begin_reports.lock().unwrap() = Some(begin_tx);
    let port = Arc::new(Archiving {
        native,
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

fn archive_at(
    client: &AiWorkerClient,
    bot: &Bot,
    request: BotRequestId,
    expected_revision: u64,
) -> ApiResult<BotLifecycleProgress> {
    match client.bots(
        context(true),
        BotCommand::Archive {
            request_id: request,
            bot_id: bot.id,
            expected_revision,
        },
    )? {
        BotResponse::Lifecycle { operation } => Ok(operation),
        _ => panic!("lifecycle progress"),
    }
}

fn archive(client: &AiWorkerClient, bot: &Bot, request: BotRequestId) -> BotLifecycleProgress {
    archive_at(client, bot, request, bot.revision).unwrap()
}

fn read_operation(
    client: &AiWorkerClient,
    bot: BotId,
    request: BotRequestId,
) -> BotLifecycleProgress {
    // A read prepared just before the operation moves the Bot's revision
    // answers `revision_conflict` (its proof is bound to the revision it was
    // prepared at); read again.
    for _ in 0..50 {
        match client.bots(
            context(true),
            BotCommand::ReadLifecycleOperation {
                request_id: request,
                bot_id: bot,
            },
        ) {
            Ok(BotResponse::Lifecycle { operation }) => return operation,
            Err(error) if error.code == BotError::RevisionConflict.code() => {
                std::thread::sleep(Duration::from_millis(10));
            }
            other => panic!(
                "lifecycle read: {:?}",
                other.map(|_| ()).map_err(|error| error.code)
            ),
        }
    }
    panic!("lifecycle read kept conflicting")
}

/// Read the operation until it stops moving.
fn settled(client: &AiWorkerClient, bot: BotId, request: BotRequestId) -> BotLifecycleProgress {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let operation = read_operation(client, bot, request);
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

fn read_bot(client: &AiWorkerClient, bot: BotId) -> Bot {
    let BotResponse::Bot { bot } = client
        .bots(context(true), BotCommand::Read { bot_id: bot })
        .unwrap()
    else {
        panic!("Bot")
    };
    bot
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn owner_of(bot: &Bot) -> d::BotOwner {
    d::BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    }
}

fn api_task(bot: &Bot, state: d::BotAdmissionState, pending: usize) -> ApiTask {
    let run = d::RunId::mint();
    ApiTask {
        view: d::BotAdmissionView {
            key: d::BotSubmissionKey {
                owner: owner_of(bot),
                bot: bot.id,
                request: BotRequestId::mint(),
            },
            run,
            state,
            queue_sequence: 1,
            fence: (state == d::BotAdmissionState::Executing).then(|| d::BotExecutionFence {
                owner: owner_of(bot),
                bot: bot.id,
                run,
                attempt: d::RunAttemptId::mint(),
                generation: d::AttemptGeneration::from_raw(1).unwrap(),
                session: s::SessionId::mint(),
                turn: Some(s::AgentTurnId::mint()),
            }),
        },
        pending,
        take_on_interrupt: false,
        held: false,
    }
}

/// An Archive captured by an earlier worker straight into the journal.
fn captured_archive(repository: &mut Repository, bot: &Bot) -> BotRequestId {
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
    request
}

#[test]
fn archive_interrupts_the_running_task_without_stop_and_unarchive_starts_nothing() {
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
    // An exact repeat answers the same operation.
    assert_eq!(archive(&client, &bot, archive_id).request_id, archive_id);
    let done = settled(&client, bot.id, archive_id);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert!(
        done.members
            .iter()
            .all(|m| m.stage == BotLifecycleMemberStage::Acknowledged)
    );
    // A changed request under a finished operation's id conflicts.
    assert_eq!(
        archive_at(&client, &bot, archive_id, bot.revision + 2)
            .unwrap_err()
            .code,
        BotError::Conflict.code()
    );
    // The existing pause barrier, then effects off at the paused revision.
    let barriers = port.barriers.lock().unwrap().clone();
    assert!(!barriers.is_empty());
    assert!(barriers.iter().all(|b| b.paused && b.bot == bot.id));
    let policies = port.policies.lock().unwrap().clone();
    assert!(!policies.is_empty());
    assert!(
        policies
            .iter()
            .all(|p| !p.effects_allowed && p.config_revision == bot.revision + 1)
    );
    // The work came from the delegate and ended as an automatic
    // interruption: the person's Stop never reached the owner.
    assert!(port.reads.load(Ordering::SeqCst) >= 1);
    assert_eq!(port.stops.load(Ordering::SeqCst), 0);
    assert_eq!(port.native.state.lock().unwrap().stop_calls, 0);
    assert!(port.native.state.lock().unwrap().interrupt_calls >= 1);
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
    assert_eq!(bots[0].lifecycle, BotLifecycle::Archived);
    let archived = read_bot(&client, bot.id);
    assert_eq!(archived.lifecycle, BotLifecycle::Archived);
    assert_eq!(archived.open_operation, None);
    let transcript = client.read_chat(context(true), bot.main_chat).unwrap();
    assert!(!transcript.messages.is_empty());
    // A change to an archived Bot's conversation answers the archived code.
    assert_eq!(
        client
            .send_turn(context(true), request(&bot))
            .map(|_| ())
            .unwrap_err()
            .code,
        ARCHIVED
    );
    // Unarchive: active again, still paused, nothing started or resumed.
    let begins = port.native.state.lock().unwrap().begin;
    let interrupts = port.interrupts.lock().unwrap().len();
    let policies = port.policies.lock().unwrap().len();
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
    let restored = read_bot(&client, bot.id);
    assert!(restored.paused);
    assert_eq!(restored.lifecycle, BotLifecycle::Active);
    assert_eq!(restored.revision, bot.revision + 3);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(port.native.state.lock().unwrap().begin, begins);
    assert_eq!(port.interrupts.lock().unwrap().len(), interrupts);
    assert_eq!(port.policies.lock().unwrap().len(), policies);
    assert!(
        port.policies
            .lock()
            .unwrap()
            .iter()
            .all(|p| !p.effects_allowed),
        "no Run's native writes were re-allowed"
    );
    assert_eq!(port.stops.load(Ordering::SeqCst), 0);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    // A task Archive ended reads as interrupted, not as stopped by the person.
    assert_eq!(
        repository.attempt(attempt).unwrap().unwrap().status,
        AttemptStatus::Interrupted
    );
}

#[test]
fn a_person_holding_an_ended_task_parks_archive_and_nothing_ends_their_session() {
    // The task already ended in AI as Interrupted (a replay gap), Failed (the
    // native turn failed) or natively Cancelled (a takeover "now"), so its
    // Run no longer reads as running under a person. The session owner says
    // otherwise, and that is what Archive asks.
    for (variant, status) in [
        ("interrupted", AttemptStatus::Interrupted),
        ("failed", AttemptStatus::Failed),
        ("cancelled", AttemptStatus::Cancelled),
    ] {
        let (interrupt_tx, interrupt_reports) = sync_channel(256);
        let native = Native {
            hold_observation: variant == "interrupted",
            replay_failure: variant == "interrupted",
            fail_first: variant == "failed",
            native_cancel: (variant == "cancelled").then_some(s::CancelReason::TakeoverBoundary),
            hold_interrupt: AtomicBool::new(true),
            interrupt_reports: Mutex::new(Some(interrupt_tx)),
            ..Native::default()
        };
        let Running {
            worker,
            client,
            returned,
            port,
            bot,
            attempt,
        } = started_with(native, true);
        // The automatic lane is already retrying its own Interrupt, pending.
        assert!(
            !interrupt_reports
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
        );
        until("the attempt ended", || {
            client
                .read_attempt(context(true), attempt)
                .is_ok_and(|a| a.attempt.status == status)
        });
        let request = BotRequestId::mint();
        archive(&client, &bot, request);
        let parked = settled(&client, bot.id, request);
        assert_eq!(parked.stage, BotLifecycleStage::Incomplete, "{variant}");
        assert_eq!(
            parked.refusal,
            Some(BotLifecycleRefusal::PersonHolding),
            "{variant}"
        );
        // O27: no Stop and no termination reached the session.
        assert_eq!(port.stops.load(Ordering::SeqCst), 0, "{variant}");
        assert_eq!(port.native.state.lock().unwrap().stop_calls, 0, "{variant}");
        assert!(!port.native.state.lock().unwrap().interrupt_settled);
        // The Bot's read names the parked operation; the hold says it waits.
        let current = read_bot(&client, bot.id);
        let open = current.open_operation.clone().expect("open operation");
        assert_eq!(open.request_id, request);
        assert_eq!(open.stage, BotLifecycleStage::Incomplete);
        assert_eq!(open.refusal, Some(BotLifecycleRefusal::PersonHolding));
        let refused = client
            .bots(
                context(true),
                BotCommand::CreateChat {
                    request_id: BotRequestId::mint(),
                    bot_id: bot.id,
                },
            )
            .unwrap_err();
        assert_eq!(refused.code, PENDING);
        assert!(
            refused.message.contains("waiting for you"),
            "{}",
            refused.message
        );
        // Not retried on its own while it waits.
        let reads = port.reads.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(port.reads.load(Ordering::SeqCst), reads);
        // The person hands back; the owner's cleanup settles the task.
        port.holding.store(false, Ordering::SeqCst);
        port.native.hold_interrupt.store(false, Ordering::SeqCst);
        until("the interruption settled", || {
            port.native.state.lock().unwrap().interrupt_settled
        });
        // Resumed by its id alone, at the revision the read reported.
        archive_at(&client, &bot, request, current.revision).unwrap();
        let done = settled(&client, bot.id, request);
        assert_eq!(
            done.stage,
            BotLifecycleStage::Completed,
            "{variant} {done:?}"
        );
        assert_eq!(port.stops.load(Ordering::SeqCst), 0, "{variant}");
        worker.shutdown();
        assert_eq!(worker.join(), WorkerExit::Clean);
        let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            repository.attempt(attempt).unwrap().unwrap().status,
            status,
            "{variant}"
        );
    }
}

#[test]
fn an_api_task_holds_archive_until_the_delegate_settles_it_and_queued_work_never_starts() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let port = Arc::new(Archiving::default());
    let executing = api_task(&bot, d::BotAdmissionState::Executing, usize::MAX);
    let reserved = api_task(&bot, d::BotAdmissionState::Reserved, 0);
    let queued = api_task(&bot, d::BotAdmissionState::Queued, 0);
    let runs = [executing.view.run, reserved.view.run, queued.view.run];
    port.api
        .lock()
        .unwrap()
        .extend([executing, reserved, queued]);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let request = BotRequestId::mint();
    archive(&client, &bot, request);
    until("every API task interrupted", || {
        runs.iter().all(|run| port.interrupts_of(*run) >= 1)
    });
    // The reserved and queued tasks settled at their first interruption; the
    // executing one is still cleaning up, and Archive waits for it.
    std::thread::sleep(Duration::from_millis(800));
    let waiting = read_operation(&client, bot.id, request);
    assert_eq!(waiting.stage, BotLifecycleStage::Settling, "{waiting:?}");
    assert_eq!(port.interrupts_of(runs[2]), 1, "queued work is not re-sent");
    assert!(
        port.interrupts_of(runs[0]) >= 2,
        "the pending one is re-read"
    );
    port.api.lock().unwrap()[0].pending = 0;
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert!(port.unsettled(bot.id).is_empty());
    // Nothing started: no conversation turn, no Stop.
    assert_eq!(port.native.state.lock().unwrap().begin, 0);
    assert_eq!(port.stops.load(Ordering::SeqCst), 0);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn a_takeover_after_the_read_keeps_its_session_and_parks_archive() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let port = Arc::new(Archiving::default());
    let mut task = api_task(&bot, d::BotAdmissionState::Executing, 0);
    // The read finds the session free; the person takes it before the
    // Interrupt arrives.
    task.take_on_interrupt = true;
    let run = task.view.run;
    port.api.lock().unwrap().push(task);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let request = BotRequestId::mint();
    archive(&client, &bot, request);
    let parked = settled(&client, bot.id, request);
    assert_eq!(parked.stage, BotLifecycleStage::Incomplete);
    assert_eq!(parked.refusal, Some(BotLifecycleRefusal::PersonHolding));
    // One Interrupt, refused by the owner's own lease check; no Stop.
    assert_eq!(port.interrupts_of(run), 1);
    assert_eq!(port.stops.load(Ordering::SeqCst), 0);
    assert!(port.api.lock().unwrap()[0].held);
    // Handed back: resuming finishes it.
    port.api.lock().unwrap()[0].held = false;
    let current = read_bot(&client, bot.id);
    archive_at(&client, &bot, request, current.revision).unwrap();
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert_eq!(port.stops.load(Ordering::SeqCst), 0);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn a_parked_archive_rereads_the_delegate_after_a_repeat_and_after_an_ai_restart() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let request = captured_archive(&mut repository, &bot);
    // Parked by an earlier worker while work was still running.
    assert!(
        repository
            .park_bot_lifecycle(bot.user_scope, request, BotLifecycleRefusal::WorkRunning, 2)
            .unwrap()
    );
    let port = Arc::new(Archiving::default());
    let task = api_task(&bot, d::BotAdmissionState::Executing, usize::MAX);
    let run = task.view.run;
    port.api.lock().unwrap().push(task);
    let (worker, returned) = spawn(repository, authority.clone(), port.clone());
    let client = worker.client();
    // Parked operations are not resumed at a restart, nor retried on their own.
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(port.reads.load(Ordering::SeqCst), 0);
    let current = read_bot(&client, bot.id);
    let open = current.open_operation.clone().unwrap();
    assert_eq!(
        (open.request_id, open.refusal),
        (request, Some(BotLifecycleRefusal::WorkRunning))
    );
    // A repeat naming only the id resumes it; the work is read from the
    // delegate, which still runs it, so the Archive waits.
    archive_at(&client, &bot, request, current.revision).unwrap();
    until("a delegate read after the repeat", || {
        port.reads.load(Ordering::SeqCst) >= 1 && port.interrupts_of(run) >= 1
    });
    let moving = read_operation(&client, bot.id, request);
    assert_eq!(moving.stage, BotLifecycleStage::Settling, "{moving:?}");
    // Only the AI worker restarts, mid-settling. Nothing is remembered: the
    // journal resumes it and the delegate is asked again.
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    let reads = port.reads.load(Ordering::SeqCst);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    until("a delegate read after the restart", || {
        port.reads.load(Ordering::SeqCst) > reads
    });
    assert_eq!(
        read_operation(&client, bot.id, request).stage,
        BotLifecycleStage::Settling
    );
    port.api.lock().unwrap()[0].pending = 0;
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert_eq!(port.stops.load(Ordering::SeqCst), 0);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn a_parked_archive_found_through_the_bot_read_is_reversed_by_its_id() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let request = captured_archive(&mut repository, &bot);
    repository
        .park_bot_lifecycle(
            bot.user_scope,
            request,
            BotLifecycleRefusal::OwnerUnavailable,
            2,
        )
        .unwrap();
    let port = Arc::new(Archiving::default());
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    // The roster and the read both name it.
    let Ok(BotResponse::List { bots }) = client.bots(
        context(true),
        BotCommand::List {
            workspace: bot.home_workspace,
        },
    ) else {
        panic!("roster")
    };
    assert_eq!(
        bots[0].open_operation.as_ref().map(|o| o.request_id),
        Some(request)
    );
    let current = read_bot(&client, bot.id);
    let open = current.open_operation.unwrap();
    let BotResponse::Lifecycle { operation } = client
        .bots(
            context(true),
            BotCommand::Restore {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                expected_revision: current.revision,
                source_request_id: open.request_id,
            },
        )
        .unwrap()
    else {
        panic!("restore progress")
    };
    assert_eq!(operation.stage, BotLifecycleStage::Completed);
    let after = read_bot(&client, bot.id);
    assert_eq!(after.open_operation, None);
    assert!(after.paused);
    assert_eq!(after.lifecycle, BotLifecycle::Active);
    // The reversed operation reads as it stopped, and a repeat of it no
    // longer moves it.
    assert_eq!(
        archive_at(&client, &bot, request, after.revision)
            .unwrap()
            .stage,
        BotLifecycleStage::Incomplete
    );
    assert_eq!(port.reads.load(Ordering::SeqCst), 0);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn while_archive_is_unfinished_native_evidence_is_its_effects_off_barrier() {
    let Running {
        worker,
        client,
        returned: _returned,
        port,
        bot,
        attempt: _,
    } = running(true);
    let request = BotRequestId::mint();
    archive(&client, &bot, request);
    // A person holds the live task: parked, unfinished, not reversed.
    assert_eq!(
        settled(&client, bot.id, request).refusal,
        Some(BotLifecycleRefusal::PersonHolding)
    );
    let installed = port.policies.lock().unwrap()[0].clone();
    let submission = port
        .native
        .state
        .lock()
        .unwrap()
        .submission
        .clone()
        .unwrap();
    let evidence = client
        .native_policy(crate::worker::bots::policy::NativePolicyRequest {
            run: submission.request.run,
            provenance: submission.provenance.clone(),
            captured: submission.policy.clone(),
            target: None,
        })
        .map_err(|e| e.code)
        .unwrap();
    // The very barrier the operation installed: effects off, same request,
    // same revision. The gateway installing it again is a no-op.
    assert!(!evidence.barrier.effects_allowed);
    assert_eq!(evidence.barrier, installed);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn a_restart_resumes_a_captured_archive_from_the_journal_and_waits_for_the_barrier() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    // Captured by an earlier worker that stopped before any owner call.
    let request = captured_archive(&mut repository, &bot);
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
    // effective; the delegate was asked and reported nothing to settle.
    assert!(!port.barriers.lock().unwrap().is_empty());
    assert!(port.policies.lock().unwrap().len() >= 3);
    assert!(port.reads.load(Ordering::SeqCst) >= 1);
    assert_eq!(port.stops.load(Ordering::SeqCst), 0);
    assert!(port.interrupts.lock().unwrap().is_empty());
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
