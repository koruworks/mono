// SPDX-License-Identifier: MPL-2.0
//! One immutable deletion payload, bounded against both complete carriers.
use super::{Control, Error, Record, encode_record};
use crate::cloud::Version1;
pub use koru_core::pages::purge::Event;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Deleting,
    Removed,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub sequence: u64,
    pub document_id: uuid::Uuid,
    pub state: State,
    pub event: Event,
}

pub fn request(event: Event) -> Control {
    Control::CloudRemoveDocument {
        version: Version1,
        operation_id: event.event_id,
        document_id: event.document_id,
        event,
    }
}
pub fn page(events: Vec<Entry>, next_cursor: u64) -> Control {
    Control::CloudRemovals {
        version: Version1,
        events,
        next_cursor,
    }
}
pub fn read(cursor: u64) -> Control {
    Control::CloudRemovalsRead {
        version: Version1,
        cursor,
    }
}

pub fn fits(event: &Event) -> bool {
    encode_record(&Record::Control(request(event.clone()))).is_ok()
        && encode_record(&Record::Control(page(
            vec![Entry {
                sequence: i64::MAX as u64,
                document_id: event.document_id,
                state: State::Deleting,
                event: event.clone(),
            }],
            i64::MAX as u64,
        )))
        .is_ok()
}

/// Call exactly once, BEFORE storing or sending. Retries must never alter it.
pub fn bounded_event(mut event: Event) -> Result<Event, Error> {
    let mut end = event.by.len().min(128);
    while !event.by.is_char_boundary(end) {
        end -= 1;
    }
    event.by.truncate(end);
    event.validate().map_err(|_| Error::Invalid)?;
    if !fits(&event) {
        event.version = None;
    }
    if !fits(&event) {
        return Err(Error::TooLarge);
    }
    Ok(event)
}

pub(super) fn validate_page(events: &[Entry], cursor: u64) -> Result<(), Error> {
    if cursor > i64::MAX as u64 {
        return Err(Error::Invalid);
    }
    let mut last = 0;
    for entry in events {
        if entry.sequence <= last
            || entry.sequence > cursor
            || entry.document_id != entry.event.document_id
        {
            return Err(Error::Invalid);
        }
        entry.event.validate().map_err(|_| Error::Invalid)?;
        last = entry.sequence;
    }
    if !events.is_empty() && last != cursor {
        return Err(Error::Invalid);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
