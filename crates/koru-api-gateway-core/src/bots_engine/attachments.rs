// SPDX-License-Identifier: MPL-2.0
//! Engine ownership for the shared attachment routes. Unknown ordinary chats
//! fall through; an engine refusal never writes a legacy attachment instead.
use super::*;
use koru_ai_core::{
    AttachmentKind, AttachmentSource, ChatAttachment, ChatAttachmentId, ChatId, Timestamp,
};
use koru_ai_wire::{ChatAttachmentResponse, ReferenceAttachmentRequest, RemoveAttachmentResponse};
use koru_bots_engine::attachments::{Command as Control, Record, Reply, Request};
use uuid::Uuid;
pub(super) mod files;
pub(super) type Call = Arc<dyn Fn(Request) -> Result<Reply, Error> + Send + Sync>;

pub(crate) enum Operation {
    Upload {
        id: ChatAttachmentId,
        name: String,
        source: AttachmentSource,
        bytes: bytes::Bytes,
    },
    Reference(ReferenceAttachmentRequest),
    Remove(ChatAttachmentId),
    Read {
        id: ChatAttachmentId,
        thumbnail: bool,
    },
    Restage {
        id: ChatAttachmentId,
        new_id: ChatAttachmentId,
    },
    Workspace(koru_ai_wire::FromWorkspaceAttachmentRequest),
}
impl Gateway {
    pub(super) fn send_attachments(
        &self,
        scope: &str,
        envelope: &koru_bots_engine::messages::Envelope,
        home: koru_ai_core::WorkspaceId,
        pages: &mut Vec<BotPage>,
    ) -> Result<Option<Value>, Error> {
        match self.attachment_call(
            scope,
            Control::SendReceipt {
                envelope: envelope.clone(),
            },
        )? {
            Reply::Receipt(Some(receipt)) => {
                return serde_json::to_value(receipt)
                    .map(Some)
                    .map_err(|_| Error::EngineUnavailable);
            }
            Reply::Receipt(None) => (),
            _ => return Err(Error::EngineUnavailable),
        }
        let thread = envelope.thread.ok_or(Error::InvalidRequest)?;
        if envelope.attachments.len() > koru_ai_core::attachments::MAX_ATTACHMENTS_PER_MESSAGE {
            return Err(Error::InvalidRequest);
        }
        for value in &envelope.attachments {
            let id: Uuid =
                serde_json::from_value(value.clone()).map_err(|_| Error::InvalidRequest)?;
            let record = self.attachment_record(scope, thread, ChatAttachmentId::from_uuid(id))?;
            if record.attachment.message_id.is_some() {
                return Err(Error::NotAllowed);
            }
            if let Some(origin) = &record.attachment.origin {
                if origin.workspace_id != home {
                    return Err(Error::NotAllowed);
                }
                pages.push(BotPage {
                    workspace: home,
                    document: origin.document_id,
                });
            }
            files::input_file(self.files.as_ref(), &self.root, &record)?;
        }
        Ok(None)
    }
    pub(crate) fn input_files(
        &self,
        grant: &koru_bots_engine::authority::input::Grant,
    ) -> Result<koru_bots_engine::workspace::input::Prepared, Error> {
        let prepared = koru_bots_engine::workspace::input::Prepared {
            delivery_hash: koru_bots_engine::workspace::input::digest(&grant.delivery)?,
            files: grant
                .files
                .iter()
                .map(|record| {
                    if record.thread != grant.resolution.thread {
                        return Err(Error::NotAllowed);
                    }
                    files::input_file(self.files.as_ref(), &self.root, record)
                })
                .collect::<Result<_, _>>()?,
        };
        prepared.validate(&grant.delivery)?;
        Ok(prepared)
    }
    fn attachment_call(&self, owner: &str, command: Control) -> Result<Reply, Error> {
        (self.attachments)(Request {
            owner: owner.to_owned(),
            sender: format!("person:{owner}"),
            command,
        })
    }
    fn attachment_record(
        &self,
        owner: &str,
        thread: Uuid,
        id: ChatAttachmentId,
    ) -> Result<Record, Error> {
        match self.attachment_call(
            owner,
            Control::Read {
                thread,
                id: id.as_uuid(),
            },
        )? {
            Reply::Record(Some(record)) => Ok(record),
            Reply::Record(None) => Err(Error::NotAllowed),
            _ => Err(Error::EngineUnavailable),
        }
    }
    fn stage_record(&self, owner: &str, record: Record) -> Result<Response, Error> {
        match self.attachment_call(owner, Control::Stage(record))? {
            Reply::Record(Some(record)) => Ok(Json(ChatAttachmentResponse {
                attachment: record.attachment,
            })
            .into_response()),
            _ => Err(Error::EngineUnavailable),
        }
    }
    pub(in crate::bots_engine) fn attachment_operation(
        &self,
        principal: &GatewayPrincipal,
        chat: ChatId,
        operation: Operation,
        context: &dyn koru_ai_core::StoreContextPort,
    ) -> Result<Option<Response>, Error> {
        let owner = self.authority.owner().map_err(authority_error)?;
        let scope = owner.user_scope.to_storage();
        match self.attachment_call(
            &scope,
            Control::Probe {
                thread: chat.as_uuid(),
            },
        )? {
            Reply::Thread(false) => return Ok(None),
            Reply::Thread(true) => (),
            _ => return Err(Error::EngineUnavailable),
        }
        if !permitted(principal) {
            return Err(Error::NotAllowed);
        }
        let view: person::ThreadView = serde_json::from_value(self.execute_owner(
            owner,
            Command::Read {
                query: Query::Thread {
                    thread: chat.as_uuid(),
                },
            },
        )?)
        .map_err(|_| Error::EngineUnavailable)?;
        let bot: person::Bot = serde_json::from_value(self.execute_owner(
            owner,
            Command::Read {
                query: Query::Bot {
                    bot: view.thread.bot,
                },
            },
        )?)
        .map_err(|_| Error::EngineUnavailable)?;
        let home = bot.definition.access.workspace;
        let snapshot = self
            .authority
            .inspect(&[home], &[])
            .map_err(authority_error)?;
        if snapshot.owner != owner {
            return Err(Error::NotAllowed);
        }
        let profile = *snapshot
            .workspaces
            .get(&home)
            .ok_or(Error::NotAllowed)?
            .as_ref()
            .map_err(|e| authority_error(*e))?;
        let check_origin = |origin: &koru_ai_core::WorkspaceFileOrigin| -> Result<(), Error> {
            if origin.workspace_id != home {
                return Err(Error::NotAllowed);
            }
            let page = BotPage {
                workspace: home,
                document: origin.document_id,
            };
            let current = self
                .authority
                .inspect(&[home], &[page.clone()])
                .map_err(authority_error)?;
            if current.owner != owner {
                return Err(Error::NotAllowed);
            }
            current
                .workspaces
                .get(&home)
                .ok_or(Error::NotAllowed)?
                .as_ref()
                .map_err(|e| authority_error(*e))?;
            current
                .pages
                .get(&page)
                .ok_or(Error::NotAllowed)?
                .as_ref()
                .map_err(|e| authority_error(*e))?;
            Ok(())
        };
        let response = match operation {
            Operation::Workspace(request) => {
                let origin = koru_ai_core::WorkspaceFileOrigin {
                    workspace_id: request.workspace,
                    document_id: request.document,
                };
                check_origin(&origin)?;
                let file = context
                    .read_workspace_file(request.workspace, request.document, profile)
                    .map_err(|_| Error::NotAllowed)?;
                let mut checked = match koru_ai_server::check_attachment(
                    chat,
                    request.id,
                    &file.file_name,
                    AttachmentSource::Workspace,
                    &file.bytes,
                ) {
                    Ok(checked) => checked,
                    Err(error) => {
                        return Ok(Some((StatusCode::BAD_REQUEST, Json(error)).into_response()));
                    }
                };
                checked.record.attachment.origin = Some(origin);
                let record =
                    files::publish(self.files.as_ref(), &self.root, &checked, &file.bytes)?;
                self.stage_record(&scope, record)?
            }
            Operation::Upload {
                id,
                name,
                source,
                bytes,
            } => {
                if matches!(source, AttachmentSource::Path | AttachmentSource::Workspace) {
                    return Err(Error::InvalidRequest);
                }
                let checked =
                    match koru_ai_server::check_attachment(chat, id, &name, source, &bytes) {
                        Ok(checked) => checked,
                        Err(error) => {
                            return Ok(Some(
                                (StatusCode::BAD_REQUEST, Json(error)).into_response(),
                            ));
                        }
                    };
                let record = files::publish(self.files.as_ref(), &self.root, &checked, &bytes)?;
                self.stage_record(&scope, record)?
            }
            Operation::Reference(request) => {
                self.stage_record(&scope, reference(chat, request)?)?
            }
            Operation::Remove(id) => {
                match self.attachment_call(
                    &scope,
                    Control::Remove {
                        thread: chat.as_uuid(),
                        id: id.as_uuid(),
                    },
                )? {
                    Reply::Removed => (),
                    _ => return Err(Error::EngineUnavailable),
                }
                // Tombstones prevent late publication. Physical collection is
                // deliberately separate from this acknowledgement.
                Json(RemoveAttachmentResponse { removed: true }).into_response()
            }
            Operation::Read { id, thumbnail } => {
                let record = self.attachment_record(&scope, chat.as_uuid(), id)?;
                let bytes = files::read(self.files.as_ref(), &self.root, &record, thumbnail)?;
                if thumbnail {
                    crate::ai::bytes_response("image/png", None, bytes)
                } else {
                    let disposition = format!(
                        "attachment; filename*=UTF-8''{}",
                        crate::ai::percent_encode_name(&record.attachment.file_name)
                    );
                    crate::ai::bytes_response(
                        &record.attachment.media_type,
                        Some(&disposition),
                        bytes,
                    )
                }
            }
            Operation::Restage { id, new_id } => {
                let mut record = self.attachment_record(&scope, chat.as_uuid(), id)?;
                if let Some(origin) = &record.attachment.origin {
                    check_origin(origin)?;
                }
                if record.attachment.message_id.is_none() || id == new_id {
                    return Err(Error::InvalidRequest);
                }
                if record.attachment.source == AttachmentSource::Path {
                    record.attachment.id = new_id;
                    record.attachment.message_id = None;
                    record.attachment.ordinal = None;
                    record.attachment.created_at = Timestamp::now();
                    self.stage_record(&scope, record)?
                } else {
                    let bytes = files::read(self.files.as_ref(), &self.root, &record, false)?;
                    let mut checked = koru_ai_server::check_attachment(
                        chat,
                        new_id,
                        &record.attachment.file_name,
                        record.attachment.source,
                        &bytes,
                    )
                    .map_err(|_| Error::InvalidRequest)?;
                    checked.record.attachment.origin = record.attachment.origin;
                    let record = files::publish(self.files.as_ref(), &self.root, &checked, &bytes)?;
                    self.stage_record(&scope, record)?
                }
            }
        };
        Ok(Some(response))
    }
}

fn reference(chat: ChatId, request: ReferenceAttachmentRequest) -> Result<Record, Error> {
    let ReferenceAttachmentRequest { id, kind, path } = request;
    if !matches!(kind, AttachmentKind::File | AttachmentKind::Folder)
        || !koru_bots_engine::workspace::input::valid_path(&path)
        || path.len() > 4096
        || path.chars().any(|c| matches!(c, '\0' | '\r' | '\n'))
        || !std::path::Path::new(&path).is_absolute()
    {
        return Err(Error::InvalidRequest);
    }
    let name = std::path::Path::new(&path)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or(Error::InvalidRequest)?;
    let file_name = koru_ai_core::attachments::sanitize_file_name(name);
    let media_type = if kind == AttachmentKind::Folder {
        "inode/directory"
    } else {
        koru_ai_core::attachments::allowed_type(name)
            .map_or("application/octet-stream", |t| t.media_type)
    }
    .to_owned();
    Ok(Record {
        thread: chat.as_uuid(),
        content_blake3: blake3::hash(path.as_bytes()).to_hex().to_string(),
        has_sent_copy: false,
        attachment: ChatAttachment {
            id,
            message_id: None,
            ordinal: None,
            kind,
            source: AttachmentSource::Path,
            file_name,
            media_type,
            byte_count: 0,
            dimensions: None,
            origin: None,
            reference_path: Some(path),
            created_at: Timestamp::now(),
        },
    })
}

pub(crate) async fn route(
    state: &Arc<GatewayState>,
    principal: &GatewayPrincipal,
    chat: ChatId,
    operation: Operation,
) -> Option<Response> {
    if !koru_bots_engine::ENABLED {
        return None;
    }
    let Some(gateway) = state.bots_engine.get().cloned() else {
        return Some(failed(Error::EngineUnavailable));
    };
    let Ok(permit) = gateway.permits.clone().try_acquire_owned() else {
        return Some(failed(Error::EngineUnavailable));
    };
    let principal = principal.clone();
    let context = koru_ai_server::StoreContextAdapter::new(state.store.client());
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        gateway.attachment_operation(&principal, chat, operation, &context)
    })
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => Some(failed(error)),
        Err(_) => Some(failed(Error::EngineUnavailable)),
    }
}
