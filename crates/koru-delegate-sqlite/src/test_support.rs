// SPDX-License-Identifier: MPL-2.0

//! Shared builders for decision-ledger states, compiled for this crate's own
//! tests and for a dependent that turns on the dev-only `test-support` feature
//! in its `[dev-dependencies]` (nothing shipped enables it). Every builder
//! drives the real `RunsDb` through its public methods only: no row is written
//! and no proof is minted that the ledger would refuse from a trusted caller.
//! Physical observations a runtime would make (an attempt ending, a session
//! binding, an input acknowledgment) are stated as the inputs they are.
//!
//! Each builder leaves only the ledger in the stated state. A runtime harness
//! (the orchestrator's in-memory Run/attempt state) must mirror it itself; the
//! "Runtime mirror" line on each builder says what has to match.
use crate::{AnswerDecision, AttemptPhase, BotLedgerError, DecisionRecord, RunsDb};
use koru_delegate_core::decisions::*;
use koru_delegate_core::{
    AgentTurnId, BotExecutionFence, ConfirmNativeResult, DelegateBootId, FreshBotPolicyProof,
    HostInstanceId, NativeResultKey, NativeResultObservation, NativeResultOperation,
    NativeResultRefusal, PrepareNativeResult, SessionId,
};

fn stale() -> BotLedgerError {
    BotLedgerError::Stale
}

/// The exact current decision CAS ticket for `record`.
pub fn expected(record: &DecisionRecord) -> DecisionExpected {
    DecisionExpected {
        owner: record.wait.origin.owner,
        run: record.wait.origin.run,
        decision: record.receipt.decision,
        revision: record.receipt.state.revision,
        binding_digest: record.receipt.binding_digest,
    }
}

/// Freeze the AI cleanup intent of a Koru decision in Holding (its initial
/// projection already acknowledged).
///
/// Ledger after: decision Pending/Holding at revision+1; wait HoldRequested
/// with the frozen `enrollment_request`; one pending Preserve Control.
/// Runtime mirror: nothing physical changed; the origin attempt still runs.
pub fn request_enrollment(
    db: &mut RunsDb,
    record: &DecisionRecord,
) -> Result<DecisionRecord, BotLedgerError> {
    let f = record.wait.origin;
    let ticket = db
        .decision_wait_execution(f.owner, f.run)?
        .ok_or_else(stale)?
        .expected;
    db.enroll_decision_cleanup(&ticket, DecisionTransitionId::mint())
}

/// Record the AI worker's Enrolled reply for the frozen intent, with the
/// admission's own `ai_attempt_id` (the AI side's receipt, as it would return).
///
/// Ledger after: wait still HoldRequested with `enrollment` set (revision+1).
/// Runtime mirror: none.
pub fn record_enrollment(
    db: &mut RunsDb,
    record: &DecisionRecord,
) -> Result<DecisionRecord, BotLedgerError> {
    let f = record.wait.origin;
    let key = record
        .wait
        .enrollment_request
        .as_ref()
        .ok_or_else(stale)?
        .key
        .clone();
    let ai_attempt = crate::BotLedger::bot_admission(db, f.run)?
        .ok_or_else(stale)?
        .submission
        .provenance
        .ai_attempt_id
        .ok_or(BotLedgerError::Invalid)?;
    db.record_decision_enrollment(
        &expected(record),
        DecisionTransitionId::mint(),
        &BotWaitEnrollmentReply::Enrolled {
            receipt_digest: key.receipt_digest,
            key,
            ai_attempt,
        },
    )
}

/// End and retire the origin attempt (as the physical internal wait-stop
/// would) and return the origin release proof built from the retained attempt.
/// Does not record it; `stop_origin` does.
///
/// Ledger after: origin attempt Ended and retired; decision/wait unchanged.
/// Runtime mirror: the origin attempt has ended and its session is gone.
pub fn origin_release(
    db: &mut RunsDb,
    record: &DecisionRecord,
    observed_at_ms: i64,
) -> Result<OriginWaitStop, BotLedgerError> {
    let f = record.wait.origin;
    let mut attempt = db.current_attempt(f.run)?.ok_or_else(stale)?;
    if !attempt.retired {
        db.advance(f.run, attempt.generation, AttemptPhase::Ended)?;
        attempt = db.retire(f.run, attempt.generation)?;
    }
    let key = &record
        .wait
        .enrollment_request
        .as_ref()
        .ok_or_else(stale)?
        .key;
    Ok(OriginWaitStop::Confirmed {
        origin_fence: f,
        wait_id: record.wait.wait,
        intent_digest: key.receipt_digest,
        enrollment_digest: key.receipt_digest,
        cause: BotWaitCleanupCause::DecisionWaitCleanup,
        creation_retirement: ObservedCreationRetirement {
            creation_key: attempt.provider_session.ok_or_else(stale)?.idempotency,
            host: attempt.host_instance.ok_or_else(stale)?,
            result: ObservedRetirementResult::KnownEnded { session: f.session },
            observed_at_ms,
        },
        native_drain: DecisionNativeDrain {
            run: f.run,
            attempt: f.attempt,
            generation: f.generation,
            observed_at_ms,
            effects: DecisionList::default(),
        },
    })
}

/// `origin_release`, then record it. The origin must have no drained native
/// effect (the proof carries an empty drain list).
///
/// Ledger after: decision Pending/Held; wait Retired with the immutable
/// release; active budget interval closed; host capacity qualifies as
/// released (`BotExecutionCapacity::QualifiedDecisionRelease`).
/// Runtime mirror: the origin attempt ended; no live attempt for the Run.
pub fn stop_origin(
    db: &mut RunsDb,
    record: &DecisionRecord,
    observed_at_ms: i64,
) -> Result<DecisionRecord, BotLedgerError> {
    let release = origin_release(db, record, observed_at_ms)?;
    db.record_decision_origin_stop(&expected(record), DecisionTransitionId::mint(), &release)
}

/// Answer AllowOnce as the binding's human, reserve the one recorded
/// successor on `host`, bind its created `session` and walk it to Running.
/// The successor's continuation input is **not** acknowledged.
///
/// Ledger after: decision Granted/ResumePrepared; wait ResumeReserved; resume
/// row SessionBound; a new attempt at origin generation+1 (fresh attempt id)
/// with provider session `{creation key, session}` on `host`, phase Running,
/// not retired. The original InitialSend is untouched. Delivery or fresh native
/// work by the successor is refused at this point.
/// Runtime mirror: the current attempt is the successor (that attempt id,
/// generation, `host`, `session`); no turn is known for it yet.
pub fn bind_successor(
    db: &mut RunsDb,
    record: &DecisionRecord,
    host: HostInstanceId,
    session: SessionId,
    proof: &FreshBotPolicyProof,
    boot: DelegateBootId,
) -> Result<DecisionRecord, BotLedgerError> {
    // Any Koru decision (an edit, a private read) is answered AllowOnce by
    // the binding's human; a native prompt has its own offered choices.
    if !record.receipt.state.kind.resumes_by_successor() {
        return Err(BotLedgerError::Invalid);
    }
    let answer = AnswerDecision {
        owner: record.wait.origin.owner,
        run: record.wait.origin.run,
        decision: record.receipt.decision,
        expected_revision: record.receipt.state.revision,
        binding_digest: record.receipt.binding_digest,
        request: DecisionAnswerId::mint(),
        human: record.receipt.binding.human_profile(),
        choice: DecisionAnswerChoice::AllowOnce {},
        authority_digest: DecisionDigest::from_bytes([9; 32]),
    };
    let record = db.answer_bot_decision(&answer, DecisionTransitionId::mint(), proof, boot)?;
    let record = db.reserve_decision_successor(
        &expected(&record),
        DecisionTransitionId::mint(),
        host,
        [42; 32],
        proof,
        boot,
    )?;
    let (_, generation, _, creation) = record
        .resume
        .as_ref()
        .ok_or_else(stale)?
        .consumer_identity();
    let record = db.bind_decision_successor(
        &expected(&record),
        DecisionTransitionId::mint(),
        creation,
        host,
        session,
    )?;
    let run = record.wait.origin.run;
    db.advance(run, generation, AttemptPhase::Starting)?;
    db.advance(run, generation, AttemptPhase::Running)?;
    Ok(record)
}

/// Record the positive acknowledgment of the successor's continuation input
/// as accepted `turn`, and return the exact consumer fence.
///
/// Ledger after: resume InputAcknowledged with `turn`; decision AwaitRetry.
/// Successor delivery and the one-use consume are now admitted.
/// Runtime mirror: the successor attempt is live on `session` with `turn` as
/// its acknowledged input turn; the returned fence is the current execution
/// fence (same owner/bot/run as the origin).
pub fn acknowledge_successor(
    db: &mut RunsDb,
    record: &DecisionRecord,
    turn: AgentTurnId,
) -> Result<(DecisionRecord, BotExecutionFence), BotLedgerError> {
    let resume = record.resume.as_ref().ok_or_else(stale)?;
    let record = db.record_decision_resume_input(
        &expected(record),
        DecisionTransitionId::mint(),
        resume.input_key,
        resume.input_fingerprint,
        Some(&DecisionInputAcknowledgment {
            turn,
            receipt_digest: DecisionDigest::from_bytes([43; 32]),
        }),
    )?;
    let (attempt, generation, session, _) = record
        .resume
        .as_ref()
        .ok_or_else(stale)?
        .consumer_identity();
    let f = record.wait.origin;
    let fence = BotExecutionFence {
        owner: f.owner,
        bot: f.bot,
        run: f.run,
        attempt,
        generation,
        session: session.ok_or_else(stale)?,
        turn: Some(turn),
    };
    Ok((record, fence))
}

/// The exact one-use edit request the binding authorizes, keyed by the
/// acknowledged consumer `fence`. The binding's `expected_revision` must be a
/// real semantic version vector; consume compares it semantically.
pub fn consume_request(
    record: &DecisionRecord,
    fence: &BotExecutionFence,
) -> Result<PrepareNativeResult, BotLedgerError> {
    let DecisionBindingV1::KoruEdit(edit) = &record.receipt.binding else {
        return Err(BotLedgerError::Invalid);
    };
    Ok(PrepareNativeResult {
        key: NativeResultKey {
            run: edit.run,
            attempt: fence.attempt,
            generation: fence.generation,
            turn: fence.turn.ok_or(BotLedgerError::Invalid)?,
            operation_id: edit.operation_id,
        },
        kind: NativeResultOperation::EditPage,
        destination: edit.workspace,
        target: Some(edit.target),
        request_fingerprint: edit.edit_fingerprint,
        expected_revision: Some(edit.expected_revision.as_slice().to_vec()),
        config_revision: edit.global_policy.config_revision.get(),
        policy_revision: edit.global_policy.policy_revision.get(),
        title: "Original edit".into(),
    })
}

/// From an acknowledged successor: consume the one edit, observe it
/// NotApplied, and request Canceled cleanup. Returns the decision and the
/// consumed intent's key.
///
/// Ledger after: decision Consumed/StopPending; wait CleanupPending owned by
/// a Requested(Canceled) carrier (pending Control); resume Closed keeping its
/// acknowledgment; the native intent Refused with a settled lease (so
/// `decision_retained_effect` returns NotApplied and the consumer's drain list
/// is empty); the successor attempt is still Running and not retired.
/// Runtime mirror: the successor attempt is live; final cleanup must end and
/// retire it, then supply `decision_native_drain` and `decision_retained_effect`.
pub fn consumed_for_cleanup(
    db: &mut RunsDb,
    record: &DecisionRecord,
    fence: &BotExecutionFence,
    proof: &FreshBotPolicyProof,
    boot: DelegateBootId,
) -> Result<(DecisionRecord, NativeResultKey), BotLedgerError> {
    let DecisionBindingV1::KoruEdit(edit) = &record.receipt.binding else {
        return Err(BotLedgerError::Invalid);
    };
    let request = consume_request(record, fence)?;
    db.consume_decision_edit(
        &expected(record),
        DecisionTransitionId::mint(),
        &request,
        edit.payload.as_slice(),
        fence,
        proof,
        boot,
    )?;
    crate::NativeResultLedger::confirm_native_result(
        db,
        &ConfirmNativeResult {
            key: request.key.clone(),
            request_fingerprint: request.request_fingerprint,
            observation: NativeResultObservation::NotApplied {
                code: NativeResultRefusal::Conflict,
            },
        },
    )?;
    let f = record.wait.origin;
    let ticket = db
        .decision_wait_execution(f.owner, f.run)?
        .ok_or_else(stale)?
        .expected;
    db.request_decision_cleanup(
        &ticket,
        DecisionTransitionId::mint(),
        DecisionClosureReason::Canceled,
    )?;
    let record = db
        .bot_decision(f.owner, f.run, record.receipt.decision)?
        .ok_or_else(stale)?;
    Ok((record, request.key))
}
