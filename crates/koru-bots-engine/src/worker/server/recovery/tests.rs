// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::operations::tests::leased_fixture;
use koru_store_core::{NativeEffectDrainReceipt, Timestamp};
use std::sync::{
    Condvar, Mutex,
    atomic::{AtomicUsize, Ordering},
};

fn settle(jobs: &mut Jobs, db: &mut Database, now: Instant) {
    let until = Instant::now() + Duration::from_secs(3);
    while !jobs.pending.is_empty() && Instant::now() < until {
        jobs.poll(db, now);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(jobs.pending.is_empty());
}
fn drain(request: RecoveryRequest) -> RecoveryReply {
    let RecoveryRequest::Fence { record, .. } = request else {
        panic!("fence first")
    };
    RecoveryReply::Drained(NativeEffectDrainReceipt {
        binding: record.store,
        fenced_at: Timestamp::from_millis(1),
        drained_at: Timestamp::from_millis(2),
    })
}
#[test]
fn pending_io_is_bounded_and_repeated_polls_do_not_duplicate_it() {
    let (mut db, _) = leased_fixture(12);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let held = Arc::clone(&gate);
    let calls = Arc::new(AtomicUsize::new(0));
    let started = Arc::clone(&calls);
    let mut jobs = Jobs::with_call(Arc::new(move |_| {
        started.fetch_add(1, Ordering::SeqCst);
        let (lock, ready) = &*held;
        let mut open = lock.lock().unwrap();
        while !*open {
            open = ready.wait(open).unwrap();
        }
        Ok(RecoveryReply::Pending)
    }));
    let now = Instant::now();
    jobs.poll(&mut db, now);
    assert_eq!(jobs.pending.len(), 8);
    for _ in 0..10 {
        jobs.poll(&mut db, now + Duration::from_secs(2));
    }
    assert_eq!(jobs.pending.len(), 8);
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    settle(&mut jobs, &mut db, now);
    assert_eq!(calls.load(Ordering::SeqCst), 8);
}
#[test]
fn failed_barrier_is_not_unconfirmed_and_lookup_follows_durable_commit() {
    let (mut db, records) = leased_fixture(1);
    let mut failed = Jobs::with_call(Arc::new(|_| Err(Error::StorageUnavailable)));
    let now = Instant::now();
    failed.poll(&mut db, now);
    settle(&mut failed, &mut db, now);
    assert!(
        db.recovery_candidates(0, 8).unwrap()[0]
            .1
            .recovery
            .drain
            .is_none()
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let called = Arc::clone(&calls);
    let mut jobs = Jobs::with_call(Arc::new(move |request| {
        called.fetch_add(1, Ordering::SeqCst);
        Ok(match request {
            request @ RecoveryRequest::Fence { .. } => drain(request),
            RecoveryRequest::Lookup { record } => {
                assert!(record.recovery.drain.is_some());
                RecoveryReply::Evidence(None)
            }
        })
    }));
    db.connection.execute_batch("CREATE TEMP TRIGGER reject_barrier BEFORE UPDATE ON operations BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let now = Instant::now();
    jobs.poll(&mut db, now);
    let until = Instant::now() + Duration::from_secs(3);
    while jobs.pending[0].result.is_none() && Instant::now() < until {
        jobs.poll(&mut db, now);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(jobs.pending[0].result.is_some());
    for _ in 0..8 {
        jobs.poll(&mut db, now);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        db.recovery_candidates(0, 8).unwrap()[0]
            .1
            .recovery
            .drain
            .is_none()
    );
    db.connection
        .execute_batch("DROP TRIGGER reject_barrier;")
        .unwrap();
    settle(&mut jobs, &mut db, now);
    assert!(
        db.recovery_candidates(0, 8).unwrap()[0]
            .1
            .recovery
            .drain
            .is_some()
    );
    jobs.poll(&mut db, now + Duration::from_secs(2));
    settle(&mut jobs, &mut db, now);
    assert!(db.recovery_candidates(0, 8).unwrap().is_empty());
    assert_eq!(
        db.connection
            .query_row(
                "SELECT state FROM operations WHERE id=?1",
                [records[0].key.operation.to_string()],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "unresolved"
    );
}
