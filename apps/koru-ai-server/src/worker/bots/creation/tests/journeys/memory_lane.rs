// SPDX-License-Identifier: MPL-2.0
//! Both effect families use the production creation thread and writer scheduler.
use super::*;

struct MemoryOwner {
    base: Arc<FakeOwner>,
    calls: SyncSender<(DocumentId, SyncSender<()>)>,
}
impl BotAuthority for MemoryOwner {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.base.owner()
    }
    fn workspace(&self, id: WorkspaceId) -> Result<ProfileId, BotError> {
        self.base.workspace(id)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.base.page(page)
    }
    fn create_role_page(&self, seed: &BotRoleSeed) -> Result<BotRoleProof, BotError> {
        self.base.create_role_page(seed)
    }
    fn prepare_memory_page(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        identity: &BotMemoryIdentity,
        cancellation: &Cancellation,
    ) -> Result<PreparedMemoryPage, MemoryAuthorityError> {
        assert_eq!(owner, self.base.owner);
        assert_eq!(reader, self.base.profile);
        let (release, held) = sync_channel(1);
        self.calls.send((identity.candidate, release)).unwrap();
        held.recv_timeout(Duration::from_secs(10)).unwrap();
        if cancellation.is_cancelled() {
            return Err(BotError::Unavailable.into());
        }
        // Synthetic Store proof: this fixture tests only the shared scheduler.
        Ok(PreparedMemoryPage::Confirmed(BotMemoryConfirmation {
            identity: identity.clone(),
            original_edit_revision: vec![1],
            live_edit_revision: vec![1],
        }))
    }
}
fn candidate(repo: &mut Repository, owner: &Arc<FakeOwner>) -> BotMemoryCreation {
    let command = BotCommand::Create {
        request_id: BotRequestId::mint(),
        bot_id: BotId::mint(),
        home_workspace: owner.workspace,
        config: config().bot_config(
            owner.existing_page(),
            selected().to_string_lossy().into_owned(),
        ),
        access: BotAccess::Default,
        selected_pages: vec![],
    };
    let BotResponse::Bot { bot } =
        execute_bot_command(repo, owner.clone(), |_| Ok(()), owner_context(), command).unwrap()
    else {
        panic!("Bot required");
    };
    repo.prepare_bot_memory_creation(
        owner.owner,
        bot.id,
        BotRequestId::mint(),
        bot.revision,
        &crate::bots::memory_seed(owner.workspace, "Memory").unwrap(),
        &"a".repeat(64),
        1,
    )
    .unwrap()
}

#[test]
fn memory_and_bot_creation_share_one_slot_and_alternate_ready_families() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let mut repo = Repository::in_memory().unwrap();
    let first = candidate(&mut repo, &owner);
    let second = candidate(&mut repo, &owner);
    let (calls, observed) = sync_channel(2);
    let authority = Arc::new(MemoryOwner {
        base: owner.clone(),
        calls,
    });
    let h = start_with(repo, authority, &fs);
    let (candidate, release_memory) = observed.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(candidate, first.identity.candidate);

    let (folder_entered, release_folder) = fs.hold_create("work");
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, _) = request_of(&command);
    h.creation(command).unwrap();
    h.worker.ping().unwrap();
    assert!(
        fs.creates().is_empty(),
        "Bot folder ran while Memory held the slot"
    );
    release_memory.send(()).unwrap();

    // A ready Bot gets the next slot, before the second Memory candidate.
    folder_entered
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    h.worker.ping().unwrap();
    assert!(matches!(
        observed.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    release_folder.send(()).unwrap();

    // After that Bot step, Memory gets a turn before the Bot's remaining steps.
    let (candidate, release_memory) = observed.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(candidate, second.identity.candidate);
    assert_eq!(
        owner.store_calls(),
        0,
        "Bot Role skipped Memory's ready turn"
    );
    release_memory.send(()).unwrap();
    assert_eq!(h.settle(request).stage, BotCreationStage::Completed);
    let repo = h.stop();
    for candidate in [first, second] {
        assert_eq!(
            repo.bot_library(owner.owner, candidate.identity.bot)
                .unwrap()
                .creation
                .unwrap()
                .state,
            BotMemoryState::Bound
        );
    }
}
