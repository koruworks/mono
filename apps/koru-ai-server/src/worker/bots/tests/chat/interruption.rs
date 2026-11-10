// SPDX-License-Identifier: MPL-2.0
//! Automatic AI failure and AI-only restart send the private Interrupt, never
//! the person's stronger Stop, even while a person holds the native session.
use super::*;

fn spawn(
    native: Arc<Native>,
    repository: Repository,
    authority: Arc<Authority>,
) -> (WorkerHandle, Receiver<Repository>) {
    let (return_tx, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-in-memory-root"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(native);
    config.test_repository = Some(repository);
    config.test_repository_return = Some(return_tx);
    (AiWorker::new(config).spawn().unwrap(), returned)
}
fn started() -> (Repository, Arc<Authority>, Bot, CommandContext) {
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
    (repository, authority, bot, context)
}
fn send(worker: &WorkerHandle, context: CommandContext, bot: &Bot) -> AiResponseStream {
    let (reply, response) = sync_channel(1);
    worker
        .client()
        .send(WorkerCommand::SendTurn {
            context,
            request: request(bot),
            reply,
        })
        .unwrap();
    response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
}
fn failed(stream: &AiResponseStream) {
    loop {
        let event = stream
            .events
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .event;
        if event.is_terminal() {
            assert!(matches!(event, PublicAiEvent::Failed { .. }), "{event:?}");
            return;
        }
    }
}
/// Two pending replies prove the automatic lane keeps retrying the same
/// Interrupt while the person holds the session.
fn held(reports: &Receiver<bool>) {
    for _ in 0..2 {
        assert!(!reports.recv_timeout(Duration::from_secs(10)).unwrap());
    }
}
fn released(native: &Native, reports: &Receiver<bool>) {
    for _ in reports.try_iter() {}
    native.hold_interrupt.store(false, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !reports.recv_timeout(Duration::from_secs(10)).unwrap() {
        assert!(Instant::now() < deadline, "interruption never settled");
    }
}
struct Release(Option<SyncSender<()>>);
impl Drop for Release {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.try_send(());
        }
    }
}

#[test]
fn ai_only_restart_while_a_person_holds_the_session_interrupts_without_stop_or_second_begin() {
    let (repository, authority, bot, context) = started();
    let (begin_tx, begun) = sync_channel(4);
    let (interrupt_tx, interrupts) = sync_channel(64);
    let native = Arc::new(Native {
        hold_observation: true,
        hold_interrupt: AtomicBool::new(true),
        begin_reports: Mutex::new(Some(begin_tx)),
        interrupt_reports: Mutex::new(Some(interrupt_tx)),
        ..Native::default()
    });
    let (worker, returned) = spawn(native.clone(), repository, authority.clone());
    let stream = send(&worker, context, &bot);
    begun.recv_timeout(Duration::from_secs(10)).unwrap();
    // Only the AI thread stops; the native session and its person stay.
    worker.shutdown();
    let _ = worker.join();
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(native.state.lock().unwrap().interrupt_calls, 0);
    let (restarted, returned) = spawn(native.clone(), repository, authority);
    held(&interrupts);
    {
        let state = native.state.lock().unwrap();
        assert_eq!(state.stop_calls, 0);
        assert!(!state.stopped);
        assert_eq!(state.begin, 1);
    }
    // Handback with positive end proof lets the owner settle the interruption.
    released(&native, &interrupts);
    restarted.shutdown();
    let _ = restarted.join();
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        repository
            .attempt(stream.accepted.attempt_id)
            .unwrap()
            .unwrap()
            .status,
        AttemptStatus::Interrupted
    );
    assert!(
        !repository
            .bot_delivery_pending(stream.accepted.attempt_id)
            .unwrap()
    );
    let state = native.state.lock().unwrap();
    assert_eq!(state.stop_calls, 0);
    assert_eq!(state.begin, 1);
    assert!(state.interrupt_settled);
    assert!(!state.delivered);
}

#[test]
fn automatic_malformed_replay_while_a_person_holds_the_session_selects_interrupt() {
    let (repository, authority, bot, context) = started();
    let (interrupt_tx, interrupts) = sync_channel(64);
    let native = Arc::new(Native {
        hold_observation: true,
        replay_failure: true,
        hold_interrupt: AtomicBool::new(true),
        interrupt_reports: Mutex::new(Some(interrupt_tx)),
        ..Native::default()
    });
    let (worker, returned) = spawn(native.clone(), repository, authority);
    let stream = send(&worker, context, &bot);
    failed(&stream);
    held(&interrupts);
    {
        let state = native.state.lock().unwrap();
        assert_eq!(state.stop_calls, 0);
        assert!(!state.stopped);
        assert_eq!(state.begin, 1);
    }
    released(&native, &interrupts);
    worker.shutdown();
    let _ = worker.join();
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        repository
            .attempt(stream.accepted.attempt_id)
            .unwrap()
            .unwrap()
            .status,
        AttemptStatus::Interrupted
    );
    let state = native.state.lock().unwrap();
    assert_eq!(state.stop_calls, 0);
    assert_eq!(state.begin, 1);
    assert!(!state.delivered);
}

#[test]
fn explicit_cancel_during_a_blocked_automatic_interrupt_uses_the_reserved_stop_lane_at_once() {
    let (repository, authority, bot, context) = started();
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    let (stop_tx, stops) = sync_channel(64);
    let (interrupt_tx, interrupts) = sync_channel(64);
    let native = Arc::new(Native {
        hold_observation: true,
        replay_failure: true,
        interrupt_gate: Mutex::new(Some((entered_tx, release))),
        stop_reports: Mutex::new(Some(stop_tx)),
        interrupt_reports: Mutex::new(Some(interrupt_tx)),
        ..Native::default()
    });
    let release = Release(Some(release_tx));
    let (worker, returned) = spawn(native.clone(), repository, authority);
    let stream = send(&worker, context.clone(), &bot);
    failed(&stream);
    // The automatic Interrupt is now blocked inside the normal admin pool.
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    let (reply, response) = sync_channel(1);
    worker
        .client()
        .send(WorkerCommand::CancelAttempt {
            context,
            attempt: stream.accepted.attempt_id,
            reply,
        })
        .unwrap();
    // The attempt already ended; its durable status is reported as it is.
    assert_eq!(
        response
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .status,
        AttemptStatus::Interrupted
    );
    stops.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(interrupts.try_recv().is_err(), "Stop did not wait for it");
    drop(release);
    // The late automatic Applied reply is ignored: Stop keeps its lane.
    assert!(interrupts.recv_timeout(Duration::from_secs(10)).unwrap());
    for _ in stops.try_iter() {}
    stops.recv_timeout(Duration::from_secs(5)).unwrap();
    stops.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.shutdown();
    let _ = worker.join();
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        repository
            .attempt(stream.accepted.attempt_id)
            .unwrap()
            .unwrap()
            .status,
        AttemptStatus::Interrupted
    );
    let state = native.state.lock().unwrap();
    assert_eq!(state.interrupt_calls, 1);
    assert!(state.stop_calls >= 3);
    assert_eq!(state.begin, 1);
}
