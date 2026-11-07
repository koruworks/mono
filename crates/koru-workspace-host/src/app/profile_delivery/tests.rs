// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_agent_wire::{
    AccountProfileCloud, AccountProfileEnrollment, CloudConnectionStage, CloudDiscovery,
};

const ACCOUNT: &str = "10000000-0000-4000-8000-000000000001";
fn profile() -> ProfileId {
    ProfileId::parse("20000000-0000-4000-8000-000000000001").unwrap()
}
fn state() -> AccountState {
    AccountState {
        active_account_id: Some(ACCOUNT.into()),
        stage: AccountStage::SignedIn,
        ..Default::default()
    }
}
fn receipt() -> CloudDiscovery {
    CloudDiscovery {
        account_id: ACCOUNT.into(),
        pass: 1,
        stale: false,
        exhausted: true,
        discovered: 0,
        admitted: 0,
        deferred: 0,
        deferred_code: None,
        carried: 0,
        failure_code: None,
        retry_at_millis: None,
        automatic_retry: false,
    }
}
fn cloud(state: &mut AccountState, discovery: Option<CloudDiscovery>) {
    state.cloud_profiles = vec![AccountProfileCloud {
        profile_id: profile().to_storage(),
        stage: CloudConnectionStage::Syncing,
        discovery,
    }];
}
fn enroll(state: &mut AccountState, enrollment: EnrollmentState) {
    state.enrollments = vec![AccountProfileEnrollment {
        profile_id: profile().to_storage(),
        state: enrollment,
        failure_code: Some("enrollment_failed".into()),
        retry_at_millis: Some(10),
    }];
}
fn read(state: &AccountState) -> ProfileDelivery {
    ProfileDelivery::read(profile(), Some(&format!("koru:{ACCOUNT}")), Some(state))
}

#[test]
fn local_is_not_applicable_while_absent_unavailable_or_mismatched_accounts_are_unknown() {
    assert_eq!(
        ProfileDelivery::read(profile(), None, None),
        ProfileDelivery::NotApplicable
    );
    assert_eq!(
        ProfileDelivery::read(profile(), Some(&format!("koru:{ACCOUNT}")), None),
        ProfileDelivery::Unknown
    );
    let mut state = state();
    enroll(&mut state, EnrollmentState::Failed);
    state.stage = AccountStage::Unavailable;
    assert_eq!(read(&state), ProfileDelivery::Unknown);
    state.stage = AccountStage::SignedIn;
    state.active_account_id = Some("another-account".into());
    assert_eq!(read(&state), ProfileDelivery::Unknown);
}

#[test]
fn enrollment_failure_and_work_precede_absent_choice_or_completed_discovery() {
    let mut state = state();
    for discovery in [None, Some(receipt())] {
        cloud(&mut state, discovery);
        enroll(&mut state, EnrollmentState::Failed);
        assert!(matches!(
            read(&state),
            ProfileDelivery::Failed {
                phase: FailurePhase::Enrollment,
                ..
            }
        ));
        for stage in [EnrollmentState::Waiting, EnrollmentState::Enrolling] {
            enroll(&mut state, stage);
            assert_eq!(
                read(&state),
                ProfileDelivery::InProgress {
                    enrollment: true,
                    counts: None
                }
            );
        }
    }
    state.cloud_profiles.clear();
    for stage in [EnrollmentState::Idle, EnrollmentState::Complete] {
        enroll(&mut state, stage);
        assert_eq!(read(&state), ProfileDelivery::NotApplicable);
    }
    state.enrollments.clear();
    assert_eq!(read(&state), ProfileDelivery::NotApplicable);
}

#[test]
fn accepted_choice_needs_a_receipt_for_the_bound_and_active_account() {
    let mut state = state();
    cloud(&mut state, None);
    assert_eq!(
        read(&state),
        ProfileDelivery::Unknown,
        "old Agent absence is not completion"
    );
    let mut discovery = receipt();
    discovery.account_id = "different-account".into();
    cloud(&mut state, Some(discovery));
    assert_eq!(read(&state), ProfileDelivery::Unknown);
    cloud(&mut state, Some(receipt()));
    state.active_account_id = Some("different-account".into());
    assert_eq!(read(&state), ProfileDelivery::Unknown);
}

#[test]
fn content_failure_precedes_every_stale_or_incomplete_count() {
    let mut state = state();
    for stale in [false, true] {
        for automatic in [false, true] {
            let mut discovery = receipt();
            discovery.stale = stale;
            discovery.exhausted = false;
            discovery.carried = 2;
            discovery.discovered = 3;
            discovery.failure_code = Some("cloud_access_revoked".into());
            discovery.retry_at_millis = Some(99);
            discovery.automatic_retry = automatic;
            cloud(&mut state, Some(discovery));
            assert_eq!(
                read(&state),
                ProfileDelivery::Failed {
                    phase: FailurePhase::Content,
                    code: Some("cloud_access_revoked".into()),
                    retry_at_millis: Some(99),
                    automatic
                }
            );
            assert!(!read(&state).may_ensure_live());
        }
    }
}

#[test]
fn each_incomplete_condition_waits_and_only_current_counts_are_presented() {
    let mut state = state();
    for variant in 0..4 {
        let mut discovery = receipt();
        match variant {
            0 => discovery.stale = true,
            1 => discovery.exhausted = false,
            2 => discovery.carried = 1,
            _ => discovery.discovered = 1,
        }
        cloud(&mut state, Some(discovery));
        let ProfileDelivery::InProgress { enrollment, counts } = read(&state) else {
            panic!("incomplete discovery cannot create")
        };
        assert!(!enrollment);
        assert_eq!(counts.is_none(), variant == 0);
        assert!(!read(&state).may_ensure_live());
    }
    for count in [0, 3] {
        let mut discovery = receipt();
        discovery.discovered = count;
        discovery.admitted = count;
        cloud(&mut state, Some(discovery));
        assert_eq!(read(&state), ProfileDelivery::Delivered);
        assert!(read(&state).may_ensure_live());
    }
}
