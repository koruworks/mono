// SPDX-License-Identifier: MPL-2.0

//! The 15-minute clock of each request. Counted time is kept by the owner in
//! memory; a row is written only on a transition: admission, a fold at a gap
//! or a clean stop, the adoption of a clock an older process left running, a
//! pause of the parent Bot, and settlement. Stored time never goes down.
use super::*;
use crate::{RunsDb, WriteOp};
use koru_delegate_core::children::{BOT_CHILD_WAIT_MS, ChildClock, ChildRequestReceipt};
use koru_delegate_core::{BotId, ChildRequestId, DelegateBootId, DeviceId};
use rusqlite::{Connection, Transaction, params};

/// The elapsed time a settling or folding statement writes: the stored
/// amount plus the open wall interval, capped at the budget.
const FOLDED: &str =
    "min(?3, wait_elapsed_ms+CASE WHEN clock='running' THEN max(0, ?2-clock_since_ms) ELSE 0 END)";

/// `settle_bot`'s one statement: the child's own settlement closes its
/// request and touches nothing of the parent's. It reads no parent row,
/// decodes nothing and inserts no publication, so nothing in the parent's
/// state can make the recipient's settlement fail. The elapsed time it
/// writes is a record only; the timeout is never decided from it.
pub(crate) fn settle_child_ended_on(
    tx: &Connection,
    child_run: RunId,
    now: i64,
) -> Result<usize, BotLedgerError> {
    tx.execute(
        &format!(
            "UPDATE bot_child_requests
                SET state='settled', settled_by='child_ended', clock='stopped',
                    clock_boot_id=NULL, clock_since_ms=NULL, wait_elapsed_ms={FOLDED},
                    revision=revision+1, settled_at_ms=?2, updated_at_ms=?2
              WHERE child_run_id=?1 AND state='open'"
        ),
        params![child_run.to_storage(), now, BOT_CHILD_WAIT_MS],
    )
    .map_err(sql)
}

/// Pausing the parent Bot folds and pauses the clocks of its own open
/// requests, in the transaction that applies the barrier. Requests of other
/// Bots, and requests this Bot is the recipient of, are not touched.
pub(crate) fn fold_parent_on(
    tx: &Connection,
    owner: BotOwner,
    parent: BotId,
    now: i64,
) -> Result<usize, BotLedgerError> {
    tx.execute(
        &format!(
            "UPDATE bot_child_requests
                SET wait_elapsed_ms={FOLDED}, clock='paused', clock_boot_id=NULL,
                    clock_since_ms=NULL, revision=revision+1, updated_at_ms=?2
              WHERE device_id=?1 AND parent_bot_id=?4 AND user_scope=?5
                AND state='open' AND clock='running'"
        ),
        params![
            owner.device.to_storage(),
            now,
            BOT_CHILD_WAIT_MS,
            parent.to_storage(),
            owner.user_scope.to_storage()
        ],
    )
    .map_err(sql)
}

fn parent_paused(
    conn: &Connection,
    owner: BotOwner,
    parent: BotId,
) -> Result<bool, BotLedgerError> {
    Ok(crate::bots::barrier_on(conn, owner, parent)?.is_some_and(|barrier| barrier.paused))
}

/// The open requests of this computer, through the open-edge index.
fn open_on(
    conn: &Connection,
    device: DeviceId,
    now: i64,
) -> Result<Vec<rows::RequestRow>, BotLedgerError> {
    let mut statement = conn
        .prepare(
            "SELECT child_request_id FROM bot_child_requests INDEXED BY bot_child_requests_open_parent
             WHERE device_id=?1 AND state='open' ORDER BY parent_bot_id,child_request_id LIMIT ?2",
        )
        .map_err(sql)?;
    let ids: Vec<String> = statement
        .query_map(
            params![device.to_storage(), CHILD_GRAPH_EDGE_LIMIT as i64 + 1],
            |row| row.get(0),
        )
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    if ids.len() > CHILD_GRAPH_EDGE_LIMIT {
        return Err(corrupt("bot_child_requests", "graph"));
    }
    ids.into_iter()
        .map(|id| {
            let id: ChildRequestId = id
                .parse()
                .map_err(|_| corrupt("bot_child_requests", "child_request_id"))?;
            rows::by_id(conn, id, now)?
                .ok_or_else(|| corrupt("bot_child_requests", "child_request_id"))
        })
        .collect()
}

fn fold_on(
    tx: &Transaction<'_>,
    device: DeviceId,
    boot: DelegateBootId,
    counted_until_ms: i64,
    resume_at_ms: i64,
    now: i64,
) -> Result<Vec<ChildRequestReceipt>, BotLedgerError> {
    let mut changed = Vec::new();
    for row in open_on(tx, device, now)? {
        let id = row.receipt.child_request;
        let paused = parent_paused(tx, row.receipt.parent.owner, row.receipt.parent.bot)?;
        let (elapsed, write) = match (row.receipt.clock, row.boot, row.since_ms) {
            // A clock of this process: count up to the earlier sample.
            (ChildClock::Running, Some(owner), Some(since)) if owner == boot => {
                let elapsed =
                    rows::elapsed_at(row.stored_elapsed_ms, Some(since), counted_until_ms);
                (
                    elapsed,
                    elapsed != row.stored_elapsed_ms || since != resume_at_ms,
                )
            }
            // A clock an older process left running: its uncounted time is
            // lost, never invented. It restarts with what was stored.
            (ChildClock::Running, Some(_), Some(_)) => (row.stored_elapsed_ms, true),
            // A paused clock restarts only once its parent Bot is unpaused.
            (ChildClock::Paused, None, None) if !paused => (row.stored_elapsed_ms, true),
            (ChildClock::Paused, None, None) => (row.stored_elapsed_ms, false),
            _ => return Err(corrupt("bot_child_requests", "clock")),
        };
        if !write {
            continue;
        }
        let updated = tx
            .execute(
                "UPDATE bot_child_requests
                    SET wait_elapsed_ms=?2, clock='running', clock_boot_id=?3, clock_since_ms=?4,
                        revision=revision+1, updated_at_ms=max(updated_at_ms,?5)
                  WHERE child_request_id=?1 AND state='open' AND revision=?6",
                params![
                    id.to_storage(),
                    elapsed,
                    boot.to_storage(),
                    resume_at_ms,
                    now,
                    row.receipt.revision.get()
                ],
            )
            .map_err(sql)?;
        require(updated == 1)?;
        changed.push(
            rows::by_id(tx, id, resume_at_ms)?
                .ok_or_else(|| corrupt("bot_child_requests", "child_request_id"))?
                .receipt,
        );
    }
    Ok(changed)
}

impl RunsDb {
    /// One transition of every running child clock on this computer. See
    /// [`crate::BotLedger::fold_child_clocks`].
    pub fn fold_child_clocks(
        &mut self,
        device: DeviceId,
        boot: DelegateBootId,
        counted_until_ms: i64,
        resume_at_ms: i64,
    ) -> Result<Vec<ChildRequestReceipt>, BotLedgerError> {
        self.write_with_error(WriteOp::ChildWait, |tx, now| {
            fold_on(tx, device, boot, counted_until_ms, resume_at_ms, now)
        })
    }
}
