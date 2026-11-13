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
    Find(FindLists),
}
fn arguments(call: &AiToolCall) -> Result<Arguments, AiError> {
    let bytes = mini_apps::bounded_json(&call.arguments).map_err(|_| invalid())?;
    match call.name.as_str() {
        mini_apps::CREATE_TOOL => Ok(Arguments::Create(
            Declaration::parse(&bytes).map_err(|_| invalid())?,
        )),
        mini_apps::LIST_TOOL => {
            let request: FindLists =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            if request.query.chars().count() > 128
                || request.query.contains('\0')
                || !(1..=20).contains(&request.limit)
                || request.offset > 1_000_000
            {
                return Err(invalid());
            }
            Ok(Arguments::Find(request))
        }
        _ => Err(unavailable()),
    }
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
        let needs_lists = match &args {
            Arguments::Find(_) => true,
            Arguments::Create(d) => matches!(d.selection, TaskSelection::List(_)),
        };
        let needs_document = matches!(&args, Arguments::Create(d) if d.document.is_some());
        let registry = self.registry.read(context, needs_lists, needs_document)?;
        if cancellation.is_cancelled() {
            authority.revoke();
            return Err(unavailable());
        }
        if !authority.live() {
            return Err(unavailable());
        }
        let content = match args {
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
                    if let TaskSelection::List(id) = declaration.selection
                        && !registry.lists.iter().any(|(list, _)| *list == id)
                    {
                        return Err(invalid());
                    }
                    if let Some(id) = declaration.document
                        && !registry.documents.contains(&id)
                    {
                        return Err(invalid());
                    }
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
    if result.name != mini_apps::CREATE_TOOL
        || result.is_error
        || result.call_id.is_empty()
        || result.call_id.len() > 256
    {
        return Err(invalid());
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
