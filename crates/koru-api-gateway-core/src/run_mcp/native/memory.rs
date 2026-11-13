// SPDX-License-Identifier: MPL-2.0

//! Bot-only Memory reads. The worker owns history, provenance and generation checks.
use super::*;
use crate::GatewayState;
use koru_ai_core::Cancellation;
use koru_ai_core::bots::memory::{BOT_MEMORY_AVAILABLE, BOT_MEMORY_RESPONSE_BYTES};
use koru_ai_core::bots::{BotMemoryNodeId, MemoryReadOperation, MemoryView};
use koru_ai_server::{
    AiWorkerClient, NativePolicyEvidence, NativePolicyRequest, RunMemoryReadRequest,
    RunMemoryReadResult,
};
use koru_delegate_core::{BotProvenance, CapturedBotPolicy};
use rmcp::model::CallToolRequestParams;
use serde::Deserialize;
use serde_json::Value;
mod write;

pub(super) fn write_definitions() -> Vec<Tool> {
    write::definitions()
}

pub(super) fn recognizes(name: &str) -> bool {
    matches!(name, "list_memory" | "read_memory") || write::recognizes(name)
}
fn captured() -> MemoryView {
    MemoryView::Captured
}
fn limit() -> u16 {
    20
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default = "captured")]
    view: MemoryView,
    cursor: Option<String>,
    #[serde(default = "limit")]
    limit: u16,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DetailArgs {
    #[serde(default = "captured")]
    view: MemoryView,
    fact_id: BotMemoryNodeId,
    cursor: Option<String>,
}
fn failure(code: &str) -> ToolFailure {
    let (code, message) = match code {
        "unsupported_capability" => ("unsupported_capability", "Bot Memory is not available yet."),
        "invalid_request" => ("invalid_request", "The Memory read arguments are invalid."),
        "memory_capture_unavailable" => (
            "memory_capture_unavailable",
            "This task has no available Memory capture.",
        ),
        "memory_history_unavailable" => (
            "memory_history_unavailable",
            "The requested Memory snapshot is unavailable.",
        ),
        "memory_cursor_expired" => (
            "memory_cursor_expired",
            "This Memory read expired. Explicitly start a new read.",
        ),
        "invalid_memory_cursor" => (
            "invalid_memory_cursor",
            "The Memory continuation is invalid. Explicitly start a new read.",
        ),
        "memory_detail_unsupported" => (
            "memory_detail_unsupported",
            "This Memory detail cannot be represented.",
        ),
        "context_too_large" => (
            "context_too_large",
            "This Memory page exceeds the read limit. Open the Memory document to read it.",
        ),
        "memory_detail_too_large" => (
            "memory_detail_too_large",
            "This Memory detail exceeds the read limit.",
        ),
        "memory_fact_not_found" => (
            "memory_fact_not_found",
            "The fact is not in this Memory snapshot.",
        ),
        "forbidden" | "permission_denied" => {
            ("permission_denied", "This Memory read is not authorized.")
        }
        _ => (
            "memory_read_unavailable",
            "The Memory read is unavailable. Try again explicitly.",
        ),
    };
    ToolFailure::new(code, message)
}
fn parse(name: &str, arguments: Value) -> Result<MemoryReadOperation, ToolFailure> {
    let operation = match name {
        "list_memory" => {
            let a: ListArgs =
                serde_json::from_value(arguments).map_err(|_| failure("invalid_request"))?;
            MemoryReadOperation::List {
                view: a.view,
                cursor: a.cursor,
                limit: a.limit,
            }
        }
        "read_memory" => {
            let a: DetailArgs =
                serde_json::from_value(arguments).map_err(|_| failure("invalid_request"))?;
            MemoryReadOperation::Detail {
                view: a.view,
                fact_id: a.fact_id,
                cursor: a.cursor,
            }
        }
        _ => return Err(failure("invalid_request")),
    };
    operation
        .validate_bounds()
        .map_err(|_| failure("invalid_request"))?;
    Ok(operation)
}
pub(super) fn definitions() -> Vec<Tool> {
    [false, true].into_iter().map(|detail| {
        let mut properties = json!({
            "view":{"type":"string","enum":["captured","current"],"default":"captured"},
            "cursor":{"type":"string","minLength":1,"maxLength":2048,"pattern":"^[A-Za-z0-9_-]+$"}
        });
        let required = if detail {
            properties["fact_id"] = json!({"type":"string","format":"uuid","pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"});
            vec!["fact_id"]
        } else {
            properties["limit"] = json!({"type":"integer","minimum":1,"maximum":20,"default":20});
            vec![]
        };
        serde_json::from_value(json!({
            "name": if detail {"read_memory"} else {"list_memory"},
            "title": if detail {"Read Memory fact"} else {"List Memory facts"},
            "description": if detail {"Read one fact from this Bot's Memory. Defaults to the task's captured snapshot. Continuations stay on that exact snapshot; an explicit Current first read observes a new snapshot."} else {"List this Bot's Memory facts. Defaults to the task's captured snapshot. Current explicitly observes a fresh snapshot. Continue only with the returned cursor; omitted prompt facts remain readable."},
            "inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
            "annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}
        })).expect("fixed Memory tool schema")
    }).collect()
}
fn result(value: Result<RunMemoryReadResult, ToolFailure>) -> CallToolResult {
    let response = match value {
        Ok(RunMemoryReadResult::List(page)) => CallToolResult::structured(json!(page)),
        Ok(RunMemoryReadResult::Detail(page)) => CallToolResult::structured(json!(page)),
        Err(error) => return CallToolResult::structured_error(json!(error)),
    };
    if serde_json::to_vec(&response).is_ok_and(|bytes| bytes.len() <= BOT_MEMORY_RESPONSE_BYTES) {
        response
    } else {
        CallToolResult::structured_error(json!(failure("memory_detail_too_large")))
    }
}

pub(super) async fn call(
    state: &Arc<GatewayState>,
    record: Arc<RunAuthorityRecord>,
    provenance: BotProvenance,
    policy: CapturedBotPolicy,
    params: CallToolRequestParams,
) -> CallToolResult {
    if write::recognizes(params.name.as_ref()) {
        return write::call(state, record, provenance, policy, params).await;
    }
    if !BOT_MEMORY_AVAILABLE {
        return result(Err(failure("unsupported_capability")));
    }
    let operation = match parse(
        params.name.as_ref(),
        Value::Object(params.arguments.unwrap_or_default()),
    ) {
        Ok(operation) => operation,
        Err(error) => return result(Err(error)),
    };
    let client = state.ai.as_ref().and_then(|ai| {
        ai.worker
            .lock()
            .ok()?
            .as_ref()
            .map(|worker| worker.client())
    });
    let Some(client) = client else {
        return result(Err(failure("memory_read_unavailable")));
    };
    let cancellation = Cancellation::default();
    let cancel_on_failure = cancellation.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        invoke(
            &client,
            &record,
            provenance,
            policy,
            operation,
            cancellation,
        )
    })
    .await;
    match outcome {
        Ok(value) => result(value),
        Err(_) => {
            cancel_on_failure.cancel();
            result(Err(failure("memory_read_unavailable")))
        }
    }
}
trait ReadPort {
    fn policy(&self, request: NativePolicyRequest) -> Result<NativePolicyEvidence, String>;
    fn read(&self, request: RunMemoryReadRequest) -> Result<RunMemoryReadResult, String>;
}
impl ReadPort for AiWorkerClient {
    fn policy(&self, request: NativePolicyRequest) -> Result<NativePolicyEvidence, String> {
        self.native_policy(request).map_err(|error| error.code)
    }
    fn read(&self, request: RunMemoryReadRequest) -> Result<RunMemoryReadResult, String> {
        self.read_bot_memory(request).map_err(|error| error.code)
    }
}
fn invoke(
    client: &dyn ReadPort,
    record: &RunAuthorityRecord,
    provenance: BotProvenance,
    captured: CapturedBotPolicy,
    operation: MemoryReadOperation,
    cancellation: Cancellation,
) -> Result<RunMemoryReadResult, ToolFailure> {
    let current = client
        .policy(NativePolicyRequest {
            run: record.run,
            provenance: provenance.clone(),
            captured: captured.clone(),
            target: None,
        })
        .map_err(|error| failure(&error))?;
    if current.barrier.key.owner != provenance.owner
        || current.barrier.key.bot != provenance.bot
        || current.barrier.policy.workspace != record.workspace
        || captured.workspace != record.workspace
    {
        return Err(failure("forbidden"));
    }
    let cancel_on_failure = cancellation.clone();
    let value = client.read(RunMemoryReadRequest {
        run: record.run,
        generation: record.generation,
        provenance,
        captured,
        cancellation,
        operation,
    });
    value.map_err(|error| {
        cancel_on_failure.cancel();
        failure(&error)
    })
}

#[cfg(test)]
mod tests;
