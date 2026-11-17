// SPDX-License-Identifier: MPL-2.0

//! One Bot Run asks other Bots and waits (M6, delegate IPC 19).
//!
//! The ledger owns every transition of a child request and of the parent's
//! wait. This owner supplies what a transition needs from the live runtime:
//! the parent's exact fence and folder claim at the ask, the proven stop of
//! the parent's origin attempt, the 15-minute clock of each request, the
//! parent's successor attempt and its continuation input, and the Stop that
//! reaches the children.
//!
//! Each step is its own function here. They follow the steps a decision wait
//! takes through `decisions/`, and they are copies, not calls: no decision
//! function is widened, and none of the decision path's Routine refusals is
//! reached from here.
//!
//! Nothing in this module holds or logs task text, a title, a path of a
//! child, or page content. A request is named by ids and closed tokens.

use std::collections::BTreeMap;

use super::bots::{owner_device, refused};
use super::{Orchestrator, RunError};
use koru_delegate_core::children::ChildRequestReceipt;
use koru_delegate_core::decisions::BotWaitId;
use koru_delegate_core::{BotId, BotOwner, ChildRequestId, IdempotencyKey, RunId};
use koru_delegate_sqlite::ChildWaitExecution;

mod authority;
mod cancel;
mod clock;
mod continuation;
mod driver;
mod launch;
mod origin_stop;
mod request;
mod reservation;
mod timeout;

#[cfg(test)]
mod tests;

pub use clock::{ChildClockSample, ChildClockTick};
pub use driver::ChildResumeStep;
pub use origin_stop::ChildEnrollmentBridge;

/// Counted host time a wait may spend meeting only errors, at a reservation
/// or with a reserved successor whose input is not acknowledged, before its
/// owner cleans it up (contract 4.5 and section 9).
pub const CHILD_STALL_LIMIT_MS: u64 = 120_000;

/// The one physical successor creation a child reservation allows, as this
/// process knows it. A reservation an earlier process made is recovered by
/// its creation key, never created a second time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChildLaunch {
    /// Reserved here; no creation has been asked for yet.
    Reserved(IdempotencyKey),
    /// The creation was asked for; its answer may have been lost.
    Dispatched(IdempotencyKey),
}

/// Working memory of every child wait this process drives. Nothing here is
/// durable and nothing here is authority: the ledger's rows are.
#[derive(Default)]
pub(crate) struct ChildState {
    /// Runs whose open child wait this process published for their tool
    /// classification, with their owner. Bounded by the open-wait cap.
    published: BTreeMap<RunId, BotOwner>,
    clock: clock::ChildClocks,
    launches: BTreeMap<RunId, ChildLaunch>,
    /// Waits that have only met errors, with the counted time at the first.
    stalled: BTreeMap<BotWaitId, u64>,
    /// The last revision of each request whose event this process published.
    events: BTreeMap<ChildRequestId, u64>,
    /// Child Runs whose Stop is still owed, with their owner and Bot.
    stops: BTreeMap<RunId, (BotOwner, BotId)>,
}

impl Orchestrator {
    /// The Run's open child wait, or `bot_fenced` when it has none.
    pub(super) fn child_ticket(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<ChildWaitExecution, RunError> {
        self.bot_ledger()?
            .child_wait_execution(owner, run)?
            .ok_or_else(|| refused("bot_fenced"))
    }

    /// Publish the event of every revision of these requests this process
    /// has not published yet. `requested` marks the ask's own commit. A bus
    /// refusal is traced and retried when the request is next observed.
    pub(super) fn child_publish(&mut self, receipts: &[ChildRequestReceipt], requested: bool) {
        for receipt in receipts {
            let revision = receipt.revision.get();
            if self
                .children
                .events
                .get(&receipt.child_request)
                .is_some_and(|published| *published >= revision)
            {
                continue;
            }
            let fact = crate::ChildRequestFact::of(receipt, requested);
            match self
                .ports
                .events
                .publish_child_request(fact.event_id(), &fact)
            {
                Ok(()) => {
                    self.children.events.insert(receipt.child_request, revision);
                }
                Err(_) => self.trace(
                    receipt.parent.run,
                    "child_request_event_unpublished",
                    format_args!("{}", revision),
                ),
            }
        }
    }

    /// Read the parent Run's requests and publish what changed since this
    /// process last looked: a settlement the child's own ending wrote, a
    /// pause, a cleanup. Best effort; a failed read publishes nothing.
    pub(super) fn child_publish_changes(&mut self, owner: BotOwner, run: RunId) {
        let now = self.ports.clock.now_ms();
        let read = self
            .bot_ledger()
            .and_then(|ledger| Ok(ledger.read_child_requests(owner, run, now)?));
        if let Ok(read) = read {
            self.child_publish(&read.requests, false);
        }
    }

    /// `ReadChildRequests`: the parent Run's requests in creation order and
    /// its open child wait. History only.
    pub fn bot_read_child_requests(
        &mut self,
        key: &koru_delegate_core::BotRunKey,
        device: koru_delegate_core::DeviceId,
    ) -> Result<koru_delegate_core::children::ChildRequests, RunError> {
        self.bot_record(key, device)?;
        let now = self.ports.clock.now_ms();
        Ok(self
            .bot_ledger()?
            .read_child_requests(key.owner, key.run, now)?)
    }

    /// `ReadBotChildWait`: the retained observation of one child wait, in any
    /// phase. `Ok(None)` only after the exact owner and Run read succeeded
    /// and found no such wait. Read-only; never permission to stop.
    pub fn bot_read_child_wait(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        run: RunId,
        wait: BotWaitId,
        device: koru_delegate_core::DeviceId,
    ) -> Result<Option<koru_delegate_core::children::BotChildWaitObservation>, RunError> {
        owner_device(owner, device)?;
        Ok(self
            .bot_ledger()?
            .read_bot_child_wait(owner, bot, run, wait)?)
    }
}
