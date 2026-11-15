// SPDX-License-Identifier: MPL-2.0

//! Bounded keyset selection of Runs with an unfinished decision projection.
//! Only the three approved partial indexes are read (Prepared, PendingSource,
//! PendingControl), each by a run_id seek; no body, history or applied row is
//! touched. The sole C decoder still validates the selected Run's shape.

use super::*;

/// The approved per-index read bound (16 + 1 identity rows each).
pub const DECISION_PROJECTION_RUN_PAGE: usize = 16;

const SEEKS: [&str; 3] = [
    "SELECT run_id FROM bot_decision_publications INDEXED BY bot_decision_publications_prepared
     WHERE state='prepared' AND run_id>?1 ORDER BY run_id LIMIT ?2",
    "SELECT run_id FROM bot_decision_publications INDEXED BY bot_decision_publications_pending_source
     WHERE state='pending' AND projection_kind='source' AND run_id>?1 ORDER BY run_id LIMIT ?2",
    "SELECT run_id FROM bot_decision_publications INDEXED BY bot_decision_publications_pending_control
     WHERE state='pending' AND projection_kind='control' AND run_id>?1 ORDER BY run_id LIMIT ?2",
];

impl RunsDb {
    /// At most `limit` distinct Runs strictly after `after`, in run_id order,
    /// merged from at most `limit + 1` rows of each partial index. A caller
    /// rotates its cursor and wraps once with `None`; there is no unbounded
    /// loop and no skip-after-LIMIT scan of ineligible history.
    pub fn decision_projection_runs(
        &self,
        after: Option<RunId>,
        limit: usize,
    ) -> Result<Vec<RunId>, LedgerError> {
        if !(1..=DECISION_PROJECTION_RUN_PAGE).contains(&limit) {
            return Err(LedgerError::Corrupt {
                table: "bot_decision_publications",
                column: "limit",
            });
        }
        let after = after.map(RunId::to_storage).unwrap_or_default();
        let mut runs = std::collections::BTreeSet::new();
        for seek in SEEKS {
            let mut statement = self
                .conn()
                .prepare_cached(seek)
                .map_err(|e| LedgerError::sql("select decision projection Runs", e))?;
            let mut rows = statement
                .query(params![after, (limit + 1) as i64])
                .map_err(|e| LedgerError::sql("select decision projection Runs", e))?;
            while let Some(row) = rows
                .next()
                .map_err(|e| LedgerError::sql("select decision projection Runs", e))?
            {
                let run: String = row
                    .get(0)
                    .map_err(|e| LedgerError::sql("select decision projection Runs", e))?;
                runs.insert(parse_run(&run).map_err(|_| LedgerError::Corrupt {
                    table: "bot_decision_publications",
                    column: "run_id",
                })?);
            }
        }
        Ok(runs.into_iter().take(limit).collect())
    }
}
