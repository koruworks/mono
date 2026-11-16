// SPDX-License-Identifier: MPL-2.0

//! The parent's resume after a child wait: a recorded successor attempt of
//! the same Run, keyed by the wait. The steps mirror a decision successor's
//! and are their own functions. A resume never changes the admission's state.
use super::*;
use crate::{
    BOT_ACTIVE_BUDGET_MS, BOT_HOST_EXECUTION_LIMIT, BotAdmissionRecord, BotExecutionCapacity,
    BotExecutionInventory, RunsDb, WriteOp,
};
use koru_delegate_core::decisions::{
    BotWaitPhase, DECISION_RECEIPT_BYTES, DecisionControlAction, DecisionInputAcknowledgment,
    DecisionResumeState, DecisionTransitionId, DecisionWaitExpected, encode_decision_value,
};
use koru_delegate_core::{
    AttemptGeneration, FreshBotPolicyProof, HostInstanceId, IdempotencyKey, ProviderSessionKey,
    ResumeSeam, RunAttemptId, SessionId,
};
use rusqlite::{Connection, Transaction, params};

/// How many child waits could be resumed right now if a host slot were free:
/// qualified releases that are `retired`, have no open request, whose parent
/// Bot is not paused and whose active budget is not spent. A decision wait
/// is never counted. `reserve_bot` offers a slot to queued work only while
/// every one of these could also be served.
pub(crate) fn due_on(
    conn: &Connection,
    inventory: &BotExecutionInventory,
    now: i64,
) -> Result<usize, BotLedgerError> {
    let mut due = 0;
    for owned in &inventory.owned {
        let BotExecutionCapacity::QualifiedChildRelease { wait, .. } = owned.capacity else {
            continue;
        };
        let execution = &owned.execution;
        let record = crate::decisions::child_wait_on(conn, execution.run, wait)?
            .ok_or_else(|| corrupt("bot_wait_runtime", "wait_id"))?;
        if record.phase == BotWaitPhase::Retired
            && rows::open_count(conn, wait)? == 0
            && !crate::bots::barrier_on(conn, execution.owner, execution.bot)?
                .is_some_and(|barrier| barrier.paused)
            && execution.active_elapsed_at(now) < BOT_ACTIVE_BUDGET_MS
        {
            due += 1;
        }
    }
    Ok(due)
}

/// Fresh activity of a Run in a child wait belongs to its reserved successor
/// alone: the wait is `resume_reserved` with no cleanup, and its resume row
/// names exactly this attempt and generation and is not closed.
pub(crate) fn fresh_activity_on(
    conn: &Connection,
    wait: &ChildWaitRecord,
    attempt: RunAttemptId,
    generation: AttemptGeneration,
) -> Result<(), BotLedgerError> {
    if wait.phase != BotWaitPhase::ResumeReserved || wait.cleanup.is_some() {
        return Err(BotLedgerError::Fenced);
    }
    let resume = crate::decisions::child_resume_on(conn, wait)?.ok_or(BotLedgerError::Fenced)?;
    let (consumer, current, _, _) = resume.consumer_identity().ok_or(BotLedgerError::Fenced)?;
    if consumer != attempt
        || current != generation
        || matches!(resume.state, DecisionResumeState::Closed { .. })
    {
        return Err(BotLedgerError::Fenced);
    }
    Ok(())
}

/// Stop closes the retained input identity; it erases no accepted turn.
pub(crate) fn close_on(tx: &Connection, wait: BotWaitId, now: i64) -> Result<(), BotLedgerError> {
    tx.execute(
        "UPDATE bot_child_resumes SET state='closed',updated_at_ms=max(updated_at_ms,?2)
          WHERE wait_id=?1 AND state<>'closed'",
        params![wait.to_storage(), now],
    )
    .map_err(sql)?;
    Ok(())
}

fn expected_wait(
    tx: &Connection,
    expected: &DecisionWaitExpected,
) -> Result<ChildWaitRecord, BotLedgerError> {
    if expected.decision.is_some() {
        return Err(BotLedgerError::Invalid);
    }
    crate::decisions::owner_on(tx, expected.owner, expected.run)?;
    crate::decisions::child_wait_on(tx, expected.run, expected.wait)?.ok_or(BotLedgerError::Stale)
}

fn execution(
    tx: &Connection,
    expected: &DecisionWaitExpected,
) -> Result<ChildWaitExecution, BotLedgerError> {
    observation::execution_on(tx, expected.owner, expected.run)?.ok_or(BotLedgerError::Stale)
}

fn reserve_on(
    tx: &Transaction<'_>,
    expected: &DecisionWaitExpected,
    host: HostInstanceId,
    fingerprint: [u8; 32],
    proof: Option<&FreshBotPolicyProof>,
    now: i64,
) -> Result<ChildSuccessorReservation, BotLedgerError> {
    let old = expected_wait(tx, expected)?;
    if let Some(resume) = crate::decisions::child_resume_on(tx, &old)? {
        let (attempt, generation, _, _) =
            resume.consumer_identity().ok_or(BotLedgerError::Fenced)?;
        let retained = crate::attempts::retained_bounded(tx, expected.run, generation)?
            .ok_or(BotLedgerError::Fenced)?;
        require(
            resume.input_fingerprint == fingerprint
                && retained.attempt == attempt
                && retained.host_instance == Some(host),
        )?;
        return Ok(ChildSuccessorReservation::AlreadyReserved(Box::new(
            execution(
                tx,
                expected_current(tx, expected)?.as_ref().unwrap_or(expected),
            )?,
        )));
    }
    if old.revision != expected.revision {
        return Err(BotLedgerError::Stale);
    }
    if old.phase != BotWaitPhase::Retired {
        return Ok(ChildSuccessorReservation::NotRetired);
    }
    if rows::open_count(tx, old.wait)? != 0 {
        return Ok(ChildSuccessorReservation::OpenRequests);
    }
    let admitted: BotAdmissionRecord = crate::bots::routines::native_admission(tx, expected.run)?;
    // A paused parent Bot holds its resume for as long as it is paused. That
    // is never a refusal, for a Routine parent either, so it is read first.
    let p = &admitted.submission.provenance;
    if crate::bots::barrier_on(tx, p.owner, p.bot)?.is_some_and(|barrier| barrier.paused) {
        return Ok(ChildSuccessorReservation::Paused);
    }
    // A Routine parent's link, its grant barrier and the Bot's barrier must
    // be current; an interactive parent continues under its captured policy.
    crate::bots::execution_fresh_on(tx, &admitted, proof, admitted.submission_version == 2)?;
    let inventory = crate::decisions::execution_inventory_on(tx, expected.owner.device)?;
    let qualified = inventory.owned.iter().any(|owned| {
        owned.execution.run == expected.run
            && matches!(owned.capacity, BotExecutionCapacity::QualifiedChildRelease { wait, .. } if wait == old.wait)
    });
    if !qualified || admitted.active_since_ms.is_some() {
        return Ok(ChildSuccessorReservation::Unqualified);
    }
    if admitted.active_elapsed_at(now) >= BOT_ACTIVE_BUDGET_MS {
        return Ok(ChildSuccessorReservation::BudgetSpent);
    }
    if inventory.occupied >= BOT_HOST_EXECUTION_LIMIT {
        return Ok(ChildSuccessorReservation::HostBusy);
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
    let f = old.origin;
    tx.execute(
        "INSERT INTO bot_child_resumes(wait_id,run_id,origin_attempt_id,origin_generation,origin_session_id,consumer_attempt_id,consumer_generation,session_creation_key,input_key,input_fingerprint,state,created_at_ms,updated_at_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'prepared',?11,?11)",
        params![
            old.wait.to_storage(),
            expected.run.to_storage(),
            f.attempt.to_storage(),
            f.generation.get(),
            f.session.to_string(),
            attempt.attempt.to_storage(),
            attempt.generation.get(),
            creation_key.to_string(),
            input_key.to_string(),
            fingerprint.as_slice(),
            now
        ],
    )
    .map_err(sql)?;
    let changed = tx
        .execute(
            "UPDATE bot_wait_runtime SET phase='resume_reserved',revision=?2,updated_at_ms=max(updated_at_ms,?3)
              WHERE wait_id=?1 AND revision=?4",
            params![
                old.wait.to_storage(),
                next(old.revision)?.get(),
                now,
                old.revision.get()
            ],
        )
        .map_err(sql)?;
    require(changed == 1)?;
    let current = expected_current(tx, expected)?.ok_or(BotLedgerError::Stale)?;
    Ok(ChildSuccessorReservation::Reserved(Box::new(execution(
        tx, &current,
    )?)))
}

/// The same wait at whatever revision it is retained at now.
fn expected_current(
    tx: &Connection,
    expected: &DecisionWaitExpected,
) -> Result<Option<DecisionWaitExpected>, BotLedgerError> {
    Ok(
        crate::decisions::child_wait_on(tx, expected.run, expected.wait)?.map(|wait| {
            DecisionWaitExpected {
                revision: wait.revision,
                ..expected.clone()
            }
        }),
    )
}

fn bind_on(
    tx: &Transaction<'_>,
    expected: &DecisionWaitExpected,
    creation_key: IdempotencyKey,
    host: HostInstanceId,
    session: SessionId,
    now: i64,
) -> Result<(), BotLedgerError> {
    let old = expected_wait(tx, expected)?;
    let resume = crate::decisions::child_resume_on(tx, &old)?.ok_or(BotLedgerError::Stale)?;
    let (attempt, generation, stored, key) =
        resume.consumer_identity().ok_or(BotLedgerError::Fenced)?;
    require(key == creation_key && session != resume.origin.session)?;
    let row = crate::attempts::retained_bounded(tx, expected.run, generation)?
        .ok_or(BotLedgerError::Fenced)?;
    require(row.attempt == attempt && row.host_instance == Some(host))?;
    if let Some(stored) = stored {
        return require(stored == session);
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
    let changed = tx
        .execute(
            "UPDATE bot_child_resumes SET consumer_session_id=?2,state=?3,updated_at_ms=max(updated_at_ms,?4)
              WHERE wait_id=?1 AND consumer_session_id IS NULL",
            params![old.wait.to_storage(), session.to_string(), state, now],
        )
        .map_err(sql)?;
    require(changed == 1)
}

fn input_on(
    tx: &Transaction<'_>,
    expected: &DecisionWaitExpected,
    transition: DecisionTransitionId,
    input_key: IdempotencyKey,
    fingerprint: [u8; 32],
    acknowledgment: Option<&DecisionInputAcknowledgment>,
    now: i64,
) -> Result<(), BotLedgerError> {
    let old = expected_wait(tx, expected)?;
    let resume = crate::decisions::child_resume_on(tx, &old)?.ok_or(BotLedgerError::Stale)?;
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
        return Ok(());
    }
    let (_, _, session, _) = resume.consumer_identity().ok_or(BotLedgerError::Fenced)?;
    require(session.is_some())?;
    if acknowledgment.is_none()
        && matches!(
            resume.state,
            DecisionResumeState::InputUnconfirmed {} | DecisionResumeState::Closed { .. }
        )
    {
        return Ok(());
    }
    require(matches!(
        resume.state,
        DecisionResumeState::SessionBound {}
            | DecisionResumeState::InputUnconfirmed {}
            | DecisionResumeState::Closed { .. }
    ))?;
    let body = acknowledgment
        .map(|value| {
            String::from_utf8(
                encode_decision_value(value, DECISION_RECEIPT_BYTES)
                    .map_err(|_| BotLedgerError::Invalid)?,
            )
            .map_err(|_| BotLedgerError::Invalid)
        })
        .transpose()?;
    let closed = matches!(resume.state, DecisionResumeState::Closed { .. });
    let state = if closed {
        "closed"
    } else if acknowledgment.is_some() {
        "input_acknowledged"
    } else {
        "input_unconfirmed"
    };
    tx.execute(
        "UPDATE bot_child_resumes SET state=?2,accepted_turn_id=?3,receipt_json=?4,updated_at_ms=max(updated_at_ms,?5)
          WHERE wait_id=?1 AND accepted_turn_id IS NULL",
        params![
            old.wait.to_storage(),
            state,
            acknowledgment.map(|value| value.turn.to_string()),
            body,
            now
        ],
    )
    .map_err(sql)?;
    // A late acknowledgment after a Stop is retained history, never
    // resurrection: only a live reserved resume clears the waiting marker.
    if acknowledgment.is_some()
        && !closed
        && old.phase == BotWaitPhase::ResumeReserved
        && old.cleanup.is_none()
    {
        crate::decisions::commit_child_control_on(
            tx,
            &old,
            transition,
            DecisionControlAction::Resume {
                via: ResumeSeam::RecordedContext,
            },
            None,
            now,
        )?;
    }
    Ok(())
}

impl RunsDb {
    /// See [`crate::BotLedger::reserve_child_successor`].
    pub fn reserve_child_successor(
        &mut self,
        expected: &DecisionWaitExpected,
        host: HostInstanceId,
        fingerprint: [u8; 32],
        proof: Option<&FreshBotPolicyProof>,
    ) -> Result<ChildSuccessorReservation, BotLedgerError> {
        self.write_with_error(WriteOp::ChildWait, |tx, now| {
            reserve_on(tx, expected, host, fingerprint, proof, now)
        })
    }

    /// Reconcile the exact creation key of the successor, including a late
    /// positive lookup after Stop. It authorizes no start and no input.
    pub fn bind_child_successor(
        &mut self,
        expected: &DecisionWaitExpected,
        creation: IdempotencyKey,
        host: HostInstanceId,
        session: SessionId,
    ) -> Result<ChildWaitExecution, BotLedgerError> {
        self.write_with_error(WriteOp::ChildWait, |tx, now| {
            bind_on(tx, expected, creation, host, session, now)?;
            let current = expected_current(tx, expected)?.ok_or(BotLedgerError::Stale)?;
            execution(tx, &current)
        })
    }

    /// See [`crate::BotLedger::record_child_resume_input`].
    pub fn record_child_resume_input(
        &mut self,
        expected: &DecisionWaitExpected,
        transition: DecisionTransitionId,
        input: IdempotencyKey,
        fingerprint: [u8; 32],
        acknowledgment: Option<&DecisionInputAcknowledgment>,
    ) -> Result<ChildWaitExecution, BotLedgerError> {
        self.write_with_error(WriteOp::ChildWait, |tx, now| {
            input_on(
                tx,
                expected,
                transition,
                input,
                fingerprint,
                acknowledgment,
                now,
            )?;
            let current = expected_current(tx, expected)?.ok_or(BotLedgerError::Stale)?;
            execution(tx, &current)
        })
    }

    /// The acknowledged continuation of the current attempt when it is a
    /// recorded child-wait successor. Read-only history.
    pub fn child_acknowledged_continuation(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<ChildAcknowledgedContinuation>, BotLedgerError> {
        crate::decisions::owner_on(self.conn(), owner, run)?;
        let Some(attempt) = crate::attempts::latest(self.conn(), run)? else {
            return Ok(None);
        };
        let Some((wait, resume)) = crate::decisions::child_consumer_on(self.conn(), run, &attempt)?
        else {
            return Ok(None);
        };
        require(wait.origin.owner == owner && wait.origin.run == run)?;
        let Some(turn) = resume.accepted_turn() else {
            return Ok(None);
        };
        let (attempt, generation, session, _) =
            resume.consumer_identity().ok_or(BotLedgerError::Fenced)?;
        Ok(Some(ChildAcknowledgedContinuation {
            wait: wait.wait,
            attempt,
            generation,
            session: session.ok_or(BotLedgerError::Fenced)?,
            turn,
        }))
    }
}
