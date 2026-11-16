// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::{AttemptGeneration, DocumentVersionId, RunId};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize};

struct Authority {
    base: Arc<dyn BotAuthority>,
    reads: AtomicUsize,
    revoked: AtomicBool,
    revoke_after_read: bool,
    cancel_after_read: bool,
    gate: Mutex<Option<(SyncSender<()>, Receiver<()>)>>,
}
impl BotAuthority for Authority {
    fn owner(&self) -> Result<BotOwner, BotError> {
        if self.revoked.load(Ordering::SeqCst) {
            Err(BotError::Forbidden)
        } else {
            self.base.owner()
        }
    }
    fn workspace(&self, workspace: WorkspaceId) -> Result<koru_ai_core::ProfileId, BotError> {
        self.base.workspace(workspace)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.base.page(page)
    }
    fn read_memory_page(
        &self,
        _: BotOwner,
        _: koru_ai_core::ProfileId,
        binding: &BotMemoryBinding,
        snapshot: MemorySnapshot,
        position: MemoryReadPosition,
        cancellation: &Cancellation,
    ) -> Result<MemoryPageResult, MemoryAuthorityError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if let Some((entered, release)) = self.gate.lock().unwrap().take() {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        if self.revoke_after_read {
            self.revoked.store(true, Ordering::SeqCst);
        }
        if self.cancel_after_read {
            cancellation.cancel();
        }
        let MemoryReadPosition::List { after, limit } = position else {
            panic!("list fixture");
        };
        Ok(MemoryPageResult::List {
            page: BotMemoryListPage::Observed {
                page: binding.page.clone(),
                version: match snapshot {
                    MemorySnapshot::Exact(version) => version,
                    MemorySnapshot::Current => DocumentVersionId::new(),
                },
                counts: BotMemoryCounts {
                    valid: 2,
                    omitted_from_prompt: 0,
                    invalid: 0,
                    not_indexed: 0,
                },
                items: vec![BotMemoryFactSummary {
                    fact_id: "12345678-1234-5123-8123-123456789abc".parse().unwrap(),
                    summary: "fixture fact".into(),
                }],
                complete: after != 0,
                continuation: None,
            },
            next: (after == 0).then_some(MemoryReadPosition::List { after: 1, limit }),
        })
    }
}
struct Delegate {
    request: RunMemoryReadRequest,
    calls: AtomicUsize,
    revoke_on: Option<usize>,
}
impl BotDelegatePort for Delegate {
    fn execute(&self, command: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        match &command {
            DelegateCommand::Hello { owner, .. } => {
                assert_eq!(*owner, self.request.provenance.owner);
                return Err(koru_delegate_api::Failure::new(
                    "fixture_boot_unavailable",
                    "No boot fixture.",
                    true,
                ));
            }
            DelegateCommand::Reconcile { key } => {
                assert_eq!(key.owner, self.request.provenance.owner);
                assert_eq!(key.bot, self.request.provenance.bot);
                return Err(koru_delegate_api::Failure::new(
                    "fixture_reconcile_unavailable",
                    "No execution fixture.",
                    true,
                ));
            }
            _ => {}
        }
        let DelegateCommand::ResolveRunAuthority { run, generation } = command else {
            panic!("unexpected delegate command");
        };
        assert_eq!(run, self.request.run);
        assert_eq!(generation, self.request.generation);
        let count = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.revoke_on == Some(count) {
            return Ok(BotReply::RunAuthority {
                authority: RunToolAuthority::Ordinary {},
            });
        }
        Ok(BotReply::RunAuthority {
            authority: RunToolAuthority::Bot {
                provenance: self.request.provenance.clone(),
                policy: self.request.captured.clone(),
            },
        })
    }
}
fn fixture(
    view: MemoryView,
    revoke_on: Option<usize>,
    revoke_after_read: bool,
    cancel_after_read: bool,
) -> (MemoryReadJob, Arc<Authority>, Arc<Delegate>) {
    let (job, authority, delegate, _) =
        full_fixture(view, revoke_on, revoke_after_read, cancel_after_read);
    (job, authority, delegate)
}
fn full_fixture(
    view: MemoryView,
    revoke_on: Option<usize>,
    revoke_after_read: bool,
    cancel_after_read: bool,
) -> (MemoryReadJob, Arc<Authority>, Arc<Delegate>, Repository) {
    let (mut repo, base, command) = super::super::super::tests::fixture();
    let bot = super::super::super::tests::created(&mut repo, base.clone(), command);
    let owner = base.owner().unwrap();
    let operation = MemoryReadOperation::List {
        view,
        cursor: None,
        limit: 20,
    };
    let cancellation = Cancellation::default();
    let request = RunMemoryReadRequest {
        run: RunId::mint(),
        generation: AttemptGeneration::FIRST,
        provenance: koru_delegate_core::BotProvenance {
            routine: None,
            owner: koru_delegate_core::BotOwner {
                user_scope: owner.user_scope,
                device: owner.device,
            },
            bot: bot.id,
            submission: BotRequestId::mint(),
            chat_id: bot.main_chat.as_uuid(),
            ai_attempt_id: None,
            assistant_message_id: None,
            config_revision: bot.revision,
            policy_revision: bot.policy_revision,
            continued_from: None,
        },
        captured: koru_delegate_core::CapturedBotPolicy {
            workspace: bot.home_workspace,
            destination: bot.home_workspace,
            access: koru_delegate_core::BotAccess::Default,
            selected_pages: Default::default(),
            connections: Default::default(),
        },
        cancellation: cancellation.clone(),
        operation: operation.clone(),
    };
    let capture = BotLibraryCapture {
        version: 1,
        owner,
        bot: bot.id,
        submission: request.provenance.submission,
        config_revision: bot.revision,
        knowledge: vec![],
        memory: BotCapturedMemory::Absent {},
        index_digest: *blake3::hash(&[]).as_bytes(),
    };
    let binding = BotMemoryBinding {
        owner,
        bot: bot.id,
        page: BotPage {
            workspace: bot.home_workspace,
            document: DocumentId::new(),
        },
        creation_request: BotRequestId::mint(),
        bound_at_ms: 2,
    };
    let authority = Arc::new(Authority {
        base,
        reads: AtomicUsize::new(0),
        revoked: AtomicBool::new(false),
        revoke_after_read,
        cancel_after_read,
        gate: Default::default(),
    });
    let delegate = Arc::new(Delegate {
        request: request.clone(),
        calls: AtomicUsize::new(0),
        revoke_on,
    });
    let (reply, _) = sync_channel(1);
    let job = MemoryReadJob {
        serial: 1,
        bot,
        owner,
        binding: Some(binding),
        subject: Subject::Run { request, capture },
        operation,
        cancellation,
        deadline: Instant::now() + Duration::from_secs(10),
        key: Arc::new(cursor::CursorKey::new(owner)),
        authority: authority.clone(),
        delegate: Some(delegate.clone()),
        reply: Reply::Run(reply),
    };
    (job, authority, delegate, repo)
}

#[test]
fn captured_absent_after_later_bind_reads_no_memory_and_checks_generation_twice() {
    let (job, authority, delegate) = fixture(MemoryView::Captured, None, false, false);
    let result = job.run().result.unwrap();
    assert!(matches!(
        result.page,
        MemoryPageResult::List {
            page: BotMemoryListPage::Absent {},
            ..
        }
    ));
    assert_eq!(authority.reads.load(Ordering::SeqCst), 0);
    assert_eq!(delegate.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn current_memory_after_recorded_absence_reads_later_binding() {
    let (job, authority, delegate) = fixture(MemoryView::Current, None, false, false);
    assert!(matches!(
        job.run().result.unwrap().page,
        MemoryPageResult::List {
            page: BotMemoryListPage::Observed { .. },
            ..
        }
    ));
    assert_eq!(authority.reads.load(Ordering::SeqCst), 1);
    assert_eq!(delegate.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn generation_revocation_before_and_after_read_refuses() {
    for revoked in [1, 2] {
        let (job, authority, delegate) = fixture(MemoryView::Current, Some(revoked), false, false);
        assert!(job.run().result.is_err());
        assert_eq!(authority.reads.load(Ordering::SeqCst), revoked - 1);
        assert_eq!(delegate.calls.load(Ordering::SeqCst), revoked);
    }
}

#[test]
fn owner_revocation_or_cancellation_after_decode_refuses_page() {
    for (revoked, cancelled) in [(true, false), (false, true)] {
        let (job, authority, _) = fixture(MemoryView::Current, None, revoked, cancelled);
        assert!(job.run().result.is_err());
        assert_eq!(authority.reads.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn absent_capture_requires_empty_digest_and_no_cursor() {
    let (mut job, authority, _) = fixture(MemoryView::Captured, None, false, false);
    let Subject::Run { capture, .. } = &mut job.subject else {
        unreachable!()
    };
    capture.index_digest = [1; 32];
    assert!(job.run().result.is_err());
    assert_eq!(authority.reads.load(Ordering::SeqCst), 0);
    let (mut job, _, _) = fixture(MemoryView::Captured, None, false, false);
    let MemoryReadOperation::List { cursor, .. } = &mut job.operation else {
        unreachable!()
    };
    *cursor = Some("abcd".into());
    assert!(job.run().result.is_err());
}

fn queued_fixture(legacy: bool) -> (WorkerHandle, RunMemoryReadRequest, Arc<Authority>) {
    let (job, authority, _, mut repository) = full_fixture(MemoryView::Current, None, false, false);
    let Subject::Run {
        mut request,
        capture,
    } = job.subject
    else {
        unreachable!()
    };
    let profile = authority.workspace(job.bot.home_workspace).unwrap();
    let intent = super::super::super::admission::make_intent(
        &job.bot,
        request.provenance.submission,
        job.bot.main_chat,
        job.bot.home_workspace,
        &BotTask {
            text: "task".into(),
            documents: vec![],
        },
        None,
        &[koru_delegate_core::RunContextItem::Document {
            workspace: job.bot.home_workspace,
            document: job.bot.config.role.document,
            version: DocumentVersionId::new(),
        }],
        profile,
    )
    .unwrap();
    if legacy {
        repository
            .prepare_bot_submission(&intent, &"a".repeat(64), job.bot.revision, 1)
            .unwrap();
    } else {
        repository
            .prepare_bot_submission_with_library(
                &intent,
                &capture,
                &"a".repeat(64),
                job.bot.revision,
                1,
            )
            .unwrap();
    }
    request.run = intent.submission.request.run;
    request.provenance = intent.submission.provenance;
    request.captured = intent.submission.policy;
    let creation = repository
        .prepare_bot_memory_creation(
            job.owner,
            job.bot.id,
            BotRequestId::mint(),
            job.bot.revision,
            &crate::bots::memory_seed(job.bot.home_workspace, "Memory").unwrap(),
            &"b".repeat(64),
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
    let delegate = Arc::new(Delegate {
        request: request.clone(),
        calls: AtomicUsize::new(0),
        revoke_on: None,
    });
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-queued-memory-read"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority.clone())
    .with_bot_delegate(delegate);
    config.test_repository = Some(repository);
    (AiWorker::new(config).spawn().unwrap(), request, authority)
}
fn enqueue(
    worker: &WorkerHandle,
    request: RunMemoryReadRequest,
) -> Receiver<ApiResult<RunMemoryReadResult>> {
    let (reply, response) = sync_channel(1);
    worker
        .client()
        .sender
        .send(Envelope::RunMemoryReadForFixture {
            request,
            reply,
            deadline: Instant::now() + Duration::from_secs(10),
        })
        .unwrap();
    response
}
fn answer(response: Receiver<ApiResult<RunMemoryReadResult>>) -> ApiResult<RunMemoryReadResult> {
    response.recv_timeout(Duration::from_secs(10)).unwrap()
}
fn token(result: RunMemoryReadResult) -> String {
    let RunMemoryReadResult::List(BotMemoryListPage::Observed {
        continuation: Some(token),
        ..
    }) = result
    else {
        panic!("first page must issue a continuation");
    };
    token
}

#[test]
fn queued_memory_reads_complete_in_fifo_serial_order() {
    let (worker, request, authority) = queued_fixture(false);
    let (entered, reached) = sync_channel(1);
    let (release, blocked) = sync_channel(1);
    *authority.gate.lock().unwrap() = Some((entered, blocked));
    let first = enqueue(&worker, request.clone());
    reached.recv_timeout(Duration::from_secs(10)).unwrap();
    let second = enqueue(&worker, request);
    worker.ping().unwrap();
    assert_eq!(authority.reads.load(Ordering::SeqCst), 1);
    release.send(()).unwrap();
    assert!(!token(answer(first).unwrap()).is_empty());
    assert!(!token(answer(second).unwrap()).is_empty());
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn queued_memory_key_retirement_discards_old_reply_and_token_but_new_key_succeeds() {
    let (worker, request, authority) = queued_fixture(false);
    let old_token = token(answer(enqueue(&worker, request.clone())).unwrap());
    let mut continuation = request.clone();
    let MemoryReadOperation::List { cursor, .. } = &mut continuation.operation else {
        unreachable!()
    };
    *cursor = Some(old_token.clone());
    let (entered, reached) = sync_channel(1);
    let (release, blocked) = sync_channel(1);
    *authority.gate.lock().unwrap() = Some((entered, blocked));
    let held = enqueue(&worker, continuation.clone());
    reached.recv_timeout(Duration::from_secs(10)).unwrap();
    let (reply, acknowledged) = sync_channel(1);
    worker
        .client()
        .sender
        .send(Envelope::RetireMemoryReadKeyForFixture { reply })
        .unwrap();
    acknowledged.recv_timeout(Duration::from_secs(10)).unwrap();
    release.send(()).unwrap();
    assert!(answer(held).is_err());
    let new_token = token(answer(enqueue(&worker, request)).unwrap());
    assert_ne!(new_token, old_token);
    assert_eq!(
        answer(enqueue(&worker, continuation)).unwrap_err().code,
        "invalid_memory_cursor"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn queued_legacy_run_cannot_read_current_or_captured_memory() {
    let (worker, mut request, authority) = queued_fixture(true);
    for view in [MemoryView::Current, MemoryView::Captured] {
        request.operation = MemoryReadOperation::List {
            view,
            cursor: None,
            limit: 20,
        };
        assert_eq!(
            answer(enqueue(&worker, request.clone())).unwrap_err().code,
            "memory_capture_unavailable"
        );
    }
    assert_eq!(authority.reads.load(Ordering::SeqCst), 0);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn memory_cursor_codes_match_gateway_restart_guidance() {
    assert_eq!(
        cursor_failure(cursor::CursorError::Invalid).code,
        "invalid_memory_cursor"
    );
    assert_eq!(
        cursor_failure(cursor::CursorError::Expired).code,
        "memory_cursor_expired"
    );
}
