// SPDX-License-Identifier: MPL-2.0

//! The trusted native page effect: the one lease-to-settlement state machine
//! for a native page result, its retained-intent recovery and the one-use
//! consume of an approved Koru edit.
//!
//! One implementation, two callers: the gateway's native tool path (a model's
//! `create_document` / `edit_document`, and its exact retry of an approved
//! edit) and the AI's approved-edit trigger (O133 design B), which saves the
//! approved edit once right after the successor's continuation is
//! acknowledged. It lives here, below the gateway, because the gateway
//! depends on this crate and not the other way round.
//!
//! Nothing here decides policy or chooses a user-facing failure text: the
//! caller supplies both through [`NativeEffectHost`]. Every failure is a
//! stable code; the caller turns it into its own failure value, so the
//! gateway's answers are byte-for-byte what they were before the move.
use crate::store_tools::{NativeStoreAuthority, native_document_title};
use koru_delegate_api::bots::{
    BotBootScan, BotCommand, BotDelegatePort, BotReply, DecisionReceiptView,
};
use koru_delegate_core::decisions::{DecisionBindingV1, DecisionPayload, DecisionResumeState};
use koru_delegate_core::{
    BotExecutionFence, BotId, BotOwner, BotProvenance, ConfirmNativeResult, DelegateBootId,
    FreshBotPolicyProof, NativeEffectPurpose, NativeResultDisposition, NativeResultIntentView,
    NativeResultKey, NativeResultKind, NativeResultObservation, NativeResultOperation,
    NativeResultReference, NativeResultRefusal, NativeResultStage, PrepareNativeResult,
};
use koru_mcp_server::native::{
    NativeReconciliation, NativeStoreIdentity, NativeWriteError, NativeWriteEvidence,
    NativeWriteKind, PreparedNativeWrite,
};
use koru_mcp_server::{LocalBackend, ToolError};
use koru_store_server::StoreWorkerClient;
use serde_json::{Value, json};

/// What the shared effect needs from its caller: its failure vocabulary and
/// the two notifications a settled or uncertain effect owes.
pub trait NativeEffectHost {
    type Failure;
    /// The caller's failure for one stable code.
    fn failure(&self, code: &'static str) -> Self::Failure;
    /// A native effect settled; it may unblock a retained access change.
    fn settled(&self);
    /// A native effect's outcome is unknown: ask the existing recovery owner.
    fn uncertain(
        &self,
        owner: BotOwner,
        bot: BotId,
        key: NativeResultKey,
    ) -> Result<(), Self::Failure>;
}

/// Who a native effect belongs to: the Store request scope and the recovery
/// subject. Taken from the Run's classified provenance or the decision's
/// binding, never from tool arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeEffectSubject {
    pub owner: BotOwner,
    pub bot: BotId,
}
impl NativeEffectSubject {
    pub fn of(provenance: &BotProvenance) -> Self {
        Self {
            owner: provenance.owner,
            bot: provenance.bot,
        }
    }
}

/// The exact approved edit, as the decision binding retains it, and the
/// acknowledged consumer that may consume it.
pub struct ApprovedEdit<'a> {
    pub fence: &'a BotExecutionFence,
    pub key: &'a NativeResultKey,
    pub fingerprint: [u8; 32],
    pub payload: &'a DecisionPayload,
}

/// What a positive save confirmation must show.
///
/// The owner registers a confirmed page result in the Run document when it
/// can; while a Koru edit decision holds that document, the result it just
/// recorded comes back `Verified` with the exact evidence and its
/// registration follows once the decision's wait has settled (O150, O152).
/// Only for the decision's own consumed edit is that reply a positive save;
/// every other result still needs `Registered`, exactly as before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveProof {
    /// `Registered` with the exact evidence (every ordinary page result).
    Registered,
    /// The decision's own consumed edit: `Registered`, or `Verified`
    /// carrying exactly the evidence that was sent.
    DecisionConsumed,
}

/// Whether `intent` is the edit `view`'s decision consumed: the exact
/// operation, target, workspace and fingerprint of its binding, under the
/// acknowledged consumer its retained resume names (attempt, generation,
/// session and accepted turn), which is also `fence`. Read from the owner's
/// exact receipt; anything else, including a receipt without an acknowledged
/// resume, is not.
pub fn decision_consumed(
    view: &DecisionReceiptView,
    fence: &BotExecutionFence,
    intent: &NativeResultIntentView,
) -> bool {
    let DecisionBindingV1::KoruEdit(binding) = &view.receipt.binding else {
        return false;
    };
    let Some(resume) = view.resume.as_ref() else {
        return false;
    };
    let (attempt, generation, session, _) = resume.consumer_identity();
    let turn = match &resume.state {
        DecisionResumeState::InputAcknowledged { acknowledgment }
        | DecisionResumeState::Closed {
            acknowledgment: Some(acknowledgment),
        } => acknowledgment.turn,
        _ => return false,
    };
    let key = &intent.key;
    resume.decision == view.receipt.decision
        && intent.purpose == (NativeEffectPurpose::Result {})
        && intent.kind == NativeResultOperation::EditPage
        && binding.run == key.run
        && binding.operation_id == key.operation_id
        && binding.target == intent.target
        && binding.workspace == intent.destination
        && binding.edit_fingerprint == intent.request_fingerprint
        && attempt == key.attempt
        && generation == key.generation
        && turn == key.turn
        && session == Some(fence.session)
        && fence.run == key.run
        && fence.attempt == key.attempt
        && fence.generation == key.generation
        && fence.turn == Some(key.turn)
}

/// The stable code a shared builder or Store refusal maps to.
pub fn refused_code(error: &ToolError) -> &'static str {
    match error.code.as_str() {
        "permission_denied" | "forbidden" => "permission_denied",
        "revision_conflict" | "idempotency_conflict" => "revision_conflict",
        "native_no_change" => "native_no_change",
        "invalid_request" => "invalid_request",
        _ => "native_request_refused",
    }
}

/// One owner command; any failure to get an answer is `native_owner_unavailable`.
pub fn owner_call<F>(
    port: &dyn BotDelegatePort,
    command: BotCommand,
    fail: &dyn Fn(&'static str) -> F,
) -> Result<BotReply, F> {
    port.execute(command)
        .map_err(|_| fail("native_owner_unavailable"))
}

/// The shared builder's request fingerprint, as 32 bytes.
pub fn parse_fingerprint(raw: &str) -> Option<[u8; 32]> {
    if raw.len() != 64 || !raw.is_ascii() {
        return None;
    }
    let mut result = [0; 32];
    for (index, value) in result.iter_mut().enumerate() {
        *value = u8::from_str_radix(&raw[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(result)
}

/// The delegate's current boot, probed with an empty scan. `None` when the
/// owner did not answer one positively.
pub fn probe_boot(port: &dyn BotDelegatePort, owner: BotOwner) -> Option<DelegateBootId> {
    match port.execute(BotCommand::Hello {
        owner,
        scan: BotBootScan::Probe {},
    }) {
        Ok(BotReply::Hello { hello }) if hello.entries.is_empty() && hello.next.is_none() => {
            Some(hello.boot)
        }
        _ => None,
    }
}

/// The Store request identity of one retained intent.
pub fn store_identity(
    subject: NativeEffectSubject,
    intent: &NativeResultIntentView,
) -> NativeStoreIdentity {
    NativeStoreIdentity {
        candidate: intent.target,
        scope: format!(
            "koru.bot.native.v1:{}:{}:{}",
            subject.owner.user_scope, subject.bot, intent.key.run
        ),
        request_key: intent.store_request.to_string(),
    }
}

/// The owner's intent is exactly the one this prepared write and key name.
pub fn validate_intent<F>(
    intent: &NativeResultIntentView,
    key: &NativeResultKey,
    prepared: &PreparedNativeWrite,
    hash: [u8; 32],
    fail: &dyn Fn(&'static str) -> F,
) -> Result<(), F> {
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
        return Err(fail("native_operation_conflict"));
    }
    Ok(())
}

/// The single lease-to-settlement state machine for Result and Memory purposes.
/// Callers validate immutable intent identity before entering. Cancellation may
/// refuse before acquisition, but cannot abandon an Execute lease here. Fresh
/// AI preparation is forbidden; settlement/recovery notifications remain owed.
pub fn execute_record<F>(
    port: &dyn BotDelegatePort,
    intent: &NativeResultIntentView,
    execute: impl FnOnce() -> Result<NativeWriteEvidence, NativeWriteError>,
    settled: impl FnOnce(),
    uncertain: impl FnOnce() -> Result<(), F>,
    fail: &dyn Fn(&'static str) -> F,
) -> Result<NativeResultIntentView, F> {
    execute_record_as(
        port,
        intent,
        SaveProof::Registered,
        execute,
        settled,
        uncertain,
        fail,
    )
}

/// [`execute_record`] with the positive save `proof` this intent needs.
pub fn execute_record_as<F>(
    port: &dyn BotDelegatePort,
    intent: &NativeResultIntentView,
    proof: SaveProof,
    execute: impl FnOnce() -> Result<NativeWriteEvidence, NativeWriteError>,
    settled: impl FnOnce(),
    uncertain: impl FnOnce() -> Result<(), F>,
    fail: &dyn Fn(&'static str) -> F,
) -> Result<NativeResultIntentView, F> {
    if intent.disposition != NativeResultDisposition::Execute
        || intent.stage != NativeResultStage::Prepared
        || intent.verified.is_some()
    {
        return Err(fail("native_operation_conflict"));
    }
    match execute() {
        Ok(evidence) => {
            let registered = confirm_record_as(port, intent, evidence, proof, fail)?;
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
                fail,
            )?;
            if !matches!(observed, BotReply::NativeResult { intent: observed } if observed.stage == NativeResultStage::Refused
                && observed.key == intent.key && observed.request_fingerprint == intent.request_fingerprint
                && observed.purpose == intent.purpose && observed.store_request == intent.store_request
                && observed.destination == intent.destination && observed.target == intent.target
                && observed.kind == intent.kind && observed.verified.is_none())
            {
                return Err(fail("save_status_unknown"));
            }
            settled();
            Err(fail(refused_code(&error)))
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
                fail,
            );
            uncertain()?;
            Err(fail("save_status_unknown"))
        }
    }
}

/// Settle retained evidence without choosing its user-facing result format.
/// Memory receipts must never flow through the ordinary page-result formatter.
pub fn confirm_record<F>(
    port: &dyn BotDelegatePort,
    intent: &NativeResultIntentView,
    evidence: NativeWriteEvidence,
    fail: &dyn Fn(&'static str) -> F,
) -> Result<NativeResultIntentView, F> {
    confirm_record_as(port, intent, evidence, SaveProof::Registered, fail)
}

/// [`confirm_record`] with the positive save `proof` this intent needs. The
/// owner's reply must carry exactly the evidence sent, whatever the proof.
pub fn confirm_record_as<F>(
    port: &dyn BotDelegatePort,
    intent: &NativeResultIntentView,
    evidence: NativeWriteEvidence,
    proof: SaveProof,
    fail: &dyn Fn(&'static str) -> F,
) -> Result<NativeResultIntentView, F> {
    let reference = NativeResultReference {
        version_id: None,
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
        fail,
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
                && (observed.stage == NativeResultStage::Registered
                    || (proof == SaveProof::DecisionConsumed
                        && observed.stage == NativeResultStage::Verified)) =>
        {
            observed
        }
        _ => return Err(fail("save_status_unknown")),
    };
    Ok(observed)
}

/// The page result a registered intent answers. Even an old Saved receipt
/// cannot expose a now-hidden page.
pub fn saved<H: NativeEffectHost + ?Sized>(
    host: &H,
    store: &StoreWorkerClient,
    authority: &NativeStoreAuthority,
    intent: &NativeResultIntentView,
) -> Result<Value, H::Failure> {
    let evidence = intent
        .verified
        .as_ref()
        .filter(|e| {
            e.valid()
                && e.operation_id == intent.key.operation_id
                && e.workspace == intent.destination
                && e.document == intent.target
        })
        .ok_or_else(|| host.failure("save_status_unknown"))?;
    native_document_title(store.clone(), authority.clone(), evidence.document)
        .map_err(|error| host.failure(refused_code(&error)))?;
    Ok(
        json!({"status":"saved","operation_id":evidence.operation_id,"workspace_id":evidence.workspace,
        "document_id":evidence.document,"title":evidence.title,
        "base_revision":evidence.version.iter().map(|b| format!("{b:02x}")).collect::<String>(),"kind":evidence.kind}),
    )
}

fn confirm<H: NativeEffectHost + ?Sized>(
    host: &H,
    port: &dyn BotDelegatePort,
    store: &StoreWorkerClient,
    authority: &NativeStoreAuthority,
    intent: &NativeResultIntentView,
    evidence: NativeWriteEvidence,
    proof: SaveProof,
) -> Result<Value, H::Failure> {
    let observed = confirm_record_as(port, intent, evidence, proof, &|code| host.failure(code))?;
    host.settled();
    saved(host, store, authority, &observed)
}

/// Settle a retained intent from its own evidence, before any fresh right is
/// asked for. `None` only for an unleased Prepared intent whose Store effect
/// is positively unconfirmed: the ordinary path may then ask for its one
/// fresh lease, which the owner itself refuses to grant twice.
#[allow(clippy::too_many_arguments)]
pub fn recover_retained<H: NativeEffectHost + ?Sized>(
    host: &H,
    port: &dyn BotDelegatePort,
    store: &StoreWorkerClient,
    backend: &LocalBackend,
    authority: &NativeStoreAuthority,
    subject: NativeEffectSubject,
    prepared: &PreparedNativeWrite,
    intent: &NativeResultIntentView,
) -> Option<Result<Value, H::Failure>> {
    recover_retained_as(
        host,
        port,
        store,
        backend,
        authority,
        subject,
        prepared,
        intent,
        SaveProof::Registered,
    )
}

/// [`recover_retained`] with the positive save `proof` this intent needs.
#[allow(clippy::too_many_arguments)]
pub fn recover_retained_as<H: NativeEffectHost + ?Sized>(
    host: &H,
    port: &dyn BotDelegatePort,
    store: &StoreWorkerClient,
    backend: &LocalBackend,
    authority: &NativeStoreAuthority,
    subject: NativeEffectSubject,
    prepared: &PreparedNativeWrite,
    intent: &NativeResultIntentView,
    proof: SaveProof,
) -> Option<Result<Value, H::Failure>> {
    let key = &intent.key;
    if intent.stage == NativeResultStage::Registered {
        return Some(saved(host, store, authority, intent));
    }
    if intent.stage == NativeResultStage::Refused {
        return Some(Err(host.failure("native_request_refused")));
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
            return Some(Err(host.failure("save_status_unknown")));
        };
        return Some(confirm(
            host,
            port,
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
            proof,
        ));
    }
    // Recover original Store proof before asking for fresh execution rights.
    // This can register a held creation after an access-narrowing barrier.
    match backend.reconcile_native_write(prepared, store_identity(subject, intent)) {
        Ok(NativeReconciliation::Verified(evidence)) => Some(confirm(
            host, port, store, authority, intent, evidence, proof,
        )),
        Ok(NativeReconciliation::Unconfirmed) if intent.stage == NativeResultStage::Prepared => {
            None
        }
        _ => Some(Err(host.failure("save_status_unknown"))),
    }
}

/// The acknowledged consumer's consume of the exact approved edit: one atomic
/// consume creates the native intent, its lease and the decision's
/// consumption together; the existing Store executor and receipt path then
/// runs once. A replay carries no Execute and only reconciles.
#[allow(clippy::too_many_arguments)]
pub fn consume<H: NativeEffectHost + ?Sized>(
    host: &H,
    store: &StoreWorkerClient,
    port: &dyn BotDelegatePort,
    backend: &LocalBackend,
    authority: &NativeStoreAuthority,
    subject: NativeEffectSubject,
    prepared: &PreparedNativeWrite,
    edit: &ApprovedEdit<'_>,
    view: &DecisionReceiptView,
    proof: &FreshBotPolicyProof,
    boot: DelegateBootId,
) -> Result<Value, H::Failure> {
    let fail = |code: &'static str| host.failure(code);
    let DecisionBindingV1::KoruEdit(binding) = &view.receipt.binding else {
        return Err(fail("native_operation_conflict"));
    };
    let description = prepared.description();
    let document = description
        .document
        .ok_or_else(|| fail("native_operation_conflict"))?;
    let title = native_document_title(store.clone(), authority.clone(), document)
        .map_err(|error| fail(refused_code(&error)))?;
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
            return Err(fail("native_execution_unavailable"));
        }
        // The consume may have committed its one lease: never run it blind.
        _ => return Err(fail("save_status_unknown")),
    };
    validate_intent(&intent, edit.key, prepared, edit.fingerprint, &fail)?;
    // The owner returned this intent for the one-use consume; it is the
    // decision's own edit only when the receipt's binding and acknowledged
    // consumer name exactly it (O152). Otherwise today's proof applies.
    let proof = if decision_consumed(view, edit.fence, &intent) {
        SaveProof::DecisionConsumed
    } else {
        SaveProof::Registered
    };
    if intent.disposition == NativeResultDisposition::Execute {
        let registered = execute_record_as(
            port,
            &intent,
            proof,
            || backend.execute_native_write(prepared, store_identity(subject, &intent)),
            || host.settled(),
            || host.uncertain(subject.owner, subject.bot, edit.key.clone()),
            &fail,
        )?;
        return saved(host, store, authority, &registered);
    }
    // Consumed replay is reconcile-only: never another Execute or Store
    // request, even when the original lease's outcome is still unknown.
    recover_retained_as(
        host, port, store, backend, authority, subject, prepared, &intent, proof,
    )
    .unwrap_or_else(|| Err(fail("save_status_unknown")))
}
