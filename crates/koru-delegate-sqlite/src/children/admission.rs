// SPDX-License-Identifier: MPL-2.0

//! One ask, in one write transaction. Replay comes before every limit. A
//! refusal leaves the transaction as an error, so nothing it read or wrote
//! is committed; the public method then reports it as an ordinary answer.
use super::*;
use crate::{BOT_ACTIVE_BUDGET_MS, BOT_PENDING_LIMIT, LedgerError, RunsDb, WriteOp};
use koru_delegate_core::children::{
    BOT_CHILD_RESUME_FLOOR_MS, BotChildWaitCleanupIntentV1, CHILD_REQUEST_LIMIT, ChildPolicyRef,
    ChildRefusal, ChildRefused, ChildRequest, ChildRequestOutcome, ChildRequested, ChildWaitView,
};
use koru_delegate_core::decisions::{
    BotWaitPhase, DECISION_EVIDENCE_BYTES, DecisionControlAction, DecisionPublicationBase,
    DecisionTransitionId, DecisionValidate, encode_decision_value,
};
use koru_delegate_core::{
    ActorRef, BotAdmissionState, BotSubmissionKey, DelegateBootId,
    INTERACTIVE_BOT_SUBMISSION_VERSION, ROUTINE_BOT_SUBMISSION_VERSION,
};
use rusqlite::{Transaction, params};

/// Why the transaction body stopped without committing.
enum Stop {
    Refused(ChildRefused),
    Ledger(BotLedgerError),
}
impl From<LedgerError> for Stop {
    fn from(error: LedgerError) -> Self {
        Self::Ledger(error.into())
    }
}
impl From<BotLedgerError> for Stop {
    fn from(error: BotLedgerError) -> Self {
        Self::Ledger(error)
    }
}
fn refuse<T>(code: ChildRefusal, ordinal: Option<usize>) -> Result<T, Stop> {
    Err(Stop::Refused(ChildRefused {
        code,
        ordinal: ordinal.and_then(|index| u8::try_from(index).ok()),
    }))
}

/// The current state of an ask that is already retained.
pub(super) fn requested_on(
    conn: &rusqlite::Connection,
    wait: &ChildWaitRecord,
    rows: Vec<rows::RequestRow>,
) -> Result<ChildRequested, BotLedgerError> {
    let open = rows
        .iter()
        .filter(|row| row.receipt.state == koru_delegate_core::children::ChildRequestState::Open)
        .count();
    let _ = conn;
    Ok(ChildRequested {
        wait: ChildWaitView {
            wait: wait.wait,
            phase: wait.phase,
            revision: wait.revision,
            open_requests: u8::try_from(open)
                .map_err(|_| corrupt("bot_child_requests", "limit"))?,
        },
        enrollment: wait.intent.clone(),
        receipts: rows.into_iter().map(|row| row.receipt).collect(),
    })
}

fn body(
    tx: &Transaction<'_>,
    request: &ChildRequest,
    payloads: &[(String, String)],
    boot: DelegateBootId,
    base: &DecisionPublicationBase,
    now: i64,
) -> Result<ChildRequestOutcome, Stop> {
    let parent = &request.parent;
    let origin = request.origin().ok_or(BotLedgerError::Invalid)?;
    let wait_id = request.wait().ok_or(BotLedgerError::Invalid)?;
    crate::decisions::owner_on(tx, parent.owner, parent.run)?;

    // 6. Replay, before every limit and every freshness check.
    let retained = rows::by_ask(tx, parent.run, request.ask_key, now)?;
    if !retained.is_empty() {
        let same = retained.len() == request.requests.len()
            && retained.iter().zip(&request.requests).all(|(row, item)| {
                row.receipt.child_request == item.child_request
                    && row.receipt.wait == wait_id
                    && row.receipt.recipient == item.binding.recipient_bot
                    && row.receipt.child_run == item.binding.child_run
                    && item
                        .binding
                        .digest()
                        .is_ok_and(|digest| digest == row.receipt.binding_digest)
            });
        if !same {
            return Err(BotLedgerError::Conflict.into());
        }
        let wait = crate::decisions::child_wait_on(tx, parent.run, wait_id)?
            .ok_or_else(|| corrupt("bot_child_requests", "wait_id"))?;
        return Ok(ChildRequestOutcome::AlreadyCreated(requested_on(
            tx, &wait, retained,
        )?));
    }
    for item in &request.requests {
        if rows::by_id(tx, item.child_request, now)?.is_some() {
            return Err(BotLedgerError::Conflict.into());
        }
    }

    // 7. The parent: its retained subject, its live fence, its budget, and no
    //    effect of its own still in flight.
    crate::bots::routines::bounded_admission(tx, parent.run)?;
    let admitted = crate::bots::routines::native_admission(tx, parent.run)?;
    let subject = &admitted.submission;
    let first = &request.requests[0].binding;
    let routine_matches = match (admitted.submission_version, &subject.provenance.routine) {
        (1, None) => {
            crate::bots::routines::deny_run_metadata(tx, parent.run)?;
            first.parent_submission == INTERACTIVE_BOT_SUBMISSION_VERSION
                && first.parent_routine.is_none()
        }
        (2, Some(routine)) => {
            crate::bots::routines::validate_admission(tx, &admitted)?;
            first.parent_submission == ROUTINE_BOT_SUBMISSION_VERSION
                && first.parent_routine.as_ref() == Some(routine)
        }
        _ => return Err(BotLedgerError::Fenced.into()),
    };
    let parent_policy = ChildPolicyRef::of(
        subject.provenance.config_revision,
        subject.provenance.policy_revision,
        &subject.policy,
    )
    .map_err(|_| BotLedgerError::Invalid)?;
    if subject.provenance.owner != parent.owner
        || subject.provenance.bot != parent.bot
        || !routine_matches
        || first.parent_policy != parent_policy
        || request
            .requests
            .iter()
            .any(|item| item.binding.workspace != subject.workspace)
        || subject.request.requested_by
            != (ActorRef::Person {
                profile: request.human,
            })
    {
        return Err(BotLedgerError::Invalid.into());
    }
    let latest = crate::attempts::latest(tx, parent.run)?.ok_or(BotLedgerError::Fenced)?;
    if admitted.state != BotAdmissionState::Executing
        || latest.attempt != origin.attempt
        || latest.generation != origin.generation
        || latest.retired
        || latest.phase != crate::AttemptPhase::Running
        || latest.provider_session.and_then(|key| key.session) != Some(origin.session)
        || !crate::decisions::acknowledged_turn_on(tx, &admitted, &latest, origin.turn)?
    {
        // The consumer of an unsettled decision is not yet an acknowledged
        // owner of fresh work. That is the open decision's refusal (A5), not
        // a stale fence.
        if crate::decisions::open_wait_reason_on(tx, parent.run)?
            == Some(crate::decisions::OpenWaitKind::Decision)
        {
            return refuse(ChildRefusal::DecisionOpen, None);
        }
        return Err(BotLedgerError::Fenced.into());
    }

    if admitted.active_elapsed_at(now) >= BOT_ACTIVE_BUDGET_MS - BOT_CHILD_RESUME_FLOOR_MS {
        return refuse(ChildRefusal::BudgetLow, None);
    }
    let effect_open: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM bot_private_effects WHERE run_id=?1 AND stage IN ('prepared','dispatched'))",
            [parent.run.to_storage()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if effect_open || crate::native_results::held_for_bot(tx, parent.owner, parent.bot)? {
        return refuse(ChildRefusal::Capacity, None);
    }

    // 8. One wait per Run.
    if let Some(open) = crate::decisions::open_wait_reason_on(tx, parent.run)? {
        return match open {
            crate::decisions::OpenWaitKind::Decision => refuse(ChildRefusal::DecisionOpen, None),
            crate::decisions::OpenWaitKind::Cleanup => refuse(ChildRefusal::Capacity, None),
            crate::decisions::OpenWaitKind::Child => {
                let wait = crate::decisions::open_child_wait_on(tx, parent.run)?
                    .ok_or_else(|| corrupt("bot_wait_runtime", "reason"))?;
                let acknowledged = crate::decisions::child_resume_on(tx, &wait)?
                    .is_some_and(|resume| resume.accepted_turn().is_some());
                // Only a resumed wait that still owes its last publication
                // is worth a retry; a wait being cleaned up is not.
                refuse(
                    if acknowledged && wait.phase == BotWaitPhase::ResumeReserved {
                        ChildRefusal::WaitClosing
                    } else {
                        ChildRefusal::WaitOpen
                    },
                    None,
                )
            }
        };
    }
    // 9. Depth one.
    if rows::by_child(tx, parent.run, now)?.is_some() || subject.provenance.parent.is_some() {
        return refuse(ChildRefusal::Depth, None);
    }
    // 11. Three requests per parent Run, for good.
    let before = rows::by_parent(tx, parent.run, now)?.len();
    if before + request.requests.len() > CHILD_REQUEST_LIMIT {
        return refuse(ChildRefusal::RecipientLimit, None);
    }
    // 12. No recipient may already wait, directly or not, on the asking Bot.
    let recipients: Vec<_> = request
        .requests
        .iter()
        .map(|item| item.binding.recipient_bot)
        .collect();
    if cycle::walk(tx, parent.owner.device, parent.bot, &recipients)?.closes {
        let ordinal = recipients.iter().position(|recipient| {
            cycle::walk(tx, parent.owner.device, parent.bot, &[*recipient])
                .is_ok_and(|walk| walk.closes)
        });
        return refuse(ChildRefusal::Cycle, ordinal);
    }
    // 13. The open-wait cap, counted exactly as a decision's preparation does.
    match crate::decisions::wait_cap_on(tx, parent.owner) {
        Ok(()) => {}
        Err(BotLedgerError::Capacity) => return refuse(ChildRefusal::Capacity, None),
        Err(error) => return Err(error.into()),
    }
    // 14. The child holds no connection grant its parent Run lacks.
    for (index, item) in request.requests.iter().enumerate() {
        if !item
            .submission
            .policy
            .connections
            .is_subset(&subject.policy.connections)
        {
            return refuse(ChildRefusal::ConnectionsNotSubset, Some(index));
        }
    }
    // 15 and 16. Each recipient's barrier and queue, as `admit_bot` reads them.
    for (index, item) in request.requests.iter().enumerate() {
        let p = &item.submission.provenance;
        if let Some(barrier) = crate::bots::barrier_on(tx, p.owner, p.bot)? {
            if barrier.paused {
                return refuse(ChildRefusal::RecipientPaused, Some(index));
            }
            if p.config_revision < barrier.epoch {
                return refuse(ChildRefusal::RecipientStale, Some(index));
            }
        }
        if crate::bots::queued_on(tx, p.owner, p.bot)? >= BOT_PENDING_LIMIT as i64 {
            return refuse(ChildRefusal::RecipientFull, Some(index));
        }
        let key = BotSubmissionKey {
            owner: p.owner,
            bot: p.bot,
            request: p.submission,
        };
        if crate::bots::reconcile_on(tx, &key)?.is_some()
            || crate::bots::intent_exists_on(tx, item.submission.request.run)?
        {
            return Err(BotLedgerError::Conflict.into());
        }
    }

    // 17. Everything, or nothing.
    for (item, (payload, ordinary)) in request.requests.iter().zip(payloads) {
        crate::bots::insert_admission_on(tx, &item.submission, payload, ordinary, now)?;
    }
    let intent = BotChildWaitCleanupIntentV1::frozen(wait_id, &origin)
        .map_err(|_| BotLedgerError::Invalid)?;
    let frozen = String::from_utf8(
        encode_decision_value(&intent, DECISION_EVIDENCE_BYTES)
            .map_err(|_| BotLedgerError::Invalid)?,
    )
    .map_err(|_| BotLedgerError::Invalid)?;
    tx.execute(
        "INSERT INTO bot_wait_runtime(wait_id,run_id,user_scope,device_id,bot_id,attempt_id,generation,session_id,turn_id,reason,phase,revision,ai_enrollment_request_json,created_at_ms,updated_at_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'child','hold_requested',1,?10,?11,?11)",
        params![
            wait_id.to_storage(),
            origin.run.to_storage(),
            origin.owner.user_scope.to_storage(),
            origin.owner.device.to_storage(),
            origin.bot.to_storage(),
            origin.attempt.to_storage(),
            origin.generation.get(),
            origin.session.to_string(),
            origin.turn.to_string(),
            frozen,
            now
        ],
    )
    .map_err(sql)?;
    for item in &request.requests {
        let binding = &item.binding;
        let body = String::from_utf8(
            binding
                .canonical_bytes()
                .map_err(|_| BotLedgerError::Invalid)?,
        )
        .map_err(|_| BotLedgerError::Invalid)?;
        let digest = binding.digest().map_err(|_| BotLedgerError::Invalid)?;
        tx.execute(
            "INSERT INTO bot_child_requests(child_request_id,parent_run_id,wait_id,ask_key,ordinal,user_scope,device_id,parent_bot_id,recipient_bot_id,human_profile_id,child_run_id,binding_version,binding_digest,binding_json,state,wait_elapsed_ms,clock,clock_boot_id,clock_since_ms,revision,created_at_ms,updated_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,1,?12,?13,'open',0,'running',?14,?15,1,?15,?15)",
            params![
                binding.child_request.to_storage(),
                binding.parent_run.to_storage(),
                binding.wait.to_storage(),
                binding.ask_key.hyphenated().to_string(),
                binding.ordinal,
                binding.owner.user_scope.to_storage(),
                binding.owner.device.to_storage(),
                binding.parent_bot.to_storage(),
                binding.recipient_bot.to_storage(),
                binding.human.to_storage(),
                binding.child_run.to_storage(),
                digest.as_bytes().as_slice(),
                body,
                boot.to_storage(),
                now
            ],
        )
        .map_err(sql)?;
    }
    let wait = crate::decisions::child_wait_on(tx, parent.run, wait_id)?
        .ok_or_else(|| corrupt("bot_wait_runtime", "wait_id"))?;
    if wait.phase != BotWaitPhase::HoldRequested || wait.intent != intent {
        return Err(corrupt("bot_wait_runtime", "enrollment_request").into());
    }
    crate::decisions::commit_child_control_on(
        tx,
        &wait,
        DecisionTransitionId::mint(),
        DecisionControlAction::ChildWaiting {},
        Some(base),
        now,
    )?;
    let rows = rows::by_wait(tx, wait_id, now)?;
    Ok(ChildRequestOutcome::Created(requested_on(tx, &wait, rows)?))
}

impl RunsDb {
    /// `RequestChild`: admit one to three child Runs, open the parent's wait
    /// and owe the waiting Control, or do nothing at all.
    pub fn request_child(
        &mut self,
        request: &ChildRequest,
        boot: DelegateBootId,
        base: &DecisionPublicationBase,
    ) -> Result<ChildRequestOutcome, BotLedgerError> {
        // Self-ask has a code of its own, so it is named before the shape
        // check that would otherwise report it as a malformed binding.
        if let Some(index) = request
            .requests
            .iter()
            .position(|item| item.binding.recipient_bot == request.parent.bot)
        {
            return Ok(ChildRequestOutcome::Refused(ChildRefused {
                code: ChildRefusal::SelfAsk,
                ordinal: u8::try_from(index).ok(),
            }));
        }
        request.validate().map_err(|_| BotLedgerError::Invalid)?;
        let mut payloads = Vec::with_capacity(request.requests.len());
        for item in &request.requests {
            let payload =
                serde_json::to_string(&item.submission).map_err(|_| BotLedgerError::Invalid)?;
            if payload.len() > 4 * 1024 * 1024 {
                return Err(BotLedgerError::Invalid);
            }
            let ordinary = serde_json::to_string(&item.submission.request)
                .map_err(|_| BotLedgerError::Invalid)?;
            payloads.push((payload, ordinary));
        }
        match self.write_with_error(WriteOp::RequestChild, |tx, now| {
            body(tx, request, &payloads, boot, base, now)
        }) {
            Ok(outcome) => Ok(outcome),
            Err(Stop::Refused(refused)) => Ok(ChildRequestOutcome::Refused(refused)),
            Err(Stop::Ledger(error)) => Err(error),
        }
    }
}
