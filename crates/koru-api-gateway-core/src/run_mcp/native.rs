// SPDX-License-Identifier: MPL-2.0

//! Native catalog and server-owned identity. No provider fields choose a principal.
use koru_artifact_core::RunAuthorityRecord;
use koru_artifact_wire::run_tools::ToolFailure;
use koru_delegate_api::bots::{BotCommand, BotDelegatePort, BotReply};
use koru_delegate_core::RunToolAuthority;
use rmcp::model::{CallToolResult, Tool};
use serde_json::json;
use std::sync::Arc;
pub(super) mod children;
mod decision;
mod dispatch;
mod memory;
mod policy;
pub(super) mod private;
pub(super) mod routines;
pub(super) use dispatch::call;

/// Which wait, if any, the Run's current classification reports. A waiting
/// Run keeps its reads; its new mutations are refused with the wait's own
/// code (O147 for a decision, contract M6 4.6 for a child wait).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::run_mcp) enum RunWait {
    None,
    /// `RunToolAuthority::BotDecisionWait`: `decision_in_progress`.
    Decision,
    /// `RunToolAuthority::BotChildWait`: `child_wait_in_progress`.
    Child,
}
impl RunWait {
    pub(in crate::run_mcp) fn of(authority: &RunToolAuthority) -> Self {
        match authority {
            RunToolAuthority::Ordinary {} | RunToolAuthority::Bot { .. } => Self::None,
            RunToolAuthority::BotDecisionWait { .. } => Self::Decision,
            RunToolAuthority::BotChildWait { .. } => Self::Child,
        }
    }
    pub(super) fn waiting(self) -> bool {
        self != Self::None
    }
}

fn unavailable() -> ToolFailure {
    ToolFailure::new(
        "run_authority_unavailable",
        "The Run tool authority is unavailable.",
    )
}
pub(super) async fn classify(
    port: Option<Arc<dyn BotDelegatePort>>,
    record: &RunAuthorityRecord,
) -> Result<RunToolAuthority, ToolFailure> {
    let port = port.ok_or_else(unavailable)?;
    let command = BotCommand::ResolveRunAuthority {
        run: record.run,
        generation: record.generation,
    };
    let reply = tokio::task::spawn_blocking(move || port.execute(command))
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
    match reply {
        BotReply::RunAuthority {
            authority: RunToolAuthority::Ordinary {},
        } => Ok(RunToolAuthority::Ordinary {}),
        BotReply::RunAuthority { authority }
            if authority.bot_subject().is_some_and(|(provenance, policy)| {
                provenance.valid()
                    && policy.valid()
                    && !provenance.owner.user_scope.as_uuid().is_nil()
                    && !provenance.owner.device.as_uuid().is_nil()
                    && policy.workspace == record.workspace
                    && record.grants.len() == 1
                    && record.grants.contains(&record.workspace)
            }) =>
        {
            // Bot, or the same Bot while its decision wait is open (O147).
            Ok(authority)
        }
        _ => Err(unavailable()),
    }
}

#[cfg(test)]
pub(super) fn definitions() -> Vec<Tool> {
    definitions_for(false, children::ChildTools::NONE)
}

/// The Bot catalog for one Run. `private` says whether the private read is
/// listed for it ([`private::listed_for`]), and `children` which of the
/// three tools that ask another Bot are ([`children::listed_for`]): none of
/// them in this build, where both gates are closed.
pub(super) fn definitions_for(private: bool, children: children::ChildTools) -> Vec<Tool> {
    let mut tools: Vec<Tool> = koru_mcp_server::native::native_tool_catalog().into_iter().map(|tool| {
        let mutation=matches!(tool.name.as_str(),"create_document"|"edit_document");
        let mut schema=tool.input_schema;
        if mutation {
            let properties=schema["properties"].as_object_mut().expect("shared object schema");
            properties.insert("operation_id".into(),json!({"type":"string","format":"uuid","pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"}));
            let object=schema.as_object_mut().expect("shared object schema");
            object.entry("required").or_insert_with(||json!([])).as_array_mut().expect("required array").push(json!("operation_id"));
        }
        let description=if mutation {format!("{} Native Bot writes require a stable operation_id; retry the same operation with identical arguments. Creation is an ordinary page at the workspace root. Only a confirmed registered native result is Saved.",tool.description)} else {tool.description};
        serde_json::from_value(json!({
            "name":tool.name,"title":tool.title,"description":description,"inputSchema":schema,
            "annotations":{"readOnlyHint":tool.safety.read_only,"destructiveHint":tool.safety.destructive,"idempotentHint":tool.name != "edit_document" && tool.safety.idempotent,"openWorldHint":false}
        })).expect("shared tool definition")
    }).collect();
    if koru_ai_core::bots::memory::BOT_MEMORY_AVAILABLE {
        tools.extend(memory::definitions());
    }
    if koru_ai_core::bots::memory::BOT_MEMORY_WRITES_AVAILABLE {
        tools.extend(memory::write_definitions());
    }
    tools.extend(routines::definitions());
    if private {
        tools.extend(private::definitions());
    }
    tools.extend(children::definitions(children));
    tools
}

#[cfg(test)]
mod tests;
