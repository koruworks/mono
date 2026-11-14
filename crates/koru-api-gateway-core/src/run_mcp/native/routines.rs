// SPDX-License-Identifier: MPL-2.0
//! Four own-Bot setup tools. Arguments cannot select an authority subject.
use super::*;
use crate::GatewayState;
use koru_ai_core::bots::{
    BotRequestId, BotRoutineCreationObserved, RoutineRecurrenceRule, RoutineScheduleName,
    validate_routine_list_bytes,
};
use koru_ai_core::{Cancellation, DocumentId};
use koru_ai_server::{
    AiWorkerClient, RunBotSubject, RunRoutineCreate, RunRoutineOperation, RunRoutineRequest,
    RunRoutineResult,
};
use koru_delegate_core::{BotProvenance, CapturedBotPolicy, Objective, ScheduleId};
use rmcp::model::CallToolRequestParams;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::time::{Duration, Instant};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Create {
    request_id: BotRequestId,
    #[schemars(range(min = 1, max = 9223372036854775807u64))]
    expected_bot_revision: u64,
    name: RoutineScheduleName,
    objective: Objective,
    #[schemars(length(max = 16))]
    documents: Vec<DocumentId>,
    recurrence: RoutineRecurrenceRule,
}
#[cfg(test)]
mod tests;
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Read {
    request_id: BotRequestId,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Retry {
    original_request_id: BotRequestId,
    observed_token: BotRoutineCreationObserved,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct List {
    after_schedule: Option<ScheduleId>,
    #[serde(default = "default_limit")]
    #[schemars(range(min = 1, max = 16))]
    limit: u8,
}
fn default_limit() -> u8 {
    16
}
pub(in crate::run_mcp) fn recognizes(name: &str) -> bool {
    matches!(
        name,
        "create_routine" | "read_routine_creation" | "retry_routine_creation" | "list_routines"
    )
}
pub(in crate::run_mcp) fn mutation(name: &str) -> bool {
    matches!(name, "create_routine" | "retry_routine_creation")
}
pub(in crate::run_mcp) fn failure(code: &'static str) -> ToolFailure {
    ToolFailure::new(
        code,
        if code == "save_status_unknown" {
            "The Routine operation may have been accepted. Read creation state with the original request identity before retrying; do not use a new request ID."
        } else {
            "The Routine operation could not be completed."
        },
    )
}
pub(in crate::run_mcp) fn error(code: &'static str) -> CallToolResult {
    CallToolResult::structured_error(json!(failure(code)))
}
fn parse(name: &str, value: Value) -> Result<RunRoutineOperation, ToolFailure> {
    validate_routine_list_bytes(&value).map_err(|_| failure("context_too_large"))?;
    let invalid = || failure("invalid_request");
    Ok(match name {
        "create_routine" => {
            // Refuse the count before building another complete typed copy.
            if value
                .get("documents")
                .and_then(Value::as_array)
                .is_none_or(|d| d.len() > 16)
            {
                return Err(invalid());
            }
            let a: Create = serde_json::from_value(value).map_err(|_| invalid())?;
            RunRoutineOperation::Create(RunRoutineCreate {
                request_id: a.request_id,
                expected_bot_revision: a.expected_bot_revision,
                name: a.name,
                objective: a.objective,
                documents: a.documents,
                recurrence: a.recurrence,
            })
        }
        "read_routine_creation" => {
            let a: Read = serde_json::from_value(value).map_err(|_| invalid())?;
            RunRoutineOperation::ReadCreation {
                request_id: a.request_id,
            }
        }
        "retry_routine_creation" => {
            let a: Retry = serde_json::from_value(value).map_err(|_| invalid())?;
            RunRoutineOperation::RetryCreation {
                original_request_id: a.original_request_id,
                observed_token: a.observed_token,
            }
        }
        "list_routines" => {
            let a: List = serde_json::from_value(value).map_err(|_| invalid())?;
            if !(1..=16).contains(&a.limit) {
                return Err(invalid());
            }
            RunRoutineOperation::List {
                after_schedule: a.after_schedule,
                limit: a.limit,
            }
        }
        _ => return Err(invalid()),
    })
}
pub(super) fn definitions() -> Vec<Tool> {
    [
        ("create_routine", "Create a Paused Routine", "Create an initially Paused Routine for this Bot using its canonical configuration and a separate copied grant. Obtain expected_bot_revision from list_routines. Reuse the exact request ID and body after an unknown reply; read creation status before retrying. This never enables or runs the Routine.", schemars::schema_for!(Create), false, true),
        ("read_routine_creation", "Read Routine creation", "Read this Bot's retained creation status. It does not report current Schedule lifecycle or permission. can_retry is a conservative eligibility hint, not authority or availability.", schemars::schema_for!(Read), true, true),
        ("retry_routine_creation", "Retry Routine creation", "Reconcile this Bot's existing pending creation using the last observed token. No new candidate is allocated. After a lost reply, read creation state before choosing another token.", schemars::schema_for!(Retry), false, false),
        ("list_routines", "List this Bot's Routines", "Read a bounded page of current applied Schedule observations, configured model, retained grants and recent local conversations. Unavailable and NotLive are explicit; these observations are not execution or permission authority. Continue with next_schedule.", schemars::schema_for!(List), true, true),
    ].into_iter().map(|(name,title,description,schema,read,idempotent)| serde_json::from_value(json!({
        "name":name,"title":title,"description":description,"inputSchema":schema,
        "annotations":{"readOnlyHint":read,"destructiveHint":false,"idempotentHint":idempotent,"openWorldHint":false}
    })).expect("fixed Routine tool schema")).collect()
}
struct CancelOnDrop(Cancellation);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn invoke(
    client: AiWorkerClient,
    subject: RunBotSubject,
    operation: RunRoutineOperation,
) -> CallToolResult {
    let cancellation = Cancellation::default();
    // This guard belongs to the async future, not the surviving blocking wait.
    let _cancel = CancelOnDrop(cancellation.clone());
    let mutating = matches!(
        operation,
        RunRoutineOperation::Create(_) | RunRoutineOperation::RetryCreation { .. }
    );
    let request = RunRoutineRequest {
        subject,
        operation,
        cancellation,
        deadline: Instant::now() + Duration::from_secs(30),
    };
    match tokio::task::spawn_blocking(move || client.routine_for_run(request)).await {
        Ok(Ok(value)) => result(value, mutating),
        Ok(Err(refused)) => CallToolResult::structured_error(json!(ToolFailure::new(
            refused.code,
            if mutating {
                "The Routine request was not confirmed. If acceptance is unknown, read creation state with the original request identity before retrying."
            } else {
                "The Routine request could not be completed."
            }
        ))),
        Err(_) => error(if mutating {
            "save_status_unknown"
        } else {
            "workspace_unavailable"
        }),
    }
}
fn result(value: RunRoutineResult, mutating: bool) -> CallToolResult {
    fn encoded<T: serde::Serialize>(value: &T) -> Option<Value> {
        validate_routine_list_bytes(value).ok()?;
        serde_json::to_value(value).ok()
    }
    let value = match value {
        RunRoutineResult::Created(v) => encoded(&v),
        RunRoutineResult::Creation(v) => encoded(&v),
        RunRoutineResult::Routines(v) => encoded(&v),
    };
    match value {
        Some(value) => CallToolResult::structured(value),
        None => error(if mutating {
            "save_status_unknown"
        } else {
            "context_too_large"
        }),
    }
}
pub(super) async fn call(
    state: &Arc<GatewayState>,
    record: Arc<RunAuthorityRecord>,
    provenance: BotProvenance,
    captured: CapturedBotPolicy,
    params: CallToolRequestParams,
) -> CallToolResult {
    let operation = match parse(
        params.name.as_ref(),
        Value::Object(params.arguments.unwrap_or_default()),
    ) {
        Ok(operation) => operation,
        Err(error) => return CallToolResult::structured_error(json!(error)),
    };
    let client = state.ai.as_ref().and_then(|ai| {
        ai.worker
            .lock()
            .ok()?
            .as_ref()
            .map(|worker| worker.client())
    });
    let Some(client) = client else {
        return error("workspace_unavailable");
    };
    if captured.workspace != record.workspace
        || record.grants.len() != 1
        || !record.grants.contains(&record.workspace)
    {
        return error("permission_denied");
    }
    invoke(
        client,
        RunBotSubject {
            run: record.run,
            generation: record.generation,
            provenance,
            captured,
        },
        operation,
    )
    .await
}
