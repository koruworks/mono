// SPDX-License-Identifier: MPL-2.0
//! Bounded live observations. Durable answers and evidence remain in the
//! message/operation owners; a stream gap is explicit, never silent loss.
use crate::{
    Database, Error,
    messages::{Person, Turn},
};
use koru_session_protocol::AgentEvent;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub struct Observation {
    pub thread: Uuid,
    pub bot: Uuid,
    pub turn: Turn,
    pub event: AgentEvent,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub cursor: u64,
    pub observation: Observation,
}
#[derive(Serialize, Deserialize)]
pub struct Page {
    pub boot: Uuid,
    pub cursor: u64,
    pub gap: bool,
    pub entries: Vec<Entry>,
}
pub struct LiveEvents {
    boot: Uuid,
    next: u64,
    bytes: usize,
    entries: VecDeque<(Entry, usize)>,
}
impl Default for LiveEvents {
    fn default() -> Self {
        Self {
            boot: Uuid::new_v4(),
            next: 1,
            bytes: 0,
            entries: VecDeque::new(),
        }
    }
}
impl LiveEvents {
    pub(crate) fn push(&mut self, observation: Observation) -> Result<(), Error> {
        let bytes = serde_json::to_vec(&observation)
            .map_err(|_| Error::InvalidRequest)?
            .len();
        if bytes > koru_bot_client_wire::MAX_FRAME_BYTES / 2 {
            return Err(Error::InputTooSmall);
        }
        while self.entries.len() >= 512 || self.bytes + bytes > 4 * 1024 * 1024 {
            let Some((_, old)) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= old;
        }
        let cursor = self.next;
        self.next = self.next.checked_add(1).ok_or(Error::StorageUnavailable)?;
        self.entries.push_back((
            Entry {
                cursor,
                observation,
            },
            bytes,
        ));
        self.bytes += bytes;
        Ok(())
    }
    /// The authenticated person's thread membership is checked before any
    /// content or cursor is exposed. UI adapters must reset on a gap.
    pub fn read(
        &self,
        db: &Database,
        person: &Person,
        thread: Uuid,
        boot: Option<Uuid>,
        after: u64,
    ) -> Result<Page, Error> {
        crate::messages::authorize_thread(&db.connection, person, thread)?;
        let floor = self.entries.front().map_or(self.next, |e| e.0.cursor);
        let gap = boot != Some(self.boot) || after >= self.next || after.saturating_add(1) < floor;
        let after = if gap { 0 } else { after };
        let mut bytes = 0;
        let mut entries = vec![];
        let mut cursor = after;
        for (entry, size) in &self.entries {
            if entry.cursor <= after {
                continue;
            }
            if entry.observation.thread == thread {
                if !entries.is_empty() && bytes + size > koru_bot_client_wire::MAX_FRAME_BYTES / 2 {
                    break;
                }
                bytes += size;
                entries.push(entry.clone());
            }
            cursor = entry.cursor;
        }
        Ok(Page {
            boot: self.boot,
            cursor,
            gap,
            entries,
        })
    }
}

#[cfg(test)]
mod tests;
