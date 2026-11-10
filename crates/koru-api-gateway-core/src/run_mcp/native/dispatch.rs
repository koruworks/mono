// SPDX-License-Identifier: MPL-2.0
//! Native tool orchestration; document semantics remain in the shared MCP owner.
use super::*;
use crate::GatewayState;
use koru_ai_server::{
    AiWorkerClient, NativePolicyEvidence, NativePolicyRequest, NativeStoreAuthority,
};
use koru_artifact_wire::run_tools::ToolFailure;
use koru_delegate_core::*;
use koru_mcp_server::native::{
    NativeReconciliation, NativeStoreIdentity, NativeWriteError, NativeWriteEvidence,
    NativeWriteKind, PreparedNativeWrite,
};
use koru_mcp_server::{RequestContext, ToolError, WorkspaceToolExecutor};
use koru_store_core::model::Scope;
use koru_store_server::StoreWorkerClient;
use rmcp::model::CallToolRequestParams;
use serde_json::Value;
use uuid::Uuid;

trait PolicySource {
    fn current(&self, request: NativePolicyRequest) -> Result<NativePolicyEvidence, ToolFailure>;
    fn settled(&self);
    fn uncertain(
        &self,
        _owner: BotOwner,
        _bot: BotId,
        _key: NativeResultKey,
    ) -> Result<(), ToolFailure> {
        Ok(())
    }
}
impl PolicySource for AiWorkerClient {
    fn current(&self, request: NativePolicyRequest) -> Result<NativePolicyEvidence, ToolFailure> {
        self.native_policy(request)
            .map_err(|_| failure("native_policy_unavailable"))
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
fn failure(code: &'static str) -> ToolFailure {
    ToolFailure::new(
        code,
        "The native document operation could not be confirmed. Inspect the result before continuing.",
    )
}
fn refused(error: ToolError) -> ToolFailure {
    let code = match error.code.as_str() {
        "permission_denied" | "forbidden" => "permission_denied",
        "revision_conflict" | "idempotency_conflict" => "revision_conflict",
        "native_no_change" => "native_no_change",
        "invalid_request" => "invalid_request",
        _ => "native_request_refused",
    };
    failure(code)
}
fn owner_call(port: &dyn BotDelegatePort, command: BotCommand) -> Result<BotReply, ToolFailure> {
    port.execute(command)
        .map_err(|_| failure("native_owner_unavailable"))
}

pub(in crate::run_mcp) async fn call(
    state: &Arc<GatewayState>,
    record: Arc<RunAuthorityRecord>,
    provenance: BotProvenance,
    captured: CapturedBotPolicy,
    params: CallToolRequestParams,
) -> CallToolResult {
    if matches!(params.name.as_ref(), "create_document" | "edit_document")
        && !state.native_mutations_ready()
    {
        return CallToolResult::structured_error(json!(failure("native_recovery_pending")));
    }
    let Some(port) = state.bot_delegate_port() else {
        return CallToolResult::structured_error(json!(failure("native_owner_unavailable")));
    };
    let client = state.ai.as_ref().and_then(|ai| {
        ai.worker
            .lock()
            .ok()?
            .as_ref()
            .map(|worker| worker.client())
    });
    let Some(client) = client else {
        return CallToolResult::structured_error(json!(failure("native_policy_unavailable")));
    };
    let store = state.store.client();
    let result = tokio::task::spawn_blocking(move || {
        invoke(
            &store,
            &client,
            port.as_ref(),
            &record,
            provenance,
            captured,
            params.name.as_ref(),
            Value::Object(params.arguments.unwrap_or_default()),
        )
    })
    .await;
    match result {
        Ok(Ok(value)) => CallToolResult::structured(value),
        Ok(Err(error)) => CallToolResult::structured_error(json!(error)),
        Err(_) => CallToolResult::structured_error(json!(failure("save_status_unknown"))),
    }
}

fn operation(arguments: &mut Value) -> Result<Uuid, ToolFailure> {
    let raw = arguments
        .as_object_mut()
        .and_then(|o| o.remove("operation_id"))
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or_else(|| failure("invalid_request"))?;
    let id = Uuid::parse_str(&raw).map_err(|_| failure("invalid_request"))?;
    if id.to_string() != raw
        || id.get_version() != Some(uuid::Version::Random)
        || id.get_variant() != uuid::Variant::RFC4122
    {
        return Err(failure("invalid_request"));
    }
    Ok(id)
}
fn fingerprint(raw: &str) -> Result<[u8; 32], ToolFailure> {
    if raw.len() != 64 || !raw.is_ascii() {
        return Err(failure("invalid_request"));
    }
    let mut result = [0; 32];
    for (index, value) in result.iter_mut().enumerate() {
        *value = u8::from_str_radix(&raw[index * 2..index * 2 + 2], 16)
            .map_err(|_| failure("invalid_request"))?;
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn invoke(
    store: &StoreWorkerClient,
    policy: &dyn PolicySource,
    port: &dyn BotDelegatePort,
    record: &RunAuthorityRecord,
    provenance: BotProvenance,
    captured: CapturedBotPolicy,
    tool: &str,
    mut arguments: Value,
) -> Result<Value, ToolFailure> {
    let mutation = match tool {
        "create_document" | "edit_document" => true,
        "search_documents" | "read_document" => false,
        _ => return Err(failure("unknown_tool")),
    };
    let operation_id = mutation.then(|| operation(&mut arguments)).transpose()?;
    let target = arguments
        .get("document_id")
        .map(|value| {
            value
                .as_str()
                .and_then(koru_ai_core::DocumentId::parse)
                .ok_or_else(|| failure("invalid_request"))
        })
        .transpose()?;
    let current = policy.current(NativePolicyRequest {
        run: record.run,
        provenance: provenance.clone(),
        captured: captured.clone(),
        target,
    })?;
    if current.barrier.key.owner != provenance.owner
        || current.barrier.key.bot != provenance.bot
        || current.barrier.policy.workspace != record.workspace
        || captured.workspace != record.workspace
    {
        return Err(failure("native_policy_unavailable"));
    }
    let authority = NativeStoreAuthority {
        owner_scope: provenance.owner.user_scope,
        expected_profile: current.profile,
        workspace: record.workspace,
    };
    let backend = koru_ai_server::native_store_backend(store.clone(), authority.clone());
    if !mutation {
        let request = RequestContext::selected(
            "local",
            "koru-bot-native",
            [Scope::ReadDocuments],
            [record.workspace],
        );
        return WorkspaceToolExecutor::new(backend)
            .invoke(&request, tool, arguments)
            .map_err(refused);
    }
    let prepared = backend
        .prepare_native_write(tool, arguments)
        .map_err(refused)?;
    let description = prepared.description();
    if description.workspace != record.workspace {
        return Err(failure("permission_denied"));
    }
    let expected_hash = fingerprint(&description.fingerprint)?;
    let submission = BotSubmissionKey {
        owner: provenance.owner,
        bot: provenance.bot,
        request: provenance.submission,
    };
    let fence = match owner_call(
        port,
        BotCommand::Reconcile {
            key: submission.clone(),
        },
    )? {
        BotReply::Admission { admission }
            if admission.key == submission && admission.run == record.run =>
        {
            admission.fence
        }
        _ => None,
    }
    .ok_or_else(|| failure("native_execution_unavailable"))?;
    if fence.owner != provenance.owner
        || fence.bot != provenance.bot
        || fence.run != record.run
        || fence.generation != record.generation
    {
        return Err(failure("native_execution_unavailable"));
    }
    let key = NativeResultKey {
        run: fence.run,
        attempt: fence.attempt,
        generation: fence.generation,
        turn: fence
            .turn
            .ok_or_else(|| failure("native_execution_unavailable"))?,
        operation_id: operation_id.expect("mutation"),
    };
    let retained = match owner_call(port, BotCommand::ReconcileNativeResult { key: key.clone() })? {
        BotReply::Missing => None,
        BotReply::NativeResult { intent } => {
            validate_intent(&intent, &key, &prepared, expected_hash)?;
            Some(intent)
        }
        _ => return Err(failure("native_owner_unavailable")),
    };
    if let Some(intent) = &retained {
        if intent.stage == NativeResultStage::Registered {
            return saved(store, &authority, intent);
        }
        if intent.stage == NativeResultStage::Refused {
            return Err(failure("native_request_refused"));
        }
        if intent.stage == NativeResultStage::Verified {
            // Durable evidence belongs to the owner, including edits whose
            // transport cannot independently prove a second Store outcome.
            let evidence = intent
                .verified
                .as_ref()
                .filter(|e| {
                    e.valid()
                        && e.operation_id == key.operation_id
                        && e.workspace == intent.destination
                        && e.document == intent.target
                })
                .ok_or_else(|| failure("save_status_unknown"))?;
            return confirm(
                port,
                policy,
                store,
                &authority,
                intent,
                NativeWriteEvidence {
                    kind: match evidence.kind {
                        NativeResultKind::CreatedPage => NativeWriteKind::Created,
                        NativeResultKind::EditedPage => NativeWriteKind::Edited,
                    },
                    workspace: evidence.workspace,
                    document: evidence.document,
                    version: evidence.version.clone(),
                    title: evidence.title.clone(),
                    confirmed_at_ms: evidence.confirmed_at_ms,
                },
            );
        }
        // Recover original Store proof before asking for fresh execution rights.
        // This can register a held creation after an access-narrowing barrier.
        match backend.reconcile_native_write(&prepared, store_identity(&provenance, intent)) {
            Ok(NativeReconciliation::Verified(evidence)) => {
                return confirm(port, policy, store, &authority, intent, evidence);
            }
            Ok(NativeReconciliation::Unconfirmed)
                if intent.stage == NativeResultStage::Prepared => {}
            _ => return Err(failure("save_status_unknown")),
        }
    }
    match owner_call(
        port,
        BotCommand::SynchronizePolicy {
            barrier: current.barrier.clone(),
        },
    )? {
        BotReply::PolicyBarrier { receipt }
            if receipt.barrier == current.barrier
                && receipt.state == BotPolicyBarrierState::Effective => {}
        _ => return Err(failure("native_policy_pending")),
    }
    match owner_call(
        port,
        BotCommand::EffectFence {
            fence: fence.clone(),
        },
    )? {
        BotReply::EffectFence {
            fence: observed,
            policy,
        } if observed == fence && policy == captured => {}
        _ => return Err(failure("native_execution_unavailable")),
    }
    let title = match description.document {
        Some(document) => {
            koru_ai_server::native_document_title(store.clone(), authority.clone(), document)
                .map_err(refused)?
        }
        None => description.title.clone(),
    };
    let request = PrepareNativeResult {
        key: key.clone(),
        kind: match description.kind {
            NativeWriteKind::Created => NativeResultOperation::CreatePage,
            NativeWriteKind::Edited => NativeResultOperation::EditPage,
        },
        destination: description.workspace,
        target: description.document,
        request_fingerprint: expected_hash,
        expected_revision: description.expected_revision.clone(),
        config_revision: current.barrier.config_revision,
        policy_revision: current.barrier.policy_revision,
        title,
    };
    let intent = match owner_call(port, BotCommand::PrepareNativeResult { request })? {
        BotReply::NativeResult { intent } => intent,
        _ => return Err(failure("save_status_unknown")),
    };
    validate_intent(&intent, &key, &prepared, expected_hash)?;
    if intent.disposition != NativeResultDisposition::Execute {
        return if intent.stage == NativeResultStage::Registered {
            saved(store, &authority, &intent)
        } else {
            Err(failure("save_status_unknown"))
        };
    }
    if intent.stage != NativeResultStage::Prepared || intent.verified.is_some() {
        return Err(failure("native_operation_conflict"));
    }
    // No AI call from here until the owner proves the lease is settled.
    match backend.execute_native_write(&prepared, store_identity(&provenance, &intent)) {
        Ok(evidence) => confirm(port, policy, store, &authority, &intent, evidence),
        Err(NativeWriteError::Refused(error)) => {
            let observed = owner_call(
                port,
                BotCommand::ConfirmNativeResult {
                    request: ConfirmNativeResult {
                        key: key.clone(),
                        request_fingerprint: expected_hash,
                        observation: NativeResultObservation::NotApplied {
                            code: match error.code.as_str() {
                                "permission_denied" | "forbidden" => {
                                    NativeResultRefusal::AccessDenied
                                }
                                "revision_conflict" | "idempotency_conflict" => {
                                    NativeResultRefusal::Conflict
                                }
                                _ => NativeResultRefusal::InvalidInput,
                            },
                        },
                    },
                },
            )?;
            if !matches!(observed, BotReply::NativeResult { intent } if intent.stage == NativeResultStage::Refused && intent.key == key && intent.request_fingerprint == expected_hash)
            {
                return Err(failure("save_status_unknown"));
            }
            policy.settled();
            Err(refused(error))
        }
        Err(NativeWriteError::Unconfirmed(_)) => {
            let _ = owner_call(
                port,
                BotCommand::ConfirmNativeResult {
                    request: ConfirmNativeResult {
                        key: key.clone(),
                        request_fingerprint: expected_hash,
                        observation: NativeResultObservation::Unconfirmed,
                    },
                },
            );
            policy.uncertain(provenance.owner, provenance.bot, key)?;
            Err(failure("save_status_unknown"))
        }
    }
}

fn validate_intent(
    intent: &NativeResultIntentView,
    key: &NativeResultKey,
    prepared: &PreparedNativeWrite,
    hash: [u8; 32],
) -> Result<(), ToolFailure> {
    let d = prepared.description();
    let kind = match d.kind {
        NativeWriteKind::Created => NativeResultOperation::CreatePage,
        NativeWriteKind::Edited => NativeResultOperation::EditPage,
    };
    if intent.key != *key
        || intent.destination != d.workspace
        || intent.kind != kind
        || intent.request_fingerprint != hash
        || d.document.is_some_and(|document| document != intent.target)
        || intent.target.as_uuid().is_nil()
        || intent.store_request.get_version_num() != 4
        || intent.store_request.as_bytes()[8] & 0xc0 != 0x80
    {
        return Err(failure("native_operation_conflict"));
    }
    Ok(())
}
fn store_identity(
    provenance: &BotProvenance,
    intent: &NativeResultIntentView,
) -> NativeStoreIdentity {
    NativeStoreIdentity {
        candidate: intent.target,
        scope: format!(
            "koru.bot.native.v1:{}:{}:{}",
            provenance.owner.user_scope, provenance.bot, intent.key.run
        ),
        request_key: intent.store_request.to_string(),
    }
}
fn confirm(
    port: &dyn BotDelegatePort,
    policy: &dyn PolicySource,
    store: &StoreWorkerClient,
    authority: &NativeStoreAuthority,
    intent: &NativeResultIntentView,
    evidence: NativeWriteEvidence,
) -> Result<Value, ToolFailure> {
    let reference = NativeResultReference {
        operation_id: intent.key.operation_id,
        workspace: evidence.workspace,
        document: evidence.document,
        version: evidence.version,
        title: evidence.title,
        confirmed_at_ms: evidence.confirmed_at_ms,
        kind: match evidence.kind {
            NativeWriteKind::Created => NativeResultKind::CreatedPage,
            NativeWriteKind::Edited => NativeResultKind::EditedPage,
        },
    };
    let observed = match owner_call(
        port,
        BotCommand::ConfirmNativeResult {
            request: ConfirmNativeResult {
                key: intent.key.clone(),
                request_fingerprint: intent.request_fingerprint,
                observation: NativeResultObservation::Durable {
                    evidence: reference.clone(),
                },
            },
        },
    )? {
        BotReply::NativeResult { intent: observed }
            if observed.key == intent.key
                && observed.request_fingerprint == intent.request_fingerprint
                && observed.destination == intent.destination
                && observed.target == intent.target
                && observed.kind == intent.kind
                && observed.verified.as_ref() == Some(&reference)
                && observed.stage == NativeResultStage::Registered =>
        {
            observed
        }
        _ => return Err(failure("save_status_unknown")),
    };
    policy.settled();
    saved(store, authority, &observed)
}
fn saved(
    store: &StoreWorkerClient,
    authority: &NativeStoreAuthority,
    intent: &NativeResultIntentView,
) -> Result<Value, ToolFailure> {
    let evidence = intent
        .verified
        .as_ref()
        .filter(|e| {
            e.valid()
                && e.operation_id == intent.key.operation_id
                && e.workspace == intent.destination
                && e.document == intent.target
        })
        .ok_or_else(|| failure("save_status_unknown"))?;
    // Even an old Saved receipt cannot expose a now-hidden page.
    koru_ai_server::native_document_title(store.clone(), authority.clone(), evidence.document)
        .map_err(refused)?;
    Ok(
        json!({"status":"saved","operation_id":evidence.operation_id,"workspace_id":evidence.workspace,
        "document_id":evidence.document,"title":evidence.title,
        "base_revision":evidence.version.iter().map(|b| format!("{b:02x}")).collect::<String>(),"kind":evidence.kind}),
    )
}

#[cfg(test)]
mod tests;
