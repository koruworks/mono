// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::sync::mpsc;

/// An `Abandoned` whose wedge only records why, and limits that never fire on
/// their own.
fn recorded() -> (Arc<Abandoned>, Arc<Mutex<Vec<Wedged>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let abandoned = Abandoned::new(8, Duration::from_secs(3600), move |why| {
        record.lock().unwrap().push(why);
    });
    (abandoned, seen)
}

/// Work that blocks until the test lets it go: the sender's drop releases it,
/// so no thread outlives the test binary's interest in it.
fn stuck() -> (mpsc::Sender<()>, impl FnOnce() -> u32 + Send + 'static) {
    let (release, wait) = mpsc::channel::<()>();
    (release, move || {
        let _ = wait.recv();
        7
    })
}

#[test]
fn quick_work_is_returned() {
    let (abandoned, _) = recorded();
    let cancel = AtomicBool::new(false);
    let outcome = bounded(
        "test-quick",
        Duration::from_secs(10),
        &cancel,
        &abandoned,
        || 42,
    );
    assert_eq!(outcome, Outcome::Done(42));
    assert_eq!(abandoned.alive(), 0);
}

/// **A call that never returns is abandoned at its budget**, not waited for.
#[test]
fn work_that_never_finishes_is_abandoned_at_its_budget() {
    let (abandoned, seen) = recorded();
    let cancel = AtomicBool::new(false);
    let (release, work) = stuck();
    let started = Instant::now();
    let outcome = bounded(
        "test-stuck",
        Duration::from_millis(100),
        &cancel,
        &abandoned,
        work,
    );
    let waited = started.elapsed();
    assert_eq!(outcome, Outcome::TimedOut);
    assert!(
        waited >= Duration::from_millis(100),
        "gave up early: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(5),
        "did not give up: {waited:?}"
    );
    assert_eq!(abandoned.alive(), 1);
    assert!(seen.lock().unwrap().is_empty());
    // Once it does finish, it is no longer counted.
    drop(release);
    let deadline = Instant::now() + Duration::from_secs(5);
    while abandoned.alive() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(abandoned.alive(), 0);
}

#[test]
fn work_that_panics_fails_without_taking_the_caller() {
    let (abandoned, _) = recorded();
    let cancel = AtomicBool::new(false);
    let outcome: Outcome<u32> = bounded(
        "test-panics",
        Duration::from_secs(10),
        &cancel,
        &abandoned,
        || panic!("a page the parser could not survive"),
    );
    assert_eq!(outcome, Outcome::Failed);
}

#[test]
fn a_cancel_ends_the_wait_at_once() {
    let (abandoned, _) = recorded();
    let cancel = AtomicBool::new(true);
    let (_release, work) = stuck();
    let started = Instant::now();
    let outcome = bounded(
        "test-cancel",
        Duration::from_secs(60),
        &cancel,
        &abandoned,
        work,
    );
    assert_eq!(outcome, Outcome::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// **The watchdog's first rule**: one stuck thread too many ends the process.
#[test]
fn too_many_abandoned_threads_at_once_wedge_the_worker() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let abandoned = Abandoned::new(1, Duration::from_secs(3600), move |why| {
        record.lock().unwrap().push(why);
    });
    let cancel = AtomicBool::new(false);
    let (_first, work) = stuck();
    assert_eq!(
        bounded(
            "test-one",
            Duration::from_millis(20),
            &cancel,
            &abandoned,
            work
        ),
        Outcome::TimedOut
    );
    assert!(seen.lock().unwrap().is_empty());
    let (_second, work) = stuck();
    assert_eq!(
        bounded(
            "test-two",
            Duration::from_millis(20),
            &cancel,
            &abandoned,
            work
        ),
        Outcome::TimedOut
    );
    assert_eq!(*seen.lock().unwrap(), vec![Wedged::TooMany]);
}

/// **The watchdog's second rule**: a stuck thread that outlives the ceiling
/// ends the process; one that finished in time does not.
#[test]
fn an_abandoned_thread_past_its_ceiling_wedges_the_worker() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let abandoned = Abandoned::new(8, Duration::from_millis(50), move |why| {
        record.lock().unwrap().push(why);
    });
    let finished_in_time = Arc::new(AtomicBool::new(false));
    abandoned.note(Arc::clone(&finished_in_time));
    finished_in_time.store(true, Ordering::Release);
    let still_running = Arc::new(AtomicBool::new(false));
    abandoned.note(still_running);
    let deadline = Instant::now() + Duration::from_secs(5);
    while seen.lock().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(*seen.lock().unwrap(), vec![Wedged::TooLong]);
}

#[test]
fn the_product_limits_are_pinned() {
    assert_eq!(PAGE_BUDGET, Duration::from_secs(30));
    assert_eq!(LOAD_BUDGET, Duration::from_secs(30));
    assert_eq!(MAX_ABANDONED_ALIVE, 2);
    assert_eq!(ABANDONED_CEILING, Duration::from_secs(120));
    assert_ne!(EXIT_WEDGED, 0);
}
