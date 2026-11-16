// SPDX-License-Identifier: MPL-2.0

use super::*;

/// Allocate the sole successor only after the original has positively released
/// capacity. The immutable original InitialSend/Fired identity is never edited.
/// C3 retains the post-allocation Control in this same transaction.
pub(crate) fn reserve_successor_on(
    tx: &Transaction<'_>,
    expected: &DecisionExpected,
    host: HostInstanceId,
    fingerprint: [u8; 32],
    proof: &FreshBotPolicyProof,
    boot: DelegateBootId,
    now: i64,
) -> Result<DecisionMutation, BotLedgerError> {
    authority::owner(tx, expected.owner, expected.run)?;
    let old = required(tx, expected.owner, expected.run, expected.decision)?;
    require(old.receipt.binding_digest == expected.binding_digest)?;
    if let Some(resume) = &old.resume {
        require(
            matches!(resume.consumer, DecisionConsumer::RecordedSuccessor { .. })
                && resume.input_fingerprint == fingerprint,
        )?;
        let (attempt, generation, _, _) = resume.consumer_identity();
        let retained = crate::attempts::retained_bounded(tx, expected.run, generation)?
            .ok_or(BotLedgerError::Fenced)?;
        require(retained.attempt == attempt && retained.host_instance == Some(host))?;
        return Ok(DecisionMutation {
            record: old,
            outcome: crate::WriteOutcome::AlreadyApplied,
        });
    }
    require(
        old.receipt.state.revision == expected.revision
            && old.receipt.state.kind.resumes_by_successor()
            && old.receipt.state.authorization == DecisionAuthorization::Granted
            && old.receipt.state.phase == DecisionPhase::Held
            && old.wait.phase == BotWaitPhase::Retired,
    )?;
    clock(&old.receipt, boot, now)?;
    let admitted = authority::current_policy(tx, &old.receipt.binding, proof)?;
    require(
        admitted.active_since_ms.is_none()
            && admitted.active_elapsed_at(now) < crate::BOT_ACTIVE_BUDGET_MS,
    )?;
    let inventory = occupancy::inventory(tx, expected.owner.device)?;
    require(inventory.owned.iter().any(|owned| {
        owned.execution.run == expected.run
            && matches!(owned.capacity, crate::BotExecutionCapacity::QualifiedDecisionRelease { wait, decision, .. }
                if wait == old.wait.wait && decision == expected.decision)
    }))?;
    if inventory.occupied >= crate::BOT_HOST_EXECUTION_LIMIT {
        return Err(BotLedgerError::Capacity);
    }
    let attempt = crate::attempts::open_on(tx, expected.run, now)?;
    let creation_key = IdempotencyKey::mint();
    let input_key = IdempotencyKey::mint();
    crate::attempts::bind_on(
        tx,
        expected.run,
        attempt.generation,
        ProviderSessionKey {
            idempotency: creation_key,
            session: None,
        },
        host,
        now,
    )?;
    let f = old.wait.origin;
    tx.execute("UPDATE bot_wait_runtime SET phase='resume_reserved',revision=?2,updated_at_ms=max(updated_at_ms,?3) WHERE wait_id=?1 AND revision=?4",
        params![old.wait.wait.to_storage(),next(old.wait.revision)?.get(),now,old.wait.revision.get()]).map_err(sql)?;
    tx.execute("UPDATE bot_decisions SET phase='resume_prepared',revision=?2,last_observed_at_ms=max(last_observed_at_ms,?3),updated_at_ms=max(updated_at_ms,?3) WHERE decision_id=?1 AND revision=?4",
        params![expected.decision.to_storage(),next(expected.revision)?.get(),now,expected.revision.get()]).map_err(sql)?;
    tx.execute("INSERT INTO bot_decision_resumes(decision_id,run_id,kind,origin_attempt_id,origin_generation,origin_session_id,consumer_attempt_id,consumer_generation,session_creation_key,input_key,input_fingerprint,state,created_at_ms,updated_at_ms)
        VALUES(?1,?2,'recorded_successor',?3,?4,?5,?6,?7,?8,?9,?10,'prepared',?11,?11)",
        params![expected.decision.to_storage(),expected.run.to_storage(),f.attempt.to_storage(),f.generation.get(),f.session.to_string(),attempt.attempt.to_storage(),attempt.generation.get(),creation_key.to_string(),input_key.to_string(),fingerprint.as_slice(),now]).map_err(sql)?;
    done(
        tx,
        expected.owner,
        expected.run,
        expected.decision,
        crate::WriteOutcome::Applied,
    )
}

/// Stop closes the retained input identity; it does not erase an accepted turn
/// or claim that an unknown creation/input has stopped. Late receipts reconcile.
pub(crate) fn close_resume_on(
    tx: &Transaction<'_>,
    expected: &DecisionExpected,
    input_key: IdempotencyKey,
    now: i64,
) -> Result<DecisionMutation, BotLedgerError> {
    let old = retained(tx, expected)?;
    let resume = old.resume.as_ref().ok_or(BotLedgerError::Stale)?;
    require(resume.input_key == input_key)?;
    if matches!(resume.state, DecisionResumeState::Closed { .. }) {
        return Ok(DecisionMutation {
            record: old,
            outcome: crate::WriteOutcome::AlreadyApplied,
        });
    }
    require(old.wait.phase == BotWaitPhase::CleanupPending)?;
    tx.execute("UPDATE bot_decision_resumes SET state='closed',updated_at_ms=max(updated_at_ms,?2) WHERE decision_id=?1",
        params![expected.decision.to_storage(),now]).map_err(sql)?;
    done(
        tx,
        expected.owner,
        expected.run,
        expected.decision,
        crate::WriteOutcome::Applied,
    )
}

fn retained(
    tx: &Connection,
    expected: &DecisionExpected,
) -> Result<DecisionRecord, BotLedgerError> {
    authority::owner(tx, expected.owner, expected.run)?;
    let old = required(tx, expected.owner, expected.run, expected.decision)?;
    require(old.receipt.binding_digest == expected.binding_digest)?;
    require(old.resume.is_some())?;
    Ok(old)
}

/// Reconcile the exact creation key, including a late positive lookup after
/// Stop or permission loss. This does not authorize starting or sending input.
/// A known session is retained before resolving its cleanup target; an unknown
/// creation is never converted into process absence.
pub(crate) fn bind_resume_session_on(
    tx: &Transaction<'_>,
    expected: &DecisionExpected,
    creation_key: IdempotencyKey,
    host: HostInstanceId,
    session: SessionId,
    now: i64,
) -> Result<DecisionMutation, BotLedgerError> {
    let old = retained(tx, expected)?;
    let resume = old.resume.as_ref().ok_or(BotLedgerError::Stale)?;
    let (attempt, generation, stored, key) = resume.consumer_identity();
    require(key == creation_key && session != resume.origin.session)?;
    // SameSession is retained read/reconciliation vocabulary, not a qualified
    // production capacity-releasing mode. This owner never enables it.
    require(matches!(
        resume.consumer,
        DecisionConsumer::RecordedSuccessor { .. }
    ))?;
    let row = crate::attempts::retained_bounded(tx, expected.run, generation)?
        .ok_or(BotLedgerError::Fenced)?;
    require(row.attempt == attempt && row.host_instance == Some(host))?;
    if let Some(stored) = stored {
        require(stored == session)?;
        return Ok(DecisionMutation {
            record: old,
            outcome: crate::WriteOutcome::AlreadyApplied,
        });
    }
    require(matches!(
        resume.state,
        DecisionResumeState::Prepared {} | DecisionResumeState::Closed { .. }
    ))?;
    crate::attempts::bind_on(
        tx,
        expected.run,
        generation,
        ProviderSessionKey {
            idempotency: creation_key,
            session: Some(session),
        },
        host,
        now.max(row.updated_at_ms),
    )?;
    let state = if matches!(resume.state, DecisionResumeState::Closed { .. }) {
        "closed"
    } else {
        "session_bound"
    };
    let changed=tx.execute("UPDATE bot_decision_resumes SET consumer_session_id=?2,state=?3,updated_at_ms=max(updated_at_ms,?4)
        WHERE decision_id=?1 AND consumer_session_id IS NULL",
        params![expected.decision.to_storage(),session.to_string(),state,now]).map_err(sql)?;
    require(changed == 1)?;
    done(
        tx,
        expected.owner,
        expected.run,
        expected.decision,
        crate::WriteOutcome::Applied,
    )
}

/// The actual keyed input port supplies this observation. The first accepted
/// turn/digest is immutable; neither unknown completion nor a later caller can
/// replace it. Fresh dispatch qualification is separate and precedes the port.
/// C3 composes the resulting control in this same transaction before commit.
pub(crate) fn record_resume_input_on(
    tx: &Transaction<'_>,
    expected: &DecisionExpected,
    input_key: IdempotencyKey,
    fingerprint: [u8; 32],
    acknowledgment: Option<&DecisionInputAcknowledgment>,
    now: i64,
) -> Result<DecisionMutation, BotLedgerError> {
    let old = retained(tx, expected)?;
    let resume = old.resume.as_ref().ok_or(BotLedgerError::Stale)?;
    require(resume.input_key == input_key && resume.input_fingerprint == fingerprint)?;
    if let DecisionResumeState::InputAcknowledged {
        acknowledgment: prior,
    }
    | DecisionResumeState::Closed {
        acknowledgment: Some(prior),
    } = &resume.state
    {
        if let Some(offered) = acknowledgment {
            require(prior == offered)?;
        }
        return Ok(DecisionMutation {
            record: old,
            outcome: crate::WriteOutcome::AlreadyApplied,
        });
    }
    require(resume.consumer_identity().2.is_some())?;
    if acknowledgment.is_none()
        && matches!(
            resume.state,
            DecisionResumeState::InputUnconfirmed {} | DecisionResumeState::Closed { .. }
        )
    {
        return Ok(DecisionMutation {
            record: old,
            outcome: crate::WriteOutcome::AlreadyApplied,
        });
    }
    require(matches!(
        resume.state,
        DecisionResumeState::SessionBound {}
            | DecisionResumeState::InputUnconfirmed {}
            | DecisionResumeState::Closed { .. }
    ))?;
    let body = acknowledgment
        .map(|a| encoded(a, DECISION_RECEIPT_BYTES))
        .transpose()?;
    let closed = matches!(resume.state, DecisionResumeState::Closed { .. });
    let state = if closed {
        "closed"
    } else if acknowledgment.is_some() {
        "input_acknowledged"
    } else {
        "input_unconfirmed"
    };
    tx.execute("UPDATE bot_decision_resumes SET state=?2,accepted_turn_id=?3,receipt_json=?4,updated_at_ms=max(updated_at_ms,?5)
        WHERE decision_id=?1 AND accepted_turn_id IS NULL",
        params![expected.decision.to_storage(),state,acknowledgment.map(|a|a.turn.to_string()),body,now]).map_err(sql)?;
    // A late acknowledgment is retained history, never resurrection. Stop,
    // expiry or terminal cleanup keep their exact phase and cleanup debt.
    if acknowledgment.is_some()
        && !closed
        && old.receipt.state.phase == DecisionPhase::ResumePrepared
        && old.receipt.state.authorization == DecisionAuthorization::Granted
        && old.wait.phase == BotWaitPhase::ResumeReserved
    {
        tx.execute("UPDATE bot_decisions SET phase='await_retry',revision=?2,updated_at_ms=max(updated_at_ms,?3)
            WHERE decision_id=?1 AND revision=?4",
            params![expected.decision.to_storage(),next(old.receipt.state.revision)?.get(),now,old.receipt.state.revision.get()]).map_err(sql)?;
    }
    done(
        tx,
        expected.owner,
        expected.run,
        expected.decision,
        crate::WriteOutcome::Applied,
    )
}
