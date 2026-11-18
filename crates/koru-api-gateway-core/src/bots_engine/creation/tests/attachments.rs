// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::bots_engine::attachments::Operation;
use koru_ai_core::{AttachmentSource, ChatAttachmentId, ChatId, StoreContextPort};
struct Context;
impl StoreContextPort for Context {
    fn resolve_document(
        &self,
        _: DocumentId,
        _: Option<ProfileId>,
    ) -> Result<koru_ai_core::DocumentContext, koru_ai_core::AiError> {
        panic!("no document read")
    }
    fn create_document(
        &self,
        _: Uuid,
        _: &koru_ai_core::DocumentDestination,
        _: &str,
    ) -> Result<DocumentId, koru_ai_core::AiError> {
        panic!("no document write")
    }
    fn read_workspace_file(
        &self,
        _: WorkspaceId,
        _: DocumentId,
        _: ProfileId,
    ) -> Result<koru_ai_core::WorkspaceFileData, koru_ai_core::AiError> {
        Ok(koru_ai_core::WorkspaceFileData {
            file_name: "fixture.txt".into(),
            bytes: b"synthetic".to_vec(),
        })
    }
}
fn principal() -> crate::GatewayPrincipal {
    crate::GatewayPrincipal {
        label: "synthetic".into(),
        scopes: vec![koru_ai_api::AiScope::ManageBots.as_str().into()],
    }
}
fn upload(id: ChatAttachmentId) -> Operation {
    Operation::Upload {
        id,
        name: "fixture.txt".into(),
        source: AttachmentSource::Drop,
        bytes: bytes::Bytes::from_static(b"synthetic"),
    }
}
fn run(
    f: &Fixture,
    chat: ChatId,
    op: Operation,
) -> Result<Option<axum::response::Response>, Error> {
    f.gateway
        .attachment_operation(&principal(), chat, op, &Context)
}
#[test]
fn upload_retry_remove_and_engine_refusal_never_fall_through_to_legacy() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let id = ChatAttachmentId::new();
    assert!(run(&f, ChatId::new(), upload(id)).unwrap().is_none());
    assert!(run(&f, bot.main_chat, upload(id)).unwrap().is_some());
    assert!(run(&f, bot.main_chat, upload(id)).unwrap().is_some());
    assert!(
        run(
            &f,
            bot.main_chat,
            Operation::Read {
                id,
                thumbnail: false
            }
        )
        .unwrap()
        .is_some()
    );
    assert!(
        run(&f, bot.main_chat, Operation::Remove(id))
            .unwrap()
            .is_some()
    );
    assert_eq!(
        run(&f, bot.main_chat, upload(id)).unwrap_err(),
        Error::NotAllowed
    );
    assert_eq!(
        run(
            &f,
            bot.main_chat,
            Operation::Read {
                id,
                thumbnail: false
            }
        )
        .unwrap_err(),
        Error::NotAllowed
    );
    f.store.moved.store(true, Ordering::Relaxed);
    assert_eq!(
        run(&f, bot.main_chat, upload(ChatAttachmentId::new())).unwrap_err(),
        Error::NotAllowed
    );
}
#[test]
fn path_reference_does_not_read_disk_and_requires_first_party_scope() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let id = ChatAttachmentId::new();
    let request = koru_ai_wire::ReferenceAttachmentRequest {
        id,
        kind: koru_ai_core::AttachmentKind::Folder,
        path: if cfg!(windows) {
            r"C:\fixture\source"
        } else {
            "/fixture/source"
        }
        .into(),
    };
    assert!(
        run(&f, bot.main_chat, Operation::Reference(request.clone()))
            .unwrap()
            .is_some()
    );
    assert_eq!(
        run(
            &f,
            bot.main_chat,
            Operation::Read {
                id,
                thumbnail: false
            }
        )
        .unwrap_err(),
        Error::NotAllowed
    );
    let mut no_scope = principal();
    no_scope.scopes.clear();
    assert_eq!(
        f.gateway
            .attachment_operation(
                &no_scope,
                bot.main_chat,
                upload(ChatAttachmentId::new()),
                &Context
            )
            .unwrap_err(),
        Error::NotAllowed
    );
    let mut invalid = request;
    invalid.path = "relative/path".into();
    assert_eq!(
        run(&f, bot.main_chat, Operation::Reference(invalid)).unwrap_err(),
        Error::InvalidRequest
    );
}
#[test]
fn workspace_copy_rechecks_scope_before_reading_bytes() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let request = koru_ai_wire::FromWorkspaceAttachmentRequest {
        id: ChatAttachmentId::new(),
        workspace: f.store.home,
        document: DocumentId::new(),
    };
    assert!(
        run(&f, bot.main_chat, Operation::Workspace(request.clone()))
            .unwrap()
            .is_some()
    );
    let mut forbidden = request;
    forbidden.id = ChatAttachmentId::new();
    forbidden.workspace = WorkspaceId::new();
    assert_eq!(
        run(&f, bot.main_chat, Operation::Workspace(forbidden)).unwrap_err(),
        Error::NotAllowed
    );
}
