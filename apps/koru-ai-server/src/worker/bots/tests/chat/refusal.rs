// SPDX-License-Identifier: MPL-2.0
//! A refused first input ends the task at once; an uncertain Begin backs off.
use super::*;

fn spawn(native: Arc<Native>) -> (WorkerHandle, Receiver<Repository>, CommandContext, Bot) {
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
    (
        AiWorker::new(config).spawn().unwrap(),
        returned,
        context,
        bot,
    )
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

fn terminal(stream: &AiResponseStream) -> PublicAiEvent {
    loop {
        let event = stream
            .events
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
            .event;
        if event.is_terminal() {
            return event;
        }
    }
}

#[test]
fn a_named_refusal_of_the_first_input_ends_the_task_at_once_on_the_interrupt_lane() {
    for (code, class) in [
        ("bot_input_too_large", AiErrorClass::ContextTooLarge),
        ("bot_input_pictures_refused", AiErrorClass::Provider),
        ("bot_input_model_refused", AiErrorClass::ModelUnavailable),
        ("bot_input_capability_refused", AiErrorClass::Configuration),
    ] {
        let (interrupt_tx, interrupts) = sync_channel(64);
        let native = Arc::new(Native {
            interrupt_reports: Mutex::new(Some(interrupt_tx)),
            begin_failures: Mutex::new(
                [koru_delegate_api::Failure::new(
                    code,
                    "The Bot operation was refused.",
                    false,
                )]
                .into(),
            ),
            ..Native::default()
        });
        let (worker, returned, context, bot) = spawn(native.clone());
        let started = Instant::now();
        let stream = send(&worker, context, &bot);
        match terminal(&stream) {
            PublicAiEvent::Failed { error, .. } => {
                assert_eq!(error.class, class, "{code}");
                assert!(!error.retryable);
            }
            event => panic!("{code}: {event:?}"),
        }
        // The automatic interruption reaches the owner; nothing waits for
        // the ten-minute active budget.
        assert!(interrupts.recv_timeout(Duration::from_secs(10)).unwrap());
        assert!(started.elapsed() < Duration::from_secs(10));
        worker.shutdown();
        let _ = worker.join();
        let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
        let attempt = repository
            .attempt(stream.accepted.attempt_id)
            .unwrap()
            .unwrap();
        assert_eq!(attempt.status, AttemptStatus::Failed);
        assert_eq!(attempt.error_class, Some(class));
        assert_eq!(attempt.stop_reason.as_deref(), Some(code));
        let reply = repository
            .message(stream.accepted.assistant_message_id)
            .unwrap()
            .unwrap();
        assert_eq!(reply.state, MessageState::Failed);
        assert_eq!(reply.error_class, Some(class));
        let state = native.state.lock().unwrap();
        assert_eq!(state.begin, 1, "{code}: the same Begin is never re-issued");
        assert_eq!(state.stop_calls, 0, "an automatic end never selects Stop");
        assert!(state.interrupt_calls > 0);
    }
}

#[test]
fn an_uncertain_begin_is_reissued_with_backoff_not_four_times_a_second() {
    let lost = || koru_delegate_api::Failure::new("port", "fixture lost reply", true);
    let native = Arc::new(Native {
        begin_failures: Mutex::new([lost(), lost(), lost()].into()),
        ..Native::default()
    });
    let (worker, _returned, context, bot) = spawn(native.clone());
    let stream = send(&worker, context, &bot);
    assert!(matches!(terminal(&stream), PublicAiEvent::Completed { .. }));
    worker.shutdown();
    let _ = worker.join();
    let state = native.state.lock().unwrap();
    // The same exact input each time, answered idempotently by the owner.
    assert_eq!(state.begin, 4);
    let gaps: Vec<_> = state
        .begin_at
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect();
    // 500 ms, 1 s, 2 s: doubling from the poll interval, as finishing does.
    for (gap, floor) in gaps.iter().zip([400, 900, 1_900]) {
        assert!(*gap >= Duration::from_millis(floor), "{gaps:?}");
    }
}
