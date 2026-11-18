// SPDX-License-Identifier: MPL-2.0

//! Worker-local journal and provider state; never evidence of a native write.

use crate::Error;
use koru_sqlite::{Migration, Schema};
use std::path::Path;
use uuid::Uuid;

const SCHEMA: Schema = Schema {
    database: "bot.db",
    understood: 1,
    migrations: &[Migration {
        to: 1,
        name: "001_bot.sql",
        sql: include_str!("../migrations/001_bot.sql"),
        columns: &[],
    }],
};

pub struct BotDatabase {
    connection: rusqlite::Connection,
}

impl BotDatabase {
    pub fn open(root: &Path, bot: Uuid) -> Result<Self, Error> {
        if bot.is_nil() {
            return Err(Error::InvalidRequest);
        }
        Ok(Self {
            connection: koru_sqlite::open(
                &SCHEMA,
                &root.join("bots").join(bot.to_string()).join("bot.db"),
            )?,
        })
    }
    pub fn in_memory() -> Result<Self, Error> {
        Ok(Self {
            connection: koru_sqlite::open_in_memory(&SCHEMA)?,
        })
    }
    pub fn journal(&self, turn: Uuid, entry: JournalEntry<'_>, at_ms: i64) -> Result<(), Error> {
        let (kind, tool, payload) = match entry {
            JournalEntry::Input(bytes) => ("input", None, bytes),
            JournalEntry::Event(bytes) => ("event", None, bytes),
            JournalEntry::ToolCall { name, payload } => ("tool_call", Some(name), payload),
            JournalEntry::EngineDelivery(bytes) => ("engine_delivery", None, bytes),
        };
        self.connection.execute("INSERT INTO turn_journal(turn,kind,tool_name,payload,recorded_at_ms) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![turn.to_string(), kind, tool, payload, at_ms])?;
        Ok(())
    }
    /// Completion means SQLite FULL durability, before the session can yield.
    pub fn save(&self, state: &AgentState) -> Result<(), Error> {
        let tx = self.connection.unchecked_transaction()?;
        tx.execute("INSERT INTO agent_state(thread,session,provider,resume_state,last_delivery,saved_at_ms) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(thread,session) DO UPDATE SET provider=excluded.provider,resume_state=excluded.resume_state,last_delivery=excluded.last_delivery,saved_at_ms=excluded.saved_at_ms",
            rusqlite::params![state.thread.to_string(), state.session.to_string(), state.provider, state.resume_state, state.last_delivery.map(|id| id.to_string()), state.saved_at_ms])?;
        // These are cumulative resume optimizations, not the journal. Keep
        // the latest two incarnations per provider/thread; otherwise N resumes
        // retain N copies of the growing proof list. Inputs/events stay in the
        // append-only journal, and engine obligations remain authoritative.
        tx.execute("DELETE FROM agent_state WHERE thread=?1 AND provider=?2 AND session NOT IN (SELECT session FROM agent_state WHERE thread=?1 AND provider=?2 ORDER BY saved_at_ms DESC,rowid DESC LIMIT 2)",rusqlite::params![state.thread.to_string(),state.provider])?;
        tx.commit()?;
        Ok(())
    }
    pub fn load(&self, thread: Uuid, session: Uuid) -> Result<Option<AgentState>, Error> {
        use rusqlite::OptionalExtension;
        let row: Option<(String, Vec<u8>, Option<String>, i64)> = self.connection.query_row(
            "SELECT provider,resume_state,last_delivery,saved_at_ms FROM agent_state WHERE thread=?1 AND session=?2",
            rusqlite::params![thread.to_string(), session.to_string()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?;
        row.map(|(provider, resume_state, delivery, saved_at_ms)| {
            Ok(AgentState {
                thread,
                session,
                provider,
                resume_state,
                saved_at_ms,
                last_delivery: delivery
                    .map(|id| Uuid::parse_str(&id).map_err(|_| Error::StorageUnavailable))
                    .transpose()?,
            })
        })
        .transpose()
    }

    /// The latest incarnation for this thread and provider. A token from a
    /// different provider is never handed to the selected adapter.
    pub fn latest(&self, thread: Uuid, provider: &str) -> Result<Option<AgentState>, Error> {
        use rusqlite::OptionalExtension;
        let session: Option<String> = self.connection.query_row(
            "SELECT session FROM agent_state WHERE thread=?1 AND provider=?2 ORDER BY saved_at_ms DESC,rowid DESC LIMIT 1",
            rusqlite::params![thread.to_string(), provider], |r| r.get(0)).optional()?;
        session
            .map(|session| {
                self.load(
                    thread,
                    Uuid::parse_str(&session).map_err(|_| Error::StorageUnavailable)?,
                )
            })
            .transpose()
            .map(Option::flatten)
    }
}

pub enum JournalEntry<'a> {
    Input(&'a [u8]),
    Event(&'a [u8]),
    ToolCall { name: &'a str, payload: &'a [u8] },
    EngineDelivery(&'a [u8]),
}

#[derive(Clone, PartialEq, Eq)]
pub struct AgentState {
    pub thread: Uuid,
    pub session: Uuid,
    pub provider: String,
    pub resume_state: Vec<u8>,
    pub last_delivery: Option<Uuid>,
    pub saved_at_ms: i64,
}

#[cfg(test)]
mod tests;
