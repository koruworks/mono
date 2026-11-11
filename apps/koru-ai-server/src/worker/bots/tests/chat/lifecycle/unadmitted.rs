// SPDX-License-Identifier: MPL-2.0
//! Work its owner had not admitted when a lifecycle operation was captured
//! is refused, never admitted (P1.7): a Send, a Retry or an API `start`
//! still waiting to reach the delegate. The fake owner refuses a Submit
//! exactly as the delegate does (`bot_stale`, `bot_fenced`), and records
//! every Submit that reaches it; none of these tests may see one for the
//! captured subject.
use super::trash::{
    another_chat, api_task_in, held_effect, lifecycle, no_owner_effects, request_for, restore_at,
    restore_chat_at, trash_at, trash_chat_at,
};
use super::*;

/// The three operations that settle work before they complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Archive,
    Trash,
    TrashChat,
}
const OPERATIONS: [Operation; 3] = [Operation::Archive, Operation::Trash, Operation::TrashChat];

fn begin(
    client: &AiWorkerClient,
    bot: &Bot,
    chat: ChatId,
    operation: Operation,
    request: BotRequestId,
) {
    match operation {
        Operation::Archive => archive_at(client, bot, request, bot.revision),
        Operation::Trash => trash_at(client, bot, request, bot.revision),
        Operation::TrashChat => trash_chat_at(client, bot, chat, request, bot.revision),
    }
    .unwrap();
}

/// Reverse a completed operation, so what it hid can be read again.
fn reverse(
    client: &AiWorkerClient,
    bot: &Bot,
    chat: ChatId,
    operation: Operation,
    source: BotRequestId,
) {
    let current = match operation {
        Operation::Trash => super::trash::trashed_revision(client, bot),
        _ => match client.bots(
            context(true),
            BotCommand::ListLifecycle {
                workspace: bot.home_workspace,
                lifecycle: BotLifecycle::Archived,
            },
        ) {
            Ok(BotResponse::List { bots }) if bots.iter().any(|b| b.id == bot.id) => {
                bots.into_iter().find(|b| b.id == bot.id).unwrap().revision
            }
            _ => read_bot(client, bot.id).revision,
        },
    };
    match operation {
        Operation::TrashChat => restore_chat_at(client, bot, chat, source, current),
        _ => restore_at(client, bot, source, current),
    }
    .unwrap();
}

/// The status of a Start/Continue receipt, read as its owner reads it.
fn submission_status(
    client: &AiWorkerClient,
    bot: BotId,
    request: BotRequestId,
) -> BotOperationStatus {
    match client
        .bots(
            context(true),
            BotCommand::ReadOperation {
                request_id: request,
                bot_id: bot,
            },
        )
        .unwrap()
    {
        BotResponse::Operation { operation } => operation.status,
        _ => panic!("operation"),
    }
}

#[test]
fn an_api_start_prepared_at_capture_is_refused_never_admitted_and_each_operation_completes() {
    for operation in OPERATIONS {
        let (mut repository, authority, create) = fixture();
        let bot = created(&mut repository, authority.clone(), create);
        // Prepared in AI; its submission never reached the owner.
        let mut start = api_task_in(
            &mut repository,
            &bot,
            bot.main_chat,
            d::BotAdmissionState::Queued,
            0,
        );
        start.admitted = false;
        let (run, start_request) = (start.view.run, start.view.key.request);
        // Captured while it waits, before any worker could reconcile it.
        let request = match operation {
            Operation::Archive => captured_archive(&mut repository, &bot),
            Operation::Trash => super::trash::captured_trash(&mut repository, &bot),
            Operation::TrashChat => {
                super::trash::captured_chat_trash(&mut repository, &bot, bot.main_chat)
            }
        };
        let port = Arc::new(Archiving::default());
        port.api.lock().unwrap().push(start);
        let (worker, _returned) = spawn(repository, authority, port.clone());
        let client = worker.client();
        let done = settled(&client, bot.id, request);
        assert_eq!(
            done.stage,
            BotLifecycleStage::Completed,
            "{operation:?} {done:?}"
        );
        // Refused by AI, never offered to the owner.
        assert_eq!(port.submits_of(run), 0, "{operation:?}");
        assert!(!port.api.lock().unwrap()[0].admitted, "{operation:?}");
        assert_eq!(port.stops_of(run), 0, "{operation:?}");
        reverse(&client, &bot, bot.main_chat, operation, request);
        assert_eq!(
            submission_status(&client, bot.id, start_request),
            BotOperationStatus::Rejected,
            "{operation:?}"
        );
        // Nothing of it starts after the restore either.
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(port.submits_of(run), 0, "{operation:?}");
        no_owner_effects(&port);
        worker.shutdown();
        assert_eq!(worker.join(), WorkerExit::Clean);
    }
}

/// A Store context reader that holds the first document read until it is
/// released. A Bot's context job never reads the Store (its documents are
/// the Run's pins), so the gate holds the one context thread on an ordinary
/// chat's read instead, and a Bot Send or Retry sent next waits behind it,
/// inside its context window: its receipt, its Pending attempt and its Run
/// binding are committed, and no live conversation task exists for it yet.
#[derive(Default)]
struct HeldContext {
    gate: Mutex<Option<(SyncSender<()>, Receiver<()>)>>,
}
impl StoreContextPort for HeldContext {
    fn resolve_document(
        &self,
        document: DocumentId,
        _reader: Option<koru_ai_core::ProfileId>,
    ) -> Result<DocumentContext, AiError> {
        if let Some((entered, release)) = self.gate.lock().unwrap().take() {
            let _ = entered.send(());
            let _ = release.recv_timeout(Duration::from_secs(60));
        }
        Ok(DocumentContext {
            document_id: document,
            display_title: "Context".into(),
            source_revision: StoreRevision(vec![1]),
            markdown: "context".into(),
        })
    }
    fn create_document(
        &self,
        _idempotency_key: Uuid,
        _destination: &DocumentDestination,
        _markdown: &str,
    ) -> Result<DocumentId, AiError> {
        unreachable!("lifecycle tests create no document")
    }
}

/// Hold the context thread: an ordinary chat's Send naming a document, whose
/// read waits until the returned sender releases it.
fn hold_context_thread(client: &AiWorkerClient, reader: &HeldContext) -> SyncSender<()> {
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    *reader.gate.lock().unwrap() = Some((entered_tx, release));
    let chat_id = ChatId::new();
    client
        .create_chat(
            context(true),
            CreateChatRequest {
                chat_id,
                title: "Ordinary".into(),
                profile_id: None,
                model: koru_ai_core::ModelSelection {
                    provider: AiProviderId::parse("mock").unwrap(),
                    requested_model: "deterministic-v1".into(),
                    default_reasoning: ReasoningChoice::Default,
                },
            },
        )
        .unwrap();
    client
        .send_turn(
            context(true),
            SendTurnRequest {
                destination_workspace: None,
                operation_id: Uuid::new_v4(),
                chat_id,
                surface: AiSurface::Standalone,
                markdown: "hold the context thread".into(),
                explicit_refs: vec![ContextRef::Document {
                    document_id: DocumentId::new(),
                }],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
                attachment_ids: Vec::new(),
                answers: None,
                skills: Vec::new(),
            },
        )
        .unwrap();
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    release_tx
}

fn send_in(
    client: &AiWorkerClient,
    context: CommandContext,
    bot: &Bot,
    chat: ChatId,
) -> SendTurnAccepted {
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::SendTurn {
            context,
            request: request_for(bot, chat),
            reply,
        })
        .unwrap();
    response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .accepted
}

/// The Bot task is where its context job would hand it on: its receipt is
/// prepared and its attempt still Pending behind the held thread.
fn wait_context_window(client: &AiWorkerClient, attempt: InferenceAttemptId) {
    until("the attempt is pending", || {
        attempt_status(client, attempt) == AttemptStatus::Pending
    });
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(attempt_status(client, attempt), AttemptStatus::Pending);
}

fn attempt_status(client: &AiWorkerClient, attempt: InferenceAttemptId) -> AttemptStatus {
    client
        .read_attempt(context(true), attempt)
        .unwrap()
        .attempt
        .status
}

#[test]
fn a_send_in_its_context_window_at_capture_is_stopped_never_admitted_and_writes_nothing_after() {
    for operation in OPERATIONS {
        let (mut repository, authority, create) = fixture();
        let bot = created(&mut repository, authority.clone(), create);
        let chat = repository
            .chat_with_access(
                bot.main_chat,
                &ChatReadAccess::authorized(vec![bot.main_chat]),
            )
            .unwrap()
            .unwrap();
        let context_reader = Arc::new(HeldContext::default());
        let port = Arc::new(Archiving::default());
        let (worker, returned) = spawn_with(
            repository,
            authority.clone(),
            port.clone(),
            Some(context_reader.clone()),
        );
        let client = worker.client();
        let release = hold_context_thread(&client, &context_reader);
        let accepted = send_in(
            &client,
            proof(authority.clone(), &bot, chat),
            &bot,
            bot.main_chat,
        );
        // Its context job waits: receipt and attempt are committed.
        wait_context_window(&client, accepted.attempt_id);
        let request = BotRequestId::mint();
        begin(&client, &bot, bot.main_chat, operation, request);
        let done = settled(&client, bot.id, request);
        assert_eq!(
            done.stage,
            BotLifecycleStage::Completed,
            "{operation:?} {done:?}"
        );
        // Stopped as the person's Stop, before anything reached the owner.
        assert_eq!(
            attempt_status(&client, accepted.attempt_id),
            AttemptStatus::Cancelled,
            "{operation:?}"
        );
        assert!(port.submits.lock().unwrap().is_empty(), "{operation:?}");
        // The context job finishes after completion: it writes nothing and
        // starts nothing.
        release.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        assert!(port.submits.lock().unwrap().is_empty(), "{operation:?}");
        assert_eq!(port.native.state.lock().unwrap().begin, 0, "{operation:?}");
        assert_eq!(
            attempt_status(&client, accepted.attempt_id),
            AttemptStatus::Cancelled,
            "{operation:?}"
        );
        // After the restore the reply reads stopped by the person.
        reverse(&client, &bot, bot.main_chat, operation, request);
        let reply = client
            .read_chat(context(true), bot.main_chat)
            .unwrap()
            .messages
            .into_iter()
            .find(|m| m.id == accepted.assistant_message_id)
            .unwrap();
        assert!(reply.state.is_terminal(), "{operation:?}");
        no_owner_effects(&port);
        worker.shutdown();
        assert_eq!(worker.join(), WorkerExit::Clean);
        let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            stopped_by_the_person(&repository, accepted.attempt_id),
            "{operation:?}"
        );
    }
}

#[test]
fn a_retry_in_its_context_window_when_its_conversation_is_trashed_writes_nothing_after() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let chat = repository
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    let context_reader = Arc::new(HeldContext::default());
    // Its first task fails natively, so its reply can be retried.
    let port = Arc::new(Archiving {
        native: Native {
            fail_first: true,
            ..Native::default()
        },
        ..Archiving::default()
    });
    let (worker, returned) = spawn_with(
        repository,
        authority.clone(),
        port.clone(),
        Some(context_reader.clone()),
    );
    let client = worker.client();
    let context = proof(authority.clone(), &bot, chat);
    // A first task fails.
    let first = send_in(&client, context.clone(), &bot, bot.main_chat);
    until("the first task failed", || {
        attempt_status(&client, first.attempt_id) == AttemptStatus::Failed
    });
    let submitted = port.submits.lock().unwrap().len();
    // Its Retry waits in its context window when the conversation is trashed.
    let release = hold_context_thread(&client, &context_reader);
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::RetryTurn {
            context,
            chat: bot.main_chat,
            request: RetryTurnRequest {
                destination_workspace: Some(bot.home_workspace),
                operation_id: Uuid::new_v4(),
                message_id: first.assistant_message_id,
                surface: AiSurface::Standalone,
                explicit_refs: vec![],
                reasoning_override: None,
                web_search: false,
                replace_pinned_documents: false,
            },
            reply,
        })
        .unwrap();
    let retry = response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .accepted;
    wait_context_window(&client, retry.attempt_id);
    let request = BotRequestId::mint();
    trash_chat_at(
        &client,
        &bot,
        bot.main_chat,
        request,
        read_bot(&client, bot.id).revision,
    )
    .unwrap();
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert_eq!(
        attempt_status(&client, retry.attempt_id),
        AttemptStatus::Cancelled
    );
    release.send(()).unwrap();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        port.submits.lock().unwrap().len(),
        submitted,
        "the Retry never reached the owner"
    );
    assert_eq!(
        attempt_status(&client, retry.attempt_id),
        AttemptStatus::Cancelled
    );
    restore_chat_at(
        &client,
        &bot,
        bot.main_chat,
        request,
        read_bot(&client, bot.id).revision,
    )
    .unwrap();
    no_owner_effects(&port);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(stopped_by_the_person(&repository, retry.attempt_id));
}

#[test]
fn a_second_operation_on_the_bot_answers_pending_naming_the_open_one() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let other = another_chat(&mut repository, &authority, &bot);
    let third = another_chat(&mut repository, &authority, &bot);
    // A task of `other` whose Stop stays unconfirmed keeps its Trash open.
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
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    // A conversation Trash completed earlier, to try restoring meanwhile.
    let earlier = BotRequestId::mint();
    trash_chat_at(&client, &bot, third, earlier, bot.revision).unwrap();
    assert_eq!(
        settled(&client, bot.id, earlier).stage,
        BotLifecycleStage::Completed
    );
    let request = BotRequestId::mint();
    trash_chat_at(&client, &bot, other, request, bot.revision).unwrap();
    until("the task's Stop was sent", || port.stops_of(run) >= 1);
    let current = read_bot(&client, bot.id);
    let open = current.open_operation.clone().unwrap();
    assert_eq!(open.request_id, request);
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
        BotCommand::TrashChat {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            chat_id: bot.main_chat,
            expected_revision: current.revision,
        },
        BotCommand::RestoreChat {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            chat_id: third,
            expected_revision: current.revision,
            source_request_id: earlier,
        },
    ] {
        let error = lifecycle(&client, competing.clone()).unwrap_err();
        assert_eq!(error.code, PENDING, "{competing:?}");
        assert!(!error.retryable, "{competing:?}");
    }
    // The Bot's read still names the one open operation.
    assert_eq!(read_bot(&client, bot.id).open_operation, Some(open));
    port.api.lock().unwrap()[0].pending = 0;
    assert_eq!(
        settled(&client, bot.id, request).stage,
        BotLifecycleStage::Completed
    );
    no_owner_effects(&port);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn a_conversation_trash_with_only_admitted_work_moves_no_revision_and_a_sibling_send_prepared_before_it_proceeds()
 {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let sibling = another_chat(&mut repository, &authority, &bot);
    let snapshot = |repository: &Repository, chat: ChatId| {
        repository
            .chat_with_access(chat, &ChatReadAccess::authorized(vec![chat]))
            .unwrap()
            .unwrap()
    };
    let main = proof(
        authority.clone(),
        &bot,
        snapshot(&repository, bot.main_chat),
    );
    // The sibling's Send, prepared at this revision before the Trash.
    let mut prepared = proof(authority.clone(), &bot, snapshot(&repository, sibling));
    Arc::get_mut(prepared.prepared.as_mut().unwrap())
        .unwrap()
        .chats = vec![(bot.id, sibling, bot.revision, bot.policy_revision)];
    let (begin_tx, begins) = sync_channel(4);
    let native = Native {
        hold_observation: true,
        ..Native::default()
    };
    *native.begin_reports.lock().unwrap() = Some(begin_tx);
    let port = Arc::new(Archiving {
        native,
        ..Archiving::default()
    });
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::SendTurn {
            context: main,
            request: request(&bot),
            reply,
        })
        .unwrap();
    response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    begins.recv_timeout(Duration::from_secs(10)).unwrap();
    // The main conversation's task is admitted and running; its Trash
    // stops it as the person's Stop and moves nothing else.
    let request = BotRequestId::mint();
    trash_chat_at(&client, &bot, bot.main_chat, request, bot.revision).unwrap();
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert_eq!(read_bot(&client, bot.id).revision, bot.revision);
    assert!(port.barriers.lock().unwrap().is_empty(), "no refusal fence");
    // The sibling's Send prepared before it still passes its revision fence.
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::SendTurn {
            context: prepared,
            request: request_for(&bot, sibling),
            reply,
        })
        .unwrap();
    let accepted = response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .accepted;
    begins.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(!attempt_status(&client, accepted.attempt_id).is_terminal());
    no_owner_effects(&port);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn more_than_256_held_effects_of_a_sibling_never_hold_a_conversation_trash_and_its_own_is_found_past_them()
 {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let other = another_chat(&mut repository, &authority, &bot);
    // A settled task of the main conversation, admitted before the Trash,
    // whose Run still holds 300 native effects.
    let sibling = admitted_api_task_in(&mut repository, &bot, bot.main_chat);
    // A settled task of `other` holding one effect, ordered after them all.
    let own = admitted_api_task_in(&mut repository, &bot, other);
    let (sibling_run, own_run) = (sibling.view.run, own.view.run);
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().extend([sibling, own]);
    {
        let mut effects = port.effects.lock().unwrap();
        for at in 0..300 {
            let mut effect = held_effect(&bot, sibling_run);
            effect.cursor.created_at_ms = at;
            effects.push(effect);
        }
        let mut effect = held_effect(&bot, own_run);
        effect.cursor.created_at_ms = 1_000;
        effects.push(effect);
    }
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let request = BotRequestId::mint();
    trash_chat_at(&client, &bot, other, request, bot.revision).unwrap();
    // Its own effect, past five pages of the sibling's, keeps it settling.
    until("every page was read", || {
        port.effect_pages.load(Ordering::SeqCst) >= 5
    });
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        read_operation(&client, bot.id, request).stage,
        BotLifecycleStage::Settling
    );
    port.effects
        .lock()
        .unwrap()
        .retain(|e| e.key.run != own_run);
    // The sibling's 300 do not hold it.
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert_eq!(port.effects.lock().unwrap().len(), 300);
    no_owner_effects(&port);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// A settled API task of `chat` whose admission AI already recorded, so
/// further tasks may be prepared beside it (the first freezes the folder).
fn admitted_api_task_in(repository: &mut Repository, bot: &Bot, chat: ChatId) -> ApiTask {
    let mut task = api_task_in(repository, bot, chat, d::BotAdmissionState::Settled, 0);
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

#[test]
fn a_cut_unsettled_page_keeps_a_conversation_trash_settling() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let other = another_chat(&mut repository, &authority, &bot);
    let port = Arc::new(Archiving::default());
    // More queued work than one read returns, none of it placeable.
    port.api
        .lock()
        .unwrap()
        .extend((0..65).map(|_| api_task(&bot, d::BotAdmissionState::Queued, 0)));
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let request = BotRequestId::mint();
    trash_chat_at(&client, &bot, other, request, bot.revision).unwrap();
    until("two reads", || port.reads.load(Ordering::SeqCst) >= 2);
    assert_eq!(
        read_operation(&client, bot.id, request).stage,
        BotLifecycleStage::Settling,
        "a cut page might hide the conversation's work"
    );
    port.api.lock().unwrap().truncate(64);
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    no_owner_effects(&port);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn a_parked_revision_change_made_only_by_its_own_refusal_resumes_to_completion() {
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
    let start_request = start.view.key.request;
    // Captured by an earlier worker, which refused the start (its refusal
    // fence moved the revision past the barrier) and parked on it.
    let request = BotRequestId::mint();
    repository
        .capture_bot_lifecycle(&koru_ai_sqlite::LifecycleCapture {
            operation: request,
            owner: BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            },
            bot: bot.id,
            kind: BotLifecycleKind::Archive,
            chat: None,
            fingerprint: "c".repeat(64),
            expected_revision: bot.revision,
            source: None,
            members: vec![],
            now: 1,
        })
        .unwrap();
    let operation = repository
        .bot_lifecycle_operation(bot.user_scope, request)
        .unwrap()
        .unwrap();
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let pause = operation.pause_request.unwrap();
    let Some(BotAdmissionReceipt::Pause(receipt)) = repository
        .bot_admission_receipt(owner, bot.id, pause)
        .unwrap()
    else {
        panic!("pause receipt")
    };
    repository
        .acknowledge_bot_pause(owner, bot.id, pause, &receipt.intent.barrier, 2)
        .unwrap();
    repository
        .prepare_bot_refusal_fence(
            owner,
            bot.id,
            start_request,
            BotAdmissionRefusal::Rejected,
            false,
            3,
        )
        .unwrap();
    assert!(
        repository
            .park_bot_lifecycle(
                bot.user_scope,
                request,
                BotLifecycleRefusal::RevisionChanged,
                4
            )
            .unwrap()
    );
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(start);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let parked = read_operation(&client, bot.id, request);
    assert_eq!(parked.refusal, Some(BotLifecycleRefusal::RevisionChanged));
    // Resumed by its id: nothing but its own settlement moved the revision.
    archive_at(&client, &bot, request, read_bot(&client, bot.id).revision).unwrap();
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert_eq!(port.submits.lock().unwrap().len(), 0);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
