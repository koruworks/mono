// SPDX-License-Identifier: MPL-2.0
//! A Send whose receipt reaches its owner while the Send is still in its
//! context window (receipt prepared, attempt Pending, no live task yet), and
//! is admitted there, then captured by an Archive, a Bot's Trash or its
//! conversation's Trash: the operation must stop it as the person's Stop
//! before it completes, and the Send must never start through the ordinary
//! provider route (the third lifecycle review's proof, kept).
use super::trash::{api_task_in, restore_chat_at, trash_chat_at};
use super::unadmitted::{attempt_status, send_in};
use super::*;

#[derive(Default)]
struct Gate {
    gate: Mutex<Option<(SyncSender<()>, Receiver<()>)>>,
}
impl StoreContextPort for Gate {
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
        unreachable!()
    }
}

fn hold(client: &AiWorkerClient, reader: &Gate) -> SyncSender<()> {
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

fn second_bot(repository: &mut Repository, authority: Arc<Authority>, create: &BotCommand) -> Bot {
    let BotCommand::Create {
        home_workspace,
        mut config,
        access,
        selected_pages,
        ..
    } = create.clone()
    else {
        panic!("create")
    };
    config.name = "Second".into();
    config.working_folder = if cfg!(windows) {
        "D:/isolated-two"
    } else {
        "/isolated-two"
    }
    .into();
    config.role.document = DocumentId::new();
    created(
        repository,
        authority,
        BotCommand::Create {
            request_id: BotRequestId::mint(),
            bot_id: BotId::mint(),
            home_workspace,
            config,
            access,
            selected_pages,
        },
    )
}

/// The ordinary provider route for the Bot's agent provider id: in
/// production `AgentSessions` inserts every agent provider into this same
/// registry. It records each start and refuses it.
#[derive(Default)]
struct Recorder {
    started: Mutex<Vec<InferenceAttemptId>>,
}
impl koru_ai_core::AiProvider for Recorder {
    fn start(
        &self,
        request: koru_ai_core::AiRequest,
        _cancellation: koru_ai_core::Cancellation,
    ) -> Result<koru_ai_core::ProviderStream, AiError> {
        self.started.lock().unwrap().push(request.attempt_id);
        Err(AiError::new(
            koru_ai_core::AiErrorClass::Provider,
            "review3 recorder",
            false,
        ))
    }
}

fn spawn_recording(
    repository: Repository,
    authority: Arc<Authority>,
    port: Arc<Archiving>,
    reader: Arc<Gate>,
    recorder: Arc<Recorder>,
) -> (WorkerHandle, Receiver<Repository>) {
    let (return_tx, returned) = sync_channel(1);
    let mut providers = ProviderRegistry::default();
    providers.insert(AiProviderId::parse("agent-codex").unwrap(), recorder);
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(koru_ai_provider::DeterministicMockProvider::default()),
    );
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-lifecycle"),
        "internal-debug",
        providers,
    )
    .with_bot_authority(Arc::new(StoreSpy {
        inner: authority,
        port: port.clone(),
    }))
    .with_bot_delegate(port)
    .with_store_context(reader);
    config.test_repository = Some(repository);
    config.test_repository_return = Some(return_tx);
    (AiWorker::new(config).spawn().unwrap(), returned)
}

fn run(trash_chat: bool, whole: bool) {
    eprintln!("REVIEW3 case trash_chat={trash_chat} whole_trash={whole}");
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create.clone());
    let other = second_bot(&mut repository, authority.clone(), &create);
    // Another Bot's start stays owed (admitted, answered context
    // unavailable, its refusal fence's barrier refused), so the receipt
    // recovery pass keeps running again on its backoff (AdmissionRetry).
    let mut owed = api_task_in(
        &mut repository,
        &other,
        other.main_chat,
        d::BotAdmissionState::Queued,
        0,
    );
    owed.admitted = false;
    owed.context_unavailable = true;
    let chat = repository
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    let reader = Arc::new(Gate::default());
    let port = Arc::new(Archiving::default());
    port.api.lock().unwrap().push(owed);
    port.refuse_barriers.store(true, Ordering::SeqCst);
    let recorder = Arc::new(Recorder::default());
    let (worker, returned) = spawn_recording(
        repository,
        authority.clone(),
        port.clone(),
        reader.clone(),
        recorder.clone(),
    );
    let client = worker.client();
    let release = hold(&client, &reader);
    let accepted = send_in(
        &client,
        proof(authority.clone(), &bot, chat),
        &bot,
        bot.main_chat,
    );
    // The receipt recovery pass, kept running by the other Bot's owed
    // start, reaches the Send's receipt while the Send is in its context
    // window. It must leave it to the Send's own path: never admitted
    // before its live task exists (which is what let it outlive the
    // operation before).
    until("the recovery pass reached the Send's receipt", || {
        port.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, name, key)| *name == "reconcile" && key.is_some())
            .count()
            >= 3
    });
    assert!(
        port.native.state.lock().unwrap().submission.is_none(),
        "the Send was admitted while still in its context window"
    );
    let before = attempt_status(&client, accepted.attempt_id);
    eprintln!("REVIEW3 trash_chat={trash_chat}: admitted while attempt is {before:?}");
    assert_eq!(before, AttemptStatus::Pending);
    port.refuse_barriers.store(false, Ordering::SeqCst);
    let request = BotRequestId::mint();
    if trash_chat {
        trash_chat_at(&client, &bot, bot.main_chat, request, bot.revision).unwrap();
    } else if whole {
        super::trash::trash_at(&client, &bot, request, bot.revision).unwrap();
    } else {
        archive_at(&client, &bot, request, bot.revision).unwrap();
    }
    let done = settled(&client, bot.id, request);
    eprintln!(
        "REVIEW3 trash_chat={trash_chat}: operation {:?}",
        done.stage
    );
    let at_completion = if trash_chat || whole {
        None
    } else {
        Some(attempt_status(&client, accepted.attempt_id))
    };
    eprintln!("REVIEW3 trash_chat={trash_chat}: attempt at completion {at_completion:?}");
    release.send(()).unwrap();
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(500));
        eprintln!(
            "REVIEW3 trash_chat={trash_chat}: ordinary provider starts for the Bot attempt {}",
            recorder
                .started
                .lock()
                .unwrap()
                .iter()
                .filter(|a| **a == accepted.attempt_id)
                .count()
        );
    }
    let ordinary_starts = recorder
        .started
        .lock()
        .unwrap()
        .iter()
        .filter(|a| **a == accepted.attempt_id)
        .count();
    if trash_chat {
        let revision = read_bot(&client, bot.id).revision;
        restore_chat_at(&client, &bot, bot.main_chat, request, revision).unwrap();
    } else if whole {
        let revision = super::trash::trashed_revision(&client, &bot);
        super::trash::restore_at(&client, &bot, request, revision).unwrap();
    }
    let after = attempt_status(&client, accepted.attempt_id);
    eprintln!(
        "REVIEW3 trash_chat={trash_chat}: attempt after the context job {after:?}, begins {}",
        port.native.state.lock().unwrap().begin
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    let by_person = stopped_by_the_person(&repository, accepted.attempt_id);
    eprintln!("REVIEW3 trash_chat={trash_chat}: stopped by the person {by_person}");
    // The design: the operation stops its subject's work as the person's
    // Stop before it completes.
    assert_eq!(done.stage, BotLifecycleStage::Completed);
    assert_eq!(
        ordinary_starts, 0,
        "the put-away Bot's task was started through the ordinary provider route"
    );
    if let Some(status) = at_completion {
        assert!(
            status.is_terminal(),
            "Archive completed while its conversation's attempt was {status:?}"
        );
    }
    assert!(
        by_person,
        "the Send ended {after:?}, not as the person's Stop (trash_chat={trash_chat})"
    );
}

#[test]
fn archive_with_an_early_admitted_send_in_its_context_window() {
    run(false, false);
}

#[test]
fn conversation_trash_with_an_early_admitted_send_in_its_context_window() {
    run(true, false);
}

#[test]
fn bot_trash_with_an_early_admitted_send_in_its_context_window() {
    run(false, true);
}
