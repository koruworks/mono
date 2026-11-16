// SPDX-License-Identifier: MPL-2.0

//! The wait graph: one node per Bot of this computer, an edge P -> Q while a
//! request from P to Q is open. A waiting parent keeps its Bot slot, so a
//! cycle is a deadlock. The walk runs inside the transaction that inserts
//! the new edges, so two opposing asks can never both pass.
use super::*;
use koru_delegate_core::{BotId, DeviceId};
use rusqlite::{Connection, params};
use std::collections::BTreeSet;

/// What the walk read, for the tests that pin its bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CycleWalk {
    pub closes: bool,
    pub edges_read: usize,
    pub expanded: usize,
}

pub(crate) const EDGES: &str = "SELECT DISTINCT recipient_bot_id FROM bot_child_requests \
INDEXED BY bot_child_requests_open_parent WHERE device_id=?1 AND parent_bot_id=?2 AND state='open'";

/// Whether an ask from `parent` to `recipients` would close a cycle: whether
/// any recipient can already reach `parent` along open edges. Each Bot is
/// expanded once. A legal ledger holds at most [`CHILD_GRAPH_EDGE_LIMIT`]
/// edges; reading one more is a broken ledger, never a refusal.
pub(crate) fn walk(
    conn: &Connection,
    device: DeviceId,
    parent: BotId,
    recipients: &[BotId],
) -> Result<CycleWalk, BotLedgerError> {
    let mut statement = conn.prepare(EDGES).map_err(sql)?;
    let mut visited: BTreeSet<BotId> = recipients.iter().copied().collect();
    let mut frontier: Vec<BotId> = visited.iter().copied().collect();
    let mut result = CycleWalk {
        closes: visited.contains(&parent),
        edges_read: 0,
        expanded: 0,
    };
    while let Some(bot) = frontier.pop() {
        if result.closes {
            break;
        }
        result.expanded += 1;
        let mut rows = statement
            .query(params![device.to_storage(), bot.to_storage()])
            .map_err(sql)?;
        while let Some(row) = rows.next().map_err(sql)? {
            result.edges_read += 1;
            if result.edges_read > CHILD_GRAPH_EDGE_LIMIT {
                return Err(corrupt("bot_child_requests", "graph"));
            }
            let next: BotId = row
                .get::<_, String>(0)
                .map_err(|_| corrupt("bot_child_requests", "recipient_bot_id"))?
                .parse()
                .map_err(|_| corrupt("bot_child_requests", "recipient_bot_id"))?;
            if next == parent {
                result.closes = true;
                break;
            }
            if visited.insert(next) {
                frontier.push(next);
            }
        }
    }
    Ok(result)
}
