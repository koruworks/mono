// SPDX-License-Identifier: MPL-2.0

//! Native catalog and server-owned identity. No provider fields choose a principal.
use koru_artifact_core::RunAuthorityRecord;
use koru_artifact_wire::run_tools::ToolFailure;
use koru_delegate_api::bots::{BotCommand, BotDelegatePort, BotReply};
use koru_delegate_core::RunToolAuthority;
use rmcp::model::{CallToolResult, Tool};
use serde_json::json;
use std::sync::Arc;
mod dispatch;
mod memory;
mod policy;
pub(super) use dispatch::call;

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
        BotReply::RunAuthority {
            authority: RunToolAuthority::Bot { provenance, policy },
        } if provenance.valid()
            && policy.valid()
            && !provenance.owner.user_scope.as_uuid().is_nil()
            && !provenance.owner.device.as_uuid().is_nil()
            && policy.workspace == record.workspace
            && record.grants.len() == 1
            && record.grants.contains(&record.workspace) =>
        {
            Ok(RunToolAuthority::Bot { provenance, policy })
        }
        _ => Err(unavailable()),
    }
}

pub(super) fn definitions() -> Vec<Tool> {
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
    tools
}

#[cfg(test)]
mod tests;
