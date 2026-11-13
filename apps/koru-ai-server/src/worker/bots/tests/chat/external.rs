// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn external_bot_task_uses_shared_turn_and_survives_subscriber_drop() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let chat = repository
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    // The standard prepared proof keeps this dispatch test independent of
    // an installed agent service. The worker still owns admission/execution.
    let prepared = proof(authority.clone(), &bot, chat);
    let (entered_tx, entered) = sync_channel(1);
    let (release, released) = sync_channel(1);
    let native = Arc::new(Native {
        submit_gate: Mutex::new(Some((entered_tx, released))),
        ..Native::default()
    });
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-external-bot-task"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(native.clone());
    config.test_repository = Some(repository);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    let send = |request: SendTurnRequest| {
        let (reply, response) = sync_channel(1);
        client
            .send(WorkerCommand::SendTurn {
                context: prepared.clone(),
                request,
                reply,
            })
            .unwrap();
        response.recv_timeout(Duration::from_secs(5)).unwrap()
    };
    let request = request(&bot);
    let stream = send(request.clone()).unwrap();
    let accepted = stream.accepted.clone();
    drop(stream);
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let read = || BotCommand::ReadTaskAnswer {
        bot_id: bot.id,
        chat_id: bot.main_chat,
        attempt_id: accepted.attempt_id,
        operation_id: request.operation_id,
        offset: 0,
    };
    let BotResponse::TaskAnswer { answer } = client.bots(context(true), read()).unwrap() else {
        panic!("answer")
    };
    assert!(!answer.status.is_terminal());
    assert_eq!(answer.markdown, None);
    assert_eq!(native.state.lock().unwrap().begin, 0);
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let BotResponse::TaskAnswer { answer } = client.bots(context(true), read()).unwrap() else {
            panic!("answer")
        };
        if answer.status.is_terminal() {
            assert_eq!(answer.status, AttemptStatus::Complete);
            assert_eq!(answer.markdown.as_deref(), Some("NATIVE-REPLY"));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "task did not finish after detaching subscriber"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let replay = send(request.clone()).unwrap();
    assert!(replay.accepted.replayed);
    assert_eq!(replay.accepted.attempt_id, accepted.attempt_id);
    assert_eq!(native.state.lock().unwrap().begin, 1);
    let mut changed = request;
    changed.markdown.push_str(" changed");
    assert_eq!(send(changed).err().unwrap().code, "request_conflict");
    let transcript = client.read_chat(context(true), bot.main_chat).unwrap();
    assert_eq!(transcript.messages.len(), 2);
    assert_eq!(transcript.messages[1].markdown, "NATIVE-REPLY");
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
