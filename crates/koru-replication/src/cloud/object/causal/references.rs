// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_blob_sync::referenced::{WalkDocument, WalkNode, referenced_blobs};
use koru_core::schema::file_references::inspect_file_references;
use loro::{Container, LoroValue, TreeParentId, ValueOrContainer};
use std::collections::BTreeSet;

/// A complete typed inspection of the same content held by `RestoredContent`.
/// Construction is private; a caller's hash list cannot become this proof.
pub struct StrictBlobReferences {
    hashes: BTreeSet<String>,
    schema: u32,
    proof: ContentProof,
}
impl std::fmt::Debug for StrictBlobReferences {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Reference addresses and scoped proof identifiers are content, not
        // operational diagnostics. Callers must deliberately access them.
        f.debug_struct("StrictBlobReferences")
            .field("schema_version", &self.schema)
            .field("reference_count", &self.hashes.len())
            .finish_non_exhaustive()
    }
}
impl StrictBlobReferences {
    pub fn hashes(&self) -> &BTreeSet<String> {
        &self.hashes
    }
    pub fn schema_version(&self) -> u32 {
        self.schema
    }
    pub fn algorithm(&self) -> &'static str {
        "files-plain-v0"
    }
    pub fn proof(&self) -> ContentProof {
        self.proof
    }
}

impl RestoredContent {
    pub fn strict_blob_references(&self) -> Result<StrictBlobReferences, StrictReferenceError> {
        self.strict_blob_references_with_budget(ReferenceBudget::default())
    }

    /// Budget exhaustion means unknown, never an empty or partial set.
    pub fn strict_blob_references_with_budget(
        &self,
        mut budget: ReferenceBudget,
    ) -> Result<StrictBlobReferences, StrictReferenceError> {
        let (schema, hashes) = match self.proof.unit {
            Unit::WorkspaceDocument { document_id } => {
                let inspected = inspect_file_references(&self.doc, budget)?;
                // `meta.id` is not checked against the storing Workspace. It is
                // the Workspace the document was created in, stamped once and
                // never moved with the document, so a document moved between
                // Workspaces keeps its old one; the storage binding of this
                // unit already proves where it is held. `meta.document` names
                // the document itself and must still match.
                let meta = self.doc.get_map("meta");
                if let Some(value) = meta.get("document") {
                    let Some(LoroValue::String(value)) = value.as_value() else {
                        return Err(StrictReferenceError::Scope);
                    };
                    if value.as_str() != identity::uuid_text(document_id) {
                        return Err(StrictReferenceError::Scope);
                    }
                }
                // Feed the canonical walker only after every raw slot has been
                // inspected. Its UI-oriented blank skipping is not a proof.
                for hash in inspected.blobs() {
                    let Some(hex) = hash.strip_prefix("b3:") else {
                        return Err(StrictReferenceError::Hash);
                    };
                    if hex.len() != 64 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
                        return Err(StrictReferenceError::Hash);
                    }
                }
                let walk = referenced_blobs(&[WalkDocument {
                    durable: true,
                    trashed: false,
                    nodes: inspected
                        .blobs()
                        .iter()
                        .map(|hash| WalkNode {
                            blob: Some(hash.clone()),
                            mime: None,
                        })
                        .collect(),
                }]);
                if walk.unreadable != 0 {
                    return Err(StrictReferenceError::Hash);
                }
                (
                    inspected.schema_version(),
                    walk.blobs
                        .into_iter()
                        .map(|blob| blob.hash.as_str().to_owned())
                        .collect(),
                )
            }
            Unit::WorkspaceManifest => {
                structural_budget(&self.doc, &mut budget)?;
                (manifest(&self.doc, &mut budget)?, BTreeSet::new())
            }
            Unit::ProfileIdentity | Unit::ProfilePrivate | Unit::ProfilePersonal { .. } => {
                structural_budget(&self.doc, &mut budget)?;
                // These existing typed validators reject unknown roots/fields,
                // malformed values, newer schemas, and foreign unit scope.
                let snapshot = self.snapshot().map_err(|_| StrictReferenceError::Budget)?;
                let expected = self
                    .proof
                    .unit
                    .profile_unit(self.proof.dataset)
                    .map_err(|_| StrictReferenceError::Scope)?;
                let (unit, supported) = match self.proof.unit {
                    Unit::ProfileIdentity => {
                        let m = IdentityManifest::from_snapshot(&snapshot)
                            .map_err(|_| StrictReferenceError::Shape)?;
                        (m.unit(), m.compatibility().writable())
                    }
                    Unit::ProfilePrivate => {
                        let m = PrivateManifest::from_snapshot(&snapshot)
                            .map_err(|_| StrictReferenceError::Shape)?;
                        (m.unit(), m.compatibility().writable())
                    }
                    Unit::ProfilePersonal { .. } => {
                        let m = PersonalManifest::from_snapshot(&snapshot)
                            .map_err(|_| StrictReferenceError::Shape)?;
                        (
                            m.unit().ok_or(StrictReferenceError::Scope)?,
                            m.compatibility().writable(),
                        )
                    }
                    _ => unreachable!(),
                };
                if unit != expected {
                    return Err(StrictReferenceError::Scope);
                }
                if !supported {
                    return Err(StrictReferenceError::Schema);
                }
                (schema(&self.doc, 1, false)?, BTreeSet::new())
            }
        };
        Ok(StrictBlobReferences {
            hashes,
            schema,
            proof: self.proof,
        })
    }
}

pub(super) fn schema(
    doc: &LoroDoc,
    maximum: i64,
    legacy: bool,
) -> Result<u32, StrictReferenceError> {
    let version = match doc.get_map("meta").get("schema_version") {
        Some(ValueOrContainer::Value(LoroValue::I64(v))) if (1..=maximum).contains(&v) => v,
        None if legacy => 1,
        _ => return Err(StrictReferenceError::Schema),
    };
    Ok(version as u32)
}

/// Bound shallow traversal before the existing typed Profile validator builds
/// a deep value. This is not a bound on decoding or Loro's resident memory.
pub(super) fn structural_budget(
    doc: &LoroDoc,
    budget: &mut ReferenceBudget,
) -> Result<(), StrictReferenceError> {
    let mut pending = vec![(ValueOrContainer::Value(doc.get_value()), 0usize)];
    while let Some((value, depth)) = pending.pop() {
        budget.field()?;
        if depth > budget.depth {
            return Err(StrictReferenceError::Budget);
        }
        match value {
            ValueOrContainer::Value(LoroValue::Container(id)) => pending.push((
                ValueOrContainer::Container(
                    doc.get_container(id).ok_or(StrictReferenceError::Shape)?,
                ),
                depth + 1,
            )),
            ValueOrContainer::Container(Container::Map(map)) => {
                if map.len() > budget.fields {
                    return Err(StrictReferenceError::Budget);
                }
                for key in map.keys() {
                    budget.bytes(key.len())?;
                    pending.push((map.get(&key).ok_or(StrictReferenceError::Shape)?, depth + 1));
                }
            }
            ValueOrContainer::Container(Container::Tree(tree)) => {
                let mut parents = vec![(TreeParentId::Root, depth)];
                while let Some((parent, depth)) = parents.pop() {
                    if let TreeParentId::Node(id) = parent {
                        if !tree.contains(id) {
                            return Err(StrictReferenceError::Shape);
                        }
                    }
                    let count = tree.children_num(parent).unwrap_or(0);
                    if count > budget.nodes || (count > 0 && depth > budget.depth) {
                        return Err(StrictReferenceError::Budget);
                    }
                    for id in tree.children(parent).unwrap_or_default() {
                        budget.nodes = budget
                            .nodes
                            .checked_sub(1)
                            .ok_or(StrictReferenceError::Budget)?;
                        pending.push((
                            ValueOrContainer::Container(Container::Map(
                                tree.get_meta(id).map_err(|_| StrictReferenceError::Shape)?,
                            )),
                            depth + 1,
                        ));
                        parents.push((TreeParentId::Node(id), depth + 1));
                    }
                }
            }
            ValueOrContainer::Container(_) => return Err(StrictReferenceError::Unsupported),
            ValueOrContainer::Value(LoroValue::Map(map)) => {
                if map.len() > budget.fields {
                    return Err(StrictReferenceError::Budget);
                }
                for (key, value) in map.iter() {
                    budget.bytes(key.len())?;
                    pending.push((ValueOrContainer::Value(value.clone()), depth + 1));
                }
            }
            ValueOrContainer::Value(LoroValue::List(list)) => {
                if list.len() > budget.fields {
                    return Err(StrictReferenceError::Budget);
                }
                for value in list.iter() {
                    pending.push((ValueOrContainer::Value(value.clone()), depth + 1));
                }
            }
            ValueOrContainer::Value(LoroValue::String(text)) => budget.bytes(text.len())?,
            ValueOrContainer::Value(LoroValue::Binary(bytes)) => budget.bytes(bytes.len())?,
            ValueOrContainer::Value(_) => {}
        }
    }
    Ok(())
}

pub(super) fn manifest(
    doc: &LoroDoc,
    budget: &mut ReferenceBudget,
) -> Result<u32, StrictReferenceError> {
    let version = schema(doc, koru_store_core::version::CRDT_SCHEMA_VERSION, true)?;
    let LoroValue::Map(roots) = doc.get_value() else {
        return Err(StrictReferenceError::Shape);
    };
    for (key, value) in roots.iter() {
        let LoroValue::Container(id) = value else {
            return Err(StrictReferenceError::Shape);
        };
        let container = doc
            .get_container(id.clone())
            .ok_or(StrictReferenceError::Shape)?;
        match key.as_str() {
            "meta" => {
                container
                    .into_map()
                    .map_err(|_| StrictReferenceError::Shape)?;
            }
            "documents" => {
                container
                    .into_tree()
                    .map_err(|_| StrictReferenceError::Shape)?;
            }
            _ => return Err(StrictReferenceError::Unsupported),
        }
    }
    check_map(
        &doc.get_map("meta"),
        &[
            "workspace_id",
            "name",
            "created_at",
            "schema_version",
            "icon",
            "color",
            "pinned",
            "archived",
            "workspace_deleted_at",
            "workspace_deletions",
        ],
        budget,
    )?;
    let tree = doc.get_tree("documents");
    let mut parents = vec![TreeParentId::Root];
    let mut ids = BTreeSet::new();
    while let Some(parent) = parents.pop() {
        for id in tree.children(parent).unwrap_or_default() {
            let meta = tree.get_meta(id).map_err(|_| StrictReferenceError::Shape)?;
            check_map(
                &meta,
                &[
                    "doc_id",
                    "title",
                    "kind",
                    "mime",
                    "note_doc",
                    "trashed_at",
                    "trashed_by",
                    "deletions",
                ],
                budget,
            )?;
            let Some(ValueOrContainer::Value(LoroValue::String(id_text))) = meta.get("doc_id")
            else {
                return Err(StrictReferenceError::Identity);
            };
            let id_value =
                uuid::Uuid::parse_str(&id_text).map_err(|_| StrictReferenceError::Identity)?;
            if !ids.insert(id_value) {
                return Err(StrictReferenceError::DuplicateIdentity);
            }
            for key in ["title", "kind"] {
                // DocumentEntry.kind is an open listing label (including
                // "page"), not koru_core::node::Kind. The closed structural
                // fields establish that this unit cannot carry blob slots.
                if !matches!(
                    meta.get(key),
                    Some(ValueOrContainer::Value(LoroValue::String(_)))
                ) {
                    return Err(StrictReferenceError::Shape);
                }
            }
            parents.push(TreeParentId::Node(id));
        }
    }
    Ok(version)
}

fn check_map(
    map: &loro::LoroMap,
    allowed: &[&str],
    budget: &mut ReferenceBudget,
) -> Result<(), StrictReferenceError> {
    for key in map.keys() {
        budget.field()?;
        if !allowed.contains(&key.as_str()) {
            return Err(StrictReferenceError::Unsupported);
        }
        match (key.as_str(), map.get(&key)) {
            (
                "deletions" | "workspace_deletions",
                Some(ValueOrContainer::Container(Container::Map(deletions))),
            ) => {
                for id in deletions.keys() {
                    budget.field()?;
                    uuid::Uuid::parse_str(&id).map_err(|_| StrictReferenceError::Identity)?;
                    let Some(ValueOrContainer::Container(Container::Map(record))) =
                        deletions.get(&id)
                    else {
                        return Err(StrictReferenceError::Shape);
                    };
                    check_map(&record, &["at", "by"], budget)?;
                    if !matches!(
                        record.get("at"),
                        Some(ValueOrContainer::Value(LoroValue::I64(_)))
                    ) {
                        return Err(StrictReferenceError::Shape);
                    }
                }
            }
            (
                "schema_version" | "created_at" | "workspace_deleted_at" | "trashed_at" | "at",
                Some(ValueOrContainer::Value(LoroValue::I64(_))),
            ) => {}
            ("pinned" | "archived", Some(ValueOrContainer::Value(LoroValue::Bool(_)))) => {}
            (
                "deletions"
                | "workspace_deletions"
                | "schema_version"
                | "created_at"
                | "workspace_deleted_at"
                | "trashed_at"
                | "at"
                | "pinned"
                | "archived",
                _,
            ) => return Err(StrictReferenceError::Shape),
            (
                "workspace_id" | "doc_id" | "note_doc",
                Some(ValueOrContainer::Value(LoroValue::String(id))),
            ) => {
                uuid::Uuid::parse_str(&id).map_err(|_| StrictReferenceError::Identity)?;
            }
            ("icon" | "color", Some(ValueOrContainer::Value(LoroValue::Null))) => {}
            (_, Some(ValueOrContainer::Value(LoroValue::String(_)))) => {}
            _ => return Err(StrictReferenceError::Shape),
        }
    }
    Ok(())
}
