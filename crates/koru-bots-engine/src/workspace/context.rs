// SPDX-License-Identifier: MPL-2.0

//! Session context is selected by registered-worker authority, not tool input.
//! The engine supplies page references; only the gateway reads their content.
use crate::{
    Database, Error,
    authority::{self, Worker},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub struct Context {
    pub role: Option<Page>,
    pub knowledge: Vec<Page>,
    pub memory: Option<Page>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Page {
    pub id: Uuid,
    pub text: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    pub scope: String,
    pub grant: Value,
    pub role: Option<Uuid>,
    pub knowledge: Vec<Uuid>,
    pub memory: Option<Uuid>,
}
impl Sources {
    pub fn validate(&self) -> Result<(), Error> {
        // The existing Bot library admits sixteen Knowledge pages, all in its
        // home workspace. Preserve that bound in the new execution owner.
        if self.knowledge.len() > 16
            || self
                .role
                .iter()
                .chain(self.knowledge.iter())
                .chain(self.memory.iter())
                .any(Uuid::is_nil)
        {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }
}
impl Database {
    pub(crate) fn context_sources(&self, who: Worker, thread: Uuid) -> Result<Sources, Error> {
        let scope = authority::membership(&self.connection, who, thread)?;
        let (grant, role, knowledge, memory, kind): (Vec<u8>, Option<String>, Vec<u8>, Option<String>, String) = self.connection.query_row(
            "SELECT b.access,b.role_page,b.knowledge_pages,b.memory_page,t.kind FROM bots b JOIN threads t ON t.id=?2 WHERE b.id=?1",
            rusqlite::params![who.bot.to_string(),thread.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )?;
        if kind == "routine" {
            return Err(Error::NotAllowed);
        }
        let parse = |value: String| Uuid::parse_str(&value).map_err(|_| Error::StorageUnavailable);
        let sources = Sources {
            scope,
            grant: serde_json::from_slice(&grant).map_err(|_| Error::StorageUnavailable)?,
            role: role.map(parse).transpose()?,
            knowledge: serde_json::from_slice(&knowledge).map_err(|_| Error::StorageUnavailable)?,
            memory: memory.map(parse).transpose()?,
        };
        sources.validate()?;
        Ok(sources)
    }
}
