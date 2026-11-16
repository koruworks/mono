// SPDX-License-Identifier: MPL-2.0

use super::*;

#[derive(Clone, Debug)]
pub struct AnswerDecision {
    pub owner: BotOwner,
    pub run: RunId,
    pub decision: DecisionId,
    pub expected_revision: DecisionRevision,
    pub binding_digest: DecisionDigest,
    pub request: DecisionAnswerId,
    pub human: ProfileId,
    pub choice: DecisionAnswerChoice,
    /// Digest retained from the authenticated off-writer human proof. SQL
    /// cannot turn this data into current Store permission by itself.
    pub authority_digest: DecisionDigest,
}

pub(crate) fn answer_on(
    tx: &Transaction<'_>,
    input: &AnswerDecision,
    proof: &FreshBotPolicyProof,
    boot: DelegateBootId,
    now: i64,
) -> Result<DecisionMutation, BotLedgerError> {
    authority::owner(tx, input.owner, input.run)?;
    let mut old = required(tx, input.owner, input.run, input.decision)?;
    let found=tx.query_row("SELECT CASE WHEN typeof(decision_id)='text' AND length(CAST(decision_id AS BLOB))=36 THEN decision_id END FROM bot_decisions WHERE user_scope=?1 AND device_id=?2 AND answer_request_id=?3",params![input.owner.user_scope.to_storage(),input.owner.device.to_storage(),input.request.to_storage()],|r|r.get::<_,String>(0)).optional().map_err(sql)?;
    if let Some(id) = found {
        require(id == input.decision.to_storage())?;
    }
    if let Some(answer) = &old.receipt.answer {
        require(
            answer.request == input.request
                && answer.expected_revision == input.expected_revision
                && answer.binding_digest == input.binding_digest
                && answer.human_profile == input.human
                && answer.choice == input.choice
                && answer.authority_digest == input.authority_digest,
        )?;
        return Ok(DecisionMutation {
            record: old,
            outcome: crate::WriteOutcome::AlreadyApplied,
        });
    }
    require(
        old.receipt.binding_digest == input.binding_digest
            && input.choice.matches_binding(&old.receipt.binding),
    )?;
    if old.receipt.state.revision != input.expected_revision {
        return Err(BotLedgerError::Stale);
    }
    let native = old.receipt.state.kind == DecisionKind::NativeApproval;
    if old.receipt.state.authorization != DecisionAuthorization::Pending
        || if native {
            old.receipt.state.phase != DecisionPhase::NativeWait
                || old.wait.phase != BotWaitPhase::Fenced
        } else {
            old.receipt.state.phase != DecisionPhase::Held
                || old.wait.phase != BotWaitPhase::Retired
                || old.wait.release.is_none()
        }
    {
        return Err(BotLedgerError::Stale);
    }
    clock(&old.receipt, boot, now)?;
    let admitted = authority::fresh(tx, &old.receipt.binding, proof, native)?;
    authority::selected_denial(tx, &old.receipt.binding, proof, &admitted)?;
    require(input.human == old.receipt.binding.human_profile())?;
    let answer = DecisionAnswerReceipt {
        decision: input.decision,
        request: input.request,
        expected_revision: input.expected_revision,
        committed_revision: next(input.expected_revision)?,
        binding_digest: input.binding_digest,
        human_profile: input.human,
        choice: input.choice.clone(),
        answered_at_ms: now,
        authority_digest: input.authority_digest,
    };
    valid(&answer)?;
    let grants = match &input.choice {
        DecisionAnswerChoice::AllowOnce {} => true,
        DecisionAnswerChoice::Deny {} => false,
        DecisionAnswerChoice::Native { offered } => offered.intent.grants(),
    };
    let authorization = if grants {
        DecisionAuthorization::Granted
    } else {
        DecisionAuthorization::Denied
    };
    let phase = if native {
        DecisionPhase::NativeDispatch
    } else if grants {
        DecisionPhase::Held
    } else {
        DecisionPhase::StopPending
    };
    let json = encoded(&answer, DECISION_RECEIPT_BYTES)?;
    let choice_json = encoded(&answer.choice, 16384)?;
    let changed=tx.execute("UPDATE bot_decisions SET revision=?2,authorization=?3,phase=?4,answer_request_id=?5,answer_profile_id=?6,answer_choice_json=?7,answer_at_ms=?8,answer_proof_digest=?9,answer_receipt_json=?10,last_observed_at_ms=?8,updated_at_ms=?8 WHERE decision_id=?1 AND revision=?11 AND authorization='pending'",params![input.decision.to_storage(),answer.committed_revision.get(),token(authorization)?,token(phase)?,input.request.to_storage(),input.human.to_storage(),choice_json,now,input.authority_digest.as_bytes().as_slice(),json,input.expected_revision.get()]).map_err(sql)?;
    if changed != 1 {
        return Err(BotLedgerError::Stale);
    }
    if let DecisionAnswerChoice::Native { offered } = &input.choice {
        let DecisionBindingV1::NativeApproval(binding) = &old.receipt.binding else {
            return Err(BotLedgerError::Invalid);
        };
        // Stable key is accepted once with the answer, never minted by a retry.
        insert(
            tx,
            "bot_decision_native_answers",
            vec![
                ("decision_id", text(input.decision.to_storage())),
                ("session_id", text(binding.origin_session.to_string())),
                ("turn_id", text(binding.origin_turn.to_string())),
                ("prompt_id", text(binding.prompt.to_string())),
                ("response_key", text(IdempotencyKey::mint().to_string())),
                ("choice_id", text(offered.id.to_string())),
                ("choice_intent", text(token(offered.intent)?)),
                (
                    "native_decision",
                    text(if grants { "approve" } else { "reject" }),
                ),
                ("state", text("prepared")),
                ("created_at_ms", number(now)),
                ("updated_at_ms", number(now)),
            ],
        )?;
    } else if !grants {
        old.wait.revision = next(old.wait.revision)?;
        tx.execute("UPDATE bot_wait_runtime SET phase='cleanup_pending',revision=?2,updated_at_ms=?3 WHERE wait_id=?1",params![old.wait.wait.to_storage(),old.wait.revision.get(),now]).map_err(sql)?;
    }
    done(
        tx,
        input.owner,
        input.run,
        input.decision,
        crate::WriteOutcome::Applied,
    )
}
