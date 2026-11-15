// SPDX-License-Identifier: MPL-2.0

//! Lossless file-reference inspection, separate from the forgiving UI reader.
//! No node-id index or projected `Node` can establish completeness: those
//! intentionally omit malformed fields and collapse duplicate identities.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrictReferenceError {
    Schema,
    Shape,
    Identity,
    DuplicateIdentity,
    Unsupported,
    Hash,
    Budget,
    Scope,
}
impl std::fmt::Display for StrictReferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "file references are unknown: {self:?}")
    }
}
impl std::error::Error for StrictReferenceError {}

/// Inspection limits, not a claim about Loro replay or expanded-memory cost.
#[derive(Clone, Copy, Debug)]
pub struct ReferenceBudget {
    pub nodes: usize,
    pub fields: usize,
    pub depth: usize,
    pub value_bytes: usize,
}
impl Default for ReferenceBudget {
    fn default() -> Self {
        Self {
            nodes: 100_000,
            fields: 2_000_000,
            depth: 256,
            value_bytes: 64 * 1024 * 1024,
        }
    }
}
impl ReferenceBudget {
    pub fn field(&mut self) -> std::result::Result<(), StrictReferenceError> {
        self.fields = self
            .fields
            .checked_sub(1)
            .ok_or(StrictReferenceError::Budget)?;
        Ok(())
    }
    pub fn bytes(&mut self, size: usize) -> std::result::Result<(), StrictReferenceError> {
        self.value_bytes = self
            .value_bytes
            .checked_sub(size)
            .ok_or(StrictReferenceError::Budget)?;
        Ok(())
    }
}
type Inspection<T> = std::result::Result<T, StrictReferenceError>;

/// Every raw blob slot from a complete, supported document. Hash parsing is
/// owned by the canonical Store reference walker, after this shape proof.
pub struct InspectedFileReferences {
    schema: u32,
    blobs: Vec<String>,
    node_ids: Vec<NodeId>,
}
impl InspectedFileReferences {
    /// Complete validated live IDs across every supported raw tree, including
    /// Trash. A projected reader must not silently omit any of these nodes.
    pub fn node_ids(&self) -> &[NodeId] {
        &self.node_ids
    }
    pub fn schema_version(&self) -> u32 {
        self.schema
    }
    pub fn blobs(&self) -> &[String] {
        &self.blobs
    }
}

pub fn inspect_file_references(
    doc: &LoroDoc,
    mut budget: ReferenceBudget,
) -> Inspection<InspectedFileReferences> {
    let LoroValue::Map(roots) = doc.get_value() else {
        return Err(StrictReferenceError::Shape);
    };
    let mut trees = Vec::new();
    for (key, value) in roots.iter() {
        budget.field()?;
        let LoroValue::Container(id) = value else {
            return Err(StrictReferenceError::Shape);
        };
        let container = doc
            .get_container(id.clone())
            .ok_or(StrictReferenceError::Shape)?;
        match key.as_str() {
            C_META => {
                container
                    .into_map()
                    .map_err(|_| StrictReferenceError::Shape)?;
            }
            C_NODES | C_LIBRARY | C_OUTLINE => trees.push(
                container
                    .into_tree()
                    .map_err(|_| StrictReferenceError::Shape)?,
            ),
            _ => return Err(StrictReferenceError::Unsupported),
        }
    }
    let meta = doc.get_map(C_META);
    let schema = match meta.get(K_SCHEMA) {
        None => LEGACY_SCHEMA_VERSION,
        Some(value) => as_i64(value).ok_or(StrictReferenceError::Schema)?,
    };
    if !(LEGACY_SCHEMA_VERSION..=SCHEMA_VERSION).contains(&schema) {
        return Err(StrictReferenceError::Schema);
    }
    for key in meta.keys() {
        budget.field()?;
        if ![K_ID, K_NAME, K_SCHEMA, K_DOCUMENT].contains(&key.as_str()) {
            return Err(StrictReferenceError::Unsupported);
        }
        if key.as_str() != K_SCHEMA {
            let text = string(&meta, &key, &mut budget)?;
            if matches!(key.as_str(), K_ID | K_DOCUMENT) && Uuid::parse_str(&text).is_err() {
                return Err(StrictReferenceError::Identity);
            }
        }
    }
    let mut ids = HashSet::new();
    let mut blobs = Vec::new();
    for tree in trees {
        let mut pending = vec![(TreeParentId::Root, 0usize)];
        let mut visited = HashSet::new();
        while let Some((parent, depth)) = pending.pop() {
            // Loro's child index has no entry for a valid leaf (or empty root).
            // Its None is not evidence of a missing node; check identity first.
            if let TreeParentId::Node(id) = parent {
                if !tree.contains(id) {
                    return Err(StrictReferenceError::Shape);
                }
            }
            let count = tree.children_num(parent).unwrap_or(0);
            if count > budget.nodes || (count > 0 && depth > budget.depth) {
                return Err(StrictReferenceError::Budget);
            }
            let children = tree.children(parent).unwrap_or_default();
            if children.len() != count {
                return Err(StrictReferenceError::Shape);
            }
            for tid in children {
                budget.nodes = budget
                    .nodes
                    .checked_sub(1)
                    .ok_or(StrictReferenceError::Budget)?;
                if !visited.insert(tid) {
                    return Err(StrictReferenceError::Shape);
                }
                let node = tree
                    .get_meta(tid)
                    .map_err(|_| StrictReferenceError::Shape)?;
                inspect_node(&node, &mut budget, &mut ids, &mut blobs)?;
                pending.push((TreeParentId::Node(tid), depth + 1));
            }
        }
    }
    let mut node_ids = ids.into_iter().collect::<Vec<_>>();
    node_ids.sort_unstable();
    Ok(InspectedFileReferences {
        schema: schema as u32,
        blobs,
        node_ids,
    })
}

fn string(map: &LoroMap, key: &str, budget: &mut ReferenceBudget) -> Inspection<String> {
    let value = map.get(key).ok_or(StrictReferenceError::Shape)?;
    let Some(LoroValue::String(text)) = value.as_value() else {
        return Err(StrictReferenceError::Shape);
    };
    budget.bytes(text.len())?;
    Ok(text.to_string())
}

fn inspect_node(
    meta: &LoroMap,
    budget: &mut ReferenceBudget,
    ids: &mut HashSet<NodeId>,
    blobs: &mut Vec<String>,
) -> Inspection<()> {
    let id = NodeId::parse(&string(meta, K_ID, budget)?).ok_or(StrictReferenceError::Identity)?;
    if !ids.insert(id) {
        return Err(StrictReferenceError::DuplicateIdentity);
    }
    let kind =
        Kind::parse(&string(meta, K_KIND, budget)?).ok_or(StrictReferenceError::Unsupported)?;
    for key in meta.keys() {
        budget.field()?;
        if ![
            K_ID,
            K_DOC_ID,
            K_KIND,
            K_TEXT,
            K_ICON,
            K_NAME,
            K_LEVEL,
            K_COLOUR,
            K_DONE,
            K_BLOB,
            K_MIME,
            K_WIDTH,
            K_HEIGHT,
            K_EMBED,
            K_ATTRS,
            K_ALIGN,
            K_HEADING,
            K_LIST_STYLE,
            K_IMG_W,
            K_IMG_H,
            K_LANG,
            K_PROSE_LANGUAGE,
            K_TABLE_LAYOUT,
            K_TRASHED_AT,
            K_ORIGIN,
            K_TRASHED_BY,
            K_RESTORED_BY,
            K_AT,
            K_EXTENT,
        ]
        .contains(&key.as_str())
        {
            return Err(StrictReferenceError::Unsupported);
        }
        match key.as_str() {
            K_ATTRS | K_TEXT | K_BLOB | K_EMBED | K_ID | K_KIND => {}
            K_DONE => {
                as_bool(meta.get(&key).ok_or(StrictReferenceError::Shape)?)
                    .ok_or(StrictReferenceError::Shape)?;
            }
            K_WIDTH | K_HEIGHT | K_IMG_W | K_IMG_H | K_HEADING | K_TRASHED_AT => {
                as_i64(meta.get(&key).ok_or(StrictReferenceError::Shape)?)
                    .ok_or(StrictReferenceError::Shape)?;
            }
            K_AT | K_EXTENT => {
                let Some(ValueOrContainer::Value(LoroValue::Map(value))) = meta.get(&key) else {
                    return Err(StrictReferenceError::Shape);
                };
                for (field, value) in value.iter() {
                    budget.field()?;
                    let allowed = if key.as_str() == K_AT {
                        [F_X, F_Y]
                    } else {
                        [F_W, F_H]
                    };
                    if !allowed.contains(&field.as_str()) || !matches!(value, LoroValue::I64(_)) {
                        return Err(StrictReferenceError::Unsupported);
                    }
                }
                let value = LoroValue::Map(value);
                if (key.as_str() == K_AT && at_from_loro(&value).is_none())
                    || (key.as_str() == K_EXTENT && extent_from_loro(&value).is_none())
                {
                    return Err(StrictReferenceError::Shape);
                }
            }
            _ => {
                let value = string(meta, &key, budget)?;
                if matches!(key.as_str(), K_DOC_ID | K_ORIGIN) && Uuid::parse_str(&value).is_err() {
                    return Err(StrictReferenceError::Identity);
                }
            }
        }
    }
    if meta.get(K_EMBED).is_some() {
        NodeId::parse(&string(meta, K_EMBED, budget)?).ok_or(StrictReferenceError::Identity)?;
    }
    // Concurrent or legacy content may retain both slots. Inspect BOTH: the
    // rendering precedence of Embed must never hide a retained blob reference.
    if meta.get(K_BLOB).is_some() {
        let hash = string(meta, K_BLOB, budget)?;
        if hash.is_empty() {
            return Err(StrictReferenceError::Hash);
        }
        blobs.push(hash);
    } else if kind.shape() == crate::node::Shape::Blob && meta.get(K_EMBED).is_none() {
        return Err(StrictReferenceError::Shape);
    }
    if let Some(text) = meta.get(K_TEXT) {
        let ValueOrContainer::Container(text) = text else {
            return Err(StrictReferenceError::Shape);
        };
        budget.bytes(
            text.into_text()
                .map_err(|_| StrictReferenceError::Shape)?
                .len_utf8(),
        )?;
    }
    if let Some(attrs) = meta.get(K_ATTRS) {
        let ValueOrContainer::Container(attrs) = attrs else {
            return Err(StrictReferenceError::Shape);
        };
        let attrs = attrs.into_map().map_err(|_| StrictReferenceError::Shape)?;
        let mut attr_ids = HashSet::new();
        for key in attrs.keys() {
            budget.field()?;
            let attr_id = AttrId::parse(&key).ok_or(StrictReferenceError::Identity)?;
            if !attr_ids.insert(attr_id) {
                return Err(StrictReferenceError::DuplicateIdentity);
            }
            let Some(ValueOrContainer::Container(record)) = attrs.get(&key) else {
                return Err(StrictReferenceError::Shape);
            };
            let record = record.into_map().map_err(|_| StrictReferenceError::Shape)?;
            for field in record.keys() {
                budget.field()?;
                if ![
                    K_ATTR_ROLE,
                    K_ATTR_TYPE,
                    K_ATTR_VALUE,
                    K_ATTR_HOME,
                    K_ATTR_TARGET,
                    K_ATTR_LABEL,
                    K_ATTR_TZ,
                ]
                .contains(&field.as_str())
                {
                    return Err(StrictReferenceError::Unsupported);
                }
                match record.get(&field) {
                    Some(ValueOrContainer::Value(LoroValue::String(s))) => budget.bytes(s.len())?,
                    Some(ValueOrContainer::Value(
                        LoroValue::I64(_) | LoroValue::Double(_) | LoroValue::Bool(_),
                    )) => {}
                    _ => return Err(StrictReferenceError::Shape),
                }
            }
            let role = string(&record, K_ATTR_ROLE, budget)?;
            read_attr_value(&record, &role).ok_or(StrictReferenceError::Unsupported)?;
            for key in [K_ATTR_LABEL, K_ATTR_TZ, K_ATTR_TARGET, K_ATTR_HOME] {
                if record.get(key).is_none() {
                    continue;
                }
                let value = string(&record, key, budget)?;
                if key == K_ATTR_TARGET && NodeId::parse(&value).is_none() {
                    return Err(StrictReferenceError::Identity);
                }
                if key == K_ATTR_HOME {
                    let (kind, ids) = value
                        .split_once(':')
                        .ok_or(StrictReferenceError::Unsupported)?;
                    let valid = match kind {
                        "profile" | "workspace" | "document" => Uuid::parse_str(ids).is_ok(),
                        "document-at" => {
                            ids.split('/').count() == 3
                                && ids.split('/').all(|id| Uuid::parse_str(id).is_ok())
                        }
                        _ => false,
                    };
                    if !valid {
                        return Err(StrictReferenceError::Unsupported);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
