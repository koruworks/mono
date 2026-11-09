// SPDX-License-Identifier: MPL-2.0

//! The discovery receipt: what a pass proved about the account's catalog,
//! grouped by current placement, with the v2 receipt's coverage and
//! lifecycles carried so a Profile row can say *empty*, *pending* or *not
//! syncing* and never confuse them (root-catalog TECHNICAL.final §7.3).

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
    {
        let mut state = lock(&world);
        let first = candidate_v2(1, Some(1));
        state.receipts = vec![
            receipt(
                vec![first],
                vec![live(1)],
                CloudCoverageV2::Complete,
                Some(&first),
            ),
            receipt(vec![], vec![], CloudCoverageV2::Complete, None),
        ];
    }
    let receipt = run(world);
    assert!(receipt.exhausted);
    assert!(!receipt.stale);
    assert_eq!(receipt.account_id, account());
    assert_eq!(receipt.coverage, Some(Coverage::Complete));
    assert_eq!(receipt.authorization_generation, Some(1));
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
    assert_eq!(counts.coverage.as_deref(), Some("complete"));
    assert_eq!(counts.profile_lifecycle.as_deref(), Some("live"));
    let empty = World::shared();
    lock(&empty).receipts = vec![receipt_page_empty()];
    let receipt = run(empty).for_profile(profile(99));
    assert!(receipt.exhausted);
    assert_eq!((receipt.discovered, receipt.admitted), (0, 0));
    assert!(receipt.failure_code.is_none());
    assert_eq!(
        receipt.profile_lifecycle, None,
        "no unit of it: not syncing"
    );
}

fn receipt_page_empty() -> CloudDiscoveryReceiptV2 {
    receipt(vec![], vec![], CloudCoverageV2::Complete, None)
}

/// The three answers a Profile row can carry, distinguishable from each
/// other and from absent content: a Cloud-off Profile has no admitted unit
/// (*not syncing*); a live Profile with nothing placed under it under
/// `complete` coverage is *empty*; the same under `partial` coverage is
/// *pending*, because a unit the projection has not read may still place
/// something under it. An unassigned Workspace groups under none of them.
#[test]
fn cloud_off_partial_and_absent_content_are_three_different_receipts() {
    let world = World::shared();
    {
        let mut state = lock(&world);
        state.receipts = vec![receipt(
            vec![candidate_v2(2, Some(2)), candidate_v2(3, None)],
            vec![profile_result(1, false, SubjectLifecycle::Live), live(2)],
            CloudCoverageV2::Complete,
            None,
        )];
        state
            .descriptors_v2
            .insert(workspace(2), descriptor_v2(2, Some(2)));
        state
            .descriptors_v2
            .insert(workspace(3), descriptor_v2(3, None));
    }
    let complete = run(world.clone());
    let empty = complete.for_profile(profile(1));
    assert_eq!((empty.discovered, empty.admitted), (0, 0));
    assert_eq!(empty.coverage.as_deref(), Some("complete"));
    assert_eq!(empty.pending_units, 0);
    assert_eq!(empty.profile_lifecycle.as_deref(), Some("live"));
    let not_syncing = complete.for_profile(profile(9));
    assert_eq!((not_syncing.discovered, not_syncing.admitted), (0, 0));
    assert_eq!(not_syncing.profile_lifecycle, None);
    let placed = complete.for_profile(profile(2));
    assert_eq!((placed.discovered, placed.admitted), (1, 1));
    assert_eq!(
        placed.unassigned, 1,
        "the unassigned Workspace is counted apart"
    );
    assert_eq!(complete.unassigned.discovered, 1);
    assert_eq!(complete.unassigned.admitted, 1);
    assert_eq!(
        complete
            .profiles
            .values()
            .map(|c| c.discovered)
            .sum::<u32>(),
        1,
        "an unassigned Workspace is under no Profile"
    );

    {
        let mut state = lock(&world);
        state.receipts[0].coverage = CloudCoverageV2::Partial {
            pending: vec![CloudUnitRefV2::WorkspaceCatalog {
                subject_id: workspace(4),
            }],
        };
    }
    let partial = run(world);
    let pending = partial.for_profile(profile(1));
    assert_eq!((pending.discovered, pending.admitted), (0, 0));
    assert_eq!(pending.coverage.as_deref(), Some("partial"));
    assert_eq!(
        pending.pending_units, 1,
        "not empty: a unit is still unread"
    );
    assert_eq!(pending.profile_lifecycle.as_deref(), Some("live"));
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
    lock(&world).receipts = vec![receipt_page(vec![
        candidate_v2(1, Some(1));
        MAX_CARRIED_CANDIDATES + 1
    ])];
    let receipt = run(world);
    assert!(!receipt.exhausted);
    assert_eq!(receipt.failure_code, Some(CLOUD_UNAVAILABLE));
}

fn receipt_page(results: Vec<CloudWorkspaceCandidateV2>) -> CloudDiscoveryReceiptV2 {
    receipt(results, vec![live(1)], CloudCoverageV2::Complete, None)
}

/// A receipt for another account, or another catalog scope than the one
/// this device bound, is refused rather than correlated.
#[test]
fn a_receipt_for_another_account_is_refused() {
    let world = one_hosted_pair();
    lock(&world).receipts[0].account_id = other_account();
    let receipt = run(world);
    assert!(!receipt.exhausted);
    assert_eq!(receipt.failure_code, Some(DATASET_MISMATCH));
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
    assert!(prior.exhausted, "the previous pass completed enumeration");
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
        receipt.exhausted,
        "stale exhaustion still belongs to the previous pass"
    );
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

#[test]
fn access_revocation_publishes_a_failure_receipt_without_automatic_retry() {
    let world = one_hosted_pair();
    lock(&world).discover_access_failures = 1;
    let receipt = run(world);
    assert_eq!(receipt.failure_code, Some("cloud_access_revoked"));
    assert!(!receipt.stale);
    assert!(
        !receipt.exhausted,
        "failed enumeration is not an empty completed catalog"
    );
    assert!(!receipt.automatic_retry);
    assert!(receipt.retry_at_millis.is_none());
    let projected = receipt.for_profile(profile(1));
    assert_eq!(
        projected.failure_code.as_deref(),
        Some("cloud_access_revoked")
    );
    assert!(!projected.automatic_retry);
    assert!(projected.retry_at_millis.is_none());
}

#[test]
fn cancelling_fresh_enumeration_never_publishes_an_exhausted_receipt() {
    // Cancel during the first non-final page, or during the final page's
    // return. Neither can publish completion for a fresh, aborted pass.
    for cancel_page in [0, 1] {
        let world = one_hosted_pair();
        let cancel = Cancel::default();
        {
            let mut world = lock(&world);
            let first = candidate_v2(1, Some(1));
            world.receipts = vec![
                receipt(
                    vec![first],
                    vec![live(1)],
                    CloudCoverageV2::Complete,
                    Some(&first),
                ),
                receipt(vec![], vec![], CloudCoverageV2::Complete, None),
            ];
            world.cancel_discovery_page = Some((cancel_page, cancel.clone()));
        }
        let clock = PauseClock::new();
        let mut c = coordinator(world.clone(), cancel.clone());
        let observed = Arc::new(Mutex::new(Vec::new()));
        let collected = observed.clone();
        c.observe(move |snapshot| collected.lock().unwrap().push(snapshot));
        c.apply(
            Command::Activate(Some(authority("1")), Some(7)),
            clock.as_ref(),
        );
        assert!(
            c.snapshot().discovery.is_none(),
            "there is no previous receipt to preserve"
        );
        c.dispatch(clock.as_ref(), true);
        assert!(
            cancel.cancelled(),
            "the cancellation happened inside discovery"
        );
        assert_eq!(
            lock(&world)
                .log
                .iter()
                .filter(|entry| entry.starts_with("discover"))
                .count(),
            cancel_page + 1
        );
        assert!(
            c.snapshot().discovery.is_none(),
            "an aborted fresh pass publishes no receipt, not exhausted=true"
        );
        let observed = observed.lock().unwrap();
        assert!(!observed.is_empty(), "the observer saw the active pass");
        assert!(
            observed.iter().all(|snapshot| snapshot.discovery.is_none()),
            "no intermediate notification advertised completion"
        );
    }
}
