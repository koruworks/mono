// SPDX-License-Identifier: MPL-2.0
//! Durable engine Routines. The actor owns time and admission; this core owns
//! definitions, grants and occurrence facts in the same database as messages.
pub(crate) mod clock;
mod control;
pub(crate) mod schedule;
mod startup;
pub(crate) mod tools;
use crate::{
    Database, Error,
    messages::{self, Person, Receipt},
};
pub use control::change;
use koru_delegate_core::{BotAccess, CapturedBotPolicy};
use koru_recurrence::RecurrenceRule;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
pub use startup::claim as claim_startup;
use uuid::Uuid;

pub const PAGE_LIMIT: u8 = 16;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Change {
    Create {
        bot: Uuid,
        expected_bot_revision: u64,
        name: String,
        objective: String,
        recurrence: RecurrenceRule,
        documents: Vec<Uuid>,
    },
    Edit {
        bot: Uuid,
        routine: Uuid,
        revision: u32,
        name: String,
        objective: String,
        recurrence: RecurrenceRule,
    },
    Timing {
        bot: Uuid,
        routine: Uuid,
        revision: u32,
        name: String,
        recurrence: RecurrenceRule,
    },
    Enable {
        bot: Uuid,
        routine: Uuid,
        revision: u32,
        enabled: bool,
    },
    Access {
        bot: Uuid,
        routine: Uuid,
        revision: u64,
        access: CapturedBotPolicy,
    },
    RunNow {
        bot: Uuid,
        routine: Uuid,
        revision: u32,
    },
}
impl Change {
    pub fn bot(&self) -> Uuid {
        match self {
            Self::Create { bot, .. }
            | Self::Edit { bot, .. }
            | Self::Timing { bot, .. }
            | Self::Enable { bot, .. }
            | Self::Access { bot, .. }
            | Self::RunNow { bot, .. } => *bot,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Routine {
    pub id: Uuid,
    pub bot: Uuid,
    pub thread: Uuid,
    pub created_by: Uuid,
    pub revision: u32,
    pub grant_revision: u64,
    pub source_policy_revision: u64,
    pub name: String,
    pub objective: String,
    pub recurrence: RecurrenceRule,
    pub access: CapturedBotPolicy,
    pub initial_access: CapturedBotPolicy,
    pub enabled: bool,
    pub effective_at_ms: i64,
    pub observed_through_ms: Option<i64>,
    pub next_due_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Page {
    pub items: Vec<Routine>,
    pub next: Option<Uuid>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Occurrence {
    pub id: Uuid,
    pub routine: Uuid,
    pub definition_revision: u32,
    pub occurrence_ms: i64,
    pub manual: bool,
    pub state: String,
    pub reason: Option<String>,
    pub message: Option<Uuid>,
    pub reply: Option<Uuid>,
    pub card: Option<Uuid>,
    pub recorded_at_ms: i64,
    pub updated_at_ms: i64,
    pub preview: Option<koru_ai_core::bots::BotReplyPreview>,
}
#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Cursor {
    pub at_ms: i64,
    pub id: Uuid,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Detail {
    pub routine: Routine,
    pub occurrences: Vec<Occurrence>,
    pub next: Option<Cursor>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Changed {
    pub routine: Routine,
    pub occurrence: Option<Occurrence>,
    /// UI may offer startup consent after this first explicit enablement.
    pub first_enabled: bool,
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(value).map_err(|_| Error::InvalidRequest)
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(bytes).map_err(|_| Error::StorageUnavailable)
}
fn id(value: String) -> Result<Uuid, Error> {
    value.parse().map_err(|_| Error::StorageUnavailable)
}
fn authorize(c: &Connection, who: &Person, bot: Uuid) -> Result<(), Error> {
    let yes: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM bots WHERE id=?1 AND owner=?2)",
        params![bot.to_string(), who.owner],
        |r| r.get(0),
    )?;
    if yes { Ok(()) } else { Err(Error::NotAllowed) }
}
fn read(c: &Connection, bot: Uuid, key: Uuid) -> Result<Routine, Error> {
    let row=c.query_row("SELECT thread,created_by,revision,grant_revision,source_policy_revision,name,objective,recurrence,access,initial_access,enabled,effective_at_ms,observed_through_ms,next_due_ms,created_at_ms,updated_at_ms FROM routines WHERE id=?1 AND bot=?2",params![key.to_string(),bot.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get::<_,Vec<u8>>(7)?,r.get::<_,Vec<u8>>(8)?,r.get::<_,Vec<u8>>(9)?,r.get(10)?,r.get(11)?,r.get(12)?,r.get(13)?,r.get(14)?,r.get(15)?))).optional()?.ok_or(Error::NotAllowed)?;
    Ok(Routine {
        id: key,
        bot,
        thread: id(row.0)?,
        created_by: id(row.1)?,
        revision: row.2,
        grant_revision: row.3,
        source_policy_revision: row.4,
        name: row.5,
        objective: row.6,
        recurrence: decode(&row.7)?,
        access: decode(&row.8)?,
        initial_access: decode(&row.9)?,
        enabled: row.10,
        effective_at_ms: row.11,
        observed_through_ms: row.12,
        next_due_ms: row.13,
        created_at_ms: row.14,
        updated_at_ms: row.15,
    })
}
fn occurrence(c: &Connection, key: Uuid) -> Result<Occurrence, Error> {
    let row=c.query_row("SELECT routine,definition_revision,occurrence_ms,manual,state,reason,message,reply,card,recorded_at_ms,updated_at_ms FROM routine_occurrences WHERE id=?1",[key.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,Option<String>>(8)?,r.get(9)?,r.get(10)?)))?;
    let mut state: String = row.4;
    if state=="running" && c.query_row("SELECT EXISTS(SELECT 1 FROM routine_occurrences r JOIN routines s ON s.id=r.routine JOIN obligations o ON o.message=r.message AND o.bot=s.bot AND o.thread=s.thread JOIN delivery_obligations x ON x.obligation=o.id JOIN deliveries d ON d.id=x.delivery AND d.state='accepted' JOIN turns t ON t.id=d.turn AND t.bot=s.bot AND t.thread=s.thread WHERE r.id=?1 AND o.state='delivered' AND t.state IN ('needs_you','parked') AND t.stopped=0)",[key.to_string()],|r|r.get::<_,bool>(0))? {
        state="needs_you".into();
    }
    let preview: Option<(String,Vec<u8>)>=c.query_row("SELECT m.id,m.payload FROM routine_occurrences o JOIN routines r ON r.id=o.routine JOIN messages m ON m.id=o.reply AND m.thread=r.thread AND m.sender=r.bot AND m.state='published' AND m.kind='words' WHERE o.id=?1 AND o.state='answered'",[key.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let preview = preview
        .map(|(message, payload)| {
            let content: messages::Content = decode(&payload)?;
            Ok::<_, Error>(koru_ai_core::bots::BotReplyPreview::new(
                koru_ai_core::MessageId::from_uuid(id(message)?),
                &content.body,
            ))
        })
        .transpose()?
        .flatten();
    Ok(Occurrence {
        id: key,
        routine: id(row.0)?,
        definition_revision: row.1,
        occurrence_ms: row.2,
        manual: row.3,
        state,
        reason: row.5,
        message: row.6.map(id).transpose()?,
        reply: row.7.map(id).transpose()?,
        card: row.8.map(id).transpose()?,
        recorded_at_ms: row.9,
        updated_at_ms: row.10,
        preview,
    })
}
impl Database {
    pub fn routine_creation(
        &self,
        who: &Person,
        bot: Uuid,
        request: Uuid,
    ) -> Result<Routine, Error> {
        authorize(&self.connection, who, bot)?;
        let key: Option<String> = self
            .connection
            .query_row(
                "SELECT id FROM routines WHERE bot=?1 AND created_by=?2",
                params![bot.to_string(), request.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        read(&self.connection, bot, id(key.ok_or(Error::NotAllowed)?)?)
    }
    pub fn routine_receipt(
        &self,
        who: &Person,
        command: Uuid,
        action: &Change,
    ) -> Result<Option<Receipt<Changed>>, Error> {
        authorize(&self.connection, who, action.bot())?;
        let digest = messages::receipts::digest("routine-change", action)?;
        Ok(
            messages::receipts::find(&self.connection, &who.receipt_subject(), command, &digest)?
                .map(|answer| Receipt {
                    answer,
                    replayed: true,
                }),
        )
    }
    pub fn routines(
        &self,
        who: &Person,
        bot: Uuid,
        after: Option<Uuid>,
        limit: u8,
    ) -> Result<Page, Error> {
        authorize(&self.connection, who, bot)?;
        if !(1..=PAGE_LIMIT).contains(&limit) {
            return Err(Error::InvalidRequest);
        }
        let keys=self.connection.prepare("SELECT id FROM routines WHERE bot=?1 AND (?2 IS NULL OR id>?2) ORDER BY id LIMIT ?3")?.query_map(params![bot.to_string(),after.map(|x|x.to_string()),limit+1],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        let mut more = keys.len() > limit as usize;
        let mut items = vec![];
        let mut bytes = 0;
        for key in keys.into_iter().take(limit as usize) {
            let item = read(&self.connection, bot, id(key)?)?;
            let size = encode(&item)?.len();
            if size > crate::person::MAX_COMMAND_BYTES {
                return Err(Error::InputTooSmall);
            }
            if bytes + size > crate::person::MAX_COMMAND_BYTES {
                more = true;
                break;
            }
            bytes += size;
            items.push(item);
        }
        let next = if more {
            items.last().map(|r| r.id)
        } else {
            None
        };
        Ok(Page { items, next })
    }
    pub fn routine(
        &self,
        who: &Person,
        bot: Uuid,
        key: Uuid,
        before: Option<Cursor>,
        limit: u8,
    ) -> Result<Detail, Error> {
        authorize(&self.connection, who, bot)?;
        if !(1..=PAGE_LIMIT).contains(&limit) {
            return Err(Error::InvalidRequest);
        }
        let routine = read(&self.connection, bot, key)?;
        let keys=self.connection.prepare("SELECT id FROM routine_occurrences WHERE routine=?1 AND (?2 IS NULL OR occurrence_ms<?2 OR (occurrence_ms=?2 AND id<?3)) ORDER BY occurrence_ms DESC,id DESC LIMIT ?4")?.query_map(params![key.to_string(),before.as_ref().map(|x|x.at_ms),before.as_ref().map(|x|x.id.to_string()),limit+1],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        let more = keys.len() > limit as usize;
        let occurrences = keys
            .into_iter()
            .take(limit as usize)
            .map(|key| occurrence(&self.connection, id(key)?))
            .collect::<Result<Vec<_>, Error>>()?;
        let next = if more {
            occurrences.last().map(|r| Cursor {
                at_ms: r.occurrence_ms,
                id: r.id,
            })
        } else {
            None
        };
        Ok(Detail {
            routine,
            occurrences,
            next,
        })
    }
}

/// One source of thread policy for session credentials, context, turn capture
/// and every current tool-call restriction. Routine access never falls back.
pub(crate) fn grant(c: &Connection, bot: Uuid, thread: Uuid) -> Result<Vec<u8>, Error> {
    let (kind,owner,main):(String,String,Vec<u8>)=c.query_row("SELECT t.kind,t.bot,b.access FROM threads t JOIN bots b ON b.id=?1 JOIN participants p ON p.thread=t.id AND p.participant=b.id AND p.kind='bot' WHERE t.id=?2",params![bot.to_string(),thread.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    // Another participating Bot always uses its own access, including when
    // invited into a Routine's thread (technical design section 10).
    if kind != "routine" || owner != bot.to_string() {
        return Ok(main);
    }
    c.query_row(
        "SELECT access FROM routines WHERE bot=?1 AND thread=?2",
        params![bot.to_string(), thread.to_string()],
        |r| r.get(0),
    )
    .optional()?
    .ok_or(Error::NotAllowed)
}

#[cfg(test)]
mod tests;
