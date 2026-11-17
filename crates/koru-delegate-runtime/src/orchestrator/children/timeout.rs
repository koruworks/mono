// SPDX-License-Identifier: MPL-2.0

//! Timeout (contract 6.2): when a request's counted time reaches its budget
//! the clock owner settles it `timed_out` in one transaction, then asks for
//! Stop of the child Run through the ordinary Stop path. The parent does not
//! wait for that stop to be proven. What the child saved stays where it is
//! and is not a result. The child's later settlement finds the request
//! settled and changes nothing.

use super::*;
use koru_delegate_core::{BotAdmissionState, BotRunKey, DeviceId};

impl Orchestrator {
    /// Settle the named requests of one owner as `timed_out` and owe each
    /// child Run a Stop. The ledger refuses a timeout that cannot be true
    /// (a clock that is not running under this boot, or stored time short of
    /// the budget); such a request is left as it is. Returns how many settled.
    pub(super) fn child_time_out(
        &mut self,
        owner: BotOwner,
        due: &[ChildRequestId],
        device: DeviceId,
    ) -> Result<usize, RunError> {
        let boot = self.delegate_boot();
        let settled = self.bot_ledger()?.settle_child_requests(owner, boot, due)?;
        self.child_publish(&settled, false);
        for receipt in &settled {
            self.children
                .stops
                .insert(receipt.child_run, (owner, receipt.recipient));
        }
        let _ = device;
        Ok(settled.len())
    }

    /// Ask again for every Stop still owed. A child that is settled, or
    /// whose Stop went through, leaves the list; one whose Stop is still
    /// being reconciled stays and is asked again on the next tick. A child
    /// left running is bounded by its own active budget.
    pub(in crate::orchestrator) fn child_retry_stops(&mut self, device: DeviceId) {
        let owed: Vec<(RunId, (BotOwner, BotId))> = self
            .children
            .stops
            .iter()
            .map(|(run, subject)| (*run, *subject))
            .collect();
        for (run, (owner, bot)) in owed {
            match self.child_stop_run(owner, bot, run, device) {
                Ok(true) => {
                    self.children.stops.remove(&run);
                }
                Ok(false) => {}
                Err(error) => self.trace(run, "child_stop_pending", format_args!("{error}")),
            }
        }
    }

    /// Whether any child Stop is still owed.
    pub fn child_stops_owed(&self) -> bool {
        !self.children.stops.is_empty()
    }

    /// One Stop of one child Run, exactly as a person's Stop: the current
    /// fence when the Run has a session, none while it is only queued. A
    /// queued child is settled without starting. `Ok(true)` once the child's
    /// admission is settled or refused.
    fn child_stop_run(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        run: RunId,
        device: DeviceId,
    ) -> Result<bool, RunError> {
        let ended = |state: BotAdmissionState| {
            matches!(
                state,
                BotAdmissionState::Settled | BotAdmissionState::Refused
            )
        };
        let Some(record) = self.bot_ledger()?.bot_execution(run)? else {
            return Ok(true);
        };
        if record.owner != owner || record.bot != bot {
            // Never a Run this wait does not name.
            return Ok(true);
        }
        if ended(record.state) {
            return Ok(true);
        }
        // A queued child whose Run document was not created at the ask is
        // completed forward first, as dispatch would.
        self.child_complete_intent(run);
        let fence = self.bot_admission_view(&record)?.fence;
        let key = BotRunKey { owner, bot, run };
        if let Err(error) = self.bot_stop(&key, device, fence.as_ref()) {
            // Stop answers "pending" while its end is being proven; the
            // cleanup owner of that Run finishes it.
            self.trace(run, "child_stop_requested", format_args!("{error}"));
        }
        Ok(self
            .bot_ledger()?
            .bot_execution(run)?
            .is_none_or(|record| ended(record.state)))
    }
}
