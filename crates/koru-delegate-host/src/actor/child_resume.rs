// SPDX-License-Identifier: MPL-2.0

//! The child wait's per-turn driver and its clock, as the actor calls them
//! (M6, delegate IPC 19).
//!
//! Each maintenance turn offers exactly one Run with an open child wait to
//! the runtime's driver, rotating after the last wait offered so no Run
//! monopolizes the turn. The driver takes at most one exact step; the actor
//! only rotates and traces. Enrollment's HTTP call never runs here: the
//! driver reaches it through the fixed publication worker's mailbox.
//!
//! The clock tick is the actor's deadline: while any child wait is open it
//! runs at least every `TICK`, samples the wall clock and a monotonic clock,
//! and lets the runtime count, fold, time out and reach the children.

use std::time::{Duration, Instant};

use koru_delegate_runtime::{ChildClockSample, ChildClockTick, ChildResumeStep};
use koru_delegate_sqlite::ChildWaitCursor;

use super::{Actor, TICK};
use crate::publication::ChildEnrollmentMailbox;

/// What one maintenance turn of the child driver did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChildTurn {
    /// No Run has an open child wait.
    Idle,
    /// The open waits could not be listed; retried on a later turn.
    Unavailable,
    /// This Run was offered and nothing changed.
    Unchanged(koru_delegate_core::RunId),
    /// This Run took this one exact step.
    Stepped(koru_delegate_core::RunId, ChildResumeStep),
}

impl Actor {
    /// One maintenance turn of the child driver. A step taken lets the loop
    /// offer the next turn without waiting a tick.
    pub(super) fn advance_child_resume(&mut self) -> ChildTurn {
        if let Err(error) = self.ensure_publication_worker() {
            tracing::warn!(target: "koru::delegate", %error, "child resume has no publication worker");
            return ChildTurn::Unavailable;
        }
        let runs = match self
            .orchestrator
            .bot_child_resume_runs(self.device, self.child_resume_cursor)
        {
            Ok(runs) => runs,
            Err(error) => {
                // A ledger that does not model child requests has none.
                tracing::debug!(target: "koru::delegate", %error, "child resume selection unavailable");
                self.child_resume_pending = false;
                return ChildTurn::Unavailable;
            }
        };
        let Some(offered) = runs.first().copied() else {
            self.child_resume_pending = false;
            return ChildTurn::Idle;
        };
        self.child_resume_cursor = Some(ChildWaitCursor {
            created_at_ms: offered.cursor.created_at_ms,
            wait: offered.cursor.wait,
        });
        self.child_resume_pending = true;
        let Some(worker) = self.publication_worker.as_mut() else {
            return ChildTurn::Unavailable;
        };
        let turn = match self.orchestrator.bot_advance_child_resume(
            offered.owner,
            offered.run,
            &mut ChildEnrollmentMailbox::new(worker),
        ) {
            Ok(ChildResumeStep::Idle) => ChildTurn::Unchanged(offered.run),
            Ok(ChildResumeStep::Waiting(code)) => {
                tracing::debug!(target: "koru::delegate", run = ?offered.run, code, "child resume waiting");
                ChildTurn::Unchanged(offered.run)
            }
            Ok(step) => {
                tracing::debug!(target: "koru::delegate", run = ?offered.run, step = ?step, "child resume stepped");
                ChildTurn::Stepped(offered.run, step)
            }
            Err(error) => {
                tracing::warn!(target: "koru::delegate", run = ?offered.run, %error, "child resume step refused");
                ChildTurn::Unchanged(offered.run)
            }
        };
        #[cfg(test)]
        self.child_log.push(turn);
        if let ChildTurn::Stepped(run, _) = turn {
            // A step may have freed or taken a host slot, or ended a Run.
            self.notify(run);
            self.wake_bots();
            // The clock owner looks again now: a new wait, a cleanup.
            self.child_clock_at = Some(Instant::now());
        }
        turn
    }

    /// Republish every open child wait this process tracks, so an
    /// acknowledged resume lifts its fence. A failure keeps the previous,
    /// fenced value.
    pub(super) fn sync_child_authorities(&mut self) {
        if let Err(error) = self.orchestrator.sync_child_authorities() {
            tracing::warn!(target: "koru::delegate", %error, "child wait classification not refreshed");
        }
    }

    /// One tick of the child clocks, and when the next one is due: at the
    /// earliest remaining budget, and at least every `TICK` while any child
    /// wait is open or a child Stop is owed. `None` stops ticking until a
    /// command or a driver step arms it again.
    pub(super) fn child_clock_tick(&mut self) -> Option<Instant> {
        let sample: ChildClockSample = self.orchestrator.child_clock_sample();
        let tick = match self.orchestrator.bot_child_clock_tick(self.device, sample) {
            Ok(tick) => tick,
            Err(error) => {
                tracing::debug!(target: "koru::delegate", %error, "child clocks not ticked");
                // Unknown is not idle: look again one ordinary tick later,
                // but only while something may be open.
                return self.child_resume_pending.then(|| Instant::now() + TICK);
            }
        };
        #[cfg(test)]
        self.child_ticks.push(tick);
        self.after_child_tick(tick)
    }

    fn after_child_tick(&mut self, tick: ChildClockTick) -> Option<Instant> {
        if tick.timed_out > 0 {
            // A settled request may have made a parent resumable, and a
            // stopped child frees a slot.
            self.child_resume_pending = true;
            self.wake_bots();
        }
        if !tick.open && !self.orchestrator.child_stops_owed() {
            return None;
        }
        self.child_resume_pending |= tick.open;
        let wait = tick
            .next_ms
            .map(Duration::from_millis)
            .map_or(TICK, |left| left.min(TICK));
        Some(Instant::now() + wait.max(Duration::from_millis(1)))
    }

    /// A clean stop folds every running clock first, so the time this
    /// process counted is not lost with it (contract 4.3).
    pub(super) fn fold_child_clocks_for_stop(&mut self) {
        if let Err(error) = self.orchestrator.bot_fold_child_clocks(self.device) {
            tracing::debug!(target: "koru::delegate", %error, "child clocks not folded at stop");
        }
    }
}
