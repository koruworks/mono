// SPDX-License-Identifier: MPL-2.0

//! The trusted private read: the delegate's intent, the connection service's
//! one read under it, and the model-facing code of every way it can end
//! (M5 contract, sections 3.5 and 5.3).
//!
//! It lives here, below the gateway, as [`crate::native_effect`] does: the
//! gateway depends on this crate and not the other way round. It runs on the
//! gateway's blocking thread, off the AI worker's writer thread.
//!
//! One call makes at most one vendor read, and only for the one delegate
//! reply that carries `execute`. A replay is reconciled: it is told what the
//! retained row says, and nothing is sent to the vendor.
//!
//! Nothing here decides policy. The caller chose the path (a standing grant,
//! or the exact retry of an approved question) from the AI worker's plan;
//! the delegate repeats the grant predicate in its own writer and is
//! authoritative.
//!
//! **Content leaves this module only as [`PrivateReadContent`]**, whose
//! `Debug` prints sizes. No failure here carries a vendor message, a link, a
//! title or a label: every failure is one fixed code.

use koru_connections::{ConnectionService, PrivateReadContent, ReadCaller, ReadOutcome};
use koru_delegate_api::bots::{BotCommand, BotDelegatePort, BotReply};
use koru_delegate_core::{
    BotExecutionFence, DelegateBootId, FreshBotPolicyProof, PrivateEffectRefusal,
    PrivateEffectStage, PrivateEffectView, PrivateOperationProposal, RunId,
};
use std::time::{Duration, Instant};

/// The whole tool call's budget (contract 3.5). The vendor's share is the
/// service's own 20 seconds inside it.
pub const PRIVATE_READ_BUDGET: Duration = Duration::from_secs(30);

/// One read the caller decided to perform.
pub struct PrivateRead<'a> {
    /// The live consumer: the Run's current attempt, generation and turn.
    pub fence: &'a BotExecutionFence,
    pub proposal: &'a PrivateOperationProposal,
    pub proof: &'a FreshBotPolicyProof,
    pub boot: DelegateBootId,
    /// `true` for the exact retry of an approved question, which consumes
    /// its decision; `false` for a Run that holds a standing grant.
    pub approved: bool,
}

/// Why a read did not return content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivateReadRefusal {
    /// The model-facing code. Final for this call.
    Code(&'static str),
    /// The delegate's own predicate says the captured or the current policy
    /// lacks the grant. Nothing was committed. The caller asks the person,
    /// or tells a Routine it has no grant.
    NotGranted,
}

fn code(code: &'static str) -> PrivateReadRefusal {
    PrivateReadRefusal::Code(code)
}

/// The model-facing code of one recorded refusal.
///
/// A Run that holds the grant is told the specific reason. A Run that was
/// only allowed once is told one thing for every refusal: the approved read
/// was not performed (contract 5.3). Two codes have no row of their own in
/// the contract's table and take the general one for both: a vendor that
/// rejected the request, and a read that was not sent.
pub fn refusal_code(refusal: PrivateEffectRefusal, granted: bool) -> &'static str {
    use PrivateEffectRefusal as R;
    if !granted {
        return "private_read_not_done";
    }
    match refusal {
        R::CredentialExpired | R::CredentialMissing => "connection_needs_sign_in",
        R::AccessDenied | R::Revoked | R::WrongAccount => "connection_revoked",
        R::CredentialLocked => "credential_locked",
        R::ItemUnavailable => "item_unavailable",
        R::DestinationChanged => "item_changed",
        R::NotDispatched | R::VendorRejected | R::Unavailable => "private_read_not_done",
    }
}

/// What a read's outcome tells the model. Only `Delivered` is content.
pub fn outcome(
    outcome: ReadOutcome,
    granted: bool,
) -> Result<PrivateReadContent, PrivateReadRefusal> {
    match outcome {
        ReadOutcome::Delivered(content) => Ok(content),
        ReadOutcome::Refused(refusal) => Err(code(refusal_code(refusal, granted))),
        // Access was removed, or the account disconnected, while the read
        // was in flight. Its content was dropped.
        ReadOutcome::Withheld => Err(code("connection_revoked")),
        ReadOutcome::ResultUnavailable => Err(code("private_result_unavailable")),
        ReadOutcome::InProgress => Err(code("private_read_in_progress")),
        // Not known, nobody left to receive it, or another process's row:
        // the model may not assume the read, and it is never repeated.
        ReadOutcome::Unknown | ReadOutcome::CallerGone | ReadOutcome::NotOurs => {
            Err(code("private_outcome_unknown"))
        }
    }
}

fn ai_owner(fence_owner: koru_delegate_core::BotOwner) -> koru_ai_core::bots::BotOwner {
    koru_ai_core::bots::BotOwner {
        user_scope: fence_owner.user_scope,
        device: fence_owner.device,
    }
}

/// The retained effect of one operation for this Run, if the delegate holds
/// one. `Err` when the delegate could not say: the caller must not go on to
/// a fresh intent, which could be a second one.
pub fn retained(
    port: &dyn BotDelegatePort,
    run: RunId,
    operation_id: uuid::Uuid,
    budget: Instant,
) -> Result<Option<PrivateEffectView>, PrivateReadRefusal> {
    let command = BotCommand::ReadPrivateEffect {
        run,
        operation_id,
        dispatcher: koru_connections::process_dispatcher(),
    };
    match port.execute_within(command, budget.saturating_duration_since(Instant::now())) {
        Ok(BotReply::Missing) => Ok(None),
        Ok(BotReply::PrivateEffect { effect })
            if effect.key.run == run && effect.key.operation_id == operation_id =>
        {
            Ok(Some(effect))
        }
        Err(failure) if failure.code == "bot_private_unavailable" => {
            Err(code("private_unavailable"))
        }
        _ => Err(code("native_owner_unavailable")),
    }
}

/// Answer an exact retry from the retained row. Nothing is sent to the
/// vendor: the reply that carried `execute` was the first one.
pub fn reconcile(
    service: &ConnectionService,
    owner: koru_delegate_core::BotOwner,
    effect: &PrivateEffectView,
    caller: &dyn ReadCaller,
    budget: Instant,
) -> Result<PrivateReadContent, PrivateReadRefusal> {
    // A row with a decision was allowed once; one without was granted.
    let granted = effect.decision.is_none();
    // A final row says what happened, whoever opened it: a row another
    // process left was closed by the delegate before it answered.
    match effect.stage {
        PrivateEffectStage::Confirmed if effect.withheld == Some(true) => {
            Err(code("connection_revoked"))
        }
        PrivateEffectStage::Confirmed => Err(code("private_result_unavailable")),
        PrivateEffectStage::Refused => Err(code(refusal_code(
            effect.refusal.unwrap_or(PrivateEffectRefusal::Unavailable),
            granted,
        ))),
        PrivateEffectStage::Unknown => Err(code("private_outcome_unknown")),
        // Still open: this process's own row. The service says whether a
        // call here is performing it, and closes it if none is.
        PrivateEffectStage::Prepared | PrivateEffectStage::Dispatched => outcome(
            service.read(ai_owner(owner), effect, caller, budget),
            granted,
        ),
    }
}

/// Record the intent at the delegate and perform the read under it, in the
/// same call (contract 3.5, steps 1 to 5).
pub fn perform(
    port: &dyn BotDelegatePort,
    service: &ConnectionService,
    read: &PrivateRead<'_>,
    caller: &dyn ReadCaller,
    budget: Instant,
) -> Result<PrivateReadContent, PrivateReadRefusal> {
    let dispatcher = service.dispatcher();
    let command = if read.approved {
        BotCommand::ConsumePrivateDecision {
            fence: read.fence.clone(),
            dispatcher,
            operation: read.proposal.clone(),
            proof: read.proof.clone(),
            boot: read.boot,
        }
    } else {
        BotCommand::PreparePrivateEffect {
            fence: read.fence.clone(),
            dispatcher,
            operation: read.proposal.clone(),
            proof: read.proof.clone(),
            boot: read.boot,
        }
    };
    let effect =
        match port.execute_within(command, budget.saturating_duration_since(Instant::now())) {
            Ok(BotReply::PrivateEffect { effect })
                if effect.key.run == read.fence.run
                    && effect.key.operation_id == read.proposal.operation_id
                    && effect.operation == *read.proposal =>
            {
                effect
            }
            // An answer about another operation says nothing about this one, and
            // the intent may have been recorded.
            Ok(_) => return Err(code("private_outcome_unknown")),
            Err(failure) => {
                return Err(match failure.code.as_str() {
                    "bot_connection_not_granted" => PrivateReadRefusal::NotGranted,
                    // The O147 fence: another approval owns the Run.
                    "bot_decision_owned" => code("decision_in_progress"),
                    "bot_conflict" => code("native_operation_conflict"),
                    // This process's dispatcher still has another read open.
                    "bot_private_effect_open" => code("private_read_in_progress"),
                    "bot_private_unavailable" => code("private_unavailable"),
                    // Any other typed refusal committed nothing.
                    other if other.starts_with("bot_") && !failure.retryable => {
                        code("native_execution_unavailable")
                    }
                    // Transport loss or a timeout: the intent may be recorded.
                    // The exact retry reads the retained row; nothing is resent.
                    _ => code("private_outcome_unknown"),
                });
            }
        };
    outcome(
        service.read(ai_owner(read.fence.owner), &effect, caller, budget),
        !read.approved,
    )
}

#[cfg(test)]
mod tests;
