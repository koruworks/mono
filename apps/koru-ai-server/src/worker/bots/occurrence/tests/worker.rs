// SPDX-License-Identifier: MPL-2.0
//! Production client/worker/Store path; delegate and catalog are strict peers.
use super::*;
use crate::bots::routine::tests::Fixture;
use std::{
    future::Future,
    pin::Pin,
    sync::{Mutex, atomic::AtomicUsize},
    task::{Context, Poll, Wake, Waker},
};
mod cases;
mod catalog;
use catalog::Catalog;

struct Signal(SyncSender<()>);
impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        let _ = self.0.try_send(());
    }
}
fn wait<T>(mut call: RoutineCall<T>) -> Result<T, RoutineBridgeRefusal> {
    let (send, receive) = sync_channel(1);
    let waker = Waker::from(Arc::new(Signal(send)));
    let mut context = Context::from_waker(&waker);
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        match Pin::new(&mut call).poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => receive
                .recv_timeout(end.saturating_duration_since(Instant::now()))
                .expect("bounded RoutineCall completion"),
        }
    }
}
fn context() -> RoutineCallContext {
    RoutineCallContext::new(
        Uuid::new_v4(),
        Instant::now() + ROUTINE_BRIDGE_DEADLINE,
        Cancellation::default(),
    )
}

struct Gate {
    entered: SyncSender<()>,
    release: Receiver<()>,
}
impl Gate {
    fn wait(self) {
        self.entered.send(()).unwrap();
        self.release.recv_timeout(Duration::from_secs(8)).unwrap();
    }
}
struct Release(Option<SyncSender<()>>);
impl Release {
    fn open(&mut self) {
        if let Some(send) = self.0.take() {
            let _ = send.send(());
        }
    }
}
impl Drop for Release {
    fn drop(&mut self) {
        self.open();
    }
}
fn gate() -> (Gate, Receiver<()>, Release) {
    let (entered, observe) = sync_channel(1);
    let (release, released) = sync_channel(1);
    (
        Gate {
            entered,
            release: released,
        },
        observe,
        Release(Some(release)),
    )
}

struct Delegate {
    records: Mutex<BTreeMap<i64, RoutineClaimRecord>>,
    calls: Mutex<Vec<&'static str>>,
    unknown_ack: AtomicBool,
    unavailable: AtomicBool,
    unavailable_slot: Mutex<Option<i64>>,
    terminal_on_ack: Mutex<Option<RoutineTerminalReason>>,
    read_gate: Mutex<Option<Gate>>,
    ack_gate: Mutex<Option<Gate>>,
}
impl Default for Delegate {
    fn default() -> Self {
        Self {
            records: Default::default(),
            calls: Default::default(),
            unknown_ack: AtomicBool::new(false),
            unavailable: AtomicBool::new(false),
            unavailable_slot: Default::default(),
            terminal_on_ack: Default::default(),
            read_gate: Default::default(),
            ack_gate: Default::default(),
        }
    }
}
fn unavailable() -> koru_delegate_api::Failure {
    koru_delegate_api::Failure::new("unavailable", "fixture unknown completion", true)
}
impl Delegate {
    fn claim(&self, prepared: &RoutinePrepared) -> RoutineClaimIdentity {
        self.claim_with_state(prepared, RoutineLinkState::Held {})
    }
    fn claim_with_state(
        &self,
        prepared: &RoutinePrepared,
        link_state: RoutineLinkState,
    ) -> RoutineClaimIdentity {
        let claim = RoutineClaimIdentity {
            slot: prepared.slot,
            definition: prepared.definition,
            ids: prepared.ids,
            preparation_hash: prepared.preparation_hash,
            claimed_at_ms: prepared.slot.occurrence_ms + 123,
        };
        let record = RoutineClaimRecord {
            claim: claim.clone(),
            submission_version: 2,
            submission: prepared.submission.clone(),
            link_state,
            updated_at_ms: claim.claimed_at_ms,
        };
        assert!(record.valid());
        self.records
            .lock()
            .unwrap()
            .insert(claim.slot.occurrence_ms, record);
        claim
    }
}
impl BotDelegatePort for Delegate {
    fn execute(&self, _: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        panic!("only finite private calls")
    }
    fn execute_within(
        &self,
        command: DelegateCommand,
        within: Duration,
    ) -> Result<BotReply, koru_delegate_api::Failure> {
        match command {
            DelegateCommand::Hello { .. } | DelegateCommand::PendingNativeEffects { .. } => {
                Err(unavailable())
            }
            DelegateCommand::ReadRoutineClaim { slot } => {
                assert!(within <= ROUTINE_BRIDGE_DEADLINE);
                self.calls.lock().unwrap().push("read");
                if let Some(gate) = self.read_gate.lock().unwrap().take() {
                    gate.wait();
                }
                if self.unavailable.load(Ordering::Acquire)
                    || *self.unavailable_slot.lock().unwrap() == Some(slot.occurrence_ms)
                {
                    return Err(unavailable());
                }
                let observation = self
                    .records
                    .lock()
                    .unwrap()
                    .get(&slot.occurrence_ms)
                    .cloned()
                    .map_or(
                        RoutineClaimObservation::Absent {
                            slot,
                            observed_at_ms: slot.occurrence_ms + 1,
                        },
                        |record| {
                            assert_eq!(record.claim.slot, slot);
                            RoutineClaimObservation::Claimed { record }
                        },
                    );
                Ok(BotReply::RoutineClaim { observation })
            }
            DelegateCommand::AcknowledgeRoutineLink { acknowledgement } => {
                assert!(within <= ROUTINE_BRIDGE_DEADLINE);
                self.calls.lock().unwrap().push("ack");
                if let Some(gate) = self.ack_gate.lock().unwrap().take() {
                    gate.wait();
                }
                let mut records = self.records.lock().unwrap();
                let record = records
                    .get_mut(&acknowledgement.claim.slot.occurrence_ms)
                    .unwrap();
                assert_eq!(record.claim, acknowledgement.claim);
                if let Some(reason) = self.terminal_on_ack.lock().unwrap().take() {
                    record.link_state = RoutineLinkState::Terminal { reason };
                }
                if !matches!(record.link_state, RoutineLinkState::Terminal { .. }) {
                    record.link_state = match acknowledgement.outcome {
                        RoutineLinkOutcome::Linked {} => RoutineLinkState::Linked {},
                        RoutineLinkOutcome::Terminal { reason } => {
                            RoutineLinkState::Terminal { reason }
                        }
                    };
                }
                if self.unknown_ack.swap(false, Ordering::AcqRel) {
                    return Err(unavailable());
                }
                Ok(BotReply::RoutineLinkAcknowledged {
                    record: record.clone(),
                })
            }
            DelegateCommand::Reconcile { key } => {
                self.calls.lock().unwrap().push("reconcile");
                let records = self.records.lock().unwrap();
                let record = records
                    .values()
                    .find(|r| r.claim.ids.submission == key.request)
                    .unwrap();
                assert_eq!(record.claim.slot.owner, key.owner);
                assert_eq!(record.claim.slot.bot, key.bot);
                Ok(BotReply::Admission {
                    admission: koru_delegate_core::BotAdmissionView {
                        key,
                        run: record.claim.ids.run,
                        state: koru_delegate_core::BotAdmissionState::Queued,
                        queue_sequence: 1,
                        fence: None,
                    },
                })
            }
            _ => panic!("unexpected delegated execution: {command:?}"),
        }
    }
}

struct Authority {
    adapter: Arc<crate::bots::BotAuthorityAdapter>,
    captures: AtomicUsize,
    verifies: AtomicUsize,
    inspections: AtomicUsize,
    capture_gate: Mutex<Option<Gate>>,
    inspect_gate: Mutex<Option<(usize, Gate)>>,
    denied: AtomicBool,
}
impl BotAuthority for Authority {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.adapter.owner()
    }
    fn workspace(&self, home: WorkspaceId) -> Result<koru_ai_core::ProfileId, BotError> {
        self.adapter.workspace(home)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.adapter.page(page)
    }
    fn inspect(
        &self,
        homes: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<BotAuthoritySnapshot, BotError> {
        let n = self.inspections.fetch_add(1, Ordering::AcqRel) + 1;
        let gate = {
            let mut pending = self.inspect_gate.lock().unwrap();
            if pending.as_ref().is_some_and(|(at, _)| *at == n) {
                pending.take().map(|(_, gate)| gate)
            } else {
                None
            }
        };
        if let Some(gate) = gate {
            gate.wait();
        }
        if self.denied.load(Ordering::Acquire) {
            return Err(BotError::Forbidden);
        }
        self.adapter.inspect(homes, pages)
    }
    fn verify_routine_definition(
        &self,
        owner: BotOwner,
        profile: koru_ai_core::ProfileId,
        identity: &BotRoutineCreationIdentity,
        definition: &RoutineScheduleDefinition,
        handoff: Option<koru_delegate_core::HandoffId>,
        cancel: &Cancellation,
    ) -> Result<(), BotError> {
        self.verifies.fetch_add(1, Ordering::AcqRel);
        self.adapter
            .verify_routine_definition(owner, profile, identity, definition, handoff, cancel)
    }
    fn capture(
        &self,
        owner: BotOwner,
        home: WorkspaceId,
        profile: koru_ai_core::ProfileId,
        pages: &[BotPage],
    ) -> Result<Vec<koru_delegate_core::RunContextItem>, BotError> {
        self.captures.fetch_add(1, Ordering::AcqRel);
        let captured = self.adapter.capture(owner, home, profile, pages)?;
        if let Some(gate) = self.capture_gate.lock().unwrap().take() {
            gate.wait();
        }
        Ok(captured)
    }
    fn capture_memory(
        &self,
        owner: BotOwner,
        profile: koru_ai_core::ProfileId,
        binding: &BotMemoryBinding,
        cancel: &Cancellation,
    ) -> Result<PreparedMemoryCapture, MemoryAuthorityError> {
        self.adapter.capture_memory(owner, profile, binding, cancel)
    }
}

struct Harness {
    fixture: Fixture,
    request: PrepareOccurrence,
    catalog: Catalog,
    delegate: Arc<Delegate>,
    authority: Arc<Authority>,
    worker: Option<WorkerHandle>,
    returned: Receiver<Repository>,
}
impl Harness {
    fn new() -> Self {
        let (fixture, mut repository) = Fixture::with_pause(false);
        let id = fixture.creation.identity.clone();
        let PreparedRoutineDocument::Confirmed(proof) = fixture
            .adapter
            .prepare_routine_document(
                fixture.owner,
                fixture.profile,
                &id,
                &Cancellation::default(),
            )
            .unwrap()
        else {
            panic!("actual retained Store proof")
        };
        repository
            .transition_bot_routine_creation(
                &id,
                fixture.creation.observed(),
                BotRoutineCreationTransition::Confirmed(proof),
                3,
            )
            .unwrap();
        let mut definition = id.seed.header.definition.clone();
        definition.revision += 1;
        definition.effective_at_ms += 1;
        definition.lifecycle = serde_json::from_str("\"active\"").unwrap();
        let live = definition.clone();
        fixture
            .memory
            .with_store(move |store| {
                let candidate = koru_ai_core::DocumentId::from_uuid(id.schedule.as_uuid());
                let doc = koru_delegate_document::ScheduleDocument::from_snapshot(
                    &store.snapshot(id.home, candidate).unwrap(),
                )
                .unwrap();
                doc.update_definition(id.owner.device, &live).unwrap();
                store
                    .apply_update(
                        id.home,
                        candidate,
                        doc.export_updates_since(&loro::VersionVector::default())
                            .unwrap(),
                        Timestamp::from_millis(4),
                        None,
                    )
                    .unwrap();
                store
                    .flush(id.home, candidate, Timestamp::from_millis(4))
                    .unwrap();
            })
            .unwrap();
        let occurrence_ms = Timestamp::now().millis() / 60_000 * 60_000;
        let request = PrepareOccurrence {
            slot: RoutineSlot {
                owner: fixture.creation.identity.owner,
                bot: fixture.bot.id,
                schedule: fixture.creation.identity.schedule,
                occurrence_ms,
            },
            workspace: fixture.bot.home_workspace,
            owner_handoff: None,
            schedule_definition: definition,
        };
        let catalog = Catalog::new();
        let delegate = Arc::new(Delegate::default());
        let authority = Arc::new(Authority {
            adapter: fixture.adapter.clone(),
            captures: AtomicUsize::new(0),
            verifies: AtomicUsize::new(0),
            inspections: AtomicUsize::new(0),
            capture_gate: Default::default(),
            inspect_gate: Default::default(),
            denied: AtomicBool::new(false),
        });
        let (returning, returned) = sync_channel(1);
        let mut config = WorkerConfig::new(
            PathBuf::from("unused-routine-occurrence"),
            "internal-debug",
            ProviderRegistry::default(),
        )
        .with_bot_authority(authority.clone())
        .with_bot_delegate(delegate.clone())
        .with_session_host(catalog.endpoint.clone());
        config.test_repository = Some(repository);
        config.test_repository_return = Some(returning);
        let worker = AiWorker::new(config).spawn().unwrap();
        let h = Self {
            fixture,
            request,
            catalog,
            delegate,
            authority,
            worker: Some(worker),
            returned,
        };
        h.ready();
        h
    }
    fn client(&self) -> AiWorkerClient {
        self.worker.as_ref().unwrap().client()
    }
    fn read_request(&self) -> ReadPreparation {
        ReadPreparation {
            slot: self.request.slot,
            semantic_hash: bot_routine_semantic_hash(
                &self.request.slot,
                &self.request.schedule_definition,
                self.request.workspace,
                None,
            )
            .unwrap(),
        }
    }
    fn ready(&self) {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let result = wait(
                self.client()
                    .read_routine_preparation(context(), self.read_request())
                    .unwrap(),
            )
            .unwrap();
            if matches!(result, ReadPreparationReply::Missing { .. }) {
                return;
            }
            assert!(
                Instant::now() < end,
                "owner probe never completed: {result:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn prepare(&self) -> RoutinePrepared {
        match wait(
            self.client()
                .prepare_routine_occurrence(context(), self.request.clone())
                .unwrap(),
        )
        .unwrap()
        {
            PrepareOccurrenceReply::Prepared { preparation } => preparation,
            other => panic!("expected real captured preparation: {other:?}"),
        }
    }
    fn finish(&mut self) -> Repository {
        let worker = self.worker.take().unwrap();
        worker.shutdown();
        assert!(matches!(worker.join(), WorkerExit::Clean));
        self.returned.recv_timeout(Duration::from_secs(5)).unwrap()
    }
    fn restart(&mut self, repository: Repository) {
        assert!(self.worker.is_none());
        let (send, returned) = sync_channel(1);
        let mut config = WorkerConfig::new(
            PathBuf::from("unused-routine-occurrence"),
            "internal-debug",
            ProviderRegistry::default(),
        )
        .with_bot_authority(self.authority.clone())
        .with_bot_delegate(self.delegate.clone())
        .with_session_host(self.catalog.endpoint.clone());
        config.test_repository = Some(repository);
        config.test_repository_return = Some(send);
        self.returned = returned;
        self.worker = Some(AiWorker::new(config).spawn().unwrap());
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.shutdown();
            let _ = worker.join();
        }
    }
}
