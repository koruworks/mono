// SPDX-License-Identifier: MPL-2.0

//! `message_attachments`: the rows of pictures and files stored with a chat
//! (chat-attachments, schema 9). The bytes are the AI service's; these rows
//! are what the chat knows about them.

use std::collections::HashSet;

use koru_ai_core::{
    AiError, AttachmentKind, AttachmentRecord, AttachmentSource, ChatAttachment, ChatAttachmentId,
    ChatId, DocumentId, MessageId, Timestamp, WorkspaceFileOrigin, WorkspaceId,
};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{
    Repository, collect_rows, decode_failed, invariant_failed, nonnegative_u32, nonnegative_u64,
    optional_id, required_id, required_u64, storage_failed,
};

/// What removing a staged attachment found.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RemovedAttachment {
    Removed {
        had_files: bool,
    },
    /// No such attachment in this chat.
    NotFound,
    /// It was sent with a message, which keeps it.
    Sent,
}

/// Every attachment folder the rows account for, for the start-up sweep:
/// the chats that exist and the attachments that have a row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KnownAttachmentDirs {
    pub chats: HashSet<ChatId>,
    pub attachments: HashSet<ChatAttachmentId>,
}

/// Read through the alias `attachment`, so a join with `messages` is not
/// ambiguous.
const COLUMNS: &str = "attachment.id, attachment.chat_id, attachment.message_id,
    attachment.ordinal, attachment.kind, attachment.source, attachment.file_name,
    attachment.media_type, attachment.byte_count, attachment.width, attachment.height,
    attachment.origin_workspace_id, attachment.origin_document_id, attachment.created_at_ms,
    attachment.has_sent_copy, attachment.content_blake3, attachment.reference_path";

impl Repository {
    /// One attachment by id, in whichever chat it belongs to.
    pub fn attachment(&self, id: ChatAttachmentId) -> Result<Option<AttachmentRecord>, AiError> {
        self.connection
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM message_attachments AS attachment
                      WHERE attachment.id = ?1"
                ),
                [id.to_storage()],
                record_from_row,
            )
            .optional()
            .map_err(storage_failed)
    }

    /// Record a staged attachment whose bytes are already published.
    pub fn stage_attachment(&mut self, record: &AttachmentRecord) -> Result<(), AiError> {
        if record.attachment.message_id.is_some() || record.attachment.ordinal.is_some() {
            return Err(invariant_failed());
        }
        insert_attachment(&self.connection, record, None)
    }

    /// Remove a staged attachment of this chat. A sent one is kept.
    pub fn remove_staged_attachment(
        &mut self,
        chat: ChatId,
        id: ChatAttachmentId,
    ) -> Result<RemovedAttachment, AiError> {
        let state: Option<(bool, bool)> = self
            .connection
            .query_row(
                "SELECT message_id IS NOT NULL, source != 'path' FROM message_attachments
                  WHERE id = ?1 AND chat_id = ?2",
                [id.to_storage(), chat.to_storage()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(storage_failed)?;
        match state {
            None => Ok(RemovedAttachment::NotFound),
            Some((true, _)) => Ok(RemovedAttachment::Sent),
            Some((false, had_files)) => {
                self.connection
                    .execute(
                        "DELETE FROM message_attachments
                          WHERE id = ?1 AND chat_id = ?2 AND message_id IS NULL",
                        [id.to_storage(), chat.to_storage()],
                    )
                    .map_err(storage_failed)?;
                Ok(RemovedAttachment::Removed { had_files })
            }
        }
    }

    /// Every attachment of a chat: the sent ones in message and ordinal
    /// order, then the staged ones in the order they were added.
    pub fn attachments_for_chat(&self, chat: ChatId) -> Result<Vec<ChatAttachment>, AiError> {
        Ok(self
            .attachment_records(chat)?
            .into_iter()
            .map(|record| record.attachment)
            .collect())
    }

    /// The same rows as [`Self::attachments_for_chat`], with what only the
    /// service reads.
    pub fn attachment_records(&self, chat: ChatId) -> Result<Vec<AttachmentRecord>, AiError> {
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT {COLUMNS} FROM message_attachments AS attachment
                  LEFT JOIN messages AS message ON message.id = attachment.message_id
                  WHERE attachment.chat_id = ?1
                  ORDER BY attachment.message_id IS NULL, message.ordinal,
                           attachment.ordinal, attachment.created_at_ms, attachment.rowid"
            ))
            .map_err(storage_failed)?;
        let rows = statement
            .query_map([chat.to_storage()], record_from_row)
            .map_err(storage_failed)?;
        collect_rows(rows)
    }

    /// Remove every staged attachment added before `cutoff`, and say how
    /// many went. Their folders become orphans the sweep removes.
    pub fn remove_staged_attachments_before(&mut self, cutoff: Timestamp) -> Result<u64, AiError> {
        self.connection
            .execute(
                "DELETE FROM message_attachments
                  WHERE message_id IS NULL AND created_at_ms < ?1",
                [cutoff.millis()],
            )
            .map(|count| u64::try_from(count).unwrap_or(u64::MAX))
            .map_err(storage_failed)
    }

    /// The chats and attachments a folder under `attachments/` may belong to.
    pub fn attachment_dirs_known(&self) -> Result<KnownAttachmentDirs, AiError> {
        let mut known = KnownAttachmentDirs::default();
        let mut chats = self
            .connection
            .prepare("SELECT id FROM chats")
            .map_err(storage_failed)?;
        for chat in collect_rows(
            chats
                .query_map([], |row| required_id(row, 0, ChatId::parse, "ChatId"))
                .map_err(storage_failed)?,
        )? {
            known.chats.insert(chat);
        }
        let mut attachments = self
            .connection
            .prepare("SELECT id FROM message_attachments")
            .map_err(storage_failed)?;
        for attachment in collect_rows(
            attachments
                .query_map([], |row| {
                    required_id(row, 0, ChatAttachmentId::parse, "ChatAttachmentId")
                })
                .map_err(storage_failed)?,
        )? {
            known.attachments.insert(attachment);
        }
        Ok(known)
    }
}

/// Bind staged attachments to the user message, in order, inside the turn's
/// transaction. Each must be this chat's and still staged; anything else
/// aborts the transaction (the dispatcher checked them a moment before, so
/// this is the invariant, not the user-facing refusal).
pub(super) fn bind_attachments(
    connection: &Connection,
    chat: ChatId,
    message: MessageId,
    ids: &[ChatAttachmentId],
    at: Timestamp,
) -> Result<(), AiError> {
    for (ordinal, id) in ids.iter().enumerate() {
        let bound = connection
            .execute(
                "UPDATE message_attachments
                    SET message_id = ?1, ordinal = ?2, bound_at_ms = ?3
                  WHERE id = ?4 AND chat_id = ?5 AND message_id IS NULL",
                params![
                    message.to_storage(),
                    i64::try_from(ordinal).map_err(|_| invariant_failed())?,
                    at.millis(),
                    id.to_storage(),
                    chat.to_storage()
                ],
            )
            .map_err(storage_failed)?;
        if bound != 1 {
            return Err(invariant_failed());
        }
    }
    Ok(())
}

/// Insert one row; `bound_at` is set for a row that is already sent (a
/// branch's copy).
pub(super) fn insert_attachment(
    connection: &Connection,
    record: &AttachmentRecord,
    bound_at: Option<Timestamp>,
) -> Result<(), AiError> {
    let attachment = &record.attachment;
    if attachment.message_id.is_some() != bound_at.is_some()
        || attachment.message_id.is_some() != attachment.ordinal.is_some()
        || (attachment.source == AttachmentSource::Workspace) != attachment.origin.is_some()
        || (attachment.source == AttachmentSource::Path) != attachment.reference_path.is_some()
        || (attachment.source == AttachmentSource::Path) != (attachment.byte_count == 0)
        || (attachment.kind == AttachmentKind::Folder
            && attachment.source != AttachmentSource::Path)
        || (attachment.kind == AttachmentKind::Image && attachment.source == AttachmentSource::Path)
    {
        return Err(invariant_failed());
    }
    let (width, height) = attachment
        .dimensions
        .map_or((None, None), |(width, height)| (Some(width), Some(height)));
    connection
        .execute(
            "INSERT INTO message_attachments (
                id, chat_id, message_id, ordinal, kind, source, file_name, media_type,
                byte_count, width, height, origin_workspace_id, origin_document_id,
                created_at_ms, has_sent_copy, content_blake3, bound_at_ms,
                reference_path
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                attachment.id.to_storage(),
                record.chat_id.to_storage(),
                attachment.message_id.map(MessageId::to_storage),
                attachment.ordinal,
                attachment.kind.as_token(),
                attachment.source.as_token(),
                attachment.file_name,
                attachment.media_type,
                required_u64(attachment.byte_count)?,
                width,
                height,
                attachment
                    .origin
                    .as_ref()
                    .map(|origin| origin.workspace_id.to_storage()),
                attachment
                    .origin
                    .as_ref()
                    .map(|origin| origin.document_id.to_storage()),
                attachment.created_at.millis(),
                record.has_sent_copy,
                record.content_blake3,
                bound_at.map(Timestamp::millis),
                attachment.reference_path,
            ],
        )
        .map_err(storage_failed)?;
    Ok(())
}

fn record_from_row(row: &Row<'_>) -> rusqlite::Result<AttachmentRecord> {
    let width = row.get::<_, Option<i64>>(9)?;
    let height = row.get::<_, Option<i64>>(10)?;
    let dimensions = match (width, height) {
        (Some(width), Some(height)) => Some((
            u32::try_from(width).map_err(|_| decode_failed(9, "attachment width"))?,
            u32::try_from(height).map_err(|_| decode_failed(10, "attachment height"))?,
        )),
        _ => None,
    };
    let origin_workspace = optional_id(row, 11, WorkspaceId::parse, "WorkspaceId")?;
    let origin_document = optional_id(row, 12, DocumentId::parse, "DocumentId")?;
    Ok(AttachmentRecord {
        chat_id: required_id(row, 1, ChatId::parse, "ChatId")?,
        attachment: ChatAttachment {
            id: required_id(row, 0, ChatAttachmentId::parse, "ChatAttachmentId")?,
            message_id: optional_id(row, 2, MessageId::parse, "MessageId")?,
            ordinal: row
                .get::<_, Option<i64>>(3)?
                .map(|_| nonnegative_u32(row, 3))
                .transpose()?,
            kind: AttachmentKind::parse(&row.get::<_, String>(4)?)
                .ok_or_else(|| decode_failed(4, "attachment kind"))?,
            source: AttachmentSource::parse(&row.get::<_, String>(5)?)
                .ok_or_else(|| decode_failed(5, "attachment source"))?,
            file_name: row.get(6)?,
            media_type: row.get(7)?,
            byte_count: nonnegative_u64(row, 8)?,
            dimensions,
            origin: match (origin_workspace, origin_document) {
                (Some(workspace_id), Some(document_id)) => Some(WorkspaceFileOrigin {
                    workspace_id,
                    document_id,
                }),
                _ => None,
            },
            reference_path: row.get(16)?,
            created_at: Timestamp::from_millis(row.get(13)?),
        },
        has_sent_copy: row.get(14)?,
        content_blake3: row.get(15)?,
    })
}
