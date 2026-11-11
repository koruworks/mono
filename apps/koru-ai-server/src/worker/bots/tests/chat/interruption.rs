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
    ended(stream, false);
}
fn ended(stream: &AiResponseStream, cancelled: bool) {
    loop {
        let event = stream
            .events
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .event;
        if event.is_terminal() {
            if cancelled {
                assert!(
                    matches!(event, PublicAiEvent::Cancelled { .. }),
                    "{event:?}"
                );
            } else {
                assert!(matches!(event, PublicAiEvent::Failed { .. }), "{event:?}");
            }
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
    // Pending replies back off instead of polling at 4 Hz for the takeover:
    // after two stalls the next attempt waits a full second.
    let start = Instant::now();
    assert!(!interrupts.recv_timeout(Duration::from_secs(10)).unwrap());
    assert!(
        start.elapsed() >= Duration::from_millis(700),
        "{:?}",
        start.elapsed()
    );
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
    // It failed for the replay gap: no frame past the baseline was committed.
    assert!(
        repository
            .bot_replay_state(stream.accepted.attempt_id)
            .unwrap()
            .last_sequence
            .is_none_or(|n| n <= s::AGENT_EVENT_INITIAL_CURSOR)
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

#[test]
fn a_native_turn_cancellation_is_automatic_across_ai_restart_and_stop_still_reaches_the_owner() {
    // A takeover "now" ends the Bot's turn at the takeover boundary; Esc in
    // the borrowed agent ends it as a plain cancellation. Neither is Stop.
    for reason in [s::CancelReason::TakeoverBoundary, s::CancelReason::Run] {
        let (repository, authority, bot, context) = started();
        let (interrupt_tx, interrupts) = sync_channel(64);
        let (stop_tx, stops) = sync_channel(64);
        let native = Arc::new(Native {
            native_cancel: Some(reason),
            hold_interrupt: AtomicBool::new(true),
            interrupt_reports: Mutex::new(Some(interrupt_tx)),
            stop_reports: Mutex::new(Some(stop_tx)),
            ..Native::default()
        });
        let (worker, returned) = spawn(native.clone(), repository, authority.clone());
        let stream = send(&worker, context.clone(), &bot);
        ended(&stream, true);
        held(&interrupts);
        worker.shutdown();
        let _ = worker.join();
        let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
        let id = stream.accepted.attempt_id;
        assert_eq!(
            repository.attempt(id).unwrap().unwrap().status,
            AttemptStatus::Cancelled
        );
        assert_eq!(repository.bot_replay_state(id).unwrap().stop_reason, None);
        // Durable Cancelled without the recorded owner Stop restarts as Interrupt.
        let (restarted, returned) = spawn(native.clone(), repository, authority);
        held(&interrupts);
        {
            let state = native.state.lock().unwrap();
            assert_eq!(state.stop_calls, 0);
            assert!(!state.stopped);
            assert_eq!(state.begin, 1);
        }
        // The person's Stop on that attempt still reaches the owner at once,
        // and reports the status the attempt durably ended with.
        let (reply, response) = sync_channel(1);
        restarted
            .client()
            .send(WorkerCommand::CancelAttempt {
                context,
                attempt: id,
                reply,
            })
            .unwrap();
        assert_eq!(
            response
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap()
                .status,
            AttemptStatus::Cancelled
        );
        stops.recv_timeout(Duration::from_secs(5)).unwrap();
        restarted.shutdown();
        let _ = restarted.join();
        let _ = returned.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(native.state.lock().unwrap().begin, 1);
    }
}

/// The owner's authority, with its strict Role creation held inside the
/// Store until released: a creation step blocked on the Store.
struct StoreHeld {
    inner: Arc<Authority>,
    gate: Mutex<Option<(SyncSender<()>, Receiver<()>)>>,
}
impl BotAuthority for StoreHeld {
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
        self.inner.inspect(workspaces, pages)
    }
    fn create_role_page(&self, _: &BotRoleSeed) -> Result<BotRoleProof, BotError> {
        if let Some((entered, release)) = self.gate.lock().unwrap().take() {
            let _ = entered.send(());
            let _ = release.recv();
        }
        Err(BotError::Unavailable)
    }
}

/// A creation whose next step is its Role page, as a restart finds it.
fn role_pending(repository: &mut Repository, authority: &Authority, bot: &Bot) -> BotRequestId {
    let folder = if cfg!(windows) {
        "D:/isolated-creation"
    } else {
        "/isolated-creation"
    };
    let seed = vec![1, 2, 3];
    let record = BotCreationRecord {
        bot: BotId::mint(),
        main_chat: ChatId::new(),
        user_scope: authority.owner.user_scope,
        owner_device: authority.owner.device,
        home_workspace: authority.workspace,
        profile: authority.profile,
        config: BotCreationConfig {
            name: "Creating".into(),
            model: bot.config.model.clone(),
            mode: None,
            fast: None,
        },
        access: BotAccess::Default,
        selected_pages: vec![],
        role: BotCreationRole::Authored {
            store_request: Uuid::new_v4(),
            candidate: DocumentId::new(),
            title: "Role".into(),
            seed_hash: blake3::hash(&seed).to_hex().to_string(),
            seed: Some(BotSeedBytes(seed)),
            request_hash: "b".repeat(64),
        },
        folder: BotCreationFolder::Selected {
            path: folder.into(),
        },
        phase: BotCreationPhase::Intent,
        reason: None,
        folder_binding: None,
        role_page: None,
        created_at_ms: 1,
    };
    let request = BotRequestId::mint();
    let scope = record.user_scope;
    repository
        .begin_bot_creation(request, &record, &"c".repeat(64), 1)
        .unwrap();
    let mut ready = record.clone();
    ready.phase = BotCreationPhase::FolderReady;
    ready.folder_binding = Some(folder.into());
    repository
        .advance_bot_creation(
            scope,
            request,
            &record,
            &ready,
            BotReceiptStage::Prepared,
            2,
        )
        .unwrap();
    let mut pending = ready.clone();
    pending.phase = BotCreationPhase::RolePending;
    repository
        .advance_bot_creation(
            scope,
            request,
            &ready,
            &pending,
            BotReceiptStage::Prepared,
            3,
        )
        .unwrap();
    request
}

#[test]
fn an_automatic_interrupt_is_dispatched_while_a_creation_step_is_blocked_on_the_store() {
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
    worker.shutdown();
    let _ = worker.join();
    let mut repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    // A restart finds both an interrupted reply and a creation whose next
    // step waits on the Store.
    let creation = role_pending(&mut repository, &authority, &bot);
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    let held_store = Arc::new(StoreHeld {
        inner: authority.clone(),
        gate: Mutex::new(Some((entered_tx, release))),
    });
    let release = Release(Some(release_tx));
    let (return_tx, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-in-memory-root"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(held_store)
    .with_bot_delegate(native.clone());
    config.test_repository = Some(repository);
    config.test_repository_return = Some(return_tx);
    let restarted = AiWorker::new(config).spawn().unwrap();
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    // The creation step is blocked inside the Store; the automatic
    // Interrupt still runs on the shared administration lane, again and
    // again while the person's session holds it.
    for _ in interrupts.try_iter() {}
    held(&interrupts);
    assert_eq!(native.state.lock().unwrap().stop_calls, 0);
    drop(release);
    restarted.shutdown();
    let _ = restarted.join();
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    // The creation is still the owner's to retry; nothing else moved it.
    let receipt = repository
        .bot_creation(authority.owner.user_scope, creation)
        .unwrap()
        .unwrap();
    assert!(receipt.open());
    assert_eq!(receipt.record.phase, BotCreationPhase::RolePending);
    assert!(
        repository
            .attempt(stream.accepted.attempt_id)
            .unwrap()
            .is_some()
    );
}
