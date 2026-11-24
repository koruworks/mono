// SPDX-License-Identifier: MPL-2.0

//! The real mobile mailbox, shared worker and production service, with only
//! hosted HTTP and the platform's native/vault replies supplied by fixtures.
use super::*;
use base64::Engine as _;
use koru_account_client::{AccountClient, HttpRequest, HttpResponse, Transport, TransportError};
use koru_account_core::DevicePlatform;
use koru_account_runtime::account::{AccountPorts, ProductionAccountService, lifecycle};
use koru_account_runtime::ports::{Clock, SystemClock, SystemEntropy};
use koru_account_wire as hosted;

const REQUESTER: u64 = 42;
const SESSION: &str = "20000000-0000-4000-8000-000000000002";
const FLOW: &str = "10000000-0000-4000-8000-000000000001";
const GRANT_FLOW: &str = "50000000-0000-4000-8000-000000000005";

fn descriptor() -> koru_device::DeviceDescriptorV1 {
    koru_device::DeviceDescriptorV1::new(
        koru_device::DeviceId::parse("a6258102-2534-4fab-bd4d-40717b066f29").unwrap(),
        "Mobile fixture".into(),
        &koru_device::DeviceSigningKey::from_secret_bytes([42; 32]),
        koru_device::ReplicationEndpointSecret::from_secret_bytes([43; 32]).endpoint_id(),
        1,
    )
}
fn tokens(refresh: &str) -> hosted::TokenPairV1 {
    hosted::TokenPairV1 {
        version: hosted::Version1,
        session_id: SESSION.parse().unwrap(),
        access_token: hosted::AccessToken("access-memory-only".into()),
        access_expires_at_millis: SystemClock.now_millis().unwrap() + 300_000,
        refresh_token: hosted::RefreshToken(refresh.into()),
        refresh_idle_expires_at_millis: SystemClock.now_millis().unwrap() + 600_000,
        refresh_absolute_expires_at_millis: SystemClock.now_millis().unwrap() + 900_000,
    }
}
fn response(value: &impl serde::Serialize) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse {
        document_removal: false,
        status: 200,
        body: serde_json::to_vec(value).unwrap(),
    })
}
#[derive(Default)]
struct Hosted {
    starts: AtomicUsize,
    completes: Mutex<Vec<Vec<u8>>>,
    exchanges: Mutex<Vec<Vec<u8>>>,
    refreshes: Mutex<Vec<Vec<u8>>>,
    lose_complete: AtomicUsize,
    lose_exchange: AtomicUsize,
}
// `fetch_update` is deprecated for `try_update`, which is unstable on the
// Rust 1.93 the Linux API image builds with; rename when that image moves.
#[allow(deprecated)]
fn lose(counter: &AtomicUsize) -> bool {
    counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
}
impl Transport for Hosted {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        match request.path_and_query.as_str() {
            "/v1/oauth/google/native/token/start" => {
                self.starts.fetch_add(1, Ordering::SeqCst);
                let start: hosted::GoogleNativeTokenStartRequestV1 =
                    serde_json::from_slice(&request.body).unwrap();
                assert_eq!(start.platform, DevicePlatform::Android);
                assert_eq!(
                    start.descriptor,
                    hosted::SignedDeviceDescriptorV1::try_from(descriptor()).unwrap()
                );
                response(&hosted::GoogleNativeTokenStartResponseV1 {
                    version: hosted::Version1,
                    flow_id: FLOW.parse().unwrap(),
                    flow_state: hosted::OAuthState("private-hosted-state".into()),
                    nonce: hosted::OAuthState("private-hosted-nonce".into()),
                    expires_at_millis: SystemClock.now_millis().unwrap() + 300_000,
                })
            }
            "/v1/oauth/google/native/token/complete" => {
                let complete: hosted::GoogleNativeTokenCompleteRequestV1 =
                    serde_json::from_slice(&request.body).unwrap();
                assert_eq!(
                    complete.id_token.0,
                    "sdk-token-bound-to-private-hosted-nonce"
                );
                assert_eq!(complete.flow_state.0, "private-hosted-state");
                self.completes.lock().unwrap().push(request.body);
                if lose(&self.lose_complete) {
                    return Err(TransportError::Unavailable);
                }
                response(
                    &hosted::GoogleNativeTokenCompleteResponseV1::ExchangeReady {
                        version: hosted::Version1,
                        flow_id: FLOW.parse().unwrap(),
                        grant: hosted::OneUseGrant(format!("{GRANT_FLOW}.one-use")),
                        expires_at_millis: SystemClock.now_millis().unwrap() + 300_000,
                    },
                )
            }
            "/v1/oauth/native/device-challenge" => {
                let exchange: hosted::NativeExchangeRequestV1 =
                    serde_json::from_slice(&request.body).unwrap();
                assert!(exchange.device_ownership_proof.is_none());
                let challenge = koru_account_core::DeviceOwnershipChallenge {
                    flow_id: "60000000-0000-4000-8000-000000000006".parse().unwrap(),
                    parent_flow_id: Some(GRANT_FLOW.parse().unwrap()),
                    device: koru_account_core::AccountDevice {
                        descriptor: descriptor(),
                        registration: koru_account_core::AccountDeviceRegistration {
                            koru_identity_id: "30000000-0000-4000-8000-000000000003"
                                .parse()
                                .unwrap(),
                            platform: DevicePlatform::Android,
                            app_version: "mobile-fixture".into(),
                            enrollment_protocol_version: Some(koru_pairing::ENROLLMENT_VERSION),
                            peer_protocol_version: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
                            registered_at_millis: 1,
                            last_seen_at_millis: 1,
                            revoked_at_millis: None,
                        },
                    },
                    expires_at_millis: SystemClock.now_millis().unwrap() + 300_000,
                    nonce: [1; 32],
                };
                response(&hosted::DeviceOwnershipChallengeResponseV1 {
                    version: hosted::Version1,
                    challenge: base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(serde_json::to_vec(&challenge).unwrap()),
                })
            }
            "/v1/oauth/native/exchange" => {
                let exchange: hosted::NativeExchangeRequestV1 =
                    serde_json::from_slice(&request.body).unwrap();
                let proof = exchange
                    .device_ownership_proof
                    .as_ref()
                    .expect("production signed ownership proof");
                let proof: koru_account_core::DeviceOwnershipProof = serde_json::from_slice(
                    &base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(proof)
                        .unwrap(),
                )
                .unwrap();
                assert!(proof.verify_signature());
                self.exchanges.lock().unwrap().push(request.body);
                if lose(&self.lose_exchange) {
                    return Err(TransportError::Unavailable);
                }
                response(&hosted::NativeExchangeResponseV1 {
                    version: hosted::Version1,
                    tokens: tokens("refresh-issued-once"),
                    account: hosted::AccountSummaryV1 {
                        version: hosted::Version1,
                        koru_identity_id: "30000000-0000-4000-8000-000000000003".parse().unwrap(),
                        display_name: "Fixture owner".into(),
                        auth_identities: Vec::new(),
                        terms_version_at_creation: "fixture".into(),
                        created_at_millis: 1,
                    },
                })
            }
            "/v1/oauth/refresh" => {
                let refresh: hosted::RefreshRequestV1 =
                    serde_json::from_slice(&request.body).unwrap();
                assert_eq!(refresh.refresh_token.0, "refresh-issued-once");
                self.refreshes.lock().unwrap().push(request.body);
                response(&tokens("refresh-rotated-on-restore"))
            }
            "/v1/oauth/logout" => response(&hosted::EmptyResponseV1 {
                version: hosted::Version1,
            }),
            _ => panic!("unexpected fake hosted route: {}", request.path_and_query),
        }
    }
}
struct Inventory;
impl lifecycle::LocalProfileInventory for Inventory {
    fn inventory(&self) -> Result<koru_store_wire::ProfileLifecycleInventory, ServiceError> {
        Ok(koru_store_wire::ProfileLifecycleInventory {
            profiles: Vec::new(),
        })
    }
}
struct ManualRetryClock {
    at: std::time::Instant,
    millis: i64,
}
impl Clock for ManualRetryClock {
    fn now_millis(&self) -> Option<i64> {
        Some(self.millis)
    }
    fn monotonic(&self) -> std::time::Instant {
        self.at
    }
}
fn mobile(hosted: Arc<Hosted>) -> MobileAccount {
    let mut bridge = PlatformBridge::new("trusted-web.apps.googleusercontent.com".into()).unwrap();
    // Manual Retry drives every recovery in this fixture; scheduler delays
    // cannot silently race the assertions or start another automatic attempt.
    Arc::get_mut(&mut bridge).unwrap().clock = Arc::new(ManualRetryClock {
        at: std::time::Instant::now(),
        millis: SystemClock.now_millis().unwrap(),
    });
    let service = ProductionAccountService::new(
        AccountClient::with_transport(hosted),
        hosted::SignedDeviceDescriptorV1::try_from(descriptor()).unwrap(),
        koru_device::DeviceSigningKey::from_secret_bytes([42; 32]),
        DevicePlatform::Android,
        "mobile-fixture".into(),
        AccountPorts {
            delivery: Arc::new(NativeDelivery(bridge.clone())),
            local_profiles: Arc::new(Inventory),
            lifecycle_repository: lifecycle::LifecycleRepository::in_memory(),
            clock: Arc::new(SystemClock),
            entropy: Arc::new(SystemEntropy),
            // No Cloud sync toggle on the phone yet (D238).
            cloud_sync: None,
        },
    );
    MobileAccount::start(service, bridge, entry())
}
fn platform(account: &MobileAccount, response: wire::account_platform_request::Response) {
    account
        .platform(&mut wire::AccountPlatformRequest {
            version: 1,
            requester: REQUESTER,
            response: Some(response),
            ..Default::default()
        })
        .unwrap();
}
fn command(account: &MobileAccount, action: wire::AccountAction) -> u64 {
    account
        .command(&wire::AccountCommandRequest {
            expected_owner_scope: Some(account.control.state().active_scope),
            version: 1,
            requester: REQUESTER,
            action: action as i32,
            locale: if action == wire::AccountAction::StartGoogle {
                "en".into()
            } else {
                String::new()
            },
            ..Default::default()
        })
        .unwrap()
}
fn vault(
    account: &MobileAccount,
    prompt: wire::AccountVaultPrompt,
    success: bool,
    credential: Option<Vec<u8>>,
) {
    platform(
        account,
        wire::account_platform_request::Response::Vault(wire::AccountVaultReply {
            request_id: prompt.request_id,
            success,
            credential,
        }),
    );
}
fn foreground(account: &MobileAccount, credential: Option<Vec<u8>>) {
    platform(
        account,
        wire::account_platform_request::Response::Foreground(true),
    );
    let load = wait_for(|| account.poll(REQUESTER, 0, true).vault);
    assert_eq!(load.action, wire::AccountVaultAction::Load as i32);
    vault(account, load, true, credential);
}
fn wait_retry(account: &MobileAccount) {
    wait_for(|| {
        let value = account.poll(REQUESTER, 0, true);
        (value
            .status
            .as_ref()
            .is_some_and(|status| status.retry_at_millis.is_some())
            && value.vault.is_none())
        .then_some(())
    });
}

#[test]
fn production_native_replays_lost_http_and_waits_for_platform_vault_ack() {
    let hosted = Arc::new(Hosted::default());
    hosted.lose_complete.store(1, Ordering::SeqCst);
    hosted.lose_exchange.store(1, Ordering::SeqCst);
    let account = mobile(hosted.clone());
    foreground(&account, None);
    wait_for(|| (account.control.state().stage == AccountStage::SignedOut).then_some(()));
    let generation = command(&account, wire::AccountAction::StartGoogle);
    let prompt = wait_for(|| account.poll(REQUESTER, 0, true).sign_in);
    assert_eq!(prompt.generation, generation);
    assert_eq!(
        prompt.server_client_id,
        "trusted-web.apps.googleusercontent.com"
    );
    assert_eq!(prompt.nonce, "private-hosted-nonce");
    assert!(account.poll(REQUESTER + 1, 0, true).sign_in.is_none());
    assert!(account.poll(REQUESTER, 0, false).sign_in.is_none());
    platform(
        &account,
        wire::account_platform_request::Response::SignIn(wire::NativeSignInReply {
            generation: prompt.generation,
            request_id: prompt.request_id,
            id_token: Some(b"sdk-token-bound-to-private-hosted-nonce".to_vec()),
            cancelled: false,
        }),
    );
    wait_retry(&account);
    command(&account, wire::AccountAction::Retry);
    wait_for(|| (hosted.exchanges.lock().unwrap().len() == 1).then_some(()));
    wait_retry(&account);
    command(&account, wire::AccountAction::Retry);
    let first_store = wait_for(|| account.poll(REQUESTER, 0, true).vault);
    assert_eq!(first_store.action, wire::AccountVaultAction::Store as i32);
    let encoded = first_store.credential.clone().unwrap();
    assert!(
        account
            .poll(REQUESTER, 0, true)
            .results
            .iter()
            .all(|r| r.kind != "signed_in")
    );
    vault(&account, first_store, false, None);
    wait_retry(&account);
    let retry_generation = command(&account, wire::AccountAction::Retry);
    let second_store = wait_for(|| account.poll(REQUESTER, 0, true).vault);
    assert_eq!(second_store.credential.as_ref().unwrap(), &encoded);
    vault(&account, second_store, true, None);
    let result = wait_for(|| {
        account
            .poll(REQUESTER, 0, true)
            .results
            .into_iter()
            .find(|r| r.kind == "signed_in")
    });
    assert_eq!(result.generation, retry_generation);
    assert_eq!(hosted.starts.load(Ordering::SeqCst), 1);
    let completes = hosted.completes.lock().unwrap();
    assert_eq!(completes.len(), 2);
    assert_eq!(completes[0], completes[1]);
    let exchanges = hosted.exchanges.lock().unwrap();
    assert_eq!(exchanges.len(), 2);
    assert_eq!(exchanges[0], exchanges[1]);
    drop(account);
    drop(exchanges);
    drop(completes);

    // A fresh host restores only the platform record, rotates through the same
    // service, and waits for the new platform vault acknowledgement.
    let restored = mobile(hosted.clone());
    foreground(&restored, Some(encoded));
    let rotated = wait_for(|| {
        restored
            .poll(REQUESTER, 0, true)
            .vault
            .filter(|v| v.action == wire::AccountVaultAction::Store as i32)
    });
    let rotated_bytes = rotated.credential.clone().unwrap();
    let credential = SessionCredential::decode(
        &SecretString::new(String::from_utf8(rotated_bytes).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(credential.refresh_token, "refresh-rotated-on-restore");
    assert_ne!(restored.control.state().stage, AccountStage::SignedIn);
    vault(&restored, rotated, true, None);
    wait_for(|| (restored.control.state().stage == AccountStage::SignedIn).then_some(()));
    assert_eq!(hosted.refreshes.lock().unwrap().len(), 1);
    assert_eq!(hosted.starts.load(Ordering::SeqCst), 1);
}

#[test]
fn production_native_cancel_removes_prompt_and_refuses_late_sdk_result() {
    let hosted = Arc::new(Hosted::default());
    let account = mobile(hosted.clone());
    foreground(&account, None);
    wait_for(|| (account.control.state().stage == AccountStage::SignedOut).then_some(()));
    let generation = command(&account, wire::AccountAction::StartGoogle);
    let prompt = wait_for(|| account.poll(REQUESTER, 0, true).sign_in);
    account
        .command(&wire::AccountCommandRequest {
            expected_owner_scope: Some(account.control.state().active_scope),
            version: 1,
            requester: REQUESTER,
            action: wire::AccountAction::Cancel as i32,
            generation,
            ..Default::default()
        })
        .unwrap();
    wait_for(|| {
        account
            .poll(REQUESTER, 0, true)
            .results
            .into_iter()
            .find(|r| r.kind == "cancelled")
    });
    assert!(account.poll(REQUESTER, 0, true).sign_in.is_none());
    assert!(
        account
            .platform(&mut wire::AccountPlatformRequest {
                version: 1,
                requester: REQUESTER,
                response: Some(wire::account_platform_request::Response::SignIn(
                    wire::NativeSignInReply {
                        generation,
                        request_id: prompt.request_id,
                        id_token: Some(b"late-sdk-token".to_vec()),
                        cancelled: false,
                    }
                )),
                ..Default::default()
            })
            .is_err()
    );
    assert!(hosted.completes.lock().unwrap().is_empty());
    assert!(hosted.exchanges.lock().unwrap().is_empty());
}
