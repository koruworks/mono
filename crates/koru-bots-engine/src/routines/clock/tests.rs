// SPDX-License-Identifier: MPL-2.0
use super::*;
#[test]
fn retry_retains_scan_and_does_not_read_until_retry_deadline() {
    let cursor = Uuid::new_v4();
    let now = Instant::now();
    let mut clock = Clock {
        zone: SystemZone::utc(),
        next: None,
        scan: Some(Some(cursor)),
        changed: false,
        retry_after: None,
    };
    clock.retry(now);
    assert_eq!(clock.scan, Some(Some(cursor)));
    let mut db = Database::in_memory().unwrap();
    // An invalid table proves the delayed/idle path performs no SQL.
    db.connection
        .execute("DROP TABLE routine_occurrences", [])
        .unwrap();
    assert!(!clock.poll(&mut db, 0, now).unwrap());
    assert!(
        clock
            .poll(&mut db, 1_000, now + Duration::from_secs(1))
            .is_err()
    );
}
#[test]
fn idle_ticks_never_reopen_or_scan_storage() {
    let now = Instant::now();
    let mut clock = Clock {
        zone: SystemZone::utc(),
        next: None,
        scan: None,
        changed: false,
        retry_after: None,
    };
    let mut db = Database::in_memory().unwrap();
    db.connection
        .execute("DROP TABLE routine_occurrences", [])
        .unwrap();
    assert!(!clock.poll(&mut db, 0, now).unwrap());
}
