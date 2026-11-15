// SPDX-License-Identifier: MPL-2.0

//! Bounded keyset selection of Runs with an open decision wait, for the
//! runtime's per-turn resume driver. Only the approved partial unique index
//! `bot_wait_runtime_open_run` is read, by a run_id seek; no evidence body is
//! decoded. The driver's own per-Run read re-validates everything.

use super::*;

/// The per-page bound of the open decision-wait enumeration.
pub const DECISION_RESUME_RUN_PAGE: usize = 16;

pub(super) const SEEK: &str = "SELECT w.run_id,
       CASE WHEN typeof(w.user_scope)='text' AND length(CAST(w.user_scope AS BLOB))=36 THEN w.user_scope END,
       CASE WHEN typeof(w.device_id)='text' AND length(CAST(w.device_id AS BLOB))=36 THEN w.device_id END
     FROM bot_wait_runtime AS w INDEXED BY bot_wait_runtime_open_run
     WHERE w.phase<>'settled' AND w.reason='decision' AND w.run_id>?1
     ORDER BY w.run_id LIMIT ?2";

impl RunsDb {
    /// At most `limit` Runs strictly after `after`, in run_id order, each with
    /// the owner its open decision wait names. A row whose owner columns are
    /// malformed is not returned and cannot starve the others; an owner that
    /// disagrees with the Run's admission is fenced by the per-Run reads the
    /// driver makes next. Settled and no-decision waits are never selected.
    pub fn decision_resume_runs(
        &self,
        after: Option<RunId>,
        limit: usize,
    ) -> Result<Vec<(BotOwner, RunId)>, BotLedgerError> {
        if !(1..=DECISION_RESUME_RUN_PAGE).contains(&limit) {
            return Err(BotLedgerError::Invalid);
        }
        let after = after.map(RunId::to_storage).unwrap_or_default();
        let mut statement = self.conn().prepare_cached(SEEK).map_err(sql)?;
        let mut rows = statement.query(params![after, limit as i64]).map_err(sql)?;
        let mut runs = Vec::with_capacity(limit);
        while let Some(row) = rows.next().map_err(sql)? {
            let run: String = row.get(0).map_err(sql)?;
            let user: Option<String> = row.get(1).map_err(sql)?;
            let device: Option<String> = row.get(2).map_err(sql)?;
            let run: RunId = run
                .parse()
                .map_err(|_| corrupt("bot_wait_runtime", "run_id"))?;
            let (Some(user), Some(device)) = (user, device) else {
                continue;
            };
            let Ok(owner) = serde_json::from_value::<BotOwner>(
                serde_json::json!({"user_scope": user, "device": device}),
            ) else {
                continue;
            };
            runs.push((owner, run));
        }
        Ok(runs)
    }
}
