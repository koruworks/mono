// SPDX-License-Identifier: MPL-2.0

//! Bot execution consumes the same full occurrence identity as native effects.
//! Historical launch evidence is separate from fresh current policy authority.
use super::*;
use crate::{BotAdmissionRecord, BotInitialSend};
use koru_delegate_core::{
    BotAdmissionState, FreshBotPolicyProof, RoutineClaimRecord, RoutineLinkState,
};

pub(in crate::bots) fn retained(
    conn: &Connection,
    admitted: &BotAdmissionRecord,
) -> Result<Option<RoutineClaimRecord>, BotLedgerError> {
    match (
        admitted.submission_version,
        admitted.submission.provenance.routine,
    ) {
        (1, None) => {
            deny_run(conn, admitted.submission.request.run)?;
            Ok(None)
        }
        (2, Some(_)) => validate_admission(conn, admitted).map(Some),
        _ => Err(BotLedgerError::Fenced),
    }
}

pub(in crate::bots) fn fresh(
    conn: &Connection,
    admitted: &BotAdmissionRecord,
    proof: Option<&FreshBotPolicyProof>,
    dispatch: bool,
) -> Result<(), BotLedgerError> {
    let Some(record) = retained(conn, admitted)? else {
        return Ok(());
    };
    if record.link_state != (RoutineLinkState::Linked {})
        || matches!(
            admitted.state,
            BotAdmissionState::Settled | BotAdmissionState::Refused
        )
    {
        return Err(BotLedgerError::Fenced);
    }
    let proof = proof.ok_or(BotLedgerError::Fenced)?;
    crate::native_results::current_composite(conn, admitted, proof)?;
    if dispatch {
        let p = &admitted.submission.provenance;
        let barrier = super::super::repository::barrier(conn, p.owner, p.bot)?
            .ok_or(BotLedgerError::Fenced)?;
        if barrier.paused || barrier.epoch != proof.global.config_revision {
            return Err(BotLedgerError::Fenced);
        }
    }
    Ok(())
}

/// Receives an already decoded admission; deliberately never calls the outer
/// occurrence decoder, which invokes this helper when validating stored Fired.
pub(super) fn launch_evidence(
    conn: &Connection,
    admitted: &BotAdmissionRecord,
    send: &BotInitialSend,
) -> Result<(), BotLedgerError> {
    use koru_delegate_core::AttemptGeneration;
    let run = admitted.submission.request.run;
    let attempt = crate::attempts::latest(conn, run)?.ok_or(BotLedgerError::Fenced)?;
    let materialized: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM run_intents WHERE run_id=?1 AND state='satisfied' AND typeof(run_document_id)='text' AND length(CAST(run_document_id AS BLOB))=36)",
        [run.to_storage()], |r| r.get(0),
    ).map_err(sql)?;
    if !materialized
        || send.turn.is_none()
        || send.generation != AttemptGeneration::FIRST
        || attempt.attempt != send.attempt
        || attempt.generation != send.generation
        || attempt.provider_session.and_then(|p| p.session).is_none()
        || attempt.host_instance.is_none()
    {
        return Err(BotLedgerError::Fenced);
    }
    Ok(())
}

pub(in crate::bots) fn acknowledged(
    conn: &Connection,
    admitted: &BotAdmissionRecord,
    send: &BotInitialSend,
) -> Result<(), BotLedgerError> {
    let Some(record) = retained(conn, admitted)? else {
        return Ok(());
    };
    launch_evidence(conn, admitted, send)?;
    let changed = conn.execute(
        "UPDATE schedule_claims SET outcome='fired' WHERE schedule_id=?1 AND occurrence_ms=?2 AND run_id=?3 AND revision=?4 AND claimed_at_ms=?5 AND outcome IN ('claimed','fired') AND busy_holder_run_id IS NULL",
        params![record.claim.slot.schedule.to_storage(), record.claim.slot.occurrence_ms,
            record.claim.ids.run.to_storage(), record.claim.definition.revision, record.claim.claimed_at_ms],
    ).map_err(sql)?;
    if changed != 1 {
        return Err(BotLedgerError::Fenced);
    }
    Ok(())
}
