// SPDX-License-Identifier: MPL-2.0

//! Local receipts for Cloud's ordered document-removal list. A cursor only
//! becomes durable after the caller's merged SystemDoc and every receipt do.
//! Each event occupies one bounded store_state value, never a growing array.

use super::*;
use uuid::Uuid;

const PAGE_MAX: usize = 64;
const CURSOR_PREFIX: &str = "document_removal.cursor.";
const EVENT_PREFIX: &str = "document_removal.received.";

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    incarnation: Uuid,
    system: DocumentId,
    /// Changing either scope id invalidates every old receipt, including if
    /// a later request names an earlier scope again. No key scan is needed.
    generation: Uuid,
    cursor: u64,
}

impl Cursor {
    fn matches(&self, incarnation: Uuid, system: DocumentId) -> bool {
        self.incarnation == incarnation && self.system == system
    }
}

fn cursor_key(workspace: WorkspaceId) -> String {
    format!("{CURSOR_PREFIX}{workspace}")
}

fn event_key(workspace: WorkspaceId, event: Uuid) -> String {
    format!("{EVENT_PREFIX}{workspace}.{event}")
}

fn scope(workspace: WorkspaceId, incarnation: Uuid, system: DocumentId) -> Result<()> {
    if workspace.as_uuid().is_nil() || incarnation.is_nil() || system.as_uuid().is_nil() {
        return Err(Error::new(
            Code::InvalidRequest,
            "nil document-removal scope",
        ));
    }
    Ok(())
}

fn decode(raw: Option<String>) -> Result<Option<Cursor>> {
    raw.map(|raw| {
        let cursor: Cursor = serde_json::from_str(&raw)
            .map_err(|_| Error::new(Code::StorageFailed, "invalid document-removal cursor"))?;
        if cursor.incarnation.is_nil()
            || cursor.system.as_uuid().is_nil()
            || cursor.generation.is_nil()
        {
            return Err(Error::new(
                Code::StorageFailed,
                "invalid document-removal cursor scope",
            ));
        }
        Ok(cursor)
    })
    .transpose()
}

struct Page {
    cursor_key: String,
    cursor_value: String,
    receipt_keys: Vec<String>,
    receipt_value: String,
}

impl Page {
    fn prepare(
        workspace: WorkspaceId,
        incarnation: Uuid,
        system: DocumentId,
        prior: Option<Cursor>,
        events: &[Uuid],
        cursor: u64,
    ) -> Result<Self> {
        scope(workspace, incarnation, system)?;
        if events.len() > PAGE_MAX || events.iter().any(Uuid::is_nil) {
            return Err(Error::new(
                Code::InvalidRequest,
                "invalid document-removal page",
            ));
        }
        let mut next = prior
            .filter(|old| old.matches(incarnation, system))
            .unwrap_or_else(|| Cursor {
                incarnation,
                system,
                generation: Uuid::new_v4(),
                cursor: 0,
            });
        next.cursor = next.cursor.max(cursor);
        let receipt_keys = events
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|event| event_key(workspace, event))
            .collect();
        Ok(Self {
            cursor_key: cursor_key(workspace),
            cursor_value: serde_json::to_string(&next)
                .map_err(|_| Error::new(Code::StorageFailed, "encode document-removal cursor"))?,
            receipt_keys,
            receipt_value: next.generation.to_string(),
        })
    }

    /// The catalog and the SystemDoc can use different durable transactions.
    /// Keeping this ordering explicit makes any partial failure a safe replay:
    /// a receipt never precedes its content, and a cursor never skips a receipt.
    fn persist<T>(
        &self,
        target: &mut T,
        flush: impl FnOnce(&mut T) -> Result<()>,
        mut set_state: impl FnMut(&mut T, &str, &str) -> Result<()>,
    ) -> Result<()> {
        flush(target)?;
        for key in &self.receipt_keys {
            set_state(target, key, &self.receipt_value)?;
        }
        set_state(target, &self.cursor_key, &self.cursor_value)
    }
}

impl<C: Catalog, B: Backend> Store<C, B> {
    /// Zero means no list has been durably consumed for this exact SystemDoc
    /// and Cloud incarnation. Reads never create or switch the stored scope.
    pub fn document_removal_cursor(
        &self,
        workspace: WorkspaceId,
        incarnation: Uuid,
        system: DocumentId,
    ) -> Result<u64> {
        scope(workspace, incarnation, system)?;
        Ok(decode(self.catalog.state(&cursor_key(workspace))?)?
            .filter(|cursor| cursor.matches(incarnation, system))
            .map_or(0, |cursor| cursor.cursor))
    }

    /// Whether this exact event's list entry was durably merged locally. A
    /// direct remove acknowledgement alone must not produce a list receipt.
    pub fn document_removal_received(
        &self,
        workspace: WorkspaceId,
        incarnation: Uuid,
        system: DocumentId,
        event_id: Uuid,
    ) -> Result<bool> {
        scope(workspace, incarnation, system)?;
        if event_id.is_nil() {
            return Err(Error::new(
                Code::InvalidRequest,
                "nil document-removal event",
            ));
        }
        let Some(cursor) = decode(self.catalog.state(&cursor_key(workspace))?)?
            .filter(|cursor| cursor.matches(incarnation, system))
        else {
            return Ok(false);
        };
        Ok(self
            .catalog
            .state(&event_key(workspace, event_id))?
            .is_some_and(|value| value == cursor.generation.to_string()))
    }

    /// The consumer validates and merges at most 64 list entries first. This
    /// method flushes that SystemDoc itself before writing event receipts and
    /// then the monotonic list cursor. A changed incarnation or SystemDoc
    /// starts a fresh receipt generation. All writes are safe to retry.
    ///
    /// Store deliberately does not interpret the SystemDoc schema; the domain
    /// consumer chooses its canonical document and validates each merged event.
    pub fn commit_document_removals(
        &mut self,
        workspace: WorkspaceId,
        incarnation: Uuid,
        system: DocumentId,
        events: &[Uuid],
        cursor: u64,
        at: Timestamp,
    ) -> Result<()> {
        let page = Page::prepare(
            workspace,
            incarnation,
            system,
            decode(self.catalog.state(&cursor_key(workspace))?)?,
            events,
            cursor,
        )?;
        page.persist(
            self,
            |store| {
                // flush alone permits an unopened document as a no-op. Ensure the
                // exact document exists first; missing content cannot earn receipts.
                store.ensure_document(workspace, system)?;
                store.flush(workspace, system, at)?;
                Ok(())
            },
            |store, key, value| store.catalog.set_state(key, value),
        )
    }
}

#[cfg(test)]
mod tests;
