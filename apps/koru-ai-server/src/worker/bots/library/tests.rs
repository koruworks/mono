// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::bots::replay::BotChatPreparation;
use koru_ai_core::{AiSurface, ProfileId};
use koru_delegate_core as d;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn fixture() -> (Repository, Bot, Arc<dyn BotAuthority>) {
    let (mut repo, authority, command) = super::super::tests::fixture();
    let bot = super::super::tests::created(&mut repo, authority.clone(), command);
    (repo, bot, authority)
}
fn context(manage: bool) -> RequestContext {
    RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "caller-chosen".into(),
            scopes: if manage {
                vec![AiScope::Chat, AiScope::ManageBots]
            } else {
                vec![AiScope::Chat]
            },
        },
        trace_id: "library-fixture".into(),
        deadline_millis: None,
    }
}
fn page(bot: &Bot) -> BotPage {
    BotPage {
        workspace: bot.home_workspace,
        document: DocumentId::new(),
    }
}
fn pins(sources: &LibrarySources) -> Vec<d::RunContextItem> {
    sources
        .pages()
        .into_iter()
        .map(|page| d::RunContextItem::Document {
            workspace: page.workspace,
            document: page.document,
            version: d::DocumentVersionId::new(),
        })
        .collect()
}

fn send_request(bot: &Bot, explicit_refs: Vec<ContextRef>, replace: bool) -> SendTurnRequest {
    SendTurnRequest {
        destination_workspace: Some(bot.home_workspace),
        operation_id: Uuid::new_v4(),
        chat_id: bot.main_chat,
        surface: AiSurface::Standalone,
        markdown: "Question".into(),
        explicit_refs,
        web_search: false,
        reasoning_override: None,
        system_prompt_override: None,
        replace_pinned_documents: replace,
        attachment_ids: vec![],
        answers: None,
        skills: vec![],
    }
}

#[test]
fn shared_chat_preserves_task_pin_replace_semantics_with_knowledge() {
    use koru_ai_core::{ContextCommit, PinnedDocument, RequestMeasurements};
    let (mut repo, bot, _) = fixture();
    let knowledge = page(&bot);
    let retained = page(&bot);
    let explicit = page(&bot);
    let bot = set(&mut repo, &bot, vec![knowledge.clone()]);
    let turn = super::super::tests::turn(bot.main_chat, Uuid::new_v4(), 1);
    repo.begin_turn(&turn).unwrap();
    repo.commit_context(&ContextCommit {
        attempt_id: turn.attempt.id,
        started_at: turn.attempt.queued_at,
        completed_at: turn.attempt.queued_at,
        measurements: RequestMeasurements::default(),
        pinned_documents: vec![PinnedDocument {
            chat_id: bot.main_chat,
            document_id: retained.document,
            display_title: "retained".into(),
            pinned: true,
            first_used_at: turn.attempt.queued_at,
            last_used_at: turn.attempt.queued_at,
        }],
        document_refs: vec![],
        message_block_refs: vec![],
        replace_pinned_documents: true,
    })
    .unwrap();
    for replace in [false, true] {
        let (reply, _) = sync_channel(1);
        let command = WorkerCommand::SendTurn {
            context: context(true).into(),
            request: send_request(
                &bot,
                vec![ContextRef::Document {
                    document_id: explicit.document,
                }],
                replace,
            ),
            reply,
        };
        let captured = super::super::chat::chat_capture(&repo, &command)
            .unwrap()
            .unwrap();
        assert_eq!(captured.library_sources.knowledge, vec![knowledge.clone()]);
        let expected = if replace {
            vec![explicit.clone()]
        } else {
            vec![explicit.clone(), retained.clone()]
        };
        assert_eq!(captured.library_sources.task_documents, expected);
        assert_eq!(captured.pages.len(), if replace { 3 } else { 4 });
        assert_eq!(repo.pinned_documents(bot.main_chat).unwrap().len(), 1);
    }
}

#[test]
fn generic_history_cleanup_preserves_bot_library_and_exact_captures() {
    let (mut repo, bot, authority) = fixture();
    let bot = set(&mut repo, &bot, vec![page(&bot)]);
    let sources = LibrarySources::read(&repo, &bot, vec![]).unwrap();
    let turn = super::super::tests::turn(bot.main_chat, Uuid::new_v4(), 1);
    let task = BotTask {
        text: turn.user_message.markdown.clone(),
        documents: vec![],
    };
    let mut intent = super::super::admission::make_library_intent(
        &bot,
        BotRequestId::mint(),
        bot.main_chat,
        bot.home_workspace,
        &task,
        None,
        &pins(&sources),
        &sources,
        ProfileId::new(),
    )
    .unwrap();
    intent.submission.provenance.ai_attempt_id = Some(turn.attempt.id.as_uuid());
    intent.submission.provenance.assistant_message_id = Some(turn.assistant_message.id.as_uuid());
    let capture = sources.capture(&intent).unwrap();
    repo.prepare_bot_chat_with_library(
        &BotChatPreparation::Turn(turn.clone()),
        &intent,
        &capture,
        HASH,
        bot.revision,
        turn.attempt.queued_at,
    )
    .unwrap();
    let refused = repo
        .prepare_bot_refusal_fence(
            capture.owner,
            bot.id,
            capture.submission,
            BotAdmissionRefusal::Rejected,
            false,
            3,
        )
        .unwrap();
    let Some(BotExecutionOutcome::RefusalFence { round, barrier, .. }) = refused.outcome else {
        panic!("fence")
    };
    repo.acknowledge_bot_refusal_fence(
        capture.owner,
        bot.id,
        capture.submission,
        round,
        &barrier,
        4,
    )
    .unwrap();
    repo.reject_bot_submission_after_fence(
        capture.owner,
        bot.id,
        capture.submission,
        round,
        &barrier,
        5,
    )
    .unwrap();
    repo.interrupt_bot_preparation(turn.attempt.id, Timestamp::now())
        .unwrap();
    let mut ordinary = repo
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    ordinary.id = ChatId::new();
    repo.create_chat(&ordinary).unwrap();
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-library-cleanup-fixture"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority);
    config.test_repository = Some(repo);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    assert_eq!(
        client.clear_history(context(true)).unwrap().chats_deleted,
        1
    );
    let mut usage = context(true);
    usage.principal.scopes.push(AiScope::ReadUsage);
    client.clear_usage(usage).unwrap();
    assert_eq!(
        client
            .read_chat(context(true), bot.main_chat)
            .unwrap()
            .messages
            .len(),
        2
    );
    let BotResponse::Library { library } = client
        .bots(context(true), BotCommand::ReadLibrary { bot_id: bot.id })
        .unwrap()
    else {
        panic!("library")
    };
    assert_eq!(library.knowledge, sources.knowledge);
    let BotResponse::TaskAnswer { answer } = client
        .bots(
            context(true),
            BotCommand::ReadTaskAnswer {
                bot_id: bot.id,
                chat_id: bot.main_chat,
                attempt_id: turn.attempt.id,
                operation_id: turn.attempt.operation_id,
                offset: 0,
            },
        )
        .unwrap()
    else {
        panic!("answer")
    };
    assert_eq!(answer.run_id, Some(intent.submission.request.run));
    assert!(answer.status.is_terminal());
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
fn set(repo: &mut Repository, bot: &Bot, pages: Vec<BotPage>) -> Bot {
    repo.set_bot_knowledge(
        &PreparedBotKnowledge {
            owner: BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            },
            bot: bot.id,
            request: BotRequestId::mint(),
            expected_revision: bot.revision,
            pages,
        },
        HASH,
        2,
    )
    .unwrap();
    repo.bot(bot.id).unwrap().unwrap()
}

#[test]
fn library_capture_union_is_exact_and_overlap_materializes_once() {
    let (mut repo, bot, _) = fixture();
    let shared = page(&bot);
    let bot = set(
        &mut repo,
        &bot,
        vec![bot.config.role.clone(), shared.clone()],
    );
    let task = BotTask {
        text: "task".into(),
        documents: vec![shared],
    };
    let sources = LibrarySources::read(&repo, &bot, task.documents.clone()).unwrap();
    let exact = pins(&sources);
    assert_eq!(exact.len(), 2);
    sources.validate(&bot, &task, &exact).unwrap();
    assert!(sources.validate(&bot, &task, &exact[..1]).is_err());
    let mut duplicate = exact.clone();
    duplicate.push(exact[0].clone());
    assert!(sources.validate(&bot, &task, &duplicate).is_err());
    let mut extra = exact.clone();
    extra.push(d::RunContextItem::Document {
        workspace: bot.home_workspace,
        document: DocumentId::new(),
        version: d::DocumentVersionId::new(),
    });
    assert!(sources.validate(&bot, &task, &extra).is_err());
    let intent = super::super::admission::make_library_intent(
        &bot,
        BotRequestId::mint(),
        bot.main_chat,
        bot.home_workspace,
        &task,
        None,
        &exact,
        &sources,
        ProfileId::new(),
    )
    .unwrap();
    let capture = sources.capture(&intent).unwrap();
    assert_eq!(capture.knowledge.len(), 2);
    assert_eq!(capture.index_digest, *blake3::hash(&[]).as_bytes());
    assert_eq!(capture.memory, BotCapturedMemory::Absent {});
    assert!(intent.submission.policy.selected_pages.is_empty());
}

#[test]
fn sixteen_knowledge_and_sixteen_task_pages_capture_thirty_three_pins() {
    let (mut repo, bot, _) = fixture();
    let knowledge = (0..16).map(|_| page(&bot)).collect();
    let bot = set(&mut repo, &bot, knowledge);
    let task = BotTask {
        text: "task".into(),
        documents: (0..16).map(|_| page(&bot)).collect(),
    };
    let sources = LibrarySources::read(&repo, &bot, task.documents.clone()).unwrap();
    let exact = pins(&sources);
    assert_eq!(exact.len(), 33);
    sources.validate(&bot, &task, &exact).unwrap();
    let mut too_many = task.documents.clone();
    too_many.push(page(&bot));
    assert!(LibrarySources::read(&repo, &bot, too_many).is_err());
}

#[test]
fn unexpected_memory_binding_refuses_new_task_without_false_absent_capture() {
    let (mut repo, bot, _) = fixture();
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let creation = repo
        .prepare_bot_memory_creation(
            owner,
            bot.id,
            BotRequestId::mint(),
            bot.revision,
            &BotMemorySeed {
                title: "Memory".into(),
                hash: [7; 32],
            },
            HASH,
            2,
        )
        .unwrap();
    repo.bind_bot_memory(
        &BotMemoryConfirmation {
            identity: creation.identity,
            original_edit_revision: vec![1],
            live_edit_revision: vec![1],
        },
        BotMemoryState::Prepared,
        3,
    )
    .unwrap();
    let bot = repo.bot(bot.id).unwrap().unwrap();
    assert_eq!(
        LibrarySources::read(&repo, &bot, vec![])
            .err()
            .unwrap()
            .code,
        "unsupported_capability"
    );
}

struct Pages {
    inner: Arc<dyn BotAuthority>,
    available: AtomicBool,
    inspected: AtomicUsize,
}

struct CaptureGate {
    inner: Arc<dyn BotAuthority>,
    gate: std::sync::Mutex<Option<(SyncSender<()>, Receiver<()>)>>,
}
impl BotAuthority for CaptureGate {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.inner.owner()
    }
    fn workspace(&self, id: WorkspaceId) -> Result<ProfileId, BotError> {
        self.inner.workspace(id)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.inner.page(page)
    }
    fn capture(
        &self,
        _: BotOwner,
        _: WorkspaceId,
        _: ProfileId,
        pages: &[BotPage],
    ) -> Result<Vec<d::RunContextItem>, BotError> {
        if let Some((entered, release)) = self.gate.lock().unwrap().take() {
            entered.send(()).unwrap();
            release.recv().unwrap();
        }
        Ok(pages
            .iter()
            .map(|page| d::RunContextItem::Document {
                workspace: page.workspace,
                document: page.document,
                version: d::DocumentVersionId::new(),
            })
            .collect())
    }
}

#[test]
fn knowledge_changed_during_capture_refuses_without_partial_turn() {
    let (repo, bot, authority) = fixture();
    let (entered_tx, entered) = sync_channel(1);
    let (release, released) = sync_channel(1);
    let authority = Arc::new(CaptureGate {
        inner: authority,
        gate: std::sync::Mutex::new(Some((entered_tx, released))),
    });
    let edit_page = page(&bot);
    // A second command already prepared under this revision may finish while
    // capture waits on Store. This is exactly its current owner/page proof.
    let edit_proof = CommandContext {
        request: context(true),
        prepared: Some(Arc::new(PreparedBotCommand {
            authority: Arc::new(
                authority
                    .inspect(&[bot.home_workspace], &[edit_page.clone()])
                    .unwrap(),
            ),
            chats: vec![(bot.id, bot.main_chat, bot.revision, bot.policy_revision)],
            runtime: Ok(()),
            pins: vec![],
            library_sources: None,
            chat_snapshot: None,
            settings: None,
            terminal_question: None,
        })),
    };
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-library-race-fixture"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority);
    config.test_repository = Some(repo);
    config.test_bot_runtime_ready = true;
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::SendTurn {
            context: context(true).into(),
            request: send_request(&bot, vec![], false),
            reply,
        })
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let (reply, saved) = sync_channel(1);
    client
        .send(WorkerCommand::Bots {
            context: edit_proof,
            request: BotCommand::SetKnowledge {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                expected_revision: bot.revision,
                pages: vec![edit_page],
            },
            reply,
        })
        .unwrap();
    assert!(matches!(
        saved.recv_timeout(Duration::from_secs(5)).unwrap().unwrap(),
        BotResponse::KnowledgeSaved { .. }
    ));
    release.send(()).unwrap();
    assert_eq!(
        response
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .err()
            .unwrap()
            .code,
        BotError::RevisionConflict.code()
    );
    assert!(
        client
            .read_chat(context(true), bot.main_chat)
            .unwrap()
            .messages
            .is_empty()
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
impl BotAuthority for Pages {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.inner.owner()
    }
    fn workspace(&self, id: WorkspaceId) -> Result<ProfileId, BotError> {
        self.inner.workspace(id)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.inspected.fetch_add(1, Ordering::SeqCst);
        if !self.available.load(Ordering::SeqCst) {
            return Err(BotError::Forbidden);
        }
        self.inner.page(page)
    }
    fn capture(
        &self,
        _: BotOwner,
        _: WorkspaceId,
        _: ProfileId,
        _: &[BotPage],
    ) -> Result<Vec<d::RunContextItem>, BotError> {
        panic!("Knowledge configuration must never capture content")
    }
}

#[test]
fn library_commands_require_owner_and_preserve_retry_after_page_loss() {
    let (repo, bot, inner) = fixture();
    let authority = Arc::new(Pages {
        inner,
        available: AtomicBool::new(true),
        inspected: AtomicUsize::new(0),
    });
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-library-command-fixture"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority.clone());
    config.test_repository = Some(repo);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    let read = BotCommand::ReadLibrary { bot_id: bot.id };
    assert!(client.bots(context(false), read.clone()).is_err());
    let selected = page(&bot);
    let command = BotCommand::SetKnowledge {
        request_id: BotRequestId::mint(),
        bot_id: bot.id,
        expected_revision: bot.revision,
        pages: vec![selected.clone()],
    };
    assert!(client.bots(context(false), command.clone()).is_err());
    let saved = client.bots(context(true), command.clone()).unwrap();
    let BotResponse::KnowledgeSaved { receipt } = &saved else {
        panic!("receipt")
    };
    assert_eq!(receipt.revision, bot.revision + 1);
    assert_eq!(receipt.pages, vec![selected]);
    authority.available.store(false, Ordering::SeqCst);
    let inspected = authority.inspected.load(Ordering::SeqCst);
    assert_eq!(client.bots(context(true), command.clone()).unwrap(), saved);
    assert_eq!(authority.inspected.load(Ordering::SeqCst), inspected);
    let BotResponse::Library { library } = client.bots(context(true), read).unwrap() else {
        panic!("library")
    };
    assert_eq!(library.knowledge, receipt.pages);
    let mut changed = command;
    if let BotCommand::SetKnowledge { pages, .. } = &mut changed {
        pages.clear();
    }
    assert_eq!(
        client.bots(context(true), changed).unwrap_err().code,
        "request_conflict"
    );
    assert!(
        client
            .bots(
                context(true),
                BotCommand::SetKnowledge {
                    request_id: BotRequestId::mint(),
                    bot_id: bot.id,
                    expected_revision: library.revision,
                    pages: receipt.pages.clone()
                }
            )
            .is_err()
    );
    let BotResponse::Bot { bot: current } = client
        .bots(context(true), BotCommand::Read { bot_id: bot.id })
        .unwrap()
    else {
        panic!("bot")
    };
    assert_eq!(current.policy_revision, bot.policy_revision);
    assert_eq!(current.selected_pages, bot.selected_pages);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
