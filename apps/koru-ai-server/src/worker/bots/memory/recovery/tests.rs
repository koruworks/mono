// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::sync::Mutex;

struct Authority {
    base: Arc<dyn BotAuthority>,
    calls: Mutex<Vec<BotMemoryIdentity>>,
    unknown_once: Mutex<bool>,
    gate: Mutex<Option<(SyncSender<()>, Receiver<()>)>>,
}
impl BotAuthority for Authority {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.base.owner()
    }
    fn workspace(&self, workspace: WorkspaceId) -> Result<koru_ai_core::ProfileId, BotError> {
        self.base.workspace(workspace)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.base.page(page)
    }
    fn prepare_memory_page(
        &self,
        owner: BotOwner,
        reader: koru_ai_core::ProfileId,
        identity: &BotMemoryIdentity,
        cancellation: &Cancellation,
    ) -> Result<PreparedMemoryPage, MemoryAuthorityError> {
        assert_eq!(owner, identity.owner);
        assert_eq!(self.workspace(identity.workspace)?, reader);
        self.calls.lock().unwrap().push(identity.clone());
        if let Some((entered, release)) = self.gate.lock().unwrap().take() {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        if cancellation.is_cancelled() {
            return Err(BotError::Unavailable.into());
        }
        if std::mem::take(&mut *self.unknown_once.lock().unwrap()) {
            return Err(BotError::Unavailable.into());
        }
        Ok(PreparedMemoryPage::Confirmed(BotMemoryConfirmation {
            identity: identity.clone(),
            original_edit_revision: vec![1],
            live_edit_revision: vec![1],
        }))
    }
}

fn fixture() -> (Repository, Bot, BotMemoryCreation, Arc<Authority>) {
    let (mut repository, base, command) = super::super::super::tests::fixture();
    let bot = super::super::super::tests::created(&mut repository, base.clone(), command);
    let owner = base.owner().unwrap();
    let creation = repository
        .prepare_bot_memory_creation(
            owner,
            bot.id,
            BotRequestId::mint(),
            bot.revision,
            &crate::bots::memory_seed(bot.home_workspace, "Memory").unwrap(),
            &"a".repeat(64),
            1,
        )
        .unwrap();
    let authority = Arc::new(Authority {
        base,
        calls: Default::default(),
        unknown_once: Default::default(),
        gate: Default::default(),
    });
    (repository, bot, creation, authority)
}
fn context() -> RequestContext {
    RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "memory-recovery".into(),
            scopes: vec![AiScope::Chat, AiScope::ManageBots],
        },
        trace_id: "fixture".into(),
        deadline_millis: None,
    }
}
fn start(
    repository: Repository,
    authority: Arc<Authority>,
) -> (WorkerHandle, Receiver<Repository>) {
    let (send, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-recovery"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority);
    config.test_repository = Some(repository);
    config.test_repository_return = Some(send);
    config.test_bot_runtime_ready = true;
    (AiWorker::new(config).spawn().unwrap(), returned)
}
fn wait_bound(worker: &WorkerHandle, bot: BotId) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let BotResponse::Library { library } = worker
            .client()
            .bots(context(), BotCommand::ReadLibrary { bot_id: bot })
            .unwrap()
        else {
            panic!("Library response required");
        };
        if library.memory.is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "Memory recovery did not bind");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn memory_recovery_reuses_unknown_candidate_and_bound_restart_is_idle() {
    let (repository, bot, original, authority) = fixture();
    *authority.unknown_once.lock().unwrap() = true;
    let (worker, returned) = start(repository, authority.clone());
    wait_bound(&worker, bot.id);
    assert_eq!(
        *authority.calls.lock().unwrap(),
        vec![original.identity.clone(), original.identity.clone()]
    );
    drop(worker);
    let repository = returned.recv_timeout(Duration::from_secs(10)).unwrap();
    let current = repository
        .bot_library(original.identity.owner, bot.id)
        .unwrap();
    assert_eq!(current.creation.unwrap().state, BotMemoryState::Bound);
    assert_eq!(
        current.binding.unwrap().page.document,
        original.identity.candidate
    );
    let (worker, returned) = start(repository, authority.clone());
    wait_bound(&worker, bot.id);
    drop(worker);
    returned.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(authority.calls.lock().unwrap().len(), 2);
}

#[test]
fn memory_recovery_late_proof_rechecks_revision_and_person_reads_remain_available() {
    let (repository, bot, original, authority) = fixture();
    let (entered, reached) = sync_channel(1);
    let (release, blocked) = sync_channel(1);
    *authority.gate.lock().unwrap() = Some((entered, blocked));
    let (worker, returned) = start(repository, authority.clone());
    reached.recv_timeout(Duration::from_secs(10)).unwrap();
    // Creation is held; the person's preparation lane still answers, and a
    // canonical settings revision cannot be overwritten by the late proof.
    worker
        .client()
        .bots(context(), BotCommand::ReadLibrary { bot_id: bot.id })
        .unwrap();
    let mut config = bot.config.clone();
    config.name = "Changed while creating Memory".into();
    worker
        .client()
        .bots(
            context(),
            BotCommand::Update {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                expected_revision: bot.revision,
                config,
            },
        )
        .unwrap();
    release.send(()).unwrap();
    wait_bound(&worker, bot.id);
    drop(worker);
    let repository = returned.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(
        repository.bot(bot.id).unwrap().unwrap().revision,
        bot.revision + 2
    );
    assert_eq!(
        *authority.calls.lock().unwrap(),
        vec![original.identity.clone(), original.identity]
    );
}

#[test]
fn memory_owner_change_cancels_proof_without_releasing_physical_work() {
    let (_, _, creation, authority) = fixture();
    let mut recovery = MemoryRecovery::default();
    recovery.observe(creation.identity.owner);
    let cancellation = Cancellation::default();
    recovery.active = Some(cancellation.clone());
    let previous_epoch = recovery.epoch;
    recovery.observe(BotOwner {
        user_scope: UserScopeId::new(),
        ..creation.identity.owner
    });
    assert!(cancellation.is_cancelled());
    assert!(recovery.active.is_some());
    assert_ne!(recovery.epoch, previous_epoch);
    let result = MemoryCreationJob {
        epoch: previous_epoch,
        work: Work::Candidate {
            creation,
            revision: 1,
        },
        authority: authority.clone(),
        cancellation,
    }
    .run();
    assert!(result.result.is_err());
    assert!(authority.calls.lock().unwrap().is_empty());
}

#[test]
fn explicit_memory_retry_wakes_backoff_without_restarting_scan() {
    let (_, _, creation, _) = fixture();
    let mut recovery = MemoryRecovery::default();
    let owner = creation.identity.owner;
    recovery.observe(owner);
    let cursor = BotMemoryRecoveryCursor {
        owner,
        after: 12,
        through: 50,
    };
    recovery.scan = Some(Some(cursor));
    recovery.page.push_back(Err(BotError::Storage));
    recovery.followup = false;
    recovery.failures = 9;
    recovery.defer();
    assert!(recovery.retry_at.is_some_and(|at| at > Instant::now()));
    recovery.wake();
    assert!(recovery.retry_at.is_none());
    assert!(recovery.followup);
    assert_eq!(recovery.scan, Some(Some(cursor)));
    assert_eq!(recovery.page.len(), 1);
}
