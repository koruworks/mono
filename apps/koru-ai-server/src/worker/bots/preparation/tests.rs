// SPDX-License-Identifier: MPL-2.0
//! "Change folder before the first task" checks the new folder off the
//! writer: it must exist and be a folder (P1.8).
use super::*;
use koru_ai_core::ProfileId;
use std::path::{Path, PathBuf};

/// One existing folder; everything else is missing.
struct OneFolder(PathBuf);
impl BotFolderSystem for OneFolder {
    fn canonical(&self, path: &Path) -> std::io::Result<PathBuf> {
        if path == self.0 {
            Ok(path.to_owned())
        } else {
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        }
    }
    fn occupied(&self, path: &Path) -> std::io::Result<bool> {
        Ok(path == self.0)
    }
    fn create_dir(&self, _: &Path) -> std::io::Result<()> {
        panic!("validating a folder never creates one")
    }
    fn is_dir(&self, path: &Path) -> std::io::Result<bool> {
        Ok(path == self.0)
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

fn runtime(folder: Option<&str>) -> ApiResult<()> {
    let existing = PathBuf::from(if cfg!(windows) {
        r"D:\work\existing"
    } else {
        "/work/existing"
    });
    let (reply, _) = sync_channel(1);
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
            request: BotCommand::Read {
                bot_id: BotId::mint(),
            },
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
        folder: folder.map(|path| {
            (
                Arc::new(OneFolder(existing.clone())) as Arc<dyn BotFolderSystem>,
                path.to_owned(),
            )
        }),
    };
    let (_, proof) = job.run(None);
    proof.ok().expect("the authority answered").runtime
}

#[test]
fn a_changed_folder_must_exist_and_be_a_folder() {
    let existing = if cfg!(windows) {
        r"D:\work\existing"
    } else {
        "/work/existing"
    };
    let missing = if cfg!(windows) {
        r"D:\work\missing"
    } else {
        "/work/missing"
    };
    assert!(runtime(None).is_ok());
    assert!(runtime(Some(existing)).is_ok());
    assert_eq!(runtime(Some(missing)).unwrap_err().code, "invalid_request");
}
