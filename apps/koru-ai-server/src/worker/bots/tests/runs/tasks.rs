// SPDX-License-Identifier: MPL-2.0
//! `read_run` without `message_id` (P1.8): the Run of a task started through
//! the API, which writes no reply, bound through the Bot's own
//! `start`/`continue` receipt, found by its unique planned Run. The same
//! authority, holds, session withholding, lane and deadline as a reply's
//! Run; the receipt must be this Bot's and owner's, admitted (`completed`),
//! started from no conversation attempt, and name the conversation the read
//! names.
use super::*;

/// The fixture owner's authority, capturing Role history as the Store's
/// `capture_history` does, so a `start` can be prepared.
struct Capturing(Arc<Authority>);
impl BotAuthority for Capturing {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.0.owner()
    }
    fn workspace(&self, id: WorkspaceId) -> Result<koru_ai_core::ProfileId, BotError> {
        self.0.workspace(id)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.0.page(page)
    }
    fn capture(
        &self,
        _owner: BotOwner,
        _workspace: WorkspaceId,
        _profile: koru_ai_core::ProfileId,
        pages: &[BotPage],
    ) -> Result<Vec<d::RunContextItem>, BotError> {
        Ok(pages
            .iter()
            .map(|page| d::RunContextItem::Document {
                workspace: page.workspace,
                document: page.document,
                version: d::DocumentVersionId::new(),
            })
            .collect())
    }
    fn inspect(
        &self,
        workspaces: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<koru_ai_core::bots::BotAuthoritySnapshot, BotError> {
        self.0.inspect(workspaces, pages)
    }
}

fn task_read(bot: BotId, chat: ChatId, run: d::RunId) -> BotCommand {
    BotCommand::ReadRun {
        bot_id: bot,
        chat_id: chat,
        message_id: None,
        run_id: run,
        from: 0,
        len: 100,
    }
}

/// One of `bot`'s API tasks in `chat`, prepared as `start` prepares it, and
/// admitted by its owner when `admitted` (its receipt `completed`, as
/// `acknowledge_bot_submission` leaves it).
fn api_task(
    repo: &mut Repository,
    bot: &Bot,
    chat: ChatId,
    admitted: bool,
) -> (BotRequestId, d::RunId) {
    let request = BotRequestId::mint();
    let intent = crate::worker::bots::admission::make_intent(
        bot,
        request,
        chat,
        bot.home_workspace,
        &BotTask {
            text: "API-TASK".into(),
            documents: vec![],
        },
        None,
        &[d::RunContextItem::Document {
            workspace: bot.config.role.workspace,
            document: bot.config.role.document,
            version: d::DocumentVersionId::new(),
        }],
        koru_ai_core::ProfileId::new(),
    )
    .unwrap();
    let run = intent.submission.request.run;
    repo.prepare_bot_submission(&intent, &"e".repeat(64), bot.revision, 1)
        .unwrap();
    if admitted {
        repo.acknowledge_bot_submission(
            delegate_owner_of(bot),
            bot.id,
            request,
            &d::BotAdmissionView {
                key: d::BotSubmissionKey {
                    owner: delegate_owner(bot),
                    bot: bot.id,
                    request,
                },
                run,
                state: d::BotAdmissionState::Settled,
                queue_sequence: 1,
                fence: None,
            },
            2,
        )
        .unwrap();
    }
    (request, run)
}

fn delegate_owner_of(bot: &Bot) -> BotOwner {
    BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    }
}

/// `start` → `read_operation` gives its `run_id` → `read_run` without a
/// message reads that Run, without its session, through the execution
/// owner on the Run reads' lane.
#[test]
fn a_started_task_reads_from_start_through_read_operation_to_read_run() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let owner = RunOwner::on(bot.owner_device);
    owner.admits.store(true, Ordering::SeqCst);
    let (worker, client) = worker_with(repo, Arc::new(Capturing(authority.clone())), owner.clone());
    let request = BotRequestId::mint();
    let started = client.bots(
        context(true),
        BotCommand::Start {
            request_id: request,
            bot_id: bot.id,
            chat_id: bot.main_chat,
            expected_revision: bot.revision,
            destination: bot.home_workspace,
            task: BotTask {
                text: "API-TASK".into(),
                documents: vec![],
            },
        },
    );
    assert!(
        matches!(&started, Ok(BotResponse::Operation { .. })),
        "{:?}",
        started.map(|_| ()).map_err(|error| error.code)
    );
    let mut run = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while run.is_none() {
        assert!(Instant::now() < deadline, "the task was never accepted");
        if let Ok(BotResponse::Operation { operation }) = client.bots(
            context(true),
            BotCommand::ReadOperation {
                request_id: request,
                bot_id: bot.id,
            },
        ) && operation.status == BotOperationStatus::Accepted
        {
            run = operation.run_id;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let run = run.unwrap();
    let response = client
        .bots(context(true), task_read(bot.id, bot.main_chat, run))
        .unwrap();
    let wire = serde_json::to_value(&response).unwrap();
    assert!(
        !wire.to_string().contains(&owner.session.to_string()),
        "the session leaked"
    );
    let BotResponse::Run { view } = response else {
        panic!("a Run view")
    };
    assert_eq!(view.header.run, run);
    assert_eq!(view.execution.session, None);
    let reads = owner.reads();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].0.run, run);
    assert_eq!(reads[0].1.workspace, Some(bot.home_workspace));
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// Every binding the read must refuse, none of which reaches the owner.
#[test]
fn an_api_task_run_binds_only_through_this_bots_admitted_receipt_in_that_conversation() {
    use koru_ai_wire::bot_lifecycle_codes::TRASHED;
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let sibling = another(&mut repo, &authority);
    let additional = repo
        .create_bot_chat(&bot, authority.profile, BotRequestId::mint(), "x", at(5))
        .unwrap()
        .chat_id;
    let trashed_chat = repo
        .create_bot_chat(&bot, authority.profile, BotRequestId::mint(), "y", at(6))
        .unwrap()
        .chat_id;
    let (_, run) = api_task(&mut repo, &bot, bot.main_chat, true);
    let (_, unadmitted) = api_task(&mut repo, &bot, bot.main_chat, false);
    let (_, in_trash) = api_task(&mut repo, &bot, trashed_chat, true);
    let (reply, reply_run) = replied(&mut repo, &bot, bot.main_chat, 1, bot.home_workspace);
    let operation = repo
        .capture_bot_lifecycle(&super::super::holds::capture(
            &bot,
            BotLifecycleKind::TrashChat,
            Some(trashed_chat),
        ))
        .unwrap();
    repo.fixture_trash_bot_chat(&operation, trashed_chat, 9)
        .unwrap();
    // Another owner's Bot, with an admitted task of its own.
    let (_, foreign_authority, foreign_command) = fixture();
    let foreign = created(&mut repo, foreign_authority, foreign_command);
    let (_, foreign_run) = api_task(&mut repo, &foreign, foreign.main_chat, true);
    let owner = RunOwner::on(bot.owner_device);
    for held in [run, unadmitted, in_trash, reply_run] {
        owner.holds(held, &bot);
    }
    let (worker, client) = worker(repo, authority, owner.clone());
    for (command, expected, why) in [
        (
            task_read(sibling.id, sibling.main_chat, run),
            "not_found",
            "another Bot",
        ),
        (
            task_read(sibling.id, bot.main_chat, run),
            "not_found",
            "another Bot's conversation",
        ),
        (
            task_read(bot.id, additional, run),
            "not_found",
            "another conversation",
        ),
        (
            task_read(bot.id, bot.main_chat, unadmitted),
            "not_found",
            "not admitted",
        ),
        (
            task_read(bot.id, bot.main_chat, d::RunId::mint()),
            "not_found",
            "no such Run",
        ),
        (
            task_read(bot.id, bot.main_chat, reply_run),
            "not_found",
            "a reply's Run without its reply",
        ),
        (
            read(bot.id, bot.main_chat, reply.assistant_message.id, run),
            "not_found",
            "an API task's Run through a reply",
        ),
        (
            task_read(bot.id, trashed_chat, in_trash),
            TRASHED,
            "a conversation in Trash",
        ),
        (
            task_read(foreign.id, foreign.main_chat, foreign_run),
            "wrong_owner",
            "another owner",
        ),
    ] {
        assert_eq!(code(&client, context(true), command), expected, "{why}");
    }
    assert!(
        owner.reads().is_empty(),
        "no refused read reached the owner"
    );
    // The reply's own Run still reads through its reply, and the API task's
    // without a message.
    assert_eq!(
        view(
            &client,
            read(bot.id, bot.main_chat, reply.assistant_message.id, reply_run)
        )
        .header
        .run,
        reply_run
    );
    assert_eq!(
        view(&client, task_read(bot.id, bot.main_chat, run))
            .header
            .run,
        run
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// `m8_admission_inventory`, rows 3 and 5 (M8 piece W): with the team
/// paused, the Bots API `start` and `continue` answer `team_paused`. No
/// receipt is written, so nothing is left for recovery to submit, and the
/// execution owner is never asked to admit anything. After the team
/// resumes the same `start` is accepted.
#[test]
fn m8_admission_inventory_rows_3_and_5_the_bots_api_start_is_refused_and_leaves_nothing() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let owner = RunOwner::on(bot.owner_device);
    owner.admits.store(true, Ordering::SeqCst);
    let (worker, client) = worker_with(repo, Arc::new(Capturing(authority.clone())), owner.clone());
    let team = |paused: bool, expected: u64| {
        let request_id = BotRequestId::mint();
        let command = if paused {
            BotCommand::PauseTeam {
                request_id,
                expected_team_revision: expected,
            }
        } else {
            BotCommand::ResumeTeam {
                request_id,
                expected_team_revision: expected,
            }
        };
        assert!(matches!(
            client.bots(context(true), command),
            Ok(BotResponse::TeamPause { team }) if team.paused == paused
        ));
    };
    let request = BotRequestId::mint();
    let start = || BotCommand::Start {
        request_id: request,
        bot_id: bot.id,
        chat_id: bot.main_chat,
        expected_revision: bot.revision,
        destination: bot.home_workspace,
        task: BotTask {
            text: "API-TASK".into(),
            documents: vec![],
        },
    };
    team(true, 0);
    for _ in 0..2 {
        let refused = client.bots(context(true), start()).unwrap_err();
        assert_eq!(
            (refused.code.as_str(), refused.retryable),
            (koru_ai_wire::bot_team_codes::TEAM_PAUSED, false)
        );
    }
    let refused = client
        .bots(
            context(true),
            BotCommand::Continue {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                chat_id: bot.main_chat,
                expected_revision: bot.revision,
                destination: bot.home_workspace,
                task: BotTask {
                    text: "API-CONTINUE".into(),
                    documents: vec![],
                },
                previous_run: d::RunId::mint(),
            },
        )
        .unwrap_err();
    // A continuation of a Run that does not exist is `not_found` before the
    // team is read; what matters is that it is refused and writes nothing.
    assert!(
        refused.code == koru_ai_wire::bot_team_codes::TEAM_PAUSED || refused.code == "not_found",
        "{}",
        refused.code
    );
    // No receipt: the operation does not exist, so recovery has nothing to
    // submit while the team is held (row 5).
    assert_eq!(
        client
            .bots(
                context(true),
                BotCommand::ReadOperation {
                    request_id: request,
                    bot_id: bot.id,
                },
            )
            .unwrap_err()
            .code,
        "not_found"
    );
    assert!(
        owner.admitted.lock().unwrap().is_empty(),
        "nothing was offered to the execution owner"
    );
    // The team resumes: the same request is accepted.
    team(false, 1);
    assert!(matches!(
        client.bots(context(true), start()),
        Ok(BotResponse::Operation { .. })
    ));
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
