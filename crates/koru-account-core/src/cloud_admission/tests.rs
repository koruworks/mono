// SPDX-License-Identifier: MPL-2.0

use super::*;

fn observed() -> Observation {
    Observation {
        binding: Binding {
            owner: Uuid::from_u128(1),
            workspace: Uuid::from_u128(2),
            incarnation: Uuid::from_u128(3),
            account_generation: 1,
            authority_generation: 1,
        },
        usage_revision: 7,
        policy_revision: 2,
        generation: Some(Uuid::from_u128(4)),
        observed_at_millis: 1_000,
        valid_until_millis: 61_000,
        allowance: CloudAllowance::Finite {
            base_bytes: 1_000,
            effective_bytes: 1_000,
        },
        read: true,
        ordinary_write: true,
        remove: true,
        complete_usage: true,
        used_bytes: Some(900),
        reserved_bytes: Some(0),
    }
}

#[test]
fn same_facts_and_timer_do_not_replay_quota_candidate() {
    let first = observed();
    let mut hold = Hold::new(
        Reason::AccountQuota,
        first.binding,
        Some(first),
        Some(150),
        1_000,
    );
    assert!(!hold.retry(Some(first), 2_000));
    let refreshed = Observation {
        observed_at_millis: 60_000,
        valid_until_millis: 120_000,
        ..first
    };
    assert!(!hold.retry(Some(refreshed), 60_000));
    assert!(!hold.retry(None, 180_000));
    let deletion = Observation {
        usage_revision: 8,
        used_bytes: Some(700),
        ..refreshed
    };
    assert!(hold.retry(Some(deletion), 60_000));
    assert!(!hold.retry(Some(deletion), 60_000));
}

#[test]
fn oversized_ignores_deletion_expiry_and_permissions_until_allowance_increases() {
    let first = observed();
    let mut hold = Hold::new(
        Reason::FileAllowance,
        first.binding,
        Some(first),
        Some(1_500),
        1_000,
    );
    let mut next = Observation {
        usage_revision: 99,
        policy_revision: 3,
        used_bytes: Some(0),
        reserved_bytes: Some(0),
        ordinary_write: false,
        ..first
    };
    assert!(!hold.retry(Some(next), 2_000));
    next.allowance = CloudAllowance::Finite {
        base_bytes: 2_000,
        effective_bytes: 2_000,
    };
    assert!(hold.retry(Some(next), 2_000));
    assert!(!hold.retry(Some(next), 2_000));
}

#[test]
fn unknown_requires_complete_current_generation_and_holds() {
    let first = observed();
    let incomplete = Observation {
        complete_usage: false,
        ..first
    };
    let mut hold = Hold::new(
        Reason::UsageUnknown,
        first.binding,
        Some(incomplete),
        Some(50),
        1_000,
    );
    assert!(!hold.retry(Some(incomplete), 2_000));
    assert!(!hold.retry(
        Some(Observation {
            reserved_bytes: None,
            ..first
        }),
        2_000
    ));
    assert!(!hold.retry(Some(first), 61_000));
    assert!(hold.retry(Some(first), 2_000));
    assert!(!hold.retry(Some(first), 2_000));
}

#[test]
fn wrong_owner_incarnation_and_generation_never_wake() {
    let first = observed();
    for binding in [
        Binding {
            owner: Uuid::from_u128(9),
            ..first.binding
        },
        Binding {
            incarnation: Uuid::from_u128(9),
            ..first.binding
        },
        Binding {
            account_generation: 2,
            ..first.binding
        },
    ] {
        let mut hold = Hold::new(
            Reason::AccountQuota,
            first.binding,
            Some(first),
            Some(150),
            1_000,
        );
        assert!(!hold.retry(
            Some(Observation {
                binding,
                usage_revision: 8,
                ..first
            }),
            2_000
        ));
    }
}

#[test]
fn resource_retry_is_once_per_minute_and_does_not_apply_to_quota() {
    let first = observed();
    for reason in [
        Reason::DocumentCount,
        Reason::StoredDocumentBytes,
        Reason::PhysicalBytes,
        Reason::StagingBytes,
        Reason::RemovalCapacity,
    ] {
        let mut hold = Hold::new(reason, first.binding, Some(first), Some(150), 1_000);
        assert!(!hold.retry(None, 60_999));
        assert!(hold.retry(None, 61_000));
        assert!(!hold.retry(None, 61_000));
        assert!(!hold.retry(None, 120_999));
        assert!(hold.retry(None, 121_000));
    }
}

#[test]
fn policy_time_boundary_can_wake_without_revision_bump() {
    let first = Observation {
        ordinary_write: false,
        ..observed()
    };
    let mut hold = Hold::new(
        Reason::PlanRequired,
        first.binding,
        Some(first),
        None,
        1_000,
    );
    assert!(hold.retry(
        Some(Observation {
            ordinary_write: true,
            ..first
        }),
        2_000
    ));
    assert!(Reason::from_code("feedback_due").is_none());
}
