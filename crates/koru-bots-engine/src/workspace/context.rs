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
        let (role, knowledge, memory): (Option<String>, Vec<u8>, Option<String>) =
            self.connection.query_row(
                "SELECT role_page,knowledge_pages,memory_page FROM bots WHERE id=?1",
                [who.bot.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        let grant = crate::routines::grant(&self.connection, who.bot, thread)?;
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
