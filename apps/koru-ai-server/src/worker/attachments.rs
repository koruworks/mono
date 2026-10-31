// SPDX-License-Identifier: MPL-2.0

//! The worker's side of chat attachments (chat-attachments; ADR-479).
//!
//! The bytes are handled on the caller's thread, never here: an upload is
//! checked, prepared and published by [`AttachmentStore`] on the gateway's
//! blocking thread, and only then does the dispatcher record its row. The
//! dispatcher reads and writes rows and checks a send's attachments; folder
//! removals and a branch's links go to the cleanup thread.

use koru_ai_core::attachments::{MAX_ATTACHMENTS_PER_MESSAGE, MAX_MESSAGE_ATTACHMENT_BYTES};
use koru_ai_core::{AttachmentInput, AttachmentRecord, ChatAttachment, ChatAttachmentId};
use koru_ai_sqlite::RemovedAttachment;
use koru_ai_wire::attachment_codes;

use crate::attachments::{BranchLink, CleanupJob};

use super::*;

/// What recording a staged attachment found.
pub(super) enum StageAnswer {
    Stored(ChatAttachment),
    /// A row with this id and the same content was already there.
    Existing(ChatAttachment),
}

/// A branch is either done (an idempotent replay, or one with nothing to
/// link) or a copy whose attachment folders must be linked first.
pub(super) enum BranchPlan {
    Done(BranchChatResponse),
    Link(Box<BranchCopy>, Vec<BranchLink>),
}

impl WorkerState {
    // ----- rows, for the caller's thread --------------------------------

    /// Before an upload is prepared: the chat must exist and take
    /// attachments, and an earlier row under the same id is returned so the
    /// caller can tell a repeat from a conflict without storing anything.
    pub(super) fn attachment_admission(
        &self,
        context: RequestContext,
        chat: ChatId,
        id: ChatAttachmentId,
    ) -> ApiResult<Option<AttachmentRecord>> {
        authorize(&context, AiScope::Chat)?;
        self.admit_chat_work(Some(chat))?;
        self.require_attachment_chat(chat)?;
        match self.repository.attachment(id).map_err(ai_failure)? {
            Some(record) if record.chat_id != chat => Err(attachment_failure(
                attachment_codes::CONFLICT,
                "This attachment identity belongs to another chat.",
                false,
                id,
            )),
            existing => Ok(existing),
        }
    }

    /// Record a staged attachment whose folder is already published.
    pub(super) fn stage_attachment(
        &mut self,
        context: RequestContext,
        record: AttachmentRecord,
    ) -> ApiResult<StageAnswer> {
        authorize(&context, AiScope::Chat)?;
        self.admit_chat_work(Some(record.chat_id))?;
        self.require_attachment_chat(record.chat_id)?;
        let id = record.attachment.id;
        if let Some(existing) = self.repository.attachment(id).map_err(ai_failure)? {
            return if existing.chat_id == record.chat_id
                && existing.content_blake3 == record.content_blake3
            {
                Ok(StageAnswer::Existing(existing.attachment))
            } else {
                Err(conflict(id))
            };
        }
        self.repository
            .stage_attachment(&record)
            .map_err(ai_failure)?;
        Ok(StageAnswer::Stored(record.attachment))
    }

    pub(super) fn remove_staged_attachment(
        &mut self,
        context: RequestContext,
        chat: ChatId,
        id: ChatAttachmentId,
    ) -> ApiResult<RemovedAttachment> {
        authorize(&context, AiScope::Chat)?;
        self.admit_chat_work(Some(chat))?;
        self.repository
            .remove_staged_attachment(chat, id)
            .map_err(ai_failure)
    }

    /// The rows of these attachments of this chat, in the order asked. A
    /// chat that takes no attachments says so, rather than "not found".
    pub(super) fn resolve_attachments(
        &self,
        context: RequestContext,
        chat: ChatId,
        ids: &[ChatAttachmentId],
    ) -> ApiResult<Vec<AttachmentRecord>> {
        authorize(&context, AiScope::Chat)?;
        self.require_attachment_chat(chat)?;
        ids.iter()
            .map(|id| {
                self.repository
                    .attachment(*id)
                    .map_err(ai_failure)?
                    .filter(|record| record.chat_id == chat)
                    .ok_or_else(|| attachment_not_found(*id))
            })
            .collect()
    }

    fn require_attachment_chat(&self, chat: ChatId) -> ApiResult<Chat> {
        let chat = self
            .repository
            .chat(chat)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        if !is_agent_provider(&chat.current_model.provider) {
            return Err(attachments_unsupported());
        }
        Ok(chat)
    }

    // ----- a send -------------------------------------------------------

    /// The send's attachments, checked before any row is written: an agent
    /// chat, no repeat, within the count and byte limits, every one this
    /// chat's and still staged, and an `@` copy only from a workspace the
    /// chat may still use (D17). Binding them is the turn transaction's.
    pub(super) fn attachments_for_send(
        &self,
        chat: &Chat,
        ids: &[ChatAttachmentId],
    ) -> ApiResult<Vec<AttachmentRecord>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        if !is_agent_provider(&chat.current_model.provider) {
            return Err(attachments_unsupported());
        }
        let mut seen = HashSet::new();
        if ids.iter().any(|id| !seen.insert(*id)) {
            return Err(failure(
                "invalid_request",
                "An attachment was named twice.",
                false,
            ));
        }
        if ids.len() > MAX_ATTACHMENTS_PER_MESSAGE {
            return Err(over_message_limit());
        }
        let mut records = Vec::with_capacity(ids.len());
        let mut total = 0u64;
        for id in ids {
            let record = self
                .repository
                .attachment(*id)
                .map_err(ai_failure)?
                .filter(|record| record.chat_id == chat.id)
                .ok_or_else(|| attachment_not_found(*id))?;
            if record.attachment.message_id.is_some() {
                return Err(attachment_failure(
                    attachment_codes::SENT,
                    "This attachment was already sent.",
                    false,
                    *id,
                ));
            }
            total = total.saturating_add(record.attachment.byte_count);
            records.push(record);
        }
        if total > MAX_MESSAGE_ATTACHMENT_BYTES {
            return Err(over_message_limit());
        }
        if records
            .iter()
            .any(|record| record.attachment.origin.is_some())
        {
            let granted = self
                .repository
                .chat_tool_policy(chat.id)
                .map_err(ai_failure)?
                .map(|policy| policy.workspace_ids)
                .unwrap_or_default();
            if let Some(revoked) = records.iter().find(|record| {
                record
                    .attachment
                    .origin
                    .as_ref()
                    .is_some_and(|origin| !granted.contains(&origin.workspace_id))
            }) {
                return Err(attachment_failure(
                    attachment_codes::WORKSPACE_NOT_GRANTED,
                    "This space is no longer shared with the chat.",
                    false,
                    revoked.attachment.id,
                ));
            }
        }
        Ok(records)
    }

    /// What one provider request carries for these rows. The paths are on
    /// this computer; which copy goes natively is settled on the provider
    /// thread, where a file may be looked at.
    pub(super) fn attachment_inputs(&self, records: &[AttachmentRecord]) -> Vec<AttachmentInput> {
        records
            .iter()
            .map(|record| AttachmentInput {
                id: record.attachment.id,
                kind: record.attachment.kind,
                file_name: record.attachment.file_name.clone(),
                media_type: record.attachment.media_type.clone(),
                byte_count: record.attachment.byte_count,
                path: self.attachments.original(record),
                native: None,
            })
            .collect()
    }

    /// The sent attachments of a chat, per user message, for its history.
    pub(super) fn attachment_inputs_by_message(
        &self,
        chat: ChatId,
    ) -> ApiResult<HashMap<MessageId, Vec<AttachmentInput>>> {
        let mut by_message: HashMap<MessageId, Vec<AttachmentRecord>> = HashMap::new();
        for record in self
            .repository
            .attachment_records(chat)
            .map_err(ai_failure)?
        {
            if let Some(message) = record.attachment.message_id {
                by_message.entry(message).or_default().push(record);
            }
        }
        Ok(by_message
            .into_iter()
            .map(|(message, records)| (message, self.attachment_inputs(&records)))
            .collect())
    }

    /// After a chat's rows are gone: its folder goes on the cleanup thread.
    pub(super) fn forget_chat_folders(&self, chats: &[ChatId]) {
        for chat in chats {
            self.cleanup.submit(CleanupJob::RemoveChat(*chat));
        }
    }

    // ----- a branch (D14) -----------------------------------------------

    /// Branch a chat. With attachments to carry, their folders are linked on
    /// the cleanup thread first and the reply is held until that reports.
    pub(super) fn begin_branch_chat(
        &mut self,
        context: RequestContext,
        source: ChatId,
        request: BranchChatRequest,
        reply: SyncSender<ApiResult<BranchChatResponse>>,
    ) {
        if self.branching.contains(&request.branch_chat_id) {
            let _ = reply.send(Err(failure(
                "chat_busy",
                "This branch is still being made.",
                true,
            )));
            return;
        }
        match self.branch_chat(context.clone(), source, request) {
            Err(error) => {
                let _ = reply.send(Err(error));
            }
            Ok(BranchPlan::Done(response)) => {
                let _ = reply.send(Ok(response));
            }
            Ok(BranchPlan::Link(copy, links)) => {
                let branch = copy.chat.id;
                self.branching.insert(branch);
                let output = self.output.clone();
                self.cleanup.submit(CleanupJob::LinkBranch {
                    links,
                    done: Box::new(move |linked| {
                        let _ = output.send(Envelope::BranchLinked {
                            context,
                            copy,
                            linked: linked.is_ok(),
                            reply,
                        });
                    }),
                });
            }
        }
    }

    /// The branch's folders are linked (or could not be): commit it, or
    /// give up and leave nothing behind.
    pub(super) fn on_branch_linked(
        &mut self,
        context: RequestContext,
        copy: Box<BranchCopy>,
        linked: bool,
        reply: SyncSender<ApiResult<BranchChatResponse>>,
    ) {
        let branch = copy.chat.id;
        self.branching.remove(&branch);
        if !linked {
            let _ = reply.send(Err(failure(
                attachment_codes::STORAGE,
                "The branch's attachments could not be copied.",
                true,
            )));
            return;
        }
        let answer = match self.repository.create_branch(&copy) {
            Ok(()) => self
                .read_chat(context, branch)
                .map(|transcript| BranchChatResponse { transcript }),
            Err(error) => {
                self.cleanup.submit(CleanupJob::RemoveChat(branch));
                Err(ai_failure(error))
            }
        };
        let _ = reply.send(answer);
    }

    /// The copied user messages' attachments, under new ids in the branch.
    pub(super) fn branch_attachments(
        &self,
        source: ChatId,
        branch: ChatId,
        message_ids: &HashMap<MessageId, MessageId>,
    ) -> ApiResult<(Vec<AttachmentRecord>, Vec<BranchLink>)> {
        let mut records = Vec::new();
        let mut links = Vec::new();
        for mut record in self
            .repository
            .attachment_records(source)
            .map_err(ai_failure)?
        {
            let Some(message) = record
                .attachment
                .message_id
                .and_then(|message| message_ids.get(&message).copied())
            else {
                continue;
            };
            let from = (record.chat_id, record.attachment.id);
            record.chat_id = branch;
            record.attachment.id = ChatAttachmentId::new();
            record.attachment.message_id = Some(message);
            links.push(BranchLink {
                from,
                to: (branch, record.attachment.id),
            });
            records.push(record);
        }
        Ok((records, links))
    }
}

// ----- the caller's half: on the gateway's blocking thread ---------------

/// Store one upload: check the chat, tell a repeat from a conflict, prepare
/// and publish the folder, then record the row.
pub(super) fn put_attachment(
    client: &AiWorkerClient,
    context: RequestContext,
    chat: ChatId,
    id: ChatAttachmentId,
    upload: koru_ai_api::AttachmentUpload,
) -> ApiResult<koru_ai_wire::ChatAttachmentResponse> {
    if upload.source == koru_ai_core::AttachmentSource::Workspace {
        return Err(failure(
            "invalid_request",
            "A workspace file is linked, not uploaded.",
            false,
        ));
    }
    let (reply, response) = sync_channel(1);
    client.send(WorkerCommand::AttachmentAdmission {
        context: context.clone(),
        chat,
        attachment: id,
        reply,
    })?;
    if let Some(existing) = receive_api(response)? {
        return if existing.content_blake3 == blake3::hash(&upload.bytes).to_hex().as_str() {
            Ok(koru_ai_wire::ChatAttachmentResponse {
                attachment: existing.attachment,
            })
        } else {
            Err(conflict(id))
        };
    }
    let store = &client.attachments;
    let prepared = store.prepare(chat, id, &upload.file_name, upload.source, &upload.bytes)?;
    drop(upload);
    store.publish(&prepared)?;
    let record = prepared.record.clone();
    let (reply, response) = sync_channel(1);
    let staged = client
        .send(WorkerCommand::StageAttachment {
            context,
            record: Box::new(record),
            reply,
        })
        .and_then(|()| receive_api(response));
    match staged {
        Ok(StageAnswer::Stored(attachment) | StageAnswer::Existing(attachment)) => {
            Ok(koru_ai_wire::ChatAttachmentResponse { attachment })
        }
        Err(error) => {
            // No row names the folder: it goes now rather than at the sweep.
            store.remove_attachment(chat, id);
            Err(error)
        }
    }
}

pub(super) fn remove_attachment(
    client: &AiWorkerClient,
    context: RequestContext,
    chat: ChatId,
    id: ChatAttachmentId,
) -> ApiResult<koru_ai_wire::RemoveAttachmentResponse> {
    let (reply, response) = sync_channel(1);
    client.send(WorkerCommand::RemoveStagedAttachment {
        context,
        chat,
        attachment: id,
        reply,
    })?;
    match receive_api(response)? {
        RemovedAttachment::Removed => {
            client.attachments.remove_attachment(chat, id);
            Ok(koru_ai_wire::RemoveAttachmentResponse { removed: true })
        }
        RemovedAttachment::NotFound => {
            Ok(koru_ai_wire::RemoveAttachmentResponse { removed: false })
        }
        RemovedAttachment::Sent => Err(attachment_failure(
            attachment_codes::SENT,
            "A sent attachment stays with its message.",
            false,
            id,
        )),
    }
}

/// The rows of these attachments, asked of the dispatcher.
pub(super) fn resolve(
    client: &AiWorkerClient,
    context: RequestContext,
    chat: ChatId,
    ids: Vec<ChatAttachmentId>,
) -> ApiResult<Vec<AttachmentRecord>> {
    let (reply, response) = sync_channel(1);
    client.send(WorkerCommand::ResolveAttachments {
        context,
        chat,
        attachments: ids,
        reply,
    })?;
    receive_api(response)
}

pub(super) fn read_attachment(
    client: &AiWorkerClient,
    context: RequestContext,
    chat: ChatId,
    id: ChatAttachmentId,
) -> ApiResult<koru_ai_api::AttachmentContent> {
    let record = resolve(client, context, chat, vec![id])?
        .pop()
        .ok_or_else(|| attachment_not_found(id))?;
    let bytes =
        std::fs::read(client.attachments.original(&record)).map_err(|_| attachment_missing(id))?;
    Ok(koru_ai_api::AttachmentContent {
        file_name: record.attachment.file_name,
        media_type: record.attachment.media_type,
        bytes,
    })
}

pub(super) fn read_thumbnail(
    client: &AiWorkerClient,
    context: RequestContext,
    chat: ChatId,
    id: ChatAttachmentId,
) -> ApiResult<Vec<u8>> {
    let record = resolve(client, context, chat, vec![id])?
        .pop()
        .ok_or_else(|| attachment_not_found(id))?;
    if record.attachment.kind != koru_ai_core::AttachmentKind::Image {
        return Err(attachment_failure(
            attachment_codes::NO_PREVIEW,
            "This attachment has no preview.",
            false,
            id,
        ));
    }
    std::fs::read(client.attachments.thumbnail(chat, id)).map_err(|_| attachment_missing(id))
}

/// Before a send names attachments: each stored file must still be on this
/// computer. Looked at here, on the caller's thread, so the dispatcher
/// stays with rows; the dispatcher checks the rows themselves.
pub(super) fn check_files_for_send(
    client: &AiWorkerClient,
    context: RequestContext,
    request: &SendTurnRequest,
) -> ApiResult<()> {
    if request.attachment_ids.is_empty() {
        return Ok(());
    }
    for record in resolve(
        client,
        context,
        request.chat_id,
        request.attachment_ids.clone(),
    )? {
        if !client.attachments.original(&record).is_file() {
            tracing::warn!(
                target: "koru_ai::attachments",
                attachment_id = %record.attachment.id,
                "a stored attachment is missing at send"
            );
            return Err(attachment_missing(record.attachment.id));
        }
    }
    Ok(())
}

// ----- failures -----------------------------------------------------------

fn attachment_failure(code: &str, message: &str, retryable: bool, id: ChatAttachmentId) -> Failure {
    let mut failure = failure(code, message, retryable);
    failure
        .details
        .insert("attachment_id".into(), id.to_storage().into());
    failure
}

fn attachment_not_found(id: ChatAttachmentId) -> Failure {
    attachment_failure(
        attachment_codes::NOT_FOUND,
        "This attachment is not stored with the chat.",
        false,
        id,
    )
}

fn attachment_missing(id: ChatAttachmentId) -> Failure {
    attachment_failure(
        attachment_codes::MISSING,
        "This file is no longer on this computer.",
        false,
        id,
    )
}

fn conflict(id: ChatAttachmentId) -> Failure {
    attachment_failure(
        attachment_codes::CONFLICT,
        "This attachment identity was already used for other content.",
        false,
        id,
    )
}

fn over_message_limit() -> Failure {
    failure(
        attachment_codes::LIMIT,
        "A message can carry up to 10 files, 50 MB in all.",
        false,
    )
}

fn attachments_unsupported() -> Failure {
    failure(
        attachment_codes::UNSUPPORTED,
        "Pictures and files can be sent to local agents only.",
        false,
    )
}
