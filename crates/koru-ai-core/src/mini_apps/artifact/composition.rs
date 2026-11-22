// SPDX-License-Identifier: MPL-2.0

//! Version 2 separates portable placement/document slots from installation IDs.
//! Version 1 retains its original struct and serialization byte-for-byte.
use super::{Envelope, MAX_BYTES, WorkspaceParameter};
use crate::mini_apps::composition::{
    self, Action, Decoration, Ink, Layout, NodeKind, Space, TextRole,
};
use crate::mini_apps::{self, Component, Declaration, DocumentReference};
use crate::{DocumentId, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Download/decode cap for a file that may contain v2. V1 still uses its exact
/// original 4096-byte codec. Portable node/default metadata expands live JSON.
pub const MAX_FILE_BYTES: usize = 16384;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SavedEnvelope {
    TaskDocument(Envelope),
    Composition(CompositionEnvelope),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionEnvelope {
    pub format_version: u32,
    pub definition: Definition,
    pub bindings: Bindings,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub catalog_id: String,
    pub a2ui_version: String,
    pub workspace: WorkspaceParameter,
    pub nodes: Vec<PortableNode>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bindings {
    pub workspace: WorkspaceId,
    pub documents: Vec<DocumentBinding>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentBinding {
    pub parameter: String,
    pub document: DocumentId,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableNode {
    pub id: String,
    pub layout: Layout,
    pub decoration: Decoration,
    pub kind: PortableKind,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "component", deny_unknown_fields)]
pub enum PortableKind {
    Row {
        children: Vec<String>,
    },
    Column {
        children: Vec<String>,
    },
    Box {
        child: String,
    },
    Text {
        text: String,
        role: TextRole,
        ink: Ink,
    },
    Spacer {
        space: Space,
    },
    Divider,
    Button {
        label: String,
        target: String,
        document: String,
    },
    DocumentHost {
        document: String,
    },
}
impl SavedEnvelope {
    pub fn capture(
        workspace: WorkspaceId,
        declaration: &Declaration,
    ) -> Result<Self, &'static str> {
        let checked = Declaration::validate(declaration.envelope.clone())?;
        let Some(tree) = checked.composition() else {
            return Envelope::capture(workspace, &checked).map(Self::TaskDocument);
        };
        let mut ordered_nodes: Vec<_> = tree.nodes().iter().collect();
        ordered_nodes.sort_by(|a, b| a.id.cmp(&b.id));
        // Slots follow stable node identities, never installation UUID order.
        // Changing bindings therefore leaves the portable definition intact.
        let mut references = Vec::new();
        for node in &ordered_nodes {
            let document = match &node.kind {
                NodeKind::DocumentHost { document } => Some(*document),
                NodeKind::Button {
                    action: Action::SelectDocument { document, .. },
                    ..
                } => Some(document.document_id),
                _ => None,
            };
            if let Some(document) = document {
                if !references.contains(&document) {
                    references.push(document);
                }
            }
        }
        let documents: Vec<_> = references
            .into_iter()
            .enumerate()
            .map(|(i, document)| DocumentBinding {
                parameter: format!("document_{i}"),
                document,
            })
            .collect();
        let slot = |id: DocumentId| {
            documents
                .iter()
                .find(|d| d.document == id)
                .unwrap()
                .parameter
                .clone()
        };
        let nodes: Vec<_> = ordered_nodes
            .into_iter()
            .map(|node| PortableNode {
                id: node.id.clone(),
                layout: node.layout.clone(),
                decoration: node.decoration.clone(),
                kind: match &node.kind {
                    NodeKind::Row { children } => PortableKind::Row {
                        children: children.clone(),
                    },
                    NodeKind::Column { children } => PortableKind::Column {
                        children: children.clone(),
                    },
                    NodeKind::Box { child } => PortableKind::Box {
                        child: child.clone(),
                    },
                    NodeKind::Text { text, role, ink } => PortableKind::Text {
                        text: text.clone(),
                        role: *role,
                        ink: *ink,
                    },
                    NodeKind::Spacer { space } => PortableKind::Spacer { space: *space },
                    NodeKind::Divider => PortableKind::Divider,
                    NodeKind::DocumentHost { document } => PortableKind::DocumentHost {
                        document: slot(*document),
                    },
                    NodeKind::Button {
                        label,
                        action: Action::SelectDocument { target, document },
                    } => PortableKind::Button {
                        label: label.clone(),
                        target: target.clone(),
                        document: slot(document.document_id),
                    },
                },
            })
            .collect();
        let result = Self::Composition(CompositionEnvelope {
            format_version: 2,
            definition: Definition {
                catalog_id: mini_apps::COMPOSITION_CATALOG_ID.into(),
                a2ui_version: "1.0".into(),
                workspace: WorkspaceParameter::Workspace,
                nodes,
            },
            bindings: Bindings {
                workspace,
                documents,
            },
        });
        result.validate()?;
        mini_apps::bounded_json_with_limit(&result, MAX_FILE_BYTES)?;
        Ok(result)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_FILE_BYTES {
            return Err("mini app exceeds byte limit");
        }
        let result: Self =
            serde_json::from_slice(bytes).map_err(|_| "unsupported mini app format")?;
        if matches!(result, Self::TaskDocument(_)) && bytes.len() > MAX_BYTES {
            return Err("mini app exceeds byte limit");
        }
        result.validate()?;
        Ok(result)
    }
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        self.validate()?;
        match self {
            Self::TaskDocument(v) => v.encode(),
            Self::Composition(_) => mini_apps::bounded_json_with_limit(self, MAX_FILE_BYTES),
        }
    }
    pub fn workspace(&self) -> WorkspaceId {
        match self {
            Self::TaskDocument(v) => v.bindings.workspace,
            Self::Composition(v) => v.bindings.workspace,
        }
    }
    pub fn task_list(&self) -> Option<uuid::Uuid> {
        match self {
            Self::TaskDocument(v) => v.bindings.task_list,
            Self::Composition(_) => None,
        }
    }
    pub fn document_references(&self) -> Vec<DocumentId> {
        match self {
            Self::TaskDocument(v) => v.bindings.document.into_iter().collect(),
            Self::Composition(v) => {
                let mut ids = Vec::new();
                for b in &v.bindings.documents {
                    if !ids.contains(&b.document) {
                        ids.push(b.document);
                    }
                }
                ids
            }
        }
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::TaskDocument(v) => v.validate(),
            Self::Composition(v) => v
                .instantiate(v.bindings.workspace, "saved-validation".into())
                .map(|_| ()),
        }
    }
    pub fn instantiate(
        &self,
        workspace: WorkspaceId,
        surface_id: String,
    ) -> Result<Declaration, &'static str> {
        match self {
            Self::TaskDocument(v) => v.instantiate(workspace, surface_id),
            Self::Composition(v) => v.instantiate(workspace, surface_id),
        }
    }
    /// Runtime surface identity is deliberately excluded from saved equality.
    pub fn matches_declaration(&self, workspace: WorkspaceId, declaration: &Declaration) -> bool {
        Self::capture(workspace, declaration).is_ok_and(|captured| captured == *self)
    }
}
impl CompositionEnvelope {
    fn instantiate(
        &self,
        workspace: WorkspaceId,
        surface_id: String,
    ) -> Result<Declaration, &'static str> {
        if self.format_version != 2
            || self.definition.catalog_id != mini_apps::COMPOSITION_CATALOG_ID
            || self.definition.a2ui_version != "1.0"
        {
            return Err("unsupported mini app version or catalog");
        }
        if workspace != self.bindings.workspace || workspace.as_uuid().is_nil() {
            return Err("mini app belongs to another workspace");
        }
        if self.bindings.documents.len() > composition::MAX_NODES
            || self.definition.nodes.len() > composition::MAX_NODES
        {
            return Err("unsupported composition size");
        }
        let mut parameters = HashSet::new();
        for binding in &self.bindings.documents {
            if !composition::valid_id(&binding.parameter)
                || binding.document.as_uuid().is_nil()
                || !parameters.insert(binding.parameter.as_str())
            {
                return Err("invalid composition bindings");
            }
        }
        let mut used = HashSet::new();
        let mut resolve = |parameter: &str| -> Result<DocumentReference, &'static str> {
            let binding = self
                .bindings
                .documents
                .iter()
                .find(|b| b.parameter == parameter)
                .ok_or("missing document parameter")?;
            used.insert(binding.parameter.as_str());
            Ok(DocumentReference {
                workspace: "current".into(),
                document_id: binding.document,
            })
        };
        let mut components = Vec::new();
        for node in &self.definition.nodes {
            let mut c = Component {
                id: node.id.clone(),
                component: String::new(),
                children: None,
                arrangement: None,
                bucket: None,
                list: None,
                document: None,
                layout: (node.layout != Layout::default()).then(|| node.layout.clone()),
                decoration: None,
                text: None,
                role: None,
                ink: None,
                space: None,
                action: None,
            };
            c.component = match &node.kind {
                PortableKind::Row { children } => {
                    c.children = Some(children.clone());
                    "Row"
                }
                PortableKind::Column { children } => {
                    c.children = Some(children.clone());
                    "Column"
                }
                PortableKind::Box { child } => {
                    c.children = Some(vec![child.clone()]);
                    "Box"
                }
                PortableKind::Text { text, role, ink } => {
                    c.text = Some(text.clone());
                    c.role = (*role != TextRole::Ui).then_some(*role);
                    c.ink = (*ink != Ink::Primary).then_some(*ink);
                    "Text"
                }
                PortableKind::Spacer { space } => {
                    c.space = Some(*space);
                    "Spacer"
                }
                PortableKind::Divider => "Divider",
                PortableKind::DocumentHost { document } => {
                    c.document = Some(resolve(document)?);
                    "DocumentHost"
                }
                PortableKind::Button {
                    label,
                    target,
                    document,
                } => {
                    c.text = Some(label.clone());
                    c.action = Some(Action::SelectDocument {
                        target: target.clone(),
                        document: resolve(document)?,
                    });
                    "Button"
                }
            }
            .into();
            if node.decoration != Decoration::default() {
                c.decoration = Some(node.decoration.clone());
            }
            components.push(c);
        }
        if used != parameters {
            return Err("unused composition binding");
        }
        Declaration::validate(mini_apps::Envelope {
            version: "1.0".into(),
            surface: mini_apps::Surface {
                surface_id,
                catalog_id: mini_apps::COMPOSITION_CATALOG_ID.into(),
                send_data_model: false,
                components,
            },
        })
    }
}

#[cfg(test)]
mod tests;
