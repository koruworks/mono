// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn startup_reloads_the_exact_seed_and_does_not_repeat_completed_effects() {
    let f = Fixture::new();
    f.gateway.recover_creations(|| false).unwrap();
    assert_eq!(f.read().phase, Phase::Completed);
    assert_eq!(f.store.writes.load(Ordering::Relaxed), 1);
    let origins = f.store.origins.lock().unwrap();
    assert_eq!(
        origins.values().next().unwrap().initial_update,
        vec![1, 2, 3]
    );
    drop(origins);
    let creates = f.fs.creates.load(Ordering::Relaxed);
    f.gateway.recover_creations(|| false).unwrap();
    assert_eq!(f.store.writes.load(Ordering::Relaxed), 1);
    assert_eq!(f.fs.creates.load(Ordering::Relaxed), creates);
}

#[test]
fn shutdown_leaves_unstarted_work_durable_and_explicit_failure_requires_retry() {
    let f = Fixture::new();
    f.gateway.recover_creations(|| true).unwrap();
    assert_eq!(f.read().phase, Phase::Intent);
    assert_eq!(f.fs.creates.load(Ordering::Relaxed), 0);
    f.store.lost.store(true, Ordering::Relaxed);
    f.gateway.recover_creations(|| false).unwrap();
    let failed = f.read();
    assert_eq!(failed.failure, Some(Failure::RoleUnavailable));
    f.gateway.recover_creations(|| false).unwrap();
    assert_eq!(f.read().revision, failed.revision);
    assert_eq!(f.store.writes.load(Ordering::Relaxed), 1);
}

#[test]
fn startup_keeps_scanning_after_short_byte_pages_and_an_unreadable_record() {
    let mut f = Fixture::new();
    let original = f.gateway.creation.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    let first = Uuid::from_u128(1);
    let second = Uuid::from_u128(2);
    let mut record = f.record.clone();
    record.request = first;
    f.gateway.creation = Arc::new(move |request| {
        match request.command {
            c::Command::Pending { after: None } => Ok(c::Reply::Pending(vec![record.clone()])),
            c::Command::Pending { after: Some(id) } if id == first => {
                let mut record = record.clone();
                record.request = second;
                Ok(c::Reply::Pending(vec![record]))
            }
            c::Command::Pending { after: Some(id) } => {
                assert_eq!(id, second);
                Ok(c::Reply::Pending(vec![]))
            }
            c::Command::Read { request } if request == first => {
                count.fetch_add(1, Ordering::Relaxed);
                Err(Error::StorageUnavailable)
            }
            c::Command::Read { request } if request == second => {
                count.fetch_add(1, Ordering::Relaxed);
                Ok(c::Reply::Record(None)) // Concurrent abandonment/retention.
            }
            _ => original(request),
        }
    });
    assert!(matches!(
        f.gateway.recover_creations(|| false),
        Err(Error::StorageUnavailable)
    ));
    assert_eq!(reads.load(Ordering::Relaxed), 2);
}
