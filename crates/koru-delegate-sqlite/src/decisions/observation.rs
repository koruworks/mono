// SPDX-License-Identifier: MPL-2.0

use super::*;

pub(super) fn execution(
    conn: &Connection,
    owner: BotOwner,
    run: RunId,
) -> Result<Option<DecisionWaitExecution>, BotLedgerError> {
    authority::owner(conn, owner, run)?;
    let wait:Option<String> = conn.query_row(
        "SELECT CASE WHEN typeof(wait_id)='text' AND length(CAST(wait_id AS BLOB))=36 THEN wait_id END
         FROM bot_wait_runtime INDEXED BY bot_wait_runtime_open_run WHERE run_id=?1 AND phase<>'settled'",
        [run.to_storage()],|r|r.get(0)).optional().map_err(sql)?;
    let Some(wait) = wait else { return Ok(None) };
    let wait = wait
        .parse()
        .map_err(|_| corrupt("bot_wait_runtime", "wait_id"))?;
    let wait =
        rows::by_wait(conn, run, wait)?.ok_or_else(|| corrupt("bot_wait_runtime", "wait_id"))?;
    require(wait.origin.owner == owner && wait.origin.run == run)?;
    let (wait, decision_state, resume, decision_expected) = match wait.reason {
        BotWaitReason::Decision { decision } => {
            let record = rows::read(conn, owner, run, decision)?
                .ok_or_else(|| corrupt("bot_wait_runtime", "decision_id"))?;
            require(record.wait.wait == wait.wait)?;
            let expected = DecisionExpected {
                owner,
                run,
                decision,
                revision: record.receipt.state.revision,
                binding_digest: record.receipt.binding_digest,
            };
            (
                record.wait,
                Some(record.receipt.state),
                record.resume,
                Some(expected),
            )
        }
        _ => {
            crate::bots::routines::bounded_admission(conn, run)?;
            let admission = crate::bots::routines::native_admission(conn, run)?;
            require(
                admission.submission.provenance.owner == owner
                    && admission.submission.provenance.bot == wait.origin.bot,
            )?;
            let origin = authority::attempt(conn, &wait.origin)?;
            require(origin.provider_session.and_then(|s| s.session) == Some(wait.origin.session))?;
            (wait, None, None, None)
        }
    };
    // A child wait has no decision and keeps its own resume row: once a
    // successor is recorded, the successor is the physical cleanup subject.
    let child_resume = match wait.reason {
        BotWaitReason::Child {} => {
            let child = rows::child::wait(conn, run, wait.wait)?
                .ok_or_else(|| corrupt("bot_wait_runtime", "reason"))?;
            rows::child::resume(conn, &child)?
        }
        _ => None,
    };
    let cleanup_target = match (&resume, &child_resume) {
        (Some(resume), _) => {
            DecisionCleanupTarget::from_resume(resume).map_err(|_| BotLedgerError::Invalid)?
        }
        (None, Some(resume)) => resume
            .cleanup_target()
            .map_err(|_| BotLedgerError::Invalid)?,
        (None, None) => {
            let origin = authority::attempt(conn, &wait.origin)?;
            DecisionCleanupTarget::Origin {
                fence: wait.origin,
                creation_key: origin
                    .provider_session
                    .ok_or(BotLedgerError::Fenced)?
                    .idempotency,
            }
        }
    };
    let (attempt, generation) = cleanup_target.attempt();
    let retained = crate::attempts::retained_bounded(conn, run, generation)?
        .ok_or_else(|| corrupt("bot_wait_runtime", "cleanup_attempt"))?;
    let latest: Option<u32> = conn
        .query_row(
            "SELECT max(generation) FROM run_attempts WHERE run_id=?1",
            [run.to_storage()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    require(
        retained.attempt == attempt
            && latest == Some(generation.get())
            && retained.provider_session.is_some_and(|p| {
                p.idempotency == cleanup_target.creation_key()
                    && p.session == cleanup_target.session()
            }),
    )?;
    Ok(Some(DecisionWaitExecution {
        expected: DecisionWaitExpected {
            owner,
            run,
            wait: wait.wait,
            revision: wait.revision,
            decision: decision_expected,
        },
        wait,
        decision_state,
        resume,
        cleanup_target,
        current_attempt: retained,
    }))
}

impl RunsDb {
    /// This is a bounded owning read, not proof of process absence or permission
    /// to stop. The runtime must use the exact returned creation key/host and
    /// its actual non-creating retire/lookup ports for physical observations.
    pub fn decision_wait_execution(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<DecisionWaitExecution>, BotLedgerError> {
        execution(self.conn(), owner, run)
    }
}
