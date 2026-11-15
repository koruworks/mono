// SPDX-License-Identifier: MPL-2.0
//! Native tool orchestration; document semantics remain in the shared MCP owner.
use super::decision::EditDecisions;
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
pub(super) fn failure(code: &'static str) -> ToolFailure {
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
    if super::routines::recognizes(params.name.as_ref()) {
        return super::routines::call(state, record, provenance, captured, params).await;
    }
    if super::memory::recognizes(params.name.as_ref()) {
        return super::memory::call(state, record, provenance, captured, params).await;
    }
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
            EditDecisions::production(),
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
pub(super) fn fingerprint(raw: &str) -> Result<[u8; 32], ToolFailure> {
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
    decisions: EditDecisions,
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
    if !super::policy::matches(&current.proof, &provenance, &captured, record.workspace) {
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
    // The exact denied arguments, retained only if this edit can become a
    // decision. Computed before the shared builder consumes them.
    let pacing = match decisions {
        EditDecisions::Open(pacing) if tool == "edit_document" => Some(pacing),
        _ => None,
    };
    let payload = pacing.and_then(|_| super::decision::payload(&arguments));
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
    // A typed selected-page denial that may become a decision. Computed from
    // the classification's captured policy and the current AI proof; the
    // owner repeats the predicate authoritatively.
    let denial = pacing.and(payload.as_ref()).and_then(|_| {
        super::decision::selected_page_denial(
            &provenance,
            &captured,
            &current.proof,
            record.workspace,
            description,
        )
    });
    let retained = match port.execute(BotCommand::ReconcileNativeResult { key: key.clone() }) {
        Ok(BotReply::Missing) => None,
        Ok(BotReply::NativeResult { intent }) => {
            validate_intent(&intent, &key, &prepared, expected_hash)?;
            Some(intent)
        }
        // While a decision owns the Run's publication the owner cannot prove
        // absence; only the decision path's own exact replay can answer.
        Err(error) if denial.is_some() && error.code == "publication_pending" => None,
        _ => return Err(failure("native_owner_unavailable")),
    };
    if let Some(intent) = &retained
        && let Some(answer) = recover_retained(
            port,
            policy,
            store,
            &backend,
            &authority,
            &provenance,
            &prepared,
            intent,
        )
    {
        return answer;
    }
    let mut synchronized = false;
    if retained.is_none()
        && let (Some(denial), Some(payload), Some(pacing)) = (denial, payload.as_ref(), pacing)
    {
        let edit = super::decision::DeniedEdit {
            fence: &fence,
            key: &key,
            denial,
            fingerprint: expected_hash,
            expected_revision: description.expected_revision.as_deref().unwrap_or_default(),
            payload,
        };
        let outcome = super::decision::decide(
            port,
            provenance.owner,
            &edit,
            &current.proof,
            pacing,
            || {
                synchronized = true;
                fresh_fence(port, &current.proof, &fence, &captured)
            },
        )?;
        if let super::decision::DecisionOutcome::Consume { view, boot } = outcome {
            return consume(
                store,
                policy,
                port,
                &backend,
                &authority,
                &provenance,
                &prepared,
                &edit,
                &view,
                &current.proof,
                boot,
            );
        }
    }
    if !synchronized {
        fresh_fence(port, &current.proof, &fence, &captured)?;
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
        config_revision: current.proof.global.config_revision,
        policy_revision: current.proof.global.policy_revision,
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
    let registered = execute_record(
        port,
        &intent,
        || backend.execute_native_write(&prepared, store_identity(&provenance, &intent)),
        || policy.settled(),
        || policy.uncertain(provenance.owner, provenance.bot, key),
    )?;
    saved(store, &authority, &registered)
}

/// The existing fresh checks before a lease: current policy synchronization
/// and the live effect fence with its unchanged captured policy.
fn fresh_fence(
    port: &dyn BotDelegatePort,
    proof: &FreshBotPolicyProof,
    fence: &BotExecutionFence,
    captured: &CapturedBotPolicy,
) -> Result<(), ToolFailure> {
    super::policy::synchronize(proof, |command| owner_call(port, command))?;
    match owner_call(
        port,
        BotCommand::EffectFence {
            fence: fence.clone(),
        },
    )? {
        BotReply::EffectFence {
            fence: observed,
            policy,
        } if observed == *fence && policy == *captured => Ok(()),
        _ => Err(failure("native_execution_unavailable")),
    }
}

/// Settle a retained intent from its own evidence, before any fresh right is
/// asked for. `None` only for an unleased Prepared intent whose Store effect
/// is positively unconfirmed: the ordinary path may then ask for its one
/// fresh lease, which the owner itself refuses to grant twice.
#[allow(clippy::too_many_arguments)]
fn recover_retained(
    port: &dyn BotDelegatePort,
    policy: &dyn PolicySource,
    store: &StoreWorkerClient,
    backend: &koru_mcp_server::LocalBackend,
    authority: &NativeStoreAuthority,
    provenance: &BotProvenance,
    prepared: &PreparedNativeWrite,
    intent: &NativeResultIntentView,
) -> Option<Result<Value, ToolFailure>> {
    let key = &intent.key;
    if intent.stage == NativeResultStage::Registered {
        return Some(saved(store, authority, intent));
    }
    if intent.stage == NativeResultStage::Refused {
        return Some(Err(failure("native_request_refused")));
    }
    if intent.stage == NativeResultStage::Verified {
        // Durable evidence belongs to the owner, including edits whose
        // transport cannot independently prove a second Store outcome.
        let Some(evidence) = intent.verified.as_ref().filter(|e| {
            e.valid()
                && e.operation_id == key.operation_id
                && e.workspace == intent.destination
                && e.document == intent.target
        }) else {
            return Some(Err(failure("save_status_unknown")));
        };
        return Some(confirm(
            port,
            policy,
            store,
            authority,
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
        ));
    }
    // Recover original Store proof before asking for fresh execution rights.
    // This can register a held creation after an access-narrowing barrier.
    match backend.reconcile_native_write(prepared, store_identity(provenance, intent)) {
        Ok(NativeReconciliation::Verified(evidence)) => {
            Some(confirm(port, policy, store, authority, intent, evidence))
        }
        Ok(NativeReconciliation::Unconfirmed) if intent.stage == NativeResultStage::Prepared => {
            None
        }
        _ => Some(Err(failure("save_status_unknown"))),
    }
}

/// The acknowledged consumer's retry of the exact approved edit: one atomic
/// consume creates the native intent, its lease and the decision's
/// consumption together; the existing Store executor and receipt path then
/// runs once. A replay carries no Execute and only reconciles.
#[allow(clippy::too_many_arguments)]
fn consume(
    store: &StoreWorkerClient,
    policy: &dyn PolicySource,
    port: &dyn BotDelegatePort,
    backend: &koru_mcp_server::LocalBackend,
    authority: &NativeStoreAuthority,
    provenance: &BotProvenance,
    prepared: &PreparedNativeWrite,
    edit: &super::decision::DeniedEdit<'_>,
    view: &koru_delegate_api::bots::DecisionReceiptView,
    proof: &FreshBotPolicyProof,
    boot: DelegateBootId,
) -> Result<Value, ToolFailure> {
    let decisions::DecisionBindingV1::KoruEdit(binding) = &view.receipt.binding else {
        return Err(failure("native_operation_conflict"));
    };
    let description = prepared.description();
    let document = description
        .document
        .ok_or_else(|| failure("native_operation_conflict"))?;
    let title = koru_ai_server::native_document_title(store.clone(), authority.clone(), document)
        .map_err(refused)?;
    let request = PrepareNativeResult {
        key: edit.key.clone(),
        kind: NativeResultOperation::EditPage,
        destination: description.workspace,
        target: Some(document),
        request_fingerprint: edit.fingerprint,
        expected_revision: description.expected_revision.clone(),
        // The binding's own policy identity: any change refuses consumption.
        config_revision: binding.global_policy.config_revision.get(),
        policy_revision: binding.global_policy.policy_revision.get(),
        title,
    };
    let intent = match port.execute(BotCommand::ConsumeDecisionEdit {
        fence: edit.fence.clone(),
        request,
        payload: edit.payload.clone(),
        proof: proof.clone(),
        boot,
    }) {
        Ok(BotReply::NativeResult { intent }) => intent,
        // A typed refusal committed nothing; there is no other route to run it.
        Err(error) if error.code.starts_with("bot_") && !error.retryable => {
            return Err(failure("native_execution_unavailable"));
        }
        // The consume may have committed its one lease: never run it blind.
        _ => return Err(failure("save_status_unknown")),
    };
    validate_intent(&intent, edit.key, prepared, edit.fingerprint)?;
    if intent.disposition == NativeResultDisposition::Execute {
        let registered = execute_record(
            port,
            &intent,
            || backend.execute_native_write(prepared, store_identity(provenance, &intent)),
            || policy.settled(),
            || policy.uncertain(provenance.owner, provenance.bot, edit.key.clone()),
        )?;
        return saved(store, authority, &registered);
    }
    // Consumed replay is reconcile-only: never another Execute or Store
    // request, even when the original lease's outcome is still unknown.
    recover_retained(
        port, policy, store, backend, authority, provenance, prepared, &intent,
    )
    .unwrap_or_else(|| Err(failure("save_status_unknown")))
}

/// The single lease-to-settlement state machine for Result and Memory purposes.
/// Callers validate immutable intent identity before entering. Cancellation may
/// refuse before acquisition, but cannot abandon an Execute lease here. Fresh
/// AI preparation is forbidden; settlement/recovery notifications remain owed.
pub(super) fn execute_record(
    port: &dyn BotDelegatePort,
    intent: &NativeResultIntentView,
    execute: impl FnOnce() -> Result<NativeWriteEvidence, NativeWriteError>,
    settled: impl FnOnce(),
    uncertain: impl FnOnce() -> Result<(), ToolFailure>,
) -> Result<NativeResultIntentView, ToolFailure> {
    if intent.disposition != NativeResultDisposition::Execute
        || intent.stage != NativeResultStage::Prepared
        || intent.verified.is_some()
    {
        return Err(failure("native_operation_conflict"));
    }
    match execute() {
        Ok(evidence) => {
            let registered = confirm_record(port, intent, evidence)?;
            settled();
            Ok(registered)
        }
        Err(NativeWriteError::Refused(error)) => {
            let observed = owner_call(
                port,
                BotCommand::ConfirmNativeResult {
                    request: ConfirmNativeResult {
                        key: intent.key.clone(),
                        request_fingerprint: intent.request_fingerprint,
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
            if !matches!(observed, BotReply::NativeResult { intent: observed } if observed.stage == NativeResultStage::Refused
                && observed.key == intent.key && observed.request_fingerprint == intent.request_fingerprint
                && observed.purpose == intent.purpose && observed.store_request == intent.store_request
                && observed.destination == intent.destination && observed.target == intent.target
                && observed.kind == intent.kind && observed.verified.is_none())
            {
                return Err(failure("save_status_unknown"));
            }
            settled();
            Err(refused(error))
        }
        Err(NativeWriteError::Unconfirmed(_)) => {
            let _ = owner_call(
                port,
                BotCommand::ConfirmNativeResult {
                    request: ConfirmNativeResult {
                        key: intent.key.clone(),
                        request_fingerprint: intent.request_fingerprint,
                        observation: NativeResultObservation::Unconfirmed,
                    },
                },
            );
            uncertain()?;
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
    if intent.purpose != (NativeEffectPurpose::Result {})
        || intent.key != *key
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
pub(super) fn store_identity(
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
    let observed = confirm_record(port, intent, evidence)?;
    policy.settled();
    saved(store, authority, &observed)
}

/// Settle retained evidence without choosing its user-facing result format.
/// Memory receipts must never flow through the ordinary page-result formatter.
pub(super) fn confirm_record(
    port: &dyn BotDelegatePort,
    intent: &NativeResultIntentView,
    evidence: NativeWriteEvidence,
) -> Result<NativeResultIntentView, ToolFailure> {
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
                && observed.purpose == intent.purpose
                && observed.store_request == intent.store_request
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
    Ok(observed)
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
