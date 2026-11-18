// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::{
    ProfileId, WorkspaceId,
    bots::{BotAuthoritySnapshot, BotOwner},
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use uuid::Uuid;

struct Authority {
    owner: BotOwner,
    home: WorkspaceId,
    page_live: AtomicBool,
    owner_moved: AtomicBool,
    reads: AtomicUsize,
}
impl BotAuthority for Authority {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(self.owner)
    }
    fn workspace(&self, workspace: WorkspaceId) -> Result<ProfileId, BotError> {
        if workspace == self.home {
            Ok(ProfileId::from_uuid(Uuid::new_v4()))
        } else {
            Err(BotError::Forbidden)
        }
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        if page.workspace == self.home && self.page_live.load(Ordering::Relaxed) {
            Ok(())
        } else {
            Err(BotError::NotFound)
        }
    }
    fn inspect(
        &self,
        workspaces: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<BotAuthoritySnapshot, BotError> {
        let mut owner = self.owner;
        if self.owner_moved.load(Ordering::Relaxed) {
            owner.user_scope = koru_store_core::UserScopeId::from_uuid(Uuid::new_v4());
        }
        Ok(BotAuthoritySnapshot {
            owner,
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
struct Fixture {
    gateway: Gateway,
    authority: Arc<Authority>,
    changed: Arc<AtomicUsize>,
    receipt: Arc<Mutex<Value>>,
    command: Command,
}
impl Fixture {
    fn new() -> Self {
        let home = WorkspaceId::from_uuid(Uuid::new_v4());
        let authority = Arc::new(Authority {
            owner: BotOwner {
                user_scope: koru_store_core::UserScopeId::from_uuid(Uuid::new_v4()),
                device: koru_delegate_core::DeviceId::new(),
            },
            home,
            page_live: AtomicBool::new(true),
            owner_moved: AtomicBool::new(false),
            reads: AtomicUsize::new(0),
        });
        let changed = Arc::new(AtomicUsize::new(0));
        let counter = changed.clone();
        let receipt = Arc::new(Mutex::new(Value::Null));
        let retained = receipt.clone();
        let scope = authority.owner.user_scope.to_storage();
        let gateway = Gateway {
            capacity: capacity::Refresh::new(Arc::new(|| Ok(()))),
            creation: Arc::new(|_| Err(Error::EngineUnavailable)),
            root: std::path::PathBuf::new(),
            folders: Arc::new(koru_bots_engine::folders::Native),
            authority: authority.clone(),
            permits: Arc::new(tokio::sync::Semaphore::new(8)),
            call: Arc::new(move |request| {
                assert_eq!(request.owner, scope);
                assert_eq!(request.sender, format!("person:{scope}"));
                match request.command {
                    Command::Read {
                        query: Query::ChangeReceipt { .. },
                    } => Ok(person::Reply(retained.lock().unwrap().clone())),
                    Command::Change { .. } => {
                        counter.fetch_add(1, Ordering::Relaxed);
                        let response =
                            serde_json::json!({"answer":{"accepted":true},"replayed":false});
                        *retained.lock().unwrap() =
                            serde_json::json!({"answer":{"accepted":true},"replayed":true});
                        Ok(person::Reply(response))
                    }
                    _ => panic!("unexpected request"),
                }
            }),
        };
        let command = Command::Change {
            id: Uuid::new_v4(),
            action: Change::Create {
                bot: Uuid::new_v4(),
                definition: person::Definition {
                    name: "synthetic".into(),
                    avatar: None,
                    configuration: koru_bots_engine::configuration::Configuration {
                        provider: "claude-acp".parse().unwrap(),
                        selection: Default::default(),
                        working_folder: None,
                    },
                    access: koru_delegate_core::CapturedBotPolicy {
                        workspace: home,
                        destination: home,
                        access: koru_delegate_core::BotAccess::Default,
                        selected_pages: Default::default(),
                        connections: Default::default(),
                    },
                    role: Some(Uuid::new_v4()),
                    knowledge: vec![],
                    memory: None,
                },
            },
        };
        Self {
            gateway,
            authority,
            changed,
            receipt,
            command,
        }
    }
    fn principal() -> GatewayPrincipal {
        GatewayPrincipal {
            label: "first-party".into(),
            scopes: vec!["bots.manage".into()],
        }
    }
}
#[test]
fn read_run_and_unvouched_callers_cannot_reach_the_owner_carrier() {
    let f = Fixture::new();
    for scopes in [
        vec![],
        vec!["bots.read".into()],
        vec!["bots.run".into()],
        vec!["ai.chat".into()],
    ] {
        assert_eq!(
            f.gateway
                .execute(
                    &GatewayPrincipal {
                        label: "synthetic".into(),
                        scopes
                    },
                    f.command.clone()
                )
                .err(),
            Some(Error::NotAllowed)
        );
    }
    assert_eq!(f.authority.reads.load(Ordering::Relaxed), 0);
}
#[test]
fn fresh_page_checks_precede_creation_but_a_retained_receipt_survives_page_removal() {
    let f = Fixture::new();
    f.authority.page_live.store(false, Ordering::Relaxed);
    assert_eq!(
        f.gateway
            .execute(&Fixture::principal(), f.command.clone())
            .err(),
        Some(Error::NotAllowed)
    );
    assert_eq!(f.changed.load(Ordering::Relaxed), 0);
    f.authority.page_live.store(true, Ordering::Relaxed);
    assert_eq!(
        f.gateway
            .execute(&Fixture::principal(), f.command.clone())
            .unwrap()["replayed"],
        false
    );
    f.authority.page_live.store(false, Ordering::Relaxed);
    assert_eq!(
        f.gateway
            .execute(&Fixture::principal(), f.command.clone())
            .unwrap(),
        *f.receipt.lock().unwrap()
    );
    assert_eq!(f.changed.load(Ordering::Relaxed), 1);
}
#[test]
fn an_owner_change_during_store_inspection_never_commits() {
    let f = Fixture::new();
    f.authority.owner_moved.store(true, Ordering::Relaxed);
    assert_eq!(
        f.gateway
            .execute(&Fixture::principal(), f.command.clone())
            .err(),
        Some(Error::NotAllowed)
    );
    assert_eq!(f.changed.load(Ordering::Relaxed), 0);
}
#[test]
fn public_command_has_no_sender_or_bootstrap_authority_and_route_uses_vouched_scope() {
    let f = Fixture::new();
    let mut body = serde_json::to_value(&f.command).unwrap();
    body["owner"] = serde_json::json!("spoofed");
    assert!(serde_json::from_value::<Command>(body).is_err());
    let route = crate::ai::ROUTES
        .iter()
        .find(|r| r.operation_id == "bots_engine_command")
        .unwrap();
    assert_eq!(route.scope, koru_ai_api::AiScope::ManageBots);
    assert!(
        !crate::effective_scopes(&["bots.manage".into()], crate::Admission::Ordinary)
            .contains(&"bots.manage".into())
    );
}

struct Folders {
    root: std::path::PathBuf,
    project: std::path::PathBuf,
    available: AtomicBool,
}
impl Folders {
    fn new() -> Self {
        let base = if cfg!(windows) {
            std::path::PathBuf::from(r"C:\synthetic")
        } else {
            std::path::PathBuf::from("/synthetic")
        };
        Self {
            root: base.join("data"),
            project: base.join("project"),
            available: AtomicBool::new(true),
        }
    }
}
impl koru_bots_engine::folders::Folders for Folders {
    fn canonical(&self, path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
        if path == self.project && !self.available.load(Ordering::Relaxed) {
            return Err(std::io::ErrorKind::NotFound.into());
        }
        if path.file_name().is_some_and(|name| name == "alias") {
            Ok(self.project.clone())
        } else {
            Ok(path.to_owned())
        }
    }
    fn directory(&self, _: &std::path::Path) -> std::io::Result<bool> {
        Ok(true)
    }
    fn occupied(&self, _: &std::path::Path) -> std::io::Result<bool> {
        panic!("configuration must not create")
    }
    fn create(&self, _: &std::path::Path) -> std::io::Result<()> {
        panic!("configuration must not create")
    }
    fn empty(&self, _: &std::path::Path) -> std::io::Result<bool> {
        panic!("selected folders may contain work")
    }
}

#[test]
fn configuration_uses_shared_faces_and_requires_canonical_existing_nonreserved_folder() {
    let mut f = Fixture::new();
    let folders = Arc::new(Folders::new());
    f.gateway.root = folders.root.clone();
    f.gateway.folders = folders.clone();
    let Command::Change {
        action: Change::Create { definition, .. },
        ..
    } = &f.command
    else {
        panic!()
    };
    let mut config = definition.configuration.clone();
    assert_eq!(
        f.gateway
            .configuration(&config, Some(koru_ai_core::bots::BOT_AVATAR_COUNT))
            .err(),
        Some(Error::InvalidRequest)
    );
    config.working_folder = Some(folders.project.to_str().unwrap().into());
    assert!(f.gateway.configuration(&config, Some(7)).is_ok());
    config.working_folder = Some(
        folders
            .project
            .with_file_name("alias")
            .to_str()
            .unwrap()
            .into(),
    );
    assert_eq!(
        f.gateway.configuration(&config, None).err(),
        Some(Error::InvalidRequest)
    );
    config.working_folder = Some(folders.root.join("store").to_str().unwrap().into());
    assert_eq!(
        f.gateway.configuration(&config, None).err(),
        Some(Error::NotAllowed)
    );
    config.working_folder = Some("relative".into());
    assert_eq!(
        f.gateway.configuration(&config, None).err(),
        Some(Error::InvalidRequest)
    );
}

#[test]
fn committed_configuration_retry_does_not_reopen_a_removed_folder() {
    let mut f = Fixture::new();
    let folders = Arc::new(Folders::new());
    f.gateway.root = folders.root.clone();
    f.gateway.folders = folders.clone();
    let Command::Change {
        action: Change::Create { definition, .. },
        ..
    } = &mut f.command
    else {
        panic!()
    };
    definition.configuration.working_folder = Some(folders.project.to_str().unwrap().into());
    f.gateway
        .execute(&Fixture::principal(), f.command.clone())
        .unwrap();
    folders.available.store(false, Ordering::Relaxed);
    assert_eq!(
        f.gateway
            .execute(&Fixture::principal(), f.command.clone())
            .unwrap()["replayed"],
        true
    );
    assert_eq!(f.changed.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn window_failures_keep_existing_ui_codes_and_retryability() {
    for (error, code, retryable) in [
        (Error::Conflict, "request_conflict", false),
        (Error::RevisionConflict, "revision_conflict", false),
        (Error::TeamRevisionConflict, "team_revision_conflict", false),
        (Error::FolderFrozen, "folder_locked", false),
        (Error::NotAllowed, "wrong_owner", false),
        (Error::EngineUnavailable, "bot_unavailable", true),
    ] {
        let response = window_failed(error);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let failure: koru_ai_wire::Failure = serde_json::from_slice(&body).unwrap();
        assert_eq!(failure.code, code);
        assert_eq!(failure.retryable, retryable);
    }
}
