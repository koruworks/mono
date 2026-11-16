// SPDX-License-Identifier: MPL-2.0

use super::*;

pub(super) fn prior(
    record: &DecisionProjectionRecord,
) -> Result<DecisionProjectionPredecessor, BotLedgerError> {
    Ok(DecisionProjectionPredecessor {
        observed_created_at_ms: record.target.predecessor().observed_created_at_ms,
        timeline_start: record
            .target
            .timeline_end()
            .map_err(|_| BotLedgerError::Invalid)?,
        projection: Some(DecisionProjectionPrior {
            transition: record.identity.transition,
            sequence: record.identity.sequence,
            target_digest: record.identity.target_digest,
        }),
        status: record.target.effects().status.clone(),
        failure_reason: record.target.effects().failure_reason,
    })
}

fn base_on(
    tx: &Connection,
    owner: BotOwner,
    run: RunId,
    wait: BotWaitId,
    base: &DecisionPublicationBase,
) -> Result<(), BotLedgerError> {
    authority::owner(tx, owner, run)?;
    crate::bots::routines::bounded_admission(tx, run)?;
    let admitted = crate::bots::routines::native_admission(tx, run)?;
    require(matches!(
        admitted.state,
        BotAdmissionState::Reserved | BotAdmissionState::Executing
    ))?;
    let (intent, request) = crate::intents::retained_bounded(tx, run)?;
    require(
        intent.state == crate::IntentState::Satisfied
            && intent.workspace == base.workspace
            && intent.run_document == Some(base.document)
            && request == admitted.submission.request
            && admitted.submission.provenance.owner == owner,
    )?;
    require(
        base.predecessor.observed_created_at_ms == base.observed_created_at_ms
            && base.predecessor.timeline_start == base.timeline_len,
    )?;
    let mut header = RunHeader::from_request(request, base.observed_created_at_ms);
    header.bot = Some(admitted.submission.provenance.clone());
    require(
        decision_projection_subject_digest(&header).map_err(|_| BotLedgerError::Invalid)?
            == base.subject_digest,
    )?;
    if let Some(bookkeeping) = &base.bookkeeping {
        valid(bookkeeping)?;
        require(
            bookkeeping.boundary.fence.owner == owner
                && bookkeeping.boundary.fence.run == run
                && bookkeeping.boundary.fence.bot == admitted.submission.provenance.bot,
        )?;
    }
    if let Some(applied) = latest_applied_on(tx, owner, run)? {
        // After an applied projection the Run document is at Bot Run format 5
        // or, once it holds an M6 field, 6: whichever its owner observed.
        require(
            matches!(u8::from(base.source_schema), 5 | 6)
                && base.workspace == applied.identity.workspace
                && base.document == applied.identity.document
                && base.subject_digest == applied.identity.subject_digest
                && base.predecessor.projection == prior(&applied)?.projection,
        )?;
        if applied.identity.wait == wait {
            require(
                base.predecessor == prior(&applied)?
                    && base.bookkeeping
                        == applied
                            .target
                            .resulting_bookkeeping()
                            .map_err(|_| BotLedgerError::Invalid)?,
            )?;
        } else {
            // The sole document owner durably observes all ordinary writes
            // between ownership intervals. Do not equate that fresh canonical
            // state with the legitimately older historical marker's status.
            let old_wait = super::super::rows::by_wait(tx, run, applied.identity.wait)?
                .ok_or_else(|| corrupt("bot_decision_publications", "prior_wait"))?;
            require(old_wait.phase == BotWaitPhase::Settled && old_wait.settled_at_ms.is_some())?;
            require(
                prefix(tx, owner, run)?
                    .iter()
                    .all(|r| r.identity.wait == wait),
            )?;
        }
    } else {
        // The first projection names the format the Bot Run was created at:
        // Bot6 or Bot5 for a Bot Run of this build or the previous one, Manual
        // or Routine, or the
        // previous default's Bot3 (Manual) / Bot4 (Routine). Only a Bot Run
        // reaches here: a non-Bot Run has no admission and was refused above.
        let routine = admitted.submission.provenance.routine.is_some();
        require(
            base.predecessor.projection.is_none()
                && match u8::from(base.source_schema) {
                    5 | 6 => true,
                    4 => routine,
                    3 => !routine,
                    _ => false,
                },
        )?;
    }
    Ok(())
}

fn source_progress(
    before: Option<&DecisionSourceBookkeeping>,
    next: &DecisionSourceBookkeeping,
) -> Result<(), BotLedgerError> {
    if let Some(before) = before {
        if before.boundary.fence == next.boundary.fence {
            // A reconnect changes the physical pump epoch. The actor checks
            // that epoch before acceptance; it is not a durable permission to
            // reset either stream frontier. Retained replay still compares the
            // complete original target, including its original epoch.
            let (sequence, other, next_other) = match next.boundary.stream {
                DecisionSourceStream::Agent => (
                    before.boundary.resulting.agent_after,
                    before.boundary.resulting.session_after,
                    next.boundary.resulting.session_after,
                ),
                DecisionSourceStream::Session => (
                    before.boundary.resulting.session_after,
                    before.boundary.resulting.agent_after,
                    next.boundary.resulting.agent_after,
                ),
            };
            require(sequence.is_none_or(|s| next.boundary.sequence > s) && other == next_other)?;
        }
        // A changed fence is qualified against the sole retained acknowledged
        // resume below; it cannot be inferred from a new producer epoch alone.
    }
    Ok(())
}

/// Shared transaction insertion owner. Decision mutation wrappers call this
/// after constructing their exact resulting disposition in the same writer.
/// It accepts no callback, bypass flag or arbitrary opaque target body.
pub(super) fn insert_on(
    tx: &Transaction<'_>,
    owner: BotOwner,
    transition: DecisionTransitionId,
    wait_id: BotWaitId,
    base: &DecisionPublicationBase,
    target: &RunProjectionTargetV1,
    now: i64,
) -> Result<DecisionProjectionRecord, BotLedgerError> {
    valid(target)?;
    let run = target.fence().run;
    authority::owner(tx, owner, run)?;
    if let Some(old) = read(tx, owner, run, transition)? {
        require(
            old.identity.wait == wait_id
                && old.target == *target
                && old.identity.workspace == base.workspace
                && old.identity.document == base.document
                && old.identity.subject_digest == base.subject_digest,
        )?;
        return Ok(old);
    }
    base_on(tx, owner, run, wait_id, base)?;
    let wait = super::super::rows::by_wait(tx, run, wait_id)?.ok_or(BotLedgerError::Stale)?;
    require(
        wait.phase != BotWaitPhase::Settled
            && wait.origin.owner == owner
            && target.fence().owner == owner
            && target.fence().bot == wait.origin.bot,
    )?;
    if let RunProjectionTargetV1::Control {
        action:
            DecisionControlAction::CleanupRequested {
                expected_wait_revision,
                cause,
            },
        ..
    } = target
    {
        if wait.revision != next(*expected_wait_revision)? {
            let retained = super::cleanup_owner::retained_for_carry(tx, owner, run)?;
            require(
                retained
                    == Some(DecisionCleanupOrigin::Requested {
                        wait: wait.wait,
                        expected_wait_revision: *expected_wait_revision,
                        cause: *cause,
                    }),
            )?;
        }
    }
    // The retained consumer of the wait, whichever table owns its resume.
    let mut resume = None;
    let decision = match wait.reason {
        BotWaitReason::Child {} => {
            let child =
                super::super::rows::child::wait(tx, run, wait_id)?.ok_or(BotLedgerError::Stale)?;
            let child_resume = super::super::rows::child::resume(tx, &child)?;
            if target.fence() != &wait.origin {
                require(child_consumer_matches(tx, run, wait_id, target.fence())?)?;
            }
            if let RunProjectionTargetV1::Control {
                disposition,
                wait_phase,
                reason,
                ..
            } = target
            {
                require(
                    disposition.is_none() && *wait_phase == wait.phase && *reason == wait.reason,
                )?;
            }
            resume = child_resume
                .as_ref()
                .and_then(|resume| resume.consumer_identity());
            None
        }
        BotWaitReason::Decision { decision } => {
            let record =
                super::super::rows::read(tx, owner, run, decision)?.ok_or(BotLedgerError::Stale)?;
            resume = record
                .resume
                .as_ref()
                .map(DecisionResume::consumer_identity);
            if target.fence() != &wait.origin {
                require(
                    record
                        .resume
                        .as_ref()
                        .is_some_and(|r| consumer_matches(r, target.fence())),
                )?;
            }
            if let RunProjectionTargetV1::Control {
                disposition: Some(disposition),
                wait_phase,
                reason,
                ..
            } = target
            {
                let state = match disposition {
                    DecisionDisposition::Open { state }
                    | DecisionDisposition::Settled { state, .. } => state,
                };
                require(
                    state == &record.receipt.state
                        && *wait_phase == record.wait.phase
                        && *reason == record.wait.reason,
                )?;
                match (disposition, &record.receipt.closure) {
                    (DecisionDisposition::Open { .. }, None) => {}
                    (
                        DecisionDisposition::Settled {
                            reason,
                            outcome_digest,
                            ..
                        },
                        Some(closure),
                    ) => require(
                        *reason == closure.reason
                            && *outcome_digest
                                == decision_outcome_digest(&record.receipt)
                                    .map_err(|_| BotLedgerError::Invalid)?,
                    )?,
                    _ => return Err(BotLedgerError::Conflict),
                }
            }
            Some(DecisionProjectionRevision {
                decision,
                revision: record.receipt.state.revision,
            })
        }
        _ => {
            require(target.fence() == &wait.origin)?;
            None
        }
    };
    let latest = crate::attempts::latest(tx, run)?.ok_or(BotLedgerError::Fenced)?;
    if target.kind() == DecisionProjectionKind::Source {
        require(
            latest.attempt == target.fence().attempt
                && latest.generation == target.fence().generation
                && latest.provider_session.and_then(|s| s.session) == Some(target.fence().session),
        )?;
    } else if let Some(resume) = resume {
        // Reservation and unknown creation have no consumer turn to fabricate.
        // Their control retains the exact origin fence while the independently
        // recorded resume identifies the current physical cleanup subject.
        let (attempt, generation, session, key) = resume;
        require(
            latest.attempt == attempt
                && latest.generation == generation
                && latest
                    .provider_session
                    .is_some_and(|s| s.idempotency == key && s.session == session),
        )?;
    } else {
        require(
            latest.attempt == wait.origin.attempt
                && latest.generation == wait.origin.generation
                && latest.provider_session.and_then(|s| s.session) == Some(wait.origin.session),
        )?;
    }
    let mut open = prefix(tx, owner, run)?;
    require(open.iter().all(|r| r.identity.wait == wait_id))?;
    if target.kind() == DecisionProjectionKind::Source && !open.is_empty() {
        return Err(BotLedgerError::Capacity);
    }
    let superseded = if target.kind() == DecisionProjectionKind::Control
        && open.last().is_some_and(|r| {
            r.identity.kind == DecisionProjectionKind::Control
                && matches!(r.state, DecisionProjectionState::Pending)
        }) {
        open.pop()
    } else {
        None
    };
    let (expected, bookkeeping) = if let Some(last) = open.last() {
        (
            prior(last)?,
            last.target
                .resulting_bookkeeping()
                .map_err(|_| BotLedgerError::Invalid)?,
        )
    } else {
        (base.predecessor.clone(), base.bookkeeping.clone())
    };
    require(target.predecessor() == &expected)?;
    match target {
        RunProjectionTargetV1::Source {
            bookkeeping: next, ..
        } => source_progress(bookkeeping.as_ref(), next)?,
        RunProjectionTargetV1::Control {
            retained_bookkeeping,
            ..
        } => require(*retained_bookkeeping == bookkeeping)?,
    }
    if let Some(delivery) = target.delivery() {
        let admission = crate::bots::routines::native_admission(tx, run)?;
        require(
            admission.delivery.as_ref() == Some(&delivery.receipt)
                && admission.delivery_publication == Some(delivery.publication),
        )?;
    }
    let last = rows::latest(tx, run)?
        .map(|id| read(tx, owner, run, id)?.ok_or(BotLedgerError::Stale))
        .transpose()?;
    let sequence = last
        .as_ref()
        .map(|r| next(r.identity.sequence))
        .transpose()?
        .unwrap_or(DecisionRevision::new(1).map_err(|_| BotLedgerError::Invalid)?);
    let identity = DecisionProjectionIdentity {
        transition,
        run,
        wait: wait_id,
        decision,
        sequence,
        workspace: base.workspace,
        document: base.document,
        source_schema: if expected.projection.is_some() && u8::from(base.source_schema) != 6 {
            DecisionSourceSchema::try_from(5).map_err(|_| BotLedgerError::Invalid)?
        } else {
            base.source_schema
        },
        subject_digest: base.subject_digest,
        kind: target.kind(),
        target_digest: target.digest().map_err(|_| BotLedgerError::Invalid)?,
    };
    target
        .matches_identity(&identity)
        .map_err(|_| BotLedgerError::Invalid)?;
    let mut offset = base.timeline_len;
    for prior in &open {
        offset = DecisionProjectionMarker::for_target(&prior.identity, &prior.target, offset)
            .map_err(|_| BotLedgerError::Invalid)?
            .timeline_end;
    }
    DecisionProjectionMarker::for_target(&identity, target, offset)
        .map_err(|_| BotLedgerError::Invalid)?;
    let bytes = target
        .canonical_bytes()
        .map_err(|_| BotLedgerError::Invalid)?;
    let body = std::str::from_utf8(bytes.as_slice()).map_err(|_| BotLedgerError::Invalid)?;
    if let Some(old) = superseded {
        tx.execute("UPDATE bot_decision_publications SET state='superseded',superseded_by_sequence=?2,completed_at_ms=?3
            WHERE transition_id=?1 AND state='pending' AND projection_kind='control'",
            params![old.identity.transition.to_storage(),sequence.get(),now.max(old.created_at_ms)]).map_err(sql)?;
    }
    tx.execute("INSERT INTO bot_decision_publications(transition_id,run_id,wait_id,decision_id,run_sequence,decision_revision,workspace_id,document_id,source_schema,subject_digest,projection_kind,target_json,target_digest,state,created_at_ms)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'pending',?14)",
        params![transition.to_storage(),run.to_storage(),wait_id.to_storage(),identity.decision.as_ref().map(|r|r.decision.to_storage()),
            sequence.get(),identity.decision.as_ref().map(|r|r.revision.get()),base.workspace.to_storage(),base.document.to_storage(),
            u8::from(identity.source_schema),base.subject_digest.as_bytes(),token(identity.kind)?,body,identity.target_digest.as_bytes(),now]).map_err(sql)?;
    rows::open(tx, run)?;
    write::required(tx, owner, &identity)
}

impl RunsDb {
    pub fn record_decision_source(
        &mut self,
        owner: BotOwner,
        transition: DecisionTransitionId,
        wait: BotWaitId,
        base: &DecisionPublicationBase,
        target: &RunProjectionTargetV1,
    ) -> Result<DecisionProjectionRecord, BotLedgerError> {
        require(target.kind() == DecisionProjectionKind::Source)?;
        self.write_with_error(crate::WriteOp::RecordDecisionProjection, |tx, now| {
            if read(tx, owner, target.fence().run, transition)?.is_some() {
                let retained = insert_on(tx, owner, transition, wait, base, target, now)?;
                super::cleanup_owner::replay_pair(tx, owner, &retained)?;
                return Ok(retained);
            }
            let cleanup = super::cleanup_owner::current(tx, owner, target.fence().run)?;
            let source = insert_on(tx, owner, transition, wait, base, target, now)?;
            if let Some(cleanup) = cleanup {
                require(cleanup.expected.wait == wait)?;
                super::cleanup_owner::carry(tx, owner, &source, &cleanup.origin, now)?;
            }
            Ok(source)
        })
    }
}
