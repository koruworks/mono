// SPDX-License-Identifier: MPL-2.0
use super::*;
mod lifecycle;
mod lists;
use koru_ai_core::{
    DocumentId, ProfileId, WorkspaceId,
    bots::{BOT_ROLE_SCOPE, BotRoleProof},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

struct Store {
    owner: BotOwner,
    home: WorkspaceId,
    profile: ProfileId,
    origins: Mutex<BTreeMap<DocumentId, BotRoleProof>>,
    lost: AtomicBool,
    deleted: AtomicBool,
    trashed: AtomicBool,
    moved: AtomicBool,
    writes: AtomicUsize,
}
impl BotAuthority for Store {
    fn owner(&self) -> Result<BotOwner, BotError> {
        let mut owner = self.owner;
        if self.moved.load(Ordering::Relaxed) {
            owner.user_scope = koru_store_core::UserScopeId::from_uuid(Uuid::new_v4());
        }
        Ok(owner)
    }
    fn workspace(&self, workspace: WorkspaceId) -> Result<ProfileId, BotError> {
        if workspace == self.home {
            Ok(self.profile)
        } else {
            Err(BotError::Forbidden)
        }
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        if page.workspace != self.home
            || (self.deleted.load(Ordering::Relaxed)
                && self.origins.lock().unwrap().contains_key(&page.document))
        {
            Err(BotError::NotFound)
        } else {
            Ok(())
        }
    }
    fn create_role_page(&self, seed: &BotRoleSeed) -> Result<BotRoleProof, BotError> {
        assert_eq!(seed.owner, self.owner);
        assert_eq!(seed.profile, self.profile);
        let mut origins = self.origins.lock().unwrap();
        let proof = origins.entry(seed.candidate).or_insert_with(|| {
            self.writes.fetch_add(1, Ordering::Relaxed);
            BotRoleProof {
                scope: BOT_ROLE_SCOPE.into(),
                request_key: seed.request_key.clone(),
                request_hash: seed.request_hash.clone(),
                initial_update: seed.seed.clone(),
                placement: BotRolePlacement::Live,
            }
        });
        if self.deleted.load(Ordering::Relaxed) {
            proof.placement = BotRolePlacement::Missing;
        } else if self.trashed.load(Ordering::Relaxed) {
            proof.placement = BotRolePlacement::Trashed;
        } else {
            proof.placement = BotRolePlacement::Live;
        }
        if self.lost.swap(false, Ordering::Relaxed) {
            return Err(BotError::Unavailable);
        }
        Ok(proof.clone())
    }
}
struct Fs {
    entries: Mutex<BTreeMap<PathBuf, bool>>,
    creates: AtomicUsize,
}
impl folders::Folders for Fs {
    fn canonical(&self, path: &Path) -> std::io::Result<PathBuf> {
        if self.entries.lock().unwrap().contains_key(path) {
            Ok(path.to_owned())
        } else {
            Err(std::io::ErrorKind::NotFound.into())
        }
    }
    fn occupied(&self, path: &Path) -> std::io::Result<bool> {
        Ok(self.entries.lock().unwrap().contains_key(path))
    }
    fn create(&self, path: &Path) -> std::io::Result<()> {
        let mut entries = self.entries.lock().unwrap();
        if entries.contains_key(path) {
            return Err(std::io::ErrorKind::AlreadyExists.into());
        }
        self.creates.fetch_add(1, Ordering::Relaxed);
        entries.insert(path.to_owned(), true);
        Ok(())
    }
    fn directory(&self, path: &Path) -> std::io::Result<bool> {
        Ok(self.entries.lock().unwrap().contains_key(path))
    }
    fn empty(&self, path: &Path) -> std::io::Result<bool> {
        self.entries
            .lock()
            .unwrap()
            .get(path)
            .copied()
            .ok_or(std::io::ErrorKind::NotFound.into())
    }
}
struct Fixture {
    gateway: Gateway,
    store: Arc<Store>,
    fs: Arc<Fs>,
    record: Record,
    abandon: Arc<AtomicBool>,
}
impl Fixture {
    fn new() -> Self {
        let root = if cfg!(windows) {
            PathBuf::from(r"C:\synthetic")
        } else {
            PathBuf::from("/synthetic")
        };
        let store = Arc::new(Store {
            owner: BotOwner {
                user_scope: koru_store_core::UserScopeId::from_uuid(Uuid::new_v4()),
                device: koru_delegate_core::DeviceId::new(),
            },
            home: WorkspaceId::new(),
            profile: ProfileId::new(),
            origins: Mutex::new(BTreeMap::new()),
            lost: AtomicBool::new(false),
            deleted: AtomicBool::new(false),
            trashed: AtomicBool::new(false),
            moved: AtomicBool::new(false),
            writes: AtomicUsize::new(0),
        });
        let fs = Arc::new(Fs {
            entries: Mutex::new(BTreeMap::from([(root.clone(), true)])),
            creates: AtomicUsize::new(0),
        });
        let database = Arc::new(Mutex::new((
            koru_bots_engine::Database::in_memory().unwrap(),
            koru_bots_engine::observations::LiveEvents::default(),
            koru_bots_engine::questions::Transient::default(),
        )));
        let person_database = database.clone();
        let abandon = Arc::new(AtomicBool::new(false));
        let flag = abandon.clone();
        let gateway = Gateway {
            authority: store.clone(),
            folders: fs.clone(),
            root,
            permits: Arc::new(tokio::sync::Semaphore::new(8)),
            call: Arc::new(move |request| {
                let mut state = person_database.lock().unwrap();
                let (database, events, answers) = &mut *state;
                person::handle(database, events, answers, request, 2)
            }),
            creation: Arc::new(move |request| {
                let mut state = database.lock().unwrap();
                let (database, _, _) = &mut *state;
                if let c::Command::Advance {
                    request: id,
                    revision,
                    step: Step::RoleVerified,
                } = request.command
                    && flag.swap(false, Ordering::Relaxed)
                {
                    c::handle(
                        database,
                        c::Request {
                            owner: request.owner.clone(),
                            sender: request.sender.clone(),
                            command: c::Command::Advance {
                                request: id,
                                revision,
                                step: Step::Abandon,
                            },
                        },
                        2,
                    )
                    .unwrap();
                }
                c::handle(database, request, 2)
            }),
        };
        let reply = gateway
            .creation_call(
                store.owner,
                c::Command::Begin {
                    request: Uuid::new_v4(),
                    bot: Uuid::new_v4(),
                    source_hash: blake3::hash(b"synthetic request").to_hex().to_string(),
                    definition: person::Definition {
                        name: "Synthetic".into(),
                        avatar: Some(2),
                        configuration: koru_bots_engine::configuration::Configuration {
                            provider: "claude".parse().unwrap(),
                            selection: Default::default(),
                            working_folder: None,
                        },
                        access: koru_delegate_core::CapturedBotPolicy {
                            workspace: store.home,
                            destination: store.home,
                            access: koru_delegate_core::BotAccess::Default,
                            selected_pages: Default::default(),
                            connections: Default::default(),
                        },
                        role: None,
                        knowledge: vec![],
                        memory: None,
                    },
                    authored: Some(c::AuthoredRole {
                        title: "Synthetic Role".into(),
                        source_hash: blake3::hash(b"synthetic").to_hex().to_string(),
                    }),
                    seed: Some(vec![1, 2, 3]),
                },
            )
            .unwrap();
        let c::Reply::Record(Some(record)) = reply else {
            panic!()
        };
        Self {
            gateway,
            store,
            fs,
            record,
            abandon,
        }
    }
    fn drive(&self, record: Record) -> Result<Record, Error> {
        self.gateway.drive_creation(self.store.owner, record)
    }
    fn read(&self) -> Record {
        self.gateway
            .creation_read(self.store.owner, self.record.request)
            .unwrap()
            .unwrap()
    }
}

#[test]
fn complete_creation_proves_recorded_origin_before_committing_bot() {
    let f = Fixture::new();
    let done = f.drive(f.record.clone()).unwrap();
    assert_eq!(done.phase, Phase::Completed);
    assert_eq!(
        done.result.as_ref().unwrap().definition.role,
        f.record.definition.role
    );
    assert_eq!(f.store.writes.load(Ordering::Relaxed), 1);
    assert_eq!(f.fs.creates.load(Ordering::Relaxed), 3);
    let retried = f.drive(f.read()).unwrap();
    assert_eq!(
        retried.result.unwrap().main_thread,
        done.result.unwrap().main_thread
    );
    assert_eq!(f.store.writes.load(Ordering::Relaxed), 1);
}

#[test]
fn lost_store_reply_reuses_origin_and_does_not_recreate_deleted_page() {
    for deleted in [false, true] {
        let f = Fixture::new();
        f.store.lost.store(true, Ordering::Relaxed);
        let parked = f.drive(f.record.clone()).unwrap();
        assert_eq!(parked.phase, Phase::RolePending);
        assert_eq!(parked.failure, Some(Failure::RoleUnavailable));
        f.store.deleted.store(deleted, Ordering::Relaxed);
        let retry = f
            .gateway
            .creation_advance(f.store.owner, &parked, Step::Retry)
            .unwrap();
        let done = f.drive(retry).unwrap();
        assert_eq!(f.store.writes.load(Ordering::Relaxed), 1);
        assert_eq!(f.fs.creates.load(Ordering::Relaxed), 3);
        if deleted {
            assert_eq!(done.failure, Some(Failure::RoleMissing));
            assert!(done.result.is_none());
            assert_eq!(
                f.gateway
                    .creation_advance(f.store.owner, &done, Step::Retry)
                    .err(),
                Some(Error::NotAllowed)
            );
        } else {
            assert_eq!(done.phase, Phase::Completed);
        }
    }
}

#[test]
fn changed_owner_or_occupied_folder_prevents_store_effect() {
    let f = Fixture::new();
    f.store.moved.store(true, Ordering::Relaxed);
    let parked = f.drive(f.record.clone()).unwrap();
    assert_eq!(parked.failure, Some(Failure::AuthorityUnavailable));
    assert_eq!(f.fs.creates.load(Ordering::Relaxed), 0);
    assert_eq!(f.store.writes.load(Ordering::Relaxed), 0);
    let f = Fixture::new();
    f.fs.entries
        .lock()
        .unwrap()
        .insert(folders::managed_path(&f.gateway.root, f.record.bot), false);
    let parked = f.drive(f.record.clone()).unwrap();
    assert_eq!(parked.failure, Some(Failure::FolderOccupied));
    assert_eq!(f.store.writes.load(Ordering::Relaxed), 0);
}

#[test]
fn late_store_proof_cannot_complete_an_abandoned_creation() {
    let f = Fixture::new();
    f.abandon.store(true, Ordering::Relaxed);
    assert_eq!(f.drive(f.record.clone()).err(), Some(Error::Conflict));
    let abandoned = f.read();
    assert_eq!(abandoned.phase, Phase::Abandoned);
    assert_eq!(abandoned.abandoned_from, Some(Phase::RolePending));
    assert!(abandoned.result.is_none());
    assert!(abandoned.seed.unwrap().bytes.is_empty());
    assert_eq!(f.store.writes.load(Ordering::Relaxed), 1);
}

fn form(f: &Fixture) -> koru_ai_core::bots::BotCommand {
    use koru_ai_core::bots::*;
    BotCommand::CreateBotWithRole {
        request_id: BotRequestId::from_uuid(Uuid::new_v4()),
        bot_id: BotId::from_uuid(Uuid::new_v4()),
        home_workspace: f.store.home,
        role: BotRoleInput::Authored {
            title: "Synthetic Role".into(),
            markdown: "private_role_marker".into(),
        },
        folder: BotFolderChoice::Managed {},
        config: BotCreationConfig {
            name: "Synthetic window Bot".into(),
            avatar: Some(4),
            model: koru_ai_core::ModelSelection {
                provider: koru_ai_core::AiProviderId::parse("agent-claude").unwrap(),
                requested_model: koru_ai_core::AGENT_DEFAULT_MODEL.into(),
                default_reasoning: Default::default(),
            },
            mode: None,
            fast: None,
        },
        access: BotAccess::Default,
        selected_pages: vec![],
    }
}
fn creation(reply: koru_ai_core::bots::BotResponse) -> koru_ai_core::bots::BotCreation {
    let koru_ai_core::bots::BotResponse::Creation { creation } = reply else {
        panic!("creation expected")
    };
    creation
}

#[test]
fn settings_save_role_atomically_and_retry_the_original_receipt_after_later_changes() {
    use koru_ai_core::bots::*;
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let mut config = bot.config.clone();
    config.name = "Revised".into();
    config.avatar = Some(9);
    config.role.document = DocumentId::new();
    let update = BotCommand::Update {
        request_id: BotRequestId::from_uuid(Uuid::new_v4()),
        bot_id: bot.id,
        expected_revision: 1,
        config: config.clone(),
    };
    let BotResponse::Bot { bot: saved } = f.gateway.window_command(update.clone()).unwrap() else {
        panic!()
    };
    assert_eq!(saved.config, config);
    assert_eq!(saved.revision, 2);
    assert_eq!(saved.policy_revision, 1);
    // A missing replacement Role refuses the whole form, including its name.
    f.store.deleted.store(true, Ordering::Relaxed);
    let mut rejected = config.clone();
    rejected.name = "Must not land".into();
    rejected.role = bot.config.role.clone();
    assert_eq!(
        f.gateway
            .window_command(BotCommand::Update {
                request_id: BotRequestId::from_uuid(Uuid::new_v4()),
                bot_id: bot.id,
                expected_revision: 2,
                config: rejected,
            })
            .err(),
        Some(Error::NotAllowed)
    );
    f.store.deleted.store(false, Ordering::Relaxed);
    let pause = BotCommand::Pause {
        request_id: BotRequestId::from_uuid(Uuid::new_v4()),
        bot_id: bot.id,
        expected_revision: 2,
    };
    let BotResponse::Bot { bot: paused } = f.gateway.window_command(pause.clone()).unwrap() else {
        panic!()
    };
    assert!(paused.paused);
    assert_eq!(paused.revision, 3);
    let BotResponse::Bot { bot: replay } = f.gateway.window_command(update.clone()).unwrap() else {
        panic!()
    };
    assert_eq!(replay, saved);
    let mut changed = update.clone();
    if let BotCommand::Update { config, .. } = &mut changed {
        config.name.push('!');
    }
    assert_eq!(
        f.gateway.window_command(changed).err(),
        Some(Error::Conflict)
    );
    let stale = BotCommand::Update {
        request_id: BotRequestId::from_uuid(Uuid::new_v4()),
        bot_id: bot.id,
        expected_revision: 1,
        config,
    };
    assert_eq!(
        f.gateway.window_command(stale).err(),
        Some(Error::RevisionConflict)
    );
    let BotResponse::Bot { bot: replay } = f.gateway.window_command(pause).unwrap() else {
        panic!()
    };
    assert_eq!(replay, paused);
}

#[test]
fn selected_folder_disappearance_does_not_block_other_settings_and_knowledge_is_independent() {
    use koru_ai_core::bots::*;
    let f = Fixture::new();
    let path = if cfg!(windows) {
        PathBuf::from(r"C:\project")
    } else {
        PathBuf::from("/project")
    };
    f.fs.entries.lock().unwrap().insert(path.clone(), true);
    let mut request = form(&f);
    if let BotCommand::CreateBotWithRole { folder, .. } = &mut request {
        *folder = BotFolderChoice::Selected {
            path: path.to_str().unwrap().into(),
        };
    }
    let bot = creation(f.gateway.window_command(request).unwrap())
        .bot
        .unwrap();
    f.fs.entries.lock().unwrap().remove(&path);
    let mut config = bot.config.clone();
    config.name = "Still editable".into();
    let BotResponse::Bot { bot: saved } = f
        .gateway
        .window_command(BotCommand::Update {
            request_id: BotRequestId::from_uuid(Uuid::new_v4()),
            bot_id: bot.id,
            expected_revision: 1,
            config: config.clone(),
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(saved.config, config);
    let pages = vec![BotPage {
        workspace: f.store.home,
        document: bot.config.role.document,
    }];
    let knowledge = BotCommand::SetKnowledge {
        request_id: BotRequestId::from_uuid(Uuid::new_v4()),
        bot_id: bot.id,
        expected_revision: 2,
        pages: pages.clone(),
    };
    let BotResponse::KnowledgeSaved { receipt } =
        f.gateway.window_command(knowledge.clone()).unwrap()
    else {
        panic!()
    };
    assert_eq!(receipt.pages, pages);
    assert_eq!(receipt.revision, 3);
    f.store.deleted.store(true, Ordering::Relaxed);
    let BotResponse::KnowledgeSaved { receipt: again } =
        f.gateway.window_command(knowledge).unwrap()
    else {
        panic!()
    };
    assert_eq!(again, receipt);
    let BotResponse::Library { library } = f
        .gateway
        .window_command(BotCommand::ReadLibrary { bot_id: bot.id })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(library.role, bot.config.role);
    assert_eq!(library.knowledge, pages);
}

#[test]
fn team_pause_uses_engine_revision_and_preserves_individual_pause() {
    use koru_ai_core::bots::*;
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    f.gateway
        .window_command(BotCommand::Pause {
            request_id: BotRequestId::from_uuid(Uuid::new_v4()),
            bot_id: bot.id,
            expected_revision: 1,
        })
        .unwrap();
    let pause = BotCommand::PauseTeam {
        request_id: BotRequestId::from_uuid(Uuid::new_v4()),
        expected_team_revision: 0,
    };
    let BotResponse::TeamPause { team } = f.gateway.window_command(pause.clone()).unwrap() else {
        panic!()
    };
    assert!(team.paused);
    assert_eq!(team.stage, TeamPauseStage::Acknowledged);
    let BotResponse::TeamPause { team: resumed } = f
        .gateway
        .window_command(BotCommand::ResumeTeam {
            request_id: BotRequestId::from_uuid(Uuid::new_v4()),
            expected_team_revision: 1,
        })
        .unwrap()
    else {
        panic!()
    };
    assert!(!resumed.paused);
    assert_eq!(resumed.revision, 2);
    let BotResponse::TeamPause { team: replay } = f.gateway.window_command(pause).unwrap() else {
        panic!()
    };
    assert_eq!(replay, team);
    assert_eq!(
        f.gateway
            .window_command(BotCommand::PauseTeam {
                request_id: BotRequestId::from_uuid(Uuid::new_v4()),
                expected_team_revision: 0,
            })
            .err(),
        Some(Error::TeamRevisionConflict)
    );
    let BotResponse::Bot { bot } = f
        .gateway
        .window_command(BotCommand::Read { bot_id: bot.id })
        .unwrap()
    else {
        panic!()
    };
    assert!(bot.paused);
    assert_eq!(bot.revision, 2);
}

#[test]
fn existing_form_creates_role_and_bot_without_legacy_database_and_retry_survives_removal() {
    use koru_ai_core::bots::*;
    let f = Fixture::new();
    let request = form(&f);
    let done = creation(f.gateway.window_command(request.clone()).unwrap());
    assert_eq!(done.stage, BotCreationStage::Completed);
    let bot = done.bot.as_ref().unwrap();
    assert_eq!(bot.config.avatar, Some(4));
    assert_eq!(
        bot.config.model.requested_model,
        koru_ai_core::AGENT_DEFAULT_MODEL
    );
    assert_eq!(done.role_page, Some(bot.config.role.clone()));
    assert!(
        !serde_json::to_string(&done)
            .unwrap()
            .contains("private_role_marker")
    );
    f.store.deleted.store(true, Ordering::Relaxed);
    f.fs.entries
        .lock()
        .unwrap()
        .remove(&folders::managed_path(&f.gateway.root, bot.id.as_uuid()));
    let retry = creation(f.gateway.window_command(request.clone()).unwrap());
    assert_eq!(retry.bot.unwrap().main_chat, bot.main_chat);
    assert_eq!(f.store.writes.load(Ordering::Relaxed), 1);
    let mut changed = request;
    if let BotCommand::CreateBotWithRole { config, .. } = &mut changed {
        config.name.push('!');
    }
    assert_eq!(
        f.gateway.window_command(changed).err(),
        Some(Error::Conflict)
    );
    let BotResponse::List { bots } = f
        .gateway
        .window_command(BotCommand::List {
            workspace: f.store.home,
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(bots.len(), 1);
    assert_eq!(bots[0].id, bot.id);
}

#[test]
fn existing_progress_form_retries_lost_reply_and_reports_abandoned_leftovers() {
    use koru_ai_core::bots::*;
    for abandon in [false, true] {
        let f = Fixture::new();
        f.store.lost.store(true, Ordering::Relaxed);
        let incomplete = creation(f.gateway.window_command(form(&f)).unwrap());
        assert_eq!(incomplete.stage, BotCreationStage::Incomplete);
        assert_eq!(incomplete.phase, BotCreationPhase::RolePending);
        assert!(incomplete.retryable && incomplete.role_page_unconfirmed);
        let request = if abandon {
            BotCommand::AbandonBotCreation {
                request_id: incomplete.request_id,
            }
        } else {
            BotCommand::RetryBotCreation {
                request_id: incomplete.request_id,
            }
        };
        let answer = creation(f.gateway.window_command(request).unwrap());
        assert_eq!(f.store.writes.load(Ordering::Relaxed), 1);
        if abandon {
            assert_eq!(answer.stage, BotCreationStage::Abandoned);
            assert_eq!(answer.phase, BotCreationPhase::RolePending);
            assert!(answer.role_page_unconfirmed);
            let leftovers = answer.left_behind.unwrap();
            assert_eq!(leftovers.role_page, incomplete.role_page);
            assert!(leftovers.folder.is_some());
            assert!(answer.bot.is_none());
        } else {
            assert_eq!(answer.stage, BotCreationStage::Completed);
        }
    }
}

#[test]
fn existing_form_pending_pages_and_shared_limits_are_preserved() {
    use koru_ai_core::bots::*;
    assert_eq!(folders::PATH_LIMIT, BOT_FOLDER_PATH_LIMIT);
    assert_eq!(c::MAX_RECORD_BYTES, BOT_RECEIPT_OUTCOME_LIMIT);
    let f = Fixture::new();
    let BotResponse::Creations { page } = f
        .gateway
        .window_command(BotCommand::ListBotCreations {
            workspace: f.store.home,
            cursor: None,
            limit: 1,
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(page.creations.len(), 1);
    assert_eq!(page.creations[0].phase, BotCreationPhase::Intent);
    assert!(page.creations[0].role_page.is_none());
    assert!(page.next_cursor.is_none());
    assert_eq!(f.store.writes.load(Ordering::Relaxed), 0);
    assert_eq!(f.fs.creates.load(Ordering::Relaxed), 0);
}

#[test]
fn restored_role_retries_same_creation_and_definite_refusal_is_not_a_leftover() {
    use koru_ai_core::bots::*;
    let f = Fixture::new();
    f.store.trashed.store(true, Ordering::Relaxed);
    let parked = creation(f.gateway.window_command(form(&f)).unwrap());
    assert_eq!(parked.reason, Some(BotCreationReason::RoleUnavailable));
    assert!(parked.retryable);
    f.store.trashed.store(false, Ordering::Relaxed);
    let restored = creation(
        f.gateway
            .window_command(BotCommand::RetryBotCreation {
                request_id: parked.request_id,
            })
            .unwrap(),
    );
    assert_eq!(restored.stage, BotCreationStage::Completed);
    assert_eq!(f.store.writes.load(Ordering::Relaxed), 1);

    let f = Fixture::new();
    f.store.lost.store(true, Ordering::Relaxed);
    let parked = f.drive(f.record.clone()).unwrap();
    f.gateway
        .creation_advance(
            f.store.owner,
            &parked,
            Step::Failed {
                reason: Failure::StoreRefused,
            },
        )
        .unwrap();
    let abandoned = creation(
        f.gateway
            .window_command(BotCommand::AbandonBotCreation {
                request_id: BotRequestId::from_uuid(parked.request),
            })
            .unwrap(),
    );
    assert!(abandoned.role_page.is_none());
    assert!(abandoned.left_behind.unwrap().role_page.is_none());
}

#[test]
fn window_reads_the_actual_first_input_freeze_time() {
    use koru_ai_core::bots::*;
    let f = Fixture::new();
    let done = creation(f.gateway.window_command(form(&f)).unwrap());
    let bot = done.bot.unwrap();
    assert!(bot.folder_frozen_at_ms.is_none());
    let scope = f.store.owner.user_scope.to_storage();
    (f.gateway.call)(person::Request {
        owner: scope.clone(),
        sender: format!("person:{scope}"),
        command: Command::Send {
            envelope: koru_bots_engine::messages::Envelope {
                id: Uuid::new_v4(),
                to: bot.id.as_uuid(),
                thread: Some(bot.main_chat.as_uuid()),
                in_reply_to: None,
                body: "synthetic task".into(),
                attachments: vec![],
                references: vec![],
                deadline_ms: None,
                reply_to: Default::default(),
            },
        },
    })
    .unwrap();
    let BotResponse::Bot { bot: read } = f
        .gateway
        .window_command(BotCommand::Read { bot_id: bot.id })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(read.folder_frozen_at_ms, Some(2));
}

#[test]
fn window_progress_reconciles_a_concurrent_abandonment_without_input_conflict() {
    use koru_ai_core::bots::*;
    let f = Fixture::new();
    f.abandon.store(true, Ordering::Relaxed);
    let answer = creation(f.gateway.window_command(form(&f)).unwrap());
    assert_eq!(answer.stage, BotCreationStage::Abandoned);
    assert_eq!(answer.phase, BotCreationPhase::RolePending);
    assert!(answer.bot.is_none());
    assert!(answer.left_behind.unwrap().role_page.is_some());
}

#[test]
fn pending_window_listing_follows_byte_limited_pages_without_hiding_creations() {
    use koru_ai_core::bots::*;
    let f = Fixture::new();
    let mut definition = f.record.definition.clone();
    definition.access.selected_pages = (0..1024).map(|_| DocumentId::new()).collect();
    for _ in 1..c::MAX_PENDING_PER_OWNER {
        f.gateway
            .creation_call(
                f.store.owner,
                c::Command::Begin {
                    request: Uuid::new_v4(),
                    bot: Uuid::new_v4(),
                    source_hash: blake3::hash(b"synthetic large selection")
                        .to_hex()
                        .to_string(),
                    definition: definition.clone(),
                    authored: None,
                    seed: None,
                },
            )
            .unwrap();
    }
    let BotResponse::Creations { page } = f
        .gateway
        .window_command(BotCommand::ListBotCreations {
            workspace: f.store.home,
            cursor: None,
            limit: 100,
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(page.creations.len(), c::MAX_PENDING_PER_OWNER);
    assert!(page.next_cursor.is_none());
}
