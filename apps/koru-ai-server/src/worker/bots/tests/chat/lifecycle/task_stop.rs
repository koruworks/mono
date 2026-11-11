// SPDX-License-Identifier: MPL-2.0
//! `stop_task` (P1.8): the person's own Stop for a task started through the
//! API, named by its `start`/`continue` request. The fake owner is the
//! lifecycle fake, whose `Reconcile` answers as `Orchestrator::bot_reconcile`
//! does (the admission and its current fence, or Missing for a Run it never
//! admitted) and whose `Stop` checks the fence as `Orchestrator::bot_stop`
//! does (`koru-delegate-runtime` `orchestrator/bots.rs`): a queued Run takes
//! none, a running one its exact fence, and a person holding the session is
//! no reason to refuse.
use super::trash::api_task_in;
use super::*;

/// One of the Bot's API tasks its owner admitted, in `state`, with its AI
/// receipt accepted as the admission path leaves it.
fn admitted(repository: &mut Repository, bot: &Bot, state: d::BotAdmissionState) -> ApiTask {
    let mut task = api_task_in(repository, bot, bot.main_chat, state, 0);
    repository
        .acknowledge_bot_submission(
            BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            },
            bot.id,
            task.view.key.request,
            &task.view,
            2,
        )
        .unwrap();
    task.admitted = true;
    task
}

fn stop(client: &AiWorkerClient, bot: BotId, request: BotRequestId) -> ApiResult<BotTaskStop> {
    match client.bots(
        context(true),
        BotCommand::StopTask {
            request_id: request,
            bot_id: bot,
        },
    )? {
        BotResponse::TaskStop { stop } => Ok(stop),
        other => panic!("a task stop, not {other:?}"),
    }
}

fn state_of(port: &Archiving, run: d::RunId) -> d::BotAdmissionState {
    port.api
        .lock()
        .unwrap()
        .iter()
        .find(|task| task.view.run == run)
        .unwrap()
        .view
        .state
}

/// A queued task never starts: the Stop goes without a fence and the owner
/// settles it. The same Stop again finds it settled and sends nothing.
#[test]
fn a_queued_task_is_stopped_before_it_starts_and_a_repeat_finds_it_settled() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let task = admitted(&mut repository, &bot, d::BotAdmissionState::Queued);
    let (request, run) = (task.view.key.request, task.view.run);
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(task);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let stopped = stop(&client, bot.id, request).unwrap();
    assert_eq!(
        stopped,
        BotTaskStop {
            request_id: request,
            bot_id: bot.id,
            run_id: run,
            outcome: BotTaskStopOutcome::Stopped,
        }
    );
    assert_eq!(state_of(&port, run), d::BotAdmissionState::Settled);
    assert_eq!(port.stops_of(run), 1);
    assert_eq!(port.interrupts_of(run), 0);
    let again = stop(&client, bot.id, request).unwrap();
    assert_eq!(again.outcome, BotTaskStopOutcome::AlreadySettled);
    assert_eq!(port.stops_of(run), 1, "nothing is sent for a settled task");
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// A running task whose session a person takes as the Stop reaches it is
/// ended anyway: the person's own Stop needs no lease, and it is never the
/// automatic interruption.
#[test]
fn a_running_task_a_person_holds_is_ended_as_the_persons_stop() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let mut task = admitted(&mut repository, &bot, d::BotAdmissionState::Executing);
    task.take_on_stop = true;
    let (request, run) = (task.view.key.request, task.view.run);
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(task);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    assert_eq!(
        stop(&client, bot.id, request).unwrap().outcome,
        BotTaskStopOutcome::Stopped
    );
    assert_eq!(state_of(&port, run), d::BotAdmissionState::Settled);
    assert_eq!(port.stops_of(run), 1);
    assert_eq!(port.interrupts_of(run), 0);
    assert!(!port.api.lock().unwrap()[0].held, "the held session ended");
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

fn cancelled(port: &Archiving, run: d::RunId) -> bool {
    port.api
        .lock()
        .unwrap()
        .iter()
        .find(|task| task.view.run == run)
        .unwrap()
        .cancelled
}

/// A running task whose Stop its owner applies but cannot confirm at once
/// (`bot_stop_unconfirmed`: `Orchestrator::cancel` recorded the person's
/// cancellation, the process's end is not proven yet; the real owner did so
/// in the scripted native journey). One Stop is sent and the answer is
/// `stopping`; nothing is sent again by itself. The task is still unsettled
/// at its owner. The same `stop_task` again sends the Stop again, which the
/// owner now confirms: `stopped`.
#[test]
fn an_unconfirmed_stop_answers_stopping_and_the_caller_asks_again() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let mut task = admitted(&mut repository, &bot, d::BotAdmissionState::Executing);
    task.pending = 1;
    let (request, run) = (task.view.key.request, task.view.run);
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(task);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    assert_eq!(
        stop(&client, bot.id, request).unwrap().outcome,
        BotTaskStopOutcome::Stopping
    );
    assert_eq!(port.stops_of(run), 1, "one Stop, no resend loop");
    assert!(cancelled(&port, run));
    assert_eq!(state_of(&port, run), d::BotAdmissionState::Executing);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(port.stops_of(run), 1, "nothing sent again by itself");
    assert_eq!(
        stop(&client, bot.id, request).unwrap().outcome,
        BotTaskStopOutcome::Stopped
    );
    assert_eq!(port.stops_of(run), 2);
    assert_eq!(state_of(&port, run), d::BotAdmissionState::Settled);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// The process exits between the unconfirmed Stop and the caller's next
/// `stop_task`: the exit proved the end and the owner settled the task, so
/// the next one finds it settled and sends nothing.
#[test]
fn a_process_that_exits_after_an_unconfirmed_stop_is_found_settled() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let mut task = admitted(&mut repository, &bot, d::BotAdmissionState::Executing);
    task.pending = 1;
    task.exits_after_unconfirmed = true;
    let (request, run) = (task.view.key.request, task.view.run);
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(task);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    assert_eq!(
        stop(&client, bot.id, request).unwrap().outcome,
        BotTaskStopOutcome::Stopping
    );
    assert_eq!(
        stop(&client, bot.id, request).unwrap().outcome,
        BotTaskStopOutcome::AlreadySettled
    );
    assert_eq!(port.stops_of(run), 1);
    assert!(
        cancelled(&port, run),
        "recorded as the person's cancellation"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// A task that ends by itself between the admission read and the Stop: the
/// owner's Stop of an ended Run only retires it, and the Run's status (an
/// end by itself, not the person's cancellation) answers `already_settled`.
#[test]
fn a_task_that_ends_by_itself_before_the_stop_arrives_is_already_settled() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let mut task = admitted(&mut repository, &bot, d::BotAdmissionState::Executing);
    task.finishes_after_read = true;
    let (request, run) = (task.view.key.request, task.view.run);
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(task);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    assert_eq!(
        stop(&worker.client(), bot.id, request).unwrap().outcome,
        BotTaskStopOutcome::AlreadySettled
    );
    assert_eq!(port.stops_of(run), 1);
    assert!(!cancelled(&port, run));
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// Which thread each authority inspection runs on.
struct ThreadSpy {
    inner: Arc<Authority>,
    threads: Mutex<Vec<String>>,
}
impl BotAuthority for ThreadSpy {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.inner.owner()
    }
    fn workspace(&self, id: WorkspaceId) -> Result<koru_ai_core::ProfileId, BotError> {
        self.inner.workspace(id)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.inner.page(page)
    }
    fn inspect(
        &self,
        workspaces: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<koru_ai_core::bots::BotAuthoritySnapshot, BotError> {
        self.threads
            .lock()
            .unwrap()
            .push(std::thread::current().name().unwrap_or_default().to_owned());
        self.inner.inspect(workspaces, pages)
    }
}

/// A task Stop makes no Store call off the writer: its one authority
/// inspection is preparation's, on the preparation lane, and none runs on
/// the Stop lane or the task Stops' own (the lane review's probe, inverted:
/// it found one on the Stop lane).
#[test]
fn a_task_stop_inspects_authority_only_in_preparation() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let task = admitted(&mut repository, &bot, d::BotAdmissionState::Executing);
    let request = task.view.key.request;
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(task);
    let spy = Arc::new(ThreadSpy {
        inner: authority,
        threads: Mutex::new(Vec::new()),
    });
    let (return_tx, _returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-lifecycle"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(spy.clone())
    .with_bot_delegate(port);
    config.test_repository = Some(repository);
    config.test_repository_return = Some(return_tx);
    let worker = AiWorker::new(config).spawn().unwrap();
    let before = spy.threads.lock().unwrap().len();
    assert_eq!(
        stop(&worker.client(), bot.id, request).unwrap().outcome,
        BotTaskStopOutcome::Stopped
    );
    let threads = spy.threads.lock().unwrap()[before..].to_vec();
    assert_eq!(threads, ["koru-ai-provider-prepare"], "{threads:?}");
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// A task Stop its owner keeps answering unconfirmed holds nothing: it is
/// answered `stopping` at once with one Stop sent, and another task's Stop
/// right after it is answered promptly (the lane review's probe, inverted:
/// it found the first resending for its whole deadline, thirty Stops, and
/// the second waiting behind it).
#[test]
fn an_unconfirmed_task_stop_delays_no_other_stop() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let mut stuck = admitted(&mut repository, &bot, d::BotAdmissionState::Executing);
    stuck.pending = 100_000;
    let other = admitted(&mut repository, &bot, d::BotAdmissionState::Queued);
    let (stuck_request, stuck_run) = (stuck.view.key.request, stuck.view.run);
    let other_request = other.view.key.request;
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(stuck);
    port.api.lock().unwrap().push(other);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let started = Instant::now();
    assert_eq!(
        stop(&client, bot.id, stuck_request).unwrap().outcome,
        BotTaskStopOutcome::Stopping
    );
    assert_eq!(port.stops_of(stuck_run), 1);
    assert_eq!(
        stop(&client, bot.id, other_request).unwrap().outcome,
        BotTaskStopOutcome::Stopped
    );
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "the two Stops took {:?}",
        started.elapsed()
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// A finished task: nothing to stop and nothing sent.
#[test]
fn a_finished_task_answers_already_settled_and_sends_no_stop() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let task = admitted(&mut repository, &bot, d::BotAdmissionState::Settled);
    let (request, run) = (task.view.key.request, task.view.run);
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(task);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    assert_eq!(
        stop(&worker.client(), bot.id, request).unwrap().outcome,
        BotTaskStopOutcome::AlreadySettled
    );
    assert_eq!(port.stops_of(run), 0);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// A task its owner refused for good before admitting it (its receipt
/// `rejected` behind an acknowledged refusal fence): it never ran, and the
/// owner is not asked.
#[test]
fn a_task_refused_before_admission_never_ran_and_its_owner_is_not_asked() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let task = api_task_in(
        &mut repository,
        &bot,
        bot.main_chat,
        d::BotAdmissionState::Queued,
        0,
    );
    let (request, run) = (task.view.key.request, task.view.run);
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let fenced = repository
        .prepare_bot_refusal_fence(
            owner,
            bot.id,
            request,
            BotAdmissionRefusal::Rejected,
            false,
            2,
        )
        .unwrap();
    let Some(BotExecutionOutcome::RefusalFence { round, barrier, .. }) = fenced.outcome else {
        panic!("a refusal fence")
    };
    repository
        .acknowledge_bot_refusal_fence(owner, bot.id, request, round, &barrier, 3)
        .unwrap();
    repository
        .reject_bot_submission_after_fence(owner, bot.id, request, round, &barrier, 4)
        .unwrap();
    let port = Arc::new(Archiving::default());
    port.barrier.lock().unwrap().insert(bot.id, barrier);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let answered = stop(&worker.client(), bot.id, request).unwrap();
    assert_eq!(answered.outcome, BotTaskStopOutcome::NeverAdmitted);
    assert_eq!(answered.run_id, run);
    assert_eq!(port.stops_of(run), 0);
    assert_eq!(
        port.calls_in(
            "reconcile",
            request,
            Instant::now(),
            Duration::from_secs(60)
        ),
        0
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// A task still being delivered to its owner (receipt `prepared`, the owner
/// away) cannot be reached by a Stop yet: `bot_operation_pending`, retryable,
/// and the owner is asked for nothing on its account. Once delivered, the
/// same Stop stops it.
#[test]
fn a_task_still_being_delivered_answers_pending_until_its_owner_admits_it() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let task = api_task_in(
        &mut repository,
        &bot,
        bot.main_chat,
        d::BotAdmissionState::Queued,
        0,
    );
    let (request, run) = (task.view.key.request, task.view.run);
    let port = Arc::new(Archiving::default());
    port.unavailable.store(true, Ordering::SeqCst);
    port.api.lock().unwrap().push(task);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let pending = stop(&client, bot.id, request).unwrap_err();
    assert_eq!(pending.code, "bot_operation_pending");
    assert!(pending.retryable);
    assert_eq!(port.stops_of(run), 0);
    // The owner answers again: receipt recovery delivers the task.
    port.unavailable.store(false, Ordering::SeqCst);
    until("the task is accepted", || {
        matches!(
            client.bots(
                context(true),
                BotCommand::ReadOperation {
                    request_id: request,
                    bot_id: bot.id,
                },
            ),
            Ok(BotResponse::Operation { operation })
                if operation.status == BotOperationStatus::Accepted
        )
    });
    assert_eq!(
        stop(&client, bot.id, request).unwrap().outcome,
        BotTaskStopOutcome::Stopped
    );
    assert_eq!(state_of(&port, run), d::BotAdmissionState::Settled);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// An owner that does not answer: `bot_unavailable`, retryable, and nothing
/// owed is left behind; the same Stop once it answers stops the task.
#[test]
fn an_owner_that_does_not_answer_is_answered_retryable_and_the_same_stop_later_stops() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let task = admitted(&mut repository, &bot, d::BotAdmissionState::Executing);
    let (request, run) = (task.view.key.request, task.view.run);
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(task);
    port.unavailable.store(true, Ordering::SeqCst);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let away = stop(&client, bot.id, request).unwrap_err();
    assert_eq!(away.code, "bot_unavailable");
    assert!(away.retryable);
    assert_eq!(state_of(&port, run), d::BotAdmissionState::Executing);
    port.unavailable.store(false, Ordering::SeqCst);
    // Nothing was kept owed: the owner sees no Stop until it is sent again.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(state_of(&port, run), d::BotAdmissionState::Executing);
    assert_eq!(
        stop(&client, bot.id, request).unwrap().outcome,
        BotTaskStopOutcome::Stopped
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// Only the caller's own Bot's API tasks: an unknown request, another
/// Bot's task named under this Bot, a task a conversation started (whose
/// request a guess could name) and a pause are `not_found`; another owner's
/// Bot is `wrong_owner`. None of them reaches the owner as a Stop.
#[test]
fn only_this_bots_own_api_tasks_can_be_stopped() {
    let Running {
        worker,
        client,
        returned: _returned,
        port,
        bot,
        attempt: _,
    } = running(false);
    let conversation = port
        .native
        .state
        .lock()
        .unwrap()
        .submission
        .as_ref()
        .unwrap()
        .provenance
        .submission;
    let stops = port.native.state.lock().unwrap().stop_calls;
    for request in [BotRequestId::mint(), conversation] {
        assert_eq!(
            stop(&client, bot.id, request).unwrap_err().code,
            "not_found"
        );
    }
    let pause = BotRequestId::mint();
    assert!(matches!(
        client.bots(
            context(true),
            BotCommand::Pause {
                request_id: pause,
                bot_id: bot.id,
                expected_revision: bot.revision,
            },
        ),
        Ok(BotResponse::Operation { .. })
    ));
    assert_eq!(stop(&client, bot.id, pause).unwrap_err().code, "not_found");
    assert_eq!(port.native.state.lock().unwrap().stop_calls, stops);
    assert!(port.stops.lock().unwrap().is_empty());
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);

    // Another Bot of the same owner, and another owner's Bot.
    let (mut repository, authority, create) = fixture();
    let mine = created(&mut repository, authority.clone(), create);
    let (_, _, mut sibling) = fixture();
    if let BotCommand::Create {
        home_workspace,
        config,
        ..
    } = &mut sibling
    {
        *home_workspace = authority.workspace;
        config.role.workspace = authority.workspace;
    }
    let sibling = created(&mut repository, authority.clone(), sibling);
    let task = admitted(&mut repository, &mine, d::BotAdmissionState::Queued);
    let (_, other_authority, other_create) = fixture();
    let foreign = created(&mut repository, other_authority, other_create);
    let foreign_task = admitted(&mut repository, &foreign, d::BotAdmissionState::Queued);
    let port = Arc::new(Archiving::default());
    let request = task.view.key.request;
    port.api.lock().unwrap().push(task);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    assert_eq!(
        stop(&client, sibling.id, request).unwrap_err().code,
        "not_found"
    );
    assert_eq!(
        stop(&client, foreign.id, foreign_task.view.key.request)
            .unwrap_err()
            .code,
        "wrong_owner"
    );
    assert!(port.stops.lock().unwrap().is_empty());
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// A Stop only reduces work, so no hold refuses it: a paused Bot, one with
/// an unfinished Archive, an archived Bot and a trashed one each have their
/// running API task stopped.
#[test]
fn no_hold_refuses_a_stop() {
    for case in ["paused", "mid-operation", "archived", "trashed"] {
        let (mut repository, authority, create) = fixture();
        let bot = created(&mut repository, authority.clone(), create);
        let task = admitted(&mut repository, &bot, d::BotAdmissionState::Executing);
        let (request, run) = (task.view.key.request, task.view.run);
        match case {
            "paused" => repository
                .fixture_execute_batch(&format!(
                    "UPDATE bots SET paused = 1 WHERE bot_id = '{}'",
                    bot.id.to_storage()
                ))
                .unwrap(),
            "mid-operation" => {
                super::probe::parked_with_nothing_owed(&mut repository, &bot);
            }
            "archived" => repository
                .fixture_set_bot_lifecycle(bot.id, BotLifecycle::Archived)
                .unwrap(),
            _ => repository
                .fixture_set_bot_lifecycle(bot.id, BotLifecycle::Trashed)
                .unwrap(),
        }
        let port = Arc::new(Archiving::default());
        port.api.lock().unwrap().push(task);
        let (worker, _returned) = spawn(repository, authority, port.clone());
        let answered = stop(&worker.client(), bot.id, request)
            .map(|stop| stop.outcome)
            .map_err(|error| error.code);
        if case == "mid-operation" {
            // The Archive's owner answers its probe at once, and its own
            // round may stop the task first: either way nothing refused it.
            assert!(
                matches!(
                    answered,
                    Ok(BotTaskStopOutcome::Stopped | BotTaskStopOutcome::AlreadySettled)
                ),
                "{case}: {answered:?}"
            );
        } else {
            assert_eq!(answered, Ok(BotTaskStopOutcome::Stopped), "{case}");
        }
        assert_eq!(
            state_of(&port, run),
            d::BotAdmissionState::Settled,
            "{case}"
        );
        worker.shutdown();
        assert_eq!(worker.join(), WorkerExit::Clean);
    }
}
