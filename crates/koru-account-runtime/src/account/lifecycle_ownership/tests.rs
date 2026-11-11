// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::identity::AccountOwner;
use koru_account_client::{HttpRequest, HttpResponse, Transport, TransportError};
use koru_account_core::{ProfileLifecycleRevision, ProfileLifecycleState};

fn owner(n: u8) -> AccountOwner {
    AccountOwner {
        account: format!("10000000-0000-4000-8000-{n:012}").parse().unwrap(),
        auth_identity: format!("20000000-0000-4000-8000-{n:012}").parse().unwrap(),
    }
}
fn profile() -> ProfileId {
    ProfileId::parse("40000000-0000-4000-8000-000000000004").unwrap()
}
fn session() -> AccountSessionId {
    "50000000-0000-4000-8000-000000000005".parse().unwrap()
}
struct Inventory(Mutex<KoruIdentityId>);
impl lifecycle::LocalProfileInventory for Inventory {
    fn inventory(&self) -> Result<koru_store_wire::ProfileLifecycleInventory, ServiceError> {
        Ok(koru_store_wire::ProfileLifecycleInventory {
            profiles: vec![koru_store_wire::ProfileLifecycleInventoryItem {
                profile_id: koru_store_core::ProfileId::from_uuid(profile().as_uuid()),
                account_binding: format!("koru:{}", self.0.lock().unwrap()),
                removed_at_millis: None,
            }],
        })
    }
}
struct NoDelivery;
impl SignInDelivery for NoDelivery {
    fn bind(&self) -> Result<Box<dyn crate::delivery::SignInSession>, ServiceError> {
        panic!("no browser in lifecycle fixture")
    }
}
struct Time(Instant);
impl Clock for Time {
    fn now_millis(&self) -> Option<i64> {
        Some(100)
    }
    fn monotonic(&self) -> Instant {
        self.0
    }
}
struct EntropyStub;
impl Entropy for EntropyStub {
    fn fill_bytes(&self, bytes: &mut [u8]) {
        bytes.fill(7);
    }
}
#[derive(Default)]
struct Hosted {
    deletes: Mutex<Vec<koru_account_core::ProfileLifecycleMutationId>>,
    rebinds: AtomicUsize,
    refreshes: AtomicUsize,
    /// The delete answers the service's 428 for a fresh sign-in.
    fresh_auth: AtomicBool,
}
impl Transport for Hosted {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let body = if request.path_and_query.ends_with("/rebind") {
            self.rebinds.fetch_add(1, Ordering::SeqCst);
            let request: RebindProfileRequestV1 = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(request.profile_id, profile());
            assert_eq!(request.erased_koru_identity_id, owner(1).account);
            serde_json::to_vec(&ProfileSummaryV1 {
                version: Version1,
                koru_identity_id: owner(2).account,
                profile_id: profile(),
                name_snapshot: None,
                locale_snapshot: None,
                auth_identities: vec![],
                lifecycle_revision: ProfileLifecycleRevision::INITIAL,
            })
            .unwrap()
        } else if request.path_and_query.ends_with("/refresh") {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            serde_json::to_vec(&TokenPairV1 {
                version: Version1,
                session_id: session(),
                access_token: AccessToken("new-access".into()),
                access_expires_at_millis: 1_000_000,
                refresh_token: koru_account_wire::RefreshToken("new-rotation".into()),
                refresh_idle_expires_at_millis: 2_000_000,
                refresh_absolute_expires_at_millis: 3_000_000,
            })
            .unwrap()
        } else if request.path_and_query == "/v1/account/devices" {
            let mut devices = crate::account::tests::current_device_list();
            devices.devices[0].display_name = "injected device".into();
            serde_json::to_vec(&devices).unwrap()
        } else if request.path_and_query == format!("/v1/account/profiles/{}", profile()) {
            let request: DeleteProfileRequestV1 = serde_json::from_slice(&request.body).unwrap();
            self.deletes.lock().unwrap().push(request.operation_id);
            if self.fresh_auth.load(Ordering::SeqCst) {
                return Ok(HttpResponse {
                    status: 428,
                    body: serde_json::to_vec(&koru_account_wire::DeveloperErrorV1 {
                        retry_after_millis: None,
                        code: koru_account_wire::ErrorCode::parse(
                            koru_account_wire::ErrorCode::FRESH_AUTHENTICATION_REQUIRED,
                        )
                        .unwrap(),
                        message: String::new(),
                    })
                    .unwrap(),
                });
            }
            return Err(TransportError::Unavailable);
        } else {
            panic!("unexpected lifecycle route: {}", request.path_and_query);
        };
        Ok(HttpResponse { status: 200, body })
    }
}
fn service(
    hosted: Arc<Hosted>,
    inventory: Arc<Inventory>,
    account: u8,
) -> ProductionAccountService {
    let signing = koru_device::DeviceSigningKey::from_secret_bytes([42; 32]);
    let endpoint = koru_device::ReplicationEndpointSecret::from_secret_bytes([43; 32]);
    let descriptor = SignedDeviceDescriptorV1::try_from(koru_device::DeviceDescriptorV1::new(
        koru_device::DeviceId::parse("a6258102-2534-4fab-bd4d-40717b066f29").unwrap(),
        "injected device".into(),
        &signing,
        endpoint.endpoint_id(),
        1,
    ))
    .unwrap();
    let mut service = ProductionAccountService::new(
        AccountClient::with_transport(hosted),
        descriptor,
        signing,
        // The platform `current_device_list` reports, or the service sees a
        // stale registration on every host but Windows and refreshes it.
        crate::account::tests::current_platform(),
        "test".into(),
        AccountPorts {
            delivery: Arc::new(NoDelivery),
            local_profiles: inventory,
            lifecycle_repository: lifecycle::LifecycleRepository::in_memory(),
            clock: Arc::new(Time(Instant::now())),
            entropy: Arc::new(EntropyStub),
            cloud_sync: None,
        },
    );
    service.durable_owner = Some(owner(account));
    service.access = Some(AccessToken("injected-access".into()));
    service.access_expires_at_millis = Some(1_000_000);
    service
}
fn delete(service: &mut ProductionAccountService) -> ServiceError {
    service
        .delete_profile(
            profile(),
            ProfileLifecycleRevision::INITIAL,
            &Cancel(Arc::new(AtomicBool::new(false))),
        )
        .unwrap_err()
}
fn retry(service: &mut ProductionAccountService) -> ServiceOutcome {
    let credential = SessionCredential::new(session().to_string(), "old-refresh".into()).unwrap();
    // The worker arms the key the vault holds beside these bytes before any
    // command can present it; a refresh without one refuses.
    service.arm_refresh_retry_key(Some([44; 32]));
    service
        .execute(
            7,
            AccountCommand::Retry,
            Some(&credential),
            &Cancel(Arc::new(AtomicBool::new(false))),
            &mut |_| {},
        )
        .unwrap()
}

#[test]
fn wrong_account_cannot_create_or_poison_another_accounts_delete_intent() {
    let hosted = Arc::new(Hosted::default());
    let inventory = Arc::new(Inventory(Mutex::new(owner(1).account)));
    let mut b = service(hosted.clone(), inventory.clone(), 2);
    assert_eq!(delete(&mut b).code, "profile_lifecycle_owner_unproven");
    assert!(b.journal().get(profile()).unwrap().is_none());
    assert!(hosted.deletes.lock().unwrap().is_empty());
    let mut a = service(hosted.clone(), inventory, 1);
    a.lifecycle_repository = b.lifecycle_repository.clone();
    a.lifecycle_ownership = b.lifecycle_ownership.clone();
    assert_eq!(delete(&mut a).code, "account_unavailable");
    assert_eq!(hosted.deletes.lock().unwrap().len(), 1);
}

#[test]
fn proven_rebind_retires_old_intent_and_fences_queued_a_before_store_binding_changes() {
    let hosted = Arc::new(Hosted::default());
    let inventory = Arc::new(Inventory(Mutex::new(owner(1).account)));
    let mut a = service(hosted.clone(), inventory.clone(), 1);
    assert_eq!(delete(&mut a).code, "account_unavailable");
    let original = a.journal().get(profile()).unwrap().unwrap();
    assert_eq!(delete(&mut a).code, "account_unavailable");
    assert_eq!(
        hosted.deletes.lock().unwrap().as_slice(),
        &[original.operation, original.operation]
    );
    let mut b = service(hosted.clone(), inventory.clone(), 2);
    b.lifecycle_repository = a.lifecycle_repository.clone();
    b.lifecycle_ownership = a.lifecycle_ownership.clone();
    assert!(
        b.next_pending_delete().unwrap().is_none(),
        "B cannot replay A's pending intent"
    );
    b.pending_rebind = Some(Pending {
        command: AccountCommand::RebindProfile {
            profile_id: profile().to_string(),
            erased_identity_id: owner(1).account.to_string(),
        },
        request: RebindProfileRequestV1 {
            version: Version1,
            profile_id: profile(),
            erased_koru_identity_id: owner(1).account,
            fresh_auth_grant: OneUseGrant("injected-proof".into()),
            explicit_rebind: true,
            name_snapshot: None,
            locale_snapshot: None,
        },
    });
    let rebound = b.finish_rebind().unwrap();
    assert_eq!(rebound.koru_identity_id, owner(2).account);
    assert!(a.journal().get(profile()).unwrap().is_none());
    assert_eq!(
        *inventory.0.lock().unwrap(),
        owner(1).account,
        "Store deliberately still reports old A binding"
    );
    assert_eq!(delete(&mut a).code, "profile_lifecycle_owner_changed");
    assert!(a.journal().get(profile()).unwrap().is_none());
    *inventory.0.lock().unwrap() = owner(2).account;
    let outcome = retry(&mut b);
    assert!(outcome.follow_up.is_none());
    assert!(matches!(outcome.credential, CredentialChange::Replace(_)));
    assert_eq!(hosted.deletes.lock().unwrap().len(), 2);
    assert_eq!(hosted.rebinds.load(Ordering::SeqCst), 1);
}

#[test]
fn restart_unknown_intent_never_replays_and_repair_does_not_discard_rotation() {
    let hosted = Arc::new(Hosted::default());
    let inventory = Arc::new(Inventory(Mutex::new(owner(1).account)));
    let mut original = service(hosted.clone(), inventory.clone(), 1);
    assert_eq!(delete(&mut original).code, "account_unavailable");
    let before = original.journal().get(profile()).unwrap().unwrap();
    let mut restarted = service(hosted.clone(), inventory, 1);
    restarted.lifecycle_repository = original.lifecycle_repository.clone();
    assert_eq!(
        delete(&mut restarted).code,
        "profile_lifecycle_owner_unproven"
    );
    let outcome = retry(&mut restarted);
    assert!(
        matches!(outcome.result, AccountOperationResult::Failed { code } if code == "profile_lifecycle_owner_unproven")
    );
    assert_eq!(
        outcome.state.failure_code.as_deref(),
        Some("profile_lifecycle_owner_unproven")
    );
    let CredentialChange::Replace(rotated) = outcome.credential else {
        panic!("repair must retain rotated credential for vault ACK");
    };
    assert_eq!(rotated.refresh_token, "new-rotation");
    assert!(outcome.follow_up.is_none());
    assert_eq!(hosted.deletes.lock().unwrap().len(), 1);
    assert_eq!(restarted.journal().get(profile()).unwrap().unwrap(), before);
}

#[test]
fn retirement_refuses_changed_exact_intent_and_rolls_back_both_tables() {
    let mut journal = lifecycle::LifecycleRepository::in_memory();
    let original = journal
        .begin_delete(profile(), ProfileLifecycleRevision::INITIAL, 1)
        .unwrap();
    let restore = journal
        .begin_restore(profile(), ProfileLifecycleRevision::new(1), 1)
        .unwrap();
    assert!(
        journal
            .retire_for_rebind(
                profile(),
                Some(koru_account_core::ProfileLifecycleMutationId::mint())
            )
            .is_err()
    );
    assert_eq!(journal.get(profile()).unwrap().unwrap(), original);
    assert_eq!(
        journal
            .begin_restore(profile(), ProfileLifecycleRevision::new(1), 2)
            .unwrap(),
        restore
    );
    journal
        .retire_for_rebind(profile(), Some(original.operation))
        .unwrap();
    assert!(journal.get(profile()).unwrap().is_none());
    assert_ne!(
        journal
            .begin_restore(profile(), ProfileLifecycleRevision::new(1), 3)
            .unwrap(),
        restore
    );
    // Replaying successful retirement after an ACK loss is harmless.
    journal.retire_for_rebind(profile(), None).unwrap();
    assert_eq!(ProfileLifecycleState::Active, original.lifecycle_state);
}

/// **A delete refused for a fresh sign-in can be tried again after a
/// restart** (2026-11-10). The 428 is a precondition: the service applied
/// nothing, so its pending row is dropped at once. Kept, it outlived the
/// in-memory owner proof, and every retry after an Agent restart was
/// refused as owner-unproven — *Profile removal paused* for good.
#[test]
fn a_delete_refused_for_a_fresh_sign_in_survives_a_restart() {
    let hosted = Arc::new(Hosted::default());
    hosted.fresh_auth.store(true, Ordering::SeqCst);
    let inventory = Arc::new(Inventory(Mutex::new(owner(1).account)));
    let mut before = service(hosted.clone(), inventory.clone(), 1);
    assert_eq!(
        delete(&mut before).code,
        koru_account_wire::ErrorCode::FRESH_AUTHENTICATION_REQUIRED
    );
    assert!(
        before.journal().get(profile()).unwrap().is_none(),
        "nothing to replay, nothing kept"
    );

    // The restart: the same journal, a new Agent with no owner proof.
    hosted.fresh_auth.store(false, Ordering::SeqCst);
    let mut after = service(hosted.clone(), inventory, 1);
    after.lifecycle_repository = before.lifecycle_repository.clone();
    assert_ne!(
        delete(&mut after).code,
        "profile_lifecycle_owner_unproven",
        "the signed-in owner may delete again"
    );
    let deletes = hosted.deletes.lock().unwrap();
    assert_eq!(deletes.len(), 2, "the retry reached the service");
    assert_ne!(deletes[0], deletes[1], "and is a new operation");
}

/// **A row an older build kept for that refusal is cleared by the next
/// delete** — the state a person is stuck in after upgrading.
#[test]
fn a_kept_fresh_sign_in_refusal_no_longer_blocks_the_next_delete() {
    let hosted = Arc::new(Hosted::default());
    let inventory = Arc::new(Inventory(Mutex::new(owner(1).account)));
    let mut service = service(hosted.clone(), inventory, 1);
    let stale = {
        let mut journal = service.journal();
        let row = journal
            .begin_delete(profile(), ProfileLifecycleRevision::INITIAL, 50)
            .unwrap();
        journal
            .record_failure(
                profile(),
                row.operation,
                koru_account_wire::ErrorCode::FRESH_AUTHENTICATION_REQUIRED,
                60,
            )
            .unwrap();
        row.operation
    };
    assert_ne!(
        delete(&mut service).code,
        "profile_lifecycle_owner_unproven"
    );
    let deletes = hosted.deletes.lock().unwrap();
    assert_eq!(deletes.len(), 1);
    assert_ne!(deletes[0], stale, "a new operation, not the refused one");
}

/// A row for a refusal that may have changed something is **kept**: only
/// the fresh-sign-in precondition proves nothing happened.
#[test]
fn any_other_failed_delete_keeps_its_row_for_an_exact_replay() {
    let hosted = Arc::new(Hosted::default());
    let inventory = Arc::new(Inventory(Mutex::new(owner(1).account)));
    let mut service = service(hosted, inventory, 1);
    delete(&mut service);
    assert!(service.journal().get(profile()).unwrap().is_some());
}
