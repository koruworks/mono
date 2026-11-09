// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_account_core::root_catalog::SubjectLifecycle;
use koru_agent_wire::AccountProfileCloud;

fn receipt(account: koru_account_core::KoruIdentityId) -> crate::cloud::DiscoveryReceipt {
    crate::cloud::DiscoveryReceipt {
        account_id: account,
        pass: 5,
        stale: false,
        exhausted: true,
        profiles: Default::default(),
        unassigned: Default::default(),
        failure_code: None,
        retry_at_millis: None,
        automatic_retry: false,
        scope: None,
        authorization_generation: None,
        coverage: None,
        lifecycles: Default::default(),
    }
}

#[test]
fn discovery_projects_empty_exhaustion_without_turning_it_into_sync_health() {
    let account = "10000000-0000-4000-8000-000000000001".parse().unwrap();
    let mut state = AccountState {
        active_scope: 7,
        active_account_id: Some(format!("{account}")),
        cloud_profiles: vec![AccountProfileCloud {
            profile_id: "20000000-0000-4000-8000-000000000001".into(),
            stage: CloudConnectionStage::Connecting,
            discovery: None,
        }],
        ..Default::default()
    };
    let mut snapshot = Snapshot {
        owner_scope: Some(7),
        account_id: Some(account),
        stage: Stage::Settled,
        discovery: Some(receipt(account)),
        ..Default::default()
    };
    project(&mut state, &snapshot);
    let receipt = state.cloud_profiles[0].discovery.as_ref().unwrap();
    assert_eq!((receipt.discovered, receipt.admitted), (0, 0));
    assert!(receipt.exhausted);
    assert_eq!(
        state.cloud_profiles[0].stage,
        CloudConnectionStage::Connecting
    );
    snapshot.discovery = None;
    project(&mut state, &snapshot);
    assert!(state.cloud_profiles[0].discovery.is_none());
}

/// Root-catalog Plan 3.2: the three answers a Profile row can carry are
/// distinguishable on the wire. Zero Workspaces under `complete` coverage
/// with a live lifecycle is *empty*; zero under `partial` is *pending*; a
/// Profile the account's catalog holds no unit for carries no lifecycle and
/// is *not syncing* — never empty.
#[test]
fn coverage_and_lifecycle_tell_empty_from_pending_from_not_syncing() {
    let account = "10000000-0000-4000-8000-000000000001".parse().unwrap();
    let live = koru_account_core::ProfileId::parse("20000000-0000-4000-8000-000000000001").unwrap();
    let off = koru_account_core::ProfileId::parse("20000000-0000-4000-8000-000000000002").unwrap();
    let mut complete = receipt(account);
    complete.scope = Some(koru_account_wire::CloudDiscoveryScopeV2::Workspaces);
    complete.authorization_generation = Some(9);
    complete.coverage = Some(crate::cloud::Coverage::Complete);
    complete.lifecycles.insert(live, SubjectLifecycle::Live);
    complete.unassigned.discovered = 2;

    let empty = complete.for_profile(live);
    assert_eq!(empty.coverage.as_deref(), Some("complete"));
    assert_eq!(empty.pending_units, 0);
    assert_eq!(empty.profile_lifecycle.as_deref(), Some("live"));
    assert_eq!((empty.discovered, empty.unassigned), (0, 2));
    assert_eq!(empty.scope.as_deref(), Some("workspaces"));
    assert_eq!(empty.authorization_generation, Some(9));

    let not_syncing = complete.for_profile(off);
    assert_eq!(not_syncing.profile_lifecycle, None);
    assert_eq!(not_syncing.coverage.as_deref(), Some("complete"));

    let mut partial = complete.clone();
    partial.coverage = Some(crate::cloud::Coverage::Partial { pending: 3 });
    let pending = partial.for_profile(live);
    assert_eq!(pending.coverage.as_deref(), Some("partial"));
    assert_eq!(pending.pending_units, 3);
    assert_eq!(pending.profile_lifecycle.as_deref(), Some("live"));
}

#[test]
fn unrelated_scope_cannot_clear_a_receipt_but_cleared_active_authority_does() {
    let account = "10000000-0000-4000-8000-000000000001".parse().unwrap();
    let mut receipt = receipt(account);
    receipt.stale = true;
    receipt.exhausted = false;
    receipt.failure_code = Some("session_revoked");
    let profile =
        koru_account_core::ProfileId::parse("20000000-0000-4000-8000-000000000001").unwrap();
    let mut state = AccountState {
        active_scope: 7,
        active_account_id: Some(format!("{account}")),
        cloud_profiles: vec![AccountProfileCloud {
            profile_id: profile.to_string(),
            stage: CloudConnectionStage::Retrying,
            discovery: Some(receipt.for_profile(profile)),
        }],
        ..Default::default()
    };
    let mut snapshot = Snapshot {
        owner_scope: Some(8),
        ..Default::default()
    };
    project(&mut state, &snapshot);
    assert!(state.cloud_profiles[0].discovery.is_some());
    snapshot.owner_scope = Some(7);
    project(&mut state, &snapshot);
    assert!(state.cloud_profiles[0].discovery.is_none());
}

#[test]
fn cloud_progress_is_account_scoped_and_never_reports_coverage() {
    let account = "10000000-0000-4000-8000-000000000001".parse().unwrap();
    let profile =
        koru_account_core::ProfileId::parse("20000000-0000-4000-8000-000000000001").unwrap();
    let mut state = AccountState {
        active_scope: 7,
        active_account_id: Some(format!("{account}")),
        cloud_profiles: vec![AccountProfileCloud {
            profile_id: profile.to_string(),
            stage: CloudConnectionStage::Connecting,
            discovery: None,
        }],
        ..Default::default()
    };
    let record =
        |stage: Stage, error_code: Option<&'static str>, placement| crate::cloud::StatusRecord {
            permit_id: uuid::Uuid::from_u128(1),
            profile_id: placement,
            account_id: account,
            account_generation: 1,
            dataset: koru_device::cloud::CloudReplicaPrincipalV1::Profile {
                profile_id: koru_device::cloud::CloudId::from_bytes(*profile.as_uuid().as_bytes()),
                lineage_id: koru_device::cloud::CloudId::from_bytes([1; 16]),
                logical_replica_id: koru_device::cloud::CloudId::from_bytes([2; 16]),
            },
            stage,
            error_code,
            pending_consent_ids: Vec::new(),
            bytes_held: 0,
            queued_jobs: 0,
        };
    let mut snapshot = Snapshot {
        owner_scope: Some(7),
        account_id: Some(account),
        stage: Stage::Settled,
        records: vec![record(Stage::Settled, None, Some(profile))],
        ..Default::default()
    };
    project(&mut state, &snapshot);
    assert_eq!(state.cloud_profiles[0].stage, CloudConnectionStage::Syncing);

    // A record under another Profile's placement — or under no placement
    // at all, an unassigned Workspace — says nothing about this Profile.
    snapshot.records = vec![record(Stage::Settled, None, None)];
    project(&mut state, &snapshot);
    assert_eq!(
        state.cloud_profiles[0].stage,
        CloudConnectionStage::Connecting
    );

    snapshot.records = vec![record(Stage::Offline, Some("cloud_offline"), Some(profile))];
    project(&mut state, &snapshot);
    assert_eq!(
        state.cloud_profiles[0].stage,
        CloudConnectionStage::Retrying
    );

    // Another account's snapshot never relabels this one.
    snapshot.account_id = Some("10000000-0000-4000-8000-000000000002".parse().unwrap());
    snapshot.records = vec![record(Stage::Settled, None, Some(profile))];
    project(&mut state, &snapshot);
    assert_eq!(
        state.cloud_profiles[0].stage,
        CloudConnectionStage::Retrying
    );
}
