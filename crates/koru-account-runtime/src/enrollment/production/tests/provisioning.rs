// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::identity::AccountOwner;
use koru_account_wire::{AccessToken, AuthIdentitySummaryV1, ProfileSummaryV1, SessionSummaryV1};
use koru_store_wire::{
    ProfileLifecycleInventory, ProfileLifecycleInventoryItem, ProvisionAccountProfile,
};

struct Local {
    rows: Mutex<Vec<ProfileLifecycleInventoryItem>>,
    writes: Mutex<Vec<ProvisionAccountProfile>>,
    fail: AtomicBool,
}
impl Local {
    fn empty() -> Self {
        Self {
            rows: Mutex::new(vec![]),
            writes: Mutex::new(vec![]),
            fail: AtomicBool::new(false),
        }
    }
}
impl LocalProfileInventory for Local {
    fn inventory(&self) -> Result<ProfileLifecycleInventory, crate::account::ServiceError> {
        Ok(ProfileLifecycleInventory {
            profiles: self.rows.lock().unwrap().clone(),
        })
    }
    fn provision(
        &self,
        profile: ProfileId,
        initial: &ProvisionAccountProfile,
    ) -> Result<(), crate::account::ServiceError> {
        if self.fail.load(Ordering::Acquire) {
            return Err(crate::account::ServiceError::retryable(
                "fixture_store_down",
            ));
        }
        self.writes.lock().unwrap().push(initial.clone());
        let mut rows = self.rows.lock().unwrap();
        if !rows.iter().any(|row| row.profile_id == profile) {
            rows.push(ProfileLifecycleInventoryItem {
                profile_id: profile,
                account_binding: format!("koru:{}", initial.identity_id),
                removed_at_millis: None,
            });
        }
        Ok(())
    }
}

struct HostedProjection {
    session: SessionSummaryV1,
    profiles: Vec<ProfileSummaryV1>,
    devices: Vec<koru_account_wire::DeviceSummaryV1>,
    receipt_host: Arc<Hosted>,
}
impl Transport for HostedProjection {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let body = if request.path_and_query.ends_with("/session") {
            serde_json::to_vec(&self.session).unwrap()
        } else if request.path_and_query.ends_with("/profiles") {
            serde_json::to_vec(&koru_account_wire::ProfileListResponseV1 {
                version: Version1,
                profiles: self.profiles.clone(),
            })
            .unwrap()
        } else if request.path_and_query.ends_with("/devices") {
            serde_json::to_vec(&koru_account_wire::DeviceListResponseV1 {
                version: Version1,
                devices: self.devices.clone(),
                truncated: false,
            })
            .unwrap()
        } else {
            return self.receipt_host.send(request);
        };
        Ok(HttpResponse {
            document_removal: false,
            status: 200,
            body,
        })
    }
}
fn authority() -> EnrollmentAuthorization {
    EnrollmentAuthorization {
        access: AccessToken("fixture-access".into()),
        namespace: "a".repeat(64),
        record_version: 1,
        session_id: "40000000-0000-4000-8000-000000000001".into(),
        owner: AccountOwner {
            account: "30000000-0000-4000-8000-000000000001".parse().unwrap(),
            auth_identity: "60000000-0000-4000-8000-000000000001".parse().unwrap(),
        },
    }
}
fn projection(f: &Fixture) -> HostedProjection {
    let credential = authority();
    HostedProjection {
        devices: vec![],
        session: SessionSummaryV1 {
            version: Version1,
            session_id: credential.session_id.parse().unwrap(),
            koru_identity_id: credential.owner.account,
            auth_identity_id: credential.owner.auth_identity,
            device_id: Some(f.joining.device_id),
            expires_at_millis: EXPIRES,
            recent_auth_at_millis: NOW,
        },
        profiles: vec![ProfileSummaryV1 {
            version: Version1,
            koru_identity_id: credential.owner.account,
            profile_id: f.profile,
            name_snapshot: Some("Work".into()),
            locale_snapshot: Some("en".into()),
            auth_identities: vec![AuthIdentitySummaryV1 {
                version: Version1,
                auth_identity_id: credential.owner.auth_identity,
                provider: koru_account_core::AuthProvider::Google,
                verified_email_snapshot: None,
            }],
            lifecycle_revision: Default::default(),
        }],
        receipt_host: f.hosted.clone(),
    }
}
fn client(hosted: HostedProjection) -> AccountClient {
    AccountClient::with_transport(Arc::new(hosted))
}
fn run(
    f: &Fixture,
    prepared: &PreparedEnrollmentV1,
    client: &AccountClient,
    local: &Local,
    check: impl Fn() -> Result<(), Failure>,
) -> Result<(), Failure> {
    super::super::provisioning::provision(
        client,
        local,
        f.profile,
        f.joining.device_id,
        prepared,
        &authority(),
        NOW,
        check,
    )
}

#[test]
fn hosted_completion_waits_for_local_profile_commit_and_exact_retry_repairs_it() {
    let f = Fixture::new();
    let prepared = f.prepared();
    let client = client(projection(&f));
    let local = Local::empty();
    local.fail.store(true, Ordering::Release);
    assert_eq!(
        run(&f, &prepared, &client, &local, || Ok(())),
        Err(Failure::Persistence)
    );
    assert!(local.rows.lock().unwrap().is_empty());
    local.fail.store(false, Ordering::Release);
    assert_eq!(run(&f, &prepared, &client, &local, || Ok(())), Ok(()));
    let writes = local.writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].name, "Work");
    assert_eq!(writes[0].identity_id, authority().owner.account.to_string());
    assert_eq!(f.hosted.receipts.lock().unwrap().len(), 2);
}

#[test]
fn changed_auth_identity_or_removed_local_profile_never_provisions() {
    let f = Fixture::new();
    let local = Local::empty();
    let mut changed = projection(&f);
    changed.session.auth_identity_id = "60000000-0000-4000-8000-000000000002".parse().unwrap();
    assert_eq!(
        run(&f, &f.prepared(), &client(changed), &local, || Ok(())),
        Err(Failure::SessionExpired)
    );
    local
        .rows
        .lock()
        .unwrap()
        .push(ProfileLifecycleInventoryItem {
            profile_id: f.profile,
            account_binding: format!("koru:{}", authority().owner.account),
            removed_at_millis: Some(NOW),
        });
    assert_eq!(
        run(
            &f,
            &f.prepared(),
            &client(projection(&f)),
            &local,
            || Ok(())
        ),
        Err(Failure::AccessRevoked)
    );
    assert!(local.writes.lock().unwrap().is_empty());
    assert!(f.hosted.receipts.lock().unwrap().is_empty());
}

#[test]
fn authority_change_during_hosted_completion_prevents_local_effect() {
    let f = Fixture::new();
    let local = Local::empty();
    let calls = AtomicUsize::new(0);
    assert_eq!(
        run(&f, &f.prepared(), &client(projection(&f)), &local, || {
            if calls.fetch_add(1, Ordering::AcqRel) == 0 {
                Ok(())
            } else {
                Err(Failure::Unavailable)
            }
        }),
        Err(Failure::Unavailable)
    );
    assert!(local.writes.lock().unwrap().is_empty());
    assert_eq!(f.hosted.receipts.lock().unwrap().len(), 1);
}

#[test]
fn fresh_session_repairs_completed_device_from_current_hosted_state_and_effective_trust() {
    let f = Fixture::new();
    let prepared = f.prepared();
    let local = Local::empty();
    let mut credential = authority();
    credential.session_id = "40000000-0000-4000-8000-000000000009".into();
    let mut hosted = projection(&f);
    hosted.session.session_id = credential.session_id.parse().unwrap();
    hosted.devices.push(koru_account_wire::DeviceSummaryV1 {
        version: Version1,
        device_id: f.joining.device_id,
        display_name: "Fixture".into(),
        platform: koru_account_core::DevicePlatform::Windows,
        app_version: "fixture".into(),
        enrollment_protocol_version: Some(ENROLLMENT_VERSION),
        peer_protocol_version: Some(PEER_PROTOCOL_VERSION),
        endpoint_id_hex: f.joining.endpoint_id.to_storage(),
        profile_states: vec![koru_account_wire::DeviceProfileSummaryV1 {
            version: Version1,
            profile_id: f.profile,
            state: DeviceProfileState::Provisioned,
        }],
        last_seen_at_millis: NOW,
        revoked: false,
    });
    let client = client(hosted);
    assert_eq!(
        super::super::provisioning::provision(
            &client,
            &local,
            f.profile,
            f.joining.device_id,
            &prepared,
            &credential,
            NOW,
            || Err(Failure::AccessRevoked),
        ),
        Err(Failure::AccessRevoked)
    );
    assert!(local.writes.lock().unwrap().is_empty());
    assert_eq!(
        super::super::provisioning::provision(
            &client,
            &local,
            f.profile,
            f.joining.device_id,
            &prepared,
            &credential,
            NOW,
            || Ok(()),
        ),
        Ok(())
    );
    assert_eq!(local.writes.lock().unwrap().len(), 1);
    assert!(
        f.hosted.receipts.lock().unwrap().is_empty(),
        "old-session receipt is never replayed under fresh-session authority"
    );
}

#[test]
fn completed_target_recovery_requires_current_authority_absence_and_effective_trust() {
    let mut f = Fixture::new();
    let prepared = f.prepared();
    f.joining_repository
        .commit_enrollment(&prepared, &f.attempt.verified, f.joining.device_id)
        .unwrap();
    let credential = authority();
    let record = f
        .joining_repository
        .begin_account_write(&credential.namespace, credential.owner)
        .unwrap();
    f.joining_repository
        .finish_account_write(
            &credential.namespace,
            &record,
            credential.owner,
            true,
            false,
        )
        .unwrap();
    assert_eq!(record.version, credential.record_version);
    let mut hosted = projection(&f);
    let mut unseen = hosted.profiles[0].clone();
    unseen.profile_id = ProfileId::new();
    hosted.profiles.push(unseen);
    let empty = ProfileLifecycleInventory { profiles: vec![] };
    let authorized = ownership::AuthorizedProfiles::verify(
        &credential,
        f.joining.device_id,
        &hosted.session,
        &hosted.profiles,
        &empty,
    )
    .unwrap();
    let removed = ProfileLifecycleInventory {
        profiles: vec![ProfileLifecycleInventoryItem {
            profile_id: f.profile,
            account_binding: format!("koru:{}", credential.owner.account),
            removed_at_millis: Some(NOW),
        }],
    };
    assert!(
        authorized
            .repair_missing_completed(
                &mut f.joining_repository,
                &removed,
                f.joining.device_id,
                BTreeSet::new()
            )
            .unwrap()
            .is_empty()
    );
    let restored = authorized
        .repair_missing_completed(
            &mut f.joining_repository,
            &empty,
            f.joining.device_id,
            BTreeSet::new(),
        )
        .unwrap();
    assert_eq!(restored, BTreeSet::from([f.profile]));
    assert_eq!(
        f.joining_repository
            .account_targets(&credential.namespace, credential.owner.account)
            .unwrap(),
        restored
    );
}
