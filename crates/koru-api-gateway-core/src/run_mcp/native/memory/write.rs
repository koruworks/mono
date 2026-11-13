// SPDX-License-Identifier: MPL-2.0
//! Receipt-first Memory writes. New effects stay closed until the next increment.
use super::super::dispatch;
use super::*;
use koru_ai_core::bots::memory::BOT_MEMORY_WRITES_AVAILABLE;
use koru_ai_server::{
    MemoryBaseEvidence, MemoryEditBaseRequest, MemoryWriteAuthorityEvidence,
    MemoryWriteAuthorityRequest, MemoryWriteSubjectEvidence, RunMemoryEnsureRequest,
    RunMemoryEnsureResult, RunMemoryWriteSubjectRequest, memory_write_fingerprint,
};
use koru_artifact_wire::run_tools::ToolFailure;
use koru_delegate_core::*;
use koru_mcp_server::native::memory::MemoryRecipeOperation;
use koru_mcp_server::native::{NativeWriteEvidence, NativeWriteKind};
use uuid::Uuid;
mod args;
#[cfg(test)]
mod tests;

pub(super) fn definitions() -> Vec<Tool> {
    args::definitions()
}
pub(super) fn recognizes(name: &str) -> bool {
    matches!(name, "remember_memory" | "correct_memory" | "forget_memory")
}
fn failure(code: &str) -> ToolFailure {
    let (code, message) = match code {
        "unsupported_capability" => (
            "unsupported_capability",
            "Bot Memory writes are not available yet.",
        ),
        "invalid_request" => (
            "invalid_request",
            "The Memory write arguments are invalid or exceed 24 KiB.",
        ),
        "invalid_memory_text" => (
            "invalid_memory_text",
            "Memory summary and detail must each be a bounded single paragraph. Use the ordinary editor for multiline content.",
        ),
        "memory_operation_conflict" | "idempotency_conflict" | "request_conflict" => (
            "idempotency_conflict",
            "This operation does not match its retained Memory request. Retry only the original operation and arguments.",
        ),
        "permission_denied" | "wrong_owner" | "forbidden" => (
            "permission_denied",
            "This Memory operation is not authorized.",
        ),
        "revision_conflict" => (
            "revision_conflict",
            "Memory changed since this operation's base. Read Current before requesting a new edit.",
        ),
        "native_request_refused" => (
            "native_request_refused",
            "The retained Memory operation was refused.",
        ),
        "native_recovery_pending" => (
            "native_recovery_pending",
            "Memory recovery is pending. Retry the same operation explicitly.",
        ),
        "save_status_unknown" => (
            "save_status_unknown",
            "The Memory write could not be confirmed. Retry only the same operation and arguments.",
        ),
        _ => (
            "memory_write_unavailable",
            "The Memory write is unavailable. Retry the same operation explicitly.",
        ),
    };
    ToolFailure::new(code, message)
}

/// All four production client signatures are pinned here. Fresh preparation is
/// wired by the following increment; no test feature enables the worker gate.
trait WritePort {
    fn subject(
        &self,
        request: RunMemoryWriteSubjectRequest,
    ) -> Result<MemoryWriteSubjectEvidence, ToolFailure>;
    #[expect(dead_code, reason = "staged fresh Memory base integration")]
    fn base(&self, request: MemoryEditBaseRequest) -> Result<MemoryBaseEvidence, ToolFailure>;
    #[expect(dead_code, reason = "staged lazy Memory creation integration")]
    fn ensure(&self, request: RunMemoryEnsureRequest)
    -> Result<RunMemoryEnsureResult, ToolFailure>;
    #[expect(dead_code, reason = "staged fresh Memory execution integration")]
    fn authority(
        &self,
        request: MemoryWriteAuthorityRequest,
    ) -> Result<MemoryWriteAuthorityEvidence, ToolFailure>;
    fn settled(&self);
    fn uncertain(
        &self,
        owner: BotOwner,
        bot: BotId,
        key: NativeResultKey,
    ) -> Result<(), ToolFailure>;
}
impl WritePort for AiWorkerClient {
    fn subject(
        &self,
        request: RunMemoryWriteSubjectRequest,
    ) -> Result<MemoryWriteSubjectEvidence, ToolFailure> {
        self.memory_write_subject(request)
            .map_err(|error| failure(&error.code))
    }
    fn base(&self, request: MemoryEditBaseRequest) -> Result<MemoryBaseEvidence, ToolFailure> {
        self.prepare_memory_edit_base(request)
            .map_err(|error| failure(&error.code))
    }
    fn ensure(
        &self,
        request: RunMemoryEnsureRequest,
    ) -> Result<RunMemoryEnsureResult, ToolFailure> {
        self.ensure_run_memory(request)
            .map_err(|error| failure(&error.code))
    }
    fn authority(
        &self,
        request: MemoryWriteAuthorityRequest,
    ) -> Result<MemoryWriteAuthorityEvidence, ToolFailure> {
        self.memory_write_authority(request)
            .map_err(|error| failure(&error.code))
    }
    fn settled(&self) {
        self.wake_native_policy();
    }
    fn uncertain(
        &self,
        owner: BotOwner,
        bot: BotId,
        key: NativeResultKey,
    ) -> Result<(), ToolFailure> {
        self.reconcile_native_operation(owner, bot, key)
            .map(|_| ())
            .map_err(|_| failure("native_recovery_pending"))
    }
}

fn result(value: Result<Value, ToolFailure>) -> CallToolResult {
    let response = match value {
        Ok(value) if serde_json::to_vec(&value).is_ok_and(|bytes| bytes.len() <= 4096) => {
            CallToolResult::structured(value)
        }
        Ok(_) => CallToolResult::structured_error(json!(failure("save_status_unknown"))),
        Err(error) => CallToolResult::structured_error(json!(error)),
    };
    if serde_json::to_vec(&response).is_ok_and(|bytes| bytes.len() <= BOT_MEMORY_RESPONSE_BYTES) {
        response
    } else {
        CallToolResult::structured_error(json!(failure("save_status_unknown")))
    }
}
fn available() -> Result<(), ToolFailure> {
    if BOT_MEMORY_WRITES_AVAILABLE {
        Ok(())
    } else {
        Err(failure("unsupported_capability"))
    }
}
pub(super) async fn call(
    state: &Arc<GatewayState>,
    record: Arc<RunAuthorityRecord>,
    provenance: BotProvenance,
    captured: CapturedBotPolicy,
    params: CallToolRequestParams,
) -> CallToolResult {
    if let Err(error) = available() {
        return result(Err(error));
    }
    let Some(port) = state.bot_delegate_port() else {
        return result(Err(failure("memory_write_unavailable")));
    };
    let client = state.ai.as_ref().and_then(|ai| {
        ai.worker
            .lock()
            .ok()?
            .as_ref()
            .map(|worker| worker.client())
    });
    let Some(client) = client else {
        return result(Err(failure("memory_write_unavailable")));
    };
    let cancellation = Cancellation::default();
    let cancel_on_failure = cancellation.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        invoke(
            &client,
            port.as_ref(),
            &record,
            provenance,
            captured,
            params.name.as_ref(),
            Value::Object(params.arguments.unwrap_or_default()),
            cancellation,
        )
    })
    .await;
    match outcome {
        Ok(value) => result(value),
        Err(_) => {
            cancel_on_failure.cancel();
            result(Err(failure("save_status_unknown")))
        }
    }
}
fn live(cancellation: &Cancellation) -> Result<(), ToolFailure> {
    if cancellation.is_cancelled() {
        Err(failure("memory_write_unavailable"))
    } else {
        Ok(())
    }
}

// Ordinary Result failures keep their established mapping. Memory's exact-body
// lookup distinguishes a known conflict from transport uncertainty.
fn owner_call(port: &dyn BotDelegatePort, command: BotCommand) -> Result<BotReply, ToolFailure> {
    port.execute(command).map_err(|error| {
        failure(match error.code.as_str() {
            "bot_conflict" => "idempotency_conflict",
            "bot_invalid" => "invalid_request",
            "bot_owner_mismatch" | "forbidden" => "permission_denied",
            "bot_fenced" | "bot_stale" => "native_request_refused",
            "bot_capacity" => "native_recovery_pending",
            _ => "memory_write_unavailable",
        })
    })
}

#[allow(clippy::too_many_arguments)]
fn invoke(
    client: &dyn WritePort,
    port: &dyn BotDelegatePort,
    record: &RunAuthorityRecord,
    provenance: BotProvenance,
    captured: CapturedBotPolicy,
    tool: &str,
    arguments: Value,
    cancellation: Cancellation,
) -> Result<Value, ToolFailure> {
    let args = args::parse(tool, arguments)?;
    live(&cancellation)?;
    if !provenance.valid()
        || !captured.valid()
        || captured.workspace != record.workspace
        || record.grants.len() != 1
        || !record.grants.contains(&record.workspace)
    {
        return Err(failure("permission_denied"));
    }
    let subject = MemoryEditSubjectV1 {
        version: 1,
        owner: provenance.owner,
        bot: provenance.bot,
        submission: provenance.submission,
        captured_config_revision: provenance.config_revision,
        action: args.action,
    };
    let current = client.subject(RunMemoryWriteSubjectRequest {
        run: record.run,
        generation: record.generation,
        provenance: provenance.clone(),
        captured,
        action: args.action,
        cancellation: cancellation.clone(),
    })?;
    if current.subject != subject
        || current.profile.as_uuid().is_nil()
        || current.config_revision < provenance.config_revision
        || current.config_revision > i64::MAX as u64
        || current.policy_revision == 0
        || current.policy_revision > i64::MAX as u64
    {
        return Err(failure("permission_denied"));
    }
    let hash = memory_write_fingerprint(
        &subject,
        record.run,
        args.operation_id,
        args.base,
        &args.operation,
    )
    .map_err(|error| failure(&error.code))?;
    live(&cancellation)?;
    let allocation = match owner_call(
        port,
        BotCommand::ReadMemoryEditAllocation {
            run: record.run,
            operation_id: args.operation_id,
            caller_fingerprint: hash,
        },
    )? {
        BotReply::Missing => return Err(failure("unsupported_capability")), // fresh path staged next
        BotReply::MemoryEditAllocation { allocation } => allocation,
        _ => return Err(failure("memory_write_unavailable")),
    };
    if !allocation.valid()
        || allocation.subject != subject
        || allocation.key.run != record.run
        || allocation.key.generation != record.generation
        || allocation.key.operation_id != args.operation_id
        || allocation.caller_fingerprint != hash
        || allocation.base != args.base
        || match &args.operation {
            MemoryRecipeOperation::Correct { fact, .. }
            | MemoryRecipeOperation::Forget { fact } => *fact != allocation.fact_id,
            _ => false,
        }
    {
        return Err(failure("memory_operation_conflict"));
    }
    live(&cancellation)?;
    let intent = match owner_call(
        port,
        BotCommand::ReconcileNativeResult {
            key: allocation.key.clone(),
        },
    )? {
        BotReply::Missing => return Err(failure("unsupported_capability")), // inert allocation, no lease
        BotReply::NativeResult { intent } => intent,
        _ => return Err(failure("memory_write_unavailable")),
    };
    validate_retained(&allocation, &intent, record.workspace)?;
    live(&cancellation)?;
    match intent.stage {
        NativeResultStage::Registered => receipt(&allocation, &intent),
        NativeResultStage::Verified => {
            let evidence = evidence(&intent)?;
            let registered = dispatch::confirm_record(port, &intent, evidence)
                .map_err(|_| failure("save_status_unknown"))?;
            client.settled();
            receipt(&allocation, &registered)
        }
        NativeResultStage::Refused => Err(failure("native_request_refused")),
        NativeResultStage::Prepared
        | NativeResultStage::Unconfirmed
        | NativeResultStage::SettledUnknown => {
            // No old history, second effect or fresh permission is needed to
            // request the existing recovery owner. An unleased row stays inert.
            client.uncertain(provenance.owner, provenance.bot, allocation.key)?;
            Err(failure("save_status_unknown"))
        }
    }
}
fn validate_retained(
    allocation: &MemoryEditAllocation,
    intent: &NativeResultIntentView,
    workspace: WorkspaceId,
) -> Result<(), ToolFailure> {
    let MemoryBaseResolution::Resolved { authority, .. } = &allocation.resolution else {
        return Err(failure("memory_operation_conflict"));
    };
    if authority.workspace != workspace
        || authority.subject() != allocation.subject
        || intent.key != allocation.key
        || intent.kind != NativeResultOperation::EditPage
        || intent.purpose
            != (NativeEffectPurpose::Memory {
                authority: authority.clone(),
            })
        || intent.destination != authority.workspace
        || intent.target != authority.memory_document
        || intent.store_request.get_version_num() != 4
        || intent.store_request.get_variant() != uuid::Variant::RFC4122
    {
        return Err(failure("memory_operation_conflict"));
    }
    Ok(())
}
fn evidence(intent: &NativeResultIntentView) -> Result<NativeWriteEvidence, ToolFailure> {
    let e = intent
        .verified
        .as_ref()
        .filter(|e| {
            e.valid()
                && e.kind == NativeResultKind::EditedPage
                && e.operation_id == intent.key.operation_id
                && e.workspace == intent.destination
                && e.document == intent.target
        })
        .ok_or_else(|| failure("save_status_unknown"))?;
    Ok(NativeWriteEvidence {
        kind: NativeWriteKind::Edited,
        workspace: e.workspace,
        document: e.document,
        version: e.version.clone(),
        title: e.title.clone(),
        confirmed_at_ms: e.confirmed_at_ms,
    })
}
fn receipt(
    allocation: &MemoryEditAllocation,
    intent: &NativeResultIntentView,
) -> Result<Value, ToolFailure> {
    if intent.stage != NativeResultStage::Registered {
        return Err(failure("save_status_unknown"));
    }
    evidence(intent)?;
    Ok(
        json!({"status":"saved", "operation_id":allocation.key.operation_id,
        "action":allocation.subject.action, "fact_id":allocation.fact_id}),
    )
}
