// SPDX-License-Identifier: MPL-2.0
//! A restart's walk of the Bot attempts in history, beside a lifecycle
//! operation (the fourth lifecycle review's F3). The walk rebuilds every
//! ended Bot conversation task that may still owe its owner something, at
//! most eight at once, and settling defers while it pages. A walk that cannot
//! finish (its owner silent) must not keep an operation from parking, so it
//! can be reversed; and the walk must not grow with history: a task its
//! owner refused for good, behind its fence, owes nothing and is not asked.
use super::trash::{another_chat, restore_chat_at};
use super::unadmitted::admitted_api_task_in;
use super::*;
use koru_ai_core::bots::replay::BotChatPreparation;

/// How a task of history ended.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// Its owner admitted it, and the person stopped it: the owner is asked
    /// again at a restart whether it settled.
    StoppedAfterAdmission,
    /// Its owner refused it for good behind its fence, and it ended
    /// interrupted: nothing of it ever ran.
    RefusedForGood,
}

/// One ended Bot conversation task, in a conversation of its own, written
/// through the repository's own paths. A task the owner admitted is also
/// one the fake owner holds as settled, so it answers it as the real owner
/// would.
fn ended_task(
    repository: &mut Repository,
    authority: &Authority,
    bot: &Bot,
    port: &Archiving,
    ended: Ended,
) -> InferenceAttemptId {
    let current = repository.bot_any_lifecycle(bot.id).unwrap().unwrap().0;
    let owner = BotOwner {
        user_scope: current.user_scope,
        device: current.owner_device,
    };
    let chat = another_chat(repository, authority, &current);
    let mut turn = crate::worker::bots::tests::turn(chat, Uuid::new_v4(), 1);
    turn.attempt.provider = current.config.model.provider.clone();
    turn.attempt.requested_model = current.config.model.requested_model.clone();
    turn.assistant_message.provider = Some(turn.attempt.provider.clone());
    turn.assistant_message.requested_model = Some(turn.attempt.requested_model.clone());
    let profile = repository
        .chat_with_access(chat, &ChatReadAccess::authorized(vec![chat]))
        .unwrap()
        .unwrap()
        .profile_id
        .unwrap();
    let request = BotRequestId::mint();
    let mut intent = crate::worker::bots::admission::make_intent(
        &current,
        request,
        chat,
        current.home_workspace,
        &BotTask {
            text: turn.user_message.markdown.clone(),
            documents: Vec::new(),
        },
        None,
        &[d::RunContextItem::Document {
            workspace: current.config.role.workspace,
            document: current.config.role.document,
            version: d::DocumentVersionId::new(),
        }],
        profile,
    )
    .unwrap();
    intent.submission.provenance.ai_attempt_id = Some(turn.attempt.id.as_uuid());
    intent.submission.provenance.assistant_message_id = Some(turn.assistant_message.id.as_uuid());
    repository
        .prepare_bot_chat(
            &BotChatPreparation::Turn(turn.clone()),
            &intent,
            &"a".repeat(64),
            current.revision,
            turn.attempt.queued_at,
        )
        .unwrap();
    let attempt = turn.attempt.id;
    match ended {
        Ended::StoppedAfterAdmission => {
            let mut task = api_task(&current, d::BotAdmissionState::Settled, 0);
            task.view.key.request = request;
            task.view.run = intent.submission.request.run;
            repository
                .acknowledge_bot_submission(owner, current.id, request, &task.view, 2)
                .unwrap();
            repository
                .cancel_bot_chat(attempt, Timestamp::now())
                .unwrap();
            port.api.lock().unwrap().push(task);
        }
        Ended::RefusedForGood => {
            let fenced = repository
                .prepare_bot_refusal_fence(
                    owner,
                    current.id,
                    request,
                    BotAdmissionRefusal::Rejected,
                    false,
                    2,
                )
                .unwrap();
            let Some(BotExecutionOutcome::RefusalFence { round, barrier, .. }) = fenced.outcome
            else {
                panic!("a refusal fence")
            };
            repository
                .acknowledge_bot_refusal_fence(owner, current.id, request, round, &barrier, 3)
                .unwrap();
            repository
                .reject_bot_submission_after_fence(owner, current.id, request, round, &barrier, 4)
                .unwrap();
            repository
                .interrupt_bot_preparation(attempt, Timestamp::now())
                .unwrap();
            // The owner holds the fence's barrier, as it would.
            port.barrier.lock().unwrap().insert(current.id, barrier);
        }
    }
    attempt
}

/// A conversation's Trash captured by an earlier worker, not yet settled.
fn captured_chat_trash(repository: &mut Repository, bot: &Bot, chat: ChatId) -> BotRequestId {
    let current = repository.bot_any_lifecycle(bot.id).unwrap().unwrap().0;
    let request = BotRequestId::mint();
    repository
        .capture_bot_lifecycle(&koru_ai_sqlite::LifecycleCapture {
            operation: request,
            owner: BotOwner {
                user_scope: current.user_scope,
                device: current.owner_device,
            },
            bot: current.id,
            kind: BotLifecycleKind::TrashChat,
            chat: Some(chat),
            fingerprint: "d".repeat(64),
            expected_revision: current.revision,
            source: None,
            members: vec![koru_ai_sqlite::LifecycleMemberDraft {
                subject: BotLifecycleSubject::Chat { chat_id: chat },
                reason: BotLifecycleReason::OwnedConversation,
                prior_state: BotLifecyclePriorState::Live,
                intended: BotLifecycleIntended::Hide,
                retained: None,
            }],
            now: 1,
        })
        .unwrap();
    request
}

/// A Bot whose first task already ran, so several of its tasks may wait at
/// once, with `history` ended tasks of each kind and a conversation's Trash
/// captured beside them.
struct History {
    repository: Repository,
    authority: Arc<Authority>,
    port: Arc<Archiving>,
    bot: Bot,
    chat: ChatId,
    trash: BotRequestId,
}

fn history(stopped: usize, refused: usize) -> History {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let port = Arc::new(Archiving::default());
    let ran = admitted_api_task_in(&mut repository, &bot, bot.main_chat);
    port.api.lock().unwrap().push(ran);
    for _ in 0..refused {
        ended_task(
            &mut repository,
            &authority,
            &bot,
            &port,
            Ended::RefusedForGood,
        );
    }
    for _ in 0..stopped {
        ended_task(
            &mut repository,
            &authority,
            &bot,
            &port,
            Ended::StoppedAfterAdmission,
        );
    }
    // The refusal fences moved the Bot's revision.
    let bot = repository.bot_any_lifecycle(bot.id).unwrap().unwrap().0;
    let chat = another_chat(&mut repository, &authority, &bot);
    let trash = captured_chat_trash(&mut repository, &bot, chat);
    let bot = repository.bot_any_lifecycle(bot.id).unwrap().unwrap().0;
    History {
        repository,
        authority,
        port,
        bot,
        chat,
        trash,
    }
}

/// [`spawn`], with the lifecycle driver's rounds shortened from 250 ms to
/// 5 ms, so its twelve rounds take a second instead of minutes. Nothing else
/// about the worker changes.
fn spawn_fast(
    repository: Repository,
    authority: Arc<Authority>,
    port: Arc<Archiving>,
) -> (WorkerHandle, Receiver<Repository>) {
    let (return_tx, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-lifecycle"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(port);
    config.test_repository = Some(repository);
    config.test_repository_return = Some(return_tx);
    config.test_lifecycle_unit = Some(Duration::from_millis(5));
    (AiWorker::new(config).spawn().unwrap(), returned)
}

fn reconciles(port: &Archiving) -> usize {
    port.calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, what, _)| *what == "reconcile")
        .count()
}

/// The owner is silent after a restart, and nine tasks of history each need
/// it asked again (more than the walk may hold at once), so the walk cannot
/// finish. The conversation's Trash beside it still parks, after its rounds,
/// and its Restore is accepted with no owner call.
#[test]
fn with_its_owner_silent_after_a_restart_an_operation_beside_a_long_history_parks_and_reverses() {
    let run = history(9, 0);
    run.port.unavailable.store(true, Ordering::SeqCst);
    let (worker, returned) = spawn_fast(run.repository, run.authority, run.port.clone());
    let client = worker.client();
    let parked = settled(&client, run.bot.id, run.trash);
    assert_eq!(parked.stage, BotLifecycleStage::Incomplete, "{parked:?}");
    assert_eq!(parked.refusal, Some(BotLifecycleRefusal::OwnerUnavailable));
    let calls = run.port.calls.lock().unwrap().len();
    restore_chat_at(
        &client,
        &run.bot,
        run.chat,
        run.trash,
        read_bot(&client, run.bot.id).revision,
    )
    .unwrap();
    assert_eq!(
        run.port.calls.lock().unwrap().len() - calls,
        0,
        "the Restore asked the owner nothing"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    drop(returned);
}

/// The owner is up after a restart. Forty tasks of history its owner refused
/// for good owe it nothing and are never asked about; the two it admitted
/// and the person stopped are each asked once. The conversation's Trash
/// beside them completes.
#[test]
fn a_restart_asks_the_owner_about_history_only_where_something_may_be_owed() {
    let run = history(2, 40);
    let (worker, returned) = spawn(run.repository, run.authority, run.port.clone());
    let client = worker.client();
    let done = settled(&client, run.bot.id, run.trash);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        reconciles(&run.port),
        2,
        "one per task that may owe something"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    drop(returned);
}
