// SPDX-License-Identifier: MPL-2.0

//! A request settles exactly once, by a compare on `state='open'`, and
//! records who settled it. The child's own ending is `clock.rs`'s one
//! statement; here are the clock owner's timeout and the parent's cleanup.
use super::*;
use crate::{RunsDb, WriteOp};
use koru_delegate_core::children::{BOT_CHILD_WAIT_MS, ChildClock, ChildRequestReceipt};
use koru_delegate_core::{ChildRequestId, DelegateBootId};
use rusqlite::{Connection, params};

/// The parent's wait is being cleaned up: every open request of the wait is
/// settled `parent_stopped` before the phase moves, as the table's trigger
/// requires. Returns how many it settled.
pub(crate) fn settle_parent_stopped_on(
    tx: &Connection,
    wait: BotWaitId,
    now: i64,
) -> Result<usize, BotLedgerError> {
    tx.execute(
        "UPDATE bot_child_requests
            SET state='settled', settled_by='parent_stopped', clock='stopped',
                clock_boot_id=NULL, clock_since_ms=NULL,
                wait_elapsed_ms=min(?3, wait_elapsed_ms+CASE WHEN clock='running'
                    THEN max(0, ?2-clock_since_ms) ELSE 0 END),
                revision=revision+1, settled_at_ms=?2, updated_at_ms=?2
          WHERE wait_id=?1 AND state='open'",
        params![wait.to_storage(), now, BOT_CHILD_WAIT_MS],
    )
    .map_err(sql)
}

impl RunsDb {
    /// The clock owner's timeout. A named request is settled `timed_out`
    /// only while it is open, its clock runs under this boot, and the stored
    /// time plus the wall interval since the clock last started could have
    /// reached the budget. That is a necessary condition, never the decision:
    /// the owner counts the time and decides. Anything else is left as it is.
    pub fn settle_child_requests(
        &mut self,
        owner: BotOwner,
        boot: DelegateBootId,
        due: &[ChildRequestId],
    ) -> Result<Vec<ChildRequestReceipt>, BotLedgerError> {
        if due.len() > CHILD_GRAPH_EDGE_LIMIT {
            return Err(BotLedgerError::Invalid);
        }
        self.write_with_error(WriteOp::ChildWait, |tx, now| {
            let mut settled = Vec::new();
            for id in due {
                let Some(row) = rows::by_id(tx, *id, now)? else {
                    return Err(BotLedgerError::Stale);
                };
                if row.receipt.parent.owner != owner {
                    return Err(BotLedgerError::Fenced);
                }
                let reached = row.receipt.clock == ChildClock::Running
                    && row.boot == Some(boot)
                    && u64::from(row.receipt.elapsed_ms) >= BOT_CHILD_WAIT_MS;
                if !reached {
                    continue;
                }
                let changed = tx
                    .execute(
                        "UPDATE bot_child_requests
                            SET state='settled', settled_by='timed_out', clock='stopped',
                                clock_boot_id=NULL, clock_since_ms=NULL, wait_elapsed_ms=?3,
                                revision=revision+1, settled_at_ms=?2, updated_at_ms=?2
                          WHERE child_request_id=?1 AND state='open' AND revision=?4",
                        params![
                            id.to_storage(),
                            now,
                            BOT_CHILD_WAIT_MS,
                            row.receipt.revision.get()
                        ],
                    )
                    .map_err(sql)?;
                require(changed == 1)?;
                settled.push(
                    rows::by_id(tx, *id, now)?
                        .ok_or_else(|| corrupt("bot_child_requests", "child_request_id"))?
                        .receipt,
                );
            }
            Ok(settled)
        })
    }
}
