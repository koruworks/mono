// SPDX-License-Identifier: MPL-2.0

//! One reader's private view state for one document.
//!
//! Today that is which toggles the reader closed. It is a Loro document of its
//! own — never part of the shared document, never replicated, never in an
//! export — stored as one complete snapshot per (document, reader Profile) in
//! the workspace's `document_private_crdts` table. Its lineage is independent
//! of the shared document's, so folding never moves a shared version vector,
//! an edit time, history or a sync signal.
//!
//! Shape (stable storage keys):
//!
//! - map `meta`: `format` = [`FORMAT`], `schema_version`, `document_id`,
//!   `owner_profile_id`. Checked on every load, so a row copied under the
//!   wrong key is refused rather than read as someone else's folds.
//! - map `nodes`: node uuid → map of private attributes. Today only
//!   [`COLLAPSED`], a Boolean; absent and `false` both mean open.
//!
//! A client names the value it wants, never "invert": repeating a change is a
//! no-op, and two windows changing different nodes both keep their change
//! because each write merges into the stored state rather than replacing it.

use std::collections::BTreeMap;

use loro::{ExportMode, LoroDoc, LoroMap, LoroValue, ValueOrContainer};
use serde::{Deserialize, Serialize};

use crate::errors::{Code, Error, Result};
use crate::ids::{DocumentId, ProfileId};
use crate::version::{Compatibility, DOCUMENT_PRIVATE_CRDT_SCHEMA_VERSION};

/// The marker that says these bytes are a document-private unit.
pub const FORMAT: &str = "koru.document-private";

/// The one private attribute this version understands. The same key as
/// `koru_core::toggle::COLLAPSED`; repeated because the Store does not link
/// the document model.
pub const COLLAPSED: &str = "koru.collapsed";

/// The most changes one request may carry. A whole-document command larger
/// than this is split into several batches by the client.
pub const MAX_BATCH: usize = 512;

/// One requested change: set `key` on `node_id` to `value`, or clear it when
/// `value` is `null`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PrivateChange {
    pub node_id: String,
    pub key: String,
    pub value: Option<bool>,
}

/// What a reader sees: their private attributes by node, and a revision that
/// grows with every effective change. Absent state is empty at revision 0.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PrivateState {
    pub revision: u64,
    pub nodes: BTreeMap<String, BTreeMap<String, bool>>,
}

/// The private unit for one (document, reader).
pub struct DocumentPrivate {
    doc: LoroDoc,
}

impl DocumentPrivate {
    /// An empty unit, written on the first effective change only.
    pub fn fresh(document: DocumentId, owner: ProfileId) -> Result<Self> {
        let doc = LoroDoc::new();
        let meta = doc.get_map("meta");
        meta.insert("format", FORMAT).map_err(loro_failed)?;
        meta.insert("schema_version", DOCUMENT_PRIVATE_CRDT_SCHEMA_VERSION)
            .map_err(loro_failed)?;
        meta.insert("document_id", document.to_storage())
            .map_err(loro_failed)?;
        meta.insert("owner_profile_id", owner.to_storage())
            .map_err(loro_failed)?;
        doc.commit();
        Ok(Self { doc })
    }

    /// Decode a stored snapshot and check it names this exact document and
    /// reader in a format this build can write.
    pub fn load(bytes: &[u8], document: DocumentId, owner: ProfileId) -> Result<Self> {
        let doc = LoroDoc::new();
        let status = doc
            .import(bytes)
            .map_err(|_| corrupt("invalid private snapshot"))?;
        if status.pending.is_some() {
            return Err(corrupt("private snapshot has missing causal ancestors"));
        }
        let meta = doc.get_map("meta");
        if string(&meta, "format").as_deref() != Some(FORMAT) {
            return Err(corrupt("not a document-private snapshot"));
        }
        if string(&meta, "document_id") != Some(document.to_storage())
            || string(&meta, "owner_profile_id") != Some(owner.to_storage())
        {
            return Err(Error::new(
                Code::ProfileUnitScopeMismatch,
                "private snapshot names another document or reader",
            ));
        }
        let found = integer(&meta, "schema_version");
        if !Compatibility::against(found, DOCUMENT_PRIVATE_CRDT_SCHEMA_VERSION).writable() {
            return Err(Error::new(
                Code::SchemaTooNew,
                "private state format is newer than this build",
            ));
        }
        Ok(Self { doc })
    }

    /// Check a batch without applying any of it: bounded, every node id a
    /// uuid, every key one this build understands.
    pub fn validate(changes: &[PrivateChange]) -> Result<()> {
        if changes.is_empty() || changes.len() > MAX_BATCH {
            return Err(Error::new(
                Code::InvalidRequest,
                "a private-state batch holds between 1 and 512 changes",
            ));
        }
        for change in changes {
            if uuid::Uuid::parse_str(&change.node_id).is_err() {
                return Err(Error::new(Code::InvalidRequest, "node_id is not a uuid"));
            }
            if change.key != COLLAPSED {
                return Err(Error::new(Code::InvalidRequest, "unknown private key"));
            }
        }
        Ok(())
    }

    /// Apply a validated batch. Returns whether anything changed — a batch
    /// that only repeats stored values changes nothing and is not saved.
    pub fn apply(&mut self, changes: &[PrivateChange]) -> Result<bool> {
        Self::validate(changes)?;
        let nodes = self.doc.get_map("nodes");
        let mut changed = false;
        for change in changes {
            let current = node_map(&nodes, &change.node_id)
                .and_then(|attributes| boolean(&attributes, &change.key));
            match change.value {
                Some(value) if current != Some(value) => {
                    // Mergeable, so two lineages that each created this
                    // node's map converge on one map rather than one winning.
                    let attributes = nodes
                        .ensure_mergeable_map(&change.node_id)
                        .map_err(loro_failed)?;
                    attributes.insert(&change.key, value).map_err(loro_failed)?;
                    changed = true;
                }
                None if current.is_some() => {
                    if let Some(attributes) = node_map(&nodes, &change.node_id) {
                        attributes.delete(&change.key).map_err(loro_failed)?;
                        if attributes.is_empty() {
                            nodes.delete(&change.node_id).map_err(loro_failed)?;
                        }
                    }
                    changed = true;
                }
                _ => {}
            }
        }
        if changed {
            self.doc.commit();
        }
        Ok(changed)
    }

    /// The reader's view of it.
    pub fn state(&self) -> PrivateState {
        let mut out = BTreeMap::new();
        let nodes = self.doc.get_map("nodes");
        nodes.for_each(|node, value| {
            let ValueOrContainer::Container(loro::Container::Map(attributes)) = value else {
                return;
            };
            let mut values = BTreeMap::new();
            attributes.for_each(|key, value| {
                if let ValueOrContainer::Value(LoroValue::Bool(value)) = value {
                    values.insert(key.to_owned(), value);
                }
            });
            if !values.is_empty() {
                out.insert(node.to_owned(), values);
            }
        });
        PrivateState {
            revision: self.revision(),
            nodes: out,
        }
    }

    /// Grows with every committed change: the count of operations this
    /// unit's own lineage holds.
    pub fn revision(&self) -> u64 {
        self.doc
            .oplog_vv()
            .iter()
            .map(|(_, counter)| *counter as u64)
            .sum()
    }

    pub fn snapshot(&self) -> Result<Vec<u8>> {
        self.doc
            .export(ExportMode::snapshot())
            .map_err(|_| Error::new(Code::StorageFailed, "private snapshot export failed"))
    }
}

fn node_map(nodes: &LoroMap, node: &str) -> Option<LoroMap> {
    match nodes.get(node)? {
        ValueOrContainer::Container(loro::Container::Map(map)) => Some(map),
        _ => None,
    }
}
fn boolean(map: &LoroMap, key: &str) -> Option<bool> {
    match map.get(key)? {
        ValueOrContainer::Value(LoroValue::Bool(v)) => Some(v),
        _ => None,
    }
}
fn string(map: &LoroMap, key: &str) -> Option<String> {
    match map.get(key)? {
        ValueOrContainer::Value(LoroValue::String(v)) => Some(v.to_string()),
        _ => None,
    }
}
fn integer(map: &LoroMap, key: &str) -> Option<i64> {
    match map.get(key)? {
        ValueOrContainer::Value(LoroValue::I64(v)) => Some(v),
        _ => None,
    }
}
fn loro_failed(_: loro::LoroError) -> Error {
    Error::new(Code::StorageFailed, "private CRDT operation failed")
}
fn corrupt(message: &'static str) -> Error {
    Error::new(Code::CorruptState, message)
}

#[cfg(test)]
mod tests;
