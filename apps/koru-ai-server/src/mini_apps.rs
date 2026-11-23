// SPDX-License-Identifier: MPL-2.0
//! Native tool validation over an explicit metadata-only registry port.
use koru_ai_core::mini_apps::{self, Declaration, TaskSelection};
use koru_ai_core::{
    AiError, AiErrorClass, AiToolCall, AiToolDefinition, AiToolExecutionContext, AiToolExecutor,
    AiToolResult, Cancellation, DocumentId,
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

pub struct Registry {
    pub lists: Vec<(uuid::Uuid, String)>,
    pub documents: Vec<DocumentId>,
}
/// Implementations verify stored profile/workspace ownership before reading.
/// The caller's explicit grants and live-host authority are checked separately.
pub trait MiniAppRegistry: Send + Sync {
    fn read(
        &self,
        context: &AiToolExecutionContext,
        lists: bool,
        documents: bool,
    ) -> Result<Registry, AiError>;
}
pub struct MiniAppTools {
    registry: Arc<dyn MiniAppRegistry>,
}
impl MiniAppTools {
    pub fn new(registry: Arc<dyn MiniAppRegistry>) -> Self {
        Self { registry }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindLists {
    query: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "page_limit")]
    limit: usize,
}
fn page_limit() -> usize {
    20
}
enum Arguments {
    Create(Declaration),
    Update(mini_apps::UpdateEnvelope),
    Find(FindLists),
}
fn arguments(call: &AiToolCall) -> Result<Arguments, AiError> {
    let bytes = mini_apps::bounded_json(&call.arguments).map_err(validation_error)?;
    match call.name.as_str() {
        mini_apps::CREATE_TOOL => Ok(Arguments::Create(
            Declaration::parse(&bytes).map_err(validation_error)?,
        )),
        mini_apps::UPDATE_TOOL => Ok(Arguments::Update(
            mini_apps::UpdateEnvelope::from_value(&call.arguments).map_err(validation_error)?,
        )),
        mini_apps::LIST_TOOL => {
            let request: FindLists = serde_json::from_value(call.arguments.clone())
                .map_err(|_| validation_error("list lookup"))?;
            if request.query.chars().count() > 128
                || request.query.contains('\0')
                || !(1..=20).contains(&request.limit)
                || request.offset > 1_000_000
            {
                return Err(validation_error("list lookup"));
            }
            Ok(Arguments::Find(request))
        }
        _ => Err(unavailable()),
    }
}

/// Fixed guidance only, after the authoritative parser has already refused.
pub(crate) fn validation_error(reason: &'static str) -> AiError {
    let (code, guidance) = match reason {
        "missing declaration field" => (
            "mini_app_validation_missing_field",
            "Supply every required field from the published schema: version/createSurface; surfaceId/catalogId/sendDataModel/components; each component's id/component and required root children/arrangement.",
        ),
        "unknown declaration field" => (
            "mini_app_validation_unknown_field",
            "Remove extra fields. Use only the exact catalog's published envelope/component properties. Literal UI text belongs only on Text/Button; document content, styles, data and argument wrappers are unsupported.",
        ),
        "duplicate declaration field" => (
            "mini_app_validation_duplicate_field",
            "Send each JSON object field once; do not repeat version, createSurface or component/reference fields.",
        ),
        "invalid declaration type" => (
            "mini_app_validation_field_type",
            "Use exact JSON types: version, IDs, component and arrangement are strings; sendDataModel is boolean false; components and children are arrays; list/document bindings are objects, not strings.",
        ),
        "invalid declaration value" => (
            "mini_app_validation_field_value",
            "Use UUID strings for listId/documentId, workspace string current, and the literal values from the published schema; do not use labels or compound objects as IDs.",
        ),
        "malformed declaration JSON" => (
            "mini_app_validation_json",
            "Send one complete valid JSON object, not Markdown or a code fence.",
        ),
        "unsupported declaration" => (
            "mini_app_validation_shape",
            "Use the exact published envelope, component and reference field names and JSON types; omit unsupported fields.",
        ),
        "surface exceeds byte limit" => (
            "mini_app_validation_size",
            "Keep the A2UI envelope within 4096 UTF-8 bytes; include references and arrangement only.",
        ),
        "unsupported surface" => (
            "mini_app_validation_surface",
            "Use version 1.0, sendDataModel false and a 1-96 ASCII surfaceId. Choose the exact published branch: koru:task-document:0.1-proposal1 with five fixed components, or koru:composition:0.1-proposal1 with one bounded root tree and one DocumentHost.",
        ),
        "missing component" | "unsupported component" => (
            "mini_app_validation_components",
            "Include each fixed id/component pair exactly once: root/TaskDocument, selector/TaskListSelector, tasks/TaskCollection, search/DocumentSearch, document/DocumentHost. Only root has children/arrangement; only tasks has bucket/list; only document has document.",
        ),
        "unsupported child graph" => (
            "mini_app_validation_children",
            "root.children must be exactly selector, tasks, search, document in that order. Child IDs are strings, not component objects.",
        ),
        "unsupported arrangement" => (
            "mini_app_validation_arrangement",
            "root.arrangement must be tasks-first or document-first.",
        ),
        "unsupported task selection" => (
            "mini_app_validation_task_selection",
            "TaskCollection must have exactly one of bucket (today, next, upcoming, later, completed) or list (workspace current and a nonnil listId UUID). Omit the unused binding.",
        ),
        "unsupported document scope" => (
            "mini_app_validation_document_scope",
            "DocumentHost.document, when supplied, must use workspace current and a nonnil documentId UUID.",
        ),
        "unsupported refinement"
        | "refinement cannot change identities or bindings"
        | "refinement surface mismatch"
        | "refinement must preserve the active editor and controls"
        | "only the named arrangement can be refined" => (
            "mini_app_validation_refinement",
            "Use version 1.0 and updateComponents with the supplied current surfaceId. Legacy replaces root TaskDocument's arrangement only. Composition replaces the full tree, preserving every ID, native kind, document binding and action; Row/Column may interchange.",
        ),
        "unsupported composition size"
        | "invalid composition identity"
        | "unsupported composition component"
        | "unsupported composition property"
        | "unsupported composition layout"
        | "missing composition children"
        | "invalid composition label"
        | "box requires one child"
        | "missing spacer token"
        | "missing local action"
        | "invalid action target"
        | "missing document binding"
        | "composition requires one document host"
        | "invalid composition graph"
        | "missing composition child"
        | "disconnected composition graph"
        | "action target is not the document host" => (
            "mini_app_validation_composition",
            "Use the exact composition schema: at most 32 unique IDs, depth 8, one connected root tree, one DocumentHost and SelectDocument actions targeting it. Keep labels within 256 UTF-8 bytes and use only published component properties/tokens.",
        ),
        "list lookup" => (
            "mini_app_validation_list_query",
            "Use only query (at most 128 characters, no NUL), offset (0 through 1000000) and limit (1 through 20).",
        ),
        _ => return invalid(),
    };
    AiError::new(AiErrorClass::Provider, format!("{code}: {guidance}"), false)
}
/// The admission path reuses the registered pure parser before even profile
/// visibility I/O. Execution repeats it before registry access.
pub(crate) fn validate_arguments(name: &str, value: &serde_json::Value) -> Result<(), AiError> {
    arguments(&AiToolCall {
        call_id: "validation".into(),
        name: name.into(),
        arguments: value.clone(),
        thought_signatures: Vec::new(),
    })
    .map(|_| ())
}
impl AiToolExecutor for MiniAppTools {
    fn definitions(&self) -> Vec<AiToolDefinition> {
        mini_apps::definitions()
    }
    fn invoke(
        &self,
        context: &AiToolExecutionContext,
        call: &AiToolCall,
        cancellation: &Cancellation,
    ) -> Result<AiToolResult, AiError> {
        let args = arguments(call)?; // Bounds precede even registry access.
        let authority = context
            .mini_app
            .as_ref()
            .filter(|a| a.live())
            .ok_or_else(unavailable)?;
        if cancellation.is_cancelled() {
            authority.revoke();
            return Err(unavailable());
        }
        if context.profile_id.is_none()
            || !context.workspace_ids.contains(&authority.host.workspace_id)
        {
            return Err(unavailable());
        }
        // A current snapshot is required for any refinement. Check bindings
        // and identities before metadata access, including for unadvertised calls.
        let refined = if let Arguments::Update(update) = &args {
            let snapshot = authority.refinement.as_ref().ok_or_else(unavailable)?;
            if authority.host.refinement != Some(snapshot.target) {
                return Err(unavailable());
            }
            Some(
                update
                    .refine(&snapshot.validate().map_err(validation_error)?)
                    .map_err(validation_error)?,
            )
        } else {
            None
        };
        let declaration = match &args {
            Arguments::Create(declaration) => Some(declaration),
            _ => refined.as_ref(),
        };
        let needs_lists = match &args {
            Arguments::Find(_) => true,
            _ => declaration
                .is_some_and(|d| matches!(d.task_selection(), Some(TaskSelection::List(_)))),
        };
        let needs_document = declaration.is_some_and(|d| !d.document_references().is_empty());
        let registry = self.registry.read(context, needs_lists, needs_document)?;
        if cancellation.is_cancelled() {
            authority.revoke();
            return Err(unavailable());
        }
        if !authority.live() {
            return Err(unavailable());
        }
        if let Some(declaration) = declaration {
            if let Some(TaskSelection::List(id)) = declaration.task_selection()
                && !registry.lists.iter().any(|(list, _)| list == id)
            {
                return Err(invalid());
            }
            if declaration
                .document_references()
                .iter()
                .any(|id| !registry.documents.contains(id))
            {
                return Err(invalid());
            }
        }
        let content = match args {
            Arguments::Update(update) => {
                let target = authority.host.refinement.ok_or_else(unavailable)?;
                if target.surface_id.is_nil() || update.update.surface_id != target.surface_name() {
                    return Err(invalid());
                }
                if !authority.begin() {
                    return Err(unavailable());
                }
                let result = serde_json::to_value(update).map_err(|_| invalid());
                if cancellation.is_cancelled() {
                    authority.revoke();
                    return Err(unavailable());
                }
                if !authority.finish(result.is_ok()) {
                    return Err(unavailable());
                }
                result?
            }
            Arguments::Find(request) => {
                let query = request.query.to_lowercase();
                let matching: Vec<_> = registry
                    .lists
                    .iter()
                    .filter(|(_, name)| name.to_lowercase().contains(&query))
                    .collect();
                let page: Vec<_> = matching.iter().skip(request.offset).take(request.limit).map(|(id,name)|
                    json!({"listId":id,"name": name.chars().take(256).collect::<String>()})).collect();
                let end = request.offset.saturating_add(page.len());
                json!({"lists":page,"next_offset":(end < matching.len()).then_some(end)})
            }
            Arguments::Create(declaration) => {
                if !authority.begin() {
                    return Err(unavailable());
                }
                let result = (|| {
                    serde_json::to_value(declaration.normalize(context.invocation_id).envelope)
                        .map_err(|_| invalid())
                })();
                if cancellation.is_cancelled() {
                    authority.revoke();
                    return Err(unavailable());
                }
                if !authority.finish(result.is_ok()) {
                    return Err(unavailable());
                }
                result?
            }
        };
        Ok(AiToolResult {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            content,
            is_error: false,
        })
    }
}
pub(crate) fn creation_event(
    context: &AiToolExecutionContext,
    result: &AiToolResult,
) -> Result<koru_ai_wire::PublicAiEvent, AiError> {
    let authority = context
        .mini_app
        .as_ref()
        .filter(|a| a.live())
        .ok_or_else(unavailable)?;
    if ![mini_apps::CREATE_TOOL, mini_apps::UPDATE_TOOL].contains(&result.name.as_str())
        || result.is_error
        || result.call_id.is_empty()
        || result.call_id.len() > 256
    {
        return Err(invalid());
    }
    if result.name == mini_apps::UPDATE_TOOL {
        let target = authority.host.refinement.ok_or_else(unavailable)?;
        let envelope =
            mini_apps::UpdateEnvelope::from_value(&result.content).map_err(|_| invalid())?;
        let snapshot = authority.refinement.as_ref().ok_or_else(unavailable)?;
        if snapshot.target != target {
            return Err(unavailable());
        }
        envelope
            .refine(&snapshot.validate().map_err(validation_error)?)
            .map_err(validation_error)?;
        if envelope.update.surface_id != target.surface_name() || !authority.claim_delivery() {
            return Err(unavailable());
        }
        return Ok(koru_ai_wire::PublicAiEvent::MiniAppUpdated {
            attempt_id: context.attempt_id,
            chat_id: context.chat_id,
            profile_id: context.profile_id.ok_or_else(unavailable)?,
            invocation_id: context.invocation_id,
            call_id: result.call_id.clone(),
            host: authority.host,
            envelope,
        });
    }
    let declaration = Declaration::from_value(&result.content).map_err(|_| invalid())?;
    if declaration.envelope.surface.surface_id != format!("mini-app-{}", context.invocation_id)
        || !authority.claim_delivery()
    {
        return Err(unavailable());
    }
    Ok(koru_ai_wire::PublicAiEvent::MiniAppCreated {
        attempt_id: context.attempt_id,
        chat_id: context.chat_id,
        profile_id: context.profile_id.ok_or_else(unavailable)?,
        invocation_id: context.invocation_id,
        call_id: result.call_id.clone(),
        host: authority.host,
        envelope: declaration.envelope,
    })
}
pub(crate) fn unavailable() -> AiError {
    AiError::new(
        AiErrorClass::Configuration,
        "Mini-app generation is unavailable for this live host or the request already created an app.",
        false,
    )
}
fn invalid() -> AiError {
    AiError::new(
        AiErrorClass::Provider,
        "The mini-app declaration or current-workspace reference is invalid.",
        false,
    )
}

#[cfg(test)]
mod tests;
