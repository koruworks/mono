// SPDX-License-Identifier: MPL-2.0

//! Selected listing facts from a complete strict manifest traversal. Absence
//! means unfiled; it never means document contents were purged.
use super::*;
use loro::{Container, LoroMap, LoroValue, TreeParentId, ValueOrContainer};
use std::collections::{BTreeMap, BTreeSet};

pub const MANIFEST_CLASSIFIER_VERSION: &str = "manifest-membership-v1";
pub const MAX_MANIFEST_SELECTION: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestMembership {
    Listed,
    Trash,
    Unfiled,
}

pub struct ManifestSelection {
    document_id: CloudId,
    membership: ManifestMembership,
    display_name: Option<String>,
}
impl ManifestSelection {
    pub fn document_id(&self) -> CloudId {
        self.document_id
    }
    pub fn membership(&self) -> ManifestMembership {
        self.membership
    }
    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }
}

/// Construction requires complete raw traversal, including all live ancestors.
/// No partial classification escapes on unsupported or malformed input.
pub struct ManifestClassification {
    schema: u32,
    proof: ContentProof,
    selected: Vec<ManifestSelection>,
}
impl std::fmt::Debug for ManifestClassification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManifestClassification")
            .field("schema", &self.schema)
            .field("selected_count", &self.selected.len())
            .finish_non_exhaustive()
    }
}
impl ManifestClassification {
    pub fn schema_version(&self) -> u32 {
        self.schema
    }
    pub fn proof(&self) -> ContentProof {
        self.proof
    }
    pub fn selected(&self) -> &[ManifestSelection] {
        &self.selected
    }
    pub fn classifier_version(&self) -> &'static str {
        MANIFEST_CLASSIFIER_VERSION
    }
}

impl RestoredContent {
    pub fn strict_manifest_classification(
        &self,
        selected: &[CloudId],
        mut budget: ReferenceBudget,
    ) -> Result<ManifestClassification, StrictReferenceError> {
        if self.proof.unit != Unit::WorkspaceManifest {
            return Err(StrictReferenceError::Scope);
        }
        if selected.len() > MAX_MANIFEST_SELECTION {
            return Err(StrictReferenceError::Budget);
        }
        if selected
            .windows(2)
            .any(|pair| pair[0].as_bytes() >= pair[1].as_bytes())
        {
            return Err(StrictReferenceError::Identity);
        }
        let node_limit = budget.nodes;
        // Bounds strings, maps and deletion events before building any index.
        references::structural_budget(&self.doc, &mut budget)?;
        let schema = references::manifest(&self.doc, &mut budget)?;
        let meta = self.doc.get_map("meta");
        let expected_workspace = identity::parts(self.proof.dataset).1;
        if uuid_value(&meta, "workspace_id")? != expected_workspace {
            return Err(StrictReferenceError::Scope);
        }
        if schema == 1 && meta.get("workspace_deletions").is_some() {
            return Err(StrictReferenceError::Schema);
        }
        let tree = self.doc.get_tree("documents");
        let all = tree.nodes();
        if all.len() > node_limit {
            return Err(StrictReferenceError::Budget);
        }
        let mut live = BTreeSet::new();
        for id in all {
            if !tree
                .is_node_deleted(&id)
                .map_err(|_| StrictReferenceError::Shape)?
            {
                live.insert(id);
            }
        }
        let mut visited = BTreeSet::new();
        let mut pending = vec![(TreeParentId::Root, false, 0usize)];
        let mut found = BTreeMap::new();
        while let Some((parent, inherited_trash, depth)) = pending.pop() {
            let children = tree.children(parent).unwrap_or_default();
            if !children.is_empty() && depth > budget.depth {
                return Err(StrictReferenceError::Budget);
            }
            for id in children {
                if !live.contains(&id) || !visited.insert(id) || tree.parent(id) != Some(parent) {
                    return Err(StrictReferenceError::Shape);
                }
                let entry = tree.get_meta(id).map_err(|_| StrictReferenceError::Shape)?;
                let document = uuid_value(&entry, "doc_id")?;
                let own_trash = trashed(&entry, schema)?;
                let trash = inherited_trash || own_trash;
                if selected
                    .binary_search_by_key(document.as_bytes(), |id| *id.as_bytes())
                    .is_ok()
                {
                    let Some(ValueOrContainer::Value(LoroValue::String(title))) =
                        entry.get("title")
                    else {
                        return Err(StrictReferenceError::Shape);
                    };
                    let display_name = (title.len() <= 512 && title.chars().count() <= 128)
                        .then(|| title.to_string());
                    found.insert(
                        *document.as_bytes(),
                        ManifestSelection {
                            document_id: document,
                            membership: if trash {
                                ManifestMembership::Trash
                            } else {
                                ManifestMembership::Listed
                            },
                            display_name,
                        },
                    );
                }
                pending.push((TreeParentId::Node(id), trash, depth + 1));
            }
        }
        // A detached node or missing ancestor cannot become evidence of absence.
        if live != visited {
            return Err(StrictReferenceError::Shape);
        }
        Ok(ManifestClassification {
            schema,
            proof: self.proof,
            selected: selected
                .iter()
                .map(|id| {
                    found.remove(id.as_bytes()).unwrap_or(ManifestSelection {
                        document_id: *id,
                        membership: ManifestMembership::Unfiled,
                        display_name: None,
                    })
                })
                .collect(),
        })
    }
}

fn uuid_value(map: &LoroMap, key: &str) -> Result<CloudId, StrictReferenceError> {
    let Some(ValueOrContainer::Value(LoroValue::String(value))) = map.get(key) else {
        return Err(StrictReferenceError::Identity);
    };
    Ok(CloudId::from_bytes(
        *uuid::Uuid::parse_str(&value)
            .map_err(|_| StrictReferenceError::Identity)?
            .as_bytes(),
    ))
}

fn trashed(map: &LoroMap, schema: u32) -> Result<bool, StrictReferenceError> {
    match map.get("deletions") {
        Some(ValueOrContainer::Container(Container::Map(events))) if schema == 2 => {
            Ok(!events.is_empty())
        }
        Some(_) if schema == 1 => Err(StrictReferenceError::Schema),
        Some(_) => Err(StrictReferenceError::Shape),
        // Schema 2 preserves old nodes' legacy stamps until that node first
        // receives a deletion map. An existing map always overrides its hint.
        None => Ok(map.get("trashed_at").is_some()),
    }
}
