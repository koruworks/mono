// SPDX-License-Identifier: MPL-2.0

//! Closed adapter vocabulary, deliberately independent of the native element
//! engine. Validation runs once at ingress; native frames consume typed nodes.
use super::{Component, DocumentReference};
use crate::DocumentId;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub const MAX_NODES: usize = 32;
pub const MAX_DEPTH: usize = 8;
pub const MAX_TEXT_BYTES: usize = 256;
pub const FIXTURE: &str = include_str!("composition.fixture.json");

macro_rules! tokens {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
        pub enum $name { $($variant),+ }
    };
}
tokens!(Space {
    S0,
    S0_5,
    S1,
    S1_5,
    S2,
    S2_5,
    S3,
    S3_5,
    S4,
    S5,
    S6,
    S7,
    S8,
    S10,
    S12
});
tokens!(Radius {
    None,
    S,
    M,
    L,
    XL,
    XXL,
    Full
});
tokens!(TextRole {
    Ui,
    Row,
    Hint,
    Meta,
    PopoverHeading
});
tokens!(Ink {
    Primary,
    Secondary,
    Hint,
    Accent
});
tokens!(Surface {
    Ground,
    Raised,
    Chip
});
tokens!(Sizing { Hug, Fill });
tokens!(Align {
    Start,
    Center,
    End,
    Stretch
});

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Layout {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<Sizing>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<Sizing>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap: Option<Space>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub padding: Option<Space>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub align: Option<Align>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Decoration {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<Surface>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<Radius>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Action {
    SelectDocument {
        target: String,
        document: DocumentReference,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub layout: Layout,
    pub decoration: Decoration,
    pub kind: NodeKind,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
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
        action: Action,
    },
    DocumentHost {
        document: DocumentId,
    },
}
impl Node {
    pub fn children(&self) -> &[String] {
        match &self.kind {
            NodeKind::Row { children } | NodeKind::Column { children } => children,
            NodeKind::Box { child } => std::slice::from_ref(child),
            _ => &[],
        }
    }
}

/// Constructed only after graph, token, action and binding validation. The root
/// is always `root`; the native host keeps editors outside this placement tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Composition {
    nodes: Vec<Node>,
    document_host: String,
}
impl Composition {
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }
    pub fn document_host(&self) -> &str {
        &self.document_host
    }
    pub fn initial_document(&self) -> Option<DocumentId> {
        self.nodes.iter().find_map(|n| match n.kind {
            NodeKind::DocumentHost { document } => Some(document),
            _ => None,
        })
    }
    pub fn document_references(&self) -> Vec<DocumentId> {
        let mut result = Vec::new();
        for node in &self.nodes {
            let id = match &node.kind {
                NodeKind::DocumentHost { document } => Some(*document),
                NodeKind::Button {
                    action: Action::SelectDocument { document, .. },
                    ..
                } => Some(document.document_id),
                _ => None,
            };
            if let Some(id) = id {
                if !result.contains(&id) {
                    result.push(id);
                }
            }
        }
        result
    }
    pub(super) fn validate(components: &[Component]) -> Result<Self, &'static str> {
        if components.is_empty() || components.len() > MAX_NODES {
            return Err("unsupported composition size");
        }
        let mut nodes = Vec::new();
        let mut ids = HashSet::new();
        let mut hosts = Vec::new();
        for c in components {
            if !valid_id(&c.id) || !ids.insert(c.id.as_str()) {
                return Err("invalid composition identity");
            }
            // Exact per-kind fields: a typed optional wire property does not
            // make that property legal on every component or legacy catalog.
            let allowed: &[&str] = match c.component.as_str() {
                "Row" | "Column" | "Box" => {
                    &["id", "component", "children", "layout", "decoration"]
                }
                "Text" => &["id", "component", "text", "role", "ink", "layout"],
                "Spacer" => &["id", "component", "space", "layout"],
                "Divider" => &["id", "component", "layout"],
                "Button" => &["id", "component", "text", "action", "layout"],
                "DocumentHost" => &["id", "component", "document", "layout"],
                _ => return Err("unsupported composition component"),
            };
            let fields = serde_json::to_value(c).map_err(|_| "invalid composition")?;
            if fields
                .as_object()
                .unwrap()
                .keys()
                .any(|key| !allowed.contains(&key.as_str()))
            {
                return Err("unsupported composition property");
            }
            let layout = c.layout.clone().unwrap_or_default();
            let container = matches!(c.component.as_str(), "Row" | "Column" | "Box");
            if (!container
                && (layout.gap.is_some() || layout.padding.is_some() || layout.align.is_some()))
                || (c.component == "Box" && layout.gap.is_some())
            {
                return Err("unsupported composition layout");
            }
            let children = || {
                c.children
                    .clone()
                    .filter(|v| !v.is_empty())
                    .ok_or("missing composition children")
            };
            let text = || {
                c.text
                    .clone()
                    .filter(|s| {
                        !s.trim().is_empty()
                            && s.len() <= MAX_TEXT_BYTES
                            && !s.chars().any(|ch| ch.is_control())
                    })
                    .ok_or("invalid composition label")
            };
            let kind = match c.component.as_str() {
                "Row" => NodeKind::Row {
                    children: children()?,
                },
                "Column" => NodeKind::Column {
                    children: children()?,
                },
                "Box" => {
                    let children = children()?;
                    if children.len() != 1 {
                        return Err("box requires one child");
                    }
                    NodeKind::Box {
                        child: children[0].clone(),
                    }
                }
                "Text" => NodeKind::Text {
                    text: text()?,
                    role: c.role.unwrap_or(TextRole::Ui),
                    ink: c.ink.unwrap_or(Ink::Primary),
                },
                "Spacer" => NodeKind::Spacer {
                    space: c.space.ok_or("missing spacer token")?,
                },
                "Divider" => NodeKind::Divider,
                "Button" => {
                    let action = c.action.clone().ok_or("missing local action")?;
                    let Action::SelectDocument { target, document } = &action;
                    if !valid_id(target) {
                        return Err("invalid action target");
                    }
                    validate_document(document)?;
                    NodeKind::Button {
                        label: text()?,
                        action,
                    }
                }
                "DocumentHost" => {
                    let document = c.document.as_ref().ok_or("missing document binding")?;
                    validate_document(document)?;
                    hosts.push(c.id.clone());
                    NodeKind::DocumentHost {
                        document: document.document_id,
                    }
                }
                _ => unreachable!(),
            };
            nodes.push(Node {
                id: c.id.clone(),
                layout,
                decoration: c.decoration.clone().unwrap_or_default(),
                kind,
            });
        }
        if hosts.len() != 1 {
            return Err("composition requires one document host");
        }
        let document_host = hosts.remove(0);
        let index: HashMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        let mut visited = HashSet::new();
        fn visit<'a>(
            id: &'a str,
            depth: usize,
            index: &HashMap<&'a str, &'a Node>,
            seen: &mut HashSet<&'a str>,
        ) -> Result<(), &'static str> {
            if depth > MAX_DEPTH || !seen.insert(id) {
                return Err("invalid composition graph");
            }
            let node = index.get(id).ok_or("missing composition child")?;
            for child in node.children() {
                visit(child, depth + 1, index, seen)?;
            }
            Ok(())
        }
        visit("root", 1, &index, &mut visited)?;
        if visited.len() != nodes.len() {
            return Err("disconnected composition graph");
        }
        for node in &nodes {
            if let NodeKind::Button {
                action: Action::SelectDocument { target, .. },
                ..
            } = &node.kind
            {
                if target != &document_host {
                    return Err("action target is not the document host");
                }
            }
        }
        Ok(Self {
            nodes,
            document_host,
        })
    }
    /// Layout refinements may move/relabel/style nodes, but cannot replace
    /// identities, native ownership, local actions or installation bindings.
    pub fn same_bindings_and_identity(&self, other: &Self) -> bool {
        self.nodes.len() == other.nodes.len()
            && self.nodes.iter().all(|n| {
                let Some(next) = other.node(&n.id) else {
                    return false;
                };
                match (&n.kind, &next.kind) {
                    (
                        NodeKind::Row { .. } | NodeKind::Column { .. },
                        NodeKind::Row { .. } | NodeKind::Column { .. },
                    )
                    | (NodeKind::Box { .. }, NodeKind::Box { .. })
                    | (NodeKind::Text { .. }, NodeKind::Text { .. })
                    | (NodeKind::Spacer { .. }, NodeKind::Spacer { .. })
                    | (NodeKind::Divider, NodeKind::Divider) => true,
                    (NodeKind::Button { action: a, .. }, NodeKind::Button { action: b, .. }) => {
                        a == b
                    }
                    (
                        NodeKind::DocumentHost { document: a },
                        NodeKind::DocumentHost { document: b },
                    ) => a == b,
                    _ => false,
                }
            })
    }
}
pub(super) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-:".contains(&b))
}
fn validate_document(reference: &DocumentReference) -> Result<(), &'static str> {
    if reference.workspace != "current" || reference.document_id.as_uuid().is_nil() {
        return Err("unsupported document scope");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
