// SPDX-License-Identifier: MPL-2.0

use super::*;

pub(crate) fn consume_on(
    tx: &Transaction<'_>,
    expected: &DecisionExpected,
    request: &PrepareNativeResult,
    payload: &[u8],
    fence: &BotExecutionFence,
    proof: &FreshBotPolicyProof,
    boot: DelegateBootId,
    now: i64,
) -> Result<(DecisionMutation, crate::NativeLeaseOutcome), BotLedgerError> {
    authority::owner(tx, expected.owner, expected.run)?;
    let old = required(tx, expected.owner, expected.run, expected.decision)?;
    require(old.receipt.binding_digest == expected.binding_digest)?;
    let DecisionBindingV1::KoruEdit(binding) = &old.receipt.binding else {
        return Err(BotLedgerError::Invalid);
    };
    if !request.valid() || payload.len() > DECISION_FRAME_BYTES {
        return Err(BotLedgerError::Invalid);
    };
    let revision = request
        .expected_revision
        .as_deref()
        .ok_or(BotLedgerError::Invalid)?;
    require(
        request.key.run == expected.run
            && request.kind == NativeResultOperation::EditPage
            && request.destination == binding.workspace
            && request.target == Some(binding.target)
            && request.config_revision == binding.global_policy.config_revision.get()
            && request.policy_revision == binding.global_policy.policy_revision.get()
            && old.receipt.binding.matches_edit_except_revision(
                request.key.operation_id,
                binding.target,
                request.request_fingerprint,
                payload,
            )
            && same_native_edit_revision(revision, binding.expected_revision.as_slice())
                .map_err(|_| BotLedgerError::Invalid)?,
    )?;
    // Authentication and immutable payload comparison precede replay. Current
    // caller generation/policy/clock do not revoke retained reconciliation.
    require(
        fence.owner == expected.owner && fence.bot == binding.bot && fence.run == expected.run,
    )?;
    if let Some(key) = super::super::effect::retained_key(tx, &old.receipt, old.resume.as_ref())? {
        let intent =
            crate::native_results::retained_result_on(tx, &key)?.ok_or(BotLedgerError::Stale)?;
        require(intent.requested_title == request.title)?;
        return Ok((
            DecisionMutation {
                record: old,
                outcome: crate::WriteOutcome::AlreadyApplied,
            },
            crate::NativeLeaseOutcome {
                intent,
                execute: None,
            },
        ));
    }
    // No adoption: even an unleased legacy Prepared intent retains its original
    // owner and Store request. The one-use path only creates an absent intent.
    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM native_result_intents WHERE run_id=?1 AND operation_id=?2) OR EXISTS(SELECT 1 FROM bot_memory_edit_allocations WHERE run_id=?1 AND operation_id=?2)",params![expected.run.to_storage(),request.key.operation_id.to_string()],|r|r.get(0)).map_err(sql)?;
    if exists {
        return Err(BotLedgerError::Conflict);
    }
    require(
        old.receipt.state.revision == expected.revision
            && old.receipt.state.authorization == DecisionAuthorization::Granted
            && old.receipt.state.phase == DecisionPhase::AwaitRetry
            && old.wait.phase == BotWaitPhase::ResumeReserved,
    )?;
    clock(&old.receipt, boot, now)?;
    let admitted = authority::current_policy(tx, &old.receipt.binding, proof)?;
    // Binding and proof carry the same Routine, or neither does. A Routine
    // edit is consumed under the Routine's own grant; an interactive proof
    // never consumes it, and a Routine proof never consumes an interactive one.
    require(
        match (&binding.routine, &binding.routine_policy, &proof.routine) {
            (None, None, None) => true,
            (Some(subject), Some(bound), Some(offered)) => {
                offered.owner == bound.owner
                    && offered.bot == bound.bot
                    && offered.schedule == bound.schedule
                    && offered.schedule == subject.provenance.schedule
            }
            _ => false,
        } && admitted.active_elapsed_at(now) < crate::BOT_ACTIVE_BUDGET_MS,
    )?;
    let resume = old.resume.as_ref().ok_or(BotLedgerError::Fenced)?;
    require(matches!(
        resume.consumer,
        DecisionConsumer::RecordedSuccessor { .. }
    ))?;
    let (attempt, generation, session, creation) = resume.consumer_identity();
    let DecisionResumeState::InputAcknowledged { acknowledgment } = &resume.state else {
        return Err(BotLedgerError::Fenced);
    };
    require(
        request.key.attempt == attempt
            && request.key.generation == generation
            && request.key.turn == acknowledgment.turn
            && fence.attempt == attempt
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
    // The lease is taken with the Routine policy the decision was bound to:
    // the current Routine policy must equal the binding's `routine_policy`
    // (request, grant revision and payload digest), exactly as the global
    // policy was compared by `current_policy` above. A changed grant revision
    // makes the answer stale here.
    require(match (&binding.routine_policy, &routine) {
        (None, None) => true,
        (Some(bound), Some(current)) => {
            current.owner == bound.owner
                && current.bot == bound.bot
                && current.schedule == bound.schedule
                && current.request == bound.request
                && current.grant_revision == bound.grant_revision.get()
                && current.payload_digest == bound.digest
        }
        _ => false,
    })?;
    crate::native_results::insert_prepared_on(tx, request, now)?;
    let lease = crate::native_results::hold_lease_on(tx, &request.key, policy, routine, now)?;
    tx.execute("INSERT INTO native_decision_leases(run_id,operation_id,decision_id,binding_digest,consumer_attempt_id,consumer_generation,consumer_turn_id,consumed_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![expected.run.to_storage(),request.key.operation_id.to_string(),expected.decision.to_storage(),expected.binding_digest.as_bytes().as_slice(),attempt.to_storage(),generation.get(),acknowledgment.turn.to_string(),now]).map_err(sql)?;
    let changed=tx.execute("UPDATE bot_decisions SET authorization='consumed',phase='effect',consumed_at_ms=?2,revision=?3,last_observed_at_ms=?2,updated_at_ms=?2 WHERE decision_id=?1 AND revision=?4 AND authorization='granted' AND phase='await_retry'",params![expected.decision.to_storage(),now,next(expected.revision)?.get(),expected.revision.get()]).map_err(sql)?;
    require(changed == 1)?;
    let record = done(
        tx,
        expected.owner,
        expected.run,
        expected.decision,
        crate::WriteOutcome::Applied,
    )?;
    let intent = crate::native_results::retained_result_on(tx, &request.key)?
        .ok_or(BotLedgerError::Stale)?;
    Ok((
        record,
        crate::NativeLeaseOutcome {
            intent,
            execute: Some(lease),
        },
    ))
}

/// The caller has made this retained exact resolved result available to its
/// continuation. An unknown effect or a stopping consumer cannot take this
/// normal exit; they require the independent positive cleanup path.
pub(crate) fn settle_effect_on(
    tx: &Transaction<'_>,
    expected: &DecisionExpected,
    outcome: &DecisionRetainedEffect,
    now: i64,
) -> Result<DecisionMutation, BotLedgerError> {
    authority::owner(tx, expected.owner, expected.run)?;
    let old = required(tx, expected.owner, expected.run, expected.decision)?;
    require(
        old.receipt.binding_digest == expected.binding_digest
            && old.receipt.state.kind == DecisionKind::KoruEdit
            && old.receipt.state.authorization == DecisionAuthorization::Consumed,
    )?;
    cleanup::effect_outcome(tx, &old, Some(outcome))?;
    let reason = match outcome {
        DecisionRetainedEffect::Durable { .. } => DecisionClosureReason::Completed,
        DecisionRetainedEffect::NotApplied { .. } => DecisionClosureReason::EffectNotApplied,
        DecisionRetainedEffect::DrainedUnknown { .. } => return Err(BotLedgerError::Fenced),
    };
    if old.receipt.state.phase == DecisionPhase::Settled {
        require(
            old.receipt
                .closure
                .as_ref()
                .is_some_and(|c| c.reason == reason),
        )?;
        return Ok(DecisionMutation {
            record: old,
            outcome: crate::WriteOutcome::AlreadyApplied,
        });
    }
    require(
        old.receipt.state.revision == expected.revision
            && old.receipt.state.phase == DecisionPhase::Effect
            && old.wait.phase == BotWaitPhase::ResumeReserved,
    )?;
    let resume = old.resume.as_ref().ok_or(BotLedgerError::Fenced)?;
    let (attempt, generation, session, creation) = resume.consumer_identity();
    let current = crate::attempts::latest(tx, expected.run)?.ok_or(BotLedgerError::Fenced)?;
    require(
        current.attempt == attempt
            && current.generation == generation
            && !current.retired
            && !current.phase.is_terminal()
            && current.provider_session
                == Some(ProviderSessionKey {
                    idempotency: creation,
                    session,
                }),
    )?;
    tx.execute("UPDATE bot_decisions SET phase='settled',revision=?2,closure_reason=?3,settled_at_ms=?4,updated_at_ms=?4 WHERE decision_id=?1 AND revision=?5",params![expected.decision.to_storage(),next(expected.revision)?.get(),token(reason)?,now.max(old.receipt.updated_at_ms),expected.revision.get()]).map_err(sql)?;
    done(
        tx,
        expected.owner,
        expected.run,
        expected.decision,
        crate::WriteOutcome::Applied,
    )
}
