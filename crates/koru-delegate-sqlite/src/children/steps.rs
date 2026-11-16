// SPDX-License-Identifier: MPL-2.0

//! The hold of a child wait: the AI owner's enrollment receipt and the
//! proven origin stop. These are a decision wait's steps without a decision,
//! written as their own functions; no decision function is widened.
use super::*;
use crate::{RunsDb, WriteOp};
use koru_delegate_core::BotAdmissionState;
use koru_delegate_core::children::BotChildWaitEnrollmentReply;
use koru_delegate_core::decisions::{
    BotWaitCleanupCause, BotWaitPhase, DECISION_EVIDENCE_BYTES, DecisionWaitExpected,
    OriginWaitStop, encode_decision_value,
};
use rusqlite::params;

fn encoded<T: serde::Serialize>(value: &T) -> Result<String, BotLedgerError> {
    String::from_utf8(
        encode_decision_value(value, DECISION_EVIDENCE_BYTES)
            .map_err(|_| BotLedgerError::Invalid)?,
    )
    .map_err(|_| BotLedgerError::Invalid)
}

impl RunsDb {
    /// Retain the AI owner's `Enrolled` answer. A late exact answer records
    /// history even after a Stop; it never changes the phase.
    pub fn record_child_enrollment(
        &mut self,
        expected: &DecisionWaitExpected,
        reply: &BotChildWaitEnrollmentReply,
    ) -> Result<ChildWaitRecord, BotLedgerError> {
        if expected.decision.is_some() {
            return Err(BotLedgerError::Invalid);
        }
        self.write_with_error(WriteOp::ChildWait, |tx, now| {
            crate::decisions::owner_on(tx, expected.owner, expected.run)?;
            let old = crate::decisions::child_wait_on(tx, expected.run, expected.wait)?
                .ok_or(BotLedgerError::Stale)?;
            let key = old.intent.key().map_err(|_| BotLedgerError::Invalid)?;
            let BotChildWaitEnrollmentReply::Enrolled { ai_attempt, .. } = reply else {
                return Err(BotLedgerError::Conflict);
            };
            if !reply.matches_request(&key, false) {
                return Err(BotLedgerError::Conflict);
            }
            if let Some(prior) = &old.enrollment {
                require(prior == reply)?;
                return Ok(old);
            }
            if old.phase == BotWaitPhase::Settled || old.revision != expected.revision {
                return Err(BotLedgerError::Stale);
            }
            let admitted = crate::bots::routines::native_admission(tx, expected.run)?;
            require(admitted.submission.provenance.ai_attempt_id == Some(*ai_attempt))?;
            let changed = tx
                .execute(
                    "UPDATE bot_wait_runtime SET ai_cleanup_receipt_json=?2,revision=?3,updated_at_ms=max(updated_at_ms,?4)
                      WHERE wait_id=?1 AND revision=?5 AND ai_cleanup_receipt_json IS NULL",
                    params![
                        old.wait.to_storage(),
                        encoded(reply)?,
                        next(old.revision)?.get(),
                        now,
                        old.revision.get()
                    ],
                )
                .map_err(sql)?;
            if changed != 1 {
                return Err(BotLedgerError::Stale);
            }
            crate::decisions::child_wait_on(tx, expected.run, expected.wait)?
                .ok_or(BotLedgerError::Stale)
        })
    }

    /// Retain the proven origin stop. From `hold_requested` the wait becomes
    /// `retired`; a wait already being cleaned up keeps `cleanup_pending`.
    /// Either way the parent's active interval closes: its 10-minute budget
    /// does not run while it waits.
    pub fn record_child_origin_stop(
        &mut self,
        expected: &DecisionWaitExpected,
        release: &OriginWaitStop,
    ) -> Result<ChildWaitRecord, BotLedgerError> {
        if expected.decision.is_some() {
            return Err(BotLedgerError::Invalid);
        }
        self.write_with_error(WriteOp::ChildWait, |tx, now| {
            crate::decisions::owner_on(tx, expected.owner, expected.run)?;
            let old = crate::decisions::child_wait_on(tx, expected.run, expected.wait)?
                .ok_or(BotLedgerError::Stale)?;
            if let Some(stored) = &old.release {
                require(stored == release)?;
                return Ok(old);
            }
            let OriginWaitStop::Confirmed {
                cause,
                creation_retirement,
                native_drain,
                ..
            } = release
            else {
                return Err(BotLedgerError::Invalid);
            };
            if old.revision != expected.revision {
                return Err(BotLedgerError::Stale);
            }
            require(
                *cause == BotWaitCleanupCause::ChildWaitCleanup
                    && old.enrollment.is_some()
                    && crate::decisions::child_resume_on(tx, &old)?.is_none()
                    && matches!(
                        old.phase,
                        BotWaitPhase::HoldRequested | BotWaitPhase::CleanupPending
                    )
                    && creation_retirement.observed_at_ms <= now
                    && native_drain.observed_at_ms <= now,
            )?;
            let mut observed = old.clone();
            observed.release = Some(release.clone());
            crate::decisions::correlate_child_release_on(tx, &observed)?;
            let latest = crate::attempts::latest(tx, expected.run)?.ok_or(BotLedgerError::Fenced)?;
            require(latest.generation == old.origin.generation)?;
            let admitted = crate::bots::routines::native_admission(tx, expected.run)?;
            require(admitted.state == BotAdmissionState::Executing)?;
            let holding = old.phase == BotWaitPhase::HoldRequested;
            let elapsed = admitted.active_elapsed_at(now).min(i64::MAX as u64) as i64;
            tx.execute(
                "UPDATE bot_admissions SET active_elapsed_ms=?2,active_since_ms=NULL,updated_at_ms=max(updated_at_ms,?3) WHERE run_id=?1",
                params![expected.run.to_storage(), elapsed, now],
            )
            .map_err(sql)?;
            let changed = tx
                .execute(
                    "UPDATE bot_wait_runtime SET release_evidence_json=?2,phase=?3,revision=?4,updated_at_ms=max(updated_at_ms,?5)
                      WHERE wait_id=?1 AND revision=?6 AND release_evidence_json IS NULL",
                    params![
                        old.wait.to_storage(),
                        encoded(release)?,
                        if holding { "retired" } else { "cleanup_pending" },
                        next(old.revision)?.get(),
                        now,
                        old.revision.get()
                    ],
                )
                .map_err(sql)?;
            require(changed == 1)?;
            crate::decisions::child_wait_on(tx, expected.run, expected.wait)?
                .ok_or(BotLedgerError::Stale)
        })
    }
}
