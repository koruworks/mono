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
pub const COMPOSITION_CATALOG_ID: &str = "koru:composition:0.1-proposal1";
pub const CREATE_TOOL: &str = "create_task_document_app";
pub const UPDATE_TOOL: &str = "update_task_document_app";
pub const LIST_TOOL: &str = "find_task_app_lists";
pub const MAX_BYTES: usize = 4096;
pub const FIXTURE: &str = include_str!("mini_apps/fixture.json");

/// An expiring bearer, minted by the AI worker for one exact live attempt.
/// Debug deliberately never prints it; it is not a workspace or tab identity.
#[derive(Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(rename = "MiniAppCapability"))]
#[serde(transparent)]
pub struct Capability(
    #[cfg_attr(feature = "schema", schemars(length(min = 46, max = 46)))] pub String,
);
impl std::fmt::Debug for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Capability([redacted])")
    }
}
impl Capability {
    pub fn in_text(text: &str) -> bool {
        text.match_indices("koru-mini-").any(|(start, _)| {
            text.get(start..start + 46)
                .is_some_and(|s| Self(s.into()).valid())
        })
    }
    pub fn mint() -> Self {
        Self(format!("koru-mini-{}", uuid::Uuid::new_v4()))
    }
    pub fn valid(&self) -> bool {
        self.0.strip_prefix("koru-mini-").is_some_and(|s| {
            s.len() == 36
                && uuid::Uuid::parse_str(s).is_ok_and(|id| {
                    !id.is_nil() && id.hyphenated().to_string().eq_ignore_ascii_case(s)
                })
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeContext {
    pub capability: Capability,
    pub workspace_id: crate::WorkspaceId,
    pub refinement_surface: Option<String>,
}
/// Redact every capability spelling, including expired ones echoed from an
/// agent's own history. Keys and values both carry untrusted provider text.
pub fn sanitize_agent_event(event: crate::AiEvent) -> crate::AiEvent {
    fn clean(text: &str) -> String {
        let mut rest = text;
        let mut result = String::new();
        while let Some(start) = rest.find("koru-mini-") {
            result.push_str(&rest[..start]);
            rest = &rest[start..];
            if rest.get(..46).is_some_and(|s| Capability(s.into()).valid()) {
                result.push_str("[redacted mini-app permission]");
                rest = &rest[46..];
            } else {
                result.push('k');
                rest = &rest[1..];
            }
        }
        result.push_str(rest);
        result
    }
    fn visit(value: &mut Value) {
        match value {
            Value::String(text) => *text = clean(text),
            Value::Array(items) => items.iter_mut().for_each(visit),
            Value::Object(fields) => {
                let old = std::mem::take(fields);
                for (key, mut value) in old {
                    visit(&mut value);
                    fields.insert(clean(&key), value);
                }
            }
            _ => {}
        }
    }
    let attempt_id = event.attempt_id();
    let mut value = serde_json::to_value(&event).expect("owned event serializes");
    visit(&mut value);
    serde_json::from_value(value).unwrap_or_else(|_| crate::AiEvent::Failed {
        attempt_id,
        error: crate::AiError::new(
            crate::AiErrorClass::Provider,
            "The agent returned invalid metadata.",
            false,
        ),
        at: crate::Timestamp::now(),
    })
}
pub const REFINEMENT_FIXTURE: &str = include_str!("mini_apps/refinement.fixture.json");

pub mod artifact;
pub mod composition;
pub use composition::Composition;

/// Serialize an already parsed value without allocating its unbounded JSON
/// representation merely to discover that it exceeds this catalog's limit.
pub fn bounded_json<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, &'static str> {
    bounded_json_with_limit(value, MAX_BYTES)
}
fn bounded_json_with_limit<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
) -> Result<Vec<u8>, &'static str> {
    struct Bounded(Vec<u8>, usize);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) > self.1 {
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
    let mut writer = Bounded(Vec::new(), limit);
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refinement: Option<RefinementTarget>,
}

/// Native lifetime and optimistic revision, deliberately outside A2UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RefinementTarget {
    pub tab_id: u64,
    pub surface_id: Uuid,
    pub base_revision: u64,
}
impl RefinementTarget {
    pub fn surface_name(self) -> String {
        format!("mini-app-{}", self.surface_id)
    }
}

/// Transient model-visible placement metadata for an exact active native owner.
/// Never place this in persisted chat, events, provider continuity or sessions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RefinementContext {
    pub target: RefinementTarget,
    pub declaration: Envelope,
}
impl RefinementContext {
    pub fn validate(&self) -> Result<Declaration, &'static str> {
        if self.target.surface_id.is_nil()
            || self.declaration.surface.surface_id != self.target.surface_name()
        {
            return Err("refinement surface mismatch");
        }
        Declaration::validate(self.declaration.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UpdateEnvelope {
    pub version: String,
    #[serde(rename = "updateComponents")]
    pub update: ComponentUpdate,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ComponentUpdate {
    pub surface_id: String,
    pub components: Vec<Component>,
}
impl UpdateEnvelope {
    pub fn from_value(value: &Value) -> Result<Self, &'static str> {
        let bytes = bounded_json(value)?;
        let result: Self = serde_json::from_slice(&bytes).map_err(|_| "unsupported refinement")?;
        result.validate_shape()?;
        Ok(result)
    }
    fn validate_shape(&self) -> Result<(), &'static str> {
        bounded_json(self)?;
        if self
            .update
            .components
            .first()
            .is_some_and(|c| c.component == "TaskDocument")
        {
            self.arrangement().map(|_| ())
        } else {
            if self.version != "1.0" {
                return Err("unsupported refinement");
            }
            Declaration::validate(Envelope {
                version: self.version.clone(),
                surface: Surface {
                    surface_id: self.update.surface_id.clone(),
                    catalog_id: COMPOSITION_CATALOG_ID.into(),
                    send_data_model: false,
                    components: self.update.components.clone(),
                },
            })
            .map(|_| ())
        }
    }
    /// Full replacement placement tree, checked against the active validated
    /// declaration. This does not authorize delivery or advance its revision.
    pub fn refine(&self, current: &Declaration) -> Result<Declaration, &'static str> {
        self.validate_shape()?;
        let checked = Declaration::validate(current.envelope.clone())?;
        let current = &checked;
        if self.update.surface_id != current.envelope.surface.surface_id {
            return Err("refinement surface mismatch");
        }
        let mut envelope = current.envelope.clone();
        if let Some(old) = current.composition() {
            envelope.surface.components = self.update.components.clone();
            let next = Declaration::validate(envelope)?;
            if !old.same_bindings_and_identity(next.composition().ok_or("catalog cannot change")?) {
                return Err("refinement cannot change identities or bindings");
            }
            Ok(next)
        } else {
            self.arrangement()?;
            let root = envelope
                .surface
                .components
                .iter_mut()
                .find(|c| c.id == "root")
                .ok_or("missing root")?;
            *root = self.update.components[0].clone();
            Declaration::validate(envelope)
        }
    }
    /// Only replacing the fixed root's arrangement is supported. In particular,
    /// missing children cannot remove an editor, selector or search control.
    pub fn arrangement(&self) -> Result<bool, &'static str> {
        bounded_json(self)?;
        if self.version != "1.0" || self.update.components.len() != 1 {
            return Err("refinement must preserve the active editor and controls");
        }
        let mut fixture = Declaration::parse(FIXTURE.as_bytes())?.envelope;
        fixture.surface.surface_id = self.update.surface_id.clone();
        if self.update.components[0].id != "root" {
            return Err("only the named arrangement can be refined");
        }
        fixture.surface.components[0] = self.update.components[0].clone();
        Declaration::validate(fixture)?
            .swapped()
            .ok_or("unsupported legacy refinement")
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<composition::Layout>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decoration: Option<composition::Decoration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<composition::TextRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ink: Option<composition::Ink>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space: Option<composition::Space>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<composition::Action>,
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
    pub kind: DeclarationKind,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeclarationKind {
    TaskDocument {
        selection: TaskSelection,
        swapped: bool,
        document: Option<DocumentId>,
    },
    Composition(Composition),
}
impl Declaration {
    pub fn task_selection(&self) -> Option<&TaskSelection> {
        match &self.kind {
            DeclarationKind::TaskDocument { selection, .. } => Some(selection),
            _ => None,
        }
    }
    pub fn swapped(&self) -> Option<bool> {
        match &self.kind {
            DeclarationKind::TaskDocument { swapped, .. } => Some(*swapped),
            _ => None,
        }
    }
    pub fn initial_document(&self) -> Option<DocumentId> {
        match &self.kind {
            DeclarationKind::TaskDocument { document, .. } => *document,
            DeclarationKind::Composition(c) => c.initial_document(),
        }
    }
    pub fn composition(&self) -> Option<&Composition> {
        match &self.kind {
            DeclarationKind::Composition(c) => Some(c),
            _ => None,
        }
    }
    /// Stable first-occurrence order; includes every local action target.
    pub fn document_references(&self) -> Vec<DocumentId> {
        match &self.kind {
            DeclarationKind::Composition(c) => c.document_references(),
            _ => self.initial_document().into_iter().collect(),
        }
    }
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_BYTES {
            return Err("surface exceeds byte limit");
        }
        let envelope: Envelope =
            serde_json::from_slice(bytes).map_err(|error| declaration_decode_reason(&error))?;
        Self::validate(envelope)
    }
    pub fn from_value(value: &Value) -> Result<Self, &'static str> {
        Self::parse(&bounded_json(value)?)
    }
    pub fn validate(envelope: Envelope) -> Result<Self, &'static str> {
        // The public event is typed but remains untrusted at a receiver.
        let encoded_len = bounded_json(&envelope)?.len();
        let surface = &envelope.surface;
        if envelope.version != "1.0"
            || ![CATALOG_ID, COMPOSITION_CATALOG_ID].contains(&surface.catalog_id.as_str())
            || surface.send_data_model
            || surface.surface_id.is_empty()
            || surface.surface_id.len() > 96
            || !surface
                .surface_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_:".contains(&b))
        {
            return Err("unsupported surface");
        }
        if surface.catalog_id == COMPOSITION_CATALOG_ID {
            // Creation replaces the caller's spelling with `mini-app-` plus
            // a hyphenated UUID (45 ASCII bytes). Reserve that growth now so
            // a valid declaration stays valid when delivered or saved/reopened.
            if encoded_len + 45usize.saturating_sub(surface.surface_id.len()) > MAX_BYTES {
                return Err("surface exceeds byte limit");
            }
            let composition = Composition::validate(&surface.components)?;
            return Ok(Self {
                envelope,
                kind: DeclarationKind::Composition(composition),
            });
        }
        if surface.components.len() != 5 {
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
                || c.layout.is_some()
                || c.decoration.is_some()
                || c.text.is_some()
                || c.role.is_some()
                || c.ink.is_some()
                || c.space.is_some()
                || c.action.is_some()
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
            kind: DeclarationKind::TaskDocument {
                selection,
                swapped,
                document,
            },
        })
    }
    pub fn normalize(mut self, invocation: crate::ToolInvocationId) -> Self {
        self.envelope.surface.surface_id = format!("mini-app-{invocation}");
        self
    }
}

/// Serde remains the sole decoding authority. Its description is inspected
/// privately only for library-owned prefixes; no key, value or path escapes.
fn declaration_decode_reason(error: &serde_json::Error) -> &'static str {
    if error.is_syntax() || error.is_eof() {
        return "malformed declaration JSON";
    }
    let description = error.to_string();
    for (prefix, reason) in [
        ("missing field ", "missing declaration field"),
        ("unknown field ", "unknown declaration field"),
        ("duplicate field ", "duplicate declaration field"),
        ("invalid type:", "invalid declaration type"),
        ("invalid value:", "invalid declaration value"),
        ("UUID parsing failed:", "invalid declaration value"),
        ("not a DocumentId", "invalid declaration value"),
    ] {
        if description.starts_with(prefix) {
            return reason;
        }
    }
    "unsupported declaration"
}

pub fn definitions() -> Vec<AiToolDefinition> {
    let mut schema: Value =
        serde_json::from_str(include_str!("mini_apps/create.schema.json")).expect("owned schema");
    schema["properties"]["createSurface"]["properties"]["catalogId"]["const"] = CATALOG_ID.into();
    let root = schema["properties"]["createSurface"]["properties"]["components"]["items"]["oneOf"]
        [0]
    .clone();
    let update_schema = json!({"type":"object","additionalProperties":false,"required":["version","updateComponents"],"properties":{"version":{"const":"1.0"},"updateComponents":{"type":"object","additionalProperties":false,"required":["surfaceId","components"],"properties":{"surfaceId":{"type":"string","minLength":1,"maxLength":96},"components":{"type":"array","minItems":1,"maxItems":1,"items":root}}}}});
    [ (CREATE_TOOL, "Create a task/document mini app", "Propose one native task/document mini app for this live host. Supply the complete A2UI 1.0 envelope. Use find_task_app_lists for named/empty lists and list_documents for document IDs. Only references and the named arrangement belong here; never task rows, task titles, document text or search results. A successful result is validated and offered to the native host; it does not acknowledge that a tab opened.", schema),
      (LIST_TOOL, "Find task lists", "Find list ID/name metadata, including empty lists, in this host's current workspace. Explicit lookup only; no task content is returned.", json!({"type":"object","properties":{"query":{"type":"string","maxLength":128},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":20}},"required":["query"],"additionalProperties":false})),
      (UPDATE_TOOL, "Rearrange the current mini app", "Change the current live mini app's named arrangement using one A2UI 1.0 updateComponents envelope. Replace only the root TaskDocument component, retaining its children selector, tasks, search, document. document-first puts the document on the left and tasks on the right; tasks-first reverses that. Removing controls or changing bindings is unsupported. A validated result is offered to the native host, which may refuse a stale revision; tool success does not acknowledge local application.", update_schema) ]
      .into_iter().map(|(name,title,description,input_schema)| AiToolDefinition {
          name: name.into(), title: title.into(), description: description.into(), input_schema,
          output_schema: json!({"type":"object"}),
          safety: AiToolSafety { read_only: true, destructive: false, idempotent: true, open_world: false },
      }).collect()
}

#[cfg(test)]
mod tests;
