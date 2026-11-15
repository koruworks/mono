// SPDX-License-Identifier: MPL-2.0

use super::*;

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
