// SPDX-License-Identifier: MPL-2.0

//! Cohort `m6_child_clock`: the 15-minute clock counts only time this
//! process ran, awake, with the parent Bot unpaused (contract 4.3). Both
//! clocks are the test's: the shared wall clock and the orchestrator's
//! monotonic override.

use super::*;
use crate::test_support::admission_barrier;
use koru_delegate_core::BotRequestId;

/// The last monotonic reading this test supplied.
fn mono(chain: &ChildChain) -> u64 {
    chain.orchestrator.children.clock.mono_override.unwrap_or(0)
}

/// Move both clocks by `wall` and `mono` milliseconds (no tick).
pub(super) fn shift(chain: &mut ChildChain, wall: i64, mono_ms: u64) {
    chain.clock.fetch_add(wall, Ordering::SeqCst);
    let next = mono(chain).saturating_add(mono_ms);
    chain.orchestrator.set_child_clock_monotonic(Some(next));
}

/// One tick of the child clocks at the current sample.
pub(super) fn tick(chain: &mut ChildChain) -> ChildClockTick {
    if chain.orchestrator.children.clock.mono_override.is_none() {
        chain.orchestrator.set_child_clock_monotonic(Some(0));
    }
    let sample = chain.orchestrator.child_clock_sample();
    let device = device(chain);
    chain
        .orchestrator
        .bot_child_clock_tick(device, sample)
        .expect("the clock ticks")
}

/// Count `ms` of ordinary running time: both clocks advance together in
/// steps no tick would call a gap, with a tick after each step.
pub(super) fn count(chain: &mut ChildChain, ms: u64) -> usize {
    let mut timed_out = tick(chain).timed_out;
    let mut left = ms;
    while left > 0 {
        let step = left.min(10_000);
        shift(chain, step as i64, step);
        timed_out += tick(chain).timed_out;
        left -= step;
    }
    timed_out
}

fn elapsed(chain: &mut ChildChain) -> u32 {
    requests(chain)[0].elapsed_ms
}

fn barrier(
    chain: &ChildChain,
    epoch: u64,
    paused: bool,
) -> koru_delegate_core::BotAdmissionBarrier {
    let mut barrier = admission_barrier(&chain.parent.submission);
    barrier.epoch = epoch;
    barrier.paused = paused;
    barrier.request = BotRequestId::from_uuid(uuid::Uuid::new_v4());
    barrier
}

#[test]
fn m6_child_clock_times_out_at_exactly_the_budget_of_counted_time() {
    let (mut chain, requested, _bridge) = held(&[1]);
    assert_eq!(count(&mut chain, BOT_CHILD_WAIT - 10_000), 0);
    let open = requests(&mut chain);
    assert_eq!(open[0].state, ChildRequestState::Open);
    assert_eq!(open[0].clock, ChildClock::Running);
    assert!(u64::from(open[0].elapsed_ms) >= BOT_CHILD_WAIT - 10_000);
    let next = tick(&mut chain).next_ms.expect("a running request");
    assert!(next <= 10_000, "{next}");
    assert_eq!(count(&mut chain, 10_000), 1);
    let settled = requests(&mut chain);
    assert_eq!(
        settled[0].child_request,
        requested.receipts[0].child_request
    );
    assert_eq!(settled[0].settled_by, Some(ChildSettledBy::TimedOut));
    assert_eq!(settled[0].clock, ChildClock::Stopped);
    assert_eq!(u64::from(settled[0].elapsed_ms), BOT_CHILD_WAIT);
}

#[test]
fn m6_child_clock_sleep_is_not_counted() {
    let (mut chain, _, _bridge) = held(&[1]);
    count(&mut chain, 100_000);
    let before = elapsed(&mut chain);
    // The computer sleeps for an hour: both clocks jump.
    shift(&mut chain, 3_600_000, 3_600_000);
    let after_gap = tick(&mut chain);
    assert!(after_gap.gap);
    assert_eq!(after_gap.timed_out, 0);
    let after = elapsed(&mut chain);
    assert!(
        after <= before + 10_000,
        "the sleep was counted: {before} -> {after}"
    );
    count(&mut chain, 50_000);
    let later = elapsed(&mut chain);
    assert!(
        later >= after + 40_000 && later <= after + 60_000,
        "{after} -> {later}"
    );
}

#[test]
fn m6_child_clock_wall_clock_set_back_expires_nothing() {
    let (mut chain, _, _bridge) = held(&[1]);
    count(&mut chain, 100_000);
    let before = elapsed(&mut chain);
    // The wall clock is set back ten minutes while five seconds pass.
    shift(&mut chain, -600_000, 5_000);
    let set_back = tick(&mut chain);
    assert!(set_back.gap);
    assert_eq!(requests(&mut chain)[0].state, ChildRequestState::Open);
    assert!(elapsed(&mut chain) <= before + 5_000);
    // And set forward a day: still a gap, nothing expires early.
    shift(&mut chain, 86_400_000, 5_000);
    assert_eq!(tick(&mut chain).timed_out, 0);
    assert_eq!(requests(&mut chain)[0].state, ChildRequestState::Open);
    assert!(u64::from(elapsed(&mut chain)) < BOT_CHILD_WAIT);
}

#[test]
fn m6_child_clock_paused_parent_is_not_counted_and_unpause_restarts_it() {
    let (mut chain, _, mut bridge) = held(&[1]);
    count(&mut chain, 60_000);
    let device = device(&chain);
    let pause = barrier(&chain, 2, true);
    chain.orchestrator.bot_barrier(&pause, device).unwrap();
    let paused = requests(&mut chain);
    assert_eq!(paused[0].clock, ChildClock::Paused);
    let at_pause = paused[0].elapsed_ms;
    // Twenty minutes pass while the parent Bot is paused: no timeout.
    assert_eq!(count(&mut chain, 1_200_000), 0);
    let still = requests(&mut chain);
    assert_eq!(still[0].state, ChildRequestState::Open);
    assert_eq!(still[0].elapsed_ms, at_pause);
    // A paused parent with nothing open is held, never refused (decision 14).
    let unpause = barrier(&chain, 3, false);
    chain.orchestrator.bot_barrier(&unpause, device).unwrap();
    let running = requests(&mut chain);
    assert_eq!(
        running[0].clock,
        ChildClock::Running,
        "restarted by the fold"
    );
    count(&mut chain, 30_000);
    let after = elapsed(&mut chain);
    assert!(after >= at_pause + 20_000, "{at_pause} -> {after}");
    let _ = advance(&mut chain, &mut bridge);
}
