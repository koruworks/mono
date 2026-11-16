// SPDX-License-Identifier: MPL-2.0

//! Closed controls use retained targets, never arbitrary caller effects.
use super::*;

fn stop_cause(
    cause: DecisionClosureReason,
) -> Result<super::super::write::DecisionStopCause, BotLedgerError> {
    use super::super::write::DecisionStopCause as C;
    Ok(match cause {
        DecisionClosureReason::Canceled => C::Canceled,
        DecisionClosureReason::Expired => C::Expired,
        DecisionClosureReason::ClockUncertain => C::ClockUncertain,
        DecisionClosureReason::AuthorityChanged => C::AuthorityChanged,
        _ => return Err(BotLedgerError::Invalid),
    })
}

pub(super) fn disposition(
    receipt: &DecisionReceipt,
) -> Result<DecisionDisposition, BotLedgerError> {
    Ok(match &receipt.closure {
        None => DecisionDisposition::Open {
            state: receipt.state.clone(),
        },
        Some(closure) => DecisionDisposition::Settled {
            state: receipt.state.clone(),
            reason: closure.reason,
            outcome_digest: decision_outcome_digest(receipt)
                .map_err(|_| BotLedgerError::Invalid)?,
        },
    })
}

impl RunsDb {
    /// Journal cleanup and its no-effects Control atomically. This preserves
    /// canonical status until actual physical cleanup is qualified. The exact
    /// retained transition replays before current revision/clock checks.
    pub fn request_decision_cleanup(
        &mut self,
        expected: &DecisionWaitExpected,
        transition: DecisionTransitionId,
        cause: DecisionClosureReason,
    ) -> Result<DecisionProjectionRecord, BotLedgerError> {
        self.write_with_error(crate::WriteOp::RecordDecisionProjection, |tx, now| {
            request_on(tx, expected, transition, cause, now)
        })
    }
}

pub(super) fn request_on(
    tx: &Transaction<'_>,
    expected: &DecisionWaitExpected,
    transition: DecisionTransitionId,
    cause: DecisionClosureReason,
    now: i64,
) -> Result<DecisionProjectionRecord, BotLedgerError> {
    let stop = stop_cause(cause)?;
    authority::owner(tx, expected.owner, expected.run)?;
    if let Some(old) = read(tx, expected.owner, expected.run, transition)? {
        require(old.identity.wait == expected.wait)?;
        let RunProjectionTargetV1::Control {
            action:
                DecisionControlAction::CleanupRequested {
                    cause: retained,
                    expected_wait_revision,
                },
            disposition,
            ..
        } = &old.target
        else {
            return Err(BotLedgerError::Conflict);
        };
        require(*retained == cause && *expected_wait_revision == expected.revision)?;
        match (&expected.decision, &old.identity.decision, disposition) {
            (Some(wanted), Some(stored), Some(disposition)) => {
                require(
                    wanted.owner == expected.owner
                        && wanted.run == expected.run
                        && wanted.decision == stored.decision,
                )?;
                let record =
                    super::super::rows::read(tx, expected.owner, expected.run, wanted.decision)?
                        .ok_or(BotLedgerError::Stale)?;
                require(record.receipt.binding_digest == wanted.binding_digest)?;
                let resulting = match disposition {
                    DecisionDisposition::Open { .. } => next(wanted.revision)?,
                    DecisionDisposition::Settled { .. } => wanted.revision,
                };
                require(stored.revision == resulting)?;
            }
            (None, None, None) => {}
            _ => return Err(BotLedgerError::Conflict),
        }
        return Ok(old);
    }
    let observed = super::super::observation::execution(tx, expected.owner, expected.run)?
        .ok_or(BotLedgerError::Stale)?;
    require(observed.expected == *expected)?;
    let context = context(tx, expected.owner, expected.run, expected.wait)?;
    let next_wait = next(expected.revision)?;
    let disposition = if let Some(decision) = &expected.decision {
        let old = super::super::rows::read(tx, expected.owner, expected.run, decision.decision)?
            .ok_or(BotLedgerError::Stale)?;
        if old.receipt.state.phase == DecisionPhase::Settled {
            // Once final cleanup is recorded nothing more can be requested
            // (contract section 6). A later retry must not supersede the
            // still-unpublished final Control with a new request; an open
            // decision refuses the same way (`phase_on`, StopPending).
            if observed.wait.cleanup.is_some() {
                return Err(BotLedgerError::Stale);
            }
            require(matches!(
                cause,
                DecisionClosureReason::Canceled | DecisionClosureReason::AuthorityChanged
            ))?;
            tx.execute("UPDATE bot_wait_runtime SET phase='cleanup_pending',revision=?2,updated_at_ms=max(updated_at_ms,?3) WHERE wait_id=?1 AND revision=?4",
                        params![expected.wait.to_storage(),next_wait.get(),now,expected.revision.get()]).map_err(sql)?;
            Some(disposition(&old.receipt)?)
        } else {
            let mutation = super::super::write::phase_on(
                tx,
                expected.owner,
                expected.run,
                decision.decision,
                decision.revision,
                stop,
                now,
            )?;
            require(mutation.record.wait.revision == next_wait)?;
            Some(disposition(&mutation.record.receipt)?)
        }
    } else {
        require(!matches!(
            observed.wait.reason,
            BotWaitReason::Decision { .. }
        ))?;
        tx.execute("UPDATE bot_wait_runtime SET phase='cleanup_pending',revision=?2,updated_at_ms=max(updated_at_ms,?3) WHERE wait_id=?1 AND revision=?4",
                    params![expected.wait.to_storage(),next_wait.get(),now,expected.revision.get()]).map_err(sql)?;
        None
    };
    let target = RunProjectionTargetV1::Control {
        version: DecisionVersionV1,
        predecessor: context.predecessor.clone(),
        effects: DecisionProjectionEffects {
            event: None,
            status: context.predecessor.status.clone(),
            failure_reason: context.predecessor.failure_reason,
            economics_at_ms: None,
            steps: DecisionList::default(),
            models: DecisionList::default(),
            publications: DecisionList::default(),
        },
        fence: observed.wait.origin,
        disposition,
        wait_phase: BotWaitPhase::CleanupPending,
        reason: observed.wait.reason,
        action: DecisionControlAction::CleanupRequested {
            cause,
            expected_wait_revision: expected.revision,
        },
        retained_bookkeeping: context.bookkeeping,
        flush: DecisionList::default(),
        delivery: None,
    };
    insert::insert_on(
        tx,
        expected.owner,
        transition,
        expected.wait,
        &context.base,
        &target,
        now,
    )
}

#[derive(Clone)]
pub(super) struct ControlContext {
    pub base: DecisionPublicationBase,
    pub predecessor: DecisionProjectionPredecessor,
    pub bookkeeping: Option<DecisionSourceBookkeeping>,
}

/// Same-wait baseline without a document/history import: one Applied body and
/// the at-most-two open prefix. Initial wait publication must already exist.
pub(super) fn context(
    conn: &Connection,
    owner: BotOwner,
    run: RunId,
    wait: BotWaitId,
) -> Result<ControlContext, BotLedgerError> {
    let mut open = prefix(conn, owner, run)?;
    require(open.iter().all(|r| r.identity.wait == wait))?;
    let applied = latest_applied_on(conn, owner, run)?;
    let base = if let Some(applied) = applied.as_ref().filter(|r| r.identity.wait == wait) {
        let predecessor = insert::prior(applied)?;
        DecisionPublicationBase {
            workspace: applied.identity.workspace,
            document: applied.identity.document,
            source_schema: 5u8.try_into().map_err(|_| BotLedgerError::Invalid)?,
            observed_created_at_ms: predecessor.observed_created_at_ms,
            timeline_len: predecessor.timeline_start,
            subject_digest: applied.identity.subject_digest,
            predecessor,
            bookkeeping: applied
                .target
                .resulting_bookkeeping()
                .map_err(|_| BotLedgerError::Invalid)?,
        }
    } else {
        let first = open.first().ok_or(BotLedgerError::Stale)?;
        DecisionPublicationBase {
            workspace: first.identity.workspace,
            document: first.identity.document,
            source_schema: first.identity.source_schema,
            observed_created_at_ms: first.target.predecessor().observed_created_at_ms,
            timeline_len: first.target.predecessor().timeline_start,
            subject_digest: first.identity.subject_digest,
            predecessor: first.target.predecessor().clone(),
            bookkeeping: match &first.target {
                RunProjectionTargetV1::Control {
                    retained_bookkeeping,
                    ..
                } => retained_bookkeeping.clone(),
                // No pre-event tally can be reconstructed from an initial
                // Source. The mandatory initial wait Control owns that base.
                RunProjectionTargetV1::Source { .. } => return Err(BotLedgerError::Fenced),
            },
        }
    };
    // Replacement excludes precisely the unprepared Control being superseded.
    if open.last().is_some_and(|r| {
        r.identity.kind == DecisionProjectionKind::Control
            && matches!(r.state, DecisionProjectionState::Pending)
    }) {
        open.pop();
    }
    let (predecessor, bookkeeping) = match open.last() {
        Some(last) => (
            insert::prior(last)?,
            last.target
                .resulting_bookkeeping()
                .map_err(|_| BotLedgerError::Invalid)?,
        ),
        None => (base.predecessor.clone(), base.bookkeeping.clone()),
    };
    Ok(ControlContext {
        base,
        predecessor,
        bookkeeping,
    })
}
