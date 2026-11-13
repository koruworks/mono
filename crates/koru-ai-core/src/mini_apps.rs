// SPDX-License-Identifier: MPL-2.0

//! Proposed, bounded task/document catalog. This is a reference language, not
//! a content data model or a general component renderer. Public approval is
//! pending; see docs/features/a2ui/CATALOG.md.

use crate::{AiToolDefinition, AiToolSafety, DocumentId, WorkspaceId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};
use uuid::Uuid;

pub const CATALOG_ID: &str = "koru:task-document:0.1-proposal1";
pub const CREATE_TOOL: &str = "create_task_document_app";
pub const LIST_TOOL: &str = "find_task_app_lists";
pub const MAX_BYTES: usize = 4096;
pub const FIXTURE: &str = include_str!("mini_apps/fixture.json");

/// Serialize an already parsed value without allocating its unbounded JSON
/// representation merely to discover that it exceeds this catalog's limit.
pub fn bounded_json<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, &'static str> {
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) > MAX_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "surface exceeds byte limit",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded(Vec::new());
    serde_json::to_writer(&mut writer, value).map_err(|_| "surface exceeds byte limit")?;
    Ok(writer.0)
}

/// UI-minted, live-only provenance. None of this is a provider argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HostContext {
    pub renderer_id: Uuid,
    pub request_id: Uuid,
    pub workspace_id: WorkspaceId,
}

/// Shared between one live worker attempt and its tool jobs. Revocation also
/// fences a job that finishes after the stream was dropped. 0=available,
/// 1=validating, 2=created, 3=revoked. It never enters a wire or saved value.
#[derive(Clone, Debug)]
pub struct Authority {
    pub host: HostContext,
    state: Arc<AtomicU8>,
    delivered: Arc<AtomicBool>,
}
impl PartialEq for Authority {
    fn eq(&self, other: &Self) -> bool {
        self.host == other.host && Arc::ptr_eq(&self.state, &other.state)
    }
}
impl Eq for Authority {}
impl Authority {
    pub fn new(host: HostContext) -> Self {
        Self {
            host,
            state: Arc::new(AtomicU8::new(0)),
            delivered: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn revoke(&self) {
        self.state.store(3, Ordering::Release);
    }
    pub fn live(&self) -> bool {
        self.state.load(Ordering::Acquire) < 3
    }
    pub fn begin(&self) -> bool {
        self.state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    pub fn finish(&self, valid: bool) -> bool {
        self.state
            .compare_exchange(
                1,
                if valid { 2 } else { 0 },
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
    /// Worker-side one-result fence, independent of provider call count.
    pub fn claim_delivery(&self) -> bool {
        self.state.load(Ordering::Acquire) == 2 && !self.delivered.swap(true, Ordering::AcqRel)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub version: String,
    #[serde(rename = "createSurface")]
    pub surface: Surface,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Surface {
    pub surface_id: String,
    pub catalog_id: String,
    pub send_data_model: bool,
    pub components: Vec<Component>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub id: String,
    pub component: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arrangement: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list: Option<ListReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<DocumentReference>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ListReference {
    pub workspace: String,
    pub list_id: Uuid,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DocumentReference {
    pub workspace: String,
    pub document_id: DocumentId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskSelection {
    Bucket(String),
    List(Uuid),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub envelope: Envelope,
    pub selection: TaskSelection,
    pub swapped: bool,
    pub document: Option<DocumentId>,
}
impl Declaration {
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_BYTES {
            return Err("surface exceeds byte limit");
        }
        let envelope: Envelope =
            serde_json::from_slice(bytes).map_err(|_| "unsupported declaration")?;
        Self::validate(envelope)
    }
    pub fn from_value(value: &Value) -> Result<Self, &'static str> {
        Self::parse(&bounded_json(value)?)
    }
    pub fn validate(envelope: Envelope) -> Result<Self, &'static str> {
        // The public event is typed but remains untrusted at a receiver.
        bounded_json(&envelope)?;
        let surface = &envelope.surface;
        if envelope.version != "1.0"
            || surface.catalog_id != CATALOG_ID
            || surface.send_data_model
            || surface.surface_id.is_empty()
            || surface.surface_id.len() > 96
            || !surface
                .surface_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_:".contains(&b))
            || surface.components.len() != 5
        {
            return Err("unsupported surface");
        }
        let expected = [
            ("root", "TaskDocument"),
            ("selector", "TaskListSelector"),
            ("tasks", "TaskCollection"),
            ("search", "DocumentSearch"),
            ("document", "DocumentHost"),
        ];
        for (id, kind) in expected {
            let mut matching = surface.components.iter().filter(|c| c.id == id);
            let c = matching.next().ok_or("missing component")?;
            if matching.next().is_some()
                || c.component != kind
                || (id != "root" && (c.children.is_some() || c.arrangement.is_some()))
                || (id != "tasks" && (c.bucket.is_some() || c.list.is_some()))
                || (id != "document" && c.document.is_some())
            {
                return Err("unsupported component");
            }
        }
        let component = |id: &str| surface.components.iter().find(|c| c.id == id).unwrap();
        let root = component("root");
        if root.children.as_deref()
            != Some(&[
                "selector".into(),
                "tasks".into(),
                "search".into(),
                "document".into(),
            ])
        {
            return Err("unsupported child graph");
        }
        let swapped = match root.arrangement.as_deref() {
            Some("tasks-first") => false,
            Some("document-first") => true,
            _ => return Err("unsupported arrangement"),
        };
        let tasks = component("tasks");
        let selection = match (&tasks.bucket, &tasks.list) {
            (Some(bucket), None)
                if ["today", "next", "upcoming", "later", "completed"]
                    .contains(&bucket.as_str()) =>
            {
                TaskSelection::Bucket(bucket.clone())
            }
            (None, Some(list)) if list.workspace == "current" && !list.list_id.is_nil() => {
                TaskSelection::List(list.list_id)
            }
            _ => return Err("unsupported task selection"),
        };
        let document = component("document")
            .document
            .as_ref()
            .map(|d| {
                if d.workspace != "current" || d.document_id.as_uuid().is_nil() {
                    Err("unsupported document scope")
                } else {
                    Ok(d.document_id)
                }
            })
            .transpose()?;
        Ok(Self {
            envelope,
            selection,
            swapped,
            document,
        })
    }
    pub fn normalize(mut self, invocation: crate::ToolInvocationId) -> Self {
        self.envelope.surface.surface_id = format!("mini-app-{invocation}");
        self
    }
}

pub fn definitions() -> Vec<AiToolDefinition> {
    let mut schema: Value =
        serde_json::from_str(include_str!("mini_apps/create.schema.json")).expect("owned schema");
    schema["properties"]["createSurface"]["properties"]["catalogId"]["const"] = CATALOG_ID.into();
    [ (CREATE_TOOL, "Create a task/document mini app", "Propose one native task/document mini app for this live host. Supply the complete A2UI 1.0 envelope. Use find_task_app_lists for named/empty lists and list_documents for document IDs. Only references and the named arrangement belong here; never task rows, task titles, document text or search results. A successful result is validated and offered to the native host; it does not acknowledge that a tab opened.", schema),
      (LIST_TOOL, "Find task lists", "Find list ID/name metadata, including empty lists, in this host's current workspace. Explicit lookup only; no task content is returned.", json!({"type":"object","properties":{"query":{"type":"string","maxLength":128},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":20}},"required":["query"],"additionalProperties":false})) ]
      .into_iter().map(|(name,title,description,input_schema)| AiToolDefinition {
          name: name.into(), title: title.into(), description: description.into(), input_schema,
          output_schema: json!({"type":"object"}),
          safety: AiToolSafety { read_only: true, destructive: false, idempotent: true, open_world: false },
      }).collect()
}

#[cfg(test)]
mod tests;
