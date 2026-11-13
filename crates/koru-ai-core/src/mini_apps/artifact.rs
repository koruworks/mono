// SPDX-License-Identifier: MPL-2.0

//! Saved declarations have no renderer identity or content. Parameter slots
//! belong to the portable definition; entity identities belong to one install.
use super::{CATALOG_ID, Declaration, TaskSelection};
use crate::{DocumentId, WorkspaceId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MIME: &str = "application/vnd.koru.mini-app+json";
pub const MAX_BYTES: usize = 4096;
pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
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
    pub arrangement: Arrangement,
    pub tasks: Tasks,
    pub document: Option<DocumentParameter>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Arrangement {
    TasksFirst,
    DocumentFirst,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields, rename_all = "snake_case")]
pub enum Tasks {
    Bucket { bucket: Bucket },
    List { parameter: ListParameter },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bucket {
    Today,
    Next,
    Upcoming,
    Later,
    Completed,
}
impl Bucket {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Next => "next",
            Self::Upcoming => "upcoming",
            Self::Later => "later",
            Self::Completed => "completed",
        }
    }
    fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "today" => Ok(Self::Today),
            "next" => Ok(Self::Next),
            "upcoming" => Ok(Self::Upcoming),
            "later" => Ok(Self::Later),
            "completed" => Ok(Self::Completed),
            _ => Err("unsupported bucket"),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListParameter {
    TaskList,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceParameter {
    Workspace,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentParameter {
    Document,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bindings {
    pub workspace: WorkspaceId,
    pub task_list: Option<Uuid>,
    pub document: Option<DocumentId>,
}

impl Envelope {
    pub fn capture(
        workspace: WorkspaceId,
        declaration: &Declaration,
    ) -> Result<Self, &'static str> {
        // Derived fields on a Declaration are public; revalidate its envelope.
        let checked = Declaration::validate(declaration.envelope.clone())?;
        let (tasks, task_list) = match checked.selection {
            TaskSelection::Bucket(bucket) => (
                Tasks::Bucket {
                    bucket: Bucket::parse(&bucket)?,
                },
                None,
            ),
            TaskSelection::List(id) => (
                Tasks::List {
                    parameter: ListParameter::TaskList,
                },
                Some(id),
            ),
        };
        let value = Self {
            format_version: FORMAT_VERSION,
            definition: Definition {
                catalog_id: CATALOG_ID.into(),
                a2ui_version: "1.0".into(),
                workspace: WorkspaceParameter::Workspace,
                arrangement: if checked.swapped {
                    Arrangement::DocumentFirst
                } else {
                    Arrangement::TasksFirst
                },
                tasks,
                document: checked.document.map(|_| DocumentParameter::Document),
            },
            bindings: Bindings {
                workspace,
                task_list,
                document: checked.document,
            },
        };
        value.validate()?;
        Ok(value)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_BYTES {
            return Err("mini app exceeds byte limit");
        }
        let value: Self =
            serde_json::from_slice(bytes).map_err(|_| "unsupported mini app format")?;
        value.validate()?;
        Ok(value)
    }

    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        self.validate()?;
        super::bounded_json(self)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.format_version != FORMAT_VERSION
            || self.definition.catalog_id != CATALOG_ID
            || self.definition.a2ui_version != "1.0"
        {
            return Err("unsupported mini app version or catalog");
        }
        if self.bindings.workspace.as_uuid().is_nil()
            || self.bindings.task_list.is_some_and(|id| id.is_nil())
            || self
                .bindings
                .document
                .is_some_and(|id| id.as_uuid().is_nil())
            || matches!(self.definition.tasks, Tasks::List { .. })
                != self.bindings.task_list.is_some()
            || self.definition.document.is_some() != self.bindings.document.is_some()
        {
            return Err("invalid mini app bindings");
        }
        Ok(())
    }

    /// Bind into a fresh live surface. Scope and membership still require
    /// authoritative Store validation before the native host receives this.
    pub fn instantiate(
        &self,
        workspace: WorkspaceId,
        surface_id: String,
    ) -> Result<Declaration, &'static str> {
        self.validate()?;
        if workspace != self.bindings.workspace {
            return Err("mini app belongs to another workspace");
        }
        let mut envelope = Declaration::parse(super::FIXTURE.as_bytes())?.envelope;
        envelope.surface.surface_id = surface_id;
        for component in &mut envelope.surface.components {
            match component.id.as_str() {
                "root" => {
                    component.arrangement = Some(
                        match self.definition.arrangement {
                            Arrangement::TasksFirst => "tasks-first",
                            Arrangement::DocumentFirst => "document-first",
                        }
                        .into(),
                    )
                }
                "tasks" => match self.definition.tasks {
                    Tasks::Bucket { bucket } => component.bucket = Some(bucket.as_str().into()),
                    Tasks::List { .. } => {
                        component.bucket = None;
                        component.list = Some(super::ListReference {
                            workspace: "current".into(),
                            list_id: self.bindings.task_list.ok_or("missing list binding")?,
                        });
                    }
                },
                "document" => {
                    component.document =
                        self.bindings
                            .document
                            .map(|document_id| super::DocumentReference {
                                workspace: "current".into(),
                                document_id,
                            })
                }
                _ => {}
            }
        }
        Declaration::validate(envelope)
    }
}

#[cfg(test)]
mod tests;
