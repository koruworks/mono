// SPDX-License-Identifier: MPL-2.0

//! Bounded owning reads of child requests and child waits. None of them
//! grants anything: not a stop, not a resume, not a result.
use super::*;
use crate::RunsDb;
use koru_delegate_core::children::{
    BotChildWaitObservation, ChildRequestReceipt, ChildRequestState, ChildRequests,
    ChildWaitDisposition, ChildWaitView,
};
use koru_delegate_core::decisions::{
    BotWaitPhase, ConsumerCleanup, DecisionCleanupTarget, DecisionClosureReason, DecisionValidate,
    DecisionWaitExpected, FreshWaitStopEligibility, OriginWaitStop,
};
use koru_delegate_core::{BotId, DeviceId};
use rusqlite::{Connection, OptionalExtension, params};

/// Whether this Run is the child of some request. A child Run never asks,
/// and never asks a private-operation question.
pub(crate) fn is_child_run_on(conn: &Connection, run: RunId) -> Result<bool, BotLedgerError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bot_child_requests WHERE child_run_id=?1)",
        [run.to_storage()],
        |r| r.get(0),
    )
    .map_err(sql)
}

fn target_on(
    conn: &Connection,
    wait: &ChildWaitRecord,
    resume: Option<&koru_delegate_core::children::ChildResume>,
) -> Result<DecisionCleanupTarget, BotLedgerError> {
    match resume {
        Some(resume) => resume.cleanup_target().map_err(|_| BotLedgerError::Invalid),
        None => {
            let origin = crate::decisions::origin_attempt_on(conn, &wait.origin)?;
            Ok(DecisionCleanupTarget::Origin {
                fence: wait.origin,
                creation_key: origin
                    .provider_session
                    .ok_or(BotLedgerError::Fenced)?
                    .idempotency,
            })
        }
    }
}

/// The Run's open child wait with its retained physical cleanup subject.
pub(crate) fn execution_on(
    conn: &Connection,
    owner: BotOwner,
    run: RunId,
) -> Result<Option<ChildWaitExecution>, BotLedgerError> {
    crate::decisions::owner_on(conn, owner, run)?;
    let Some(wait) = crate::decisions::open_child_wait_on(conn, run)? else {
        return Ok(None);
    };
    require(wait.origin.owner == owner && wait.origin.run == run)?;
    let resume = crate::decisions::child_resume_on(conn, &wait)?;
    let cleanup_target = target_on(conn, &wait, resume.as_ref())?;
    let (attempt, generation) = cleanup_target.attempt();
    let retained = crate::attempts::retained_bounded(conn, run, generation)?
        .ok_or_else(|| corrupt("bot_wait_runtime", "cleanup_attempt"))?;
    let latest = crate::attempts::latest(conn, run)?.ok_or(BotLedgerError::Fenced)?;
    require(
        retained.attempt == attempt
            && latest.generation == generation
            && retained.provider_session.is_some_and(|key| {
                key.idempotency == cleanup_target.creation_key()
                    && key.session == cleanup_target.session()
            }),
    )?;
    let requests: Vec<ChildRequestReceipt> = rows::by_wait(conn, wait.wait, wait.updated_at_ms)?
        .into_iter()
        .map(|row| row.receipt)
        .collect();
    let open = requests
        .iter()
        .filter(|receipt| receipt.state == ChildRequestState::Open)
        .count();
    Ok(Some(ChildWaitExecution {
        expected: DecisionWaitExpected {
            owner,
            run,
            wait: wait.wait,
            revision: wait.revision,
            decision: None,
        },
        wait,
        requests,
        open_requests: u8::try_from(open).map_err(|_| corrupt("bot_child_requests", "limit"))?,
        resume,
        cleanup_target,
        current_attempt: retained,
    }))
}

impl RunsDb {
    /// See [`crate::BotLedger::child_wait_execution`].
    pub fn child_wait_execution(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<ChildWaitExecution>, BotLedgerError> {
        execution_on(self.conn(), owner, run)
    }

    /// The parent Run's requests in creation order, with its open child wait.
    pub fn read_child_requests(
        &self,
        owner: BotOwner,
        run: RunId,
        now_ms: i64,
    ) -> Result<ChildRequests, BotLedgerError> {
        let conn = self.conn();
        crate::decisions::owner_on(conn, owner, run)?;
        let requests: Vec<ChildRequestReceipt> = rows::by_parent(conn, run, now_ms)?
            .into_iter()
            .map(|row| row.receipt)
            .collect();
        let wait = crate::decisions::open_child_wait_on(conn, run)?
            .map(|wait| {
                Ok::<_, BotLedgerError>(ChildWaitView {
                    wait: wait.wait,
                    phase: wait.phase,
                    revision: wait.revision,
                    open_requests: rows::open_count(conn, wait.wait)?,
                })
            })
            .transpose()?;
        Ok(ChildRequests { wait, requests })
    }

    /// The request this Run is the child of, if any.
    pub fn child_request_of(
        &self,
        owner: BotOwner,
        child_run: RunId,
        now_ms: i64,
    ) -> Result<Option<ChildRequestReceipt>, BotLedgerError> {
        crate::decisions::owner_on(self.conn(), owner, child_run)?;
        let row = rows::by_child(self.conn(), child_run, now_ms)?;
        if let Some(row) = &row {
            require(row.receipt.parent.owner == owner)?;
        }
        Ok(row.map(|row| row.receipt))
    }

    /// The observation behind `ReadBotChildWait`, for a wait of this Run in
    /// any phase. `None` when the Run has no such child wait.
    pub fn read_bot_child_wait(
        &self,
        owner: BotOwner,
        bot: BotId,
        run: RunId,
        wait: BotWaitId,
    ) -> Result<Option<BotChildWaitObservation>, BotLedgerError> {
        let conn = self.conn();
        crate::decisions::owner_on(conn, owner, run)?;
        let Some(record) = crate::decisions::child_wait_on(conn, run, wait)? else {
            return Ok(None);
        };
        require(record.origin.owner == owner && record.origin.bot == bot)?;
        let resume = crate::decisions::child_resume_on(conn, &record)?;
        let closing = record.cleanup.is_some() || record.phase == BotWaitPhase::CleanupPending;
        let disposition = if closing {
            // The origin's own turn was delivered, or the wait was stopped.
            let delivered: Option<String> = conn
                .query_row(
                    "SELECT delivery_attempt_id FROM bot_admissions WHERE run_id=?1",
                    [run.to_storage()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql)?
                .flatten();
            ChildWaitDisposition::Closed {
                cause: if delivered == Some(record.origin.attempt.to_storage()) {
                    DecisionClosureReason::AiTerminal
                } else {
                    DecisionClosureReason::Canceled
                },
            }
        } else {
            ChildWaitDisposition::Open {}
        };
        let consumer_cleanup = match &record.cleanup {
            Some(cleanup) => cleanup.clone(),
            None if record.phase == BotWaitPhase::CleanupPending => ConsumerCleanup::Owed {
                cleanup_revision: record.revision,
                target: target_on(conn, &record, resume.as_ref())?,
            },
            None => ConsumerCleanup::NotRequested {},
        };
        let observation = BotChildWaitObservation {
            key: record.intent.key().map_err(|_| BotLedgerError::Invalid)?,
            cleanup_intent: record.intent.clone(),
            current_revision: record.revision,
            fresh_stop_eligibility: match record.phase {
                BotWaitPhase::HoldRequested => FreshWaitStopEligibility::EligibleOriginWaitStop,
                BotWaitPhase::Retired | BotWaitPhase::ResumeReserved => {
                    FreshWaitStopEligibility::Superseded
                }
                _ => FreshWaitStopEligibility::Terminal,
            },
            origin_stop: record.release.clone().unwrap_or(OriginWaitStop::Pending {}),
            consumer_cleanup,
            disposition,
            resume,
        };
        observation
            .validate()
            .map_err(|_| corrupt("bot_wait_runtime", "observation"))?;
        Ok(Some(observation))
    }

    /// See [`crate::BotLedger::child_resume_runs`].
    pub fn child_resume_runs(
        &self,
        device: DeviceId,
        after: Option<&ChildWaitCursor>,
        limit: usize,
    ) -> Result<Vec<ChildWaitRun>, BotLedgerError> {
        let conn = self.conn();
        let limit = limit.clamp(1, CHILD_RESUME_RUN_PAGE) as i64;
        let (after_ms, after_wait) = after.map_or((i64::MIN, String::new()), |cursor| {
            (cursor.created_at_ms, cursor.wait.to_storage())
        });
        let mut statement = conn
            .prepare(
                "SELECT wait_id,run_id,created_at_ms FROM bot_wait_runtime INDEXED BY bot_wait_runtime_child_open
                  WHERE device_id=?1 AND reason='child' AND phase<>'settled'
                    AND (created_at_ms>?2 OR (created_at_ms=?2 AND wait_id>?3))
                  ORDER BY created_at_ms,wait_id LIMIT ?4",
            )
            .map_err(sql)?;
        let page: Vec<(String, String, i64)> = statement
            .query_map(
                params![device.to_storage(), after_ms, after_wait, limit],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        page.into_iter()
            .map(|(wait, run, created_at_ms)| {
                let wait: BotWaitId = wait
                    .parse()
                    .map_err(|_| corrupt("bot_wait_runtime", "wait_id"))?;
                let run: RunId = run
                    .parse()
                    .map_err(|_| corrupt("bot_wait_runtime", "run_id"))?;
                let record = crate::decisions::child_wait_on(conn, run, wait)?
                    .ok_or_else(|| corrupt("bot_wait_runtime", "wait_id"))?;
                require(record.origin.owner.device == device)?;
                Ok(ChildWaitRun {
                    owner: record.origin.owner,
                    run,
                    wait,
                    phase: record.phase,
                    open_requests: rows::open_count(conn, wait)?,
                    cursor: ChildWaitCursor {
                        created_at_ms,
                        wait,
                    },
                })
            })
            .collect()
    }
}
