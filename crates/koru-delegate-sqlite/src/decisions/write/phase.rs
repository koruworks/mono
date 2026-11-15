// SPDX-License-Identifier: MPL-2.0

use super::*;

#[derive(Clone, Copy, Debug)]
pub(crate) enum DecisionStopCause {
    Canceled,
    Expired,
    ClockUncertain,
    AuthorityChanged,
}

/// This fences fresh work and records cleanup debt; it does not settle an
/// unresolved effect, discount capacity, or prove that a process stopped.
pub(crate) fn phase_on(
    tx: &Transaction<'_>,
    owner: BotOwner,
    run: RunId,
    id: DecisionId,
    expected: DecisionRevision,
    cause: DecisionStopCause,
    now: i64,
) -> Result<DecisionMutation, BotLedgerError> {
    authority::owner(tx, owner, run)?;
    let old = required(tx, owner, run, id)?;
    if old.receipt.state.phase == DecisionPhase::Settled {
        return Err(BotLedgerError::Stale);
    }
    let auth = if old.receipt.state.authorization == DecisionAuthorization::Consumed {
        DecisionAuthorization::Consumed
    } else {
        match cause {
            DecisionStopCause::Expired | DecisionStopCause::ClockUncertain => {
                DecisionAuthorization::Expired
            }
            _ => DecisionAuthorization::Canceled,
        }
    };
    // C3 replays the exact retained control target/cause before this helper.
    // The row's authorization alone cannot prove which stop request committed.
    if old.receipt.state.phase == DecisionPhase::StopPending {
        return Err(BotLedgerError::Stale);
    }
    if old.receipt.state.revision != expected {
        return Err(BotLedgerError::Stale);
    }
    if matches!(cause, DecisionStopCause::Expired) && now < old.receipt.deadline.deadline_at_ms {
        return Err(BotLedgerError::Stale);
    }
    // Caller decides ClockUncertain from its retained clock/boot observation.
    // The decision row does not claim a new elapsed-time service.
    let updated = now.max(old.receipt.updated_at_ms);
    tx.execute("UPDATE bot_decisions SET authorization=?2,phase='stop_pending',revision=?3,last_observed_at_ms=max(last_observed_at_ms,?4),updated_at_ms=?4 WHERE decision_id=?1 AND revision=?5",params![id.to_storage(),token(auth)?,next(expected)?.get(),updated,expected.get()]).map_err(sql)?;
    tx.execute("UPDATE bot_wait_runtime SET phase='cleanup_pending',revision=?2,updated_at_ms=max(updated_at_ms,?3) WHERE wait_id=?1 AND revision=?4",params![old.wait.wait.to_storage(),next(old.wait.revision)?.get(),updated,old.wait.revision.get()]).map_err(sql)?;
    if let Some(resume) = &old.resume {
        super::resume::close_resume_on(tx,&DecisionExpected {
            owner,run,decision:id,revision:next(expected)?,binding_digest:old.receipt.binding_digest,
        },resume.input_key,updated)?;
    }
    done(tx, owner, run, id, crate::WriteOutcome::Applied)
}

/// Freeze exact post-transition AI cleanup intent before external enrollment.
/// C3 must atomically retain the matching control target before commit.
pub(crate) fn enroll_on(
    tx: &Transaction<'_>,
    owner: BotOwner,
    run: RunId,
    id: DecisionId,
    expected: DecisionRevision,
    wait_revision: DecisionRevision,
    now: i64,
) -> Result<DecisionMutation, BotLedgerError> {
    authority::owner(tx, owner, run)?;
    let old = required(tx, owner, run, id)?;
    if let Some(request) = &old.wait.enrollment_request {
        require(
            request.key.decision_revision == next(expected)?
                && request.key.wait_revision == next(wait_revision)?,
        )?;
        return Ok(DecisionMutation {
            record: old,
            outcome: crate::WriteOutcome::AlreadyApplied,
        });
    }
    if old.receipt.state.revision != expected || old.wait.revision != wait_revision {
        return Err(BotLedgerError::Stale);
    }
    if old.receipt.state.kind != DecisionKind::KoruEdit
        || old.receipt.state.authorization != DecisionAuthorization::Pending
        || old.receipt.state.phase != DecisionPhase::Holding
        || old.wait.phase != BotWaitPhase::Fenced
    {
        return Err(BotLedgerError::Fenced);
    }
    let f = old.receipt.binding.origin();
    let intent = BotWaitCleanupIntentV1 {
        version: DecisionVersionV1,
        owner,
        bot: f.bot,
        run,
        wait_id: old.wait.wait,
        decision_id: id,
        decision_revision: next(expected)?,
        wait_revision: next(wait_revision)?,
        origin_attempt: f.attempt,
        origin_generation: f.generation,
        origin_session: f.session,
        origin_turn: f.turn,
        cause: BotWaitCleanupCause::DecisionWaitCleanup,
    };
    let request = EnrollBotWaitCleanup {
        key: intent.key().map_err(|_| BotLedgerError::Invalid)?,
        cleanup_intent: intent,
    };
    valid(&request)?;
    let body = encoded(&request, DECISION_EVIDENCE_BYTES)?;
    tx.execute("UPDATE bot_wait_runtime SET phase='hold_requested',revision=?2,ai_enrollment_request_json=?3,updated_at_ms=max(updated_at_ms,?4) WHERE wait_id=?1 AND revision=?5",params![old.wait.wait.to_storage(),request.key.wait_revision.get(),body,now,wait_revision.get()]).map_err(sql)?;
    tx.execute("UPDATE bot_decisions SET revision=?2,updated_at_ms=max(updated_at_ms,?3) WHERE decision_id=?1 AND revision=?4",params![id.to_storage(),request.key.decision_revision.get(),now,expected.get()]).map_err(sql)?;
    done(tx, owner, run, id, crate::WriteOutcome::Applied)
}

/// A late exact Enrolled reply records history even after Stop; it never
/// changes current eligibility, phase, or permission to issue an internal stop.
pub(crate) fn record_enrollment_on(
    tx: &Transaction<'_>,
    owner: BotOwner,
    run: RunId,
    id: DecisionId,
    reply: &BotWaitEnrollmentReply,
    now: i64,
) -> Result<DecisionMutation, BotLedgerError> {
    authority::owner(tx, owner, run)?;
    let old = required(tx, owner, run, id)?;
    let request = old
        .wait
        .enrollment_request
        .as_ref()
        .ok_or(BotLedgerError::Stale)?;
    if !matches!(reply, BotWaitEnrollmentReply::Enrolled { .. })
        || !reply.matches_request(&request.key, false)
    {
        return Err(BotLedgerError::Conflict);
    }
    if let Some(prior) = &old.wait.enrollment {
        require(prior == reply)?;
        return Ok(DecisionMutation {
            record: old,
            outcome: crate::WriteOutcome::AlreadyApplied,
        });
    }
    let BotWaitEnrollmentReply::Enrolled { ai_attempt, .. } = reply else {
        return Err(BotLedgerError::Conflict);
    };
    let admitted = authority::retained(tx, &old.receipt.binding)?;
    require(admitted.submission.provenance.ai_attempt_id == Some(*ai_attempt))?;
    let body = encoded(reply, DECISION_EVIDENCE_BYTES)?;
    let changed=tx.execute("UPDATE bot_wait_runtime SET ai_cleanup_receipt_json=?2,revision=?3,updated_at_ms=max(updated_at_ms,?4) WHERE wait_id=?1 AND revision=?5 AND ai_cleanup_receipt_json IS NULL",params![old.wait.wait.to_storage(),body,next(old.wait.revision)?.get(),now,old.wait.revision.get()]).map_err(sql)?;
    if changed != 1 {
        return Err(BotLedgerError::Stale);
    }
    done(tx, owner, run, id, crate::WriteOutcome::Applied)
}
