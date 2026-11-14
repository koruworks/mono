// SPDX-License-Identifier: MPL-2.0
//! Production worker/adapter/Store mutation; only state observation is test-only.
use super::*;
use crate::bots::routine::tests::Fixture;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

struct Delegate {
    route: koru_delegate_core::RoutineRouteIdentity,
    additional: Vec<koru_delegate_core::RoutineRouteIdentity>,
    calls: Mutex<Vec<koru_delegate_core::RoutineRouteIdentity>>,
    registered: AtomicBool,
    unknown_once: AtomicBool,
    altered: bool,
    invalidate_owner: AtomicBool,
}
impl BotDelegatePort for Delegate {
    fn execute(&self, _: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        panic!("Routine recovery requires finite private calls")
    }
    fn execute_within(
        &self,
        command: DelegateCommand,
        within: Duration,
    ) -> Result<BotReply, koru_delegate_api::Failure> {
        match command {
            DelegateCommand::RegisterRoutineRoute { identity } => {
                assert!(within <= Duration::from_secs(10));
                assert!(identity == self.route || self.additional.contains(&identity));
                self.calls.lock().unwrap().push(identity.clone());
                self.registered.store(true, Ordering::Release);
                if self.unknown_once.swap(false, Ordering::AcqRel) {
                    return Err(koru_delegate_api::Failure::new(
                        "unknown",
                        "lost acknowledgement",
                        true,
                    ));
                }
                let mut echoed = identity;
                if self.altered {
                    echoed.created_at_ms += 1;
                }
                Ok(BotReply::RoutineRoute { identity: echoed })
            }
            // Startup reconciliation stays separate from creation authority.
            DelegateCommand::Hello { .. }
                if self.invalidate_owner.swap(false, Ordering::AcqRel) =>
            {
                Err(koru_delegate_api::Failure::new(
                    "bot_boot_owner_changed",
                    "owner changed",
                    true,
                ))
            }
            DelegateCommand::Hello { .. } | DelegateCommand::PendingNativeEffects { .. } => Err(
                koru_delegate_api::Failure::new("unavailable", "fixture startup", true),
            ),
            _ => panic!("unexpected delegate command: {command:?}"),
        }
    }
}
struct Authority {
    adapter: Arc<crate::bots::BotAuthorityAdapter>,
    delegate: Arc<Delegate>,
    calls: Mutex<Vec<BotRoutineCreationIdentity>>,
    unknown_once: AtomicBool,
    gate: Mutex<Option<(SyncSender<()>, Receiver<()>)>>,
    cancellations: Mutex<Vec<Cancellation>>,
}
impl BotAuthority for Authority {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.adapter.owner()
    }
    fn workspace(&self, home: WorkspaceId) -> Result<ProfileId, BotError> {
        self.adapter.workspace(home)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.adapter.page(page)
    }
    fn prepare_routine_document(
        &self,
        owner: BotOwner,
        profile: ProfileId,
        id: &BotRoutineCreationIdentity,
        cancel: &Cancellation,
    ) -> Result<PreparedRoutineDocument, BotError> {
        assert!(self.delegate.registered.load(Ordering::Acquire));
        self.calls.lock().unwrap().push(id.clone());
        self.cancellations.lock().unwrap().push(cancel.clone());
        let result = self
            .adapter
            .prepare_routine_document(owner, profile, id, cancel)?;
        if let Some((entered, release)) = self.gate.lock().unwrap().take() {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        if self.unknown_once.swap(false, Ordering::AcqRel) {
            return Err(BotError::Unavailable);
        }
        Ok(result)
    }
}
fn ports(f: &Fixture) -> (Arc<Delegate>, Arc<Authority>) {
    let delegate = Arc::new(Delegate {
        route: f.creation.identity.route(),
        additional: vec![],
        calls: Default::default(),
        registered: AtomicBool::new(false),
        unknown_once: AtomicBool::new(false),
        altered: false,
        invalidate_owner: AtomicBool::new(false),
    });
    let authority = Arc::new(Authority {
        adapter: f.adapter.clone(),
        delegate: delegate.clone(),
        calls: Default::default(),
        unknown_once: AtomicBool::new(false),
        gate: Default::default(),
        cancellations: Default::default(),
    });
    (delegate, authority)
}
fn start(
    repository: Repository,
    authority: Arc<dyn BotAuthority>,
    delegate: Arc<Delegate>,
) -> (WorkerHandle, Receiver<Repository>) {
    let (send, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-routine-recovery"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(delegate);
    config.test_repository = Some(repository);
    config.test_repository_return = Some(send);
    (AiWorker::new(config).spawn().unwrap(), returned)
}
fn observe(worker: &WorkerHandle, f: &Fixture) -> BotRoutineCreation {
    let (reply, response) = sync_channel(1);
    worker
        .client()
        .sender
        .send(Envelope::ReadRoutineCreationForFixture {
            owner: f.owner,
            bot: f.bot.id,
            schedule: f.creation.identity.schedule,
            reply,
        })
        .unwrap();
    response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .unwrap()
}
fn wait_state(
    worker: &WorkerHandle,
    f: &Fixture,
    expected: BotRoutineCreationState,
) -> BotRoutineCreation {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let current = observe(worker, f);
        if current.state == expected {
            return current;
        }
        assert!(
            Instant::now() < deadline,
            "Routine recovery did not reach {expected:?}"
        );
        // Bounded read-only observation with a wait, not a scheduling control.
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn context() -> RequestContext {
    RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "routine-fixture".into(),
            scopes: vec![AiScope::ManageBots, AiScope::Chat],
        },
        trace_id: "fixture".into(),
        deadline_millis: None,
    }
}

#[test]
fn routine_worker_lost_route_and_store_replies_reuse_identity_then_bound_restart_is_idle() {
    let (f, repo) = Fixture::new();
    let (delegate, authority) = ports(&f);
    delegate.unknown_once.store(true, Ordering::Release);
    authority.unknown_once.store(true, Ordering::Release);
    let (worker, returned) = start(repo, authority.clone(), delegate.clone());
    let bound = wait_state(&worker, &f, BotRoutineCreationState::Bound);
    assert_eq!(bound.identity, f.creation.identity);
    assert_eq!(delegate.calls.lock().unwrap().len(), 3);
    assert_eq!(
        *authority.calls.lock().unwrap(),
        vec![f.creation.identity.clone(); 2]
    );
    let original = f.origin();
    drop(worker);
    let repo = returned.recv_timeout(Duration::from_secs(10)).unwrap();
    let (worker, returned) = start(repo, authority.clone(), delegate.clone());
    assert_eq!(observe(&worker, &f), bound);
    drop(worker);
    returned.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(delegate.calls.lock().unwrap().len(), 3);
    assert_eq!(f.origin(), original);
}

#[test]
fn routine_job_never_reaches_store_without_exact_route_ack_or_after_cancellation() {
    for altered in [false, true] {
        let (f, _) = Fixture::new();
        let (mut delegate, _) = ports(&f);
        let d = Arc::get_mut(&mut delegate).unwrap();
        d.altered = altered;
        if !altered {
            d.unknown_once.store(true, Ordering::Release);
        }
        let authority = Arc::new(Authority {
            adapter: f.adapter.clone(),
            delegate: delegate.clone(),
            calls: Default::default(),
            unknown_once: AtomicBool::new(false),
            gate: Default::default(),
            cancellations: Default::default(),
        });
        let cancel = Cancellation::default();
        let result = RoutineCreationJob {
            epoch: 1,
            work: Work::Candidate {
                creation: f.creation.clone(),
                profile: f.profile,
            },
            authority: authority.clone(),
            delegate: delegate.clone(),
            cancellation: cancel.clone(),
        }
        .run();
        assert!(result.result.is_err());
        assert!(authority.calls.lock().unwrap().is_empty());
        cancel.cancel();
        let result = RoutineCreationJob {
            epoch: 1,
            work: Work::Candidate {
                creation: f.creation.clone(),
                profile: f.profile,
            },
            authority: authority.clone(),
            delegate: delegate.clone(),
            cancellation: cancel,
        }
        .run();
        assert!(result.result.is_err());
        assert_eq!(delegate.calls.lock().unwrap().len(), 1);
    }
}

#[test]
fn routine_worker_late_store_proof_cannot_bind_after_settings_edit_and_reads_stay_responsive() {
    let (f, repo) = Fixture::new();
    let (delegate, authority) = ports(&f);
    let (entered, reached) = sync_channel(1);
    let (release, held) = sync_channel(1);
    *authority.gate.lock().unwrap() = Some((entered, held));
    let (worker, returned) = start(repo, authority.clone(), delegate);
    reached.recv_timeout(Duration::from_secs(10)).unwrap();
    worker
        .client()
        .bots(context(), BotCommand::ReadLibrary { bot_id: f.bot.id })
        .unwrap();
    let mut config = f.bot.config.clone();
    config.name = "Changed during Store work".into();
    worker
        .client()
        .bots(
            context(),
            BotCommand::UpdateSettings {
                request_id: BotRequestId::mint(),
                bot_id: f.bot.id,
                expected_revision: 1,
                config,
            },
        )
        .unwrap();
    release.send(()).unwrap();
    let conflict = wait_state(&worker, &f, BotRoutineCreationState::Conflict);
    assert_eq!(conflict.identity, f.creation.identity);
    assert_eq!(
        conflict.refusal,
        Some(BotRoutineCreationRefusal::AuthorityChanged)
    );
    drop(worker);
    let repo = returned.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(
        repo.bot_routine_creation(f.owner, f.bot.id, f.creation.identity.schedule)
            .unwrap()
            .unwrap()
            .state,
        BotRoutineCreationState::Conflict
    );
    assert_eq!(authority.calls.lock().unwrap().len(), 1);
}

#[test]
fn routine_worker_rejects_positive_prejob_drift_without_registering_or_creating() {
    let (f, mut repo) = Fixture::new();
    let mut updated = f.bot.clone();
    updated.revision += 1;
    updated.config.name = "Changed before recovery".into();
    repo.save_bot(
        &updated,
        f.profile,
        BotRequestId::mint(),
        &"d".repeat(64),
        Some(f.bot.revision),
    )
    .unwrap();
    let (delegate, authority) = ports(&f);
    let (worker, returned) = start(repo, authority.clone(), delegate.clone());
    let conflict = wait_state(&worker, &f, BotRoutineCreationState::Conflict);
    assert_eq!(conflict.identity, f.creation.identity);
    assert_eq!(
        conflict.refusal,
        Some(BotRoutineCreationRefusal::AuthorityChanged)
    );
    assert!(authority.calls.lock().unwrap().is_empty());
    assert!(delegate.calls.lock().unwrap().is_empty());
    drop(worker);
    let mut repo = returned.recv_timeout(Duration::from_secs(10)).unwrap();
    let retry = repo
        .prepare_bot_routine_creation(
            f.owner,
            &f.command,
            koru_delegate_core::RoutineDigest32::from_bytes([7; 32]),
            99,
        )
        .unwrap();
    assert_eq!(retry.state, BotRoutineCreationState::Conflict);
    assert_eq!(retry.route, f.creation.identity.route());
}

#[test]
fn routine_job_rechecks_authority_after_a_real_store_disagreement() {
    struct Revoked {
        base: Arc<crate::bots::BotAuthorityAdapter>,
        revoked: AtomicBool,
    }
    impl BotAuthority for Revoked {
        fn owner(&self) -> Result<BotOwner, BotError> {
            if self.revoked.load(Ordering::Acquire) {
                return Err(BotError::Forbidden);
            }
            self.base.owner()
        }
        fn workspace(&self, home: WorkspaceId) -> Result<ProfileId, BotError> {
            self.base.workspace(home)
        }
        fn page(&self, page: &BotPage) -> Result<(), BotError> {
            self.base.page(page)
        }
        fn prepare_routine_document(
            &self,
            owner: BotOwner,
            reader: ProfileId,
            id: &BotRoutineCreationIdentity,
            cancel: &Cancellation,
        ) -> Result<PreparedRoutineDocument, BotError> {
            let answer = self
                .base
                .prepare_routine_document(owner, reader, id, cancel)?;
            assert!(matches!(answer, PreparedRoutineDocument::Disagreement(_)));
            self.revoked.store(true, Ordering::Release);
            Ok(answer)
        }
    }
    let (f, _) = Fixture::new();
    f.adapter
        .prepare_routine_document(
            f.owner,
            f.profile,
            &f.creation.identity,
            &Cancellation::default(),
        )
        .unwrap();
    let id = f.creation.identity.clone();
    f.memory
        .with_store(move |store| {
            store
                .trash_document_entry(
                    id.home,
                    DocumentId::from_uuid(id.schedule.as_uuid()),
                    Timestamp::from_millis(4),
                    None,
                )
                .unwrap();
        })
        .unwrap();
    let (delegate, _) = ports(&f);
    let result = RoutineCreationJob {
        epoch: 1,
        work: Work::Candidate {
            creation: f.creation,
            profile: f.profile,
        },
        authority: Arc::new(Revoked {
            base: f.adapter,
            revoked: AtomicBool::new(false),
        }),
        delegate,
        cancellation: Cancellation::default(),
    }
    .run();
    assert!(matches!(result.result, Err(BotError::Forbidden)));
}

#[test]
fn routine_owner_invalidation_cancels_without_releasing_physical_work() {
    let (f, _) = Fixture::new();
    let (delegate, authority) = ports(&f);
    let mut recovery = RoutineRecovery::default();
    recovery.observe(f.owner);
    let cancellation = Cancellation::default();
    recovery.active = Some(cancellation.clone());
    let epoch = recovery.epoch;
    recovery.invalidate_owner();
    assert!(cancellation.is_cancelled());
    assert!(recovery.active.is_some());
    assert_ne!(epoch, recovery.epoch);
    let result = RoutineCreationJob {
        epoch,
        work: Work::Candidate {
            creation: f.creation,
            profile: f.profile,
        },
        authority: authority.clone(),
        delegate: delegate.clone(),
        cancellation,
    }
    .run();
    assert!(result.result.is_err());
    assert!(delegate.calls.lock().unwrap().is_empty());
    assert!(authority.calls.lock().unwrap().is_empty());
}

#[test]
fn held_memory_creation_keeps_the_physical_slot_then_routine_progresses() {
    struct Mixed {
        routine: Arc<Authority>,
        gate: Mutex<Option<(SyncSender<()>, Receiver<()>)>>,
    }
    impl BotAuthority for Mixed {
        fn owner(&self) -> Result<BotOwner, BotError> {
            self.routine.owner()
        }
        fn workspace(&self, home: WorkspaceId) -> Result<ProfileId, BotError> {
            self.routine.workspace(home)
        }
        fn page(&self, page: &BotPage) -> Result<(), BotError> {
            self.routine.page(page)
        }
        fn prepare_memory_page(
            &self,
            owner: BotOwner,
            reader: ProfileId,
            id: &BotMemoryIdentity,
            cancel: &Cancellation,
        ) -> Result<PreparedMemoryPage, MemoryAuthorityError> {
            if let Some((entered, release)) = self.gate.lock().unwrap().take() {
                entered.send(()).unwrap();
                release.recv_timeout(Duration::from_secs(10)).unwrap();
            }
            self.routine
                .adapter
                .prepare_memory_page(owner, reader, id, cancel)
        }
        fn prepare_routine_document(
            &self,
            owner: BotOwner,
            reader: ProfileId,
            id: &BotRoutineCreationIdentity,
            cancel: &Cancellation,
        ) -> Result<PreparedRoutineDocument, BotError> {
            self.routine
                .prepare_routine_document(owner, reader, id, cancel)
        }
    }
    let (f, mut repo) = Fixture::new();
    let (delegate, authority) = ports(&f);
    repo.prepare_bot_memory_creation(
        f.owner,
        f.bot.id,
        BotRequestId::mint(),
        f.bot.revision,
        &crate::bots::memory_seed(f.bot.home_workspace, "Memory").unwrap(),
        &"c".repeat(64),
        3,
    )
    .unwrap();
    let (entered, reached) = sync_channel(1);
    let (release, blocked) = sync_channel(1);
    let mixed = Arc::new(Mixed {
        routine: authority.clone(),
        gate: Mutex::new(Some((entered, blocked))),
    });
    let (worker, returned) = start(repo, mixed, delegate);
    reached.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(authority.calls.lock().unwrap().is_empty());
    assert_eq!(
        observe(&worker, &f).state,
        BotRoutineCreationState::Prepared
    );
    worker
        .client()
        .bots(context(), BotCommand::ReadLibrary { bot_id: f.bot.id })
        .unwrap();
    release.send(()).unwrap();
    wait_state(&worker, &f, BotRoutineCreationState::Bound);
    drop(worker);
    let repo = returned.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(
        repo.bot_library(f.owner, f.bot.id)
            .unwrap()
            .binding
            .is_some()
    );
    assert_eq!(authority.calls.lock().unwrap().len(), 1);
}

#[test]
fn spawned_owner_cancellation_keeps_held_routine_slot_until_matching_completion() {
    let (f, mut repo) = Fixture::new();
    let second: BotRoutineCreate = serde_json::from_value(serde_json::json!({
        "request":BotRequestId::mint(),"bot":f.bot.id,"expected_bot_revision":1,
        "name":"Second","objective":"Other work","documents":[],"recurrence":{"type":"floating","cron":"0 10 * * *"}
    })).unwrap();
    let second = repo
        .prepare_bot_routine_creation(
            f.owner,
            &second,
            koru_delegate_core::RoutineDigest32::from_bytes([8; 32]),
            3,
        )
        .unwrap();
    let (mut delegate, authority) = ports(&f);
    // Replace the fixture port before starting the worker; neither route is
    // manufactured by a worker override.
    drop(authority);
    Arc::get_mut(&mut delegate)
        .unwrap()
        .additional
        .push(second.route);
    let authority = Arc::new(Authority {
        adapter: f.adapter.clone(),
        delegate: delegate.clone(),
        calls: Default::default(),
        unknown_once: AtomicBool::new(false),
        gate: Default::default(),
        cancellations: Default::default(),
    });
    let (entered, reached) = sync_channel(1);
    let (release, blocked) = sync_channel(1);
    *authority.gate.lock().unwrap() = Some((entered, blocked));
    let (worker, returned) = start(repo, authority.clone(), delegate.clone());
    reached.recv_timeout(Duration::from_secs(10)).unwrap();
    delegate.invalidate_owner.store(true, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if authority.cancellations.lock().unwrap()[0].is_cancelled() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "normal boot owner-change did not cancel creation"
        );
        worker.ping().unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    // The second prepared row is ready, but cancelled physical work is still
    // held. Ping/read stay responsive without detaching a replacement job.
    for _ in 0..5 {
        worker.ping().unwrap();
        assert_eq!(authority.calls.lock().unwrap().len(), 1);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        observe(&worker, &f).state,
        BotRoutineCreationState::Prepared
    );
    release.send(()).unwrap();
    wait_state(&worker, &f, BotRoutineCreationState::Bound);
    drop(worker);
    returned.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(authority.calls.lock().unwrap().len() >= 2);
}
