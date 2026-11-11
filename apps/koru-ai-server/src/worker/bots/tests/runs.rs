// SPDX-License-Identifier: MPL-2.0
//! `read_run` (P1.8 B1) through the real worker: the writer checks the owner,
//! the conversation's Trash and the binding of the Run to the reply; a job
//! on the Run reads' own bounded lane checks authority again and asks the
//! execution owner within a deadline; the answer is the Run view with
//! nothing that names its session.
use super::*;
use koru_delegate_api::bots::{BotCommand as D, BotDelegatePort, BotReply};
use koru_delegate_api::wire::TimelineQuery;
use koru_delegate_core as d;
use koru_delegate_core::{BotRunKey, RunExecution, RunView, TimelineItem, TimelineWindow};
use std::collections::HashMap;
use std::sync::Mutex;

/// The execution owner's `Read`, refusing exactly what the real one refuses
/// and answering what it answers. Each branch names the real code it
/// mirrors (`koru-delegate-host` `actor/bots.rs` `BotCommand::Read`, which
/// calls `koru-delegate-runtime` `Orchestrator::bot_read_allowed` and then
/// `Actor::view`); a Run read always states how long it may wait, as
/// `LocalBotDelegate::execute_within` bounds its socket.
struct RunOwner {
    /// The delegate worker's own device (`Actor::device`).
    device: DeviceId,
    /// The provider session every Run here was driven in.
    session: d::SessionId,
    /// The ledger's immutable binding of each Bot Run: the owner and Bot its
    /// `BotExecutionRecord` holds.
    ledger: Mutex<HashMap<d::RunId, (d::BotOwner, BotId)>>,
    /// History entries every Run holds.
    entries: u64,
    reads: Mutex<Vec<(BotRunKey, TimelineQuery)>>,
    gate: Mutex<Option<(SyncSender<()>, Receiver<()>)>>,
    /// The delegate worker does not answer.
    down: AtomicBool,
    /// Its Runs say the agent asked for them (no Bot Run does).
    agent_requested: AtomicBool,
}

impl RunOwner {
    fn on(device: DeviceId) -> Arc<Self> {
        Arc::new(Self {
            device,
            session: d::SessionId::mint(),
            ledger: Mutex::default(),
            entries: 7,
            reads: Mutex::default(),
            gate: Mutex::default(),
            down: AtomicBool::new(false),
            agent_requested: AtomicBool::new(false),
        })
    }
    fn holds(&self, run: d::RunId, bot: &Bot) {
        self.ledger
            .lock()
            .unwrap()
            .insert(run, (delegate_owner(bot), bot.id));
    }
    fn reads(&self) -> Vec<(BotRunKey, TimelineQuery)> {
        self.reads.lock().unwrap().clone()
    }
    /// The history a Bot Run's owner writes once it starts: `bot_mark_started`
    /// records the attempt and its session on the Started step; turns follow,
    /// one with the agent as its actor, and a usage fact.
    fn timeline(&self, at: u64) -> TimelineItem {
        let attempt = d::RunAttemptId::from_uuid(Uuid::from_u128(1));
        let step = |kind, refs: Vec<d::TimelineRef>| {
            TimelineItem::Step(d::TimelineEntry {
                id: d::TimelineEntryId::mint(),
                kind,
                refs: d::TimelineRefs::new(refs).unwrap(),
                at_ms: at as i64,
            })
        };
        match at {
            0 => step(d::StepKind::RunCreated, vec![]),
            1 => step(
                d::StepKind::Started,
                vec![
                    d::TimelineRef::Attempt { attempt },
                    d::TimelineRef::Session {
                        session: self.session,
                    },
                ],
            ),
            2 => step(
                d::StepKind::TurnStarted {
                    originator: d::TurnOriginator::Orchestrator,
                },
                vec![
                    d::TimelineRef::Attempt { attempt },
                    d::TimelineRef::Actor {
                        actor: d::ActorRef::Agent {
                            session: self.session,
                        },
                    },
                ],
            ),
            3 => TimelineItem::Usage(d::UsageFact {
                identity: d::UsageFactId::mint(),
                attempt,
                turn: d::AgentTurnId::mint(),
                revision: d::AmendmentRevision(0),
                input: Some(1),
                output: Some(2),
                cached_input: None,
                model: d::ModelRef::mint(),
                completeness: d::Completeness::Complete,
            }),
            _ => step(
                d::StepKind::Completed,
                vec![d::TimelineRef::Attempt { attempt }],
            ),
        }
    }
}

/// `koru-delegate-host` `bot_failure` for a `bot_*` port code.
fn refused(code: &str) -> koru_delegate_api::Failure {
    koru_delegate_api::Failure::new(code, "The Bot operation was refused.", false)
}

impl BotDelegatePort for RunOwner {
    fn execute(&self, command: D) -> Result<BotReply, koru_delegate_api::Failure> {
        assert!(
            !matches!(command, D::Read { .. }),
            "a Run read always states how long it may wait"
        );
        // Startup recovery (native effects, deliveries) is not under test.
        Err(koru_delegate_api::Failure::new(
            "fixture_pending",
            "fixture recovery stays pending",
            true,
        ))
    }

    fn execute_within(
        &self,
        command: D,
        within: Duration,
    ) -> Result<BotReply, koru_delegate_api::Failure> {
        assert_eq!(
            std::thread::current().name(),
            Some("koru-ai-provider-run-read"),
            "the execution owner is asked on the Run reads' own lane"
        );
        let D::Read { key, window } = command else {
            panic!("only a Run read is bounded");
        };
        self.reads.lock().unwrap().push((key.clone(), window));
        if let Some((entered, release)) = self.gate.lock().unwrap().take() {
            entered.send(()).unwrap();
            // `LocalDelegate::call_within`: the socket's read times out.
            if release.recv_timeout(within).is_err() {
                return Err(koru_delegate_api::Failure::unavailable());
            }
        }
        if self.down.load(Ordering::SeqCst) {
            // `LocalDelegate` when no delegate worker answers.
            return Err(koru_delegate_api::Failure::unavailable());
        }
        // `owner_device`: the asserted owner's device is this worker's own.
        if key.owner.device != self.device || self.device.as_uuid().is_nil() {
            return Err(refused("bot_owner_mismatch"));
        }
        // `bot_record`: an unknown Run is `RunError::UnknownRun`, which
        // `failure_of` spells `unknown_run`; a Run bound to another owner or
        // Bot is `bot_owner_mismatch`.
        let Some((owner, bot)) = self.ledger.lock().unwrap().get(&key.run).copied() else {
            return Err(koru_delegate_api::Failure::new(
                "unknown_run",
                "No run by that id exists on this device.",
                false,
            ));
        };
        if owner != key.owner || bot != key.bot {
            return Err(refused("bot_owner_mismatch"));
        }
        // `Actor::view` over `Orchestrator::view`: the live session, and the
        // window `from.min(total)`, `bounded_len()` entries, as recorded.
        let start = window.from.min(self.entries);
        let items = (start..self.entries)
            .take(window.bounded_len())
            .map(|n| self.timeline(n))
            .collect();
        let mut header = d::RunHeader::from_request(
            d::DelegationRequest {
                run: key.run,
                requested_by: d::ActorRef::Person {
                    profile: d::ProfileId::from_uuid(Uuid::new_v4()),
                },
                objective: d::Objective::new("RUN-OBJECTIVE").unwrap(),
                agent: d::SessionProfileKey::BuiltIn("codex".parse().unwrap()),
                requested_model: None,
                scope: d::WorkspaceScope::BotFolder {
                    folder: d::BotWorkingFolder::new("D:/isolated".into()).unwrap(),
                },
                context: Vec::new(),
                grants: Default::default(),
                executing_device: self.device,
                origin: Default::default(),
            },
            1,
        );
        header.bot = Some(d::BotProvenance {
            owner,
            bot,
            submission: BotRequestId::mint(),
            chat_id: Uuid::new_v4(),
            ai_attempt_id: Some(Uuid::new_v4()),
            assistant_message_id: Some(Uuid::new_v4()),
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
        });
        header.status = d::RunStatus::Completed;
        if self.agent_requested.load(Ordering::SeqCst) {
            header.requested_by = d::ActorRef::Agent {
                session: self.session,
            };
        }
        Ok(BotReply::Run {
            view: RunView {
                document: d::DocumentId::from_uuid(Uuid::new_v4()),
                header,
                execution: RunExecution {
                    device: self.device,
                    session: Some(self.session),
                    run_workspace: Some("D:/isolated".into()),
                },
                context: Vec::new(),
                timeline: TimelineWindow {
                    total: self.entries,
                    from: start,
                    items,
                },
            },
        })
    }
}

fn delegate_owner(bot: &Bot) -> d::BotOwner {
    d::BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    }
}

fn worker(
    repo: Repository,
    authority: Arc<dyn BotAuthority>,
    owner: Arc<RunOwner>,
) -> (WorkerHandle, AiWorkerClient) {
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-run-reads"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(owner);
    config.test_repository = Some(repo);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    (worker, client)
}

fn completed(repo: &mut Repository, chat: ChatId, offset: i64) -> PendingTurn {
    let mut pending = turn(chat, Uuid::new_v4(), offset);
    // After whatever the conversation already holds.
    let next = repo.messages(chat).unwrap().len() as u32;
    pending.user_message.ordinal = next;
    pending.assistant_message.ordinal = next + 1;
    repo.begin_turn(&pending).unwrap();
    repo.apply_event(
        &AiEvent::Completed {
            attempt_id: pending.attempt.id,
            stop_reason: None,
            at: at(offset + 1),
        },
        &[MessageBlock {
            id: MessageBlockId::new(),
            message_id: pending.assistant_message.id,
            ordinal: 0,
            kind: MessageBlockKind::Paragraph,
            markdown: "Bot reply".into(),
            plain_text: "Bot reply".into(),
        }],
    )
    .unwrap();
    pending
}

/// A finished reply in `chat`, written by a Run whose record is in `record`.
fn replied(
    repo: &mut Repository,
    bot: &Bot,
    chat: ChatId,
    offset: i64,
    record: WorkspaceId,
) -> (PendingTurn, d::RunId) {
    let pending = completed(repo, chat, offset);
    let run = d::RunId::mint();
    repo.record_bot_chat_run_for_fixture(
        bot,
        pending.attempt.id,
        chat,
        pending.assistant_message.id,
        run,
        record,
    )
    .unwrap();
    (pending, run)
}

/// Another Bot of the same owner and workspace.
fn another(repo: &mut Repository, authority: &Arc<Authority>) -> Bot {
    let (_, _, mut command) = fixture();
    if let BotCommand::Create {
        home_workspace,
        config,
        ..
    } = &mut command
    {
        *home_workspace = authority.workspace;
        config.role.workspace = authority.workspace;
    }
    created(repo, authority.clone(), command)
}

/// A branch's copy of `chat` through `through`, as the shared Branch builds it.
fn copy(repo: &Repository, bot: &Bot, chat: ChatId, through: MessageId) -> BranchCopy {
    let id = ChatId::new();
    let mut ids = HashMap::new();
    let messages: Vec<Message> = repo
        .messages(chat)
        .unwrap()
        .into_iter()
        .map(|mut message| {
            let origin = message.id;
            message.id = MessageId::new();
            message.chat_id = id;
            message.origin_message_id = Some(origin);
            ids.insert(origin, message.id);
            message
        })
        .collect();
    let blocks = repo
        .blocks(chat)
        .unwrap()
        .into_iter()
        .filter_map(|mut block| {
            block.message_id = *ids.get(&block.message_id)?;
            block.id = MessageBlockId::new();
            Some(block)
        })
        .collect();
    let mut tool_policy = repo.chat_tool_policy(chat).unwrap().unwrap();
    tool_policy.chat_id = id;
    let source = repo
        .chat_with_access(chat, &ChatReadAccess::authorized(vec![chat]))
        .unwrap()
        .unwrap();
    BranchCopy {
        chat: Chat {
            id,
            title: "Branch".into(),
            branched_from: Some(BranchOrigin {
                chat_id: chat,
                through_message_id: through,
            }),
            bot: None,
            agent_mode: bot.config.mode.clone(),
            agent_fast: bot.config.fast,
            pinned_at: None,
            ..source
        },
        tool_policy,
        messages,
        blocks,
        citations: vec![],
        pinned_documents: vec![],
        attachments: vec![],
    }
}

fn read(bot: BotId, chat: ChatId, message: MessageId, run: d::RunId) -> BotCommand {
    BotCommand::ReadRun {
        bot_id: bot,
        chat_id: chat,
        message_id: message,
        run_id: run,
        from: 0,
        len: 100,
    }
}

fn view(client: &AiWorkerClient, command: BotCommand) -> RunView {
    match client.bots(context(true), command).unwrap() {
        BotResponse::Run { view } => view,
        other => panic!("a Run view, not {other:?}"),
    }
}

fn code(client: &AiWorkerClient, context: RequestContext, command: BotCommand) -> String {
    client.bots(context, command).unwrap_err().code
}

#[test]
fn the_owner_reads_the_run_that_wrote_a_reply_without_its_session() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let (reply, run) = replied(&mut repo, &bot, bot.main_chat, 1, bot.home_workspace);
    let owner = RunOwner::on(bot.owner_device);
    owner.holds(run, &bot);
    let (worker, client) = worker(repo, authority, owner.clone());

    let response = client
        .bots(
            context(true),
            BotCommand::ReadRun {
                bot_id: bot.id,
                chat_id: bot.main_chat,
                message_id: reply.assistant_message.id,
                run_id: run,
                from: 1,
                len: 3,
            },
        )
        .unwrap();
    let wire = serde_json::to_value(&response).unwrap();
    assert_eq!(wire["result"], "run");
    assert!(
        wire["view"]["execution"].get("session").is_none(),
        "{}",
        wire["view"]["execution"]
    );
    // The owner's Started step names the session, and a turn names the agent
    // by it: neither leaves, anywhere in the answer.
    let text = serde_json::to_string(&response).unwrap();
    assert!(
        !text.contains(&owner.session.to_string()),
        "the session leaked: {text}"
    );
    let BotResponse::Run { view } = response else {
        panic!("a Run view")
    };
    assert_eq!(view.header.run, run);
    assert_eq!(view.header.status, d::RunStatus::Completed);
    assert_eq!(view.execution.session, None);
    assert_eq!(view.execution.device, bot.owner_device);
    // The owner's own folder stays, as the Bot read returns it.
    assert_eq!(view.execution.run_workspace.as_deref(), Some("D:/isolated"));
    assert_eq!((view.timeline.total, view.timeline.from), (7, 1));
    assert_eq!(view.timeline.items.len(), 3);
    // The steps are kept with their other references.
    let refs = |n: usize| match &view.timeline.items[n] {
        TimelineItem::Step(entry) => entry.refs.iter().cloned().collect::<Vec<_>>(),
        other => panic!("a step, not {other:?}"),
    };
    let attempt = d::TimelineRef::Attempt {
        attempt: d::RunAttemptId::from_uuid(Uuid::from_u128(1)),
    };
    assert_eq!(refs(0), vec![attempt.clone()]);
    assert_eq!(refs(1), vec![attempt]);
    assert!(view.header.bot.is_some());
    // The execution owner was asked for exactly this Bot Run, owner and
    // window, naming the record's workspace.
    assert_eq!(
        owner.reads(),
        vec![(
            BotRunKey {
                owner: delegate_owner(&bot),
                bot: bot.id,
                run,
            },
            TimelineQuery {
                from: 1,
                len: 3,
                workspace: Some(bot.home_workspace),
            },
        )]
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn a_run_that_is_not_this_replys_is_not_found_and_the_owner_is_never_asked() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let other = another(&mut repo, &authority);
    let (reply, run) = replied(&mut repo, &bot, bot.main_chat, 1, bot.home_workspace);
    let additional = repo
        .create_bot_chat(&bot, authority.profile, BotRequestId::mint(), "x", at(5))
        .unwrap()
        .chat_id;
    // Bound in ai.db, but the execution owner has never heard of it.
    let (forgotten, forgotten_run) = replied(&mut repo, &bot, additional, 6, bot.home_workspace);
    let owner = RunOwner::on(bot.owner_device);
    owner.holds(run, &bot);
    let (worker, client) = worker(repo, authority, owner.clone());
    let message = reply.assistant_message.id;
    for (case, command) in [
        ("another Bot", read(other.id, bot.main_chat, message, run)),
        (
            "another Bot's conversation",
            read(other.id, other.main_chat, message, run),
        ),
        (
            "a Bot nobody made",
            read(BotId::mint(), bot.main_chat, message, run),
        ),
        (
            "another conversation",
            read(bot.id, additional, message, run),
        ),
        (
            "an unknown conversation",
            read(bot.id, ChatId::new(), message, run),
        ),
        (
            "the question, not the reply",
            read(bot.id, bot.main_chat, reply.user_message.id, run),
        ),
        (
            "an unknown message",
            read(bot.id, bot.main_chat, MessageId::new(), run),
        ),
        (
            "another Run",
            read(bot.id, bot.main_chat, message, d::RunId::mint()),
        ),
        (
            "another reply's Run",
            read(bot.id, bot.main_chat, message, forgotten_run),
        ),
    ] {
        assert_eq!(code(&client, context(true), command), "not_found", "{case}");
    }
    assert!(
        owner.reads().is_empty(),
        "nothing unbound reaches the owner"
    );
    // Bound here and unknown to the execution owner: still `not_found`.
    assert_eq!(
        code(
            &client,
            context(true),
            read(
                bot.id,
                additional,
                forgotten.assistant_message.id,
                forgotten_run
            )
        ),
        "not_found"
    );
    assert_eq!(owner.reads().len(), 1);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn a_branch_copy_reads_its_source_run_and_still_does_once_the_source_is_purged() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let (live, live_run) = replied(&mut repo, &bot, bot.main_chat, 1, bot.home_workspace);
    let (gone, gone_run) = replied(&mut repo, &bot, bot.main_chat, 3, bot.home_workspace);
    let first = copy(&repo, &bot, bot.main_chat, live.assistant_message.id);
    repo.create_bot_branch(&bot, &first).unwrap();
    let second = copy(&repo, &bot, bot.main_chat, gone.assistant_message.id);
    repo.create_bot_branch(&bot, &second).unwrap();
    let copied = |branch: &BranchCopy, source: MessageId| {
        branch
            .messages
            .iter()
            .find(|m| m.origin_message_id == Some(source))
            .unwrap()
            .id
    };
    let live_copy = copied(&first, live.assistant_message.id);
    let gone_copy = copied(&second, gone.assistant_message.id);
    // The second reply's source is purged; its copy keeps a reference.
    repo.purge_bot_chat_run_source_for_fixture(gone_run, second.chat.id, gone_copy)
        .unwrap();
    let owner = RunOwner::on(bot.owner_device);
    owner.holds(live_run, &bot);
    owner.holds(gone_run, &bot);
    let (worker, client) = worker(repo, authority, owner.clone());
    assert_eq!(
        view(&client, read(bot.id, first.chat.id, live_copy, live_run))
            .header
            .run,
        live_run
    );
    assert_eq!(
        view(&client, read(bot.id, second.chat.id, gone_copy, gone_run))
            .header
            .run,
        gone_run
    );
    // The purged source's own reply no longer binds; the copy is named in
    // its own conversation only.
    assert_eq!(
        code(
            &client,
            context(true),
            read(bot.id, bot.main_chat, gone.assistant_message.id, gone_run)
        ),
        "not_found"
    );
    assert_eq!(
        code(
            &client,
            context(true),
            read(bot.id, bot.main_chat, live_copy, live_run)
        ),
        "not_found"
    );
    assert_eq!(owner.reads().len(), 2);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn a_conversation_in_trash_refuses_with_its_hold_and_an_archived_bot_reads() {
    use koru_ai_wire::bot_lifecycle_codes::{BOT_TRASHED, TRASHED};
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let trashed_bot = another(&mut repo, &authority);
    let archived = another(&mut repo, &authority);
    let additional = repo
        .create_bot_chat(&bot, authority.profile, BotRequestId::mint(), "x", at(5))
        .unwrap()
        .chat_id;
    let (kept, kept_run) = replied(&mut repo, &bot, bot.main_chat, 1, bot.home_workspace);
    let (hidden, hidden_run) = replied(&mut repo, &bot, additional, 6, bot.home_workspace);
    let (gone, gone_run) = replied(
        &mut repo,
        &trashed_bot,
        trashed_bot.main_chat,
        8,
        bot.home_workspace,
    );
    let (old, old_run) = replied(
        &mut repo,
        &archived,
        archived.main_chat,
        10,
        bot.home_workspace,
    );
    let operation = repo
        .capture_bot_lifecycle(&super::holds::capture(
            &bot,
            BotLifecycleKind::TrashChat,
            Some(additional),
        ))
        .unwrap();
    repo.fixture_trash_bot_chat(&operation, additional, 9)
        .unwrap();
    repo.fixture_set_bot_lifecycle(trashed_bot.id, BotLifecycle::Trashed)
        .unwrap();
    repo.fixture_set_bot_lifecycle(archived.id, BotLifecycle::Archived)
        .unwrap();
    let owner = RunOwner::on(bot.owner_device);
    for (run, of) in [
        (kept_run, &bot),
        (hidden_run, &bot),
        (gone_run, &trashed_bot),
        (old_run, &archived),
    ] {
        owner.holds(run, of);
    }
    let (worker, client) = worker(repo, authority, owner.clone());
    assert_eq!(
        code(
            &client,
            context(true),
            read(bot.id, additional, hidden.assistant_message.id, hidden_run)
        ),
        TRASHED
    );
    assert_eq!(
        code(
            &client,
            context(true),
            read(
                trashed_bot.id,
                trashed_bot.main_chat,
                gone.assistant_message.id,
                gone_run
            )
        ),
        BOT_TRASHED
    );
    assert!(owner.reads().is_empty());
    // The Bot's other conversation, and an archived Bot's history, read.
    assert_eq!(
        view(
            &client,
            read(bot.id, bot.main_chat, kept.assistant_message.id, kept_run)
        )
        .header
        .run,
        kept_run
    );
    assert_eq!(
        view(
            &client,
            read(
                archived.id,
                archived.main_chat,
                old.assistant_message.id,
                old_run
            )
        )
        .header
        .run,
        old_run
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn authority_is_checked_on_the_writer_and_again_off_it_and_the_owner_repeats_the_binding() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let (_, stranger, stranger_command) = fixture();
    let foreign = created(&mut repo, stranger.clone(), stranger_command);
    let (reply, run) = replied(&mut repo, &bot, bot.main_chat, 1, bot.home_workspace);
    let (theirs, their_run) = replied(
        &mut repo,
        &foreign,
        foreign.main_chat,
        3,
        foreign.home_workspace,
    );
    // A Run whose record sits in a workspace the owner cannot reach now.
    let elsewhere = WorkspaceId::new();
    let additional = repo
        .create_bot_chat(&bot, authority.profile, BotRequestId::mint(), "x", at(5))
        .unwrap()
        .chat_id;
    let (moved, moved_run) = replied(&mut repo, &bot, additional, 6, elsewhere);
    // Runs the execution owner binds to another Bot, and one it is asked
    // about from another device.
    let (misbound, misbound_run) = replied(&mut repo, &bot, additional, 8, bot.home_workspace);
    let owner = RunOwner::on(bot.owner_device);
    owner.holds(run, &bot);
    owner.holds(moved_run, &bot);
    owner.holds(misbound_run, &foreign);
    owner.holds(their_run, &foreign);
    let (worker, client) = worker(repo, authority.clone(), owner.clone());
    let mine = read(bot.id, bot.main_chat, reply.assistant_message.id, run);

    // Ordinary chat scope: refused before anything is looked at.
    assert_eq!(code(&client, context(false), mine.clone()), "forbidden");
    // Someone else's Bot.
    assert_eq!(
        code(
            &client,
            context(true),
            read(
                foreign.id,
                foreign.main_chat,
                theirs.assistant_message.id,
                their_run
            )
        ),
        "wrong_owner"
    );
    assert!(owner.reads().is_empty());
    // Off the writer, the record's workspace is inspected again.
    assert_eq!(
        code(
            &client,
            context(true),
            read(bot.id, additional, moved.assistant_message.id, moved_run)
        ),
        "wrong_owner"
    );
    assert!(
        owner.reads().is_empty(),
        "refused before the owner is asked"
    );
    // The execution owner repeats its immutable binding; the AI owner's
    // own checks passed, so its disagreement is `not_found`, never "it
    // exists elsewhere".
    assert_eq!(
        code(
            &client,
            context(true),
            read(
                bot.id,
                additional,
                misbound.assistant_message.id,
                misbound_run
            )
        ),
        "not_found"
    );
    assert_eq!(owner.reads().len(), 1);
    // An owner that does not answer is retryable.
    owner.down.store(true, Ordering::SeqCst);
    let unavailable = client.bots(context(true), mine.clone()).unwrap_err();
    assert_eq!(unavailable.code, "bot_unavailable");
    assert!(unavailable.retryable);
    owner.down.store(false, Ordering::SeqCst);
    assert_eq!(view(&client, mine.clone()).header.run, run);
    // Revoked current authority over the Bot's workspace.
    authority.allowed.store(false, Ordering::SeqCst);
    assert_eq!(code(&client, context(true), mine), "wrong_owner");
    assert_eq!(owner.reads().len(), 3);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn an_execution_owner_on_another_device_refuses_the_owner_it_is_told() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let (reply, run) = replied(&mut repo, &bot, bot.main_chat, 1, bot.home_workspace);
    let owner = RunOwner::on(DeviceId::from_uuid(Uuid::new_v4()));
    owner.holds(run, &bot);
    let (worker, client) = worker(repo, authority, owner.clone());
    assert_eq!(
        code(
            &client,
            context(true),
            read(bot.id, bot.main_chat, reply.assistant_message.id, run)
        ),
        "not_found"
    );
    assert_eq!(owner.reads().len(), 1);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn a_window_is_one_to_five_hundred_entries() {
    assert_eq!(BOT_RUN_WINDOW_LIMIT, koru_delegate_api::wire::MAX_WINDOW);
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let (reply, run) = replied(&mut repo, &bot, bot.main_chat, 1, bot.home_workspace);
    let owner = RunOwner::on(bot.owner_device);
    owner.holds(run, &bot);
    let (worker, client) = worker(repo, authority, owner.clone());
    let window = |from, len| BotCommand::ReadRun {
        bot_id: bot.id,
        chat_id: bot.main_chat,
        message_id: reply.assistant_message.id,
        run_id: run,
        from,
        len,
    };
    for len in [0, BOT_RUN_WINDOW_LIMIT + 1] {
        assert_eq!(
            code(&client, context(true), window(0, len)),
            "invalid_request"
        );
    }
    assert!(owner.reads().is_empty());
    let all = view(&client, window(0, BOT_RUN_WINDOW_LIMIT));
    assert_eq!(all.timeline.items.len(), 7);
    // Past the end is an empty window, not a refusal.
    let past = view(&client, window(50, 1));
    assert_eq!((past.timeline.from, past.timeline.items.len()), (7, 0));
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// While one Run read waits on its execution owner, the Bot's other work does
/// not: its own conversation reads (their preparation hop), another Bot
/// command's preparation, and another Run read's refusal are all answered;
/// and the hung read itself is answered `bot_unavailable` (retryable) by its
/// deadline, never left to the caller's worker timeout.
#[test]
fn a_hung_execution_owner_holds_only_its_own_read_and_is_answered_by_the_deadline() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let (reply, run) = replied(&mut repo, &bot, bot.main_chat, 1, bot.home_workspace);
    let owner = RunOwner::on(bot.owner_device);
    owner.holds(run, &bot);
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    *owner.gate.lock().unwrap() = Some((entered_tx, release));
    let (worker, client) = worker(repo, authority, owner.clone());
    let (reply_tx, answer) = sync_channel(1);
    let sent = Instant::now();
    client
        .send(WorkerCommand::Bots {
            context: context(true).into(),
            request: read(bot.id, bot.main_chat, reply.assistant_message.id, run),
            reply: reply_tx,
        })
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    // The read is in the owner's hands; the writer and the Bot's other work
    // still answer.
    worker.ping().unwrap();
    assert_eq!(
        client
            .read_chat(context(true), bot.main_chat)
            .unwrap()
            .chat
            .id,
        bot.main_chat
    );
    assert!(matches!(
        client
            .bots(
                context(true),
                BotCommand::ListBotChats {
                    bot_id: bot.id,
                    cursor: None,
                    limit: 10,
                },
            )
            .unwrap(),
        BotResponse::Chats { .. }
    ));
    assert_eq!(
        code(
            &client,
            context(true),
            read(bot.id, bot.main_chat, MessageId::new(), run)
        ),
        "not_found"
    );
    assert!(
        answer.try_recv().is_err(),
        "the read waits for its owner, not the writer"
    );
    // Never released: its deadline answers it.
    let late = answer
        .recv_timeout(super::super::reads::RUN_READ_DEADLINE + Duration::from_secs(5))
        .unwrap()
        .unwrap_err();
    assert_eq!(late.code, "bot_unavailable");
    assert!(late.retryable);
    assert!(sent.elapsed() < super::super::reads::RUN_READ_DEADLINE + Duration::from_secs(2));
    drop(release_tx);
    // Once the owner answers again, the Run reads.
    assert_eq!(
        view(
            &client,
            read(bot.id, bot.main_chat, reply.assistant_message.id, run)
        )
        .header
        .run,
        run
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// The Run reads' lane holds one read running and
/// `RUN_READ_QUEUE_CAPACITY` waiting; one more is answered
/// `ai_worker_saturated` at once, and a read whose deadline passed while it
/// waited asks nobody.
#[test]
fn a_full_run_read_lane_answers_at_once_and_a_late_read_asks_nobody() {
    let (output, _events) = sync_channel(64);
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut admin = ProviderAdminPool::spawn(None, None, output, shutdown.clone()).unwrap();
    // The shared administration lane is blocked too: the Run reads' lane
    // does not wait on it.
    let (admin_entered_tx, admin_entered) = sync_channel(1);
    let (admin_release_tx, admin_release) = sync_channel(1);
    admin
        .try_submit(ProviderAdminJob::BlockForFixture {
            entered: admin_entered_tx,
            release: admin_release,
        })
        .unwrap_or_else(|_| panic!("an empty queue"));
    admin_entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let (_, authority, command) = fixture();
    let BotCommand::Create { bot_id, .. } = command else {
        unreachable!()
    };
    let run = d::RunId::mint();
    let key = BotRunKey {
        owner: d::BotOwner {
            user_scope: authority.owner.user_scope,
            device: authority.owner.device,
        },
        bot: bot_id,
        run,
    };
    let owner = RunOwner::on(authority.owner.device);
    owner
        .ledger
        .lock()
        .unwrap()
        .insert(run, (key.owner, bot_id));
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    *owner.gate.lock().unwrap() = Some((entered_tx, release));
    let job = |deadline, reply| {
        BotRunReadJob::fixture(
            authority.clone(),
            authority.owner,
            authority.workspace,
            key.clone(),
            owner.clone(),
            deadline,
            reply,
        )
    };
    let far = Instant::now() + Duration::from_secs(60);
    let (reply, running) = sync_channel(1);
    job(far, reply).submit(Some(&admin));
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    // One waits until it may run; the other's deadline passes while it waits.
    let (reply, waiting) = sync_channel(1);
    job(far, reply).submit(Some(&admin));
    let (reply, late) = sync_channel(1);
    job(Instant::now() + Duration::from_millis(50), reply).submit(Some(&admin));
    let (reply, answer) = sync_channel(1);
    job(far, reply).submit(Some(&admin));
    let full = answer.try_recv().unwrap().unwrap_err();
    assert_eq!(full.code, "ai_worker_saturated");
    assert!(full.retryable);
    // Without a pool at all it is answered at once too.
    let (reply, answer) = sync_channel(1);
    job(far, reply).submit(None);
    assert!(answer.try_recv().unwrap().is_err());
    std::thread::sleep(Duration::from_millis(100));
    release_tx.send(()).unwrap();
    for answer in [running, waiting] {
        assert!(matches!(
            answer.recv_timeout(Duration::from_secs(5)).unwrap(),
            Ok(BotResponse::Run { .. })
        ));
    }
    let late = late
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap_err();
    assert_eq!(late.code, "bot_unavailable");
    assert!(late.retryable);
    // The late read never reached the owner.
    assert_eq!(owner.reads().len(), 2);
    drop(admin_release_tx);
    admin.stop();
}

/// The pairs a transcript names, in message order.
fn named(transcript: &ChatTranscript) -> Vec<(MessageId, d::RunId)> {
    transcript
        .usage
        .iter()
        .filter_map(|entry| entry.run_id.map(|run| (entry.message_id, run)))
        .collect()
}

#[test]
fn the_transcript_names_each_replys_run_and_read_run_accepts_every_pair() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let (kept, kept_run) = replied(&mut repo, &bot, bot.main_chat, 1, bot.home_workspace);
    // Refused before the execution owner admitted it: no Run to name.
    let (refused, refused_run) = replied(&mut repo, &bot, bot.main_chat, 3, bot.home_workspace);
    repo.set_bot_run_receipt_stage_for_fixture(refused_run, "rejected")
        .unwrap();
    // A reply no Run wrote.
    let plain = completed(&mut repo, bot.main_chat, 5);
    let (gone, gone_run) = replied(&mut repo, &bot, bot.main_chat, 7, bot.home_workspace);
    let first = copy(&repo, &bot, bot.main_chat, kept.assistant_message.id);
    repo.create_bot_branch(&bot, &first).unwrap();
    let second = copy(&repo, &bot, bot.main_chat, gone.assistant_message.id);
    repo.create_bot_branch(&bot, &second).unwrap();
    let copied = |branch: &BranchCopy, source: MessageId| {
        branch
            .messages
            .iter()
            .find(|m| m.origin_message_id == Some(source))
            .unwrap()
            .id
    };
    let kept_copy = copied(&first, kept.assistant_message.id);
    let gone_copy = copied(&second, gone.assistant_message.id);
    repo.purge_bot_chat_run_source_for_fixture(gone_run, second.chat.id, gone_copy)
        .unwrap();
    // An ordinary chat beside them.
    let ordinary = repo
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .map(|chat| Chat {
            id: ChatId::new(),
            bot: None,
            ..chat.unwrap()
        })
        .unwrap();
    repo.create_chat(&ordinary).unwrap();
    completed(&mut repo, ordinary.id, 9);
    let owner = RunOwner::on(bot.owner_device);
    for run in [kept_run, refused_run, gone_run] {
        owner.holds(run, &bot);
    }
    let (worker, client) = worker(repo, authority, owner.clone());

    let main = client.read_chat(context(true), bot.main_chat).unwrap();
    assert_eq!(named(&main), vec![(kept.assistant_message.id, kept_run)]);
    // The refused reply, the reply with no Run and the questions name none.
    for absent in [
        refused.assistant_message.id,
        plain.assistant_message.id,
        kept.user_message.id,
        gone.assistant_message.id,
    ] {
        assert!(
            main.usage
                .iter()
                .all(|e| e.message_id != absent || e.run_id.is_none())
        );
    }
    let first_read = client.read_chat(context(true), first.chat.id).unwrap();
    assert_eq!(named(&first_read), vec![(kept_copy, kept_run)]);
    // The second branch copied the whole prefix: the first reply's copy
    // names its live source's Run, the purged one's its reference; the
    // refused reply's copy names nothing.
    let second_read = client.read_chat(context(true), second.chat.id).unwrap();
    assert_eq!(
        named(&second_read),
        vec![
            (copied(&second, kept.assistant_message.id), kept_run),
            (gone_copy, gone_run)
        ]
    );
    // The round trip: every pair named is one `read_run` accepts for the
    // same caller.
    for (chat, transcript) in [
        (bot.main_chat, &main),
        (first.chat.id, &first_read),
        (second.chat.id, &second_read),
    ] {
        for (message, run) in named(transcript) {
            assert_eq!(
                view(&client, read(bot.id, chat, message, run)).header.run,
                run
            );
        }
    }
    // An ordinary chat's transcript is as it was: no Run named anywhere.
    let plain_read = client.read_chat(context(true), ordinary.id).unwrap();
    assert!(named(&plain_read).is_empty());
    assert!(
        !serde_json::to_string(&plain_read)
            .unwrap()
            .contains("run_id")
    );
    // Only the Bot's owner reads its conversation, and with it the Runs.
    assert_eq!(
        client
            .read_chat(context(false), bot.main_chat)
            .unwrap_err()
            .code,
        "not_found"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// A header that names an agent session cannot be handed out without it,
/// and cannot lose it without misreporting who asked: it is refused.
#[test]
fn a_run_whose_header_names_an_agent_session_is_refused() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let (reply, run) = replied(&mut repo, &bot, bot.main_chat, 1, bot.home_workspace);
    let owner = RunOwner::on(bot.owner_device);
    owner.holds(run, &bot);
    owner.agent_requested.store(true, Ordering::SeqCst);
    let (worker, client) = worker(repo, authority, owner.clone());
    let refused = client
        .bots(
            context(true),
            read(bot.id, bot.main_chat, reply.assistant_message.id, run),
        )
        .unwrap_err();
    assert_eq!(refused.code, "bot_run_unreadable");
    assert!(!refused.retryable);
    assert!(!refused.message.contains(&owner.session.to_string()));
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// The transcript is the person's conversation: when its Runs cannot be
/// named it is still answered, without them.
#[test]
fn a_transcript_whose_runs_cannot_be_named_is_still_answered() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let (kept, kept_run) = replied(&mut repo, &bot, bot.main_chat, 1, bot.home_workspace);
    // A reply no Run wrote: its lookup reaches the references table, which
    // this fixture breaks.
    completed(&mut repo, bot.main_chat, 3);
    repo.fixture_execute_batch("DROP TABLE bot_historical_run_refs;")
        .unwrap();
    let owner = RunOwner::on(bot.owner_device);
    owner.holds(kept_run, &bot);
    let (worker, client) = worker(repo, authority, owner);
    let transcript = client.read_chat(context(true), bot.main_chat).unwrap();
    assert_eq!(transcript.messages.len(), 4);
    assert!(named(&transcript).is_empty());
    assert!(
        transcript
            .messages
            .iter()
            .any(|m| m.id == kept.assistant_message.id)
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
