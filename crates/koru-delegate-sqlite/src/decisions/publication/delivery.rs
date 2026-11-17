// SPDX-License-Identifier: MPL-2.0

//! Receipt-qualified delivery while the Run handle is held exclusively. The
//! SQL delivery receipt (with its publication EventId) and the owed header
//! publication commit in one writer; neither is evidence of the other's apply.
use super::*;

/// Exact retry of a retained Delivery Control: the same wait, receipt,
/// outcome and allocated step. Today's state and policy are not consulted.
fn replay(
    conn: &Connection,
    retained: &DecisionProjectionRecord,
    receipt: &BotDeliveryReceipt,
    expected: &DecisionWaitExpected,
    outcome: DecisionDeliveryOutcome,
    step: TimelineEntryId,
) -> Result<EventId, BotLedgerError> {
    let RunProjectionTargetV1::Control {
        action: DecisionControlAction::Delivery { outcome: stored },
        delivery: Some(delivery),
        effects,
        ..
    } = &retained.target
    else {
        return Err(BotLedgerError::Conflict);
    };
    require(
        retained.identity.wait == expected.wait
            && *stored == outcome
            && delivery.receipt == *receipt
            && effects.steps.as_slice().iter().all(|s| s.id == step),
    )?;
    let admission = crate::bots::routines::native_admission(conn, receipt.run)?;
    require(
        admission.delivery.as_ref() == Some(receipt)
            && admission.delivery_publication == Some(delivery.publication),
    )?;
    Ok(delivery.publication)
}

/// The Control fence for this receipt: the wait origin, or the recorded
/// successor whose continuation input was acknowledged. Nothing else qualifies.
fn fence(
    receipt: &BotDeliveryReceipt,
    wait: &DecisionWaitRecord,
    resume: Option<&DecisionResume>,
) -> Result<DecisionFence, BotLedgerError> {
    if receipt.attempt == wait.origin.attempt && receipt.generation == wait.origin.generation {
        require(resume.is_none())?;
        return Ok(wait.origin);
    }
    let resume = resume.ok_or(BotLedgerError::Fenced)?;
    let (attempt, generation, session, _) = resume.consumer_identity();
    let turn = match &resume.state {
        DecisionResumeState::InputAcknowledged { acknowledgment }
        | DecisionResumeState::Closed {
            acknowledgment: Some(acknowledgment),
        } => acknowledgment.turn,
        _ => return Err(BotLedgerError::Fenced),
    };
    require(receipt.attempt == attempt && receipt.generation == generation)?;
    Ok(DecisionFence {
        attempt,
        generation,
        session: session.ok_or(BotLedgerError::Fenced)?,
        turn,
        ..wait.origin
    })
}

/// The Control fence of a receipt during a child wait: the origin while no
/// successor is recorded, or the successor whose input was acknowledged.
fn child_fence(
    receipt: &BotDeliveryReceipt,
    wait: &crate::ChildWaitRecord,
    resume: Option<&koru_delegate_core::children::ChildResume>,
) -> Result<DecisionFence, BotLedgerError> {
    if receipt.attempt == wait.origin.attempt && receipt.generation == wait.origin.generation {
        // M6 piece R (O127): a successor that was reserved but never
        // acknowledged does not take the origin's answer away; its resume is
        // being cleaned up. An acknowledged successor does.
        require(resume.is_none_or(|resume| resume.accepted_turn().is_none()))?;
        return Ok(wait.origin);
    }
    let resume = resume.ok_or(BotLedgerError::Fenced)?;
    let (attempt, generation, session, _) =
        resume.consumer_identity().ok_or(BotLedgerError::Fenced)?;
    require(receipt.attempt == attempt && receipt.generation == generation)?;
    Ok(DecisionFence {
        attempt,
        generation,
        session: session.ok_or(BotLedgerError::Fenced)?,
        turn: resume.accepted_turn().ok_or(BotLedgerError::Fenced)?,
        ..wait.origin
    })
}

#[allow(clippy::too_many_arguments)]
fn record_on(
    tx: &Transaction<'_>,
    receipt: &BotDeliveryReceipt,
    expected: &DecisionWaitExpected,
    transition: DecisionTransitionId,
    outcome: DecisionDeliveryOutcome,
    step: TimelineEntryId,
    artifacts: u32,
    at_ms: i64,
    now: i64,
) -> Result<(EventId, DecisionProjectionRecord), BotLedgerError> {
    if !receipt.valid() || receipt.run != expected.run {
        return Err(BotLedgerError::Invalid);
    }
    authority::owner(tx, expected.owner, expected.run)?;
    if let Some(retained) = read(tx, expected.owner, expected.run, transition)? {
        let event = replay(tx, &retained, receipt, expected, outcome, step)?;
        return Ok((event, retained));
    }
    let current = super::super::observation::execution(tx, expected.owner, expected.run)?
        .ok_or(BotLedgerError::Stale)?;
    require(current.expected == *expected)?;
    // Completion needs a settled decision and no owed cleanup; a retained-only
    // or unknown-work receipt is history and may be recorded in any open wait.
    let child = match current.wait.reason {
        BotWaitReason::Child {} => {
            let child = super::super::rows::child::wait(tx, expected.run, expected.wait)?
                .ok_or(BotLedgerError::Stale)?;
            let resume = super::super::rows::child::resume(tx, &child)?;
            Some((child, resume))
        }
        _ => None,
    };
    if outcome == DecisionDeliveryOutcome::Completed {
        require(match &child {
            // M6: a child wait completes only through its acknowledged
            // successor, while nothing of the wait is being cleaned up.
            Some((record, resume))
                if receipt.attempt == record.origin.attempt
                    && receipt.generation == record.origin.generation =>
            {
                // M6 piece R (O127): the origin's own turn completed
                // naturally before any successor was acknowledged. It is an
                // ordinary completion and stands as the Run's one answer;
                // the requests settle below and the wait is cleaned up.
                resume
                    .as_ref()
                    .is_none_or(|resume| resume.accepted_turn().is_none())
            }
            Some((_, resume)) => {
                current.wait.phase == BotWaitPhase::ResumeReserved
                    && current.wait.cleanup.is_none()
                    && resume
                        .as_ref()
                        .is_some_and(|resume| resume.accepted_turn().is_some())
            }
            None => {
                current
                    .decision_state
                    .as_ref()
                    .is_some_and(|s| s.phase == DecisionPhase::Settled)
                    && current.wait.phase != BotWaitPhase::CleanupPending
            }
        })?;
    }
    let fence = match &child {
        Some((record, resume)) => child_fence(receipt, record, resume.as_ref())?,
        None => fence(receipt, &current.wait, current.resume.as_ref())?,
    };
    if let Some((record, _)) = &child {
        // The origin's own turn was delivered while it waited: the parent's
        // turn stands as its answer and no request may stay open. The phase
        // is moved by the cleanup request that follows, never here.
        if receipt.attempt == record.origin.attempt
            && receipt.generation == record.origin.generation
        {
            crate::children::settle_parent_stopped_on(tx, expected.wait, now)?;
        }
    }
    // The same successor-aware receipt owner as the non-exclusive path.
    let event = crate::bots::delivery_on(tx, receipt, now)?;
    let context = control::context(tx, expected.owner, expected.run, expected.wait)?;
    let before = &context.predecessor.status;
    let (event_kind, steps, publications) = if before.is_terminal() {
        // An absorbing terminal status keeps its cause; the receipt is history.
        (None, Vec::new(), Vec::new())
    } else {
        let attempt_ref = || {
            TimelineRefs::new([TimelineRef::Attempt {
                attempt: receipt.attempt,
            }])
            .map_err(|_| BotLedgerError::Invalid)
        };
        match outcome {
            DecisionDeliveryOutcome::Retained => (None, Vec::new(), Vec::new()),
            DecisionDeliveryOutcome::Completed => (
                Some(RunEvent::BotResponseDelivered),
                vec![TimelineEntry {
                    id: step,
                    kind: StepKind::Completed,
                    refs: attempt_ref()?,
                    at_ms,
                }],
                vec![DecisionProjectionPublication {
                    event,
                    step: Some(step),
                    fact: RunFact::Completed {
                        run: receipt.run,
                        attempt: receipt.attempt,
                        generation: receipt.generation,
                        artifacts,
                    },
                }],
            ),
            DecisionDeliveryOutcome::InterruptedUnknown => (
                Some(RunEvent::Interrupt {
                    prior_attempt: PriorAttempt::Unknown,
                }),
                vec![TimelineEntry {
                    id: step,
                    kind: StepKind::Interrupted {
                        prior_attempt: PriorAttempt::Unknown,
                    },
                    refs: attempt_ref()?,
                    at_ms,
                }],
                Vec::new(),
            ),
        }
    };
    let status = match &event_kind {
        Some(event) => transition_bot(before, event).map_err(|_| BotLedgerError::Fenced)?,
        None => before.clone(),
    };
    let disposition = match current.wait.reason {
        BotWaitReason::Decision { decision } => {
            let record = super::super::rows::read(tx, expected.owner, expected.run, decision)?
                .ok_or(BotLedgerError::Stale)?;
            Some(control::disposition(&record.receipt)?)
        }
        _ => None,
    };
    let target = RunProjectionTargetV1::Control {
        version: DecisionVersionV1,
        predecessor: context.predecessor.clone(),
        effects: DecisionProjectionEffects {
            event: event_kind,
            status,
            failure_reason: context.predecessor.failure_reason,
            economics_at_ms: None,
            steps: DecisionList::new(steps).map_err(|_| BotLedgerError::Invalid)?,
            models: DecisionList::default(),
            publications: DecisionList::new(publications).map_err(|_| BotLedgerError::Invalid)?,
        },
        fence,
        disposition,
        wait_phase: current.wait.phase,
        reason: current.wait.reason,
        action: DecisionControlAction::Delivery { outcome },
        retained_bookkeeping: context.bookkeeping.clone(),
        flush: DecisionList::default(),
        delivery: Some(DecisionProjectionDelivery {
            receipt: receipt.clone(),
            publication: event,
        }),
    };
    let record = insert::insert_on(
        tx,
        expected.owner,
        transition,
        expected.wait,
        &context.base,
        &target,
        now,
    )?;
    Ok((event, record))
}

impl RunsDb {
    /// Record a delivery receipt and its owed header publication atomically
    /// while the Run handle is held exclusively. Exact retained replay first.
    /// The receipt keeps `record_bot_delivery`'s successor-aware owner; the
    /// Control is the publication still owed, not proof it applied.
    #[allow(clippy::too_many_arguments)]
    pub fn record_decision_delivery(
        &mut self,
        receipt: &BotDeliveryReceipt,
        expected: &DecisionWaitExpected,
        transition: DecisionTransitionId,
        outcome: DecisionDeliveryOutcome,
        step: TimelineEntryId,
        artifacts: u32,
        at_ms: i64,
    ) -> Result<(EventId, DecisionProjectionRecord), BotLedgerError> {
        self.write_with_error(crate::WriteOp::RecordDecisionProjection, |tx, now| {
            record_on(
                tx, receipt, expected, transition, outcome, step, artifacts, at_ms, now,
            )
        })
    }
}
