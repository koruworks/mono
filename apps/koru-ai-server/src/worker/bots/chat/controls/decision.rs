// SPDX-License-Identifier: MPL-2.0
//! A person's answer to a decision's card, through the private Bot decision
//! route (`AnswerDecision`), on the existing shared approval controls.
//!
//! The card names its decision by its retained link, never by parsing a
//! native id. A Koru card answers AllowOnce or Deny; a native card answers one
//! of the prompt's own offered choices. The answer carries current human
//! proof (an off-writer authority read whose profile must be the decision's
//! human), the stable request id of this human answer and the exact revision:
//! a Koru revision comes from a positive reread of the wait, a native one from
//! the retained receipt. Exact replay of an answer already sent resends its
//! first form unchanged. The delegate's ledger owns every check that grants.
use super::*;
use crate::worker::bots::decision_wait::{DecisionWaitDelegate, DecisionWaitSubject};
use koru_ai_sqlite::BotDecisionCard;
use koru_delegate_api::bots::DecisionAnswerRequest;
use koru_delegate_core::FreshBotPolicyProof;
use koru_delegate_core::decisions::{
    DecisionAnswerChoice, DecisionAnswerId, DecisionAuthorization, DecisionBindingV1,
    DecisionDigest, DecisionDisposition, DecisionId, DecisionNativeChoice, DecisionPhase,
};
use koru_session_protocol::ApprovalIntent;

#[cfg(test)]
mod tests;

/// The person-facing sentence for an answer that was not applied. It already
/// exists on this route; no new wording is introduced here.
const ANSWER_NOT_APPLIED: &str = "The current Bot request could not be answered. Its latest state will appear in the conversation.";

/// How an answer that was not applied appears to the person. A retryable
/// failure is unknown completion: the card's latest state will follow. The
/// legacy `bot_decision_owned` refusal (O136) and every other definite
/// refusal read the same way; lost authority reads as an inactive request.
pub(in crate::worker) fn person_failure(code: &str, retryable: bool) -> Failure {
    if retryable {
        return failure("tool_not_resumable", ANSWER_NOT_APPLIED, true);
    }
    match code {
        "bot_fenced"
        | "bot_owner_mismatch"
        | "bot_boot_stale"
        | "bot_routine_decision_unsupported"
        | "decision_human_changed" => tool_not_resumable(),
        _ => failure("tool_not_resumable", ANSWER_NOT_APPLIED, false),
    }
}

fn refused(code: &str) -> koru_delegate_api::Failure {
    koru_delegate_api::Failure::new(code, "The decision answer was not sent.", false)
}
fn unknown() -> koru_delegate_api::Failure {
    koru_delegate_api::Failure::new("bot_unavailable", "Bot control unavailable.", true)
}

/// A v4-shaped UUID from 32 derived bytes: stable for the same input.
fn derived_uuid(domain: &str, material: &[u8]) -> Uuid {
    let bytes = blake3::derive_key(domain, material);
    let mut id = [0u8; 16];
    id.copy_from_slice(&bytes[..16]);
    id[6] = (id[6] & 0x0f) | 0x40;
    id[8] = (id[8] & 0x3f) | 0x80;
    Uuid::from_bytes(id)
}

/// One human answer's stable request id: the same decision and the same
/// choice give the same id across retries; a different choice never does.
pub(in crate::worker) fn answer_request_id(
    decision: DecisionId,
    choice: &DecisionAnswerChoice,
) -> ApiResult<DecisionAnswerId> {
    let mut material = decision.as_uuid().as_bytes().to_vec();
    material.extend(serde_json::to_vec(choice).map_err(|_| tool_decision_conflict())?);
    derived_uuid("koru.bot.decision.answer-request.v1", &material)
        .to_string()
        .parse()
        .map_err(|_| tool_decision_conflict())
}

/// O138: the session key of one ordinary native answer, stable across that
/// answer's retries, and different for a different decision or choice on the
/// same prompt (a reject and a later allow never share a key).
pub(in crate::worker) fn native_answer_key(
    approval: koru_session_protocol::ApprovalId,
    decision: koru_session_protocol::ApprovalDecision,
    choice: Option<koru_session_protocol::ApprovalChoiceId>,
) -> ApiResult<koru_delegate_core::IdempotencyKey> {
    let approve = decision == koru_session_protocol::ApprovalDecision::Approve;
    let material = format!(
        "{approval}|{}|{}",
        if approve { "approve" } else { "reject" },
        choice.map(|c| c.to_string()).unwrap_or_default()
    );
    derived_uuid("koru.bot.native.answer-request.v1", material.as_bytes())
        .to_string()
        .parse()
        .map_err(|_| tool_decision_conflict())
}

/// An Allow on a private read whose connection changed since the question
/// was asked. Nothing was forwarded and nothing was read. The text is fixed:
/// it names no account and no file.
pub(in crate::worker) fn private_connection_changed() -> Failure {
    failure(
        "bot_connection_changed",
        "This connection changed, so this can no longer be allowed. Nothing was read.",
        false,
    )
}

/// The person's choice on this card, in the decision's own vocabulary.
pub(in crate::worker) fn card_choice(
    binding: &DecisionBindingV1,
    choice: Option<&str>,
    answers: bool,
    approve: bool,
) -> ApiResult<DecisionAnswerChoice> {
    if answers {
        return Err(tool_decision_conflict());
    }
    match binding {
        // A private read is answered as an edit is (M5): Allow once or Deny,
        // with no offered choice. "Allow once" changes no grant.
        DecisionBindingV1::KoruEdit(_) | DecisionBindingV1::PrivateOperation(_) => {
            if choice.is_some() {
                return Err(tool_decision_conflict());
            }
            Ok(if approve {
                DecisionAnswerChoice::AllowOnce {}
            } else {
                DecisionAnswerChoice::Deny {}
            })
        }
        DecisionBindingV1::NativeApproval(native) => Ok(DecisionAnswerChoice::Native {
            offered: native_choice(native.choices.as_slice(), choice, approve)?,
        }),
    }
}

/// One of the native prompt's own offered choices, matched by its offered id
/// (never parsed as a decision id) or, without one, the single plain Allow or
/// Reject. A choice that does not match the button pressed refuses.
pub(in crate::worker) fn native_choice(
    offered: &[DecisionNativeChoice],
    choice: Option<&str>,
    approve: bool,
) -> ApiResult<DecisionNativeChoice> {
    let pick = match choice {
        Some(choice) => offered.iter().find(|o| o.id.to_string() == choice),
        None => {
            let wanted = if approve {
                ApprovalIntent::Allow
            } else {
                ApprovalIntent::Reject
            };
            let mut plain = offered.iter().filter(|o| o.intent == wanted);
            match (plain.next(), plain.next()) {
                (Some(one), None) => Some(one),
                _ => None,
            }
        }
    }
    .ok_or_else(tool_decision_conflict)?;
    if pick.intent.grants() != approve {
        return Err(tool_decision_conflict());
    }
    Ok(pick.clone())
}

/// The receipt a card's answer must name. Gone (an AI restart, or never
/// handed over) means the card is no longer active: refused before any job
/// is built, so no answer is ever sent without it.
pub(in crate::worker) fn card_receipt<'a>(
    waits: &'a crate::worker::bots::decision_wait::DecisionWaitDriver,
    card: &BotDecisionCard,
) -> ApiResult<&'a koru_delegate_api::bots::DecisionReceiptView> {
    waits.receipt(card.decision).ok_or_else(tool_not_resumable)
}

/// Digest of the off-writer human proof the answer was authorized under.
fn human_proof_digest(
    answer: &DecisionAnswerRequest,
    profile: koru_ai_core::ProfileId,
    proof: &FreshBotPolicyProof,
) -> DecisionDigest {
    let material = serde_json::to_vec(&(
        answer.owner,
        answer.decision,
        answer.request,
        profile,
        proof.global.config_revision,
        proof.global.policy_revision,
    ))
    .unwrap_or_default();
    DecisionDigest::from_bytes(blake3::derive_key(
        "koru.bot.decision.human-proof.v1",
        &material,
    ))
}

/// What the job needs off the writer to complete one answer.
pub(in crate::worker) struct DecisionHuman {
    pub(super) authority: Arc<dyn BotAuthority>,
    pub(super) owner: BotOwner,
    pub(super) home: WorkspaceId,
    pub(super) human: koru_ai_core::ProfileId,
    /// Koru only: the positive reread of the wait's current revision.
    pub(super) reread: Option<(Arc<dyn DecisionWaitDelegate>, DecisionWaitSubject)>,
    pub(super) card: BotDecisionCard,
    /// An exact replay of the answer's first sent form: sent unchanged.
    pub(super) replay: bool,
}
/// The answer as it was actually sent, for the writer's completion.
pub(in crate::worker) struct DecisionSent {
    pub(super) card: BotDecisionCard,
    pub(super) answer: DecisionAnswerRequest,
    pub(super) koru: bool,
}

/// Off the writer: human proof, Koru reread, then the one delegate call.
pub(super) fn answer(
    human: &DecisionHuman,
    command: DelegateCommand,
    port: &dyn BotDelegatePort,
) -> (
    Result<BotReply, koru_delegate_api::Failure>,
    Option<Box<DecisionSent>>,
) {
    let DelegateCommand::AnswerDecision {
        mut answer,
        proof,
        boot,
    } = command
    else {
        return (Err(refused("bot_invalid")), None);
    };
    if !human.replay {
        let checked = match human.authority.inspect(&[human.home], &[]) {
            Ok(checked) => checked,
            Err(_) => return (Err(unknown()), None),
        };
        let profile = checked.workspace(human.home);
        if checked.owner != human.owner || profile.as_ref() != Ok(&human.human) {
            return (Err(refused("decision_human_changed")), None);
        }
        answer.authority_digest = human_proof_digest(&answer, human.human, &proof);
        if let Some((delegate, subject)) = &human.reread {
            let observed = match delegate.read_decision_wait(
                subject,
                koru_delegate_api::routines::ROUTINE_BRIDGE_DEADLINE,
            ) {
                Ok(Some(observed)) => observed,
                Ok(None) => return (Err(refused("bot_stale")), None),
                Err(_) => return (Err(unknown()), None),
            };
            match &observed.disposition {
                DecisionDisposition::Open { state }
                    if state.authorization == DecisionAuthorization::Pending
                        && state.phase == DecisionPhase::Held =>
                {
                    answer.expected_revision = state.revision;
                }
                // Not answerable now (still holding, or already moved on).
                _ => return (Err(refused("bot_stale")), None),
            }
        }
    }
    let sent = Box::new(DecisionSent {
        card: human.card.clone(),
        answer: answer.clone(),
        koru: matches!(
            answer.choice,
            DecisionAnswerChoice::AllowOnce {} | DecisionAnswerChoice::Deny {}
        ),
    });
    (
        port.execute(DelegateCommand::AnswerDecision {
            answer,
            proof,
            boot,
        }),
        Some(sent),
    )
}

/// The barrier of the grant a Bot Routine runs under now, from the one reader
/// (`routine_current_grant_of`) over its Bound creation in the Bot's home.
/// Anything else (no such Routine, not Bound, another home) is not
/// answerable: the card reads as no longer active.
pub(in crate::worker) fn routine_grant_barrier(
    repository: &Repository,
    bot: &Bot,
    schedule: koru_delegate_core::ScheduleId,
) -> ApiResult<koru_delegate_core::RoutinePolicyBarrier> {
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let creation = repository
        .bot_routine_creation(owner, bot.id, schedule)
        .map_err(|_| tool_not_resumable())?
        .ok_or_else(tool_not_resumable)?;
    if creation.state != koru_ai_core::bots::BotRoutineCreationState::Bound
        || creation.identity.home != bot.home_workspace
    {
        return Err(tool_not_resumable());
    }
    let current = repository
        .routine_current_grant_of(&creation)
        .map_err(|_| tool_not_resumable())?;
    Ok(crate::worker::bots::policy::current_routine_barrier(
        &creation.identity,
        &current,
    ))
}

impl WorkerState {
    /// The Routine half of a decision's fresh proof: `None` for an
    /// interactive decision; for a Routine decision, the barrier of the grant
    /// the Routine runs under now, from the one reader (never today's Bot
    /// settings). The delegate's ledger compares it with the decision's
    /// binding and its own current row; this only says what is current.
    pub(in crate::worker) fn decision_routine_proof(
        &self,
        bot: &Bot,
        binding: &DecisionBindingV1,
    ) -> ApiResult<Option<koru_delegate_core::RoutinePolicyBarrier>> {
        let Some(subject) = binding.routine() else {
            return Ok(None);
        };
        routine_grant_barrier(&self.repository, bot, subject.provenance.schedule).map(Some)
    }

    /// Prepare one card answer on the writer. `None` from the registry means
    /// the decision's receipt was never handed over: honestly not answerable.
    pub(super) fn prepare_decision_answer(
        &self,
        context: &CommandContext,
        bot: &Bot,
        row: &ToolActivity,
        card: BotDecisionCard,
        decision: &koru_ai_wire::ToolDecisionRequest,
        approve: bool,
        reply: &SyncSender<ApiResult<ToolDecisionResponse>>,
    ) -> ApiResult<ControlJob> {
        if card.ai_attempt != row.attempt_id {
            return Err(tool_decision_conflict());
        }
        let view = card_receipt(&self.decision_waits, &card)?;
        let receipt = &view.receipt;
        let (owner, bot_id, run, human, routine, koru) = match &receipt.binding {
            DecisionBindingV1::KoruEdit(v) => (
                v.owner,
                v.bot,
                v.run,
                v.human_profile,
                v.routine.is_some(),
                true,
            ),
            DecisionBindingV1::NativeApproval(v) => (
                v.owner,
                v.bot,
                v.run,
                v.human_profile,
                v.routine.is_some(),
                false,
            ),
            DecisionBindingV1::PrivateOperation(v) => (
                v.owner,
                v.bot,
                v.run,
                v.human_profile,
                v.routine.is_some(),
                true,
            ),
        };
        if receipt.decision != card.decision
            || bot_id != bot.id
            || run != card.run
            || owner.user_scope != bot.user_scope
            || owner.device != bot.owner_device
        {
            return Err(tool_decision_conflict());
        }
        // A Routine Koru edit is answered, under the Routine's own current
        // grant (P4.3 section 4.10). A native prompt on a Routine Run is not
        // a decision (O125) and stays refused.
        if routine && !koru {
            return Err(tool_not_resumable());
        }
        // A Routine Run never has a private question (M5, D6). The delegate
        // refuses to prepare one; a receipt that claims one is not answered.
        let private = crate::worker::connections::plan::private_binding(&receipt.binding);
        if routine && private.is_some() {
            return Err(tool_not_resumable());
        }
        let routine = self.decision_routine_proof(bot, &receipt.binding)?;
        let choice = card_choice(
            &receipt.binding,
            decision.choice.as_deref(),
            decision.answers.is_some(),
            approve,
        )?;
        let request = answer_request_id(card.decision, &choice)?;
        let (answer, replay) = match self.decision_waits.answered(request) {
            Some(first) => (first.clone(), true),
            None => (
                DecisionAnswerRequest {
                    owner,
                    run,
                    decision: card.decision,
                    expected_revision: receipt.state.revision,
                    binding_digest: receipt.binding_digest,
                    request,
                    human,
                    choice,
                    authority_digest: DecisionDigest::from_bytes([0; 32]),
                },
                false,
            ),
        };
        // Before a first Allow of a private read is forwarded, the
        // connection and the item are read again (M5, contract 3.6): if the
        // account is no longer connected at the revision the question was
        // asked for, or the file is no longer the one chosen, the Allow is
        // not sent. A Deny is always forwarded, and an Allow already sent is
        // replayed as it was.
        if let Some(private) = private
            && approve
            && !replay
            && !self.private_question_current(
                BotOwner {
                    user_scope: bot.user_scope,
                    device: bot.owner_device,
                },
                &private.proposal(),
            )
        {
            return Err(private_connection_changed());
        }
        let global = match self.lifecycle_policy_barrier(bot)? {
            Some(barrier) => barrier,
            None => super::super::super::policy::barrier(bot)?,
        };
        let reread = if koru && !replay {
            Some((
                self.decision_waits
                    .delegate()
                    .ok_or_else(tool_not_resumable)?,
                DecisionWaitSubject {
                    owner,
                    bot: bot_id,
                    run,
                    wait_id: view.wait,
                    decision_id: card.decision,
                },
            ))
        } else {
            None
        };
        let command = DelegateCommand::AnswerDecision {
            answer,
            proof: FreshBotPolicyProof { global, routine },
            boot: receipt.deadline.created_boot,
        };
        super::super::preparation::encoded_size(&command).map_err(ai_failure)?;
        Ok(ControlJob {
            context: context.clone(),
            workspace: bot.home_workspace,
            attempt: row.attempt_id,
            invocation: row.id,
            command,
            port: self.bot_delegate.clone().ok_or_else(tool_not_resumable)?,
            reply: reply.clone(),
            decision: Some(Box::new(DecisionHuman {
                authority: self.bot_authority.clone().ok_or_else(tool_not_resumable)?,
                owner: BotOwner {
                    user_scope: bot.user_scope,
                    device: bot.owner_device,
                },
                home: bot.home_workspace,
                human,
                reread,
                card,
                replay,
            })),
        })
    }

    /// The writer's view of one sent answer. An exact positive receipt keeps
    /// the answer's first form for replay, settles a Koru card (a native card
    /// settles from the native prompt's own resolution) and continues as an
    /// applied answer. A definite refusal forgets that form; an unknown
    /// outcome keeps it so a retry resends exactly the same answer.
    pub(super) fn settle_decision_answer(
        &mut self,
        result: &mut ControlResult,
        sent: DecisionSent,
    ) {
        let accepted = match &result.result {
            Ok(BotReply::Decision { decision: view })
                if view.receipt.decision == sent.card.decision
                    && view
                        .receipt
                        .answer
                        .as_ref()
                        .is_some_and(|answer| answer.request == sent.answer.request) =>
            {
                Some(view.clone())
            }
            _ => None,
        };
        if let Some(view) = accepted {
            let authorization = view.receipt.state.authorization;
            let _ = self.decision_waits.retain_answer(sent.answer);
            let _ = self.decision_waits.remember(view);
            if sent.koru {
                self.settle_koru_card(sent.card.invocation, authorization);
            }
            result.result = Ok(BotReply::Applied);
        } else if result.result.is_ok() {
            // Another reply, or a receipt for another answer: never applied.
            result.result = Err(refused("bot_conflict"));
        } else if result.result.as_ref().is_err_and(|error| error.retryable) {
            let _ = self.decision_waits.retain_answer(sent.answer);
        } else {
            self.decision_waits.drop_answer(sent.answer.request);
        }
    }

    fn settle_koru_card(
        &mut self,
        invocation: ToolInvocationId,
        authorization: DecisionAuthorization,
    ) {
        let Ok(Some(row)) = self.repository.tool_activity(invocation) else {
            return;
        };
        let Some(row) = settled_koru_card(row, authorization, Timestamp::now()) else {
            return;
        };
        if self.repository.advance_agent_activity(&row).is_ok() {
            self.publish(AiEvent::ToolActivity {
                attempt_id: row.attempt_id,
                activity: row,
            });
        }
    }
}

/// The Koru card after an applied answer: Allow is answered (Complete,
/// Approved), Deny is Denied. An already ended card, or an authorization
/// that is not an answer, changes nothing.
pub(in crate::worker) fn settled_koru_card(
    mut row: ToolActivity,
    authorization: DecisionAuthorization,
    at: Timestamp,
) -> Option<ToolActivity> {
    if row.status.is_terminal() || row.approval != ToolApproval::Pending {
        return None;
    }
    match authorization {
        DecisionAuthorization::Granted | DecisionAuthorization::Consumed => {
            row.status = ToolActivityStatus::Complete;
            row.approval = ToolApproval::Approved;
            row.started_at = Some(at.max(row.proposed_at));
        }
        DecisionAuthorization::Denied => {
            row.status = ToolActivityStatus::Denied;
            row.approval = ToolApproval::Denied;
            row.error_code = Some(ToolErrorCode::PermissionDenied);
        }
        _ => return None,
    }
    row.completed_at = Some(at.max(row.proposed_at));
    Some(row)
}
