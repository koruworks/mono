// SPDX-License-Identifier: MPL-2.0
//! Native tool orchestration; document semantics remain in the shared MCP owner.
use super::decision::EditDecisions;
use super::*;
use crate::GatewayState;
use koru_ai_server::native_effect::{self as effect, NativeEffectHost, NativeEffectSubject};
use koru_ai_server::{
    AiWorkerClient, NativePolicyEvidence, NativePolicyRequest, NativeStoreAuthority,
};
use koru_artifact_wire::run_tools::ToolFailure;
use koru_delegate_core::*;
use koru_mcp_server::native::{
    NativeStoreIdentity, NativeWriteError, NativeWriteEvidence, NativeWriteKind,
    PreparedNativeWrite,
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
    /// Hand the AI the exact positive decision receipt the owner answered,
    /// so its card can name the binding, human and revision. History only:
    /// it grants nothing, and a failure never changes the tool's answer.
    fn record_decision(
        &self,
        view: &koru_delegate_api::bots::DecisionReceiptView,
    ) -> Result<(), ToolFailure>;
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
    fn record_decision(
        &self,
        view: &koru_delegate_api::bots::DecisionReceiptView,
    ) -> Result<(), ToolFailure> {
        self.record_bot_decision(view.clone())
            .map_err(|_| failure("decision_record_unavailable"))
    }
}
/// The shared native effect ([`koru_ai_server::native_effect`]) answering in
/// this path's failure vocabulary and notifying through the AI policy source.
struct Host<'a>(&'a dyn PolicySource);
impl NativeEffectHost for Host<'_> {
    type Failure = ToolFailure;
    fn failure(&self, code: &'static str) -> ToolFailure {
        failure(code)
    }
    fn settled(&self) {
        self.0.settled();
    }
    fn uncertain(
        &self,
        owner: BotOwner,
        bot: BotId,
        key: NativeResultKey,
    ) -> Result<(), ToolFailure> {
        self.0.uncertain(owner, bot, key)
    }
}
pub(super) fn failure(code: &'static str) -> ToolFailure {
    ToolFailure::new(
        code,
        "The native document operation could not be confirmed. Inspect the result before continuing.",
    )
}
fn refused(error: ToolError) -> ToolFailure {
    failure(effect::refused_code(&error))
}
fn owner_call(port: &dyn BotDelegatePort, command: BotCommand) -> Result<BotReply, ToolFailure> {
    effect::owner_call(port, command, &failure)
}

/// The mutating tools a child wait refuses here, before any owner is asked
/// (contract M6 4.6): a Memory write and a Routine Create or Retry. The two
/// document mutations take the native path below, which settles a retained
/// save from its own evidence first; `ask_bot` and the private read answer
/// the fence in their own modules.
pub(super) fn child_fenced_here(name: &str) -> bool {
    matches!(name, "remember_memory" | "correct_memory" | "forget_memory")
        || super::routines::mutation(name)
}

pub(in crate::run_mcp) async fn call(
    state: &Arc<GatewayState>,
    record: Arc<RunAuthorityRecord>,
    provenance: BotProvenance,
    captured: CapturedBotPolicy,
    wait: RunWait,
    params: CallToolRequestParams,
) -> CallToolResult {
    // The three tools that ask another Bot (M6). Closed in this build: each
    // answers `unknown_tool` before any owner is asked.
    if super::children::recognizes(params.name.as_ref()) {
        return super::children::call(state, record, provenance, captured, wait, params).await;
    }
    if wait == RunWait::Child && child_fenced_here(params.name.as_ref()) {
        return super::children::fenced(state, &record, &provenance).await;
    }
    if super::routines::recognizes(params.name.as_ref()) {
        return super::routines::call(state, record, provenance, captured, params).await;
    }
    if super::memory::recognizes(params.name.as_ref()) {
        return super::memory::call(state, record, provenance, captured, params).await;
    }
    // The private read (M5). Closed in this build: it answers its own typed
    // refusal before any owner is asked.
    if super::private::recognizes(params.name.as_ref()) {
        return super::private::call(state, record, provenance, captured, wait, params).await;
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
        invoke_for(
            &store,
            &client,
            port.as_ref(),
            &record,
            provenance,
            captured,
            params.name.as_ref(),
            Value::Object(params.arguments.unwrap_or_default()),
            EditDecisions::production(),
            wait,
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
    effect::parse_fingerprint(raw).ok_or_else(|| failure("invalid_request"))
}

/// [`invoke_for`] for a Run that is not waiting (`false`) or waits on a
/// decision (`true`): the shape every existing caller and test uses.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn invoke(
    store: &StoreWorkerClient,
    policy: &dyn PolicySource,
    port: &dyn BotDelegatePort,
    record: &RunAuthorityRecord,
    provenance: BotProvenance,
    captured: CapturedBotPolicy,
    tool: &str,
    arguments: Value,
    decisions: EditDecisions,
    waiting: bool,
) -> Result<Value, ToolFailure> {
    invoke_for(
        store,
        policy,
        port,
        record,
        provenance,
        captured,
        tool,
        arguments,
        decisions,
        if waiting {
            RunWait::Decision
        } else {
            RunWait::None
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn invoke_for(
    store: &StoreWorkerClient,
    policy: &dyn PolicySource,
    port: &dyn BotDelegatePort,
    record: &RunAuthorityRecord,
    provenance: BotProvenance,
    captured: CapturedBotPolicy,
    tool: &str,
    mut arguments: Value,
    decisions: EditDecisions,
    wait: RunWait,
) -> Result<Value, ToolFailure> {
    let waiting = wait.waiting();
    // A child wait never prepares a decision (contract M6 4.6).
    let decisions = decisions.for_wait(wait);
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
    let fence = execution_fence(port, &provenance, record)?;
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
        // A waiting Run (O147) is refused below with its typed fence instead.
        Err(error) if (denial.is_some() || waiting) && error.code == "publication_pending" => None,
        _ => return Err(failure("native_owner_unavailable")),
    };
    if let Some(intent) = &retained {
        // O152: while the Run waits, the exact retry of a decision's own
        // consumed edit is answered from a `Verified` save too; only the
        // decision's read-only replay can say that it is. Everything else
        // keeps today's proof.
        let proof = match (waiting, denial, payload.as_ref()) {
            (true, Some(denial), Some(payload)) => super::decision::retry_proof(
                port,
                provenance.owner,
                &super::decision::DeniedEdit {
                    fence: &fence,
                    key: &key,
                    denial,
                    fingerprint: expected_hash,
                    expected_revision: description.expected_revision.as_deref().unwrap_or_default(),
                    payload,
                },
                &current.proof,
                intent,
            ),
            _ => effect::SaveProof::Registered,
        };
        if let Some(answer) = effect::recover_retained_as(
            &Host(policy),
            port,
            store,
            &backend,
            &authority,
            NativeEffectSubject::of(&provenance),
            &prepared,
            intent,
            proof,
        ) {
            return answer;
        }
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
            &|view| policy.record_decision(view),
            || {
                synchronized = true;
                fresh_fence(port, &current.proof, &fence, &captured)
            },
        )?;
        if let super::decision::DecisionOutcome::Consume { view, boot } = outcome {
            return effect::consume(
                &Host(policy),
                store,
                port,
                &backend,
                &authority,
                NativeEffectSubject::of(&provenance),
                &prepared,
                &effect::ApprovedEdit {
                    fence: edit.fence,
                    key: edit.key,
                    fingerprint: edit.fingerprint,
                    payload: edit.payload,
                },
                &view,
                &current.proof,
                boot,
            );
        }
    }
    // O147: while the Run's decision wait is open, nothing new is accepted
    // here. Reads returned above; a retained intent was settled from its own
    // evidence above; the exact decision retry and consume took the decision
    // path. Anything else would need a fresh lease. A child wait (M6 4.6)
    // has no decision path at all: every new mutation is refused here.
    match wait {
        RunWait::Decision => return Err(super::decision::in_progress(&key)),
        RunWait::Child => {
            return Err(super::children::in_progress(
                port,
                &provenance,
                record,
                Some(key.operation_id),
            ));
        }
        RunWait::None => {}
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
            effect::saved(&Host(policy), store, &authority, &intent)
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
    effect::saved(&Host(policy), store, &authority, &registered)
}

/// The Run's live execution fence, from the owner's admission of its own
/// submission: the attempt, generation, session and turn an effect is bound
/// to. Anything that is not exactly this Run's is no fence.
pub(super) fn execution_fence(
    port: &dyn BotDelegatePort,
    provenance: &BotProvenance,
    record: &RunAuthorityRecord,
) -> Result<BotExecutionFence, ToolFailure> {
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
    Ok(fence)
}

/// The existing fresh checks before a lease: current policy synchronization
/// and the live effect fence with its unchanged captured policy.
pub(super) fn fresh_fence(
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

// The effect half lives in `koru_ai_server::native_effect`, shared with the
// AI's approved-edit trigger (O133 design B). These keep this module's
// signatures for the Memory path and the tests; each is the shared function
// answering in this path's failure vocabulary.

/// The single lease-to-settlement state machine for Result and Memory purposes
/// ([`effect::execute_record`]).
pub(super) fn execute_record(
    port: &dyn BotDelegatePort,
    intent: &NativeResultIntentView,
    execute: impl FnOnce() -> Result<NativeWriteEvidence, NativeWriteError>,
    settled: impl FnOnce(),
    uncertain: impl FnOnce() -> Result<(), ToolFailure>,
) -> Result<NativeResultIntentView, ToolFailure> {
    effect::execute_record(port, intent, execute, settled, uncertain, &failure)
}

fn validate_intent(
    intent: &NativeResultIntentView,
    key: &NativeResultKey,
    prepared: &PreparedNativeWrite,
    hash: [u8; 32],
) -> Result<(), ToolFailure> {
    effect::validate_intent(intent, key, prepared, hash, &failure)
}
pub(super) fn store_identity(
    provenance: &BotProvenance,
    intent: &NativeResultIntentView,
) -> NativeStoreIdentity {
    effect::store_identity(NativeEffectSubject::of(provenance), intent)
}

/// Settle retained evidence without choosing its user-facing result format
/// ([`effect::confirm_record`]).
pub(super) fn confirm_record(
    port: &dyn BotDelegatePort,
    intent: &NativeResultIntentView,
    evidence: NativeWriteEvidence,
) -> Result<NativeResultIntentView, ToolFailure> {
    effect::confirm_record(port, intent, evidence, &failure)
}

#[cfg(test)]
mod tests;
