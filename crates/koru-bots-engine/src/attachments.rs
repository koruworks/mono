// SPDX-License-Identifier: MPL-2.0
//! Durable attachment metadata. The gateway validates and publishes bytes;
//! the engine owns the staged identity and binds it atomically to a message.
//! No file access or provider-session cleanup belongs in this module.
use crate::{
    Database, Error,
    messages::{self, Person},
};
use koru_ai_core::attachments::{
    MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS_PER_MESSAGE, MAX_MESSAGE_ATTACHMENT_BYTES,
    sanitize_file_name,
};
use koru_ai_core::{AttachmentKind, AttachmentSource, ChatAttachment, MessageId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use uuid::Uuid;
mod cleanup;
pub use cleanup::Sweep;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(rename = "BotEngineAttachmentRecord"))]
pub struct Record {
    pub thread: Uuid,
    pub attachment: ChatAttachment,
    pub content_blake3: String,
    pub has_sent_copy: bool,
}
impl std::fmt::Debug for Record {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AttachmentRecord(<redacted>)")
    }
}
impl Record {
    fn validate(&self) -> Result<(), Error> {
        let a = &self.attachment;
        if self.thread.is_nil()
            || encode(self)?.len() > 16 * 1024
            || a.id.as_uuid().is_nil()
            || a.message_id.is_some()
            || a.ordinal.is_some()
            || a.file_name != sanitize_file_name(&a.file_name)
            || a.file_name.is_empty()
            || a.byte_count > MAX_ATTACHMENT_BYTES
            || self.content_blake3.len() != 64
            || !self
                .content_blake3
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || (a.source == AttachmentSource::Path) != a.reference_path.is_some()
            || a.reference_path.as_ref().is_some_and(|p| {
                p.is_empty() || p.len() > 4096 || p.chars().any(|c| matches!(c, '\0' | '\r' | '\n'))
            })
            || (self.has_sent_copy
                && (a.kind != AttachmentKind::Image || a.source == AttachmentSource::Path))
            || (a.kind == AttachmentKind::Folder && a.source != AttachmentSource::Path)
            || (a.source != AttachmentSource::Path && a.byte_count == 0)
        {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(value).map_err(|_| Error::InvalidRequest)
}

/// Gateway-only control, carried under the private worker authority. The
/// public person-command endpoint must never accept caller-written metadata.
#[derive(Clone, Serialize, Deserialize)]
pub struct Request {
    pub owner: String,
    pub sender: String,
    pub command: Command,
}
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AttachmentRequest(<redacted>)")
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub enum Command {
    Expire {
        after: Option<Uuid>,
        cutoff_ms: i64,
    },
    Collect {
        thread: Uuid,
        id: Uuid,
        cutoff_ms: i64,
    },
    SendReceipt {
        envelope: crate::messages::Envelope,
    },
    /// Routing distinguishes an absent ordinary chat from an engine thread.
    /// An existing thread with another owner is refused, never a fallback.
    Probe {
        thread: Uuid,
    },
    Stage(Record),
    Read {
        thread: Uuid,
        id: Uuid,
    },
    Remove {
        thread: Uuid,
        id: Uuid,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Reply {
    Sweep(Sweep),
    Collect(bool),
    Receipt(Option<crate::messages::Receipt<crate::messages::Accepted>>),
    Thread(bool),
    Record(Option<Record>),
    Removed,
}
pub fn handle(db: &mut Database, request: Request) -> Result<Reply, Error> {
    if request.owner.is_empty()
        || request.owner.len() > 256
        || request.sender.is_empty()
        || request.sender.len() > 256
        || encode(&request)?.len() > crate::person::MAX_COMMAND_BYTES
    {
        return Err(Error::InvalidRequest);
    }
    let who = Person {
        owner: request.owner,
        sender: request.sender,
    };
    match request.command {
        Command::Expire { after, cutoff_ms } => db
            .expire_attachments(&who, after, cutoff_ms)
            .map(Reply::Sweep),
        Command::Collect {
            thread,
            id,
            cutoff_ms,
        } => db
            .collect_attachment(&who, thread, id, cutoff_ms)
            .map(Reply::Collect),
        Command::SendReceipt { envelope } => {
            db.message_receipt(&who, &envelope).map(Reply::Receipt)
        }
        Command::Probe { thread } => {
            let exists: bool = db.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1)",
                [thread.to_string()],
                |r| r.get(0),
            )?;
            if exists {
                messages::authorize_thread(&db.connection, &who, thread)?;
            }
            Ok(Reply::Thread(exists))
        }
        Command::Stage(record) => db
            .stage_attachment(&who, &record)
            .map(|r| Reply::Record(Some(r))),
        Command::Read { thread, id } => db.read_attachment(&who, thread, id).map(Reply::Record),
        Command::Remove { thread, id } => {
            db.remove_attachment(&who, thread, id)?;
            Ok(Reply::Removed)
        }
    }
}
pub(crate) fn load(c: &Connection, thread: Uuid, id: Uuid) -> Result<Option<Record>, Error> {
    let row: Option<(Vec<u8>,Option<String>,Option<u32>)> = c.query_row(
        "SELECT record,message,ordinal FROM attachments WHERE id=?1 AND thread=?2 AND removed=0",
        params![id.to_string(),thread.to_string()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).optional()?;
    row.map(|(bytes, message, ordinal)| {
        let mut record: Record =
            serde_json::from_slice(&bytes).map_err(|_| Error::StorageUnavailable)?;
        record.attachment.message_id = message
            .map(|s| {
                Uuid::parse_str(&s)
                    .map(MessageId::from_uuid)
                    .map_err(|_| Error::StorageUnavailable)
            })
            .transpose()?;
        record.attachment.ordinal = ordinal;
        Ok(record)
    })
    .transpose()
}

impl Database {
    /// Called only after the gateway has durably published and checked bytes.
    /// Repeating an upload ignores only its newly sampled creation timestamp.
    pub fn stage_attachment(&mut self, who: &Person, record: &Record) -> Result<Record, Error> {
        record.validate()?;
        let tx = self.connection.transaction()?;
        messages::authorize_thread(&tx, who, record.thread)?;
        let active: bool = tx.query_row("SELECT t.lifecycle='active' AND b.lifecycle='active' FROM threads t JOIN bots b ON b.id=t.bot WHERE t.id=?1",
            [record.thread.to_string()], |r| r.get(0))?;
        if !active {
            return Err(Error::NotAllowed);
        }
        let id = record.attachment.id.as_uuid();
        if let Some(existing) = load(&tx, record.thread, id)? {
            let mut comparable = existing.clone();
            comparable.attachment.message_id = None;
            comparable.attachment.ordinal = None;
            comparable.attachment.created_at = record.attachment.created_at;
            return if comparable == *record {
                Ok(existing)
            } else {
                Err(Error::Conflict)
            };
        }
        let used: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM attachments WHERE id=?1)",
            [id.to_string()],
            |r| r.get(0),
        )?;
        if used {
            return Err(Error::NotAllowed);
        }
        tx.execute(
            "INSERT INTO attachments(id,thread,record) VALUES (?1,?2,?3)",
            params![id.to_string(), record.thread.to_string(), encode(record)?],
        )?;
        tx.commit()?;
        Ok(record.clone())
    }

    pub fn read_attachment(
        &self,
        who: &Person,
        thread: Uuid,
        id: Uuid,
    ) -> Result<Option<Record>, Error> {
        messages::authorize_thread(&self.connection, who, thread)?;
        load(&self.connection, thread, id)
    }

    /// A tombstone wins a race against a late upload. Sent content is immutable.
    pub fn remove_attachment(&mut self, who: &Person, thread: Uuid, id: Uuid) -> Result<(), Error> {
        let tx = self.connection.transaction()?;
        messages::authorize_thread(&tx, who, thread)?;
        let row: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT thread,message FROM attachments WHERE id=?1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((owner, message)) = row {
            if owner != thread.to_string() || message.is_some() {
                return Err(Error::NotAllowed);
            }
            tx.execute(
                "UPDATE attachments SET removed=1 WHERE id=?1",
                [id.to_string()],
            )?;
        } else {
            if id.is_nil() {
                return Err(Error::InvalidRequest);
            }
            tx.execute(
                "INSERT INTO attachments(id,thread,record,removed) VALUES (?1,?2,x'7b7d',1)",
                params![id.to_string(), thread.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

/// Resolve every id before inserting the message; the caller commits these
/// values and `bind` in the same Accept transaction. Unknown ids fail closed.
pub(crate) fn prepare(
    c: &Connection,
    thread: Uuid,
    message: Uuid,
    ids: &[Value],
) -> Result<Vec<Value>, Error> {
    if ids.len() > MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(Error::InvalidRequest);
    }
    let mut seen = HashSet::new();
    let mut total = 0u64;
    ids.iter()
        .enumerate()
        .map(|(index, value)| {
            let id: Uuid =
                serde_json::from_value(value.clone()).map_err(|_| Error::InvalidRequest)?;
            if !seen.insert(id) {
                return Err(Error::InvalidRequest);
            }
            let mut record = load(c, thread, id)?.ok_or(Error::NotAllowed)?;
            if record.attachment.message_id.is_some() {
                return Err(Error::NotAllowed);
            }
            total = total
                .checked_add(record.attachment.byte_count)
                .ok_or(Error::InvalidRequest)?;
            if total > MAX_MESSAGE_ATTACHMENT_BYTES {
                return Err(Error::InvalidRequest);
            }
            record.attachment.message_id = Some(MessageId::from_uuid(message));
            record.attachment.ordinal = Some(index as u32);
            serde_json::to_value(record.attachment).map_err(|_| Error::InvalidRequest)
        })
        .collect()
}
pub(crate) fn bind(
    c: &Connection,
    thread: Uuid,
    message: Uuid,
    ids: &[Value],
) -> Result<(), Error> {
    for (ordinal, value) in ids.iter().enumerate() {
        let id: Uuid = serde_json::from_value(value.clone()).map_err(|_| Error::InvalidRequest)?;
        if c.execute("UPDATE attachments SET message=?3,ordinal=?4 WHERE id=?1 AND thread=?2 AND message IS NULL AND removed=0",
            params![id.to_string(),thread.to_string(),message.to_string(),ordinal as u32])? != 1 {
            return Err(Error::NotAllowed);
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
