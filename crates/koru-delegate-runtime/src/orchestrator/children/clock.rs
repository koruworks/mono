// SPDX-License-Identifier: MPL-2.0

//! The 15-minute clock of each child request, as this process counts it
//! (contract 4.3).
//!
//! What counts is time during which the delegate is running, the computer is
//! awake and the parent Bot is not paused. The ledger stores each clock as an
//! amount already counted and the wall time its running interval began, and
//! reports elapsed time on the wall clock. This owner makes that report equal
//! counted time: on every tick it samples the wall clock and a monotonic
//! clock, and whenever the two disagree about the interval it writes one fold
//! that counts only what both agree on and restarts the interval at the new
//! sample. A sleep, a wall clock set back or forward, and the time before
//! this process started are therefore never counted. Samples are never
//! written; a fold is a write on a transition.
//!
//! The owner holds no per-request state. A request is timed out when the
//! ledger's own elapsed time, after any fold, has reached the budget.

use super::*;
use koru_delegate_core::DeviceId;
use koru_delegate_core::children::{BOT_CHILD_WAIT_MS, ChildClock, ChildRequestState};
use koru_delegate_core::decisions::BotWaitPhase;
use koru_delegate_sqlite::{CHILD_RESUME_RUN_PAGE, ChildWaitCursor};

/// An interval longer than this on either clock is a gap (a sleep, a stall):
/// it is not counted.
pub const CHILD_CLOCK_GAP_MS: u64 = 15_000;
/// The two clocks may differ by this much over one interval before the
/// difference is folded away.
pub const CHILD_CLOCK_DRIFT_MS: u64 = 1_000;
/// Pages of open child waits one tick reads. 50 open waits per computer fit
/// in four; the bound only stops an unexpected ledger from looping.
const TICK_PAGES: usize = 8;

/// One reading of both clocks, taken by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildClockSample {
    /// The wall clock, milliseconds since the epoch.
    pub wall_ms: i64,
    /// A monotonic clock, milliseconds since any fixed origin of this process.
    pub mono_ms: u64,
}

/// What one tick found and did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChildClockTick {
    /// Whether any child wait is open on this computer. While true the
    /// caller keeps ticking at least every `TICK`.
    pub open: bool,
    /// Counted time until the earliest running request reaches its budget.
    pub next_ms: Option<u64>,
    /// Requests this tick settled `timed_out`.
    pub timed_out: usize,
    /// Whether the interval since the previous sample was a gap.
    pub gap: bool,
}

/// The owner's memory: the previous sample and the counted time of this
/// process. Nothing per request.
#[derive(Default)]
pub(crate) struct ChildClocks {
    last: Option<ChildClockSample>,
    /// Counted host time since this process first sampled, for the rule that
    /// ends a wait which only meets errors.
    counted_ms: u64,
    /// Whether the last tick found an open wait.
    open: bool,
    /// A monotonic reading supplied by a host that drives time itself (a
    /// test clock). `None` reads the process's own monotonic clock.
    pub(super) mono_override: Option<u64>,
}

/// What an interval between two samples is worth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Interval {
    /// Both clocks agree: this much is counted and nothing is written.
    Counted(u64),
    /// The clocks disagree by more than the drift bound: only the smaller
    /// difference is counted, and the clocks are folded to it.
    Folded(u64),
    /// A gap: nothing is counted.
    Gap,
}

pub(crate) fn interval(last: ChildClockSample, now: ChildClockSample) -> Interval {
    let wall = now.wall_ms.checked_sub(last.wall_ms);
    let mono = now.mono_ms.checked_sub(last.mono_ms);
    let (Some(wall), Some(mono)) = (wall, mono) else {
        return Interval::Gap;
    };
    // A wall clock that went backward is a gap, whatever the other says.
    let Ok(wall) = u64::try_from(wall) else {
        return Interval::Gap;
    };
    if wall > CHILD_CLOCK_GAP_MS || mono > CHILD_CLOCK_GAP_MS {
        return Interval::Gap;
    }
    let counted = wall.min(mono);
    if wall.abs_diff(mono) > CHILD_CLOCK_DRIFT_MS {
        Interval::Folded(counted)
    } else {
        Interval::Counted(counted)
    }
}

impl ChildClocks {
    /// An ask was admitted. If no wait was open the previous sample is stale
    /// (nothing ticked meanwhile), so it is dropped: the next tick starts a
    /// fresh interval instead of seeing a gap.
    pub(super) fn started(&mut self, _wall_ms: i64) {
        if !self.open {
            self.last = None;
        }
        self.open = true;
    }
    pub(super) fn counted_ms(&self) -> u64 {
        self.counted_ms
    }
}

impl Orchestrator {
    /// A sample of both clocks: the wall clock of the clock port, and the
    /// process's own monotonic clock unless a host supplied one.
    pub fn child_clock_sample(&mut self) -> ChildClockSample {
        static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let origin = *ORIGIN.get_or_init(std::time::Instant::now);
        ChildClockSample {
            wall_ms: self.ports.clock.now_ms(),
            mono_ms: self
                .children
                .clock
                .mono_override
                .unwrap_or_else(|| u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX)),
        }
    }

    /// Supply the monotonic reading of every later sample (`Some`), or go
    /// back to the process's own clock (`None`). For a host that drives time
    /// itself; the wall clock is the clock port's either way.
    pub fn set_child_clock_monotonic(&mut self, mono_ms: Option<u64>) {
        self.children.clock.mono_override = mono_ms;
    }

    /// One tick of the child clocks of this computer: account for the
    /// interval since the previous sample, restart what a pause or an older
    /// process left stopped, time out every request whose counted time
    /// reached the budget, publish what changed, and reach the children of a
    /// parent that is being cleaned up.
    pub fn bot_child_clock_tick(
        &mut self,
        device: DeviceId,
        sample: ChildClockSample,
    ) -> Result<ChildClockTick, RunError> {
        let boot = self.delegate_boot();
        let mut tick = ChildClockTick::default();
        // Selection first: with no open wait nothing is written at all.
        let waits = self.child_open_waits(device)?;
        tick.open = !waits.is_empty();
        let previous = self.children.clock.last.replace(sample);
        self.children.clock.open = tick.open;
        if !tick.open {
            self.children.events.clear();
            self.children.stalled.clear();
            self.child_retry_stops(device);
            return Ok(tick);
        }
        let mut folded = Vec::new();
        match previous {
            // The first sample of this process: adopt every clock an older
            // process left running, with its stored time. The time it did
            // not fold is lost, never invented (A11).
            None => {
                folded = self.bot_ledger()?.fold_child_clocks(
                    device,
                    boot,
                    sample.wall_ms,
                    sample.wall_ms,
                )?
            }
            Some(last) => match interval(last, sample) {
                Interval::Counted(counted) => {
                    self.children.clock.counted_ms =
                        self.children.clock.counted_ms.saturating_add(counted);
                }
                Interval::Folded(counted) => {
                    self.children.clock.counted_ms =
                        self.children.clock.counted_ms.saturating_add(counted);
                    let until = last
                        .wall_ms
                        .saturating_add(i64::try_from(counted).unwrap_or(i64::MAX));
                    folded = self.bot_ledger()?.fold_child_clocks(
                        device,
                        boot,
                        until,
                        sample.wall_ms,
                    )?;
                }
                Interval::Gap => {
                    tick.gap = true;
                    // Fold each running clock up to the earlier sample and
                    // restart it at the later one.
                    folded = self.bot_ledger()?.fold_child_clocks(
                        device,
                        boot,
                        last.wall_ms,
                        sample.wall_ms,
                    )?;
                }
            },
        }
        self.child_publish(&folded, false);
        // One pass over the open waits: what is due, what is paused under a
        // parent that is no longer paused, and what changed.
        let mut seen = std::collections::BTreeSet::new();
        let mut due: Vec<(BotOwner, Vec<ChildRequestId>)> = Vec::new();
        let mut unpaused = false;
        for wait in &waits {
            let read = self
                .bot_ledger()?
                .read_child_requests(wait.owner, wait.run, sample.wall_ms);
            let read = match read {
                Ok(read) => read,
                Err(error) => {
                    self.trace(
                        wait.run,
                        "child_requests_unreadable",
                        format_args!("{error:?}"),
                    );
                    continue;
                }
            };
            let mine: Vec<_> = read
                .requests
                .into_iter()
                .filter(|receipt| receipt.wait == wait.wait)
                .collect();
            self.child_publish(&mine, false);
            for receipt in &mine {
                seen.insert(receipt.child_request);
                if receipt.state != ChildRequestState::Open {
                    continue;
                }
                match receipt.clock {
                    ChildClock::Running => {
                        let left = BOT_CHILD_WAIT_MS.saturating_sub(u64::from(receipt.elapsed_ms));
                        if left == 0 {
                            match due.iter_mut().find(|(owner, _)| *owner == wait.owner) {
                                Some((_, ids)) => ids.push(receipt.child_request),
                                None => due.push((wait.owner, vec![receipt.child_request])),
                            }
                        } else {
                            tick.next_ms = Some(tick.next_ms.map_or(left, |old| old.min(left)));
                        }
                    }
                    ChildClock::Paused => {
                        unpaused |= !self
                            .bot_ledger()?
                            .bot_barrier(receipt.parent.owner, receipt.parent.bot)?
                            .is_some_and(|barrier| barrier.paused);
                    }
                    ChildClock::Stopped => {}
                }
            }
            // A parent that is being cleaned up: its children are stopped.
            if wait.phase == BotWaitPhase::CleanupPending {
                self.child_cancel_children(wait.owner, wait.run, wait.wait);
            }
        }
        self.children.events.retain(|id, _| seen.contains(id));
        if unpaused {
            // A paused clock restarts only through a fold, once its parent
            // Bot is no longer paused (ledger correction 10).
            let restarted = self.bot_ledger()?.fold_child_clocks(
                device,
                boot,
                sample.wall_ms,
                sample.wall_ms,
            )?;
            self.child_publish(&restarted, false);
            tick.next_ms = Some(tick.next_ms.map_or(1, |old| old.min(1)));
        }
        for (owner, ids) in due {
            tick.timed_out += self.child_time_out(owner, &ids, device)?;
        }
        self.child_retry_stops(device);
        Ok(tick)
    }

    /// Fold every running child clock now, before a transition that reads
    /// the wall clock (a pause) or needs a paused clock restarted (an
    /// unpause, a clean stop). With no open wait this writes nothing.
    pub fn bot_fold_child_clocks(&mut self, device: DeviceId) -> Result<(), RunError> {
        let sample = self.child_clock_sample();
        let boot = self.delegate_boot();
        let Some(last) = self.children.clock.last else {
            // Nothing was sampled yet in this process: adopt now, so a pause
            // never counts the time an older process was not running.
            self.children.clock.last = Some(sample);
            let folded = self.bot_ledger()?.fold_child_clocks(
                device,
                boot,
                sample.wall_ms,
                sample.wall_ms,
            )?;
            self.child_publish(&folded, false);
            return Ok(());
        };
        if !self.children.clock.open {
            return Ok(());
        }
        let until = match interval(last, sample) {
            Interval::Counted(counted) | Interval::Folded(counted) => {
                self.children.clock.counted_ms =
                    self.children.clock.counted_ms.saturating_add(counted);
                last.wall_ms
                    .saturating_add(i64::try_from(counted).unwrap_or(i64::MAX))
            }
            Interval::Gap => last.wall_ms,
        };
        self.children.clock.last = Some(sample);
        let folded = self
            .bot_ledger()?
            .fold_child_clocks(device, boot, until, sample.wall_ms)?;
        self.child_publish(&folded, false);
        Ok(())
    }

    /// Every open child wait of this computer, oldest first, bounded.
    pub(super) fn child_open_waits(
        &mut self,
        device: DeviceId,
    ) -> Result<Vec<koru_delegate_sqlite::ChildWaitRun>, RunError> {
        let mut waits = Vec::new();
        let mut after: Option<ChildWaitCursor> = None;
        for _ in 0..TICK_PAGES {
            let page = self.bot_ledger()?.child_resume_runs(
                device,
                after.as_ref(),
                CHILD_RESUME_RUN_PAGE,
            )?;
            let full = page.len() == CHILD_RESUME_RUN_PAGE;
            after = page.last().map(|wait| wait.cursor);
            waits.extend(page);
            if !full {
                return Ok(waits);
            }
        }
        Err(refused("bot_child_inventory_unbounded"))
    }
}
