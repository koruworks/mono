// SPDX-License-Identifier: MPL-2.0
//! Trash and Restore of a Bot through the full worker (P1.7 S7), with the Archive tests' fake delegate owner. Trash
//! stops work as the person's Stop, exactly as Archive does, and then only
//! hides: no page, Run record or folder moves (O41). Every test asserts that
//! no delegate Run `Trash`/`Restore` was sent and that the Store was never
//! reached ([`StoreSpy`]).
use super::*;

fn lifecycle(client: &AiWorkerClient, command: BotCommand) -> ApiResult<BotLifecycleProgress> {
    match client.bots(context(true), command)? {
        BotResponse::Lifecycle { operation } => Ok(operation),
        _ => panic!("lifecycle progress"),
    }
}

fn trash_at(
    client: &AiWorkerClient,
    bot: &Bot,
    request: BotRequestId,
    expected_revision: u64,
) -> ApiResult<BotLifecycleProgress> {
    lifecycle(
        client,
        BotCommand::Trash {
            request_id: request,
            bot_id: bot.id,
            expected_revision,
        },
    )
}

fn restore_at(
    client: &AiWorkerClient,
    bot: &Bot,
    source: BotRequestId,
    expected_revision: u64,
) -> ApiResult<BotLifecycleProgress> {
    lifecycle(
        client,
        BotCommand::Restore {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision,
            source_request_id: source,
        },
    )
}

/// The Trash filter, as a Trash view reads it.
fn trashed(client: &AiWorkerClient, bot: &Bot) -> Vec<Bot> {
    let Ok(BotResponse::List { bots }) = client.bots(
        context(true),
        BotCommand::ListLifecycle {
            workspace: bot.home_workspace,
            lifecycle: BotLifecycle::Trashed,
        },
    ) else {
        panic!("Trash listing")
    };
    bots
}

/// The current revision of a Bot in the Trash filter.
fn trashed_revision(client: &AiWorkerClient, bot: &Bot) -> u64 {
    trashed(client, bot)
        .into_iter()
        .find(|b| b.id == bot.id)
        .expect("listed in Trash")
        .revision
}

fn read_code(client: &AiWorkerClient, chat: ChatId) -> Option<String> {
    client
        .read_chat(context(true), chat)
        .err()
        .map(|error| error.code)
}

/// Another conversation of the Bot, made before the worker starts.
fn another_chat(repository: &mut Repository, authority: &Authority, bot: &Bot) -> ChatId {
    repository
        .create_bot_chat(
            bot,
            authority.profile,
            BotRequestId::mint(),
            &"f".repeat(64),
            Timestamp::now(),
        )
        .unwrap()
        .chat_id
}

/// A task started through the API in `chat`: its AI submission is prepared
/// (so AI knows which conversation its Run is), and the fake owner admits it
/// in `state` when it is submitted.
fn api_task_in(
    repository: &mut Repository,
    bot: &Bot,
    chat: ChatId,
    state: d::BotAdmissionState,
    pending: usize,
) -> ApiTask {
    let request = BotRequestId::mint();
    let current = repository.bot_any_lifecycle(bot.id).unwrap().unwrap().0;
    let intent = crate::worker::bots::admission::make_intent(
        &current,
        request,
        chat,
        current.home_workspace,
        &BotTask {
            text: "API-TASK".into(),
            documents: vec![],
        },
        None,
        &[d::RunContextItem::Document {
            workspace: current.config.role.workspace,
            document: current.config.role.document,
            version: d::DocumentVersionId::new(),
        }],
        koru_ai_core::ProfileId::new(),
    )
    .unwrap();
    repository
        .prepare_bot_submission(&intent, &"e".repeat(64), current.revision, 1)
        .unwrap();
    let mut task = api_task(&current, state, pending);
    task.view.key.request = request;
    task.view.run = intent.submission.request.run;
    if let Some(fence) = &mut task.view.fence {
        fence.run = task.view.run;
    }
    task
}

/// No delegate-side Run Trash/Restore, and no Store call, ever (O41).
fn no_owner_effects(port: &Archiving) {
    assert_eq!(port.run_lifecycle.load(Ordering::SeqCst), 0);
    assert_eq!(port.store_calls.load(Ordering::SeqCst), 0);
}

fn left_in_place() -> Vec<BotLeftInPlace> {
    vec![BotLeftInPlace::Pages, BotLeftInPlace::RunRecords]
}

#[test]
fn trash_ends_a_held_running_task_and_restore_brings_back_a_paused_bot_that_starts_nothing() {
    let Running {
        worker,
        client,
        returned,
        port,
        bot,
        attempt,
    } = running(true);
    let before: Vec<_> = client
        .read_chat(context(true), bot.main_chat)
        .unwrap()
        .messages
        .iter()
        .map(|m| m.id)
        .collect();
    let request = BotRequestId::mint();
    let captured = trash_at(&client, &bot, request, bot.revision).unwrap();
    assert_eq!(captured.kind, BotLifecycleKind::Trash);
    // The result says, as a field, that pages and Run records stay put.
    assert_eq!(captured.left_in_place, left_in_place());
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert_eq!(done.left_in_place, left_in_place());
    assert!(
        done.members
            .iter()
            .all(|m| m.stage == BotLifecycleMemberStage::Acknowledged)
    );
    // Stopped as the person's Stop, which ended the session a person held;
    // paused and effects off first, exactly as Archive.
    assert!(port.stops_of(port.conversation_run()) >= 1);
    assert!(port.native.state.lock().unwrap().stopped);
    assert!(port.interrupts.lock().unwrap().is_empty());
    assert!(
        port.barriers
            .lock()
            .unwrap()
            .iter()
            .all(|b| b.paused && b.bot == bot.id)
    );
    assert!(
        port.policies
            .lock()
            .unwrap()
            .iter()
            .all(|p| !p.effects_allowed && p.config_revision == bot.revision + 1)
    );
    // Hidden: missing to a read, out of the roster, in the Trash filter.
    assert_eq!(
        client
            .bots(context(true), BotCommand::Read { bot_id: bot.id })
            .unwrap_err()
            .code,
        "not_found"
    );
    let Ok(BotResponse::List { bots }) = client.bots(
        context(true),
        BotCommand::List {
            workspace: bot.home_workspace,
        },
    ) else {
        panic!("roster")
    };
    assert!(bots.iter().all(|b| b.id != bot.id));
    let listed = trashed(&client, &bot);
    assert_eq!(
        listed.iter().map(|b| b.id).collect::<Vec<_>>(),
        vec![bot.id]
    );
    assert_eq!(listed[0].lifecycle, BotLifecycle::Trashed);
    assert_eq!(listed[0].revision, bot.revision + 2);
    assert_eq!(read_code(&client, bot.main_chat).as_deref(), Some(TRASHED));
    assert_eq!(
        client
            .send_turn(context(true), request_for(&bot, bot.main_chat))
            .map(|_| ())
            .unwrap_err()
            .code,
        TRASHED
    );
    // Restore: paused, nothing starts, the same conversation comes back.
    let begins = port.native.state.lock().unwrap().begin;
    let stops = port.stops.lock().unwrap().len();
    let policies = port.policies.lock().unwrap().len();
    let restored = restore_at(&client, &bot, request, bot.revision + 2).unwrap();
    assert_eq!(restored.stage, BotLifecycleStage::Completed);
    assert!(restored.left_in_place.is_empty());
    let back = read_bot(&client, bot.id);
    assert_eq!(back.lifecycle, BotLifecycle::Active);
    assert!(back.paused);
    assert_eq!(back.revision, bot.revision + 3);
    assert_eq!(back.main_chat, bot.main_chat);
    let after: Vec<_> = client
        .read_chat(context(true), bot.main_chat)
        .unwrap()
        .messages
        .iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(after, before, "the same history");
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(port.native.state.lock().unwrap().begin, begins);
    assert_eq!(port.stops.lock().unwrap().len(), stops);
    assert_eq!(port.policies.lock().unwrap().len(), policies);
    no_owner_effects(&port);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(stopped_by_the_person(&repository, attempt));
}

/// A Send for `chat` of the Bot.
fn request_for(bot: &Bot, chat: ChatId) -> SendTurnRequest {
    let mut send = request(bot);
    send.chat_id = chat;
    send
}

#[test]
fn trash_waits_for_an_executing_api_task_and_queued_work_never_starts() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let port = Arc::new(Archiving::default());
    let executing = api_task(&bot, d::BotAdmissionState::Executing, 2);
    let queued = api_task(&bot, d::BotAdmissionState::Queued, 0);
    let runs = [executing.view.run, queued.view.run];
    port.api.lock().unwrap().extend([executing, queued]);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let request = BotRequestId::mint();
    trash_at(&client, &bot, request, bot.revision).unwrap();
    until("both API tasks stopped", || {
        runs.iter().all(|run| port.stops_of(*run) >= 1)
    });
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    // Not trashed until the unconfirmed Stop was confirmed.
    assert!(port.stops_of(runs[0]) >= 3);
    assert_eq!(port.stops_of(runs[1]), 1, "queued work is not re-sent");
    assert!(port.unsettled(bot.id).is_empty());
    assert_eq!(port.native.state.lock().unwrap().begin, 0);
    assert!(port.interrupts.lock().unwrap().is_empty());
    assert_eq!(trashed_revision(&client, &bot), bot.revision + 2);
    no_owner_effects(&port);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn an_archived_bot_is_trashed_and_restored_to_the_archive() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let port = Arc::new(Archiving::default());
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let archive_id = BotRequestId::mint();
    archive(&client, &bot, archive_id);
    assert_eq!(
        settled(&client, bot.id, archive_id).stage,
        BotLifecycleStage::Completed
    );
    let archived = read_bot(&client, bot.id);
    let (barriers, reads) = (
        port.barriers.lock().unwrap().len(),
        port.reads.load(Ordering::SeqCst),
    );
    let trash = BotRequestId::mint();
    trash_at(&client, &bot, trash, archived.revision).unwrap();
    let done = settled(&client, bot.id, trash);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    // Already paused and settled: no second pause; the owner is still asked.
    assert_eq!(port.barriers.lock().unwrap().len(), barriers);
    assert!(port.reads.load(Ordering::SeqCst) > reads);
    assert_eq!(trashed_revision(&client, &bot), archived.revision + 1);
    restore_at(&client, &bot, trash, archived.revision + 1).unwrap();
    // Back in the Archive, still paused; a change answers the archived code.
    let back = read_bot(&client, bot.id);
    assert_eq!(back.lifecycle, BotLifecycle::Archived);
    assert!(back.paused);
    assert_eq!(
        client
            .bots(
                context(true),
                BotCommand::CreateChat {
                    request_id: BotRequestId::mint(),
                    bot_id: bot.id,
                },
            )
            .unwrap_err()
            .code,
        ARCHIVED
    );
    no_owner_effects(&port);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// A whole-Bot Trash captured by an earlier worker, as the driver captures it.
fn captured_trash(repository: &mut Repository, bot: &Bot) -> BotRequestId {
    let request = BotRequestId::mint();
    repository
        .capture_bot_lifecycle(&koru_ai_sqlite::LifecycleCapture {
            operation: request,
            owner: BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            },
            bot: bot.id,
            kind: BotLifecycleKind::Trash,
            chat: None,
            fingerprint: "c".repeat(64),
            expected_revision: bot.revision,
            source: None,
            members: vec![
                koru_ai_sqlite::LifecycleMemberDraft {
                    subject: BotLifecycleSubject::Bot,
                    reason: BotLifecycleReason::Bot,
                    prior_state: BotLifecyclePriorState::Live,
                    intended: BotLifecycleIntended::Hide,
                    retained: None,
                },
                koru_ai_sqlite::LifecycleMemberDraft {
                    subject: BotLifecycleSubject::Chat {
                        chat_id: bot.main_chat,
                    },
                    reason: BotLifecycleReason::OwnedConversation,
                    prior_state: BotLifecyclePriorState::Live,
                    intended: BotLifecycleIntended::Hide,
                    retained: None,
                },
            ],
            now: 1,
        })
        .unwrap();
    request
}

#[test]
fn a_restart_resumes_a_captured_bot_trash_and_its_restore_follows_another_restart() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    // Captured, before any owner call, by an earlier worker (paused at capture).
    let trash = captured_trash(&mut repository, &bot);
    let port = Arc::new(Archiving::default());
    let (worker, returned) = spawn(repository, authority.clone(), port.clone());
    let client = worker.client();
    let done = settled(&client, bot.id, trash);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert!(!port.barriers.lock().unwrap().is_empty());
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    // After another restart the completed Trash is restored as usual.
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let revision = trashed_revision(&client, &bot);
    restore_at(&client, &bot, trash, revision).unwrap();
    assert_eq!(read_code(&client, bot.main_chat), None);
    assert!(read_bot(&client, bot.id).paused);
    no_owner_effects(&port);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// Starts the Trash under test: of the whole Bot, or of `chat` alone.
type Begin =
    fn(&AiWorkerClient, &Bot, ChatId, BotRequestId, u64) -> ApiResult<BotLifecycleProgress>;

fn begin_bot_trash(
    client: &AiWorkerClient,
    bot: &Bot,
    _chat: ChatId,
    request: BotRequestId,
    revision: u64,
) -> ApiResult<BotLifecycleProgress> {
    trash_at(client, bot, request, revision)
}

/// A Trash whose work has not ended keeps settling across an AI-only
/// restart, and every competing or changed command conflicts meanwhile.
fn unfinished_trash_keeps_settling(begin: Begin) {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let other = another_chat(&mut repository, &authority, &bot);
    // A task of `other` whose Stop stays unconfirmed for now.
    let task = api_task_in(
        &mut repository,
        &bot,
        other,
        d::BotAdmissionState::Executing,
        usize::MAX,
    );
    let run = task.view.run;
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(task);
    let (worker, returned) = spawn(repository, authority.clone(), port.clone());
    let client = worker.client();
    let request = BotRequestId::mint();
    begin(&client, &bot, other, request, bot.revision).unwrap();
    until("the task's Stop was sent", || port.stops_of(run) >= 1);
    assert_eq!(
        read_operation(&client, bot.id, request).stage,
        BotLifecycleStage::Settling
    );
    // One moving operation per Bot: anything else conflicts.
    let current = read_bot(&client, bot.id);
    assert_eq!(current.open_operation.unwrap().request_id, request);
    for competing in [
        BotCommand::Archive {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: current.revision,
        },
        BotCommand::Trash {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: current.revision,
        },
    ] {
        assert_eq!(
            client.bots(context(true), competing).unwrap_err().code,
            BotError::Conflict.code()
        );
    }
    // The same id with another input conflicts too.
    assert_eq!(
        begin(&client, &bot, bot.main_chat, request, bot.revision + 7)
            .unwrap_err()
            .code,
        BotError::Conflict.code()
    );
    // Only the AI worker restarts: the journal resumes it, the owner is
    // asked again, and nothing is remembered.
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    let stops = port.stops_of(run);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    until("a Stop after the restart", || port.stops_of(run) > stops);
    assert_eq!(
        read_operation(&client, bot.id, request).stage,
        BotLifecycleStage::Settling
    );
    port.api.lock().unwrap()[0].pending = 0;
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    // An exact repeat answers the finished operation.
    assert_eq!(
        begin(&client, &bot, other, request, bot.revision).unwrap(),
        done
    );
    assert_eq!(read_code(&client, other).as_deref(), Some(TRASHED));
    assert_eq!(port.interrupts_of(run), 0);
    no_owner_effects(&port);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn an_unfinished_bot_trash_keeps_settling_across_a_restart() {
    unfinished_trash_keeps_settling(begin_bot_trash);
}

#[test]
fn every_change_to_a_trashed_bot_or_its_conversation_answers_its_code() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let port = Arc::new(Archiving::default());
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let trash = BotRequestId::mint();
    trash_at(&client, &bot, trash, bot.revision).unwrap();
    assert_eq!(
        settled(&client, bot.id, trash).stage,
        BotLifecycleStage::Completed
    );
    let revision = trashed_revision(&client, &bot);
    let task = BotTask {
        text: "CURRENT-TASK".into(),
        documents: vec![],
    };
    let changes = vec![
        BotCommand::CreateChat {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
        },
        BotCommand::Update {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: revision,
            config: bot.config.clone(),
        },
        BotCommand::UpdateAccess {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: revision,
            expected_policy_revision: bot.policy_revision,
            access: BotAccess::Workspace,
            selected_pages: vec![],
        },
        BotCommand::Pause {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: revision,
        },
        BotCommand::Resume {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: revision,
        },
        BotCommand::Start {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            chat_id: bot.main_chat,
            expected_revision: revision,
            destination: bot.home_workspace,
            task: task.clone(),
        },
        BotCommand::Continue {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            chat_id: bot.main_chat,
            expected_revision: revision,
            destination: bot.home_workspace,
            task,
            previous_run: d::RunId::mint(),
        },
        BotCommand::ReconcileOperation {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
        },
        BotCommand::Archive {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: revision,
        },
        BotCommand::Trash {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: revision,
        },
    ];
    for change in changes {
        assert_eq!(
            client.bots(context(true), change.clone()).unwrap_err().code,
            TRASHED,
            "{change:?}"
        );
    }
    // Its conversation: a change and a read alike.
    assert_eq!(
        client
            .send_turn(context(true), request(&bot))
            .map(|_| ())
            .unwrap_err()
            .code,
        TRASHED
    );
    assert_eq!(read_code(&client, bot.main_chat).as_deref(), Some(TRASHED));
    // Reads of the Bot answer as missing; its operation's progress reads.
    for read in [
        BotCommand::Read { bot_id: bot.id },
        BotCommand::ReadOperation {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
        },
        BotCommand::ListBotChats {
            bot_id: bot.id,
            cursor: None,
            limit: 10,
        },
    ] {
        assert_eq!(
            client.bots(context(true), read.clone()).unwrap_err().code,
            "not_found",
            "{read:?}"
        );
    }
    assert_eq!(
        read_operation(&client, bot.id, trash).stage,
        BotLifecycleStage::Completed
    );
    // A Restore naming anything but its Trash brings nothing back.
    assert_eq!(
        restore_at(&client, &bot, BotRequestId::mint(), revision)
            .unwrap_err()
            .code,
        "not_found"
    );
    no_owner_effects(&port);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
