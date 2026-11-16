// SPDX-License-Identifier: MPL-2.0

//! The decision half of a private operation (M5). A private decision is a
//! Koru decision: its wait, hold, answer, successor, fence, expiry and cleanup
//! are the shared owners, reached through
//! `DecisionKind::resumes_by_successor`. This file holds only what differs
//! from an edit: the grant predicate, the one-use consume that writes
//! `bot_private_effects` instead of a native intent and lease, and the
//! retained outcome that settlement and cleanup name.
use super::write::DecisionMutation;
use super::*;
use crate::private_effects::{
    PrivateEffectOutcome, PrivateEffectRecord, PrivateLedgerError, PrivateRoutineFence,
};

/// The grant predicate, repeated in the writer from the captured and the
/// offered current policy (which `authority::fresh` has proved is the latest
/// effective barrier). Only "this Bot has no grant for this connection" is a
/// question (D7): when both hold the grant no question is needed, and a
/// Routine Run never has a private question (D6).
pub(super) fn grant_denial(
    binding: &DecisionPrivateBinding,
    proof: &FreshBotPolicyProof,
) -> Result<(), BotLedgerError> {
    if binding.routine.is_some() || binding.routine_policy.is_some() || proof.routine.is_some() {
        return Err(BotLedgerError::Fenced);
    }
    let current = &proof.global.policy;
    require(
        binding.workspace == current.workspace
            && binding.workspace == current.destination
            && binding.workspace == binding.captured_policy.destination,
    )?;
    let grant = binding.proposal().grant();
    // A grant added after the Run was admitted does not silently broaden the
    // Run: the captured policy must hold it too.
    if binding.captured_policy.grants_connection(&grant) && current.grants_connection(&grant) {
        return Err(BotLedgerError::Fenced);
    }
    Ok(())
}

/// The acknowledged consumer of a decision, from its retained resume.
fn consumer(
    resume: &DecisionResume,
) -> Option<(
    RunAttemptId,
    AttemptGeneration,
    Option<SessionId>,
    AgentTurnId,
)> {
    let (attempt, generation, session, _) = resume.consumer_identity();
    let turn = match &resume.state {
        DecisionResumeState::InputAcknowledged { acknowledgment }
        | DecisionResumeState::Closed {
            acknowledgment: Some(acknowledgment),
        } => acknowledgment.turn,
        _ => return None,
    };
    Some((attempt, generation, session, turn))
}

/// Correlation on every decision read: a private decision has its effect row
/// exactly when it is consumed, that row names the decision's own operation,
/// binding and acknowledged consumer, and no other kind of decision has one.
pub(super) fn retained_effect(
    conn: &Connection,
    receipt: &DecisionReceipt,
    resume: Option<&DecisionResume>,
) -> Result<Option<PrivateEffectRecord>, BotLedgerError> {
    let found = crate::private_effects::by_decision_on(conn, receipt.decision)?;
    let DecisionBindingV1::PrivateOperation(binding) = &receipt.binding else {
        require(found.is_none())?;
        return Ok(None);
    };
    let bad = |column| corrupt("bot_private_effects", column);
    if found.is_some() != (receipt.state.authorization == DecisionAuthorization::Consumed) {
        return Err(bad("consumed"));
    }
    let Some(effect) = found else {
        return Ok(None);
    };
    let (attempt, generation, _, turn) = resume
        .and_then(consumer)
        .ok_or_else(|| bad("acknowledgment"))?;
    let origin = binding.origin();
    if effect.owner != origin.owner
        || effect.bot != origin.bot
        || effect.key.run != origin.run
        || effect.key.operation_id != binding.operation_id
        || effect.key.attempt != attempt
        || effect.key.generation != generation
        || effect.key.turn != turn
        || effect.binding_digest != Some(receipt.binding_digest)
        || effect.operation != binding.proposal()
        || effect.routine.is_some()
        || effect.barrier != binding.global_policy.key.request
        || effect.config_revision != binding.global_policy.config_revision.get()
        || effect.policy_revision != binding.global_policy.policy_revision.get()
        || &effect.policy_digest != binding.global_policy.digest.as_bytes()
    {
        return Err(bad("correlation"));
    }
    Ok(Some(effect))
}

/// The retained outcome of a consumed private decision. An open row is
/// Fenced: its dispatcher still owns it, it is never reported as not applied,
/// and cleanup keeps waiting.
pub(super) fn retained_outcome(
    conn: &Connection,
    record: &DecisionRecord,
) -> Result<DecisionRetainedEffect, BotLedgerError> {
    let effect = retained_effect(conn, &record.receipt, record.resume.as_ref())?
        .ok_or_else(|| corrupt("bot_private_effects", "consumed"))?;
    effect.retained_effect().ok_or(BotLedgerError::Fenced)
}

/// Settlement and final cleanup may name only the decision's own effect row,
/// exactly as retained, and only once it is final.
pub(super) fn effect_outcome(
    conn: &Connection,
    record: &DecisionRecord,
    outcome: &DecisionRetainedEffect,
) -> Result<(), BotLedgerError> {
    require(retained_outcome(conn, record)? == *outcome)
}

/// One-use consume of an allowed private decision. It writes the effect row
/// at `prepared` and moves the decision to `consumed/effect` in the caller's
/// transaction; the first fresh commit answers the one `Execute`.
pub(crate) fn consume_on(
    tx: &Transaction<'_>,
    expected: &DecisionExpected,
    dispatcher: PrivateDispatcherId,
    proposal: &PrivateOperationProposal,
    fence: &BotExecutionFence,
    proof: &FreshBotPolicyProof,
    boot: DelegateBootId,
    now: i64,
) -> Result<(DecisionMutation, PrivateEffectOutcome), PrivateLedgerError> {
    authority::owner(tx, expected.owner, expected.run)?;
    let old = rows::read(tx, expected.owner, expected.run, expected.decision)?
        .ok_or(BotLedgerError::Stale)?;
    require(old.receipt.binding_digest == expected.binding_digest)?;
    let DecisionBindingV1::PrivateOperation(binding) = &old.receipt.binding else {
        return Err(BotLedgerError::Invalid.into());
    };
    if !proposal.valid() {
        return Err(BotLedgerError::Invalid.into());
    }
    // The same operation id with another proposal is a conflict, never a
    // second meaning.
    require(old.receipt.binding.matches_private(proposal))?;
    // Authentication and immutable comparison precede replay. Current caller
    // generation, policy, dispatcher and clock do not revoke reconciliation.
    if fence.owner != expected.owner || fence.bot != binding.bot || fence.run != expected.run {
        return Err(BotLedgerError::Fenced.into());
    }
    if let Some(effect) = retained_effect(tx, &old.receipt, old.resume.as_ref())? {
        return Ok((
            DecisionMutation {
                record: old,
                outcome: crate::WriteOutcome::AlreadyApplied,
            },
            PrivateEffectOutcome::replay(effect),
        ));
    }
    if crate::private_effects::operation_id_taken_on(tx, expected.run, proposal.operation_id)? {
        return Err(BotLedgerError::Conflict.into());
    }
    if crate::private_effects::open_in_run_on(tx, expected.run)?.is_some() {
        return Err(PrivateLedgerError::EffectOpen);
    }
    require(
        old.receipt.state.revision == expected.revision
            && old.receipt.state.authorization == DecisionAuthorization::Granted
            && old.receipt.state.phase == DecisionPhase::AwaitRetry
            && old.wait.phase == BotWaitPhase::ResumeReserved,
    )?;
    super::write::clock(&old.receipt, boot, now)?;
    // The Bot's barrier must still be the one the decision is bound to: a
    // policy that moved since the question was asked makes the answer stale.
    let admitted = authority::current_policy(tx, &old.receipt.binding, proof)?;
    require(
        binding.routine.is_none()
            && binding.routine_policy.is_none()
            && proof.routine.is_none()
            && admitted.active_elapsed_at(now) < crate::BOT_ACTIVE_BUDGET_MS,
    )?;
    let resume = old.resume.as_ref().ok_or(BotLedgerError::Fenced)?;
    require(matches!(
        resume.consumer,
        DecisionConsumer::RecordedSuccessor { .. }
    ))?;
    let (attempt, generation, session, creation) = resume.consumer_identity();
    let DecisionResumeState::InputAcknowledged { acknowledgment } = &resume.state else {
        return Err(BotLedgerError::Fenced.into());
    };
    // The consumer must be exactly the acknowledged resume tuple.
    require(
        fence.attempt == attempt
            && fence.generation == generation
            && Some(fence.session) == session
            && fence.turn == Some(acknowledgment.turn),
    )?;
    let current = crate::attempts::latest(tx, expected.run)?.ok_or(BotLedgerError::Fenced)?;
    require(
        current.attempt == attempt
            && current.generation == generation
            && !current.retired
            && current.phase == crate::AttemptPhase::Running
            && current.provider_session
                == Some(ProviderSessionKey {
                    idempotency: creation,
                    session,
                }),
    )?;
    let (policy, routine) = crate::native_results::current_composite(tx, &admitted, proof)?;
    require(routine.is_none())?;
    let effect = PrivateEffectRecord {
        owner: expected.owner,
        bot: binding.bot,
        key: PrivateEffectKey {
            run: expected.run,
            operation_id: proposal.operation_id,
            attempt,
            generation,
            turn: acknowledgment.turn,
        },
        dispatcher,
        decision: Some(expected.decision),
        binding_digest: Some(expected.binding_digest),
        operation: proposal.clone(),
        barrier: policy.key.request,
        config_revision: policy.config_revision,
        policy_revision: policy.policy_revision,
        policy_digest: policy.payload_digest,
        routine: None::<PrivateRoutineFence>,
        stage: PrivateEffectStage::Prepared,
        refusal: None,
        outcome_bytes: None,
        withheld: None,
        dispatched_at_ms: None,
        resolved_at_ms: None,
        created_at_ms: now,
        updated_at_ms: now,
    };
    crate::private_effects::insert_prepared_on(tx, &effect)?;
    let changed=tx.execute("UPDATE bot_decisions SET authorization='consumed',phase='effect',consumed_at_ms=?2,revision=?3,last_observed_at_ms=?2,updated_at_ms=?2 WHERE decision_id=?1 AND revision=?4 AND authorization='granted' AND phase='await_retry'",params![expected.decision.to_storage(),now,next(expected.revision)?.get(),expected.revision.get()]).map_err(sql)?;
    require(changed == 1)?;
    let record = rows::read(tx, expected.owner, expected.run, expected.decision)?
        .ok_or(BotLedgerError::Stale)?;
    let effect = crate::private_effects::read_on(tx, expected.run, proposal.operation_id)?
        .ok_or(BotLedgerError::Stale)?;
    Ok((
        DecisionMutation {
            record,
            outcome: crate::WriteOutcome::Applied,
        },
        PrivateEffectOutcome::execute(effect),
    ))
}

/// The typed refusals of a private question, decided in the writer after the
/// replay and before the shared prepare body. Each means nothing was written.
pub(crate) fn prepare_on(
    tx: &Transaction<'_>,
    binding: &DecisionBindingV1,
    proof: &FreshBotPolicyProof,
    boot: DelegateBootId,
    now: i64,
) -> Result<DecisionMutation, PrivateLedgerError> {
    let DecisionBindingV1::PrivateOperation(private) = binding else {
        return Err(BotLedgerError::Invalid.into());
    };
    valid(binding)?;
    let f = binding.origin();
    authority::owner(tx, f.owner, f.run)?;
    // Replay first: the shared body returns the retained decision, or a
    // conflict when the operation id holds another proposal or kind.
    if rows::matching(tx, binding)?.is_none() {
        // D6. The stored admission decides, never the binding's own claim.
        crate::bots::routines::bounded_admission(tx, f.run)?;
        let admitted = crate::bots::routines::native_admission(tx, f.run)?;
        if admitted.submission.provenance.routine.is_some() || private.routine.is_some() {
            return Err(PrivateLedgerError::RoutineDecisionUnsupported);
        }
        // M6: a child Run never asks a private-operation question. Its
        // connections were reduced to what its parent also holds, so a one-use
        // approval would hand private content to a parent with no grant.
        if crate::children::is_child_run_on(tx, f.run)? {
            return Err(BotLedgerError::Fenced.into());
        }
        if crate::private_effects::operation_id_taken_on(tx, f.run, private.operation_id)? {
            return Err(BotLedgerError::Conflict.into());
        }
        if crate::private_effects::open_in_run_on(tx, f.run)?.is_some() {
            return Err(PrivateLedgerError::EffectOpen);
        }
        let owned: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM bot_wait_runtime WHERE run_id=?1 AND phase<>'settled')
                     OR EXISTS(SELECT 1 FROM bot_decisions WHERE run_id=?1 AND phase<>'settled')",
                [f.run.to_storage()],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if owned {
            return Err(PrivateLedgerError::DecisionOwned);
        }
        // Only against the proved latest effective barrier is "both hold the
        // grant" a fact; a stale proof is fenced by this call first.
        authority::fresh(tx, binding, proof, true)?;
        let grant = private.proposal().grant();
        if private.captured_policy.grants_connection(&grant)
            && proof.global.policy.grants_connection(&grant)
        {
            return Err(PrivateLedgerError::ConnectionGranted);
        }
    }
    Ok(super::write::prepare_on(tx, binding, proof, boot, now)?)
}

impl RunsDb {
    /// The decision that asked for one private read, by its operation id.
    /// Exact retained history; it grants nothing.
    pub fn bot_private_decision_for_operation(
        &self,
        owner: BotOwner,
        run: RunId,
        operation: uuid::Uuid,
    ) -> Result<Option<DecisionRecord>, BotLedgerError> {
        authority::owner(self.conn(), owner, run)?;
        let id = rows::private_operation(self.conn(), run, operation)?;
        id.map(|id| {
            rows::read(self.conn(), owner, run, id)?
                .ok_or_else(|| corrupt("bot_decisions", "identity"))
        })
        .transpose()
    }
}
