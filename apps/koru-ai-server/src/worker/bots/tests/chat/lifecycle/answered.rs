// SPDX-License-Identifier: MPL-2.0
//! An operation parked because its owner did not answer takes up its work
//! again once the owner answers (the follow-up to the third lifecycle
//! review's F2): the owner restarts and starts a Run it had queued before the
//! AI side could deliver the operation's pause; the operation, taken up as a
//! person's Continue would take it up, stops that Run and completes. A
//! reversed operation stays reversed, and one parked for another reason
//! stays parked.
use super::unadmitted::{admitted_api_task_in, submission_status};
use super::*;

fn stop(worker: WorkerHandle, returned: Receiver<Repository>) -> Repository {
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    returned.recv_timeout(Duration::from_secs(5)).unwrap()
}

/// A Run the owner admitted before the operation, still queued, that its
/// dispatcher starts as soon as nothing holds the Bot paused.
fn queued_run(repository: &mut Repository, bot: &Bot) -> (ApiTask, d::RunId) {
    let mut queued = admitted_api_task_in(repository, bot, bot.main_chat);
    queued.view.state = d::BotAdmissionState::Queued;
    queued.view.fence = None;
    queued.dispatch = true;
    let run = queued.view.run;
    (queued, run)
}

/// An Archive captured by an earlier worker and parked for `refusal` before
/// its pause reached the owner.
fn parked_archive(
    repository: &mut Repository,
    bot: &Bot,
    refusal: BotLifecycleRefusal,
) -> BotRequestId {
    let operation = captured_archive(repository, bot);
    assert!(
        repository
            .park_bot_lifecycle(bot.user_scope, operation, refusal, 2)
            .unwrap()
    );
    operation
}

/// Read the operation until it completes.
fn completed(client: &AiWorkerClient, bot: BotId, request: BotRequestId) -> BotLifecycleProgress {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let operation = read_operation(client, bot, request);
        if operation.stage == BotLifecycleStage::Completed {
            return operation;
        }
        assert!(
            Instant::now() < deadline,
            "still {:?} ({:?})",
            operation.stage,
            operation.refusal
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn started_then_stopped(port: &Archiving, run: d::RunId) {
    let api = port.api.lock().unwrap();
    assert!(api[0].started, "the restarted owner started the queued Run");
    assert_eq!(api[0].view.state, d::BotAdmissionState::Settled);
    drop(api);
    assert!(port.stops_of(run) >= 1);
}

/// The owner is down when the Archive is captured; the Archive parks for
/// its silence with its pause owed. The owner restarts and its dispatcher
/// starts the Run it had queued, before anything reaches it. Its first
/// answer (the owed pause, acknowledged) takes the Archive up again, which
/// stops that Run as the person's Stop and completes.
#[test]
fn a_parked_archive_takes_up_its_work_when_its_owner_answers_and_stops_what_its_restart_started() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let (queued, run) = queued_run(&mut repository, &bot);
    let operation = parked_archive(&mut repository, &bot, BotLifecycleRefusal::OwnerUnavailable);
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(queued);
    port.unavailable.store(true, Ordering::SeqCst);
    let (worker, returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    std::thread::sleep(Duration::from_millis(300));
    port.restart_owner();
    port.unavailable.store(false, Ordering::SeqCst);
    let done = completed(&client, bot.id, operation);
    assert_eq!(done.refusal, None);
    started_then_stopped(&port, run);
    assert_eq!(read_bot(&client, bot.id).lifecycle, BotLifecycle::Archived);
    let _ = stop(worker, returned);
}

/// The same, with the AI worker restarted while the owner is away: the new
/// worker's first contact with the owner takes the parked Archive up.
#[test]
fn a_parked_archive_is_taken_up_after_an_ai_restart_when_its_owner_answers() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let (queued, run) = queued_run(&mut repository, &bot);
    let operation = parked_archive(&mut repository, &bot, BotLifecycleRefusal::OwnerUnavailable);
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(queued);
    port.unavailable.store(true, Ordering::SeqCst);
    let (worker, returned) = spawn(repository, authority.clone(), port.clone());
    std::thread::sleep(Duration::from_millis(300));
    let repository = stop(worker, returned);
    port.restart_owner();
    port.unavailable.store(false, Ordering::SeqCst);
    let (worker, returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    completed(&client, bot.id, operation);
    started_then_stopped(&port, run);
    let _ = stop(worker, returned);
}

/// An Archive whose pause the owner acknowledged, parked in `settling`
/// because the owner then stopped answering while a Run it had started
/// kept running: nothing is owed to the owner, so only the recovery pass's
/// probe can learn that it is back. Its answer takes the Archive up, which
/// stops the Run and completes.
#[test]
fn a_parked_archive_with_nothing_owed_learns_its_owner_is_back_and_stops_the_run() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let operation = captured_archive(&mut repository, &bot);
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
    let barrier = receipt.intent.barrier.clone();
    repository
        .acknowledge_bot_pause(owner, bot.id, pause, &barrier, 3)
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
    assert!(
        repository
            .park_bot_lifecycle(
                bot.user_scope,
                operation,
                BotLifecycleRefusal::OwnerUnavailable,
                5
            )
            .unwrap()
    );
    let port = Arc::new(Archiving::default());
    // The owner holds the acknowledged pause, and runs a Run regardless.
    port.barrier.lock().unwrap().insert(bot.id, barrier);
    let running = api_task(&bot, d::BotAdmissionState::Executing, 0);
    let run = running.view.run;
    port.api.lock().unwrap().push(running);
    port.unavailable.store(true, Ordering::SeqCst);
    let (worker, returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    std::thread::sleep(Duration::from_millis(300));
    port.unavailable.store(false, Ordering::SeqCst);
    completed(&client, bot.id, operation);
    assert!(port.stops_of(run) >= 1);
    assert_eq!(
        port.api.lock().unwrap()[0].view.state,
        d::BotAdmissionState::Settled
    );
    let _ = stop(worker, returned);
}

/// A parked Archive the engine takes up again by itself, once its owner
/// answers, moves with no command of the person's. A Restore pressed at that
/// moment answers `bot_lifecycle_pending`, retryable (the fourth lifecycle
/// review's F5): sent again once the Archive has finished, it is accepted.
#[test]
fn a_restore_of_an_archive_the_engine_took_up_again_is_retryable_and_accepted_once_it_ends() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let operation = captured_archive(&mut repository, &bot);
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
    let barrier = receipt.intent.barrier.clone();
    repository
        .acknowledge_bot_pause(owner, bot.id, pause, &barrier, 3)
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
    assert!(
        repository
            .park_bot_lifecycle(
                bot.user_scope,
                operation,
                BotLifecycleRefusal::OwnerUnavailable,
                5
            )
            .unwrap()
    );
    let port = Arc::new(Archiving::default());
    port.barrier.lock().unwrap().insert(bot.id, barrier);
    // A Run whose Stop the owner keeps answering unconfirmed: the Archive,
    // once taken up, keeps moving.
    let running = api_task(&bot, d::BotAdmissionState::Executing, usize::MAX);
    port.api.lock().unwrap().push(running);
    let (worker, returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    until("the engine took the parked Archive up again", || {
        !matches!(
            read_operation(&client, bot.id, operation).stage,
            BotLifecycleStage::Incomplete | BotLifecycleStage::Completed
        )
    });
    let refused =
        super::trash::restore_at(&client, &bot, operation, read_bot(&client, bot.id).revision)
            .unwrap_err();
    assert_eq!(refused.code, PENDING);
    assert!(refused.retryable, "a moving source ends: try again");
    // The Run's Stop lands; the Archive completes; the same Restore, sent
    // again, is accepted.
    port.api.lock().unwrap()[0].pending = 0;
    let deadline = Instant::now() + Duration::from_secs(30);
    let restored = loop {
        let revision = match client.bots(
            context(true),
            BotCommand::ListLifecycle {
                workspace: bot.home_workspace,
                lifecycle: BotLifecycle::Archived,
            },
        ) {
            Ok(BotResponse::List { bots }) => {
                bots.iter().find(|b| b.id == bot.id).map(|b| b.revision)
            }
            _ => None,
        }
        .unwrap_or_else(|| read_bot(&client, bot.id).revision);
        match super::trash::restore_at(&client, &bot, operation, revision) {
            Ok(progress) => break progress,
            Err(error) if error.retryable && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => panic!("restore {}", error.code),
        }
    };
    assert_eq!(restored.stage, BotLifecycleStage::Completed);
    let back = read_bot(&client, bot.id);
    assert_eq!(back.lifecycle, BotLifecycle::Active);
    assert!(back.paused);
    let _ = stop(worker, returned);
}

/// A parked Archive the person reversed is not taken up when the owner
/// answers, and nothing stops on its behalf: a Run the person starts after
/// Resume must not be ended by an operation they reversed. What the person
/// reads is true: the Bot is paused, with no open operation; the reversed
/// operation reads incomplete; the Run the restarted owner started before
/// the pause landed keeps running, and its Start reads accepted with that
/// Run. No AI read says it is running and no per-Run Stop reaches it (an
/// API-started Bot Run has none: the ordinary Run routes refuse Bot Runs);
/// the person's own action that stops it is a new Archive, which stops it
/// as their Stop and completes.
#[test]
fn a_reversed_parked_archive_stays_reversed_and_the_persons_archive_stops_what_slipped_through() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let (queued, run) = queued_run(&mut repository, &bot);
    let start = queued.view.key.request;
    let operation = parked_archive(&mut repository, &bot, BotLifecycleRefusal::OwnerUnavailable);
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(queued);
    port.unavailable.store(true, Ordering::SeqCst);
    let (worker, returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    super::trash::restore_at(&client, &bot, operation, read_bot(&client, bot.id).revision).unwrap();
    port.restart_owner();
    port.unavailable.store(false, Ordering::SeqCst);
    until("the owner holds the Bot paused", || {
        port.barrier
            .lock()
            .unwrap()
            .get(&bot.id)
            .is_some_and(|b| b.paused)
    });
    std::thread::sleep(Duration::from_millis(1500));
    let reversed = read_operation(&client, bot.id, operation);
    assert_eq!(reversed.stage, BotLifecycleStage::Incomplete);
    let read = read_bot(&client, bot.id);
    assert!(read.paused);
    assert_eq!(read.lifecycle, BotLifecycle::Active);
    assert_eq!(read.open_operation, None);
    assert_eq!(port.stops_of(run), 0, "a reversed operation stops nothing");
    assert_eq!(
        port.api.lock().unwrap()[0].view.state,
        d::BotAdmissionState::Executing
    );
    let BotResponse::Operation { operation: started } = client
        .bots(
            context(true),
            BotCommand::ReadOperation {
                request_id: start,
                bot_id: bot.id,
            },
        )
        .unwrap()
    else {
        panic!("operation")
    };
    assert_eq!(started.status, BotOperationStatus::Accepted);
    assert_eq!(started.run_id, Some(run));
    // The person archives the Bot again: their Stop reaches the Run.
    let again = BotRequestId::mint();
    archive_at(&client, &bot, again, read.revision).unwrap();
    completed(&client, bot.id, again);
    assert!(port.stops_of(run) >= 1);
    assert_eq!(
        port.api.lock().unwrap()[0].view.state,
        d::BotAdmissionState::Settled
    );
    let _ = stop(worker, returned);
}

/// An operation parked for another reason than its owner's silence is not
/// taken up when the owner answers: it waits for the person.
#[test]
fn an_archive_parked_for_running_work_stays_parked_when_its_owner_answers() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let operation = parked_archive(&mut repository, &bot, BotLifecycleRefusal::WorkRunning);
    let pause = repository
        .bot_lifecycle_operation(bot.user_scope, operation)
        .unwrap()
        .unwrap()
        .pause_request
        .unwrap();
    let port = Arc::new(Archiving::default());
    let (worker, returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    until("the owner acknowledged the pause", || {
        submission_status(&client, bot.id, pause) == BotOperationStatus::Accepted
    });
    std::thread::sleep(Duration::from_millis(1500));
    let parked = read_operation(&client, bot.id, operation);
    assert_eq!(parked.stage, BotLifecycleStage::Incomplete);
    assert_eq!(parked.refusal, Some(BotLifecycleRefusal::WorkRunning));
    assert_eq!(port.reads.load(Ordering::SeqCst), 0);
    let _ = stop(worker, returned);
}
