// SPDX-License-Identifier: MPL-2.0
//! "Change folder before the first task" checks the new folder off the
//! writer (P1.8): it must exist, be a folder and not be Koru's own data,
//! and Update binds it where it resolves.
use super::*;
use koru_ai_core::{ModelSelection, ProfileId};
use std::path::{Path, PathBuf};

fn at(windows: &str, unix: &str) -> PathBuf {
    PathBuf::from(if cfg!(windows) { windows } else { unix })
}
fn existing() -> PathBuf {
    at(r"D:\work\existing", "/work/existing")
}
fn shortcut() -> PathBuf {
    at(r"D:\work\shortcut", "/work/shortcut")
}
fn ai_root() -> PathBuf {
    at(r"D:\koru\ai", "/koru/ai")
}
fn store_root() -> PathBuf {
    at(r"D:\koru\store", "/koru/store")
}

/// A few existing folders and one link to `existing`; nothing else exists.
struct Folders(Vec<PathBuf>);
impl Folders {
    fn new() -> Self {
        let bots = ai_root().join("bots");
        Self(vec![
            existing(),
            ai_root(),
            bots.clone(),
            bots.join("other").join("work"),
            store_root(),
        ])
    }
}
impl BotFolderSystem for Folders {
    fn canonical(&self, path: &Path) -> std::io::Result<PathBuf> {
        if path == shortcut() {
            Ok(existing())
        } else if self.0.iter().any(|folder| folder == path) {
            Ok(path.to_owned())
        } else {
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        }
    }
    fn occupied(&self, path: &Path) -> std::io::Result<bool> {
        Ok(self.0.iter().any(|folder| folder == path))
    }
    fn create_dir(&self, _: &Path) -> std::io::Result<()> {
        panic!("validating a folder never creates one")
    }
    fn is_dir(&self, path: &Path) -> std::io::Result<bool> {
        Ok(self.0.iter().any(|folder| folder == path))
    }
    fn is_empty_dir(&self, _: &Path) -> std::io::Result<bool> {
        panic!("validating a folder never lists one")
    }
}

struct Owner(BotOwner);
impl BotAuthority for Owner {
    fn owner(&self) -> Result<BotOwner, BotError> {
        Ok(self.0)
    }
    fn workspace(&self, _: WorkspaceId) -> Result<ProfileId, BotError> {
        Err(BotError::Forbidden)
    }
    fn page(&self, _: &BotPage) -> Result<(), BotError> {
        Err(BotError::Forbidden)
    }
}

fn update(folder: &str) -> BotCommand {
    let workspace = WorkspaceId::new();
    BotCommand::Update {
        request_id: BotRequestId::mint(),
        bot_id: BotId::mint(),
        expected_revision: 1,
        config: BotConfig {
            name: "Fixture".into(),
            role: BotPage {
                workspace,
                document: DocumentId::new(),
            },
            working_folder: folder.into(),
            model: ModelSelection {
                provider: AiProviderId::parse("agent-codex").unwrap(),
                requested_model: "fixture".into(),
                default_reasoning: ReasoningChoice::Default,
            },
            mode: None,
            fast: None,
        },
    }
}

/// Prepare an Update whose folder changed to `folder` (or one that did not
/// change it), and answer the command as the writer will see it.
fn prepare(folder: Option<&str>) -> (BotCommand, ApiResult<()>) {
    let (reply, _) = sync_channel(1);
    let request = update(folder.unwrap_or(existing().to_str().unwrap()));
    let job = BotPreparationJob {
        command: WorkerCommand::Bots {
            context: RequestContext {
                principal: koru_ai_api::AiPrincipal {
                    label: "fixture".into(),
                    scopes: vec![AiScope::ManageBots],
                },
                trace_id: "fixture".into(),
                deadline_millis: None,
            }
            .into(),
            request,
            reply,
        },
        authority: Arc::new(Owner(BotOwner {
            user_scope: UserScopeId::new(),
            device: DeviceId::from_uuid(Uuid::new_v4()),
        })),
        workspaces: vec![],
        pages: vec![],
        chats: vec![],
        model: None,
        capture: None,
        chat_snapshot: None,
        terminal_question: None,
        delegate: None,
        runtime_ready: false,
        folder: folder.map(|path| {
            (
                Arc::new(Folders::new()) as Arc<dyn BotFolderSystem>,
                creation::ReservedFolders {
                    ai_root: ai_root(),
                    store_root: Some(store_root()),
                },
                path.to_owned(),
            )
        }),
    };
    let (command, proof) = job.run(None);
    let WorkerCommand::Bots { request, .. } = command else {
        panic!("the same command comes back")
    };
    (request, proof.ok().expect("the authority answered").runtime)
}

fn folder_of(command: &BotCommand) -> &str {
    match command {
        BotCommand::Update { config, .. } => &config.working_folder,
        _ => panic!("an Update"),
    }
}

#[test]
fn a_changed_folder_must_exist_and_be_a_folder() {
    let missing = at(r"D:\work\missing", "/work/missing");
    assert!(prepare(None).1.is_ok());
    assert!(prepare(Some(existing().to_str().unwrap())).1.is_ok());
    assert_eq!(
        prepare(Some(missing.to_str().unwrap())).1.unwrap_err().code,
        "invalid_request"
    );
}

#[test]
fn update_binds_the_folder_where_it_resolves() {
    let (command, runtime) = prepare(Some(shortcut().to_str().unwrap()));
    assert!(runtime.is_ok());
    assert_eq!(folder_of(&command), existing().to_str().unwrap());
    // An unchanged folder is not looked at and stays as it is.
    let (command, _) = prepare(None);
    assert_eq!(folder_of(&command), existing().to_str().unwrap());
}

#[test]
fn koru_s_own_data_is_never_a_working_folder() {
    for reserved in [
        ai_root(),
        store_root(),
        ai_root().join("bots"),
        ai_root().join("bots").join("other").join("work"),
    ] {
        let (command, runtime) = prepare(Some(reserved.to_str().unwrap()));
        let refused = runtime.unwrap_err();
        assert_eq!(
            refused.code,
            koru_ai_wire::bot_creation_codes::FOLDER_RESERVED,
            "{reserved:?}"
        );
        assert!(!refused.retryable);
        // A refused folder is never bound.
        assert_eq!(folder_of(&command), reserved.to_str().unwrap());
    }
}
