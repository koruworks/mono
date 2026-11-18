// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::bots_engine::attachments::Operation;
use koru_ai_core::{AttachmentSource, ChatAttachmentId, ChatId, StoreContextPort};
use koru_ai_wire::bot_threads::{Command as C, Response as R};
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

#[test]
fn uploaded_files_bind_to_real_messages_replay_and_recall_without_legacy_state() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let id = ChatAttachmentId::new();
    run(&f, bot.main_chat, upload(id)).unwrap();
    let command = C::Send {
        id: Uuid::new_v4(),
        bot: bot.id,
        thread: bot.main_chat,
        body: "file".into(),
        in_reply_to: None,
        attachments: vec![serde_json::json!(id.as_uuid())],
        references: vec![],
    };
    let R::Accepted {
        message, replayed, ..
    } = f
        .gateway
        .thread_command(&principal(), command.clone(), |_| Ok(()))
        .ok()
        .unwrap()
    else {
        panic!()
    };
    assert!(!replayed);
    let R::Accepted {
        message: again,
        replayed,
        ..
    } = f
        .gateway
        .thread_command(&principal(), command, |_| Ok(()))
        .ok()
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(message, again);
    assert!(replayed);
    assert_eq!(
        run(&f, bot.main_chat, Operation::Remove(id)).unwrap_err(),
        Error::NotAllowed
    );
    assert!(
        run(
            &f,
            bot.main_chat,
            Operation::Restage {
                id,
                new_id: ChatAttachmentId::new()
            }
        )
        .unwrap()
        .is_some()
    );
    let R::History { messages, .. } = f
        .gateway
        .thread_command(
            &principal(),
            C::History {
                thread: bot.main_chat,
                before: None,
            },
            |_| Ok(()),
        )
        .ok()
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(messages.len(), 1);
    let attachment: koru_ai_core::ChatAttachment =
        serde_json::from_value(messages[0].content.attachments[0].clone()).unwrap();
    assert_eq!(attachment.id, id);
    assert_eq!(attachment.message_id.unwrap().as_uuid(), message);
    assert_eq!(attachment.ordinal, Some(0));
}

#[test]
fn workspace_source_is_fresh_for_new_sends_but_cannot_revoke_a_retained_receipt() {
    let mut f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let source = *f.store.origins.lock().unwrap().keys().next().unwrap();
    let first = ChatAttachmentId::new();
    let second = ChatAttachmentId::new();
    for id in [first, second] {
        run(
            &f,
            bot.main_chat,
            Operation::Workspace(koru_ai_wire::FromWorkspaceAttachmentRequest {
                id,
                workspace: f.store.home,
                document: source,
            }),
        )
        .unwrap();
    }
    let command = |id| C::Send {
        id: Uuid::new_v4(),
        bot: bot.id,
        thread: bot.main_chat,
        body: "workspace file".into(),
        in_reply_to: None,
        attachments: vec![serde_json::json!(id)],
        references: vec![],
    };
    let sent = command(first.as_uuid());
    assert!(
        f.gateway
            .thread_command(&principal(), sent.clone(), |_| Ok(()))
            .is_ok()
    );
    f.store.deleted.store(true, Ordering::Relaxed);
    assert!(matches!(
        f.gateway
            .thread_command(&principal(), command(second.as_uuid()), |_| Ok(())),
        Err(crate::bots_engine::window::readiness::Refusal::Engine(
            Error::NotAllowed
        ))
    ));
    f.gateway.files = Arc::new(crate::bots_engine::attachments::files::tests::Memory::new(
        &f.gateway.root,
    ));
    assert!(matches!(
        f.gateway.thread_command(&principal(), sent, |_| Ok(())),
        Ok(R::Accepted { replayed: true, .. })
    ));
}
