// SPDX-License-Identifier: MPL-2.0
//! The probe of operations parked for their owner (the fourth lifecycle
//! review's F2). The recovery pass asks one page of them per pass; with
//! more parked than a page holds, it must reach every one in turn, however
//! many of the first ones never get an answer, and a probe that fails for a
//! reason that is not the owner being away keeps nothing armed.
use super::*;

/// The owner's authority, per workspace: the owner can lose it over some.
/// Every inspection naming a lost workspace is counted.
struct PerWorkspace {
    owner: BotOwner,
    profile: koru_ai_core::ProfileId,
    lost: Mutex<std::collections::HashSet<WorkspaceId>>,
    lost_inspections: AtomicUsize,
}
impl BotAuthority for PerWorkspace {
    fn owner(&self) -> Result<BotOwner, BotError> {
        Ok(self.owner)
    }
    fn workspace(&self, id: WorkspaceId) -> Result<koru_ai_core::ProfileId, BotError> {
        if self.lost.lock().unwrap().contains(&id) {
            Err(BotError::Forbidden)
        } else {
            Ok(self.profile)
        }
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.workspace(page.workspace).map(|_| ())
    }
    fn capture(
        &self,
        _owner: BotOwner,
        _workspace: WorkspaceId,
        _profile: koru_ai_core::ProfileId,
        _pages: &[BotPage],
    ) -> Result<Vec<d::RunContextItem>, BotError> {
        Err(BotError::Unavailable)
    }
    fn inspect(
        &self,
        workspaces: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<koru_ai_core::bots::BotAuthoritySnapshot, BotError> {
        if workspaces
            .iter()
            .any(|id| self.lost.lock().unwrap().contains(id))
        {
            self.lost_inspections.fetch_add(1, Ordering::SeqCst);
        }
        Ok(koru_ai_core::bots::BotAuthoritySnapshot {
            owner: self.owner,
            workspaces: workspaces
                .iter()
                .map(|id| (*id, self.workspace(*id)))
                .collect(),
            pages: pages
                .iter()
                .map(|page| (page.clone(), self.page(page)))
                .collect(),
        })
    }
}

/// A Bot of the fixture's owner in a workspace of its own.
fn bot_in_own_workspace(repository: &mut Repository, template: &Authority) -> Bot {
    let workspace = WorkspaceId::new();
    let authority = Arc::new(Authority {
        owner: template.owner,
        workspace,
        profile: template.profile,
        allowed: AtomicBool::new(true),
        inspect_gate: Default::default(),
    });
    let BotCommand::Create { config, .. } = fixture().2 else {
        unreachable!()
    };
    let mut config = config;
    config.role.workspace = workspace;
    created(
        repository,
        authority,
        BotCommand::Create {
            request_id: BotRequestId::mint(),
            bot_id: BotId::mint(),
            home_workspace: workspace,
            config,
            access: BotAccess::Default,
            selected_pages: vec![],
        },
    )
}

/// An Archive whose pause the owner acknowledged, parked in `settling`
/// because the owner stopped answering: nothing is owed to the owner, so
/// only the probe can learn that it is back.
fn parked_with_nothing_owed(repository: &mut Repository, bot: &Bot) -> BotRequestId {
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let operation = captured_archive(repository, bot);
    let pause = repository
        .bot_lifecycle_operation(bot.user_scope, operation)
        .unwrap()
        .unwrap()
        .pause_request
        .unwrap();
    let Some(BotAdmissionReceipt::Pause(receipt)) = repository
        .bot_admission_receipt(owner, bot.id, pause)
        .unwrap()
    else {
        panic!("pause receipt")
    };
    repository
        .acknowledge_bot_pause(owner, bot.id, pause, &receipt.intent.barrier, 3)
        .unwrap();
    assert!(
        repository
            .advance_bot_lifecycle(
                bot.user_scope,
                operation,
                BotLifecycleStage::Captured,
                BotLifecycleStage::Settling,
                false,
                4,
            )
            .unwrap()
    );
    assert!(
        repository
            .park_bot_lifecycle(
                bot.user_scope,
                operation,
                BotLifecycleRefusal::OwnerUnavailable,
                5
            )
            .unwrap()
    );
    operation
}

/// Seventeen operations parked for their owner, one page and one more. The
/// owner has lost its authority over the workspaces of the sixteen that come
/// first, so their probes never get an answer. The seventeenth's owner is
/// back: it is reached on a later pass and completes. Once it has, the
/// sixteen it can never ask keep nothing armed: the probe stops asking.
#[test]
fn every_parked_operation_is_probed_in_turn_and_lost_authority_keeps_nothing_armed() {
    let (mut repository, template, _) = fixture();
    let mut parked: Vec<(Bot, BotRequestId)> = (0..17)
        .map(|_| {
            let bot = bot_in_own_workspace(&mut repository, &template);
            let operation = parked_with_nothing_owed(&mut repository, &bot);
            (bot, operation)
        })
        .collect();
    // The probe's order is the Bots' ids.
    parked.sort_by_key(|(bot, _)| bot.id.to_storage());
    let authority = Arc::new(PerWorkspace {
        owner: template.owner,
        profile: template.profile,
        lost: Mutex::new(
            parked[..16]
                .iter()
                .map(|(bot, _)| bot.home_workspace)
                .collect(),
        ),
        lost_inspections: AtomicUsize::new(0),
    });
    let port = Arc::new(Archiving::default());
    let (return_tx, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-lifecycle"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority.clone())
    .with_bot_delegate(port.clone());
    config.test_repository = Some(repository);
    config.test_repository_return = Some(return_tx);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    let (last, operation) = &parked[16];
    // It starts parked: wait until it has completed, not merely settled.
    until("the seventeenth was probed, taken up and completed", || {
        read_operation(&client, last.id, *operation).stage == BotLifecycleStage::Completed
    });
    // The sixteen are never asked (no authority), and once every page has
    // been read the probe asks nothing more on their account.
    std::thread::sleep(Duration::from_millis(1500));
    let before = authority.lost_inspections.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(
        authority.lost_inspections.load(Ordering::SeqCst),
        before,
        "lost authority kept the probe armed"
    );
    assert!(before > 0, "the sixteen were each reached");
    assert!(port.reads.load(Ordering::SeqCst) > 0);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    for (bot, operation) in &parked[..16] {
        let still = repository
            .bot_lifecycle_operation(bot.user_scope, *operation)
            .unwrap()
            .unwrap();
        assert_eq!(still.stage, BotLifecycleStage::Incomplete);
        assert_eq!(still.refusal, Some(BotLifecycleRefusal::OwnerUnavailable));
    }
}
