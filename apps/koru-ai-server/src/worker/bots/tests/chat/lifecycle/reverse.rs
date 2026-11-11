// SPDX-License-Identifier: MPL-2.0
//! Reversing an Archive or a Trash that parked because the delegate owner
//! was away, and the owner coming back (P1.7, the second lifecycle review).
//!
//! The owner is unreachable from before the operation is captured, so its
//! pause never lands and nothing the operation captured is ever admitted:
//! a Send in its conversation and a task started through the API, both
//! still waiting to reach the owner. The person reverses the parked
//! operation, the owner comes back, and then the Bot must read paused,
//! Resume must work with no restart, nothing captured may ever start, the
//! Send reads stopped by the person and the API task refused, and no step
//! may ask the owner faster than its backoff allows. Each case runs again
//! through an AI-only restart at each stage: the fake owner (and so what the
//! delegate holds) survives, the worker and its memory do not.
//!
//! Parking takes twelve backoff rounds (minutes), so the journal is parked
//! between two workers, exactly as the driver writes it.
use super::trash::api_task_in;
use super::unadmitted::{admitted_api_task_in, send_in, submission_status};
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Archive,
    Trash,
}

/// Where an extra AI-only restart happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Restart {
    /// Only the one the park needs.
    Park,
    /// Also while the operation is still captured.
    Captured,
    /// Also after the reversal, while the owner is still away.
    Reversed,
    /// Also the moment the owner is back, before anything reached it.
    Returned,
}

/// How long the owner stays away after the reversal, and the window at its
/// end in which each thing may be asked only a few times: every retry has
/// backed off to seconds by then, where a 250 ms poll would ask ten times.
const AWAY: Duration = Duration::from_secs(6);
const WINDOW: Duration = Duration::from_millis(2500);
const AT_MOST: usize = 6;

struct Worker {
    worker: WorkerHandle,
    returned: Receiver<Repository>,
    client: AiWorkerClient,
}
impl Worker {
    fn start(repository: Repository, authority: Arc<Authority>, port: Arc<Archiving>) -> Self {
        let (worker, returned) = spawn(repository, authority, port);
        let client = worker.client();
        Self {
            worker,
            returned,
            client,
        }
    }
    fn stop(self) -> Repository {
        self.worker.shutdown();
        assert_eq!(self.worker.join(), WorkerExit::Clean);
        self.returned.recv_timeout(Duration::from_secs(5)).unwrap()
    }
    /// An AI-only restart: the owner and its state stay as they are.
    fn restart(self, authority: Arc<Authority>, port: Arc<Archiving>) -> Self {
        Self::start(self.stop(), authority, port)
    }
}

fn reversed_while_the_owner_is_away(kind: Kind, restart: Restart) {
    let case = format!("{kind:?} {restart:?}");
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let chat = repository
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    // A first task, admitted and settled long ago, freezes the folder so
    // that more can wait beside each other.
    let earlier = admitted_api_task_in(&mut repository, &bot, bot.main_chat);
    let earlier_request = earlier.view.key.request;
    // A task started through the API: prepared in AI, never admitted. Were
    // it ever submitted, the owner would admit it and start it.
    let mut start = api_task_in(
        &mut repository,
        &bot,
        bot.main_chat,
        d::BotAdmissionState::Queued,
        0,
    );
    start.admitted = false;
    start.dispatch = true;
    let (start_run, start_request) = (start.view.run, start.view.key.request);
    let port = Arc::new(Archiving::default());
    port.unavailable.store(true, Ordering::SeqCst);
    port.api.lock().unwrap().extend([earlier, start]);
    let mut worker = Worker::start(repository, authority.clone(), port.clone());
    // A Send in the main conversation; its owner never answers.
    let send = send_in(
        &worker.client,
        proof(authority.clone(), &bot, chat),
        &bot,
        bot.main_chat,
    );
    let send_request = {
        let mut found = None;
        until("the Send asked its owner", || {
            found = port
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter_map(|(_, name, key)| (*name == "reconcile").then_some(*key).flatten())
                .find(|key| *key != start_request && *key != earlier_request);
            found.is_some()
        });
        found.unwrap()
    };
    // The person archives (or trashes) the Bot.
    let operation = BotRequestId::mint();
    let revision = read_bot(&worker.client, bot.id).revision;
    match kind {
        Kind::Archive => archive_at(&worker.client, &bot, operation, revision),
        Kind::Trash => super::trash::trash_at(&worker.client, &bot, operation, revision),
    }
    .unwrap();
    if restart == Restart::Captured {
        worker = worker.restart(authority.clone(), port.clone());
        std::thread::sleep(Duration::from_millis(800));
    }
    // It parks: the owner never answered.
    let mut repository = worker.stop();
    assert!(
        repository
            .park_bot_lifecycle(
                bot.user_scope,
                operation,
                BotLifecycleRefusal::OwnerUnavailable,
                Timestamp::now().millis(),
            )
            .unwrap(),
        "{case}"
    );
    let pause = repository
        .bot_lifecycle_operation(bot.user_scope, operation)
        .unwrap()
        .unwrap()
        .pause_request
        .unwrap();
    let mut worker = Worker::start(repository, authority.clone(), port.clone());
    let parked = read_bot(&worker.client, bot.id);
    assert_eq!(
        parked.open_operation.map(|open| open.request_id),
        Some(operation),
        "{case}"
    );
    // The person reverses it while the owner is still away.
    let reversed = super::trash::restore_at(&worker.client, &bot, operation, parked.revision)
        .unwrap_or_else(|error| panic!("{case}: restore {}", error.code));
    assert_eq!(reversed.stage, BotLifecycleStage::Completed, "{case}");
    let restored = read_bot(&worker.client, bot.id);
    assert!(restored.paused, "{case}");
    assert_eq!(restored.lifecycle, BotLifecycle::Active, "{case}");
    assert_eq!(restored.open_operation, None, "{case}");
    if restart == Restart::Reversed {
        worker = worker.restart(authority.clone(), port.clone());
    }
    // While the owner stays away, every retry backs off.
    std::thread::sleep(AWAY);
    let end = Instant::now();
    for (what, request) in [
        ("reconcile", send_request),
        ("reconcile", start_request),
        ("barrier", pause),
    ] {
        let asked = port.calls_in(what, request, end, WINDOW);
        assert!(asked <= AT_MOST, "{case}: {what} asked {asked} times");
    }
    // The owner comes back.
    port.unavailable.store(false, Ordering::SeqCst);
    if restart == Restart::Returned {
        worker = worker.restart(authority.clone(), port.clone());
    }
    let client = &worker.client;
    // An API client that kept polling its task asks for it again.
    let _ = client.bots(
        context(true),
        BotCommand::ReconcileOperation {
            bot_id: bot.id,
            request_id: start_request,
        },
    );
    // Resume works with no restart: an answer that something is still
    // settling is retryable, and a client sends it again.
    let resume = BotRequestId::mint();
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let revision = read_bot(client, bot.id).revision;
        match client.bots(
            context(true),
            BotCommand::Resume {
                request_id: resume,
                bot_id: bot.id,
                expected_revision: revision,
            },
        ) {
            Ok(BotResponse::Operation { .. }) => break,
            Err(error) if error.retryable && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            other => panic!(
                "{case}: resume {:?}",
                other.map(|_| ()).map_err(|error| error.code)
            ),
        }
    }
    until("the Bot resumed", || {
        submission_status(client, bot.id, resume) == BotOperationStatus::Accepted
    });
    assert!(!read_bot(client, bot.id).paused, "{case}");
    // What the operation captured stays refused.
    until("the API task reads refused", || {
        submission_status(client, bot.id, start_request) == BotOperationStatus::Rejected
    });
    until("the Send's task reads refused", || {
        submission_status(client, bot.id, send_request) == BotOperationStatus::Rejected
    });
    // Once refused, nobody asks about either any more.
    std::thread::sleep(Duration::from_millis(1500));
    let end = Instant::now();
    for request in [send_request, start_request] {
        let asked = port.calls_in("reconcile", request, end, Duration::from_millis(1400))
            + port.calls_in("submit", request, end, Duration::from_millis(1400));
        assert!(asked <= 1, "{case}: still asked {asked} times");
    }
    // Nothing captured ever reached the owner or started.
    assert!(
        port.submits.lock().unwrap().is_empty(),
        "{case}: submitted {:?}",
        port.submits.lock().unwrap()
    );
    {
        let api = port.api.lock().unwrap();
        let start = api.iter().find(|t| t.view.run == start_run).unwrap();
        assert!(!start.admitted && !start.started, "{case}");
    }
    assert_eq!(port.native.state.lock().unwrap().begin, 0, "{case}");
    super::trash::no_owner_effects(&port);
    let repository = worker.stop();
    // The Send reads stopped by the person.
    assert!(
        stopped_by_the_person(&repository, send.attempt_id),
        "{case}"
    );
}

#[test]
fn an_archive_reversed_while_its_owner_is_away_leaves_a_paused_bot_that_resumes_and_starts_nothing()
{
    reversed_while_the_owner_is_away(Kind::Archive, Restart::Park);
}

#[test]
fn a_trash_reversed_while_its_owner_is_away_leaves_a_paused_bot_that_resumes_and_starts_nothing() {
    reversed_while_the_owner_is_away(Kind::Trash, Restart::Park);
}

#[test]
fn the_same_through_a_restart_while_the_operation_is_captured() {
    for kind in [Kind::Archive, Kind::Trash] {
        reversed_while_the_owner_is_away(kind, Restart::Captured);
    }
}

#[test]
fn the_same_through_a_restart_after_the_reversal() {
    for kind in [Kind::Archive, Kind::Trash] {
        reversed_while_the_owner_is_away(kind, Restart::Reversed);
    }
}

#[test]
fn the_same_through_a_restart_the_moment_the_owner_is_back() {
    for kind in [Kind::Archive, Kind::Trash] {
        reversed_while_the_owner_is_away(kind, Restart::Returned);
    }
}

/// A Send its owner never admitted, stopped by an Archive whose pause the
/// owner keeps refusing: its refusal fence cannot land, so its receipt stays
/// prepared, and each Reconcile answering Missing waits longer than the last
/// instead of asking four times a second.
#[test]
fn a_stopped_send_the_owner_never_admitted_backs_off_while_its_refusal_cannot_land() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let chat = repository
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    let port = Arc::new(Archiving::default());
    port.unavailable.store(true, Ordering::SeqCst);
    let worker = Worker::start(repository, authority.clone(), port.clone());
    let send = send_in(
        &worker.client,
        proof(authority.clone(), &bot, chat),
        &bot,
        bot.main_chat,
    );
    let send_request = {
        let mut found = None;
        until("the Send asked its owner", || {
            found = port
                .calls
                .lock()
                .unwrap()
                .iter()
                .find_map(|(_, name, key)| (*name == "reconcile").then_some(*key).flatten());
            found.is_some()
        });
        found.unwrap()
    };
    // The owner answers again, but refuses every barrier.
    port.refuse_barriers.store(true, Ordering::SeqCst);
    port.unavailable.store(false, Ordering::SeqCst);
    let operation = BotRequestId::mint();
    archive_at(
        &worker.client,
        &bot,
        operation,
        read_bot(&worker.client, bot.id).revision,
    )
    .unwrap();
    until("the Send reads stopped", || {
        super::unadmitted::attempt_status(&worker.client, send.attempt_id)
            == AttemptStatus::Cancelled
    });
    std::thread::sleep(Duration::from_secs(4));
    let asked = port.calls_in(
        "reconcile",
        send_request,
        Instant::now(),
        Duration::from_secs(3),
    );
    assert!(asked <= 5, "asked {asked} times in three seconds");
    assert!(port.submits.lock().unwrap().is_empty());
    let _ = worker.stop();
}

/// A queued Run the owner would start the moment it reserved it stays
/// queued once an Archive's pause has landed (`reserve_bot` answers Paused
/// and gives it no fence), and the Archive's Stop, with no fence, cancels it
/// unstarted.
#[test]
fn a_queued_run_stays_queued_under_the_archives_pause_and_is_stopped_unstarted() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let mut queued = admitted_api_task_in(&mut repository, &bot, bot.main_chat);
    queued.view.state = d::BotAdmissionState::Queued;
    queued.view.fence = None;
    queued.dispatch = true;
    let run = queued.view.run;
    // Nothing asks the owner about the Bot's work before the Archive's pause
    // lands; had anything, its dispatcher would have started the Run.
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(queued);
    let worker = Worker::start(repository, authority, port.clone());
    let operation = BotRequestId::mint();
    archive_at(&worker.client, &bot, operation, bot.revision).unwrap();
    let done = settled(&worker.client, bot.id, operation);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert!(port.stops_of(run) >= 1);
    {
        let api = port.api.lock().unwrap();
        assert!(!api[0].started, "the queued Run started");
        assert_eq!(api[0].view.state, d::BotAdmissionState::Settled);
        assert_eq!(api[0].view.fence, None);
    }
    let _ = worker.stop();
}

/// The owner admits an API task but answers `bot_context_unavailable`: AI
/// fences it, finds it admitted after the fence, records it accepted, and an
/// Archive then stops it like any admitted Run.
#[test]
fn an_admission_answered_context_unavailable_is_found_admitted_and_archive_stops_it() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let mut start = api_task_in(
        &mut repository,
        &bot,
        bot.main_chat,
        d::BotAdmissionState::Queued,
        0,
    );
    start.admitted = false;
    start.context_unavailable = true;
    let (run, request) = (start.view.run, start.view.key.request);
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(start);
    let worker = Worker::start(repository, authority, port.clone());
    let client = &worker.client;
    // Startup recovery reconciles it, finds it missing and submits it.
    until("the task reads accepted", || {
        submission_status(client, bot.id, request) == BotOperationStatus::Accepted
    });
    assert_eq!(port.submits_of(run), 1);
    let operation = BotRequestId::mint();
    archive_at(client, &bot, operation, read_bot(client, bot.id).revision).unwrap();
    let done = settled(client, bot.id, operation);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert!(port.stops_of(run) >= 1);
    assert_eq!(
        port.api.lock().unwrap()[0].view.state,
        d::BotAdmissionState::Settled
    );
    let _ = worker.stop();
}

/// A conversation's Trash refuses its own waiting task behind one fence;
/// its siblings' waiting tasks then reach an owner whose barrier is newer
/// than their revision, are refused as stale and fenced in turn, each once,
/// or are admitted if they got there first. However they interleave, every
/// receipt ends, the trashed conversation's is never submitted, and the
/// revision moves a bounded number of times: each renewal of a fence needs
/// another receipt's barrier to have landed first, and a barrier lands at
/// most once per receipt.
#[test]
fn a_conversation_trash_refusal_cascade_through_its_siblings_ends() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let sibling = super::trash::another_chat(&mut repository, &authority, &bot);
    let earlier = admitted_api_task_in(&mut repository, &bot, bot.main_chat);
    let mut tasks = vec![earlier];
    let mut waiting = Vec::new();
    for chat in [bot.main_chat, sibling, sibling] {
        let mut task = api_task_in(
            &mut repository,
            &bot,
            chat,
            d::BotAdmissionState::Settled,
            0,
        );
        task.admitted = false;
        waiting.push((chat, task.view.run, task.view.key.request));
        tasks.push(task);
    }
    let revision = repository
        .bot_any_lifecycle(bot.id)
        .unwrap()
        .unwrap()
        .0
        .revision;
    let operation = super::trash::captured_chat_trash(&mut repository, &bot, bot.main_chat);
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().extend(tasks);
    let worker = Worker::start(repository, authority, port.clone());
    let client = &worker.client;
    let done = settled(client, bot.id, operation);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    for (_, _, request) in &waiting {
        until("every waiting task ended", || {
            submission_status(client, bot.id, *request) != BotOperationStatus::Pending
        });
    }
    let (_, own_run, own) = waiting[0];
    assert_eq!(
        submission_status(client, bot.id, own),
        BotOperationStatus::Rejected
    );
    assert_eq!(port.submits_of(own_run), 0);
    // One fence for the Trash, at most one more per sibling, and at most
    // one renewal per barrier that landed before another.
    let moved = read_bot(client, bot.id).revision - revision;
    assert!(moved <= 1 + 2 + 2, "the revision moved {moved} times");
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(read_bot(client, bot.id).revision - revision, moved);
    let _ = worker.stop();
}
