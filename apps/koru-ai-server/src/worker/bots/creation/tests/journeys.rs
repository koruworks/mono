// SPDX-License-Identifier: MPL-2.0
//! Whole creations through the real AI writer and its bounded pool, with
//! failures injected before and after every boundary, restarts over the
//! same in-memory database, and the writer answering while a port blocks.
use super::*;

struct Harness {
    worker: WorkerHandle,
    client: AiWorkerClient,
    returned: Receiver<Repository>,
}

fn start(repo: Repository, owner: &Arc<FakeOwner>, fs: &Arc<FakeFs>) -> Harness {
    let (returned_tx, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-creation"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(owner.clone());
    config.test_repository = Some(repo);
    config.test_repository_return = Some(returned_tx);
    config.test_bot_creation =
        Some(CreationDriver::new(root(), fs.clone()).without_model_discovery());
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    Harness {
        worker,
        client,
        returned,
    }
}

impl Harness {
    fn stop(self) -> Repository {
        drop(self.worker);
        self.returned
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker returns its repository")
    }
    fn send(&self, command: BotCommand) -> ApiResult<BotResponse> {
        self.client.bots(owner_context(), command)
    }
    fn creation(&self, command: BotCommand) -> ApiResult<BotCreation> {
        self.send(command).map(|response| match response {
            BotResponse::Creation { creation } => creation,
            other => panic!("unexpected {other:?}"),
        })
    }
    fn read(&self, request_id: BotRequestId) -> ApiResult<BotCreation> {
        self.creation(BotCommand::ReadBotCreation { request_id })
    }
    /// Wait until the creation stops moving.
    fn settle(&self, request_id: BotRequestId) -> BotCreation {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let creation = self.read(request_id).unwrap();
            if creation.stage != BotCreationStage::Pending {
                return creation;
            }
            assert!(
                Instant::now() < deadline,
                "creation never settled: {creation:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn list(
        &self,
        workspace: WorkspaceId,
        cursor: Option<BotCreationCursor>,
        limit: u16,
    ) -> ApiResult<BotCreationPage> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.send(BotCommand::ListBotCreations {
                workspace,
                cursor: cursor.clone(),
                limit,
            }) {
                Ok(BotResponse::Creations { page }) => return Ok(page),
                Ok(other) => panic!("unexpected {other:?}"),
                // The startup scan has not read every receipt yet.
                Err(error) if error.code == "ai_unavailable" && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
        }
    }
}

fn work(bot: BotId) -> PathBuf {
    root().join("bots").join(bot.to_storage()).join("work")
}

#[test]
fn an_authored_role_and_managed_folder_make_one_page_one_folder_one_bot_and_one_chat() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, bot_id) = request_of(&command);
    let first = h.creation(command.clone()).unwrap();
    assert_eq!(first.stage, BotCreationStage::Pending);
    assert_eq!(first.phase, BotCreationPhase::Intent);
    assert_eq!(first.bot, None);
    let done = h.settle(request);
    assert_eq!(done.stage, BotCreationStage::Completed, "{done:?}");
    let bot = done.bot.clone().expect("the completed Bot");
    assert_eq!(bot.id, bot_id);
    assert_eq!(bot.config.working_folder, work(bot_id).to_str().unwrap());
    // One page, created once, under the recorded candidate.
    let pages = owner.stored();
    assert_eq!(pages.len(), 1);
    let (candidate, page) = pages.iter().next().unwrap();
    assert_eq!(done.role_page, Some(bot.config.role.clone()));
    assert_eq!(bot.config.role.document, *candidate);
    assert_eq!(page.scope, BOT_ROLE_SCOPE);
    assert_eq!(owner.store_calls(), 1);
    assert_eq!(fs.creates().last(), Some(&work(bot_id)));
    // An exact retry returns the completed Bot and does nothing again.
    let creates = fs.creates().len();
    let again = h.creation(command.clone()).unwrap();
    assert_eq!(again.stage, BotCreationStage::Completed);
    assert_eq!(again.bot, Some(bot.clone()));
    assert_eq!(owner.store_calls(), 1);
    assert_eq!(fs.creates().len(), creates);
    // Changed input under the same request conflicts.
    let BotCommand::CreateBotWithRole {
        request_id,
        bot_id,
        home_workspace,
        role,
        folder,
        mut config,
        access,
        selected_pages,
    } = command
    else {
        unreachable!()
    };
    config.name = "Other".into();
    let changed = BotCommand::CreateBotWithRole {
        request_id,
        bot_id,
        home_workspace,
        role,
        folder,
        config,
        access,
        selected_pages,
    };
    assert_eq!(h.creation(changed).unwrap_err().code, "request_conflict");
    let repo = h.stop();
    assert_eq!(repo.bot(bot.id).unwrap(), Some(bot.clone()));
    let chats: Vec<_> = repo
        .all_bot_chats()
        .unwrap()
        .into_iter()
        .filter(|(id, _)| *id == bot.id)
        .collect();
    assert_eq!(chats, vec![(bot.id, bot.main_chat)]);
}

#[test]
fn an_existing_role_and_a_selected_folder_make_nothing_and_stay_shared() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root(), selected()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let role = owner.existing_page();
    let command = create(
        &owner,
        BotRoleInput::Existing { page: role.clone() },
        BotFolderChoice::Selected {
            path: selected().to_str().unwrap().into(),
        },
    );
    let (request, _) = request_of(&command);
    h.creation(command).unwrap();
    let done = h.settle(request);
    assert_eq!(done.stage, BotCreationStage::Completed, "{done:?}");
    let bot = done.bot.unwrap();
    assert_eq!(bot.config.role, role);
    assert_eq!(bot.config.working_folder, selected().to_str().unwrap());
    assert!(fs.creates().is_empty());
    assert_eq!(owner.store_calls(), 0);
    h.stop();
}

#[test]
fn a_selected_folder_that_is_not_a_folder_is_refused_before_any_intent() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let command = create(
        &owner,
        authored(),
        BotFolderChoice::Selected {
            path: selected().to_str().unwrap().into(),
        },
    );
    let (request, _) = request_of(&command);
    assert_eq!(h.creation(command).unwrap_err().code, "invalid_request");
    assert_eq!(h.read(request).unwrap_err().code, "not_found");
    assert_eq!(owner.store_calls(), 0);
    h.stop();
}

#[test]
fn a_lost_store_reply_replays_the_same_candidate_key_and_bytes() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    owner.fail_store(Fail::After);
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, _) = request_of(&command);
    h.creation(command.clone()).unwrap();
    let parked = h.settle(request);
    assert_eq!(parked.stage, BotCreationStage::Incomplete);
    assert_eq!(parked.reason, Some(BotCreationReason::StoreUnavailable));
    assert_eq!(parked.phase, BotCreationPhase::RolePending);
    // The page exists, the Bot does not: the person can find the page,
    // marked as not yet confirmed.
    assert_eq!(owner.stored().len(), 1);
    let candidate = *owner.stored().keys().next().unwrap();
    assert_eq!(parked.role_page.map(|page| page.document), Some(candidate));
    assert!(parked.role_page_unconfirmed);
    assert!(parked.retryable);
    assert_eq!(parked.bot, None);
    // Retry asks the same recorded request again, and the Store answers
    // from the origin it recorded: nothing is created a second time.
    let resumed = h.creation(command).unwrap();
    assert_eq!(resumed.stage, BotCreationStage::Pending);
    let done = h.settle(request);
    assert_eq!(done.stage, BotCreationStage::Completed, "{done:?}");
    assert!(!done.role_page_unconfirmed);
    let seeds = owner.store.lock().unwrap().seeds.clone();
    assert_eq!(seeds.len(), 2);
    assert_eq!(seeds[0], seeds[1], "one candidate, byte-identical seed");
    assert_eq!(owner.stored().len(), 1);
    assert_eq!(owner.created(), 1);
    assert_eq!(done.bot.unwrap().config.role.document, seeds[0].0);
    h.stop();
}

#[test]
fn a_store_that_did_not_answer_leaves_no_page_and_retry_creates_it_once() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    owner.fail_store(Fail::Before);
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, _) = request_of(&command);
    h.creation(command.clone()).unwrap();
    let parked = h.settle(request);
    assert_eq!(parked.reason, Some(BotCreationReason::StoreUnavailable));
    assert!(owner.stored().is_empty());
    h.creation(command).unwrap();
    assert_eq!(h.settle(request).stage, BotCreationStage::Completed);
    assert_eq!(owner.stored().len(), 1);
    h.stop();
}

#[test]
fn after_the_store_receipt_expires_the_origin_still_wins_and_a_human_edit_stays() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    owner.fail_store(Fail::After);
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, _) = request_of(&command);
    h.creation(command.clone()).unwrap();
    assert_eq!(
        h.settle(request).reason,
        Some(BotCreationReason::StoreUnavailable)
    );
    // Seven days pass, and the person edits the page meanwhile.
    owner.expire_receipts();
    {
        let mut store = owner.store.lock().unwrap();
        let page = store.pages.values_mut().next().unwrap();
        page.current = b"edited by a person".to_vec();
    }
    h.creation(command).unwrap();
    let done = h.settle(request);
    assert_eq!(done.stage, BotCreationStage::Completed, "{done:?}");
    assert_eq!(owner.store.lock().unwrap().replayed_from_origin, 1);
    let pages = owner.stored();
    assert_eq!(pages.len(), 1);
    assert_eq!(
        pages.values().next().unwrap().current,
        b"edited by a person".to_vec()
    );
    h.stop();
}

#[test]
fn a_folder_that_fails_before_or_after_mkdir_resumes_to_one_binding() {
    for fail in [Fail::Before, Fail::After] {
        let owner = FakeOwner::new();
        let fs = FakeFs::with(&[root()]);
        let h = start(Repository::in_memory().unwrap(), &owner, &fs);
        fs.fail_create("work", fail);
        let command = create(&owner, authored(), BotFolderChoice::Managed {});
        let (request, bot_id) = request_of(&command);
        h.creation(command.clone()).unwrap();
        let parked = h.settle(request);
        assert_eq!(parked.stage, BotCreationStage::Incomplete, "{fail:?}");
        assert_eq!(parked.reason, Some(BotCreationReason::FolderUnavailable));
        assert_eq!(parked.phase, BotCreationPhase::FolderPending);
        assert_eq!(fs.exists(&work(bot_id)), fail == Fail::After);
        assert_eq!(owner.store_calls(), 0, "no Store call before the folder");
        h.creation(command).unwrap();
        let done = h.settle(request);
        assert_eq!(done.stage, BotCreationStage::Completed, "{fail:?} {done:?}");
        assert_eq!(
            done.bot.unwrap().config.working_folder,
            work(bot_id).to_str().unwrap()
        );
        h.stop();
    }
}

#[test]
fn an_occupied_managed_folder_is_rejected_with_no_effect_and_frees_the_bot_id() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, bot_id) = request_of(&command);
    fs.mkdirs(&work(bot_id));
    let before = fs.present();
    h.creation(command).unwrap();
    let done = h.settle(request);
    assert_eq!(done.stage, BotCreationStage::Rejected);
    assert_eq!(done.reason, Some(BotCreationReason::FolderOccupied));
    assert!(fs.creates().is_empty());
    assert_eq!(fs.present(), before, "nothing is cleaned up");
    assert_eq!(owner.store_calls(), 0);
    // The BotId is free again: a new request with a selected folder uses it.
    fs.mkdirs(&selected());
    let reuse = BotCommand::CreateBotWithRole {
        request_id: BotRequestId::mint(),
        bot_id,
        home_workspace: owner.workspace,
        role: authored(),
        folder: BotFolderChoice::Selected {
            path: selected().to_str().unwrap().into(),
        },
        config: config(),
        access: BotAccess::Default,
        selected_pages: vec![],
    };
    let (reuse_request, _) = request_of(&reuse);
    h.creation(reuse).unwrap();
    assert_eq!(h.settle(reuse_request).stage, BotCreationStage::Completed);
    h.stop();
}

#[test]
fn competing_requests_for_one_bot_id_conflict_before_any_effect_even_after_a_restart() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root(), selected()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    owner.fail_store(Fail::Before);
    let first = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, bot_id) = request_of(&first);
    h.creation(first).unwrap();
    assert_eq!(h.settle(request).stage, BotCreationStage::Incomplete);
    let creates = fs.creates().len();
    let rival = |request_id| BotCommand::CreateBotWithRole {
        request_id,
        bot_id,
        home_workspace: owner.workspace,
        role: authored(),
        folder: BotFolderChoice::Selected {
            path: selected().to_str().unwrap().into(),
        },
        config: config(),
        access: BotAccess::Default,
        selected_pages: vec![],
    };
    let rival_request = BotRequestId::mint();
    assert_eq!(
        h.creation(rival(rival_request)).unwrap_err().code,
        "request_conflict"
    );
    assert_eq!(fs.creates().len(), creates);
    assert_eq!(owner.store_calls(), 1);
    assert_eq!(h.read(rival_request).unwrap_err().code, "not_found");
    // Restarted: the reservation is durable.
    let repo = h.stop();
    let h = start(repo, &owner, &fs);
    assert_eq!(
        h.creation(rival(BotRequestId::mint())).unwrap_err().code,
        "request_conflict"
    );
    // The parked one is listed after the restart, and stays parked.
    let page = h.list(owner.workspace, None, 10).unwrap();
    assert_eq!(page.creations.len(), 1);
    assert_eq!(page.creations[0].request_id, request);
    assert_eq!(page.creations[0].stage, BotCreationStage::Incomplete);
    assert_eq!(owner.store_calls(), 1);
    h.stop();
}

#[test]
fn a_retry_is_checked_for_authority_before_its_input_is_compared() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    owner.fail_store(Fail::Before);
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, _) = request_of(&command);
    h.creation(command.clone()).unwrap();
    assert_eq!(h.settle(request).stage, BotCreationStage::Incomplete);
    let BotCommand::CreateBotWithRole {
        request_id,
        bot_id,
        home_workspace,
        role,
        folder,
        mut config,
        access,
        selected_pages,
    } = command
    else {
        unreachable!()
    };
    config.name = "Changed".into();
    let changed = BotCommand::CreateBotWithRole {
        request_id,
        bot_id,
        home_workspace,
        role,
        folder,
        config,
        access,
        selected_pages,
    };
    // Its own home workspace out of reach: refused as the owner's, never as
    // "your input differs", which would confirm what the request holds.
    owner.allowed.store(false, Ordering::SeqCst);
    assert_eq!(h.creation(changed.clone()).unwrap_err().code, "wrong_owner");
    owner.allowed.store(true, Ordering::SeqCst);
    let repo = h.stop();
    // The same person on another device: the request is not this device's.
    let other = Arc::new(FakeOwner {
        owner: BotOwner {
            user_scope: owner.owner.user_scope,
            device: DeviceId::from_uuid(Uuid::new_v4()),
        },
        workspace: owner.workspace,
        profile: owner.profile,
        moved: Mutex::new(None),
        allowed: AtomicBool::new(true),
        pages: Mutex::new(BTreeSet::new()),
        store: Mutex::new(StoreState::default()),
        inspections: AtomicUsize::new(0),
        denied: AtomicUsize::new(0),
    });
    let h = start(repo, &other, &fs);
    assert_eq!(h.creation(changed).unwrap_err().code, "wrong_owner");
    assert_eq!(other.store_calls(), 0);
    h.stop();
}

/// The intent exactly as preparation builds it, for seeding a database
/// state a crash would leave.
fn prepared(owner: &Arc<FakeOwner>, fs: &Arc<FakeFs>, command: &BotCommand) -> BotCreationRecord {
    let (reply, _) = sync_channel(1);
    let job = CreationPreparationJob {
        context: owner_context().into(),
        request: command.clone(),
        reply,
        authority: owner.clone(),
        folders: fs.clone(),
        reserved: reserved(),
        root: root(),
        discover_models: false,
    };
    job.prepare(None).1.unwrap()
}

#[test]
fn a_restart_after_a_lost_mkdir_and_a_lost_store_reply_resumes_without_a_second_anything() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, bot_id) = request_of(&command);
    let record = prepared(&owner, &fs, &command);
    let mut repo = Repository::in_memory().unwrap();
    let scope = owner.owner.user_scope;
    let print = fingerprint(&command).unwrap();
    repo.begin_bot_creation(request, &record, &print, 1)
        .unwrap();
    // The folder was made and the process died before T2.
    let mut pending = record.clone();
    pending.phase = BotCreationPhase::FolderPending;
    repo.advance_bot_creation(
        scope,
        request,
        &record,
        &pending,
        BotReceiptStage::Prepared,
        2,
    )
    .unwrap();
    fs.mkdirs(&work(bot_id));
    let h = start(repo, &owner, &fs);
    let done = h.settle(request);
    assert_eq!(done.stage, BotCreationStage::Completed, "{done:?}");
    assert_eq!(
        done.bot.unwrap().config.working_folder,
        work(bot_id).to_str().unwrap()
    );
    let mut repo = h.stop();

    // Again, with the Role page made and its reply lost before T3.
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, bot_id) = request_of(&command);
    let record = prepared(&owner, &fs, &command);
    let scope = owner.owner.user_scope;
    repo.begin_bot_creation(request, &record, &fingerprint(&command).unwrap(), 1)
        .unwrap();
    let mut ready = record.clone();
    ready.phase = BotCreationPhase::FolderReady;
    ready.folder_binding = Some(work(bot_id).to_str().unwrap().into());
    repo.advance_bot_creation(
        scope,
        request,
        &record,
        &ready,
        BotReceiptStage::Prepared,
        2,
    )
    .unwrap();
    let mut role = ready.clone();
    role.phase = BotCreationPhase::RolePending;
    repo.advance_bot_creation(scope, request, &ready, &role, BotReceiptStage::Prepared, 3)
        .unwrap();
    fs.mkdirs(&work(bot_id));
    owner
        .create_role_page(&role_seed(&role, owner.profile).unwrap())
        .unwrap();
    assert_eq!(owner.stored().len(), 1);
    let h = start(repo, &owner, &fs);
    let done = h.settle(request);
    assert_eq!(done.stage, BotCreationStage::Completed, "{done:?}");
    assert_eq!(owner.stored().len(), 1, "one candidate");
    assert_eq!(owner.store_calls(), 2);
    assert!(
        fs.creates().is_empty(),
        "a recorded binding is never remade"
    );
    h.stop();
}

#[test]
fn an_intent_with_no_folder_yet_treats_anything_there_as_not_ours() {
    // Crash between T1 and the folder mark: nothing of ours can be there.
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, bot_id) = request_of(&command);
    let record = prepared(&owner, &fs, &command);
    let mut repo = Repository::in_memory().unwrap();
    repo.begin_bot_creation(request, &record, &fingerprint(&command).unwrap(), 1)
        .unwrap();
    fs.mkdirs(&work(bot_id));
    let h = start(repo, &owner, &fs);
    let done = h.settle(request);
    assert_eq!(done.stage, BotCreationStage::Rejected);
    assert_eq!(done.reason, Some(BotCreationReason::FolderOccupied));
    h.stop();
}

#[test]
fn revocation_midway_parks_without_dispatch_and_reading_needs_current_authority() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let (entered, release) = fs.hold_create("work");
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, _) = request_of(&command);
    h.creation(command.clone()).unwrap();
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    // Access is revoked while the folder is being made.
    owner.allowed.store(false, Ordering::SeqCst);
    release.send(()).unwrap();
    // The Role step reauthorizes first and dispatches nothing. Its refused
    // inspection is the only one while revoked; its answer is applied once
    // the writer has handled everything sent before this ping.
    let deadline = Instant::now() + Duration::from_secs(10);
    while owner.denied.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "the Role step never ran");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(100));
    h.worker.ping().unwrap();
    assert_eq!(owner.store_calls(), 0);
    // While revoked, the receipt is not disclosed or listed.
    assert_eq!(h.read(request).unwrap_err().code, "wrong_owner");
    assert_eq!(
        h.list(owner.workspace, None, 10).unwrap_err().code,
        "wrong_owner"
    );
    // Restored: it stays parked until the person retries it.
    owner.allowed.store(true, Ordering::SeqCst);
    let parked = h.read(request).unwrap();
    assert_eq!(parked.stage, BotCreationStage::Incomplete);
    assert_eq!(parked.reason, Some(BotCreationReason::OwnerUnavailable));
    assert_eq!(parked.phase, BotCreationPhase::RolePending);
    h.creation(command).unwrap();
    assert_eq!(h.settle(request).stage, BotCreationStage::Completed);
    assert_eq!(owner.stored().len(), 1);
    h.stop();
}

#[test]
fn the_writer_answers_while_a_creation_port_is_blocked() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let (entered, release) = owner.hold_store();
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, _) = request_of(&command);
    h.creation(command).unwrap();
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    // The Store port is blocked; the writer still answers at once.
    let started = Instant::now();
    for _ in 0..5 {
        h.worker.ping().unwrap();
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    release.send(()).unwrap();
    assert_eq!(h.settle(request).stage, BotCreationStage::Completed);
    h.stop();
}

#[test]
fn a_completed_folder_that_disappears_is_never_recreated() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, bot_id) = request_of(&command);
    h.creation(command.clone()).unwrap();
    assert_eq!(h.settle(request).stage, BotCreationStage::Completed);
    // A person removes the folder outside Koru.
    fs.state.lock().unwrap().dirs.remove(&key(&work(bot_id)));
    let creates = fs.creates().len();
    assert_eq!(
        h.creation(command.clone()).unwrap().stage,
        BotCreationStage::Completed
    );
    let repo = h.stop();
    let h = start(repo, &owner, &fs);
    assert_eq!(
        h.creation(command).unwrap().stage,
        BotCreationStage::Completed
    );
    assert_eq!(h.list(owner.workspace, None, 10).unwrap().creations, vec![]);
    assert_eq!(fs.creates().len(), creates);
    assert!(!fs.exists(&work(bot_id)));
    h.stop();
}

#[test]
fn a_step_the_writer_cannot_record_is_shown_and_retried_with_backoff_never_dropped() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let repo = Repository::in_memory().unwrap();
    // T2, the folder binding, cannot be written.
    repo.fixture_execute_batch(
        "CREATE TRIGGER fail_t2 BEFORE UPDATE ON bot_command_receipts
         WHEN NEW.outcome_json LIKE '%\"phase\":\"folder_ready\"%'
         BEGIN SELECT RAISE(ABORT, 'fixture'); END;",
    )
    .unwrap();
    let h = start(repo, &owner, &fs);
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, bot_id) = request_of(&command);
    h.creation(command.clone()).unwrap();
    let folder_jobs = || {
        fs.state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(op, at)| *op == "create_dir" && *at == key(&work(bot_id)))
            .count()
    };
    let stalled = h.settle(request);
    assert_eq!(stalled.stage, BotCreationStage::Incomplete, "{stalled:?}");
    assert_eq!(stalled.reason, Some(BotCreationReason::StorageFailed));
    assert_eq!(stalled.phase, BotCreationPhase::FolderPending);
    // It stays listed, and is tried again after a backoff, not per pass.
    let listed = h.list(owner.workspace, None, 10).unwrap();
    assert_eq!(listed.creations.len(), 1);
    assert_eq!(
        listed.creations[0].reason,
        Some(BotCreationReason::StorageFailed)
    );
    std::thread::sleep(Duration::from_millis(2500));
    let tries = folder_jobs();
    assert!((2..=4).contains(&tries), "{tries} folder jobs in 2.5 s");
    assert_eq!(owner.store_calls(), 0);
    let repo = h.stop();
    repo.fixture_execute_batch("DROP TRIGGER fail_t2").unwrap();
    // Restarted, it resumes from its durable phase and adopts its own folder.
    let h = start(repo, &owner, &fs);
    let done = h.settle(request);
    assert_eq!(done.stage, BotCreationStage::Completed, "{done:?}");
    assert_eq!(
        done.bot.unwrap().config.working_folder,
        work(bot_id).to_str().unwrap()
    );
    h.stop();
}

#[test]
fn a_completed_creation_reads_with_its_archived_bot_and_without_a_trashed_one() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, bot_id) = request_of(&command);
    h.creation(command.clone()).unwrap();
    assert_eq!(h.settle(request).stage, BotCreationStage::Completed);
    let repo = h.stop();
    // Archived: the creation still reads, with the Bot as it reads now.
    repo.fixture_execute_batch(&format!(
        "UPDATE bots SET lifecycle='archived' WHERE bot_id='{}'",
        bot_id.to_storage()
    ))
    .unwrap();
    let h = start(repo, &owner, &fs);
    let archived = h.read(request).unwrap();
    assert_eq!(archived.stage, BotCreationStage::Completed);
    assert_eq!(
        archived.bot.map(|bot| (bot.id, bot.lifecycle)),
        Some((bot_id, BotLifecycle::Archived))
    );
    // The exact retry is a read too: it answers the same, and changes nothing.
    assert_eq!(
        h.creation(command.clone()).unwrap().stage,
        BotCreationStage::Completed
    );
    let repo = h.stop();
    // Trashed: the creation is the owner's record and stays readable, but
    // the Bot is not shown, as a Bot read answers `not_found` for it.
    repo.fixture_execute_batch(&format!(
        "UPDATE bots SET lifecycle='trashed' WHERE bot_id='{}'",
        bot_id.to_storage()
    ))
    .unwrap();
    let h = start(repo, &owner, &fs);
    let trashed = h.read(request).unwrap();
    assert_eq!(trashed.stage, BotCreationStage::Completed);
    assert_eq!(trashed.bot, None);
    assert_eq!(trashed.bot_id, bot_id);
    assert_eq!(h.list(owner.workspace, None, 10).unwrap().creations, vec![]);
    assert_eq!(owner.store_calls(), 1);
    h.stop();
}

#[test]
fn the_list_pages_unfinished_creations_oldest_first() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let mut parked = Vec::new();
    for _ in 0..3 {
        owner.fail_store(Fail::Before);
        let command = create(&owner, authored(), BotFolderChoice::Managed {});
        let (request, _) = request_of(&command);
        h.creation(command).unwrap();
        assert_eq!(h.settle(request).stage, BotCreationStage::Incomplete);
        parked.push(request);
        std::thread::sleep(Duration::from_millis(3));
    }
    let done = create(&owner, authored(), BotFolderChoice::Managed {});
    let (done_request, _) = request_of(&done);
    h.creation(done).unwrap();
    assert_eq!(h.settle(done_request).stage, BotCreationStage::Completed);
    let first = h.list(owner.workspace, None, 2).unwrap();
    assert_eq!(
        first
            .creations
            .iter()
            .map(|c| c.request_id)
            .collect::<Vec<_>>(),
        parked[..2]
    );
    let rest = h
        .list(owner.workspace, first.next_cursor.clone(), 2)
        .unwrap();
    assert_eq!(
        rest.creations
            .iter()
            .map(|c| c.request_id)
            .collect::<Vec<_>>(),
        parked[2..]
    );
    assert_eq!(rest.next_cursor, None);
    assert!(rest.creations.iter().all(|c| c.bot.is_none()));
    assert_eq!(
        h.list(owner.workspace, None, 0).unwrap_err().code,
        "invalid_request"
    );
    assert_eq!(
        h.list(WorkspaceId::new(), None, 10).unwrap_err().code,
        "wrong_owner"
    );
    h.stop();
}

#[test]
fn a_candidate_with_another_origin_is_a_conflict_and_never_a_second_candidate() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, bot_id) = request_of(&command);
    let record = prepared(&owner, &fs, &command);
    let scope = owner.owner.user_scope;
    let mut repo = Repository::in_memory().unwrap();
    repo.begin_bot_creation(request, &record, &fingerprint(&command).unwrap(), 1)
        .unwrap();
    let mut ready = record.clone();
    ready.phase = BotCreationPhase::FolderReady;
    ready.folder_binding = Some(work(bot_id).to_str().unwrap().into());
    repo.advance_bot_creation(
        scope,
        request,
        &record,
        &ready,
        BotReceiptStage::Prepared,
        2,
    )
    .unwrap();
    // Something else already holds the recorded candidate.
    let candidate = record.role_target().document;
    owner.store.lock().unwrap().pages.insert(
        candidate,
        StoredPage {
            workspace: owner.workspace,
            scope: "koru-native".into(),
            key: Uuid::new_v4().to_string(),
            hash: "f".repeat(64),
            initial: vec![1],
            current: vec![1],
            placement: BotRolePlacement::Live,
        },
    );
    let h = start(repo, &owner, &fs);
    let parked = h.settle(request);
    assert_eq!(parked.stage, BotCreationStage::Incomplete);
    assert_eq!(parked.reason, Some(BotCreationReason::RoleConflict));
    // The identity holds another origin's page: none of ours can exist.
    assert_eq!(parked.role_page, None);
    assert!(!parked.retryable, "terminal: only Abandon ends it");
    // Retry only reports it; it never mints another candidate.
    let again = h.creation(command).unwrap();
    assert_eq!(again.reason, Some(BotCreationReason::RoleConflict));
    let seeds = owner.store.lock().unwrap().seeds.clone();
    assert_eq!(seeds.len(), 1);
    assert!(seeds.iter().all(|(asked, _)| *asked == candidate));
    assert_eq!(owner.stored().len(), 1);
    h.stop();
}

impl Harness {
    fn retry(&self, request_id: BotRequestId) -> ApiResult<BotCreation> {
        self.creation(BotCommand::RetryBotCreation { request_id })
    }
    fn abandon(&self, request_id: BotRequestId) -> ApiResult<BotCreation> {
        self.creation(BotCommand::AbandonBotCreation { request_id })
    }
}

/// A creation whose Store reply was lost: the page exists, unconfirmed.
fn lost_reply(h: &Harness, owner: &Arc<FakeOwner>) -> (BotCommand, BotRequestId, DocumentId) {
    owner.fail_store(Fail::After);
    let command = create(owner, authored(), BotFolderChoice::Managed {});
    let (request, _) = request_of(&command);
    h.creation(command.clone()).unwrap();
    let parked = h.settle(request);
    assert_eq!(parked.reason, Some(BotCreationReason::StoreUnavailable));
    let page = parked.role_page.expect("the page that may exist is shown");
    assert!(parked.role_page_unconfirmed);
    (command, request, page.document)
}

#[test]
fn a_page_deleted_forever_is_never_brought_back_and_the_bot_never_attaches_to_it() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let (command, request, page) = lost_reply(&h, &owner);
    // The person sees an unexplained page and deletes it forever: the
    // manifest entry goes, the Store keeps its row and origin.
    owner.place(page, BotRolePlacement::Missing);
    let replayed = h.creation(command.clone()).unwrap();
    assert_eq!(replayed.stage, BotCreationStage::Pending);
    let parked = h.settle(request);
    assert_eq!(parked.stage, BotCreationStage::Incomplete, "{parked:?}");
    assert_eq!(parked.reason, Some(BotCreationReason::RoleMissing));
    assert!(!parked.retryable, "only Abandon ends it");
    assert!(parked.role_page_unconfirmed);
    assert_eq!(parked.bot, None);
    // Nothing was created again, and Retry does not try.
    assert_eq!(owner.created(), 1);
    let calls = owner.store_calls();
    assert_eq!(
        h.retry(request).unwrap().reason,
        Some(BotCreationReason::RoleMissing)
    );
    assert_eq!(owner.store_calls(), calls);
    assert_eq!(
        owner.stored()[&page].placement,
        BotRolePlacement::Missing,
        "never registered again"
    );
    // A restart does not resume it either.
    let repo = h.stop();
    let h = start(repo, &owner, &fs);
    assert_eq!(
        h.read(request).unwrap().reason,
        Some(BotCreationReason::RoleMissing)
    );
    assert_eq!(owner.store_calls(), calls);
    h.stop();
}

#[test]
fn a_page_left_in_trash_parks_until_it_is_restored_and_then_retry_finishes() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let (_, request, page) = lost_reply(&h, &owner);
    owner.place(page, BotRolePlacement::Trashed);
    // Retry by request alone: the client no longer has the draft.
    assert_eq!(h.retry(request).unwrap().stage, BotCreationStage::Pending);
    let parked = h.settle(request);
    assert_eq!(parked.reason, Some(BotCreationReason::RoleUnavailable));
    assert!(parked.retryable);
    // Restored from Trash, the same Retry finishes it, on the same page.
    owner.place(page, BotRolePlacement::Live);
    h.retry(request).unwrap();
    let done = h.settle(request);
    assert_eq!(done.stage, BotCreationStage::Completed, "{done:?}");
    assert_eq!(done.bot.unwrap().config.role.document, page);
    assert_eq!(owner.created(), 1);
    h.stop();
}

#[test]
fn retry_by_request_resumes_from_the_recorded_intent_under_fresh_authority() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    owner.fail_store(Fail::Before);
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, bot_id) = request_of(&command);
    h.creation(command).unwrap();
    assert_eq!(h.settle(request).stage, BotCreationStage::Incomplete);
    // Without current authority Retry discloses nothing and does nothing.
    owner.allowed.store(false, Ordering::SeqCst);
    assert_eq!(h.retry(request).unwrap_err().code, "wrong_owner");
    owner.allowed.store(true, Ordering::SeqCst);
    assert_eq!(h.retry(BotRequestId::mint()).unwrap_err().code, "not_found");
    assert!(owner.stored().is_empty());
    h.retry(request).unwrap();
    let done = h.settle(request);
    assert_eq!(done.stage, BotCreationStage::Completed, "{done:?}");
    assert_eq!(done.bot_id, bot_id);
    assert_eq!(
        done.folder_binding.as_deref(),
        Some(work(bot_id).to_str().unwrap())
    );
    // Retry of a completed creation only reports it.
    assert_eq!(h.retry(request).unwrap().stage, BotCreationStage::Completed);
    assert_eq!(owner.created(), 1);
    h.stop();
}

#[test]
fn abandon_ends_a_creation_names_what_it_leaves_and_keeps_the_bot_id() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let (_, request, page) = lost_reply(&h, &owner);
    let bot_id = h.read(request).unwrap().bot_id;
    let before = fs.present();
    let ended = h.abandon(request).unwrap();
    assert_eq!(ended.stage, BotCreationStage::Abandoned);
    assert!(!ended.retryable);
    assert_eq!(ended.bot, None);
    // It names the page and folder it may have made; it removes neither.
    let left = ended.left_behind.clone().expect("what it leaves behind");
    assert_eq!(left.role_page.map(|p| p.document), Some(page));
    assert_eq!(left.folder.as_deref(), Some(work(bot_id).to_str().unwrap()));
    assert_eq!(fs.present(), before);
    assert_eq!(owner.stored()[&page].placement, BotRolePlacement::Live);
    // Abandoning again answers the same; Retry only reports it.
    assert_eq!(h.abandon(request).unwrap(), ended);
    assert_eq!(h.retry(request).unwrap().stage, BotCreationStage::Abandoned);
    // It is no longer listed as unfinished, and its BotId stays reserved.
    assert_eq!(h.list(owner.workspace, None, 10).unwrap().creations, vec![]);
    let reuse = BotCommand::CreateBotWithRole {
        request_id: BotRequestId::mint(),
        bot_id,
        home_workspace: owner.workspace,
        role: authored(),
        folder: BotFolderChoice::Managed {},
        config: config(),
        access: BotAccess::Default,
        selected_pages: vec![],
    };
    assert_eq!(h.creation(reuse).unwrap_err().code, "request_conflict");
    // Its retained Role text is gone; across a restart it stays ended.
    let repo = h.stop();
    let receipt = repo
        .bot_creation(owner.owner.user_scope, request)
        .unwrap()
        .unwrap();
    assert!(matches!(
        receipt.record.role,
        BotCreationRole::Authored { seed: None, .. }
    ));
    let h = start(repo, &owner, &fs);
    assert_eq!(h.read(request).unwrap().stage, BotCreationStage::Abandoned);
    assert_eq!(owner.created(), 1);
    h.stop();
}

#[test]
fn a_completed_creation_cannot_be_abandoned_and_a_shared_role_is_never_left_behind() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, _) = request_of(&command);
    h.creation(command).unwrap();
    assert_eq!(h.settle(request).stage, BotCreationStage::Completed);
    assert_eq!(h.abandon(request).unwrap_err().code, "request_conflict");
    assert_eq!(h.read(request).unwrap().stage, BotCreationStage::Completed);
    // An existing Role page that stops being readable before the Bot is
    // made: abandoning names the managed folder, never the shared page.
    let role = owner.existing_page();
    let (entered, release) = fs.hold_create("work");
    let shared = create(
        &owner,
        BotRoleInput::Existing { page: role.clone() },
        BotFolderChoice::Managed {},
    );
    let (shared_request, bot_id) = request_of(&shared);
    h.creation(shared).unwrap();
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    owner.pages.lock().unwrap().remove(&role.document);
    release.send(()).unwrap();
    let parked = h.settle(shared_request);
    assert_eq!(parked.reason, Some(BotCreationReason::RoleUnavailable));
    assert_eq!(parked.role_page, Some(role));
    let ended = h.abandon(shared_request).unwrap();
    assert_eq!(
        ended.left_behind,
        Some(BotCreationLeftovers {
            role_page: None,
            folder: Some(work(bot_id).to_str().unwrap().to_owned()),
        })
    );
    h.stop();
}

#[test]
fn a_workspace_moved_to_another_profile_midway_is_followed_not_parked_for_good() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let (entered, release) = fs.hold_create("work");
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, _) = request_of(&command);
    h.creation(command).unwrap();
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    // The person moves the workspace to another Profile meanwhile.
    let moved = ProfileId::new();
    *owner.moved.lock().unwrap() = Some(moved);
    release.send(()).unwrap();
    let done = h.settle(request);
    assert_eq!(done.stage, BotCreationStage::Completed, "{done:?}");
    let repo = h.stop();
    let bot = done.bot.unwrap();
    assert_eq!(
        repo.chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat])
        )
        .unwrap()
        .unwrap()
        .profile_id,
        Some(moved)
    );
}

#[test]
fn a_definite_store_refusal_is_terminal_not_store_unavailable() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    owner.store.lock().unwrap().refuse = true;
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    let (request, _) = request_of(&command);
    h.creation(command).unwrap();
    let parked = h.settle(request);
    assert_eq!(parked.reason, Some(BotCreationReason::StoreRefused));
    assert!(!parked.retryable);
    assert!(owner.stored().is_empty());
    assert_eq!(
        h.abandon(request).unwrap().stage,
        BotCreationStage::Abandoned
    );
    h.stop();
}
