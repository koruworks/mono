// SPDX-License-Identifier: MPL-2.0
//! Fresh preparation ends at the native lease; settlement is shared with Result.
use super::*;
use koru_ai_core::bots::{BotMemoryBinding, BotMemoryState};
use koru_ai_server::{MemoryBaseSelection, NativeStoreAuthority};
use koru_mcp_server::native::memory::{
    MemoryRecipeChange, MemoryWriteAction, MemoryWriteAuthority, MemoryWriteBinding,
};
use koru_store_server::StoreWorkerClient;

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    client: &dyn WritePort,
    port: &dyn BotDelegatePort,
    store: &StoreWorkerClient,
    record: &RunAuthorityRecord,
    subject_request: &RunMemoryWriteSubjectRequest,
    current: &MemoryWriteSubjectEvidence,
    args: &args::Arguments,
    hash: [u8; 32],
    old: Option<MemoryEditAllocation>,
) -> Result<Value, ToolFailure> {
    let cancel = &subject_request.cancellation;
    live(cancel)?;
    let barrier = current
        .new_work_barrier
        .as_ref()
        .ok_or_else(|| failure("permission_denied"))?;
    validate_barrier(barrier, &current.subject, record.workspace)?;
    if barrier.config_revision != current.config_revision
        || barrier.policy_revision != current.policy_revision
    {
        return Err(failure("permission_denied"));
    }
    // Invalid explicit history/fact never consumes a durable allocation slot.
    if old.is_none() {
        if let MemoryBaseSelector::History { version } = args.base {
            let proof = client.base(MemoryEditBaseRequest {
                subject_request: subject_request.clone(),
                selection: MemoryBaseSelection::UnallocatedHistory { version },
                operation: args.operation.clone(),
            })?;
            validate_base(&proof, &current.subject, record.workspace, args)?;
        }
    }
    live(cancel)?;
    let fence = synchronize(port, record, subject_request, barrier)?;
    let key = NativeResultKey {
        run: record.run,
        attempt: fence.attempt,
        generation: record.generation,
        turn: fence
            .turn
            .ok_or_else(|| failure("native_request_refused"))?,
        operation_id: args.operation_id,
    };
    let mut allocation = match old {
        Some(value) if value.key == key => value,
        Some(_) => return Err(failure("native_request_refused")),
        None => {
            live(cancel)?;
            let request = AllocateMemoryEditRequest {
                key: key.clone(),
                caller_fingerprint: hash,
                subject: current.subject.clone(),
                base: args.base,
                current_config_revision: barrier.config_revision,
                current_policy_revision: barrier.policy_revision,
                existing_fact_id: match args.operation {
                    MemoryRecipeOperation::Correct { fact, .. }
                    | MemoryRecipeOperation::Forget { fact } => Some(fact),
                    _ => None,
                },
            };
            match owner_call(port, BotCommand::AllocateMemoryEdit { request })? {
                BotReply::MemoryEditAllocation { allocation } => allocation,
                _ => return Err(failure("memory_write_unavailable")),
            }
        }
    };
    validate_allocation(&allocation, &current.subject, record, args, hash)?;
    if allocation.key != key {
        return Err(failure("memory_operation_conflict"));
    }
    // A concurrent exact call may already have prepared or settled this key.
    if let Some(saved) = retained(client, port, &allocation, record.workspace, cancel)? {
        return Ok(saved);
    }

    let binding = if matches!(args.base, MemoryBaseSelector::OriginalSeed {}) {
        live(cancel)?;
        let state = client.ensure(RunMemoryEnsureRequest {
            subject_request: subject_request.clone(),
            allocation: allocation.clone(),
        })?;
        live(cancel)?;
        match state.state {
            BotMemoryState::Prepared | BotMemoryState::Unconfirmed => {
                return Err(creation_state(
                    "memory_creation_pending",
                    args.operation_id,
                    state.state,
                ));
            }
            BotMemoryState::Conflict => {
                return Err(creation_state(
                    "memory_creation_conflict",
                    args.operation_id,
                    state.state,
                ));
            }
            BotMemoryState::Abandoned => {
                return Err(creation_state(
                    "memory_creation_abandoned",
                    args.operation_id,
                    state.state,
                ));
            }
            BotMemoryState::Bound => {
                let binding = state
                    .binding
                    .ok_or_else(|| failure("memory_write_unavailable"))?;
                if binding.owner.user_scope != current.subject.owner.user_scope
                    || binding.owner.device != current.subject.owner.device
                    || binding.bot != current.subject.bot
                    || binding.page.workspace != record.workspace
                    || binding.creation_request != state.creation_request
                    || binding.page.document.as_uuid().is_nil()
                    || binding.bound_at_ms < 0
                {
                    return Err(failure("permission_denied"));
                }
                Some(binding)
            }
        }
    } else {
        None
    };
    live(cancel)?;
    let base = client.base(MemoryEditBaseRequest {
        subject_request: subject_request.clone(),
        selection: MemoryBaseSelection::Allocated {
            allocation: allocation.clone(),
        },
        operation: args.operation.clone(),
    })?;
    validate_base(&base, &current.subject, record.workspace, args)?;
    if let Some(binding) = binding {
        validate_binding(&binding, &base.authority)?;
    }
    live(cancel)?;
    let resolved = match owner_call(
        port,
        BotCommand::ResolveMemoryEditBase {
            key: key.clone(),
            caller_fingerprint: hash,
            authority: base.authority.clone(),
            expected_version: base.version,
            expected_revision: base.expected_revision.clone(),
            current_config_revision: barrier.config_revision,
            current_policy_revision: barrier.policy_revision,
        },
    )? {
        BotReply::MemoryEditAllocation { allocation } => allocation,
        _ => return Err(failure("memory_write_unavailable")),
    };
    validate_allocation(&resolved, &current.subject, record, args, hash)?;
    if !same_identity(&allocation, &resolved) {
        return Err(failure("memory_operation_conflict"));
    }
    let MemoryBaseResolution::Resolved {
        authority,
        version,
        expected_revision,
    } = &resolved.resolution
    else {
        return Err(failure("memory_operation_conflict"));
    };
    if *authority != base.authority
        || *version != base.version
        || !same_native_edit_revision(expected_revision, &base.expected_revision).unwrap_or(false)
        || match &allocation.resolution {
            MemoryBaseResolution::Resolved {
                authority: prior,
                version: v,
                expected_revision: bytes,
            } => prior != authority || v != version || bytes != expected_revision,
            MemoryBaseResolution::Unresolved {} => false,
        }
    {
        return Err(failure("memory_operation_conflict"));
    }
    allocation = resolved;
    let MemoryBaseResolution::Resolved {
        authority,
        version,
        expected_revision,
    } = &allocation.resolution
    else {
        unreachable!()
    };
    let store_authority = NativeStoreAuthority {
        owner_scope: current.subject.owner.user_scope,
        expected_profile: current.profile,
        workspace: record.workspace,
    };
    let backend = koru_ai_server::native_store_backend(store.clone(), store_authority);
    let prepared = backend
        .prepare_memory_write(
            base.recipe,
            MemoryWriteBinding {
                authority: MemoryWriteAuthority {
                    version: authority.version,
                    owner: authority.owner.user_scope,
                    device: authority.owner.device,
                    bot: authority.bot.as_uuid(),
                    submission: authority.submission.as_uuid(),
                    captured_config_revision: authority.captured_config_revision,
                    creation_request: authority.creation_request.as_uuid(),
                    workspace: authority.workspace,
                    memory_document: authority.memory_document,
                    action: match authority.action {
                        MemoryEditAction::Remember => MemoryWriteAction::Remember,
                        MemoryEditAction::Correct => MemoryWriteAction::Correct,
                        MemoryEditAction::Forget => MemoryWriteAction::Forget,
                    },
                },
                history: version.as_uuid(),
                expected_revision: expected_revision.clone(),
                fact: allocation.fact_id,
                detail: allocation.detail_id,
                source: allocation.source_id,
                source_run_document: allocation.source_run_document,
                created_at_ms: allocation.created_at_ms,
                reader: current.profile,
            },
        )
        .map_err(|error| failure(&error.code))?;
    let description = prepared.description();
    if description.workspace != authority.workspace
        || description.document != Some(authority.memory_document)
        || description.kind != NativeWriteKind::Edited
        || description.expected_revision.as_ref() != Some(expected_revision)
    {
        return Err(failure("memory_operation_conflict"));
    }
    let expected_hash = dispatch::fingerprint(&description.fingerprint)?;
    live(cancel)?;
    let fresh = client.authority(MemoryWriteAuthorityRequest {
        subject_request: subject_request.clone(),
        resolved_allocation: allocation.clone(),
    })?;
    if fresh.authority != *authority
        || fresh.profile != current.profile
        || fresh.barrier.config_revision < barrier.config_revision
        || fresh.barrier.policy_revision < barrier.policy_revision
    {
        return Err(failure("permission_denied"));
    }
    validate_barrier(&fresh.barrier, &current.subject, record.workspace)?;
    live(cancel)?;
    if synchronize(port, record, subject_request, &fresh.barrier)? != fence {
        return Err(failure("native_request_refused"));
    }
    live(cancel)?; // Last cooperative cancellation before acquiring a possible lease.
    let request = PrepareNativeResult {
        key: key.clone(),
        kind: NativeResultOperation::EditPage,
        destination: authority.workspace,
        target: Some(authority.memory_document),
        request_fingerprint: expected_hash,
        expected_revision: Some(expected_revision.clone()),
        config_revision: fresh.barrier.config_revision,
        policy_revision: fresh.barrier.policy_revision,
        title: description.title.clone(),
    };
    let intent = match owner_call(
        port,
        BotCommand::PrepareMemoryEdit {
            request,
            caller_fingerprint: hash,
            authority: authority.clone(),
        },
    ) {
        Ok(BotReply::NativeResult { intent }) => intent,
        Ok(_) => return unknown(client, &allocation),
        Err(error) if error.code == "memory_write_unavailable" => {
            return unknown(client, &allocation);
        }
        Err(error) => return Err(error),
    };
    // A bad echo may still conceal an acquired lease. Never execute it; request
    // recovery for the known request key, never the untrusted returned key.
    if validate_retained(&allocation, &intent, record.workspace).is_err()
        || intent.request_fingerprint != expected_hash
    {
        return unknown(client, &allocation);
    }
    if intent.disposition != NativeResultDisposition::Execute {
        return retained(client, port, &allocation, record.workspace, cancel)?
            .ok_or_else(|| failure("save_status_unknown"));
    }
    // No cancellation or fresh AI call between Execute and its settlement.
    let registered = dispatch::execute_record(
        port,
        &intent,
        || {
            backend.execute_native_write(
                &prepared,
                dispatch::store_identity(&subject_request.provenance, &intent),
            )
        },
        || client.settled(),
        || client.uncertain(current.subject.owner, current.subject.bot, key.clone()),
    );
    match registered {
        Ok(registered) => receipt(&allocation, &registered),
        Err(error)
            if matches!(
                error.code.as_str(),
                "native_owner_unavailable" | "save_status_unknown" | "native_operation_conflict"
            ) =>
        {
            unknown(client, &allocation)
        }
        Err(error) => Err(failure(&error.code)),
    }
}

fn unknown(
    client: &dyn WritePort,
    allocation: &MemoryEditAllocation,
) -> Result<Value, ToolFailure> {
    let _ = client.uncertain(
        allocation.subject.owner,
        allocation.subject.bot,
        allocation.key.clone(),
    );
    Err(failure("save_status_unknown"))
}
fn creation_state(code: &str, operation: Uuid, state: BotMemoryState) -> ToolFailure {
    let mut error = failure(code);
    error.details = Some(json!({"operation_id":operation,"state":state}));
    error
}
fn validate_binding(
    binding: &BotMemoryBinding,
    authority: &MemoryEditAuthorityV1,
) -> Result<(), ToolFailure> {
    if binding.owner.user_scope != authority.owner.user_scope
        || binding.owner.device != authority.owner.device
        || binding.bot != authority.bot
        || binding.page.workspace != authority.workspace
        || binding.page.document != authority.memory_document
        || binding.creation_request != authority.creation_request
    {
        return Err(failure("memory_operation_conflict"));
    }
    Ok(())
}
fn validate_barrier(
    barrier: &BotPolicyBarrier,
    subject: &MemoryEditSubjectV1,
    workspace: WorkspaceId,
) -> Result<(), ToolFailure> {
    if barrier.key.owner != subject.owner
        || barrier.key.bot != subject.bot
        || !barrier.effects_allowed
        || !barrier.policy.valid()
        || barrier.policy.workspace != workspace
        || !(subject.captured_config_revision..=i64::MAX as u64).contains(&barrier.config_revision)
        || !(1..=i64::MAX as u64).contains(&barrier.policy_revision)
    {
        return Err(failure("permission_denied"));
    }
    Ok(())
}
fn synchronize(
    port: &dyn BotDelegatePort,
    record: &RunAuthorityRecord,
    subject: &RunMemoryWriteSubjectRequest,
    barrier: &BotPolicyBarrier,
) -> Result<BotExecutionFence, ToolFailure> {
    live(&subject.cancellation)?;
    match owner_call(
        port,
        BotCommand::SynchronizePolicy {
            barrier: barrier.clone(),
        },
    )? {
        BotReply::PolicyBarrier { receipt }
            if receipt.barrier == *barrier && receipt.state == BotPolicyBarrierState::Effective => {
        }
        _ => return Err(failure("native_policy_pending")),
    }
    let key = BotSubmissionKey {
        owner: subject.provenance.owner,
        bot: subject.provenance.bot,
        request: subject.provenance.submission,
    };
    let fence = match owner_call(port, BotCommand::Reconcile { key: key.clone() })? {
        BotReply::Admission { admission }
            if admission.key == key
                && admission.run == record.run
                && admission.state == BotAdmissionState::Executing =>
        {
            admission.fence
        }
        _ => None,
    }
    .ok_or_else(|| failure("native_request_refused"))?;
    if fence.owner != key.owner
        || fence.bot != key.bot
        || fence.run != record.run
        || fence.generation != record.generation
        || fence.turn.is_none()
    {
        return Err(failure("native_request_refused"));
    }
    match owner_call(
        port,
        BotCommand::EffectFence {
            fence: fence.clone(),
        },
    )? {
        BotReply::EffectFence {
            fence: found,
            policy,
        } if found == fence && policy == subject.captured => {}
        _ => return Err(failure("native_request_refused")),
    }
    live(&subject.cancellation)?;
    Ok(fence)
}
fn same_identity(a: &MemoryEditAllocation, b: &MemoryEditAllocation) -> bool {
    a.key == b.key
        && a.caller_fingerprint == b.caller_fingerprint
        && a.subject == b.subject
        && a.base == b.base
        && a.ensure == b.ensure
        && a.source_run_document == b.source_run_document
        && a.fact_id == b.fact_id
        && a.detail_id == b.detail_id
        && a.source_id == b.source_id
        && a.created_at_ms == b.created_at_ms
}
fn validate_base(
    base: &MemoryBaseEvidence,
    subject: &MemoryEditSubjectV1,
    workspace: WorkspaceId,
    args: &args::Arguments,
) -> Result<(), ToolFailure> {
    if !base.authority.valid()
        || base.authority.subject() != *subject
        || base.authority.workspace != workspace
        || base.version.as_uuid().is_nil()
        || base.recipe.workspace != workspace
        || base.recipe.document != base.authority.memory_document
        || base.recipe.history != base.version.as_uuid()
        || !same_native_edit_revision(&base.expected_revision, &base.expected_revision)
            .unwrap_or(false)
        || matches!(args.base, MemoryBaseSelector::History { version } if version != base.version)
    {
        return Err(failure("memory_operation_conflict"));
    }
    base.recipe
        .validate()
        .map_err(|error| failure(&error.code))?;
    let matches = match (&args.operation, &base.recipe.change) {
        (
            MemoryRecipeOperation::Remember { summary, detail },
            MemoryRecipeChange::Remember {
                summary: s,
                detail: d,
                ..
            },
        ) => summary == s && detail == d,
        (
            MemoryRecipeOperation::Correct {
                fact,
                summary,
                detail,
            },
            MemoryRecipeChange::Correct {
                fact: f,
                summary: s,
                detail: d,
                ..
            },
        ) => fact == f && summary == s && detail == d,
        (MemoryRecipeOperation::Forget { fact }, MemoryRecipeChange::Forget { fact: f, .. }) => {
            fact == f
        }
        _ => false,
    };
    if !matches {
        return Err(failure("memory_operation_conflict"));
    }
    Ok(())
}
