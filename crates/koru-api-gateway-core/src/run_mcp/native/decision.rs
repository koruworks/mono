// SPDX-License-Identifier: MPL-2.0

//! One-use Koru edit decisions on the native `edit_document` path (plan M4
//! P4.2, slice F; contract sections 1–3 and 9).
//!
//! Only a **positively typed selected-page denial** moves to a decision, and
//! only before the operation's first Prepare: the existing native lookup has
//! already answered Missing, so no native intent, lease or Store request
//! exists for it. Every other failure keeps its existing refusal: a
//! definite (precommit) owner refusal of the decision returns to the
//! ordinary lease path, which refuses exactly as before. The owner, not this
//! classification, is authoritative: the ledger repeats the selected-page
//! predicate (and refuses a page this Run created) in its own writer.
//!
//! The decision command is idempotent: the delegate replays the Run's open
//! decision for this exact operation before any fresh check, so the same
//! command is the retained lookup, the fresh preparation and the bounded
//! read of the initial projection's acknowledgment. Only a positively
//! acknowledged initial projection (`Pending/Holding` or `Pending/Held`)
//! answers `permission_pending`; after the command may have committed, any
//! uncertainty answers `decision_status_unknown`, keyed by the original
//! operation id. The acknowledged consumer's retry of the same edit is handed
//! back to the caller for the one atomic consume.
//!
//! **Closed in this build** ([`EDIT_DECISIONS_AVAILABLE`]): the delivery lead
//! reviews the coupled exposure candidate before any availability change.
use super::dispatch::failure;
use koru_artifact_wire::run_tools::ToolFailure;
use koru_delegate_api::bots::{
    BotBootScan, BotCommand, BotDelegatePort, BotReply, DecisionEditProposal, DecisionReceiptView,
};
use koru_delegate_core::decisions::{
    DecisionAuthorization, DecisionBindingV1, DecisionEditAction, DecisionPayload, DecisionPhase,
    DecisionResumeState, DecisionValidate, DecisionVersionVector,
};
use koru_delegate_core::{
    BotAccess, BotExecutionFence, BotOwner, BotProvenance, CapturedBotPolicy, DelegateBootId,
    DocumentId, FreshBotPolicyProof, NativeResultKey, WorkspaceId, same_native_edit_revision,
};
use koru_mcp_server::native::{NativeWriteDescription, NativeWriteKind};
use serde_json::{Value, json};
use std::time::Duration;

/// Availability gate for answerable edit decisions. While `false` the native
/// edit path is exactly the ordinary one and no decision command is sent.
/// Opening it is an availability transition the delivery lead reviews.
pub(super) const EDIT_DECISIONS_AVAILABLE: bool = false;

/// How long the immediate response may wait for the initial projection's
/// positive acknowledgment, as a bounded number of exact replays.
#[derive(Clone, Copy, Debug)]
pub(super) struct DecisionPacing {
    /// Replays after the first answer; the loop never exceeds this.
    pub replays: u8,
    pub first: Duration,
    pub ceiling: Duration,
}
impl DecisionPacing {
    /// About two seconds in all, on the caller's blocking thread.
    const PRODUCTION: Self = Self {
        replays: 7,
        first: Duration::from_millis(25),
        ceiling: Duration::from_millis(500),
    };
}

/// Whether this call may move a selected-page denial to a decision.
#[derive(Clone, Copy, Debug)]
pub(super) enum EditDecisions {
    Closed,
    Open(DecisionPacing),
}
impl EditDecisions {
    pub(super) fn production() -> Self {
        if EDIT_DECISIONS_AVAILABLE {
            Self::Open(DecisionPacing::PRODUCTION)
        } else {
            Self::Closed
        }
    }
}

/// A positively typed selected-page write denial of one existing page.
/// Constructed only by [`selected_page_denial`]; it is a reason to ask the
/// owner, never permission to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PageWriteDenial {
    target: DocumentId,
}

/// The only refusal that may become a decision (contract section 1, O98):
/// an `edit_document` of an existing page in the Run's own home, under an
/// interactive (non-Routine, O105) Bot whose captured or current policy does
/// not grant the page while every other identity matches and effects are
/// allowed. Anything else is `None` and keeps its existing refusal.
pub(super) fn selected_page_denial(
    provenance: &BotProvenance,
    captured: &CapturedBotPolicy,
    proof: &FreshBotPolicyProof,
    workspace: WorkspaceId,
    description: &NativeWriteDescription,
) -> Option<PageWriteDenial> {
    let current = &proof.global.policy;
    let target = description.document?;
    let homed =
        |p: &CapturedBotPolicy| p.valid() && p.workspace == workspace && p.destination == workspace;
    let grants = |p: &CapturedBotPolicy| {
        p.access == BotAccess::Workspace || p.selected_pages.contains(&target)
    };
    let eligible = description.kind == NativeWriteKind::Edited
        && provenance.routine.is_none()
        && proof.routine.is_none()
        && proof.global.effects_allowed
        && description.workspace == workspace
        && description.expected_revision.is_some()
        && homed(captured)
        && homed(current)
        && !(grants(captured) && grants(current));
    eligible.then_some(PageWriteDenial { target })
}

/// The exact denied edit, as the decision binding retains it.
pub(super) struct DeniedEdit<'a> {
    pub fence: &'a BotExecutionFence,
    pub key: &'a NativeResultKey,
    pub denial: PageWriteDenial,
    pub fingerprint: [u8; 32],
    pub expected_revision: &'a [u8],
    pub payload: &'a DecisionPayload,
}

/// The canonical bounded payload of the denied arguments (without
/// `operation_id`): sorted keys, compact. `None` when it cannot be retained,
/// which keeps the existing refusal.
pub(super) fn payload(arguments: &Value) -> Option<DecisionPayload> {
    let bytes = serde_json::to_vec(arguments).ok()?;
    DecisionPayload::new(bytes).ok()
}

/// What the decision path settled.
pub(super) enum DecisionOutcome {
    /// The exact acknowledged consumer retried the edit: consume it once.
    Consume {
        view: Box<DecisionReceiptView>,
        boot: DelegateBootId,
    },
    /// A definite owner refusal before anything was committed: keep the
    /// ordinary path and its existing refusal.
    Ordinary,
}

enum Sent {
    Decision(Box<DecisionReceiptView>),
    /// Another decision (or no-decision cleanup) owns the Run.
    Owned,
    /// The same operation with a different edit.
    Conflict,
    /// A definite precommit refusal of a fresh preparation.
    Refused,
    /// After the command may have committed, nothing positive came back.
    Unknown,
}

/// Run the decision path for one denied edit. `fresh` performs the existing
/// fresh policy synchronization and effect fence; it runs only when the
/// first command found nothing to replay and its preparation was refused.
/// `Err` is the final tool answer (pending, unknown, in progress or refusal).
pub(super) fn decide(
    port: &dyn BotDelegatePort,
    owner: BotOwner,
    edit: &DeniedEdit<'_>,
    proof: &FreshBotPolicyProof,
    pacing: DecisionPacing,
    fresh: impl FnOnce() -> Result<(), ToolFailure>,
) -> Result<DecisionOutcome, ToolFailure> {
    let Some(boot) = probe_boot(port, owner) else {
        // Nothing decision-related was sent: the ordinary path answers.
        return Ok(DecisionOutcome::Ordinary);
    };
    let first = match send(port, edit, proof, boot) {
        Sent::Refused => {
            fresh()?;
            send(port, edit, proof, boot)
        }
        other => other,
    };
    match first {
        Sent::Decision(view) => answer(port, edit, proof, boot, pacing, view),
        Sent::Refused => Ok(DecisionOutcome::Ordinary),
        Sent::Owned => Err(in_progress(edit.key)),
        Sent::Conflict => Err(failure("native_operation_conflict")),
        Sent::Unknown => Err(status_unknown(edit.key)),
    }
}

fn probe_boot(port: &dyn BotDelegatePort, owner: BotOwner) -> Option<DelegateBootId> {
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

fn send(
    port: &dyn BotDelegatePort,
    edit: &DeniedEdit<'_>,
    proof: &FreshBotPolicyProof,
    boot: DelegateBootId,
) -> Sent {
    let Ok(expected_revision) = DecisionVersionVector::new(edit.expected_revision.to_vec()) else {
        return Sent::Refused;
    };
    let command = BotCommand::PrepareEditDecision {
        fence: edit.fence.clone(),
        edit: DecisionEditProposal {
            target: edit.denial.target,
            action: DecisionEditAction::EditDocument,
            operation_id: edit.key.operation_id,
            edit_fingerprint: edit.fingerprint,
            expected_revision,
            payload: edit.payload.clone(),
        },
        proof: proof.clone(),
        boot,
    };
    match port.execute(command) {
        Ok(BotReply::Decision { decision }) => Sent::Decision(Box::new(decision)),
        Ok(_) => Sent::Unknown,
        Err(f) if f.code == "bot_decision_owned" => Sent::Owned,
        Err(f) if f.code == "bot_conflict" => Sent::Conflict,
        // The actor answered a typed refusal: its transaction did not commit.
        Err(f) if f.code.starts_with("bot_") && !f.retryable => Sent::Refused,
        // Transport loss, a timeout, an unexpected or a retryable answer:
        // the preparation may have committed.
        Err(_) => Sent::Unknown,
    }
}

fn answer(
    port: &dyn BotDelegatePort,
    edit: &DeniedEdit<'_>,
    proof: &FreshBotPolicyProof,
    boot: DelegateBootId,
    pacing: DecisionPacing,
    mut view: Box<DecisionReceiptView>,
) -> Result<DecisionOutcome, ToolFailure> {
    let mut delay = pacing.first;
    for replay in 0..=pacing.replays {
        if !same_edit(&view, edit) {
            return Err(failure("native_operation_conflict"));
        }
        // Every positive, validated receipt of this exact edit passes here
        // (the fresh reply on the first pass, each replay after). INTEGRATION
        // (AI owner's `AiWorkerClient::record_bot_decision`, not on this
        // base): hand `(*view).clone()` to the AI here, before any answer.
        if consumer(&view, edit.fence) {
            return Ok(DecisionOutcome::Consume { view, boot });
        }
        if !origin(&view, edit.fence) {
            return Err(failure("native_execution_unavailable"));
        }
        let state = &view.receipt.state;
        match (state.authorization, state.phase) {
            (DecisionAuthorization::Pending, DecisionPhase::Holding | DecisionPhase::Held) => {
                return Err(pending(&view, edit.key));
            }
            (DecisionAuthorization::Pending, DecisionPhase::Publishing) => {}
            (DecisionAuthorization::Denied, _) => return Err(closed(edit.key, "decision_denied")),
            (DecisionAuthorization::Expired, _) => {
                return Err(closed(edit.key, "decision_expired"));
            }
            (DecisionAuthorization::Canceled, _) => {
                return Err(closed(edit.key, "decision_canceled"));
            }
            // Granted or consumed: the recorded continuation owns it now.
            (DecisionAuthorization::Granted | DecisionAuthorization::Consumed, _) => {
                return Err(in_progress(edit.key));
            }
            _ => return Err(status_unknown(edit.key)),
        }
        if replay == pacing.replays {
            break;
        }
        // The initial projection has not been acknowledged yet. Replay is a
        // read in the owner; it writes nothing and grants nothing.
        std::thread::sleep(delay);
        delay = (delay * 2).min(pacing.ceiling);
        view = match send(port, edit, proof, boot) {
            Sent::Decision(next) => next,
            _ => return Err(status_unknown(edit.key)),
        };
    }
    Err(status_unknown(edit.key))
}

fn same_edit(view: &DecisionReceiptView, edit: &DeniedEdit<'_>) -> bool {
    let DecisionBindingV1::KoruEdit(binding) = &view.receipt.binding else {
        return false;
    };
    view.receipt.validate().is_ok()
        && binding.run == edit.key.run
        && binding.owner == edit.fence.owner
        && binding.bot == edit.fence.bot
        && binding.action == DecisionEditAction::EditDocument
        && same_native_edit_revision(edit.expected_revision, binding.expected_revision.as_slice())
            .unwrap_or(false)
        && view.receipt.binding.matches_edit_except_revision(
            edit.key.operation_id,
            edit.denial.target,
            edit.fingerprint,
            edit.payload.as_slice(),
        )
}

fn origin(view: &DecisionReceiptView, fence: &BotExecutionFence) -> bool {
    let f = view.receipt.binding.origin();
    f.attempt == fence.attempt
        && f.generation == fence.generation
        && f.session == fence.session
        && Some(f.turn) == fence.turn
}

/// The exact acknowledged consumer: the retained resume names this attempt,
/// generation, session and acknowledged input turn.
pub(super) fn consumer(view: &DecisionReceiptView, fence: &BotExecutionFence) -> bool {
    view.resume.as_ref().is_some_and(|resume| {
        let (attempt, generation, session, _) = resume.consumer_identity();
        let turn = match &resume.state {
            DecisionResumeState::InputAcknowledged { acknowledgment }
            | DecisionResumeState::Closed {
                acknowledgment: Some(acknowledgment),
            } => Some(acknowledgment.turn),
            _ => None,
        };
        resume.decision == view.receipt.decision
            && attempt == fence.attempt
            && generation == fence.generation
            && session == Some(fence.session)
            && turn.is_some()
            && turn == fence.turn
    })
}

fn pending(view: &DecisionReceiptView, key: &NativeResultKey) -> ToolFailure {
    let receipt = &view.receipt;
    let mut answer = ToolFailure::new(
        "permission_pending",
        "A person must approve this edit before it is made. Stop here: the task continues after their answer. If you retry, use the same operation_id and identical arguments; never mint a new operation for this edit.",
    );
    answer.details = Some(json!({
        "operation_id": key.operation_id,
        "decision_id": receipt.decision,
        "revision": receipt.state.revision.get(),
        "binding_digest": receipt.binding_digest,
        "deadline_at_ms": receipt.deadline.deadline_at_ms,
        "authorization": receipt.state.authorization,
        "phase": receipt.state.phase,
    }));
    answer
}

pub(super) fn status_unknown(key: &NativeResultKey) -> ToolFailure {
    keyed(
        key,
        "decision_status_unknown",
        "Whether this edit is waiting for a person's approval could not be confirmed. It is not evidence that no request exists. Retry with the same operation_id and identical arguments; never mint a new operation for this edit.",
    )
}

fn in_progress(key: &NativeResultKey) -> ToolFailure {
    keyed(
        key,
        "decision_in_progress",
        "Another approval request owns this task. No new edit can be asked until it is answered; do not retry this edit under a new operation.",
    )
}

fn closed(key: &NativeResultKey, code: &'static str) -> ToolFailure {
    keyed(
        key,
        code,
        "The approval request for this edit is closed; the edit was not authorized.",
    )
}

fn keyed(key: &NativeResultKey, code: &'static str, message: &'static str) -> ToolFailure {
    let mut answer = ToolFailure::new(code, message);
    answer.details = Some(json!({"operation_id": key.operation_id}));
    answer
}

#[cfg(test)]
mod tests;
