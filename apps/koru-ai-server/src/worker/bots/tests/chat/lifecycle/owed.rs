// SPDX-License-Identifier: MPL-2.0
//! What an operation leaves owed to an owner that refuses it, restarts or
//! is slow to begin a task (the third lifecycle review: F2, F3, F5).
use super::unadmitted::{admitted_api_task_in, submission_status};
use super::*;

fn start(
    repository: Repository,
    authority: Arc<Authority>,
    port: Arc<Archiving>,
) -> (WorkerHandle, AiWorkerClient, Receiver<Repository>) {
    let (worker, returned) = spawn(repository, authority, port);
    let client = worker.client();
    (worker, client, returned)
}

fn stop(worker: WorkerHandle, returned: Receiver<Repository>) -> Repository {
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    returned.recv_timeout(Duration::from_secs(5)).unwrap()
}

fn resume(client: &AiWorkerClient, bot: BotId) -> BotRequestId {
    let request = BotRequestId::mint();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match client.bots(
            context(true),
            BotCommand::Resume {
                request_id: request,
                bot_id: bot,
                expected_revision: read_bot(client, bot).revision,
            },
        ) {
            Ok(BotResponse::Operation { .. }) => return request,
            Err(error) if error.retryable && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            other => panic!("resume {:?}", other.map(|_| ()).map_err(|e| e.code)),
        }
    }
}

/// An Archive parked and reversed while the owner holds a newer barrier for
/// the Bot than the operation's pause (its epoch is ahead): the owner
/// answers that pause `bot_stale`, for good. The pause is settled rejected
/// and the AI side's state is offered again at newer revisions until the
/// owner takes it, so the Bot is paused at its owner as AI says, and Resume
/// is accepted: nothing waits on a pause that can never land.
#[test]
fn a_pause_the_owner_answers_stale_is_offered_again_newer_and_never_wedges_the_bot() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let operation = captured_archive(&mut repository, &bot);
    assert!(
        repository
            .park_bot_lifecycle(
                bot.user_scope,
                operation,
                BotLifecycleRefusal::OwnerUnavailable,
                2
            )
            .unwrap()
    );
    let port = Arc::new(Archiving::default());
    // The owner holds an unpaused barrier well ahead of the AI revision.
    port.barrier.lock().unwrap().insert(
        bot.id,
        d::BotAdmissionBarrier {
            owner: owner_of(&bot),
            bot: bot.id,
            request: BotRequestId::mint(),
            epoch: bot.revision + 12,
            paused: false,
        },
    );
    let (worker, client, returned) = start(repository, authority, port.clone());
    super::trash::restore_at(&client, &bot, operation, read_bot(&client, bot.id).revision).unwrap();
    until("the owner holds the Bot paused", || {
        port.barrier
            .lock()
            .unwrap()
            .get(&bot.id)
            .is_some_and(|b| b.paused)
    });
    assert!(read_bot(&client, bot.id).paused);
    let resumed = resume(&client, bot.id);
    until("the Bot resumed", || {
        submission_status(&client, bot.id, resumed) == BotOperationStatus::Accepted
    });
    assert!(!port.barrier.lock().unwrap()[&bot.id].paused);
    let _ = stop(worker, returned);
}

/// A host that will never take a barrier for this owner (another device):
/// the owed pause is settled rejected rather than wedging the Bot, and
/// every later change is accepted for preparation, never answered
/// `bot_operation_pending` for good.
#[test]
fn a_pause_the_owner_can_never_take_is_settled_rejected_and_wedges_nothing() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let operation = captured_archive(&mut repository, &bot);
    let pause = repository
        .bot_lifecycle_operation(bot.user_scope, operation)
        .unwrap()
        .unwrap()
        .pause_request
        .unwrap();
    assert!(
        repository
            .park_bot_lifecycle(
                bot.user_scope,
                operation,
                BotLifecycleRefusal::OwnerUnavailable,
                2
            )
            .unwrap()
    );
    let port = Arc::new(Archiving::default());
    // The host serves another device: it refuses every command of this one.
    *port.device.lock().unwrap() = Some(d::DeviceId::from_uuid(Uuid::new_v4()));
    let (worker, client, returned) = start(repository, authority, port.clone());
    super::trash::restore_at(&client, &bot, operation, read_bot(&client, bot.id).revision).unwrap();
    until("the pause reads rejected", || {
        submission_status(&client, bot.id, pause) == BotOperationStatus::Rejected
    });
    let resumed = resume(&client, bot.id);
    until("the Resume is settled too", || {
        submission_status(&client, bot.id, resumed) != BotOperationStatus::Pending
    });
    let _ = stop(worker, returned);
}

/// The owner restarts and its dispatcher starts a Run it had queued before
/// an Archive was captured, before the AI side could deliver the Archive's
/// pause (the window only the delegate can close). A moving Archive then
/// stops it as the person's Stop before it completes.
#[test]
fn a_run_the_owner_starts_on_its_own_restart_is_stopped_by_a_moving_archive() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let mut queued = admitted_api_task_in(&mut repository, &bot, bot.main_chat);
    queued.view.state = d::BotAdmissionState::Queued;
    queued.view.fence = None;
    queued.dispatch = true;
    let run = queued.view.run;
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(queued);
    port.unavailable.store(true, Ordering::SeqCst);
    let (worker, client, returned) = start(repository, authority, port.clone());
    let operation = BotRequestId::mint();
    archive_at(&client, &bot, operation, bot.revision).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    // The owner comes back from its own restart and dispatches at once.
    port.restart_owner();
    port.unavailable.store(false, Ordering::SeqCst);
    assert!(port.api.lock().unwrap()[0].started);
    let done = settled(&client, bot.id, operation);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert!(port.stops_of(run) >= 1);
    assert_eq!(
        port.api.lock().unwrap()[0].view.state,
        d::BotAdmissionState::Settled
    );
    let _ = stop(worker, returned);
}

/// A task waiting to send its first input (its Begin retried after a
/// retryable refusal) when its Bot is archived never begins: the operation
/// stops it as the person's Stop, and the runtime's own check before Begin
/// refuses to send it while the hold stands.
#[test]
fn a_task_waiting_to_begin_never_begins_once_its_bot_is_held() {
    let native = Native::default();
    for _ in 0..2 {
        native
            .begin_failures
            .lock()
            .unwrap()
            .push_back(koru_delegate_api::Failure::new(
                "bot_unavailable",
                "fixture busy",
                true,
            ));
    }
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
    let port = Arc::new(Archiving {
        native,
        ..Archiving::default()
    });
    let (worker, client, returned) = start(repository, authority, port.clone());
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::SendTurn {
            context,
            request: request(&bot),
            reply,
        })
        .unwrap();
    let accepted = response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .accepted;
    until("its first Begin was refused", || {
        port.native.state.lock().unwrap().begin_at.len() >= 1
    });
    let begins = port.native.state.lock().unwrap().begin_at.len();
    let operation = BotRequestId::mint();
    archive_at(&client, &bot, operation, bot.revision).unwrap();
    let done = settled(&client, bot.id, operation);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(port.native.state.lock().unwrap().begin_at.len(), begins);
    let repository = stop(worker, returned);
    assert!(stopped_by_the_person(&repository, accepted.attempt_id));
}
