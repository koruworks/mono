// SPDX-License-Identifier: MPL-2.0
//! Real no-path Store, authority adapter and canonical worker composition.
use super::super::*;
use super::*;
use crate::bots::BotAuthorityAdapter;
use koru_delegate_api::bots::{BotCommand as D, BotDelegatePort, BotReply};
use koru_delegate_core as d;
use koru_store_server::memory::MemoryStore;
use std::sync::Mutex;
pub(in crate::worker::bots) mod gate;

struct Delegate {
    owner: d::BotOwner,
    bot: BotId,
    submissions: Mutex<Vec<d::BotSubmission>>,
    sent: SyncSender<d::BotSubmission>,
    probes: Mutex<Option<SyncSender<()>>>,
    drains: Mutex<Option<SyncSender<()>>>,
    authority_mode: std::sync::atomic::AtomicU8,
}
impl BotDelegatePort for Delegate {
    fn execute(&self, command: D) -> Result<BotReply, koru_delegate_api::Failure> {
        match command {
            D::Hello { owner, .. } => {
                assert_eq!(owner, self.owner);
                if let Some(report) = self.probes.lock().unwrap().as_ref() {
                    let _ = report.try_send(());
                }
            }
            D::PendingNativeEffects {
                owner,
                bot,
                limit,
                after,
            } => {
                assert_eq!(owner, self.owner);
                assert!(bot.is_none_or(|bot| bot == self.bot));
                assert_eq!(limit, 1);
                assert!(after.is_none());
                if let Some(report) = self.drains.lock().unwrap().as_ref() {
                    let _ = report.try_send(());
                }
                return Ok(BotReply::NativePendingEffects {
                    page: d::NativePendingEffects {
                        targets: vec![],
                        next: None,
                    },
                });
            }
            D::Barrier { barrier } => {
                assert_eq!(barrier.owner, self.owner);
                assert_eq!(barrier.bot, self.bot);
            }
            D::Reconcile { key } => {
                assert_eq!(key.owner, self.owner);
                assert_eq!(key.bot, self.bot);
                return Ok(BotReply::Missing);
            }
            D::Submit { submission } => {
                assert_eq!(submission.provenance.owner, self.owner);
                assert_eq!(submission.provenance.bot, self.bot);
                self.submissions.lock().unwrap().push(submission.clone());
                let _ = self.sent.try_send(submission);
            }
            D::ResolveRunAuthority { run, generation } => {
                assert_eq!(generation, d::AttemptGeneration::FIRST);
                let submissions = self.submissions.lock().unwrap();
                let submission = submissions.iter().find(|s| s.request.run == run).unwrap();
                let mut provenance = submission.provenance.clone();
                let mut policy = submission.policy.clone();
                match self.authority_mode.load(Ordering::SeqCst) {
                    0 => {}
                    1 => {
                        return Err(koru_delegate_api::Failure::new(
                            "fixture_unavailable",
                            "Authority unavailable",
                            true,
                        ));
                    }
                    2 => provenance.bot = BotId::mint(),
                    3 => policy.workspace = WorkspaceId::new(),
                    _ => panic!("invalid fixture mode"),
                }
                return Ok(BotReply::RunAuthority {
                    authority: d::RunToolAuthority::Bot { provenance, policy },
                });
            }
            // The team value (M8): the fixture owner takes it as a delegate does.
            D::TeamBarrier { barrier } => return Ok(BotReply::TeamBarrier { barrier }),
            other => panic!(
                "unexpected composition delegate command: {}",
                serde_json::to_string(&other).unwrap()
            ),
        }
        Err(koru_delegate_api::Failure::new(
            "fixture_unavailable",
            "No execution runtime in this fixture.",
            true,
        ))
    }
}

pub(in crate::worker::bots) struct Fixture {
    store: MemoryStore,
    pub(in crate::worker::bots) authority: Arc<BotAuthorityAdapter>,
    pub(in crate::worker::bots) bot: Bot,
    delegate: Arc<Delegate>,
    submitted: Receiver<d::BotSubmission>,
}
pub(in crate::worker::bots) struct Running {
    pub(in crate::worker::bots) worker: WorkerHandle,
    returned: Receiver<Repository>,
}
impl Running {
    pub(in crate::worker::bots) fn stop(self) -> Repository {
        self.worker.shutdown();
        assert_eq!(self.worker.join(), WorkerExit::Clean);
        self.returned.recv_timeout(Duration::from_secs(10)).unwrap()
    }
}
pub(in crate::worker::bots) fn context() -> RequestContext {
    RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "memory-composition".into(),
            scopes: vec![AiScope::Chat, AiScope::ManageBots],
        },
        trace_id: "fixture".into(),
        deadline_millis: None,
    }
}
impl Fixture {
    pub(in crate::worker::bots) fn new() -> (Self, Repository) {
        let store = MemoryStore::open("memory-worker-composition").unwrap();
        let (home, role) = store
            .with_store(|service| {
                let at = Timestamp::now();
                let profile = service
                    .create_profile("Reader", None, None, Default::default(), at)
                    .unwrap()
                    .id;
                let home = service.create_workspace(profile, "Home", at).unwrap().id;
                let (role, _) = service
                    .create_document(home, "Role", "page", None, None, at)
                    .unwrap();
                (home, role)
            })
            .unwrap();
        let authority = Arc::new(
            BotAuthorityAdapter::from_device(store.client(), &Uuid::new_v4().to_string()).unwrap(),
        );
        let (mut repository, _, mut command) = super::super::tests::fixture();
        let BotCommand::Create {
            home_workspace,
            config,
            ..
        } = &mut command
        else {
            unreachable!()
        };
        *home_workspace = home;
        config.role = BotPage {
            workspace: home,
            document: role,
        };
        let BotResponse::Bot { bot } = execute_bot_command(
            &mut repository,
            authority.clone(),
            |_| Ok(()),
            context(),
            command,
        )
        .unwrap() else {
            panic!("created Bot")
        };
        let (sent, submitted) = sync_channel(16);
        let delegate = Arc::new(Delegate {
            owner: d::BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            },
            bot: bot.id,
            submissions: Mutex::new(vec![]),
            sent,
            probes: Default::default(),
            drains: Default::default(),
            authority_mode: Default::default(),
        });
        (
            Self {
                store,
                authority,
                bot,
                delegate,
                submitted,
            },
            repository,
        )
    }
    fn start(&self, repository: Repository) -> Running {
        self.start_ports(repository, self.authority.clone(), self.delegate.clone())
    }
    pub(in crate::worker::bots) fn start_ports(
        &self,
        repository: Repository,
        authority: Arc<dyn BotAuthority>,
        delegate: Arc<dyn BotDelegatePort>,
    ) -> Running {
        self.start_observed(repository, authority, delegate, None)
    }
    pub(in crate::worker::bots) fn start_observed(
        &self,
        repository: Repository,
        authority: Arc<dyn BotAuthority>,
        delegate: Arc<dyn BotDelegatePort>,
        completion: Option<SyncSender<InferenceAttemptId>>,
    ) -> Running {
        let (send, returned) = sync_channel(1);
        let mut config = WorkerConfig::new(
            PathBuf::from("unused-memory-composition"),
            "internal-debug",
            ProviderRegistry::default(),
        )
        .with_bot_authority(authority)
        .with_bot_delegate(delegate);
        config.test_repository = Some(repository);
        config.test_repository_return = Some(send);
        config.test_bot_runtime_ready = true;
        config.test_context_completion = completion;
        Running {
            worker: AiWorker::new(config).spawn().unwrap(),
            returned,
        }
    }
    pub(in crate::worker::bots) fn owner(
        &self,
        running: &Running,
        command: BotCommand,
    ) -> ApiResult<BotResponse> {
        running.worker.client().bots(context(), command)
    }
    pub(in crate::worker::bots) fn status(&self, running: &Running) -> BotMemoryStatus {
        let BotResponse::MemoryStatus { status } = self
            .owner(
                running,
                BotCommand::ReadMemory {
                    bot_id: self.bot.id,
                },
            )
            .unwrap()
        else {
            panic!("Memory status")
        };
        status
    }
    pub(in crate::worker::bots) fn ensure(&self, running: &Running) -> (BotRequestId, BotPage) {
        let request_id = BotRequestId::mint();
        self.owner(
            running,
            BotCommand::EnsureMemory {
                request_id,
                bot_id: self.bot.id,
                expected_revision: self.bot.revision,
                title: "Memory".into(),
            },
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = self.status(running);
            if let BotMemoryStatusState::Candidate {
                state: BotMemoryState::Bound,
                candidate,
                creation_request,
                ..
            } = status.state
            {
                assert_eq!(creation_request, request_id);
                return (request_id, candidate);
            }
            assert!(Instant::now() < deadline, "Memory did not bind: {status:?}");
            std::thread::yield_now();
        }
    }
    pub(in crate::worker::bots) fn facts(
        &self,
        page: &BotPage,
        label: &str,
    ) -> (BotMemoryNodeId, koru_core::NodeId) {
        let page = page.clone();
        let label = label.to_owned();
        self.store
            .with_store(move |service| {
                let mut doc = koru_core::WorkspaceDoc::from_snapshot(
                    &service.snapshot(page.workspace, page.document).unwrap(),
                )
                .unwrap();
                let root = koru_core::schema::genesis_root_id(koru_core::DocumentId::from_uuid(
                    page.document.as_uuid(),
                ));
                let mut first = None;
                for n in 0..3 {
                    let mut node = koru_core::Node::paragraph(&format!("{label}-{n}")).unwrap();
                    node.meta.heading = Some(koru_core::Heading::H2);
                    let fact = doc.insert(koru_core::Parent::Node(root), node).unwrap();
                    let body = doc
                        .insert(
                            koru_core::Parent::Node(fact),
                            koru_core::Node::paragraph(&format!("{label}-detail-{n}").repeat(256))
                                .unwrap(),
                        )
                        .unwrap();
                    first.get_or_insert((BotMemoryNodeId::from_uuid(fact.as_uuid()), body));
                }
                service
                    .apply_update(
                        page.workspace,
                        page.document,
                        doc.export_all_updates().unwrap(),
                        Timestamp::now(),
                        None,
                    )
                    .unwrap();
                service
                    .flush(page.workspace, page.document, Timestamp::now())
                    .unwrap();
                first.unwrap()
            })
            .unwrap()
    }
    fn edit(&self, page: &BotPage, node: koru_core::NodeId, text: &str) {
        let page = page.clone();
        let text = text.to_owned();
        self.store
            .with_store(move |service| {
                let mut doc = koru_core::WorkspaceDoc::from_snapshot(
                    &service.snapshot(page.workspace, page.document).unwrap(),
                )
                .unwrap();
                doc.set_text(node, &text).unwrap();
                service
                    .apply_update(
                        page.workspace,
                        page.document,
                        doc.export_all_updates().unwrap(),
                        Timestamp::now(),
                        None,
                    )
                    .unwrap();
                service
                    .flush(page.workspace, page.document, Timestamp::now())
                    .unwrap();
            })
            .unwrap();
    }
    fn capture(&self, running: &Running) -> d::BotSubmission {
        let revision = self.status(running).revision;
        running
            .worker
            .client()
            .bots(
                context(),
                BotCommand::Start {
                    request_id: BotRequestId::mint(),
                    bot_id: self.bot.id,
                    chat_id: self.bot.main_chat,
                    expected_revision: revision,
                    destination: self.bot.home_workspace,
                    task: BotTask {
                        text: "raw task".into(),
                        documents: vec![],
                    },
                },
            )
            .unwrap();
        self.submitted
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
    }
}
fn send_owner(
    worker: &WorkerHandle,
    context: CommandContext,
    request: BotCommand,
) -> ApiResult<BotResponse> {
    let (reply, response) = sync_channel(1);
    worker
        .client()
        .send(WorkerCommand::Bots {
            context,
            request,
            reply,
        })
        .unwrap();
    response.recv_timeout(Duration::from_secs(10)).unwrap()
}
fn run_read(
    worker: &WorkerHandle,
    submission: &d::BotSubmission,
    operation: MemoryReadOperation,
) -> ApiResult<RunMemoryReadResult> {
    worker.client().read_bot_memory(RunMemoryReadRequest {
        run: submission.request.run,
        generation: d::AttemptGeneration::FIRST,
        provenance: submission.provenance.clone(),
        captured: submission.policy.clone(),
        cancellation: Cancellation::default(),
        operation,
    })
}

#[test]
fn real_memory_store_worker_creation_capture_and_historical_reads_compose() {
    let (fixture, repository) = Fixture::new();
    let running = fixture.start(repository);
    let (_, page) = fixture.ensure(&running);
    let (fact, body) = fixture.facts(&page, "original");
    let BotResponse::MemoryList {
        page:
            BotMemoryListPage::Observed {
                version: owner_version,
                continuation: Some(owner_cursor),
                items,
                ..
            },
    } = fixture
        .owner(
            &running,
            BotCommand::ListMemory {
                bot_id: fixture.bot.id,
                cursor: None,
                limit: 1,
            },
        )
        .unwrap()
    else {
        panic!("issued owner page")
    };
    assert_eq!(items[0].fact_id, fact);
    let submission = fixture.capture(&running);
    assert_eq!(submission.input.text, "raw task");
    assert!(submission.request.context.iter().all(|pin| match pin {
        d::RunContextItem::Document { document, .. }
        | d::RunContextItem::Block { document, .. } => *document != page.document,
        _ => true,
    }));
    let RunMemoryReadResult::List(BotMemoryListPage::Observed {
        continuation: Some(run_cursor),
        version: captured_version,
        ..
    }) = run_read(
        &running.worker,
        &submission,
        MemoryReadOperation::List {
            view: MemoryView::Captured,
            cursor: None,
            limit: 1,
        },
    )
    .unwrap()
    else {
        panic!("issued captured page")
    };
    fixture.edit(&page, body, "changed live detail");
    fixture.facts(&page, "later");
    let BotResponse::MemoryList {
        page: BotMemoryListPage::Observed {
            version, counts, ..
        },
    } = fixture
        .owner(
            &running,
            BotCommand::ListMemory {
                bot_id: fixture.bot.id,
                cursor: Some(owner_cursor),
                limit: 1,
            },
        )
        .unwrap()
    else {
        panic!("continued owner page")
    };
    assert_eq!(version, owner_version);
    assert_eq!(counts.valid, 3);
    let RunMemoryReadResult::List(BotMemoryListPage::Observed {
        version, counts, ..
    }) = run_read(
        &running.worker,
        &submission,
        MemoryReadOperation::List {
            view: MemoryView::Captured,
            cursor: Some(run_cursor),
            limit: 1,
        },
    )
    .unwrap()
    else {
        panic!("continued captured page")
    };
    assert_eq!(version, captured_version);
    assert_eq!(counts.valid, 3);
    let detail = run_read(
        &running.worker,
        &submission,
        MemoryReadOperation::Detail {
            view: MemoryView::Captured,
            fact_id: fact,
            cursor: None,
        },
    )
    .unwrap();
    let RunMemoryReadResult::Detail(detail) = detail else {
        panic!()
    };
    let encoded = serde_json::to_string(&detail).unwrap();
    assert!(encoded.contains("original-detail-0"));
    assert!(!encoded.contains("changed live detail"));
    let BotMemoryDetailPage::Observed {
        mut continuation, ..
    } = detail
    else {
        panic!()
    };
    let mut pages = 1;
    while let Some(cursor) = continuation {
        let RunMemoryReadResult::Detail(next) = run_read(
            &running.worker,
            &submission,
            MemoryReadOperation::Detail {
                view: MemoryView::Captured,
                fact_id: fact,
                cursor: Some(cursor),
            },
        )
        .unwrap() else {
            panic!()
        };
        assert!(serde_json::to_vec(&next).unwrap().len() <= 4096);
        assert!(
            !serde_json::to_string(&next)
                .unwrap()
                .contains("changed live detail")
        );
        let BotMemoryDetailPage::Observed {
            version,
            continuation: token,
            ..
        } = next
        else {
            panic!()
        };
        assert_eq!(version, captured_version);
        continuation = token;
        pages += 1;
        assert!(pages < 20, "detail continuation did not progress");
    }
    assert!(
        pages > 1,
        "the actual read must exercise an issued detail cursor"
    );
    let BotResponse::MemoryDetail { page: detail } = fixture
        .owner(
            &running,
            BotCommand::ReadMemoryFact {
                bot_id: fixture.bot.id,
                fact_id: fact,
                cursor: None,
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert!(
        serde_json::to_string(&detail)
            .unwrap()
            .contains("changed live detail")
    );
    let repository = running.stop();
    let capture = repository
        .bot_library_capture(
            fixture.authority.owner().unwrap(),
            fixture.bot.id,
            submission.provenance.submission,
        )
        .unwrap()
        .unwrap();
    assert!(
        matches!(capture.memory, BotCapturedMemory::Captured { version, .. } if version == captured_version)
    );
}

#[test]
fn enabled_memory_owner_dispatch_preserves_manage_permission_and_bot_identity() {
    let (fixture, repository) = Fixture::new();
    let running = fixture.start(repository);
    let command = BotCommand::ReadMemory {
        bot_id: fixture.bot.id,
    };
    assert!(matches!(
        running
            .worker
            .client()
            .bots(context(), command.clone())
            .unwrap(), BotResponse::MemoryStatus { status } if status.bot_id == fixture.bot.id
    ));
    assert!(
        running
            .worker
            .client()
            .bots(
                context(),
                BotCommand::ReadMemory {
                    bot_id: BotId::mint()
                }
            )
            .is_err()
    );
    let mut request: CommandContext = context().into();
    request.request.principal.scopes = vec![AiScope::Chat];
    assert_eq!(
        send_owner(&running.worker, request, command)
            .unwrap_err()
            .code,
        "forbidden"
    );
    running.stop();
}

#[test]
fn lost_memory_control_reply_reconciles_and_abandon_retry_retains_original_page() {
    let (fixture, repository) = Fixture::new();
    let authority = Arc::new(gate::Gate::new(fixture.authority.clone()));
    let (created, release) = authority.hold_creation();
    let running = fixture.start_ports(repository, authority, fixture.delegate.clone());
    let request_id = BotRequestId::mint();
    let command = BotCommand::EnsureMemory {
        request_id,
        bot_id: fixture.bot.id,
        expected_revision: fixture.bot.revision,
        title: "Memory".into(),
    };
    let request: CommandContext = context().into();
    let (reply, response) = sync_channel(1);
    running
        .worker
        .client()
        .send(WorkerCommand::Bots {
            context: request,
            request: command.clone(),
            reply,
        })
        .unwrap();
    drop(response); // A lost reply is not a new request identity.
    created.recv_timeout(Duration::from_secs(10)).unwrap();
    let status = fixture.status(&running);
    let BotMemoryStatusState::Candidate {
        candidate,
        state,
        observed_updated_at_ms,
        creation_request,
        ..
    } = status.state
    else {
        panic!()
    };
    assert_eq!(creation_request, request_id);
    assert_eq!(state, BotMemoryState::Prepared);
    assert_eq!(
        fixture.owner(&running, command).unwrap(),
        BotResponse::MemoryStatus {
            status: fixture.status(&running)
        }
    );
    let original = fixture
        .store
        .with_store({
            let candidate = candidate.clone();
            move |service| {
                service
                    .bounded_creation_origin(candidate.workspace, candidate.document)
                    .unwrap()
            }
        })
        .unwrap();
    let abandon = BotCommand::AbandonMemory {
        bot_id: fixture.bot.id,
        creation_request,
        observed_state: state,
        observed_updated_at_ms,
    };
    fixture.owner(&running, abandon.clone()).unwrap();
    assert_eq!(
        fixture.owner(&running, abandon.clone()).unwrap_err().code,
        "request_conflict"
    );
    release.send(()).unwrap();
    let repository = running.stop();
    assert!(
        repository
            .bot_library(fixture.authority.owner().unwrap(), fixture.bot.id)
            .unwrap()
            .binding
            .is_none()
    );
    let running = fixture.start(repository);
    let BotMemoryStatusState::Candidate {
        state,
        observed_updated_at_ms,
        candidate: retained,
        ..
    } = fixture.status(&running).state
    else {
        panic!()
    };
    assert_eq!(state, BotMemoryState::Abandoned);
    assert_eq!(retained, candidate);
    fixture
        .owner(
            &running,
            BotCommand::RetryMemory {
                bot_id: fixture.bot.id,
                creation_request,
                observed_state: state,
                observed_updated_at_ms,
            },
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if matches!(
            fixture.status(&running).state,
            BotMemoryStatusState::Candidate {
                state: BotMemoryState::Bound,
                ..
            }
        ) {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(
        fixture.owner(&running, abandon).unwrap_err().code,
        "request_conflict"
    );
    let replayed = fixture
        .store
        .with_store(move |service| {
            service
                .bounded_creation_origin(candidate.workspace, candidate.document)
                .unwrap()
        })
        .unwrap();
    assert_eq!(
        original, replayed,
        "Retry must keep the first immutable seed origin"
    );
    running.stop();
}

#[test]
fn committed_store_creation_before_ai_bind_is_reconciled_after_worker_restart() {
    let (fixture, mut repository) = Fixture::new();
    let owner = fixture.authority.owner().unwrap();
    let creation = repository
        .prepare_bot_memory_creation(
            owner,
            fixture.bot.id,
            BotRequestId::mint(),
            fixture.bot.revision,
            &crate::bots::memory_seed(fixture.bot.home_workspace, "Memory").unwrap(),
            &"a".repeat(64),
            1,
        )
        .unwrap();
    let reader = fixture
        .authority
        .workspace(fixture.bot.home_workspace)
        .unwrap();
    assert!(matches!(
        fixture
            .authority
            .prepare_memory_page(owner, reader, &creation.identity, &Cancellation::default())
            .unwrap(),
        PreparedMemoryPage::Confirmed(_)
    ));
    // Simulate the crash between independent Store and AI commits: only the
    // Store effect and the original Prepared intent survive to this worker.
    let running = fixture.start(repository);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let BotMemoryStatusState::Candidate {
            state: BotMemoryState::Bound,
            candidate,
            creation_request,
            ..
        } = fixture.status(&running).state
        {
            assert_eq!(candidate.document, creation.identity.candidate);
            assert_eq!(creation_request, creation.identity.creation_request);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let repository = running.stop();
    let before = repository.bot_library(owner, fixture.bot.id).unwrap();
    let running = fixture.start(repository);
    fixture.status(&running);
    let repository = running.stop();
    assert_eq!(
        repository.bot_library(owner, fixture.bot.id).unwrap(),
        before
    );
}

#[test]
fn actual_capture_does_not_replace_unavailable_or_mismatched_delegate_authority() {
    let (fixture, repository) = Fixture::new();
    let running = fixture.start(repository);
    let (_, page) = fixture.ensure(&running);
    fixture.facts(&page, "fact");
    let submission = fixture.capture(&running);
    for (mode, code) in [
        (1, "ai_unavailable"),
        (2, "wrong_owner"),
        (3, "wrong_owner"),
    ] {
        fixture
            .delegate
            .authority_mode
            .store(mode, Ordering::SeqCst);
        let error = run_read(
            &running.worker,
            &submission,
            MemoryReadOperation::List {
                view: MemoryView::Captured,
                cursor: None,
                limit: 1,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, code);
    }
    fixture.delegate.authority_mode.store(0, Ordering::SeqCst);
    assert!(
        run_read(
            &running.worker,
            &submission,
            MemoryReadOperation::List {
                view: MemoryView::Captured,
                cursor: None,
                limit: 1
            }
        )
        .is_ok()
    );
    running.stop();
}

#[test]
fn held_actual_memory_creation_leaves_native_drain_boot_and_preparation_responsive() {
    let (fixture, repository) = Fixture::new();
    let authority = Arc::new(gate::Gate::new(fixture.authority.clone()));
    let (created, release) = authority.hold_creation();
    let recovery = Arc::new(crate::worker::native_recovery::NativeMutationGate::closed());
    let (send, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-held-memory"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(fixture.delegate.clone())
    .with_native_effect_recovery(fixture.store.client(), recovery.clone());
    config.test_repository = Some(repository);
    config.test_repository_return = Some(send);
    config.test_bot_runtime_ready = true;
    let running = Running {
        worker: AiWorker::new(config).spawn().unwrap(),
        returned,
    };
    fixture
        .owner(
            &running,
            BotCommand::EnsureMemory {
                request_id: BotRequestId::mint(),
                bot_id: fixture.bot.id,
                expected_revision: fixture.bot.revision,
                title: "Memory".into(),
            },
        )
        .unwrap();
    created.recv_timeout(Duration::from_secs(10)).unwrap();
    let (probe, probed) = sync_channel(4);
    let (drain, drained) = sync_channel(4);
    *fixture.delegate.probes.lock().unwrap() = Some(probe);
    *fixture.delegate.drains.lock().unwrap() = Some(drain);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !recovery.ready() {
        running.worker.ping().unwrap();
        assert!(Instant::now() < deadline);
    }
    let key = d::NativeResultKey {
        run: d::RunId::mint(),
        attempt: d::RunAttemptId::mint(),
        generation: d::AttemptGeneration::FIRST,
        turn: d::AgentTurnId::mint(),
        operation_id: Uuid::new_v4(),
    };
    running
        .worker
        .client()
        .reconcile_native_operation(fixture.delegate.owner, fixture.bot.id, key)
        .unwrap();
    let BotMemoryStatusState::Candidate {
        state,
        creation_request,
        observed_updated_at_ms,
        ..
    } = fixture.status(&running).state
    else {
        panic!()
    };
    assert_eq!(state, BotMemoryState::Prepared);
    fixture
        .owner(
            &running,
            BotCommand::RetryMemory {
                bot_id: fixture.bot.id,
                creation_request,
                observed_state: state,
                observed_updated_at_ms,
            },
        )
        .unwrap();
    drained.recv_timeout(Duration::from_secs(10)).unwrap();
    probed.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(matches!(
        fixture.status(&running).state,
        BotMemoryStatusState::Candidate {
            state: BotMemoryState::Prepared,
            ..
        }
    ));
    release.send(()).unwrap();
    running.stop();
}
