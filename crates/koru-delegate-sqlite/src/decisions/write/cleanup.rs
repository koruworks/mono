// SPDX-License-Identifier: MPL-2.0

//! Physical evidence records retained by the same writer as their Control.
use super::*;

pub(crate) fn origin_stop_on(
    tx: &Transaction<'_>,
    expected: &DecisionExpected,
    release: &OriginWaitStop,
    now: i64,
) -> Result<DecisionMutation, BotLedgerError> {
    authority::owner(tx, expected.owner, expected.run)?;
    let old = required(tx, expected.owner, expected.run, expected.decision)?;
    require(old.receipt.binding_digest == expected.binding_digest)?;
    if let Some(stored) = &old.wait.release {
        require(stored == release)?;
        return Ok(DecisionMutation {
            record: old,
            outcome: crate::WriteOutcome::AlreadyApplied,
        });
    }
    require(
        old.receipt.state.revision == expected.revision
            && old.receipt.state.kind == DecisionKind::KoruEdit
            && old.resume.is_none()
            && old.wait.enrollment.is_some()
            && matches!(
                old.wait.phase,
                BotWaitPhase::HoldRequested | BotWaitPhase::CleanupPending
            ),
    )?;
    let OriginWaitStop::Confirmed {
        creation_retirement,
        native_drain,
        ..
    } = release
    else {
        return Err(BotLedgerError::Invalid);
    };
    require(creation_retirement.observed_at_ms <= now && native_drain.observed_at_ms <= now)?;
    let mut observed = old.wait.clone();
    observed.release = Some(release.clone());
    rows::correlate_release(tx, &observed)?;
    let latest: u32 = tx
        .query_row(
            "SELECT max(generation) FROM run_attempts WHERE run_id=?1",
            [expected.run.to_storage()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    require(latest == old.wait.origin.generation.get())?;
    let holding = old.wait.phase == BotWaitPhase::HoldRequested;
    require(if holding {
        old.receipt.state.phase == DecisionPhase::Holding
            && old.receipt.state.authorization == DecisionAuthorization::Pending
    } else {
        old.receipt.state.phase == DecisionPhase::StopPending
    })?;
    let admitted = authority::retained(tx, &old.receipt.binding)?;
    require(admitted.state == crate::BotAdmissionState::Executing)?;
    // Physical release ends the previous active interval. Neither a decision
    // nor a successor resets accumulated execution time.
    let elapsed = admitted.active_elapsed_at(now).min(i64::MAX as u64) as i64;
    tx.execute("UPDATE bot_admissions SET active_elapsed_ms=?2,active_since_ms=NULL,updated_at_ms=max(updated_at_ms,?3) WHERE run_id=?1", params![expected.run.to_storage(),elapsed,now]).map_err(sql)?;
    tx.execute("UPDATE bot_wait_runtime SET release_evidence_json=?2,phase=?3,revision=?4,updated_at_ms=max(updated_at_ms,?5) WHERE wait_id=?1 AND release_evidence_json IS NULL",
        params![old.wait.wait.to_storage(),encoded(release,DECISION_EVIDENCE_BYTES)?,if holding {"retired"} else {"cleanup_pending"},next(old.wait.revision)?.get(),now]).map_err(sql)?;
    tx.execute("UPDATE bot_decisions SET phase=?2,revision=?3,updated_at_ms=max(updated_at_ms,?4) WHERE decision_id=?1",
        params![expected.decision.to_storage(),if holding {"held"} else {"stop_pending"},next(old.receipt.state.revision)?.get(),now]).map_err(sql)?;
    done(
        tx,
        expected.owner,
        expected.run,
        expected.decision,
        crate::WriteOutcome::Applied,
    )
}

/// Final evidence always names the currently retained consumer. Historical
/// origin release is never substituted for an unknown or live successor.
pub(crate) fn consumer_cleanup_on(
    tx: &Transaction<'_>,
    expected: &DecisionWaitExpected,
    cleanup: &ConsumerCleanup,
    now: i64,
) -> Result<DecisionMutation, BotLedgerError> {
    valid(cleanup)?;
    authority::owner(tx, expected.owner, expected.run)?;
    let decision = expected.decision.as_ref().ok_or(BotLedgerError::Invalid)?;
    require(decision.owner == expected.owner && decision.run == expected.run)?;
    let old = required(tx, expected.owner, expected.run, decision.decision)?;
    require(
        old.receipt.binding_digest == decision.binding_digest && old.wait.wait == expected.wait,
    )?;
    if let Some(stored) = &old.wait.cleanup {
        require(stored == cleanup)?;
        return Ok(DecisionMutation {
            record: old,
            outcome: crate::WriteOutcome::AlreadyApplied,
        });
    }
    let observed =
        observation::execution(tx, expected.owner, expected.run)?.ok_or(BotLedgerError::Stale)?;
    require(observed.expected == *expected && old.wait.phase == BotWaitPhase::CleanupPending)?;
    let ConsumerCleanup::Confirmed {
        cleanup_revision,
        target,
        creation_retirement,
        attempt_retirement,
        native_drain,
        retained_effect_outcome,
    } = cleanup
    else {
        return Err(BotLedgerError::Invalid);
    };
    require(
        *cleanup_revision == expected.revision
            && *target == observed.cleanup_target
            && observed.current_attempt.retired
            && observed.current_attempt.phase.is_terminal()
            && observed.current_attempt.host_instance == Some(creation_retirement.host)
            && attempt_retirement.retired_at_ms == observed.current_attempt.updated_at_ms
            && creation_retirement.observed_at_ms <= now
            && native_drain.observed_at_ms <= now,
    )?;
    drain::correlate(tx, expected.owner, old.wait.origin.bot, native_drain)?;
    effect_outcome(tx, &old, retained_effect_outcome.as_ref())?;
    tx.execute("UPDATE bot_wait_runtime SET cleanup_evidence_json=?2,revision=?3,updated_at_ms=max(updated_at_ms,?4) WHERE wait_id=?1 AND cleanup_evidence_json IS NULL",
        params![expected.wait.to_storage(),encoded(cleanup,DECISION_EVIDENCE_BYTES)?,next(expected.revision)?.get(),now]).map_err(sql)?;
    done(
        tx,
        expected.owner,
        expected.run,
        decision.decision,
        crate::WriteOutcome::Applied,
    )
}

fn effect_outcome(
    conn: &Connection,
    record: &DecisionRecord,
    outcome: Option<&DecisionRetainedEffect>,
) -> Result<(), BotLedgerError> {
    if record.receipt.state.kind != DecisionKind::KoruEdit
        || record.receipt.state.authorization != DecisionAuthorization::Consumed
    {
        return require(outcome.is_none());
    }
    let outcome = outcome.ok_or(BotLedgerError::Fenced)?;
    let key = match outcome {
        DecisionRetainedEffect::NotApplied { key, .. }
        | DecisionRetainedEffect::Durable { key, .. }
        | DecisionRetainedEffect::DrainedUnknown { key, .. } => key,
    };
    let DecisionBindingV1::KoruEdit(binding) = &record.receipt.binding else {
        return Err(BotLedgerError::Invalid);
    };
    let resume = record.resume.as_ref().ok_or(BotLedgerError::Fenced)?;
    let (attempt, generation, _, _) = resume.consumer_identity();
    let acknowledgment = match &resume.state {
        DecisionResumeState::InputAcknowledged { acknowledgment }
        | DecisionResumeState::Closed {
            acknowledgment: Some(acknowledgment),
        } => acknowledgment,
        _ => return Err(BotLedgerError::Fenced),
    };
    require(
        key.run == binding.run
            && key.attempt == attempt
            && key.generation == generation
            && key.turn == acknowledgment.turn
            && key.operation_id == binding.operation_id,
    )?;
    let companion:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM native_decision_leases WHERE run_id=?1 AND operation_id=?2 AND decision_id=?3 AND consumer_attempt_id=?4 AND consumer_generation=?5 AND consumer_turn_id=?6 AND consumed_at_ms=?7)",
        params![key.run.to_storage(),key.operation_id.to_string(),record.receipt.decision.to_storage(),key.attempt.to_storage(),key.generation.get(),key.turn.to_string(),record.receipt.consumed_at_ms],|r|r.get(0)).map_err(sql)?;
    require(companion)?;
    let retained =
        crate::native_results::retained_result_on(conn, key)?.ok_or(BotLedgerError::Fenced)?;
    require(
        retained.kind == NativeResultOperation::EditPage
            && retained.target == binding.target
            && retained.destination == binding.workspace
            && retained.fingerprint == binding.edit_fingerprint
            && retained.lease_state == crate::NativeLeaseState::Settled,
    )?;
    require(match outcome {
        DecisionRetainedEffect::NotApplied { code, .. } => {
            retained.refusal == Some(*code) && retained.stage == NativeResultStage::Refused
        }
        DecisionRetainedEffect::Durable { reference, .. } => {
            retained.verified.as_ref() == Some(reference)
                && matches!(
                    retained.stage,
                    NativeResultStage::Verified | NativeResultStage::Registered
                )
        }
        DecisionRetainedEffect::DrainedUnknown { drain, .. } => {
            retained.drain.as_ref() == Some(drain)
                && retained.stage == NativeResultStage::SettledUnknown
        }
    })
}
