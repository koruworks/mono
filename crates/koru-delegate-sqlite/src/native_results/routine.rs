// SPDX-License-Identifier: MPL-2.0

//! Independent Routine evidence on the existing native-effect transaction.
use super::*;
use crate::{BotAdmissionRecord, RoutinePolicyRecord, bots::routines};
use rusqlite::{Connection, OptionalExtension, params};

fn corrupt(column: &'static str) -> BotLedgerError {
    crate::LedgerError::Corrupt {
        table: "native_routine_leases",
        column,
    }
    .into()
}

pub(super) fn admission(
    conn: &Connection,
    run: RunId,
) -> Result<BotAdmissionRecord, BotLedgerError> {
    routines::native_admission(conn, run)
}

/// Subject comes only from the stored, explicitly versioned admission.
pub(super) fn subject(
    conn: &Connection,
    admitted: &BotAdmissionRecord,
) -> Result<Option<ScheduleId>, BotLedgerError> {
    match (
        admitted.submission_version,
        admitted.submission.provenance.routine.as_ref(),
    ) {
        (1, None) => Ok(None),
        (2, Some(r)) => {
            routines::validate_admission(conn, admitted)?;
            Ok(Some(r.schedule))
        }
        _ => Err(corrupt("submission_subject")),
    }
}

pub(super) fn linked(
    conn: &Connection,
    admitted: &BotAdmissionRecord,
) -> Result<(), BotLedgerError> {
    match (
        admitted.submission_version,
        admitted.submission.provenance.routine.as_ref(),
    ) {
        (1, None) => Ok(()),
        (2, Some(_))
            if matches!(
                routines::validate_admission(conn, admitted)?.link_state,
                RoutineLinkState::Linked {}
            ) =>
        {
            Ok(())
        }
        _ => Err(BotLedgerError::Fenced),
    }
}

pub(super) fn current(
    conn: &Connection,
    admitted: &BotAdmissionRecord,
    proof: &FreshBotPolicyProof,
) -> Result<Option<RoutinePolicyRecord>, BotLedgerError> {
    if !proof.effects_allowed_for(&admitted.submission) {
        return Err(BotLedgerError::Fenced);
    }
    let Some(offered) = &proof.routine else {
        return Ok(None);
    };
    let stored = routines::current_policy(conn, offered.owner, offered.bot, offered.schedule)?
        .ok_or(BotLedgerError::Fenced)?;
    if stored.request != offered.request
        || stored.grant_revision != offered.grant_revision
        || stored.payload_digest != offered.digest().map_err(|_| BotLedgerError::Invalid)?
        || stored.state != BotPolicyBarrierState::Effective
        || !stored.effects_allowed
    {
        return Err(BotLedgerError::Fenced);
    }
    Ok(Some(stored))
}

pub(super) fn insert(
    conn: &Connection,
    key: &NativeResultKey,
    policy: &RoutinePolicyRecord,
) -> Result<(), BotLedgerError> {
    conn.execute("INSERT INTO native_routine_leases(run_id,operation_id,user_scope,device_id,bot_id,schedule_id,barrier_request_id,grant_revision,payload_digest) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![key.run.to_storage(),key.operation_id.to_string(),policy.owner.user_scope.to_storage(),policy.owner.device.to_storage(),policy.bot.to_storage(),policy.schedule.to_storage(),policy.request.to_storage(),policy.grant_revision,policy.payload_digest.as_bytes().as_slice()]).map_err(rows::sql)?;
    Ok(())
}

/// Every retained read checks historical proof, without asking a newer policy
/// to reauthorize an already acquired effect. Terminal settled history is valid.
pub(super) fn validate_record(
    conn: &Connection,
    record: &mut NativeIntentRecord,
    admitted: &BotAdmissionRecord,
) -> Result<(), BotLedgerError> {
    let schedule = subject(conn, admitted)?;
    let p = &admitted.submission.provenance;
    let found = conn.query_row("SELECT
        CASE WHEN typeof(user_scope)='text' AND length(CAST(user_scope AS BLOB))=36 THEN user_scope END,
        CASE WHEN typeof(device_id)='text' AND length(CAST(device_id AS BLOB))=36 THEN device_id END,
        CASE WHEN typeof(bot_id)='text' AND length(CAST(bot_id AS BLOB))=36 THEN bot_id END,
        CASE WHEN typeof(schedule_id)='text' AND length(CAST(schedule_id AS BLOB))=36 THEN schedule_id END,
        CASE WHEN typeof(barrier_request_id)='text' AND length(CAST(barrier_request_id AS BLOB))=36 THEN barrier_request_id END,
        grant_revision, CASE WHEN typeof(payload_digest)='blob' AND length(payload_digest)=32 THEN payload_digest END
        FROM native_routine_leases WHERE run_id=?1 AND operation_id=?2",
        params![record.key.run.to_storage(),record.key.operation_id.to_string()], |r| {
            Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,u64>(5)?,r.get::<_,Vec<u8>>(6)?))
        }).optional().map_err(rows::sql)?;
    let Some(lease) = record.lease.as_mut() else {
        return if found.is_none() {
            Ok(())
        } else {
            Err(corrupt("unleased_companion"))
        };
    };
    if lease.policy.key.owner != p.owner || lease.policy.key.bot != p.bot {
        return Err(corrupt("lease_owner"));
    }
    let Some(schedule) = schedule else {
        return if found.is_none() {
            Ok(())
        } else {
            Err(corrupt("interactive_companion"))
        };
    };
    let (user, device, bot, stored_schedule, request, grant, digest) =
        found.ok_or_else(|| corrupt("missing_companion"))?;
    if user != p.owner.user_scope.to_storage()
        || device != p.owner.device.to_storage()
        || bot != p.bot.to_storage()
        || stored_schedule != schedule.to_storage()
        || grant
            < p.routine
                .as_ref()
                .ok_or_else(|| corrupt("subject"))?
                .grant_revision
    {
        return Err(corrupt("companion_subject"));
    }
    let request = request.parse().map_err(|_| corrupt("barrier_request"))?;
    let stored = routines::read_policy(conn, p.owner, p.bot, schedule, Some(request))?
        .ok_or_else(|| corrupt("companion_policy"))?;
    if stored.grant_revision != grant
        || stored.payload_digest.as_bytes().as_slice() != digest.as_slice()
        || stored.state != BotPolicyBarrierState::Effective
        || !stored.effects_allowed
    {
        return Err(corrupt("companion_policy"));
    }
    if record.lease_state == NativeLeaseState::Held
        && matches!(
            admitted.state,
            crate::BotAdmissionState::Settled | crate::BotAdmissionState::Refused
        )
    {
        return Err(corrupt("terminal_held"));
    }
    lease.routine = Some(stored);
    Ok(())
}

pub(super) fn current_permissions(proof: &FreshBotPolicyProof) -> &CapturedBotPolicy {
    proof
        .routine
        .as_ref()
        .map_or(&proof.global.policy, |r| &r.policy)
}
