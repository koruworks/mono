// SPDX-License-Identifier: MPL-2.0

use super::*;

use std::collections::VecDeque;
use std::str::FromStr;
use std::sync::Mutex;

use ed25519_dalek::SigningKey;
use koru_account_core::workspace_lifecycle::{WorkspaceCloudState, WorkspaceLifecycleStatus};
use koru_account_core::{AccountSessionId, AuthFlowId, KoruIdentityId, ProfileId};
use koru_account_session::{
    DEVICE_ATTESTATION_AUDIENCE, DEVICE_ATTESTATION_MAX_MILLIS, DEVICE_ATTESTATION_VERSION,
    DeviceAttestationClaimsV1, DeviceAttestationKeyRing,
};
use koru_device::{DeviceId, DevicePublicKey, ReplicationEndpointId};

struct FakeTransport {
    requests: Mutex<Vec<HttpRequest>>,
    responses: Mutex<VecDeque<HttpResponse>>,
}

impl FakeTransport {
    fn responding<T: Serialize>(value: &T) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(VecDeque::from([HttpResponse {
                status: 200,
                body: serde_json::to_vec(value).unwrap(),
            }])),
        })
    }

    fn empty() -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(VecDeque::new()),
        })
    }
}

impl Transport for FakeTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(TransportError::Unavailable)
    }
}

fn flow_id() -> AuthFlowId {
    AuthFlowId::from_str("00000000-0000-4000-8000-000000000001").unwrap()
}

#[test]
fn callback_query_and_request_debug_never_disclose_secrets() {
    let request = HttpRequest {
        method: HttpMethod::Get,
        path_and_query:
            "/v1/oauth/google/web/callback?code=provider-code-sentinel&state=state-sentinel".into(),
        headers: Vec::new(),
        body: Vec::new(),
    };
    let debug = format!("{request:?}");
    assert!(!debug.contains("provider-code-sentinel"));
    assert!(!debug.contains("state-sentinel"));
    assert_eq!(debug.matches('?').count(), 0);
    assert!(request.path_and_query.contains("provider-code-sentinel"));
}

#[test]
fn browser_operations_emit_exact_origin_and_never_accept_a_url_return() {
    let response = AuthStartResponseV1 {
        version: Version1,
        flow_id: flow_id(),
        authorization_url: "https://accounts.example.test/auth".into(),
        expires_at_millis: 50,
    };
    let transport = FakeTransport::responding(&response);
    let client = AccountClient::with_transport(transport.clone())
        .with_browser_origin("https://app.example.test".into())
        .unwrap();
    client
        .google_web_start(&GoogleWebStartRequestV1 {
            version: Version1,
            return_target: SymbolicReturnTarget::DownloadWindows,
            locale: "en-US".into(),
            client_state: OAuthState("bff-state".into()),
        })
        .unwrap();
    let requests = transport.requests.lock().unwrap();
    assert_eq!(
        requests[0]
            .headers
            .iter()
            .find(|(name, _)| name == "origin")
            .unwrap()
            .1,
        "https://app.example.test"
    );
    assert!(!requests[0].path_and_query.contains("return="));
}

#[test]
fn web_session_and_csrf_are_redacted_and_only_names_enter_debug() {
    let response = EmptyResponseV1 { version: Version1 };
    let transport = FakeTransport::responding(&response);
    let client = AccountClient::with_transport(transport.clone())
        .with_browser_origin("https://app.example.test".into())
        .unwrap();
    let session = WebSessionToken("web-session-sentinel".into());
    let csrf = BrowserCsrf::new("csrf-sentinel".into()).unwrap();
    let authorization = AccountAuthorization::WebSession {
        session: &session,
        csrf: &csrf,
    };
    client
        .logout(
            &LogoutRequestV1 {
                version: Version1,
                session_id: "00000000-0000-4000-8000-000000000002".parse().unwrap(),
            },
            &authorization,
        )
        .unwrap();
    let debug = format!(
        "{:?} {:?}",
        authorization,
        transport.requests.lock().unwrap()[0]
    );
    assert!(!debug.contains("web-session-sentinel"));
    assert!(!debug.contains("csrf-sentinel"));
}

#[test]
fn server_error_messages_are_not_retained_by_client_errors() {
    let transport = Arc::new(FakeTransport {
        requests: Mutex::new(Vec::new()),
        responses: Mutex::new(VecDeque::from([HttpResponse {
            status: 503,
            body: serde_json::to_vec(&DeveloperErrorV1 {
                retry_after_millis: None,
                code: ErrorCode::parse(ErrorCode::CAPABILITY_UNAVAILABLE).unwrap(),
                message: "pii-sentinel@example.test".into(),
            })
            .unwrap(),
        }])),
    });
    let error = AccountClient::with_transport(transport).jwks().unwrap_err();
    assert!(!format!("{error:?} {error}").contains("pii-sentinel"));
    assert!(
        matches!(error, ClientError::Api { code, .. } if code.as_str() == ErrorCode::CAPABILITY_UNAVAILABLE)
    );
}

#[test]
fn rate_limit_delay_survives_transport_with_bounded_legacy_fallback() {
    for (supplied, expected) in [
        (Some(42_123), 42_123),
        (None, 60_000),
        (Some(0), 1_000),
        (Some(u64::MAX), 600_000),
    ] {
        let transport = Arc::new(FakeTransport {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(VecDeque::from([HttpResponse {
                status: 429,
                body: serde_json::to_vec(&DeveloperErrorV1 {
                    code: ErrorCode::parse(ErrorCode::RATE_LIMITED).unwrap(),
                    message: "private-server-message".into(),
                    retry_after_millis: supplied,
                })
                .unwrap(),
            }])),
        });
        let error = AccountClient::with_transport(transport).jwks().unwrap_err();
        assert_eq!(
            error,
            ClientError::RateLimited {
                retry_after_millis: expected
            }
        );
        assert!(!format!("{error:?}").contains("private-server-message"));
    }
}

/// **The client sends through the wire's catalog** and has none of its own:
/// every request names a `RouteId`, and `route_by_id` must answer that id
/// with that route's method and path — the catalog is indexed by id, so a
/// route out of place would send another route's. How many routes there are
/// is the shared catalog's to assert (`route_catalog_is_unique_and_schema_complete`
/// in koru-account-wire), so adding one there does not break this crate.
#[test]
fn request_catalog_and_server_catalog_are_the_same_values() {
    assert!(!ROUTES.is_empty());
    for route in ROUTES {
        assert_eq!(route_by_id(route.id).path, route.path);
        assert_eq!(route_by_id(route.id).method, route.method);
    }
}

#[test]
fn native_ownership_inspection_posts_current_secret_and_device_without_minting_authorization() {
    let summary = SessionSummaryV1 {
        version: Version1,
        session_id: "00000000-0000-4000-8000-000000000002".parse().unwrap(),
        koru_identity_id: "00000000-0000-4000-8000-000000000003".parse().unwrap(),
        auth_identity_id: "00000000-0000-4000-8000-000000000004".parse().unwrap(),
        device_id: Some(DeviceId::parse("00000000-0000-4000-8000-000000000005").unwrap()),
        expires_at_millis: 900,
        recent_auth_at_millis: 100,
    };
    let transport = FakeTransport::responding(&summary);
    let client = AccountClient::with_transport(transport.clone());
    let request = NativeSessionInspectRequestV1 {
        version: Version1,
        session_id: summary.session_id,
        refresh_token: RefreshToken("private-inspection-refresh".into()),
        device_id: summary.device_id.unwrap(),
    };
    assert_eq!(client.inspect_native_session(&request).unwrap(), summary);
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, HttpMethod::Post);
    assert_eq!(
        requests[0].path_and_query,
        "/v1/oauth/native/session/inspect"
    );
    assert!(
        requests[0]
            .headers
            .iter()
            .all(|(name, _)| !name.eq_ignore_ascii_case("authorization"))
    );
    assert_eq!(
        serde_json::from_slice::<NativeSessionInspectRequestV1>(&requests[0].body).unwrap(),
        request
    );
    assert!(!format!("{request:?} {:?}", requests[0]).contains("private-inspection-refresh"));
}

#[test]
fn embedded_web_session_derives_csrf_without_a_second_secret() {
    use base64::Engine as _;
    let csrf = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([9_u8; 32]);
    let session = WebSessionToken(format!("{}.{}.{}", flow_id(), "session-secret", csrf));
    let response = EmptyResponseV1 { version: Version1 };
    let transport = FakeTransport::responding(&response);
    let client = AccountClient::with_transport(transport.clone())
        .with_browser_origin("https://app.example.test".into())
        .unwrap();
    let authorization = AccountAuthorization::from_web_session(&session).unwrap();
    client
        .logout(
            &LogoutRequestV1 {
                version: Version1,
                session_id: "00000000-0000-4000-8000-000000000002".parse().unwrap(),
            },
            &authorization,
        )
        .unwrap();
    let request = &transport.requests.lock().unwrap()[0];
    assert!(
        request
            .headers
            .iter()
            .any(|(name, value)| name == CSRF_HEADER && value == &csrf)
    );
    assert!(!format!("{authorization:?} {request:?}").contains(&csrf));
}

#[test]
fn production_transport_rejects_remote_cleartext_but_allows_literal_loopback() {
    assert!(ReqwestTransport::new("http://remote.example.test").is_err());
    assert!(ReqwestTransport::new("http://localhost.example.test:43123").is_err());
    assert!(ReqwestTransport::new("http://localhost:43123").is_ok());
    assert!(ReqwestTransport::new("http://127.0.0.1:43123").is_ok());
    assert!(ReqwestTransport::new("http://[::1]:43123").is_ok());
    assert!(ReqwestTransport::new("https://api.example.test").is_ok());
}

fn test_attestation(
    kid: &str,
    seed: u8,
) -> (koru_account_session::SignedDeviceAttestationV1, JwkV1) {
    let signing = SigningKey::from_bytes(&[seed; 32]);
    let ring =
        DeviceAttestationKeyRing::new(kid.into(), vec![(kid.into(), signing.clone())]).unwrap();
    let now = 1_000_000;
    let signed = ring
        .sign(&DeviceAttestationClaimsV1 {
            version: DEVICE_ATTESTATION_VERSION,
            issuer: "https://account.example.test".into(),
            audience: DEVICE_ATTESTATION_AUDIENCE.into(),
            koru_identity_id: KoruIdentityId::from_str("10000000-0000-4000-8000-000000000001")
                .unwrap(),
            device_id: DeviceId::parse("20000000-0000-4000-8000-000000000002").unwrap(),
            signing_public_key: DevicePublicKey::from_bytes([21; 32]),
            endpoint_id: ReplicationEndpointId::from_bytes([22; 32]),
            authorized_profile_ids: vec![
                ProfileId::parse("30000000-0000-4000-8000-000000000003").unwrap(),
            ],
            session_id: AccountSessionId::from_str("40000000-0000-4000-8000-000000000004").unwrap(),
            token_id: "attestation-jti".into(),
            issued_at_millis: now,
            expires_at_millis: now + DEVICE_ATTESTATION_MAX_MILLIS,
        })
        .unwrap();
    let jwk = JwkV1 {
        kty: "OKP".into(),
        crv: "Ed25519".into(),
        key_use: "sig".into(),
        alg: "EdDSA".into(),
        kid: kid.into(),
        x: base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(signing.verifying_key().to_bytes()),
    };
    (signed, jwk)
}

fn jwks(attestation_keys: Vec<JwkV1>) -> JwksResponseV1 {
    JwksResponseV1 {
        version: Version1,
        keys: Vec::new(),
        attestation_keys,
        capability_keys: Vec::new(),
    }
}

#[test]
fn configured_bridge_client_verifies_old_evidence_through_its_real_jwks_refresh_path() {
    let (signed, jwk) = test_attestation("old-attestation", 41);
    let (_, stale) = test_attestation("unrelated", 42);
    let transport = FakeTransport::responding(&jwks(vec![jwk]));
    let policy = koru_account_session::RegionalCredentialPolicy {
        region: koru_account_core::workspace_authorization::Region::Us,
        legacy_issuer: "https://account.example.test".into(),
        regional_issuer: "https://us.account.example.test".into(),
        legacy_cutover_millis: 1_000_000,
        issuance: koru_account_session::CredentialIssuance::Regional,
        legacy_access_kids: ["old-access".into()].into(),
        legacy_attestation_kids: ["old-attestation".into()].into(),
    };
    let client = AccountClient::with_transport(transport.clone())
        .with_regional_credentials(policy)
        .unwrap();
    let mut cached = jwks(vec![stale]);
    assert!(
        client
            .verify_device_attestation(
                &signed,
                "https://us.account.example.test",
                1_000_001,
                &mut cached
            )
            .is_ok()
    );
    assert_eq!(transport.requests.lock().unwrap().len(), 1);
    assert!(
        client
            .verify_device_attestation(
                &signed,
                "https://us.account.example.test",
                1_000_000 + DEVICE_ATTESTATION_MAX_MILLIS,
                &mut cached
            )
            .is_err()
    );
    assert_eq!(
        transport.requests.lock().unwrap().len(),
        1,
        "expired evidence never refreshes JWKS"
    );
}

#[test]
fn bridge_client_still_configured_for_legacy_issuance_accepts_regional_version_two() {
    let (legacy, jwk) = test_attestation("new-attestation", 41);
    let key = SigningKey::from_bytes(&[41; 32]);
    let keys = std::collections::BTreeMap::from([("new-attestation".into(), key.verifying_key())]);
    let mut claims = koru_account_session::verify_device_attestation(
        &legacy,
        &keys,
        "https://account.example.test",
        1_000_000,
    )
    .unwrap()
    .claims()
    .clone();
    let mut policy = koru_account_session::RegionalCredentialPolicy {
        region: koru_account_core::workspace_authorization::Region::Us,
        legacy_issuer: "https://account.example.test".into(),
        regional_issuer: "https://us.account.example.test".into(),
        legacy_cutover_millis: 1_000_000,
        issuance: koru_account_session::CredentialIssuance::Regional,
        legacy_access_kids: ["old-access".into()].into(),
        legacy_attestation_kids: ["old-attestation".into()].into(),
    };
    claims.issuer = policy.regional_issuer.clone();
    let signer =
        DeviceAttestationKeyRing::new("new-attestation".into(), [("new-attestation".into(), key)])
            .unwrap()
            .with_regional_policy(policy.clone())
            .unwrap();
    let regional = signer.sign(&claims).unwrap();
    policy.issuance = koru_account_session::CredentialIssuance::Legacy;
    let client = AccountClient::with_transport(FakeTransport::empty())
        .with_regional_credentials(policy)
        .unwrap();
    let verified = client
        .verify_device_attestation(
            &regional,
            "https://account.example.test",
            1_000_001,
            &mut jwks(vec![jwk]),
        )
        .unwrap();
    assert_eq!(verified.claims().version, 2);
    assert_eq!(verified.claims().issuer, "https://us.account.example.test");
}

#[test]
fn attestation_verifier_uses_cache_and_refreshes_unknown_kid_once() {
    let (signed, signing_jwk) = test_attestation("attest-new", 41);
    let (_, stale_jwk) = test_attestation("attest-old", 42);
    let transport = FakeTransport::responding(&jwks(vec![signing_jwk.clone()]));
    let client = AccountClient::with_transport(transport.clone());
    let mut cached = jwks(vec![stale_jwk]);
    let verified = client
        .verify_device_attestation(
            &signed,
            "https://account.example.test",
            1_000_001,
            &mut cached,
        )
        .unwrap();
    assert_eq!(verified.claims().token_id, "attestation-jti");
    assert_eq!(transport.requests.lock().unwrap().len(), 1);
    assert_eq!(cached.attestation_keys, vec![signing_jwk]);

    let no_fetch = FakeTransport::empty();
    AccountClient::with_transport(no_fetch.clone())
        .verify_device_attestation(
            &signed,
            "https://account.example.test",
            1_000_002,
            &mut cached,
        )
        .unwrap();
    assert!(no_fetch.requests.lock().unwrap().is_empty());
}

#[test]
fn attestation_verifier_never_refreshes_non_unknown_failures_or_access_keys() {
    let (signed, jwk) = test_attestation("attest", 51);
    let (_, wrong_material) = test_attestation("attest", 52);
    let no_fetch = FakeTransport::empty();
    let mut wrong = jwks(vec![wrong_material]);
    assert_eq!(
        AccountClient::with_transport(no_fetch.clone()).verify_device_attestation(
            &signed,
            "https://account.example.test",
            1_000_001,
            &mut wrong,
        ),
        Err(AttestationVerificationError::InvalidAttestation)
    );
    assert!(no_fetch.requests.lock().unwrap().is_empty());

    let mut access_only = jwks(Vec::new());
    access_only.keys.push(jwk);
    assert_eq!(
        AccountClient::with_transport(FakeTransport::empty()).verify_device_attestation(
            &signed,
            "https://account.example.test",
            1_000_001,
            &mut access_only,
        ),
        Err(AttestationVerificationError::InvalidKeySet)
    );
}

#[test]
fn attestation_refresh_still_unknown_and_malformed_sets_fail_closed() {
    let (signed, _) = test_attestation("wanted", 61);
    let (_, stale) = test_attestation("stale", 62);
    let (_, still_unknown) = test_attestation("other", 63);
    let transport = FakeTransport::responding(&jwks(vec![still_unknown]));
    let mut cached = jwks(vec![stale]);
    assert_eq!(
        AccountClient::with_transport(transport.clone()).verify_device_attestation(
            &signed,
            "https://account.example.test",
            1_000_001,
            &mut cached,
        ),
        Err(AttestationVerificationError::InvalidAttestation)
    );
    assert_eq!(transport.requests.lock().unwrap().len(), 1);

    let (_, mut malformed) = test_attestation("malformed", 64);
    malformed.x.push('=');
    assert!(matches!(
        attestation_keys(&jwks(vec![malformed])),
        Err(AttestationVerificationError::InvalidKeySet)
    ));
    let (_, key) = test_attestation("bounded", 65);
    assert!(matches!(
        attestation_keys(&jwks(vec![key; 17])),
        Err(AttestationVerificationError::InvalidKeySet)
    ));
}

#[test]
fn enrollment_routes_render_exact_bounded_paths_and_put_csrf() {
    let profile = ProfileId::parse("30000000-0000-4000-8000-000000000003").unwrap();
    let device = DeviceId::parse("20000000-0000-4000-8000-000000000002").unwrap();
    assert_eq!(
        render_route_path(RouteId::ListEnrollmentCandidates, profile, None).unwrap(),
        "/v1/account/profiles/30000000-0000-4000-8000-000000000003/enrollment-candidates"
    );
    assert_eq!(
        render_route_path(RouteId::IssueDeviceAttestation, profile, None).unwrap(),
        "/v1/account/profiles/30000000-0000-4000-8000-000000000003/device-attestations"
    );
    assert_eq!(
        render_route_path(RouteId::ProvisionDevice, profile, Some(device)).unwrap(),
        "/v1/account/profiles/30000000-0000-4000-8000-000000000003/devices/20000000-0000-4000-8000-000000000002/provisioning"
    );
    assert!(matches!(
        render_route_path(RouteId::ProvisionDevice, profile, None),
        Err(ClientError::ContractMismatch)
    ));

    let response = ProvisionDeviceResponseV1 {
        version: Version1,
        result: ProvisioningResultV1::Provisioned,
    };
    let transport = FakeTransport::responding(&response);
    let client = AccountClient::with_transport(transport.clone())
        .with_browser_origin("https://app.example.test".into())
        .unwrap();
    let session = WebSessionToken("session-sentinel".into());
    let csrf = BrowserCsrf::new("csrf-sentinel".into()).unwrap();
    client
        .provision_device(
            profile,
            device,
            &ProvisionDeviceRequestV1 {
                version: Version1,
                attestation: DeviceAttestationEvidenceV1 {
                    compact: "signed-attestation".into(),
                },
                completion_receipt: EnrollmentCompletionReceiptV1("signed-receipt".into()),
            },
            &AccountAuthorization::WebSession {
                session: &session,
                csrf: &csrf,
            },
        )
        .unwrap();
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests[0].method, HttpMethod::Put);
    assert!(
        requests[0]
            .headers
            .iter()
            .any(|(name, value)| name == CSRF_HEADER && value == "csrf-sentinel")
    );
    assert!(
        requests[0]
            .headers
            .iter()
            .any(|(name, value)| name == "origin" && value == "https://app.example.test")
    );
}

#[test]
fn creation_decisions_send_exact_original_proof_to_distinct_public_routes() {
    let request = AccountCreationRequestV1 {
        version: Version1,
        authority: OneUseGrant("authority-sentinel".into()),
        client_state: OAuthState("state-sentinel".into()),
        pkce_verifier: Some(PkceVerifier("verifier-sentinel".into())),
    };
    for confirm in [false, true] {
        let response = AccountCreationResponseV1::Cancelled { version: Version1 };
        let transport = FakeTransport::responding(&response);
        let client = AccountClient::with_transport(transport.clone());
        let actual = if confirm {
            client.confirm_account_creation(&request)
        } else {
            client.cancel_account_creation(&request)
        }
        .unwrap();
        assert_eq!(actual, response);
        let sent = transport.requests.lock().unwrap();
        assert_eq!(sent[0].method, HttpMethod::Post);
        assert_eq!(
            sent[0].path_and_query,
            if confirm {
                "/v1/oauth/account/confirm"
            } else {
                "/v1/oauth/account/cancel"
            }
        );
        assert_eq!(sent[0].body, serde_json::to_vec(&request).unwrap());
        assert!(
            !sent[0]
                .headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        );
        for secret in ["authority-sentinel", "state-sentinel", "verifier-sentinel"] {
            assert!(!format!("{request:?} {:?}", sent[0]).contains(secret));
        }
    }
}

#[test]
fn profile_authorization_removal_uses_exact_profile_edge_and_confirmation() {
    let profile = ProfileId::parse("30000000-0000-4000-8000-000000000003").unwrap();
    let auth_identity =
        koru_account_core::AuthIdentityId::from_str("40000000-0000-4000-8000-000000000004")
            .unwrap();
    let response = ProfileAuthorizationRemovalResponseV1 {
        version: Version1,
        status: ProfileAuthorizationRemovalStatusV1::Removed,
        profile_offline_only: true,
    };
    let transport = FakeTransport::responding(&response);
    let client = AccountClient::with_transport(transport.clone());
    let access = AccessToken("access-sentinel".into());
    let removed = client
        .remove_profile_authorization(
            profile,
            &RemoveProfileAuthorizationRequestV1 {
                version: Version1,
                auth_identity_id: auth_identity,
                confirm_profile_offline_only: true,
            },
            &AccountAuthorization::Bearer(&access),
        )
        .unwrap();
    assert_eq!(removed, response);
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests[0].method, HttpMethod::Post);
    assert_eq!(
        requests[0].path_and_query,
        "/v1/account/profiles/30000000-0000-4000-8000-000000000003/auth-identities/remove"
    );
    let body: RemoveProfileAuthorizationRequestV1 =
        serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body.auth_identity_id, auth_identity);
    assert!(body.confirm_profile_offline_only);
    assert!(
        requests[0]
            .headers
            .iter()
            .any(|(name, value)| name == "authorization" && value == "Bearer access-sentinel")
    );
}
#[test]
fn pure_origin_validation_preserves_transport_policy() {
    for (origin, accepted) in [
        ("https://api.koru.do", true),
        ("https://us.api.koru.do/", true),
        ("http://localhost:8080", true),
        ("http://127.0.0.2:8080", true),
        ("http://[::1]:8080", true),
        ("http://192.168.1.2:8080", false),
        ("http://localhost.evil.test", false),
        ("https://api.koru.do/path", false),
        ("https://user@api.koru.do", false),
        ("https://api.koru.do?x=1", false),
        ("https://api.koru.do#x", false),
        ("ftp://api.koru.do", false),
    ] {
        assert_eq!(validate_api_origin(origin).is_ok(), accepted, "{origin}");
    }
}

#[test]
fn cloud_sync_token_and_descriptor_render_exact_workspace_paths() {
    let workspace =
        koru_account_core::WorkspaceId::parse("40000000-0000-4000-8000-000000000004").unwrap();
    let route = route_by_id(RouteId::IssueCloudSyncToken);
    let path = route
        .path
        .replace("{workspace_id}", &workspace.to_storage());
    assert_eq!(
        path,
        "/v1/account/workspaces/40000000-0000-4000-8000-000000000004/cloud/sync-token"
    );
    assert_eq!(
        route_for(route.method, &path).map(|value| value.id),
        Some(RouteId::IssueCloudSyncToken)
    );
    let descriptor = route_by_id(RouteId::DescribeCloudWorkspace);
    let path = descriptor
        .path
        .replace("{workspace_id}", &workspace.to_storage());
    assert_eq!(
        route_for(descriptor.method, &path).map(|value| value.id),
        Some(RouteId::DescribeCloudWorkspace)
    );
}

fn lifecycle_workspace() -> koru_account_core::WorkspaceId {
    koru_account_core::WorkspaceId::parse("40000000-0000-4000-8000-000000000004").unwrap()
}

fn lifecycle_response(
    workspace: koru_account_core::WorkspaceId,
) -> WorkspaceCloudLifecycleResponseV1 {
    WorkspaceCloudLifecycleResponseV1 {
        version: Version1,
        status: WorkspaceLifecycleStatus::Committed,
        workspace_id: workspace,
        state: WorkspaceCloudState::Enabled,
        incarnation_id: uuid::Uuid::from_u128(5),
        membership_epoch: 1,
        cloud_access_epoch: 1,
        authorization_version: 1,
    }
}

#[test]
fn workspace_cloud_lifecycle_posts_exact_paths_and_bearer_authorization() {
    let workspace = lifecycle_workspace();
    let profile = ProfileId::parse("30000000-0000-4000-8000-000000000003").unwrap();
    let operation = uuid::Uuid::from_u128(6);
    let access = AccessToken("access-sentinel".into());
    let authorization = AccountAuthorization::Bearer(&access);

    let challenge = WorkspaceCloudActivationChallengeV1 {
        version: Version1,
        challenge_id: uuid::Uuid::from_u128(7),
        workspace_id: workspace,
        signing_bytes: vec![9; 48],
        expires_at_millis: 1_000_000,
    };
    let transport = FakeTransport::responding(&challenge);
    let client = AccountClient::with_transport(transport.clone());
    assert_eq!(
        client
            .begin_workspace_cloud_activation(
                workspace,
                &BeginWorkspaceCloudActivationRequestV1 {
                    version: Version1,
                    operation_id: operation,
                    owner_profile_id: profile,
                    expected_authorization_version: 0,
                },
                &authorization,
            )
            .unwrap(),
        challenge
    );
    {
        let sent = transport.requests.lock().unwrap();
        assert_eq!(sent[0].method, HttpMethod::Post);
        assert_eq!(
            sent[0].path_and_query,
            "/v1/account/workspaces/40000000-0000-4000-8000-000000000004/cloud/activation"
        );
        assert!(
            sent[0]
                .headers
                .iter()
                .any(|(name, value)| name == "authorization" && value == "Bearer access-sentinel")
        );
    }

    let response = lifecycle_response(workspace);
    let transport = FakeTransport::responding(&response);
    let client = AccountClient::with_transport(transport.clone());
    assert_eq!(
        client
            .complete_workspace_cloud_activation(
                workspace,
                &CompleteWorkspaceCloudActivationRequestV1 {
                    version: Version1,
                    challenge_id: challenge.challenge_id,
                    signature: vec![7; 64],
                },
                &authorization,
            )
            .unwrap(),
        response
    );
    assert_eq!(
        transport.requests.lock().unwrap()[0].path_and_query,
        "/v1/account/workspaces/40000000-0000-4000-8000-000000000004/cloud/activation/complete"
    );

    let transport = FakeTransport::responding(&response);
    let client = AccountClient::with_transport(transport.clone());
    client
        .disable_workspace_cloud(
            workspace,
            &MutateWorkspaceCloudRequestV1 {
                version: Version1,
                operation_id: operation,
                owner_profile_id: profile,
                expected_authorization_version: 1,
                expected_incarnation_id: response.incarnation_id,
            },
            &authorization,
        )
        .unwrap();
    let sent = transport.requests.lock().unwrap();
    assert_eq!(sent[0].method, HttpMethod::Post);
    assert_eq!(
        sent[0].path_and_query,
        "/v1/account/workspaces/40000000-0000-4000-8000-000000000004/cloud/disable"
    );
}

#[test]
fn workspace_route_rendering_refuses_a_path_the_catalog_does_not_own() {
    let workspace = lifecycle_workspace();
    for route in [
        RouteId::BeginWorkspaceCloudActivation,
        RouteId::CompleteWorkspaceCloudActivation,
        RouteId::DisableWorkspaceCloud,
        RouteId::IssueCloudSyncToken,
        RouteId::DescribeCloudWorkspace,
    ] {
        let path = render_workspace_route_path(route, workspace).unwrap();
        assert!(!path.contains(['{', '}']));
        assert_eq!(
            route_for(route_by_id(route).method, &path).map(|value| value.id),
            Some(route)
        );
    }
    // A route with no Workspace placeholder is a contract mismatch, never a
    // request sent with the literal placeholder still in the path.
    assert!(matches!(
        render_workspace_route_path(RouteId::ListCloudWorkspaces, workspace),
        Err(ClientError::ContractMismatch)
    ));
}

#[test]
fn workspace_cloud_lifecycle_surfaces_the_hosted_refusal_code() {
    let access = AccessToken("access-sentinel".into());
    let transport = FakeTransport::empty();
    transport.responses.lock().unwrap().push_back(HttpResponse {
        status: 409,
        body: br#"{"code":"workspace_version_mismatch","message":"stale"}"#.to_vec(),
    });
    let error = AccountClient::with_transport(transport)
        .begin_workspace_cloud_activation(
            lifecycle_workspace(),
            &BeginWorkspaceCloudActivationRequestV1 {
                version: Version1,
                operation_id: uuid::Uuid::from_u128(1),
                owner_profile_id: ProfileId::parse("30000000-0000-4000-8000-000000000003").unwrap(),
                expected_authorization_version: 0,
            },
            &AccountAuthorization::Bearer(&access),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        ClientError::Api { status: 409, ref code } if code.as_str() == "workspace_version_mismatch"
    ));
}

/// A deployment that does not serve a route answers in its own words, not in
/// Koru's: the live v1 service's 404 on the binding route carries no
/// `DeveloperErrorV1`. The status survives that, because it is the only
/// thing such an answer says — and the whole v1 compatibility window turns
/// on reading it. A body that claimed to succeed is still an invalid
/// response: the envelope is only required of a refusal.
#[test]
fn a_refusal_without_the_error_envelope_keeps_its_status() {
    let access = AccessToken("access-sentinel".into());
    let bodies: [&[u8]; 4] = [
        b"<html><head><title>404 Not Found</title></head></html>",
        b"",
        br#"{"detail":"Not Found"}"#,
        br#"{"code":"not a code","message":"x"}"#,
    ];
    for body in bodies {
        let transport = FakeTransport::empty();
        transport.responses.lock().unwrap().push_back(HttpResponse {
            status: 404,
            body: body.to_vec(),
        });
        let error = AccountClient::with_transport(transport)
            .bind_cloud_catalog(
                &CloudCatalogBindingRequestV2 {
                    version: Version2,
                    held: None,
                },
                &AccountAuthorization::Bearer(&access),
            )
            .unwrap_err();
        assert_eq!(
            error,
            ClientError::ApiWithoutCode { status: 404 },
            "a 404 answered as {body:?} must keep its status"
        );
    }
    let transport = FakeTransport::empty();
    transport.responses.lock().unwrap().push_back(HttpResponse {
        status: 200,
        body: b"<html>not a binding</html>".to_vec(),
    });
    assert_eq!(
        AccountClient::with_transport(transport)
            .bind_cloud_catalog(
                &CloudCatalogBindingRequestV2 {
                    version: Version2,
                    held: None,
                },
                &AccountAuthorization::Bearer(&access),
            )
            .unwrap_err(),
        ClientError::InvalidResponse
    );
}
