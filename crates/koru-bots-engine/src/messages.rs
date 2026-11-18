// SPDX-License-Identifier: MPL-2.0

//! Durable message life. Identity arrives from a trusted carrier, never from
//! the envelope. The engine actor serializes these transactions; no provider,
//! Store, clock, process or network is opened by this core.

mod accept;
pub(crate) mod collaboration;
mod history;
mod publish;
pub(crate) mod receipts;
mod refusal;
mod skills;
pub(crate) use skills::work_skills;
mod steering;
pub(crate) mod turns;

use crate::{Database, Error};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const DELIVERY_LIMIT: usize = 32;
pub const HISTORY_LIMIT: usize = 200;
pub const PAGE_LIMIT: usize = koru_bot_client_wire::limits::MAX_PAGE_ENTRIES as usize;
pub const RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Only a carrier that has authenticated/vouched the caller may construct this
/// value. It deliberately has no Deserialize implementation.
pub struct Person {
    pub sender: String,
    pub owner: String,
}
impl Person {
    pub(crate) fn receipt_subject(&self) -> String {
        format!("person:{}:{}:{}", self.owner.len(), self.owner, self.sender)
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Envelope {
    pub id: Uuid,
    pub to: Uuid,
    pub thread: Option<Uuid>,
    pub in_reply_to: Option<Uuid>,
    pub body: String,
    pub attachments: Vec<serde_json::Value>,
    pub references: Vec<serde_json::Value>,
    pub deadline_ms: Option<i64>,
    #[serde(default)]
    pub reply_to: ReplyTo,
}

/// Stored content has no Debug: messages and references are never diagnostics.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Content {
    pub body: String,
    pub attachments: Vec<serde_json::Value>,
    pub references: Vec<serde_json::Value>,
    pub in_reply_to: Option<Uuid>,
    pub deadline_ms: Option<i64>,
    #[serde(default)]
    pub reply_to: ReplyTo,
}

/// Rung 1 answers in its thread. Place delivery is added with Routines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReplyTo {
    #[default]
    Thread,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    pub message: Uuid,
    pub thread: Uuid,
    pub sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt<T> {
    pub answer: T,
    pub replayed: bool,
}

impl<T: Serialize> Receipt<T> {
    /// Uses the established Bot protocol's durable receipt and frame codec.
    /// Thread sequence is not a feed cursor.
    pub fn wire(&self, command: Uuid) -> Result<koru_bot_client_wire::WireMessage, Error> {
        Ok(koru_bot_client_wire::WireMessage::Receipt(
            koru_bot_client_wire::Receipt {
                request_id: command,
                outcome: koru_bot_client_wire::Outcome::Completed,
                response: Some(
                    koru_bot_client_wire::JsonObject::from_serialize(&self.answer)
                        .map_err(|_| Error::InvalidRequest)?,
                ),
                failure: None,
                replayed: self.replayed,
                feed_cursor: None,
            },
        ))
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub sequence: u64,
    pub sender: String,
    pub kind: String,
    pub content: Content,
    pub created_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Turn {
    pub id: Uuid,
    pub generation: u64,
    pub worker: Uuid,
    pub session: Uuid,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct InputMessage {
    pub id: Uuid,
    pub sequence: u64,
    pub sender: String,
    /// Only body text may be cut. Attachments and references stay structured;
    /// an input too small for them is refused without accepting a delivery.
    pub content: Content,
    pub truncated: bool,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    pub id: Uuid,
    pub turn: Turn,
    pub history_boundary: u64,
    pub context: Vec<InputMessage>,
    pub work: Vec<InputMessage>,
    pub older_history_available: bool,
    pub continuation: bool,
    pub uncertain: bool,
    /// Delivery ids proven present in resumed history. Their obligations are
    /// still work to finish, but their content must not be sent again.
    pub resume_accepted: Vec<Uuid>,
}

#[derive(Serialize, Deserialize)]
pub enum Resume {
    Rebuilt,
    /// This is supplied only by an adapter qualified to read exact delivery ids.
    Resumed {
        found_deliveries: Vec<Uuid>,
    },
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub id: Uuid,
    pub operation: Uuid,
    pub kind: OutputKind,
    pub reference: serde_json::Value,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    Page,
    Table,
    Task,
}
impl OutputKind {
    pub fn token(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Table => "table",
            Self::Task => "task",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Publish {
    pub command: Uuid,
    pub turn: Turn,
    pub body: String,
    pub outputs: Vec<Output>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Published {
    pub message: Uuid,
    pub answered: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stopped {
    pub cancelled: u64,
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(value).map_err(|_| Error::InvalidRequest)
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(bytes).map_err(|_| Error::StorageUnavailable)
}
fn id(value: String) -> Result<Uuid, Error> {
    Uuid::parse_str(&value).map_err(|_| Error::StorageUnavailable)
}

pub(crate) fn authorize_bot(
    connection: &rusqlite::Connection,
    person: &Person,
    bot: Uuid,
) -> Result<(), Error> {
    if person.sender.is_empty() || person.owner.is_empty() || bot.is_nil() {
        return Err(Error::Unauthenticated);
    }
    let allowed: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM bots WHERE id=?1 AND owner=?2 AND lifecycle='active')",
        rusqlite::params![bot.to_string(), person.owner],
        |r| r.get(0),
    )?;
    if allowed {
        Ok(())
    } else {
        Err(Error::NotAllowed)
    }
}
pub(crate) fn authorize_thread(
    connection: &rusqlite::Connection,
    person: &Person,
    thread: Uuid,
) -> Result<(), Error> {
    if person.owner.is_empty() || person.sender.is_empty() {
        return Err(Error::Unauthenticated);
    }
    let allowed:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM threads t JOIN bots b ON b.id=t.bot WHERE t.id=?1 AND b.owner=?2 AND (t.kind='between_bots' OR EXISTS(SELECT 1 FROM participants p WHERE p.thread=t.id AND p.participant=?3 AND p.kind='person')))",rusqlite::params![thread.to_string(),person.owner,person.sender],|r|r.get(0))?;
    if allowed {
        Ok(())
    } else {
        Err(Error::NotAllowed)
    }
}
pub(crate) fn next_sequence(connection: &rusqlite::Connection, thread: Uuid) -> Result<u64, Error> {
    Ok(connection.query_row(
        "SELECT COALESCE(MAX(sequence),0)+1 FROM messages WHERE thread=?1",
        [thread.to_string()],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
pub(crate) mod tests;
