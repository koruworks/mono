// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::bots::replay::BotChatPreparation;
use koru_store_core::ports::BoundedDocumentVersion;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

struct Authority {
    base: Arc<dyn BotAuthority>,
    evidence: Mutex<Option<BoundedDocumentVersion>>,
    reads: AtomicUsize,
    revoked: AtomicBool,
    gate: Mutex<Option<(SyncSender<()>, Receiver<()>)>>,
    creation_gate: Mutex<Option<(SyncSender<()>, Receiver<()>)>>,
}
impl BotAuthority for Authority {
    fn prepare_memory_page(
        &self,
        _: BotOwner,
        _: koru_ai_core::ProfileId,
        _: &BotMemoryIdentity,
        _: &Cancellation,
    ) -> Result<PreparedMemoryPage, MemoryAuthorityError> {
        let gate = self.creation_gate.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        Err(BotError::Unavailable.into())
    }
    fn owner(&self) -> Result<BotOwner, BotError> {
        if self.revoked.load(Ordering::SeqCst) {
            Err(BotError::Forbidden)
        } else {
            self.base.owner()
        }
    }
    fn workspace(&self, home: WorkspaceId) -> Result<koru_ai_core::ProfileId, BotError> {
        self.base.workspace(home)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.base.page(page)
    }
    fn inspect(
        &self,
        homes: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<BotAuthoritySnapshot, BotError> {
        let gate = self.gate.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        Ok(BotAuthoritySnapshot {
            owner: self.owner()?,
            workspaces: homes
                .iter()
                .map(|home| (*home, self.workspace(*home)))
                .collect(),
            pages: pages
                .iter()
                .map(|page| (page.clone(), self.page(page)))
                .collect(),
        })
    }
    fn memory_edit_history(
        &self,
        owner: BotOwner,
        reader: koru_ai_core::ProfileId,
        binding: &BotMemoryBinding,
        version: DocumentVersionId,
        _: &Cancellation,
    ) -> Result<BoundedDocumentVersion, MemoryAuthorityError> {
        assert_eq!(owner, self.base.owner().unwrap());
        assert_eq!(reader, self.base.workspace(binding.page.workspace).unwrap());
        self.reads.fetch_add(1, Ordering::SeqCst);
        let value = self
            .evidence
            .lock()
            .unwrap()
            .clone()
            .ok_or(MemoryAuthorityError::HistoryUnavailable)?;
        if value.id.as_uuid() != version.as_uuid() {
            return Err(MemoryAuthorityError::HistoryUnavailable);
        }
        Ok(value)
    }
    fn memory_original_seed_history(
        &self,
        owner: BotOwner,
        reader: koru_ai_core::ProfileId,
        identity: &BotMemoryIdentity,
        binding: &BotMemoryBinding,
        cancel: &Cancellation,
    ) -> Result<BoundedDocumentVersion, MemoryAuthorityError> {
        assert_eq!(identity.candidate, binding.page.document);
        let version = DocumentVersionId::from_uuid(
            self.evidence.lock().unwrap().as_ref().unwrap().id.as_uuid(),
        );
        self.memory_edit_history(owner, reader, binding, version, cancel)
    }
}
struct Delegate {
    request: RunMemoryWriteSubjectRequest,
    session: koru_delegate_core::SessionId,
    allocation: Mutex<Option<MemoryEditAllocation>>,
    calls: AtomicUsize,
    stopped: AtomicBool,
}
impl BotDelegatePort for Delegate {
    fn execute(&self, command: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        let refused = || {
            koru_delegate_api::Failure::new("fixture_unavailable", "No fixture execution.", true)
        };
        match command {
            DelegateCommand::Hello { owner, .. } => {
                assert_eq!(owner, self.request.provenance.owner);
                Err(refused())
            }
            DelegateCommand::ResolveRunAuthority { run, generation } => {
                self.calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(run, self.request.run);
                assert_eq!(generation, self.request.generation);
                Ok(BotReply::RunAuthority {
                    authority: RunToolAuthority::Bot {
                        provenance: self.request.provenance.clone(),
                        policy: self.request.captured.clone(),
                    },
                })
            }
            DelegateCommand::ReadMemoryEditAllocation {
                run,
                operation_id,
                caller_fingerprint,
            } => {
                let allocation = self.allocation.lock().unwrap().clone().unwrap();
                assert_eq!(
                    (run, operation_id, caller_fingerprint),
                    (
                        allocation.key.run,
                        allocation.key.operation_id,
                        allocation.caller_fingerprint
                    )
                );
                Ok(BotReply::MemoryEditAllocation { allocation })
            }
            DelegateCommand::Reconcile { key } => {
                assert_eq!(key.owner, self.request.provenance.owner);
                let Some(allocation) = self.allocation.lock().unwrap().clone() else {
                    return Err(refused());
                };
                let fence = koru_delegate_core::BotExecutionFence {
                    owner: key.owner,
                    bot: key.bot,
                    run: allocation.key.run,
                    attempt: allocation.key.attempt,
                    generation: allocation.key.generation,
                    session: self.session,
                    turn: Some(allocation.key.turn),
                };
                Ok(BotReply::Admission {
                    admission: koru_delegate_core::BotAdmissionView {
                        key,
                        run: allocation.key.run,
                        state: koru_delegate_core::BotAdmissionState::Executing,
                        queue_sequence: 1,
                        fence: Some(fence),
                    },
                })
            }
            DelegateCommand::EffectFence { fence } => {
                if self.stopped.load(Ordering::SeqCst) {
                    Err(refused())
                } else {
                    Ok(BotReply::EffectFence {
                        fence,
                        policy: self.request.captured.clone(),
                    })
                }
            }
            // The team value (M8): the fixture owner takes it as a delegate does.
            DelegateCommand::TeamBarrier { barrier } => Ok(BotReply::TeamBarrier { barrier }),
            other => panic!("unexpected private command: {other:?}"),
        }
    }
}
struct Fixture {
    repo: Repository,
    bot: Bot,
    request: RunMemoryWriteSubjectRequest,
    authority: Arc<Authority>,
    delegate: Arc<Delegate>,
    binding: Option<BotMemoryBinding>,
}
impl Fixture {
    fn new(bound: bool) -> Self {
        Self::new_kind(bound, false)
    }
    fn new_kind(bound: bool, shared: bool) -> Self {
        let (mut repo, base, command) = super::super::super::tests::fixture();
        let bot = super::super::super::tests::created(&mut repo, base.clone(), command);
        let owner = base.owner().unwrap();
        let request_id = BotRequestId::mint();
        let mut intent = super::super::super::admission::make_intent(
            &bot,
            request_id,
            bot.main_chat,
            bot.home_workspace,
            &BotTask {
                text: "Remember a fact".into(),
                documents: vec![],
            },
            None,
            &[koru_delegate_core::RunContextItem::Document {
                workspace: bot.home_workspace,
                document: bot.config.role.document,
                version: DocumentVersionId::new(),
            }],
            base.workspace(bot.home_workspace).unwrap(),
        )
        .unwrap();
        let capture = BotLibraryCapture {
            version: 1,
            owner,
            bot: bot.id,
            submission: request_id,
            config_revision: bot.revision,
            knowledge: vec![],
            memory: BotCapturedMemory::Absent {},
            index_digest: *blake3::hash(&[]).as_bytes(),
        };
        if shared {
            let mut turn = super::super::super::tests::turn(bot.main_chat, Uuid::new_v4(), 1);
            turn.user_message.markdown = "Remember a fact".into();
            intent.submission.provenance.ai_attempt_id = Some(turn.attempt.id.as_uuid());
            intent.submission.provenance.assistant_message_id =
                Some(turn.assistant_message.id.as_uuid());
            repo.prepare_bot_chat_with_library(
                &BotChatPreparation::Turn(turn.clone()),
                &intent,
                &capture,
                &"a".repeat(64),
                bot.revision,
                turn.attempt.queued_at,
            )
            .unwrap();
        } else {
            repo.prepare_bot_submission_with_library(
                &intent,
                &capture,
                &"a".repeat(64),
                bot.revision,
                1,
            )
            .unwrap();
        }
        let request = RunMemoryWriteSubjectRequest {
            run: intent.submission.request.run,
            generation: AttemptGeneration::FIRST,
            provenance: intent.submission.provenance,
            captured: intent.submission.policy,
            action: MemoryEditAction::Remember,
            cancellation: Cancellation::default(),
        };
        let mut binding = None;
        let mut evidence = None;
        if bound {
            let seed = crate::bots::memory_seed(bot.home_workspace, "Memory").unwrap();
            let creation = repo
                .prepare_bot_memory_creation(
                    owner,
                    bot.id,
                    BotRequestId::mint(),
                    bot.revision,
                    &seed,
                    &"b".repeat(64),
                    2,
                )
                .unwrap();
            let page = BotPage {
                workspace: bot.home_workspace,
                document: creation.identity.candidate,
            };
            let core_page = koru_core::DocumentId::from_uuid(page.document.as_uuid());
            let mut doc = koru_core::WorkspaceDoc::page_genesis(
                koru_core::WorkspaceId::from_uuid(page.workspace.as_uuid()),
                core_page,
            )
            .unwrap();
            doc.set_text(koru_core::schema::genesis_root_id(core_page), "Memory")
                .unwrap();
            let snapshot = doc.export_snapshot().unwrap();
            let version_vector = doc.doc().oplog_vv().encode();
            evidence = Some(BoundedDocumentVersion {
                id: koru_store_core::ids::DocumentVersionId::new(),
                document: page.document,
                checksum: koru_store_core::lifecycle::checksum(&snapshot),
                snapshot,
                version_vector: version_vector.clone(),
                frontiers: doc.doc().oplog_frontiers().encode(),
                captured_at: Timestamp::from_millis(3),
            });
            repo.bind_bot_memory(
                &BotMemoryConfirmation {
                    identity: creation.identity.clone(),
                    original_edit_revision: version_vector.clone(),
                    live_edit_revision: version_vector,
                },
                creation.observed(),
                3,
            )
            .unwrap();
            binding = repo.bot_library(owner, bot.id).unwrap().binding;
        }
        let authority = Arc::new(Authority {
            base,
            evidence: Mutex::new(evidence),
            reads: AtomicUsize::new(0),
            revoked: AtomicBool::new(false),
            gate: Default::default(),
            creation_gate: Default::default(),
        });
        let delegate = Arc::new(Delegate {
            request: request.clone(),
            session: koru_delegate_core::SessionId::mint(),
            allocation: Default::default(),
            calls: AtomicUsize::new(0),
            stopped: AtomicBool::new(false),
        });
        Self {
            repo,
            bot,
            request,
            authority,
            delegate,
            binding,
        }
    }
    fn allocation(&self, base: MemoryBaseSelector) -> MemoryEditAllocation {
        let key = koru_delegate_core::NativeResultKey {
            run: self.request.run,
            attempt: koru_delegate_core::RunAttemptId::from_uuid(Uuid::new_v4()),
            generation: self.request.generation,
            turn: koru_delegate_core::AgentTurnId::mint(),
            operation_id: Uuid::new_v4(),
        };
        let subject = self.request.subject();
        MemoryEditAllocation {
            caller_fingerprint: memory_write_fingerprint(
                &subject,
                key.run,
                key.operation_id,
                base,
                &operation(),
            )
            .unwrap(),
            key,
            subject,
            base,
            ensure: matches!(base, MemoryBaseSelector::OriginalSeed {}).then(|| {
                koru_delegate_core::MemoryEnsureIdentity {
                    request_id: BotRequestId::mint(),
                    expected_revision: self.repo.bot(self.bot.id).unwrap().unwrap().revision,
                    title: "Memory".into(),
                }
            }),
            resolution: MemoryBaseResolution::Unresolved {},
            source_run_document: DocumentId::new(),
            fact_id: Uuid::new_v4(),
            detail_id: Some(Uuid::new_v4()),
            source_id: Some(Uuid::new_v4()),
            created_at_ms: 4,
        }
    }
    fn start(self) -> (WorkerHandle, Receiver<Repository>) {
        let (send, returned) = sync_channel(1);
        let mut config = WorkerConfig::new(
            PathBuf::from("unused-memory-write-fixture"),
            "internal-debug",
            ProviderRegistry::default(),
        )
        .with_bot_authority(self.authority)
        .with_bot_delegate(self.delegate);
        config.test_repository = Some(self.repo);
        config.test_repository_return = Some(send);
        (AiWorker::new(config).spawn().unwrap(), returned)
    }
}
fn operation() -> MemoryRecipeOperation {
    MemoryRecipeOperation::Remember {
        summary: "Fact".into(),
        detail: "Detail".into(),
    }
}
fn enqueue(
    worker: &WorkerHandle,
    request: RunMemoryWriteSubjectRequest,
    work: Work,
) -> Receiver<ApiResult<WriteValue>> {
    let (reply, response) = sync_channel(1);
    worker
        .client()
        .sender
        .send(Envelope::RunMemoryWrite {
            request,
            work,
            reply,
            deadline: Instant::now() + Duration::from_secs(10),
        })
        .unwrap();
    response
}
fn answer(response: Receiver<ApiResult<WriteValue>>) -> ApiResult<WriteValue> {
    response.recv_timeout(Duration::from_secs(10)).unwrap()
}
fn finish(worker: WorkerHandle, returned: Receiver<Repository>) -> Repository {
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    returned.recv_timeout(Duration::from_secs(10)).unwrap()
}

#[test]
fn archived_receipt_subject_needs_no_generation_binding_or_history_and_trash_refuses() {
    for lifecycle in [BotLifecycle::Archived, BotLifecycle::Trashed] {
        let mut f = Fixture::new(false);
        f.repo
            .fixture_set_bot_lifecycle(f.bot.id, lifecycle)
            .unwrap();
        f.delegate.stopped.store(true, Ordering::SeqCst);
        let request = f.request.clone();
        let authority = f.authority.clone();
        let delegate = f.delegate.clone();
        let (worker, returned) = f.start();
        let result = answer(enqueue(&worker, request, Work::Subject));
        if lifecycle == BotLifecycle::Archived {
            assert!(matches!(
                result,
                Ok(WriteValue::Subject(MemoryWriteSubjectEvidence {
                    new_work_barrier: None,
                    ..
                }))
            ));
        } else {
            assert!(result.is_err());
        }
        assert_eq!(delegate.calls.load(Ordering::SeqCst), 0);
        assert_eq!(authority.reads.load(Ordering::SeqCst), 0);
        finish(worker, returned);
    }
}

#[test]
fn explicit_base_is_validated_off_writer_and_corrupt_history_is_refused() {
    for variant in 0..3 {
        let f = Fixture::new(true);
        let version = DocumentVersionId::from_uuid(
            f.authority
                .evidence
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .id
                .as_uuid(),
        );
        if variant == 1 {
            f.authority
                .evidence
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .checksum = "invalid".into();
        } else if variant == 2 {
            f.authority
                .evidence
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .version_vector =
                vec![0; koru_store_core::ports::MAX_BOUNDED_HISTORY_POSITION_BYTES + 1];
        }
        let request = f.request.clone();
        let authority = f.authority.clone();
        let delegate = f.delegate.clone();
        let (worker, returned) = f.start();
        let result = answer(enqueue(
            &worker,
            request,
            Work::Base {
                selection: MemoryBaseSelection::UnallocatedHistory { version },
                operation: operation(),
            },
        ));
        if variant != 0 {
            assert!(result.is_err());
        } else {
            assert!(matches!(result, Ok(WriteValue::Base(_))));
        }
        assert_eq!(authority.reads.load(Ordering::SeqCst), 1);
        assert!(delegate.calls.load(Ordering::SeqCst) >= 1);
        finish(worker, returned);
    }
}

#[test]
fn original_seed_base_requires_frozen_ensure_alias_before_history() {
    for variant in 0..3 {
        let mut f = Fixture::new(true);
        let mut allocation = f.allocation(MemoryBaseSelector::OriginalSeed {});
        if variant != 0 {
            let ensure = allocation.ensure.as_ref().unwrap();
            let bot = f.repo.bot(f.bot.id).unwrap().unwrap();
            let (seed, fingerprint) = super::super::commands::ensure_body(
                &bot,
                ensure.request_id,
                ensure.expected_revision,
                &ensure.title,
            )
            .unwrap();
            f.repo
                .prepare_bot_memory_creation(
                    f.authority.base.owner().unwrap(),
                    bot.id,
                    ensure.request_id,
                    ensure.expected_revision,
                    &seed,
                    &fingerprint,
                    4,
                )
                .unwrap();
            if variant == 1 {
                allocation.ensure.as_mut().unwrap().expected_revision += 1;
            }
        }
        *f.delegate.allocation.lock().unwrap() = Some(allocation.clone());
        let request = f.request.clone();
        let authority = f.authority.clone();
        let (worker, returned) = f.start();
        let result = answer(enqueue(
            &worker,
            request,
            Work::Base {
                selection: MemoryBaseSelection::Allocated { allocation },
                operation: operation(),
            },
        ));
        if variant == 2 {
            assert!(matches!(result, Ok(WriteValue::Base(_))));
        } else {
            assert!(result.is_err());
        }
        assert_eq!(
            authority.reads.load(Ordering::SeqCst),
            usize::from(variant == 2)
        );
        finish(worker, returned);
    }
}

#[test]
fn stopped_turn_during_ensure_preparation_never_admits_creation() {
    let f = Fixture::new(false);
    let allocation = f.allocation(MemoryBaseSelector::OriginalSeed {});
    *f.delegate.allocation.lock().unwrap() = Some(allocation.clone());
    let (entered, reached) = sync_channel(1);
    let (release, blocked) = sync_channel(1);
    *f.authority.gate.lock().unwrap() = Some((entered, blocked));
    let request = f.request.clone();
    let delegate = f.delegate.clone();
    let owner = f.authority.base.owner().unwrap();
    let bot = f.bot.id;
    let (worker, returned) = f.start();
    let response = enqueue(&worker, request, Work::Ensure { allocation });
    reached.recv_timeout(Duration::from_secs(10)).unwrap();
    delegate.stopped.store(true, Ordering::SeqCst);
    release.send(()).unwrap();
    assert!(answer(response).is_err());
    let repo = finish(worker, returned);
    assert!(repo.bot_library(owner, bot).unwrap().creation.is_none());
}

#[test]
fn canonical_terminal_chat_attempt_refuses_ensure_but_receipt_subject_remains_readable() {
    let mut f = Fixture::new_kind(false, true);
    let id = InferenceAttemptId::parse(&f.request.provenance.ai_attempt_id.unwrap().to_string())
        .unwrap();
    f.repo
        .cancel_bot_chat(id, Timestamp::from_millis(4))
        .unwrap();
    let allocation = f.allocation(MemoryBaseSelector::OriginalSeed {});
    *f.delegate.allocation.lock().unwrap() = Some(allocation.clone());
    let request = f.request.clone();
    let delegate = f.delegate.clone();
    let owner = f.authority.base.owner().unwrap();
    let bot = f.bot.id;
    let (worker, returned) = f.start();
    assert!(
        answer(enqueue(
            &worker,
            request.clone(),
            Work::Ensure { allocation }
        ))
        .is_err()
    );
    assert!(matches!(
        answer(enqueue(&worker, request, Work::Subject)),
        Ok(WriteValue::Subject(_))
    ));
    assert_eq!(delegate.calls.load(Ordering::SeqCst), 0);
    let repo = finish(worker, returned);
    assert!(repo.bot_library(owner, bot).unwrap().creation.is_none());
}

#[test]
fn write_fingerprint_freezes_exact_body_and_full_uuid_fact_domain_and_rejects_lf() {
    let f = Fixture::new(false);
    let subject = f.request.subject();
    let id = Uuid::new_v4();
    let base = MemoryBaseSelector::OriginalSeed {};
    let first = memory_write_fingerprint(&subject, f.request.run, id, base, &operation()).unwrap();
    assert_eq!(
        first,
        memory_write_fingerprint(&subject, f.request.run, id, base, &operation()).unwrap()
    );
    assert_ne!(
        first,
        memory_write_fingerprint(&subject, f.request.run, Uuid::new_v4(), base, &operation())
            .unwrap()
    );
    assert!(
        memory_write_fingerprint(
            &subject,
            f.request.run,
            id,
            base,
            &MemoryRecipeOperation::Remember {
                summary: "Fact".into(),
                detail: "One\nTwo".into()
            }
        )
        .is_err()
    );
    let mut correction = subject.clone();
    correction.action = MemoryEditAction::Correct;
    let existing = "12345678-1234-5123-8123-123456789abc".parse().unwrap();
    assert!(
        memory_write_fingerprint(
            &correction,
            f.request.run,
            id,
            MemoryBaseSelector::History {
                version: DocumentVersionId::new()
            },
            &MemoryRecipeOperation::Correct {
                fact: existing,
                summary: "Fact".into(),
                detail: "Detail".into()
            }
        )
        .is_ok()
    );
    assert!(memory_write_fingerprint(&correction, f.request.run, id, base, &operation()).is_err());
}

#[test]
fn enabled_private_write_client_runs_subject_base_authority_and_rejects_foreign_owner() {
    let f = Fixture::new(true);
    let request = f.request.clone();
    let history = f.authority.evidence.lock().unwrap().clone().unwrap();
    let version = DocumentVersionId::from_uuid(history.id.as_uuid());
    let mut allocation = f.allocation(MemoryBaseSelector::History { version });
    *f.delegate.allocation.lock().unwrap() = Some(allocation.clone());
    let authority = f.authority.clone();
    let delegate = f.delegate.clone();
    let (worker, returned) = f.start();
    let client = worker.client();
    let subject = client.memory_write_subject(request.clone()).unwrap();
    assert_eq!(subject.subject, request.subject());
    assert!(subject.new_work_barrier.unwrap().effects_allowed);
    let base = client
        .prepare_memory_edit_base(MemoryEditBaseRequest {
            subject_request: request.clone(),
            selection: MemoryBaseSelection::Allocated {
                allocation: allocation.clone(),
            },
            operation: operation(),
        })
        .unwrap();
    assert_eq!(base.version, version);
    allocation.resolution = MemoryBaseResolution::Resolved {
        authority: base.authority.clone(),
        version,
        expected_revision: base.expected_revision,
    };
    *delegate.allocation.lock().unwrap() = Some(allocation.clone());
    let final_authority = client
        .memory_write_authority(MemoryWriteAuthorityRequest {
            subject_request: request.clone(),
            resolved_allocation: allocation,
        })
        .unwrap();
    assert_eq!(final_authority.authority, base.authority);
    let mut foreign = request;
    foreign.provenance.owner.user_scope = UserScopeId::new();
    assert!(client.memory_write_subject(foreign).is_err());
    assert!(authority.reads.load(Ordering::SeqCst) > 0);
    assert!(delegate.calls.load(Ordering::SeqCst) > 0);
    finish(worker, returned);
}

#[test]
fn final_write_authority_requires_exact_resolved_binding_and_live_turn() {
    for variant in 0..3 {
        let f = Fixture::new(true);
        let evidence = f.authority.evidence.lock().unwrap().clone().unwrap();
        let version = DocumentVersionId::from_uuid(evidence.id.as_uuid());
        let mut allocation = f.allocation(MemoryBaseSelector::History { version });
        let binding = f.binding.as_ref().unwrap();
        let authority = MemoryEditAuthorityV1 {
            version: 1,
            owner: f.request.provenance.owner,
            bot: f.bot.id,
            submission: f.request.provenance.submission,
            captured_config_revision: f.request.provenance.config_revision,
            creation_request: binding.creation_request,
            workspace: binding.page.workspace,
            memory_document: if variant == 1 {
                DocumentId::new()
            } else {
                binding.page.document
            },
            action: MemoryEditAction::Remember,
        };
        allocation.resolution = MemoryBaseResolution::Resolved {
            authority: authority.clone(),
            version,
            expected_revision: evidence.version_vector,
        };
        *f.delegate.allocation.lock().unwrap() = Some(allocation.clone());
        if variant == 2 {
            f.delegate.stopped.store(true, Ordering::SeqCst);
        }
        let request = f.request.clone();
        let (worker, returned) = f.start();
        let result = answer(enqueue(&worker, request, Work::Authority { allocation }));
        if variant == 0 {
            let Ok(WriteValue::Authority(result)) = result else {
                panic!("fresh authority")
            };
            assert_eq!(result.authority, authority);
            assert!(result.barrier.effects_allowed);
        } else {
            assert!(result.is_err());
        }
        finish(worker, returned);
    }
}

#[test]
fn allocated_base_preserves_first_semantically_equal_vv_encoding() {
    fn uint(mut value: u64, into: &mut Vec<u8>) {
        while value >= 128 {
            into.push((value as u8 & 127) | 128);
            value >>= 7;
        }
        into.push(value as u8);
    }
    fn encoded(entries: &[(u64, i32)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        uint(entries.len() as u64, &mut bytes);
        for (peer, counter) in entries {
            uint(*peer, &mut bytes);
            uint(
                ((*counter as i64) << 1 ^ (*counter as i64) >> 63) as u64,
                &mut bytes,
            );
        }
        bytes
    }
    let f = Fixture::new(true);
    let evidence = f.authority.evidence.lock().unwrap().clone().unwrap();
    let vv = loro::VersionVector::decode(&evidence.version_vector).unwrap();
    let mut entries: Vec<_> = vv.iter().map(|(peer, counter)| (*peer, *counter)).collect();
    entries.sort();
    assert!(entries.len() >= 2);
    let mut frozen = encoded(&entries);
    if frozen == evidence.version_vector {
        entries.reverse();
        frozen = encoded(&entries);
    }
    assert_ne!(frozen, evidence.version_vector);
    assert_eq!(loro::VersionVector::decode(&frozen).unwrap(), vv);
    let version = DocumentVersionId::from_uuid(evidence.id.as_uuid());
    let mut allocation = f.allocation(MemoryBaseSelector::History { version });
    let binding = f.binding.as_ref().unwrap();
    allocation.resolution = MemoryBaseResolution::Resolved {
        authority: MemoryEditAuthorityV1 {
            version: 1,
            owner: f.request.provenance.owner,
            bot: f.bot.id,
            submission: f.request.provenance.submission,
            captured_config_revision: f.request.provenance.config_revision,
            creation_request: binding.creation_request,
            workspace: binding.page.workspace,
            memory_document: binding.page.document,
            action: MemoryEditAction::Remember,
        },
        version,
        expected_revision: frozen.clone(),
    };
    *f.delegate.allocation.lock().unwrap() = Some(allocation.clone());
    let request = f.request.clone();
    let (worker, returned) = f.start();
    let result = answer(enqueue(
        &worker,
        request,
        Work::Base {
            selection: MemoryBaseSelection::Allocated { allocation },
            operation: operation(),
        },
    ))
    .unwrap();
    let WriteValue::Base(result) = result else {
        panic!("base")
    };
    assert_eq!(result.expected_revision, frozen);
    finish(worker, returned);
}

#[test]
fn cancellation_or_owner_revocation_during_subject_proof_discards_reply() {
    for cancel in [false, true] {
        let f = Fixture::new(false);
        let (entered, reached) = sync_channel(1);
        let (release, blocked) = sync_channel(1);
        *f.authority.gate.lock().unwrap() = Some((entered, blocked));
        let request = f.request.clone();
        let cancellation = request.cancellation.clone();
        let authority = f.authority.clone();
        let (worker, returned) = f.start();
        let reply = enqueue(&worker, request, Work::Subject);
        reached.recv_timeout(Duration::from_secs(10)).unwrap();
        if cancel {
            cancellation.cancel();
        } else {
            authority.revoked.store(true, Ordering::SeqCst);
        }
        release.send(()).unwrap();
        assert!(answer(reply).is_err());
        finish(worker, returned);
    }
}

#[test]
fn fresh_ensure_completion_rejects_cancellation_owner_change_and_retired_epoch() {
    for variant in 0..3 {
        let f = Fixture::new(false);
        let allocation = f.allocation(MemoryBaseSelector::OriginalSeed {});
        *f.delegate.allocation.lock().unwrap() = Some(allocation.clone());
        let (entered, reached) = sync_channel(1);
        let (release, blocked) = sync_channel(1);
        *f.authority.gate.lock().unwrap() = Some((entered, blocked));
        let request = f.request.clone();
        let cancellation = request.cancellation.clone();
        let authority = f.authority.clone();
        let owner = authority.base.owner().unwrap();
        let bot = f.bot.id;
        let (worker, returned) = f.start();
        let response = enqueue(&worker, request, Work::Ensure { allocation });
        reached.recv_timeout(Duration::from_secs(10)).unwrap();
        match variant {
            0 => cancellation.cancel(),
            1 => authority.revoked.store(true, Ordering::SeqCst),
            _ => {
                let (reply, retired) = sync_channel(1);
                worker
                    .client()
                    .sender
                    .send(Envelope::RetireMemoryWritesForFixture { reply })
                    .unwrap();
                retired.recv_timeout(Duration::from_secs(10)).unwrap();
            }
        }
        release.send(()).unwrap();
        assert!(answer(response).is_err());
        let repo = finish(worker, returned);
        assert!(repo.bot_library(owner, bot).unwrap().creation.is_none());
    }
}

#[test]
fn ensure_reuses_the_shared_durable_body_and_exact_request_after_preparation() {
    let f = Fixture::new(false);
    let allocation = f.allocation(MemoryBaseSelector::OriginalSeed {});
    *f.delegate.allocation.lock().unwrap() = Some(allocation.clone());
    let request = f.request.clone();
    let bot = f.bot.clone();
    let owner = f.authority.base.owner().unwrap();
    let ensure = allocation.ensure.clone().unwrap();
    let (entered, reached) = sync_channel(1);
    let (release, blocked) = sync_channel(1);
    *f.authority.creation_gate.lock().unwrap() = Some((entered, blocked));
    let (worker, returned) = f.start();
    let first = worker
        .client()
        .ensure_run_memory(RunMemoryEnsureRequest {
            subject_request: request.clone(),
            allocation: allocation.clone(),
        })
        .unwrap();
    reached.recv_timeout(Duration::from_secs(10)).unwrap();
    let second = worker
        .client()
        .ensure_run_memory(RunMemoryEnsureRequest {
            subject_request: request,
            allocation,
        })
        .unwrap();
    assert_eq!(first.creation_request, ensure.request_id);
    assert_eq!(first.creation_request, second.creation_request);
    worker.shutdown();
    release.send(()).unwrap();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repo = returned.recv_timeout(Duration::from_secs(10)).unwrap();
    let (seed, fingerprint) = super::super::commands::ensure_body(
        &bot,
        ensure.request_id,
        ensure.expected_revision,
        &ensure.title,
    )
    .unwrap();
    let creation = repo
        .bot_memory_creation_receipt(
            owner,
            bot.id,
            ensure.request_id,
            ensure.expected_revision,
            &seed,
            &fingerprint,
        )
        .unwrap()
        .unwrap();
    assert_eq!(creation.identity.creation_request, first.creation_request);
}
