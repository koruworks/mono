// SPDX-License-Identifier: MPL-2.0

//! Read-only evidence for bounded consumers of an untrusted document snapshot.
use super::*;
use loro::{Container, TextDelta};

mod content;
pub use content::{NodeContentRead, NodeContentReadError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentReadValidationError {
    NodeLimit { limit: usize },
    TextLimit { limit: usize },
    InvalidNodeIdentity,
    DuplicateNodeIdentity,
    InvalidTree,
    SeedMismatch,
}

type ReadResult<T> = std::result::Result<T, DocumentReadValidationError>;
use DocumentReadValidationError as E;

/// Raw roots include roots the ordinary display walker hides, such as Trash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentReadIdentity {
    pub live_nodes: usize,
    pub root_nodes: Vec<NodeId>,
}

/// Classification evidence, not a replacement for the document's content model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeReadEvidence {
    pub kind: Kind,
    pub heading: Option<Heading>,
    pub text: Option<String>,
    pub has_marks: bool,
    pub has_attribute_data: bool,
}

impl WorkspaceDoc {
    /// Check raw public identities before trusting the permissive reader index.
    ///
    /// Loro materializes the raw ID Vec before its length can be checked. This
    /// bounds subsequent metadata work, not import/reindex or heap allocation.
    pub fn validate_node_identities(&self, max_nodes: usize) -> ReadResult<DocumentReadIdentity> {
        let tree = self.nodes();
        let raw = tree.nodes();
        if raw.len() > max_nodes {
            return Err(E::NodeLimit { limit: max_nodes });
        }
        let mut ids = HashSet::new();
        let mut live = HashMap::new();
        for tid in raw {
            if tree.is_node_deleted(&tid).map_err(|_| E::InvalidTree)? {
                continue;
            }
            let meta = tree.get_meta(tid).map_err(|_| E::InvalidTree)?;
            let id = read_node_id(&meta).ok_or(E::InvalidNodeIdentity)?;
            if !ids.insert(id) {
                return Err(E::DuplicateNodeIdentity);
            }
            live.insert(tid, id);
        }
        let root_nodes = tree
            .children(TreeParentId::Root)
            .unwrap_or_default()
            .into_iter()
            .map(|tid| live.get(&tid).copied().ok_or(E::InvalidTree))
            .collect::<ReadResult<Vec<_>>>()?;
        Ok(DocumentReadIdentity {
            live_nodes: live.len(),
            root_nodes,
        })
    }

    /// Read classification and bounded text without creating missing containers.
    /// Unknown mark/attribute data remains visible as evidence, not discarded by
    /// the ordinary reader's forward-compatible attribute decoder.
    pub fn read_node_evidence(
        &self,
        id: NodeId,
        remaining_text_bytes: usize,
    ) -> ReadResult<NodeReadEvidence> {
        let tid = self.tree_id(id).map_err(|_| E::InvalidNodeIdentity)?;
        let meta = self.nodes().get_meta(tid).map_err(|_| E::InvalidTree)?;
        let kind = meta
            .get(K_KIND)
            .and_then(as_string)
            .and_then(|s| Kind::parse(&s))
            .ok_or(E::InvalidTree)?;
        let heading = match meta.get(K_HEADING) {
            None => None,
            Some(value) => Heading::from_level(as_i64(value).ok_or(E::InvalidTree)?),
        };
        let has_attribute_data = match meta.get(K_ATTRS) {
            None => false,
            Some(ValueOrContainer::Container(Container::Map(attrs))) => !attrs.is_empty(),
            Some(_) => return Err(E::InvalidTree),
        };
        let nontext = if let Some(value) = meta.get(K_EMBED) {
            let target = as_string(value).ok_or(E::InvalidTree)?;
            NodeId::parse(&target).ok_or(E::InvalidTree)?;
            true
        } else if let Some(value) = meta.get(K_BLOB) {
            as_string(value).ok_or(E::InvalidTree)?;
            true
        } else {
            false
        };
        let (text, has_marks) = if nontext {
            // Follow read_node's content precedence; stale secondary text is
            // not rendered and must not turn a real blob into malformed prose.
            (None, false)
        } else {
            let text = existing_text(&meta)?;
            if text.len_utf8() > remaining_text_bytes {
                return Err(E::TextLimit {
                    limit: remaining_text_bytes,
                });
            }
            let has_marks = has_marks(&text);
            (Some(text.to_string()), has_marks)
        };
        Ok(NodeReadEvidence {
            kind,
            heading,
            text,
            has_marks,
            has_attribute_data,
        })
    }

    /// Exact semantic proof of page_genesis followed only by a title edit.
    /// Neither regenerated update bytes nor replica/change metadata are identity.
    pub fn validate_page_seed_v1(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
        title: &str,
    ) -> ReadResult<()> {
        let checked = self.validate_node_identities(2)?;
        let root = genesis_root_id(document);
        let seed = genesis_seed_id(document);
        if checked.live_nodes != 2 || checked.root_nodes != [root] {
            return Err(E::SeedMismatch);
        }
        let shallow = self.doc.get_value();
        let roots = shallow.as_map().ok_or(E::SeedMismatch)?;
        for (name, value) in roots.iter() {
            if name.as_str() == C_META || name.as_str() == C_NODES {
                continue;
            }
            let LoroValue::Container(id) = value else {
                return Err(E::SeedMismatch);
            };
            if !empty_handle(self.doc.get_container(id.clone()).ok_or(E::SeedMismatch)?) {
                return Err(E::SeedMismatch);
            }
        }
        let meta = self.doc.get_map(C_META);
        if !exact_keys(&meta, &[K_ID, K_NAME, K_SCHEMA])
            || meta.get(K_ID).and_then(as_string).as_deref()
                != Some(workspace.to_storage().as_str())
            || meta.get(K_NAME).and_then(as_string).as_deref() != Some("")
            || meta.get(K_SCHEMA).and_then(as_i64) != Some(PAGE_GENESIS_SCHEMA_VERSION_V1)
        {
            return Err(E::SeedMismatch);
        }
        let tree = self.nodes();
        let root_tid = self.tree_id(root).map_err(|_| E::SeedMismatch)?;
        let seed_tid = self.tree_id(seed).map_err(|_| E::SeedMismatch)?;
        if tree
            .children(TreeParentId::Node(root_tid))
            .unwrap_or_default()
            != [seed_tid]
            || !tree
                .children(TreeParentId::Node(seed_tid))
                .unwrap_or_default()
                .is_empty()
        {
            return Err(E::SeedMismatch);
        }
        let root_meta = tree.get_meta(root_tid).map_err(|_| E::SeedMismatch)?;
        let seed_meta = tree.get_meta(seed_tid).map_err(|_| E::SeedMismatch)?;
        if !exact_keys(&root_meta, &[K_DOC_ID, K_ID, K_KIND, K_DONE, K_TEXT])
            || read_document_id(&root_meta) != Some(document)
            || !exact_keys(&seed_meta, &[K_ID, K_KIND, K_DONE, K_TEXT])
        {
            return Err(E::SeedMismatch);
        }
        for (meta, kind, expected) in [
            (&root_meta, Kind::Document, title),
            (&seed_meta, Kind::Paragraph, ""),
        ] {
            if meta.get(K_KIND).and_then(as_string).as_deref() != Some(kind.as_str())
                || meta.get(K_DONE).and_then(as_bool) != Some(false)
            {
                return Err(E::SeedMismatch);
            }
            let text = existing_text(meta).map_err(|_| E::SeedMismatch)?;
            if text.len_utf8() != expected.len() || has_marks(&text) || text.to_string() != expected
            {
                return Err(E::SeedMismatch);
            }
        }
        Ok(())
    }
}

fn existing_text(meta: &LoroMap) -> ReadResult<loro::LoroText> {
    match meta.get(K_TEXT) {
        Some(ValueOrContainer::Container(Container::Text(text))) => Ok(text),
        _ => Err(E::InvalidTree),
    }
}

fn has_marks(text: &loro::LoroText) -> bool {
    text.to_delta().iter().any(|delta| match delta {
        TextDelta::Insert { attributes, .. } => {
            attributes.as_ref().is_some_and(|attrs| !attrs.is_empty())
        }
        _ => true,
    })
}

fn exact_keys(map: &LoroMap, expected: &[&str]) -> bool {
    map.len() == expected.len() && map.keys().all(|key| expected.contains(&key.as_str()))
}

fn empty_handle(container: Container) -> bool {
    match container {
        Container::Map(map) => map.is_empty(),
        Container::Text(text) => text.len_utf8() == 0,
        Container::Tree(tree) => tree.nodes().is_empty(),
        Container::List(list) => list.is_empty(),
        Container::MovableList(list) => list.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
