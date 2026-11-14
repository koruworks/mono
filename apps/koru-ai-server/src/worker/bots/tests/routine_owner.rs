// SPDX-License-Identifier: MPL-2.0
//! Real owner dispatch/preparation with in-memory SQL, no delegate or Store.
use super::*;
use std::sync::Mutex;

struct Owner {
    inner: Arc<Authority>,
    pages: Mutex<Vec<BotPage>>,
    deny_pages: AtomicBool,
}
impl BotAuthority for Owner {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.inner.owner()
    }
    fn workspace(&self, id: WorkspaceId) -> Result<ProfileId, BotError> {
        self.inner.workspace(id)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.pages.lock().unwrap().push(page.clone());
        if self.deny_pages.load(Ordering::SeqCst) {
            Err(BotError::Unavailable)
        } else {
            self.inner.page(page)
        }
    }
    fn capture(
        &self,
        _: BotOwner,
        _: WorkspaceId,
        _: ProfileId,
        _: &[BotPage],
    ) -> Result<Vec<koru_delegate_core::RunContextItem>, BotError> {
        panic!("owner creation never captures content")
    }
}
fn start(
    repo: Repository,
    authority: Arc<Owner>,
) -> (WorkerHandle, AiWorkerClient, Receiver<Repository>) {
    let (send, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-routine-owner"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority);
    config.test_repository = Some(repo);
    config.test_repository_return = Some(send);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    (worker, client, returned)
}
fn owner(authority: Arc<Authority>) -> Arc<Owner> {
    Arc::new(Owner {
        inner: authority,
        pages: Default::default(),
        deny_pages: AtomicBool::new(false),
    })
}
fn create(bot: &Bot) -> BotCommand {
    serde_json::from_value(serde_json::json!({
        "command":"create_routine", "request_id":BotRequestId::mint(), "bot_id":bot.id,
        "expected_bot_revision":bot.revision, "name":"Daily", "objective":"Private task",
        "documents":[DocumentId::new()], "recurrence":{"type":"floating","cron":"0 9 * * *"}
    }))
    .unwrap()
}
fn request(command: &BotCommand) -> BotRequestId {
    match command {
        BotCommand::CreateRoutine { request_id, .. } => *request_id,
        _ => unreachable!(),
    }
}
fn read(client: &AiWorkerClient, bot: BotId, request_id: BotRequestId) -> BotRoutineCreationStatus {
    let BotResponse::RoutineCreation { creation } = client
        .bots(
            context(true),
            BotCommand::ReadRoutineCreation {
                bot_id: bot,
                request_id,
            },
        )
        .unwrap()
    else {
        panic!("creation status")
    };
    creation
}
fn retained(
    repo: &mut Repository,
    authority: &Authority,
    command: &BotCommand,
) -> BotRoutineCreation {
    let (input, fp) = routine::create_body(command).unwrap();
    repo.prepare_bot_routine_creation(authority.owner, &input, fp, 10)
        .unwrap();
    repo.bot_routine_creation_by_request(authority.owner, input.bot, input.request)
        .unwrap()
        .unwrap()
}
fn stop(worker: WorkerHandle, returned: Receiver<Repository>) -> Repository {
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    returned.recv_timeout(Duration::from_secs(5)).unwrap()
}

#[test]
fn routine_owner_creates_paused_offline_and_retries_the_same_pending_identity() {
    let (mut repo, authority, command) = fixture();
    let mut bot = created(&mut repo, authority.clone(), command);
    let pause_id = BotRequestId::mint();
    let pause = repo
        .prepare_bot_pause(
            authority.owner,
            bot.id,
            pause_id,
            bot.revision,
            true,
            &"a".repeat(64),
            3,
        )
        .unwrap();
    repo.acknowledge_bot_pause(authority.owner, bot.id, pause_id, &pause.intent.barrier, 4)
        .unwrap();
    bot = repo.bot(bot.id).unwrap().unwrap();
    assert!(bot.paused);
    let command = create(&bot);
    let authority = owner(authority);
    let (worker, client, returned) = start(repo, authority.clone());
    assert_eq!(
        client
            .bots(context(false), command.clone())
            .unwrap_err()
            .code,
        "forbidden"
    );
    assert!(authority.pages.lock().unwrap().is_empty());
    let BotResponse::RoutineCreated { receipt } =
        client.bots(context(true), command.clone()).unwrap()
    else {
        panic!("receipt")
    };
    let first = read(&client, bot.id, request(&command));
    assert_eq!(receipt.schedule_id, first.schedule_id);
    assert_eq!(
        first.observed_token.state,
        BotRoutineCreationState::Prepared
    );
    assert!(
        first.can_retry,
        "permission does not require an online delegate"
    );
    let retry = BotCommand::RetryRoutineCreation {
        bot_id: bot.id,
        original_request_id: request(&command),
        observed_token: first.observed_token,
    };
    assert_eq!(
        client.bots(context(true), retry).unwrap(),
        BotResponse::RoutineCreation {
            creation: first.clone()
        }
    );
    assert_eq!(
        client.bots(context(true), command.clone()).unwrap(),
        BotResponse::RoutineCreated { receipt }
    );
    let repo = stop(worker, returned);
    let row = repo
        .bot_routine_creation_by_request(authority.inner.owner, bot.id, request(&command))
        .unwrap()
        .unwrap();
    assert_eq!(row.identity.schedule, first.schedule_id);
    assert_eq!(
        row.identity.grant.source_bot_policy_revision,
        bot.policy_revision
    );
    assert_eq!(row.identity.grant.grant_revision, 1);
    assert!(matches!(
        row.identity.seed.header.definition.lifecycle,
        koru_delegate_schedule::ScheduleLifecycle::Paused {
            reason: koru_delegate_schedule::SchedulePauseReason::Manual
        }
    ));
    assert_eq!(authority.pages.lock().unwrap().len(), 1);
}

#[test]
fn routine_owner_exact_replay_skips_fresh_pages_and_source_but_keeps_owner_metadata() {
    let (mut repo, authority, bot_command) = fixture();
    let bot = created(&mut repo, authority.clone(), bot_command);
    let command = create(&bot);
    let original = retained(&mut repo, &authority, &command);
    let expected = BotResponse::RoutineCreated {
        receipt: (&repo
            .bot_routine_creation_receipt(authority.owner, bot.id, request(&command))
            .unwrap()
            .unwrap())
            .into(),
    };
    let mut changed = bot.clone();
    changed.revision += 1;
    changed.policy_revision += 1;
    changed.config.model.requested_model = "offline-changed".into();
    repo.save_bot(
        &changed,
        authority.profile,
        BotRequestId::mint(),
        &"b".repeat(64),
        Some(bot.revision),
    )
    .unwrap();
    let authority = owner(authority);
    authority.deny_pages.store(true, Ordering::SeqCst);
    let (worker, client, returned) = start(repo, authority.clone());
    assert_eq!(
        client.bots(context(true), command.clone()).unwrap(),
        expected
    );
    let status = read(&client, bot.id, request(&command));
    assert_eq!(status.source_bot_policy_revision, bot.policy_revision);
    assert!(
        !status.can_retry,
        "changed current source does not match frozen evidence"
    );
    let mut different = command.clone();
    if let BotCommand::CreateRoutine { name, .. } = &mut different {
        *name = serde_json::from_str("\"changed\"").unwrap();
    }
    assert_eq!(
        client.bots(context(true), different).unwrap_err().code,
        "request_conflict"
    );
    let mut fresh = command.clone();
    if let BotCommand::CreateRoutine {
        request_id,
        expected_bot_revision,
        ..
    } = &mut fresh
    {
        *request_id = BotRequestId::mint();
        *expected_bot_revision = changed.revision;
    }
    assert_eq!(
        client.bots(context(true), fresh).unwrap_err().code,
        "workspace_unavailable"
    );
    assert_eq!(
        authority.pages.lock().unwrap().len(),
        1,
        "only fresh creation inspects task documents"
    );
    authority.inner.allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        client
            .bots(context(true), command.clone())
            .unwrap_err()
            .code,
        "wrong_owner"
    );
    let repo = stop(worker, returned);
    assert_eq!(
        repo.bot_routine_creation_by_request(authority.inner.owner, bot.id, request(&command))
            .unwrap(),
        Some(original)
    );
}

#[test]
fn routine_owner_status_and_retry_distinguish_pending_bound_conflict_and_stale_tokens() {
    for state in [
        BotRoutineCreationState::Unconfirmed,
        BotRoutineCreationState::Bound,
        BotRoutineCreationState::Conflict,
    ] {
        let (mut repo, authority, bot_command) = fixture();
        let bot = created(&mut repo, authority.clone(), bot_command);
        let command = create(&bot);
        let original = retained(&mut repo, &authority, &command);
        // Synthetic repository evidence tests command projection, not Store proof.
        let transition = match state {
            BotRoutineCreationState::Unconfirmed => BotRoutineCreationTransition::Unconfirmed,
            BotRoutineCreationState::Bound => {
                BotRoutineCreationTransition::Confirmed(BotRoutineStoreConfirmation {
                    identity: original.identity.clone(),
                    origin_edit_revision: vec![1],
                })
            }
            _ => BotRoutineCreationTransition::Conflict(BotRoutineCreationRefusal::DocumentChanged),
        };
        let current = repo
            .transition_bot_routine_creation(
                &original.identity,
                original.observed(),
                transition,
                20,
            )
            .unwrap();
        let (worker, client, returned) = start(repo, owner(authority));
        let status = read(&client, bot.id, request(&command));
        assert_eq!(status.observed_token, current.observed());
        assert_eq!(
            status.can_retry,
            state == BotRoutineCreationState::Unconfirmed
        );
        let retry = |token| BotCommand::RetryRoutineCreation {
            bot_id: bot.id,
            original_request_id: request(&command),
            observed_token: token,
        };
        assert_eq!(
            client
                .bots(context(true), retry(original.observed()))
                .unwrap_err()
                .code,
            "request_conflict"
        );
        let result = client.bots(context(true), retry(current.observed()));
        if state == BotRoutineCreationState::Conflict {
            assert_eq!(result.unwrap_err().code, "request_conflict");
        } else {
            assert_eq!(
                result.unwrap(),
                BotResponse::RoutineCreation { creation: status }
            );
        }
        stop(worker, returned);
    }
}

#[test]
fn routine_owner_lifecycle_and_cross_kind_receipt_refuse_before_task_pages() {
    for lifecycle in [
        BotLifecycle::Active,
        BotLifecycle::Archived,
        BotLifecycle::Trashed,
    ] {
        let (mut repo, authority, bot_command) = fixture();
        let bot = created(&mut repo, authority.clone(), bot_command);
        let command = create(&bot);
        let c = retained(&mut repo, &authority, &command);
        let chat_request = BotRequestId::mint();
        repo.create_bot_chat(
            &bot,
            authority.profile,
            chat_request,
            &"c".repeat(64),
            at(11),
        )
        .unwrap();
        repo.fixture_set_bot_lifecycle(bot.id, lifecycle).unwrap();
        let authority = owner(authority);
        let (worker, client, returned) = start(repo, authority.clone());
        if lifecycle == BotLifecycle::Trashed {
            assert_eq!(
                client
                    .bots(
                        context(true),
                        BotCommand::ReadRoutineCreation {
                            bot_id: bot.id,
                            request_id: request(&command)
                        }
                    )
                    .unwrap_err()
                    .code,
                "not_found"
            );
        } else {
            assert_eq!(
                read(&client, bot.id, request(&command)).can_retry,
                lifecycle == BotLifecycle::Active
            );
        }
        let mut collision = command.clone();
        if let BotCommand::CreateRoutine { request_id, .. } = &mut collision {
            *request_id = chat_request;
        }
        let expected = match lifecycle {
            BotLifecycle::Active => "request_conflict",
            BotLifecycle::Archived => "bot_archived",
            _ => "not_found",
        };
        assert_eq!(
            client.bots(context(true), collision).unwrap_err().code,
            expected
        );
        let retry = BotCommand::RetryRoutineCreation {
            bot_id: bot.id,
            original_request_id: request(&command),
            observed_token: c.observed(),
        };
        if lifecycle != BotLifecycle::Active {
            assert_eq!(
                client.bots(context(true), retry).unwrap_err().code,
                expected
            );
        }
        assert!(authority.pages.lock().unwrap().is_empty());
        stop(worker, returned);
    }
}

#[test]
fn routine_owner_fingerprint_keeps_bare_json_and_rejects_duplicate_documents() {
    let (_, _, bot_command) = fixture();
    let BotCommand::Create { bot_id, .. } = bot_command else {
        unreachable!()
    };
    let command: BotCommand = serde_json::from_value(serde_json::json!({"command":"create_routine","request_id":BotRequestId::mint(),"bot_id":bot_id,"expected_bot_revision":1,"name":"Daily","objective":"task","documents":[],"recurrence":{"type":"floating","cron":"0 9 * * *"}})).unwrap();
    let (_, digest) = routine::create_body(&command).unwrap();
    assert_eq!(
        digest.as_bytes(),
        blake3::hash(&serde_json::to_vec(&command).unwrap()).as_bytes()
    );
    let mut duplicate = command;
    if let BotCommand::CreateRoutine { documents, .. } = &mut duplicate {
        let id = DocumentId::new();
        *documents = vec![id, id];
    }
    assert_eq!(
        routine::create_body(&duplicate).err().unwrap().code,
        "invalid_request"
    );
}

#[test]
fn routine_owner_late_preparation_revision_allows_only_exact_replay() {
    for replay in [false, true] {
        let (mut repo, authority, bot_command) = fixture();
        let bot = created(&mut repo, authority.clone(), bot_command);
        let command = create(&bot);
        if replay {
            retained(&mut repo, &authority, &command);
        }
        let authority = owner(authority);
        let (worker, client, returned) = start(repo, authority.clone());
        let proof = || PreparedBotCommand {
            authority: authority.clone(),
            chats: vec![(bot.id, bot.main_chat, bot.revision, bot.policy_revision)],
            runtime: Ok(()),
            pins: vec![],
            library_sources: None,
            chat_snapshot: None,
            settings: None,
            terminal_question: None,
        };
        let prepared = CommandContext {
            memory_for_fixture: None,
            request: context(true),
            prepared: Some(Arc::new(proof())),
        };
        let mut config = bot.config.clone();
        config.model.requested_model = "changed-after-preparation".into();
        let (reply, response) = sync_channel(1);
        client
            .send(WorkerCommand::Bots {
                context: prepared,
                request: BotCommand::Update {
                    request_id: BotRequestId::mint(),
                    bot_id: bot.id,
                    expected_revision: bot.revision,
                    config,
                },
                reply,
            })
            .unwrap();
        response
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        let (reply, response) = sync_channel(1);
        client
            .sender
            .send(Envelope::BotPrepared {
                command: Box::new(WorkerCommand::Bots {
                    context: context(true).into(),
                    request: command,
                    reply,
                }),
                proof: Ok(proof()),
            })
            .unwrap();
        let result = response.recv_timeout(Duration::from_secs(5)).unwrap();
        if replay {
            assert!(matches!(
                result.unwrap(),
                BotResponse::RoutineCreated { .. }
            ));
        } else {
            assert_eq!(result.unwrap_err().code, "revision_conflict");
        }
        stop(worker, returned);
    }
}
