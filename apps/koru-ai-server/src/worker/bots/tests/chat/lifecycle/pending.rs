// SPDX-License-Identifier: MPL-2.0
//! The Bot read says what is still being delivered to its execution owner
//! (`pending_change`): while a reversed parked operation's pause stays owed,
//! every change of the Bot answers `bot_operation_pending`, and the read
//! names the pause, since when, and the owner's last answer.
use super::*;

fn list(client: &AiWorkerClient, command: BotCommand) -> Vec<Bot> {
    match client.bots(context(true), command).unwrap() {
        BotResponse::List { bots } => bots,
        _ => panic!("list"),
    }
}

#[test]
fn a_reversed_archives_owed_pause_is_named_on_the_bot_until_its_owner_takes_it() {
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
    port.unavailable.store(true, Ordering::SeqCst);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    super::trash::restore_at(&client, &bot, operation, read_bot(&client, bot.id).revision).unwrap();
    // A change of the Bot waits on it, and says so.
    let refused = client
        .bots(
            context(true),
            BotCommand::Resume {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                expected_revision: read_bot(&client, bot.id).revision,
            },
        )
        .unwrap_err();
    assert_eq!(refused.code, BotError::Settling.code());
    until("the read names the owner's last answer", || {
        read_bot(&client, bot.id)
            .pending_change
            .is_some_and(|change| change.last_answer.is_some())
    });
    let read = read_bot(&client, bot.id);
    let change = read.pending_change.clone().unwrap();
    assert_eq!(change.request_id, pause);
    assert_eq!(change.kind, BotOperationKind::Pause);
    // The pause was made with the capture, at the fixture's time.
    assert_eq!(change.since_ms, 1);
    assert_eq!(change.last_answer.as_deref(), Some("bot_unavailable"));
    // The roster carries the same.
    let roster = list(
        &client,
        BotCommand::List {
            workspace: bot.home_workspace,
        },
    );
    assert_eq!(roster[0].pending_change, Some(change));
    // The owner answers: nothing is pending any more, and the field is gone
    // from the bytes, not only empty.
    port.unavailable.store(false, Ordering::SeqCst);
    until("the pause is delivered", || {
        read_bot(&client, bot.id).pending_change.is_none()
    });
    let wire = serde_json::to_value(read_bot(&client, bot.id)).unwrap();
    assert!(wire.get("pending_change").is_none());
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// A person's Pause its owner refuses as conflicting (`bot_conflict`: it
/// holds another barrier at that epoch) can never land as it is, so the AI
/// owner renews the same change under a request of its own at the next
/// revision, and the Bot is not wedged (the third review's F3). Renewed
/// truthfully and bounded (the fourth review's F4): the person's own Pause
/// reads `rejected`, the Bot's `pending_change` names the request the AI
/// owner made, with the answer that renewed it, and an owner that keeps
/// refusing is offered the change once a backed-off recovery pass, never in
/// a loop. Once it takes it, the Bot is paused at the owner as AI says.
#[test]
fn a_pause_its_owner_refuses_as_conflicting_is_renewed_on_the_backoff_and_named_truthfully() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let port = Arc::new(Archiving::default());
    port.refuse_barriers.store(true, Ordering::SeqCst);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let request = BotRequestId::mint();
    client
        .bots(
            context(true),
            BotCommand::Pause {
                request_id: request,
                bot_id: bot.id,
                expected_revision: bot.revision,
            },
        )
        .unwrap();
    until("the person's Pause was refused", || {
        super::unadmitted::submission_status(&client, bot.id, request)
            == BotOperationStatus::Rejected
    });
    std::thread::sleep(Duration::from_secs(3));
    let now = Instant::now();
    let offered = port
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(at, name, _)| *name == "barrier" && now - *at <= Duration::from_secs(3))
        .count();
    assert!(offered <= 6, "offered {offered} times in three seconds");
    let change = read_bot(&client, bot.id).pending_change.unwrap();
    assert_ne!(change.request_id, request, "a request the AI owner made");
    assert_eq!(change.kind, BotOperationKind::Pause);
    assert_eq!(change.last_answer.as_deref(), Some("bot_conflict"));
    // The owner takes it: the Bot is paused there, and nothing is pending.
    port.refuse_barriers.store(false, Ordering::SeqCst);
    until("the renewed pause is delivered", || {
        read_bot(&client, bot.id).pending_change.is_none()
    });
    assert!(read_bot(&client, bot.id).paused);
    assert!(
        port.barrier
            .lock()
            .unwrap()
            .get(&bot.id)
            .is_some_and(|b| b.paused)
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// The Archived filter fills it the same way, for an archived Bot whose
/// pause is still owed (a fixture: the Archive is recorded completed while
/// its pause never reached the owner).
#[test]
fn the_archived_filter_names_a_change_still_being_delivered() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let operation = captured_archive(&mut repository, &bot);
    let op = repository
        .bot_lifecycle_operation(bot.user_scope, operation)
        .unwrap()
        .unwrap();
    let pause = op.pause_request.unwrap();
    repository.fixture_complete_bot_lifecycle(&op, 3).unwrap();
    repository
        .fixture_set_bot_lifecycle(bot.id, BotLifecycle::Archived)
        .unwrap();
    let port = Arc::new(Archiving::default());
    port.unavailable.store(true, Ordering::SeqCst);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let archived = list(
        &client,
        BotCommand::ListLifecycle {
            workspace: bot.home_workspace,
            lifecycle: BotLifecycle::Archived,
        },
    );
    assert_eq!(archived.len(), 1);
    let change = archived[0].pending_change.clone().expect("pending");
    assert_eq!(
        (change.request_id, change.kind),
        (pause, BotOperationKind::Pause)
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
