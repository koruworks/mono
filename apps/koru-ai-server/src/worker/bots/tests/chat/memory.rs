// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::sync::atomic::AtomicUsize;

struct MemoryAuthority {
    inner: Arc<Authority>,
    captures: AtomicUsize,
    history: Mutex<Vec<(d::DocumentVersionId, Vec<u8>)>>,
    corrupt: bool,
}
impl BotAuthority for MemoryAuthority {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.inner.owner()
    }
    fn workspace(&self, id: WorkspaceId) -> Result<ProfileId, BotError> {
        self.inner.workspace(id)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.inner.page(page)
    }
    fn inspect(
        &self,
        workspaces: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<BotAuthoritySnapshot, BotError> {
        self.inner.inspect(workspaces, pages)
    }
    fn capture(
        &self,
        _: BotOwner,
        _: WorkspaceId,
        _: ProfileId,
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
    fn capture_memory(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        binding: &BotMemoryBinding,
        _: &Cancellation,
    ) -> Result<PreparedMemoryCapture, MemoryAuthorityError> {
        assert!(
            std::thread::current()
                .name()
                .unwrap_or_default()
                .starts_with("koru-ai-provider")
        );
        assert_eq!(owner, binding.owner);
        assert_eq!(self.workspace(binding.page.workspace)?, reader);
        let n = self.captures.fetch_add(1, Ordering::SeqCst) + 1;
        let bytes = format!("<memory-fixture-{n}>").into_bytes();
        let version = d::DocumentVersionId::new();
        self.history.lock().unwrap().push((version, bytes.clone()));
        Ok(PreparedMemoryCapture {
            owner,
            bot: binding.bot,
            creation_request: binding.creation_request,
            page: binding.page.clone(),
            version,
            nodes: vec![],
            omitted_count: 0,
            invalid_count: 0,
            index_digest: *blake3::hash(&bytes).as_bytes(),
        })
    }
    fn replay_memory_index(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        home: WorkspaceId,
        capture: &BotLibraryCapture,
        binding: Option<&BotMemoryBinding>,
        cancellation: &Cancellation,
    ) -> Result<VerifiedMemoryIndex, MemoryAuthorityError> {
        assert_eq!(std::thread::current().name(), Some("koru-ai-context"));
        assert!(!cancellation.is_cancelled());
        if let BotCapturedMemory::Captured { version, .. } = capture.memory {
            assert_eq!(binding.unwrap().owner, owner);
            assert_eq!(self.workspace(home)?, reader);
            let mut bytes = self
                .history
                .lock()
                .unwrap()
                .iter()
                .find(|(v, _)| *v == version)
                .ok_or(MemoryAuthorityError::HistoryUnavailable)?
                .1
                .clone();
            if self.corrupt {
                bytes.push(b'!');
            }
            Ok(VerifiedMemoryIndex {
                capture: capture.clone(),
                bytes,
            })
        } else {
            self.inner
                .replay_memory_index(owner, reader, home, capture, binding, cancellation)
        }
    }
}

struct Inputs {
    native: Native,
    inputs: Mutex<Vec<s::PendingInput>>,
    refuse_submit: AtomicBool,
}
impl BotDelegatePort for Inputs {
    fn execute(&self, command: D) -> Result<BotReply, koru_delegate_api::Failure> {
        if let D::Barrier { barrier } = &command {
            let mut state = self.native.state.lock().unwrap();
            if state.barrier.as_ref().is_some_and(|old| {
                old.epoch > barrier.epoch || (old.epoch == barrier.epoch && old != barrier)
            }) {
                return Err(koru_delegate_api::Failure::new(
                    "bot_conflict",
                    "fixture barrier conflict",
                    false,
                ));
            }
            state.barrier = Some(barrier.clone());
            return Ok(BotReply::Barrier {
                barrier: barrier.clone(),
            });
        }
        if matches!(&command, D::Submit { .. }) && self.refuse_submit.swap(false, Ordering::SeqCst)
        {
            return Err(koru_delegate_api::Failure::new(
                "bot_stale",
                "fixture stale submission",
                false,
            ));
        }
        if let D::BeginTurn { input, .. } = &command {
            self.inputs.lock().unwrap().push(input.clone());
        }
        self.native.execute(command)
    }
}
fn start(
    native: Native,
    corrupt: bool,
) -> (
    WorkerHandle,
    Bot,
    Arc<MemoryAuthority>,
    Arc<Inputs>,
    Receiver<Repository>,
) {
    let (mut repository, inner, create) = fixture();
    let mut bot = created(&mut repository, inner.clone(), create);
    let owner = inner.owner;
    let creation = repository
        .prepare_bot_memory_creation(
            owner,
            bot.id,
            BotRequestId::mint(),
            bot.revision,
            &BotMemorySeed {
                title: "Memory".into(),
                hash: [7; 32],
            },
            &"a".repeat(64),
            2,
        )
        .unwrap();
    repository
        .bind_bot_memory(
            &BotMemoryConfirmation {
                identity: creation.identity.clone(),
                original_edit_revision: vec![1],
                live_edit_revision: vec![1],
            },
            creation.observed(),
            3,
        )
        .unwrap();
    bot = repository.bot(bot.id).unwrap().unwrap();
    let authority = Arc::new(MemoryAuthority {
        inner,
        captures: AtomicUsize::new(0),
        history: Mutex::new(vec![]),
        corrupt,
    });
    let inputs = Arc::new(Inputs {
        native,
        inputs: Mutex::new(vec![]),
        refuse_submit: AtomicBool::new(false),
    });
    let (returned_tx, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-delivery"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority.clone())
    .with_bot_delegate(inputs.clone());
    config.test_repository = Some(repository);
    config.test_repository_return = Some(returned_tx);
    config.test_bot_runtime_ready = true;
    (
        AiWorker::new(config).spawn().unwrap(),
        bot,
        authority,
        inputs,
        returned,
    )
}
fn send(worker: &WorkerHandle, request: SendTurnRequest) -> AiResponseStream {
    let (reply, response) = sync_channel(1);
    worker
        .client()
        .send(WorkerCommand::SendTurn {
            context: context(true).into(),
            request,
            reply,
        })
        .unwrap();
    response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
}
fn terminal(stream: &AiResponseStream) -> PublicAiEvent {
    loop {
        let event = stream
            .events
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .event;
        if event.is_terminal() {
            return event;
        }
    }
}

#[test]
fn memory_delivery_preserves_raw_task_and_exact_retry_and_unknown_begin_input() {
    let native = Native {
        begin_failures: Mutex::new(
            [koru_delegate_api::Failure::new(
                "delegate_unavailable",
                "fixture unknown Begin",
                true,
            )]
            .into(),
        ),
        ..Native::default()
    };
    let (worker, bot, authority, inputs, returned) = start(native, false);
    let request = request(&bot);
    let stream = send(&worker, request.clone());
    assert!(matches!(terminal(&stream), PublicAiEvent::Completed { .. }));
    let replay = send(&worker, request);
    assert!(replay.accepted.replayed);
    assert_eq!(stream.accepted.attempt_id, replay.accepted.attempt_id);
    assert_eq!(authority.captures.load(Ordering::SeqCst), 1);
    let delivered = inputs.inputs.lock().unwrap();
    assert_eq!(delivered.len(), 2);
    assert_eq!(
        delivered[0], delivered[1],
        "unknown Begin resends the frozen input"
    );
    assert_eq!(delivered[0].text, "CURRENT-TASK\n\n<memory-fixture-1>");
    drop(delivered);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repo = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    let receipt = repo
        .bot_submission_for_attempt(stream.accepted.attempt_id)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.intent.submission.input.text, "CURRENT-TASK");
    assert_eq!(
        repo.message(stream.accepted.user_message_id)
            .unwrap()
            .unwrap()
            .markdown,
        "CURRENT-TASK"
    );
}

#[test]
fn memory_retry_turn_captures_current_again() {
    let (tx, interrupted) = sync_channel(4);
    let (worker, bot, authority, inputs, _) = start(
        Native {
            fail_first: true,
            interrupt_reports: Mutex::new(Some(tx)),
            ..Native::default()
        },
        false,
    );
    let first = send(&worker, request(&bot));
    assert!(matches!(terminal(&first), PublicAiEvent::Failed { .. }));
    assert!(interrupted.recv_timeout(Duration::from_secs(5)).unwrap());
    let (reply, response) = sync_channel(1);
    worker
        .client()
        .send(WorkerCommand::RetryTurn {
            context: context(true).into(),
            chat: bot.main_chat,
            request: RetryTurnRequest {
                destination_workspace: Some(bot.home_workspace),
                operation_id: Uuid::new_v4(),
                message_id: first.accepted.assistant_message_id,
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
        .unwrap();
    assert!(matches!(terminal(&retry), PublicAiEvent::Completed { .. }));
    assert_eq!(authority.captures.load(Ordering::SeqCst), 2);
    assert!(
        inputs
            .inputs
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .text
            .ends_with("<memory-fixture-2>")
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn memory_digest_mismatch_interrupts_before_any_begin() {
    let (worker, bot, _, inputs, _) = start(Native::default(), true);
    let stream = send(&worker, request(&bot));
    assert!(terminal(&stream).is_terminal());
    assert!(inputs.inputs.lock().unwrap().is_empty());
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn memory_external_shared_turn_keeps_capture_after_subscriber_drop() {
    let (tx, delivered) = sync_channel(4);
    let (worker, bot, authority, inputs, _) = start(
        Native {
            delivery_reports: Mutex::new(Some(tx)),
            ..Native::default()
        },
        false,
    );
    let stream = send(&worker, request(&bot));
    drop(stream);
    delivered.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(authority.captures.load(Ordering::SeqCst), 1);
    assert_eq!(
        inputs.inputs.lock().unwrap()[0].text,
        "CURRENT-TASK\n\n<memory-fixture-1>"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn memory_automatic_resend_captures_current_again() {
    let (tx, delivered) = sync_channel(4);
    let (worker, bot, authority, inputs, returned) = start(
        Native {
            delivery_reports: Mutex::new(Some(tx)),
            ..Native::default()
        },
        false,
    );
    inputs.refuse_submit.store(true, Ordering::SeqCst);
    let first = send(&worker, request(&bot));
    assert!(terminal(&first).is_terminal());
    delivered.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(authority.captures.load(Ordering::SeqCst), 2);
    assert_eq!(
        inputs.inputs.lock().unwrap()[0].text,
        "CURRENT-TASK\n\n<memory-fixture-2>"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repo = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(repo.attempts_for_chat(bot.main_chat).unwrap().len(), 2);
}
