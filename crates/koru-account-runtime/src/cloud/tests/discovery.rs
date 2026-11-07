// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::cloud::coordinator::Coordinator;

fn coordinator(world: Arc<Mutex<World>>, cancel: Cancel) -> Coordinator<FakeApi, FakeHost> {
    Coordinator::new(
        FakeApi(world.clone()),
        FakeHost(world),
        Arc::new(Steps::default()),
        cancel,
    )
}

fn run(world: Arc<Mutex<World>>) -> DiscoveryReceipt {
    let clock = PauseClock::new();
    let mut c = coordinator(world, Cancel::default());
    c.apply(
        Command::Activate(Some(authority("1")), Some(7)),
        clock.as_ref(),
    );
    c.dispatch(clock.as_ref(), true);
    c.snapshot()
        .discovery
        .expect("settled pass publishes discovery")
}

#[test]
fn final_empty_page_is_exhaustion_and_empty_discovery_is_explicit() {
    let world = one_hosted_pair();
    lock(&world).pages = vec![
        page(vec![candidate(1)], Some(&candidate(1))),
        page(vec![], None),
    ];
    let receipt = run(world);
    assert!(receipt.exhausted);
    assert!(!receipt.stale);
    assert_eq!(receipt.account_id, account());
    let counts = receipt.for_profile(profile(1));
    assert_eq!(
        (
            counts.discovered,
            counts.admitted,
            counts.deferred,
            counts.carried
        ),
        (1, 1, 0, 0)
    );
    let empty = World::shared();
    lock(&empty).pages = vec![page(vec![], None)];
    let receipt = run(empty).for_profile(profile(99));
    assert!(receipt.exhausted);
    assert_eq!((receipt.discovered, receipt.admitted), (0, 0));
    assert!(receipt.failure_code.is_none());
}

#[test]
fn failed_admission_is_a_failure_receipt_even_after_exhaustion() {
    let world = one_hosted_pair();
    lock(&world).not_ready.insert(workspace(1));
    let receipt = run(world).for_profile(profile(1));
    assert!(receipt.exhausted);
    assert_eq!(
        (receipt.discovered, receipt.admitted, receipt.deferred),
        (1, 0, 1)
    );
    assert_eq!(receipt.failure_code.as_deref(), Some(DESCRIPTOR_NOT_READY));
    assert_eq!(receipt.deferred_code.as_deref(), Some(DESCRIPTOR_NOT_READY));
    assert!(receipt.automatic_retry);
    assert!(receipt.retry_at_millis.is_some());
}

#[test]
fn enumeration_failure_and_capacity_refusal_never_claim_exhaustion() {
    let world = one_hosted_pair();
    lock(&world).discover_failures = 1;
    let receipt = run(world);
    assert!(!receipt.exhausted);
    assert_eq!(receipt.failure_code, Some(CLOUD_OFFLINE));
    let world = World::shared();
    lock(&world).pages = vec![page(vec![candidate(1); MAX_CARRIED_CANDIDATES + 1], None)];
    let receipt = run(world);
    assert!(!receipt.exhausted);
    assert_eq!(receipt.failure_code, Some(CLOUD_UNAVAILABLE));
}

#[test]
fn pass_start_publishes_stale_and_abort_keeps_the_previous_receipt_stale() {
    let world = one_hosted_pair();
    let clock = PauseClock::new();
    let cancel = Cancel::default();
    let mut c = coordinator(world, cancel.clone());
    c.apply(
        Command::Activate(Some(authority("1")), Some(7)),
        clock.as_ref(),
    );
    c.dispatch(clock.as_ref(), true);
    let prior = c.snapshot().discovery.unwrap();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let collected = observed.clone();
    c.observe(move |snapshot| {
        collected.lock().unwrap().push(snapshot);
        cancel.request();
    });
    c.apply(Command::Wake, clock.as_ref());
    clock.advance(CHANGE_DEBOUNCE.as_millis() as u64);
    c.dispatch(clock.as_ref(), true);
    let receipt = c.snapshot().discovery.unwrap();
    assert!(receipt.stale);
    assert_eq!(receipt.pass, prior.pass);
    assert!(
        observed
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.revision > prior.pass && s.discovery.as_ref().is_some_and(|d| d.stale))
    );
    // Account/session replacement cannot carry the receipt into a new authority.
    c.apply(
        Command::Activate(Some(authority("2")), Some(7)),
        clock.as_ref(),
    );
    assert!(c.snapshot().discovery.is_none());
}

#[test]
fn terminal_failure_retains_nonautomatic_recovery() {
    let world = one_hosted_pair();
    lock(&world).discover_auth_failures = 1;
    let receipt = run(world).for_profile(profile(1));
    assert_eq!(receipt.failure_code.as_deref(), Some(SESSION_EXPIRED));
    assert!(!receipt.automatic_retry);
    assert!(receipt.retry_at_millis.is_none());
}
