// SPDX-License-Identifier: MPL-2.0

//! Cancel reaches the children (contract 6.4). When the parent's wait is
//! cleaned up (a Stop, the budget stop, the origin turn's own delivery, an
//! unqualified wait) the ledger settles every open request `parent_stopped`
//! in the transaction that moves the wait to `cleanup_pending`. This owner
//! then asks for Stop of each child Run of that wait that has not ended.
//!
//! Only Runs named by the wait's own requests are stopped. A child's saved
//! pages are never deleted. If a Stop does not go through, the request is
//! already settled; the clock tick asks again until the child has ended.

use super::*;
use koru_delegate_core::{BotAdmissionState, DeviceId};

impl Orchestrator {
    /// Owe a Stop to every child Run of this wait that has not ended, and
    /// publish what the cleanup changed. Nothing is stopped here.
    pub(in crate::orchestrator) fn child_cancel_children(
        &mut self,
        owner: BotOwner,
        run: RunId,
        wait: BotWaitId,
    ) {
        let now = self.ports.clock.now_ms();
        let read = self
            .bot_ledger()
            .and_then(|ledger| Ok(ledger.read_child_requests(owner, run, now)?));
        let Ok(read) = read else {
            return;
        };
        for receipt in read.requests.iter().filter(|receipt| receipt.wait == wait) {
            if !matches!(
                receipt.child,
                BotAdmissionState::Settled | BotAdmissionState::Refused
            ) {
                self.children
                    .stops
                    .insert(receipt.child_run, (owner, receipt.recipient));
            }
        }
        self.child_publish(&read.requests, false);
    }

    /// The parent Run's wait was just asked to clean up: reach its children
    /// now rather than on the next tick. A Run with no open child wait is
    /// untouched. Best effort; the tick repeats it.
    pub(crate) fn child_cleanup_requested(&mut self, run: RunId, device: DeviceId) {
        let Ok(Some(record)) = self
            .bot_ledger()
            .and_then(|ledger| Ok(ledger.bot_execution(run)?))
        else {
            return;
        };
        let Ok(Some(execution)) = self
            .bot_ledger()
            .and_then(|ledger| Ok(ledger.child_wait_execution(record.owner, run)?))
        else {
            return;
        };
        self.children.launches.remove(&run);
        self.children.stalled.remove(&execution.wait.wait);
        self.child_cancel_children(record.owner, run, execution.wait.wait);
        self.child_retry_stops(device);
        if let Err(error) = self.sync_child_authority(record.owner, run) {
            self.trace(run, "child_wait_unpublished", format_args!("{error}"));
        }
    }
}
