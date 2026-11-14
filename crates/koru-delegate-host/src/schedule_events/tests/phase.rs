// SPDX-License-Identifier: MPL-2.0
use super::super::ScheduleEventAuthorityGate;
use koru_delegate_runtime::SchedulePhaseGate;
use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

#[test]
fn routine_phase_expired_entry_never_takes_the_ordinary_gate() {
    let gate = ScheduleEventAuthorityGate::new();
    assert!(gate.enter_until(Instant::now()).is_err());
    assert!(gate.enter().is_some());
}

#[test]
fn routine_phase_owned_and_ordinary_guards_share_one_exclusion_state() {
    let gate = ScheduleEventAuthorityGate::new();
    let ordinary = gate.enter().unwrap();
    assert!(
        gate.enter_until(Instant::now() + Duration::from_millis(1))
            .is_err()
    );
    drop(ordinary);
    let owned = gate
        .enter_until(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(
        gate.enter_until(Instant::now() + Duration::from_millis(1))
            .is_err()
    );
    drop(owned);
    assert!(gate.enter().is_some());
}

#[test]
fn routine_phase_stop_wakes_contended_entry_and_never_reopens_on_drop() {
    let gate = Arc::new(ScheduleEventAuthorityGate::new());
    let ordinary = gate.enter().unwrap();
    let other = gate.clone();
    let (ready, started) = mpsc::sync_channel(1);
    let (done, answer) = mpsc::sync_channel(1);
    let thread = std::thread::spawn(move || {
        ready.send(()).unwrap();
        done.send(
            other
                .enter_until(Instant::now() + Duration::from_secs(30))
                .is_err(),
        )
        .unwrap();
    });
    started.recv_timeout(Duration::from_secs(1)).unwrap();
    gate.stop();
    assert!(answer.recv_timeout(Duration::from_secs(1)).unwrap());
    drop(ordinary);
    assert!(gate.enter().is_none());
    assert!(
        gate.enter_until(Instant::now() + Duration::from_secs(1))
            .is_err()
    );
    thread.join().unwrap();
}

#[test]
fn routine_phase_owned_guard_retains_same_gate_after_original_handle_drops() {
    let gate = ScheduleEventAuthorityGate::new();
    let observer = gate.clone();
    let held = gate
        .enter_until(Instant::now() + Duration::from_secs(1))
        .unwrap();
    drop(gate);
    assert!(
        observer
            .enter_until(Instant::now() + Duration::from_millis(1))
            .is_err()
    );
    drop(held);
    assert!(observer.enter().is_some());
}
