// SPDX-License-Identifier: MPL-2.0

//! Grow-only permanent deletion events under the existing pages root.

use super::*;
use crate::pages::purge::{Coverage, Event, Insert};
use uuid::Uuid;

const PURGED: &str = "purged";

impl SystemDoc {
    /// Every retained document tombstone, including malformed evidence. The
    /// latter still prevents resurrection, but never proves local content safe
    /// to discard. Call `purge_coverage` before reconciling local content.
    pub fn purged_documents(&self) -> Vec<DocumentId> {
        let Some(documents) = record_map(&self.doc.get_map(C_PAGES), PURGED) else {
            return Vec::new();
        };
        let mut ids: Vec<_> = documents
            .keys()
            .filter_map(|key| DocumentId::parse(&key))
            .collect();
        ids.sort_unstable();
        ids
    }

    pub fn is_document_purged(&self, document: DocumentId) -> bool {
        record_map(&self.doc.get_map(C_PAGES), PURGED)
            .is_some_and(|documents| documents.get(&document.to_storage()).is_some())
    }

    /// All events in deterministic document/event order. A malformed payload
    /// is a refusal, not a silently missing event or a successful Cloud read.
    pub fn purge_events(&self) -> Result<Vec<Event>> {
        let mut out = Vec::new();
        let mut identities = std::collections::HashSet::new();
        for document in self.purged_documents() {
            for event in self.document_purge_events(document)? {
                if !identities.insert(event.event_id) {
                    return Err(StoreError::Malformed("purge_event_conflict"));
                }
                out.push(event);
            }
        }
        Ok(out)
    }

    pub fn document_purge_events(&self, document: DocumentId) -> Result<Vec<Event>> {
        let Some(documents) = record_map(&self.doc.get_map(C_PAGES), PURGED) else {
            return Ok(Vec::new());
        };
        let Some(value) = documents.get(&document.to_storage()) else {
            return Ok(Vec::new());
        };
        let ValueOrContainer::Container(container) = value else {
            return Err(StoreError::Malformed("purge_document_events"));
        };
        let events = container
            .into_map()
            .map_err(|_| StoreError::Malformed("purge_document_events"))?;
        let mut out = Vec::new();
        for key in events.keys() {
            let event = events
                .get(&key)
                .and_then(read_event)
                .filter(|event| {
                    event.document_id == document.as_uuid()
                        && key.as_str() == event.event_id.to_string()
                })
                .ok_or(StoreError::Malformed("purge_event"))?;
            out.push(event);
        }
        out.sort_unstable_by_key(|event| event.event_id);
        Ok(out)
    }

    /// No evidence means live. Unknown or malformed evidence requires a
    /// question whenever this device holds content for the original id.
    pub fn purge_coverage(&self, document: DocumentId) -> Option<Coverage> {
        if !self.is_document_purged(document) {
            return None;
        }
        Some(match self.document_purge_events(document) {
            Ok(events) => Coverage::of(&events).unwrap_or(Coverage::Unknown),
            Err(_) => Coverage::Unknown,
        })
    }

    /// Persist the fixed payload only once. The caller must have checked both
    /// full carrier encodings before this first write; this function never
    /// changes the version to make an event fit a later transmission.
    pub fn write_purge_event(&mut self, event: &Event) -> Result<Insert> {
        event
            .validate()
            .map_err(|_| StoreError::Malformed("purge_event"))?;
        // Event identity is global, not merely unique under one document.
        for previous in self.purge_events()? {
            if previous.event_id == event.event_id {
                return Ok(if previous == *event {
                    Insert::Present
                } else {
                    Insert::Conflict
                });
            }
        }
        let events = self
            .doc
            .get_map(C_PAGES)
            .ensure_mergeable_map(PURGED)?
            .ensure_mergeable_map(&event.document_id.to_string())?;
        // One atomic value: concurrently observed fields cannot form a
        // synthetic payload that neither writer ever minted.
        events.insert(&event.event_id.to_string(), write_event(event))?;
        self.doc.commit();
        Ok(Insert::Inserted)
    }

    /// Raw CRDT imports must not overwrite an immutable event. Preview on a
    /// Loro fork only after this document contains tombstones, retaining the
    /// live document and its observers. Refuse an entire corrupt batch before
    /// any of its state is imported; never choose its winning payload by time.
    pub(super) fn check_purge_import<'a>(
        &self,
        frames: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<()> {
        if self.purged_documents().is_empty() {
            return Ok(());
        }
        let before = self.purge_events()?;
        let candidate = Self {
            doc: self.doc.fork(),
        };
        for frame in frames {
            candidate.doc.import(frame)?;
        }
        let after: BTreeMap<_, _> = candidate
            .purge_events()?
            .into_iter()
            .map(|event| (event.event_id, event))
            .collect();
        for event in before {
            if after.get(&event.event_id) != Some(&event) {
                return Err(StoreError::Malformed("purge_event_conflict"));
            }
        }
        Ok(())
    }
}

fn write_event(event: &Event) -> LoroValue {
    let mut fields = HashMap::from([
        (
            "event_id".to_owned(),
            LoroValue::from(event.event_id.to_string()),
        ),
        (
            "document_id".to_owned(),
            LoroValue::from(event.document_id.to_string()),
        ),
        (
            "root_id".to_owned(),
            LoroValue::from(event.root_id.to_string()),
        ),
        ("by".to_owned(), LoroValue::from(event.by.clone())),
    ]);
    if let Some(version) = &event.version {
        fields.insert(
            "version".to_owned(),
            LoroValue::Binary(version.clone().into()),
        );
    }
    LoroValue::from(fields)
}

fn read_event(value: ValueOrContainer) -> Option<Event> {
    let ValueOrContainer::Value(LoroValue::Map(fields)) = value else {
        return None;
    };
    if fields.keys().any(|key| {
        !matches!(
            key.as_str(),
            "event_id" | "document_id" | "root_id" | "version" | "by"
        )
    }) {
        return None;
    }
    let text = |name: &str| match fields.get(name)? {
        LoroValue::String(value) => Some(value.to_string()),
        _ => None,
    };
    let version = match fields.get("version") {
        None => None,
        Some(LoroValue::Binary(value)) => Some(value.to_vec()),
        _ => return None,
    };
    let event = Event {
        event_id: Uuid::parse_str(&text("event_id")?).ok()?,
        document_id: Uuid::parse_str(&text("document_id")?).ok()?,
        root_id: Uuid::parse_str(&text("root_id")?).ok()?,
        version,
        by: text("by")?,
    };
    event.validate().ok()?;
    Some(event)
}

#[cfg(test)]
mod tests;
