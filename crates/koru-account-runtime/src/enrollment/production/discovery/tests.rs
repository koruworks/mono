// SPDX-License-Identifier: MPL-2.0

use super::*;
use base64::Engine as _;
use koru_account_client::{HttpRequest, HttpResponse, Transport, TransportError};
use koru_account_session::{DeviceAttestationClaimsV1, DeviceAttestationKeyRing};
use koru_account_wire::{DeviceAttestationResponseV1, JwkV1};
use std::sync::atomic::AtomicI64;

const ISSUED_AT: i64 = 1_000_000;
const EXPIRES_AT: i64 = ISSUED_AT + 300_000;
const ISSUER: &str = "https://account.invalid";

struct Time(AtomicI64);
impl Clock for Time {
    fn now_millis(&self) -> Option<i64> {
        Some(self.0.load(Ordering::SeqCst))
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}

struct Hosted {
    time: Arc<Time>,
    evidence: SignedDeviceAttestationV1,
    keys: JwksResponseV1,
    receive_time: i64,
    key_refresh_time: i64,
}
impl Transport for Hosted {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let body = if request.path_and_query.ends_with("/device-attestations") {
            self.time.0.store(self.receive_time, Ordering::SeqCst);
            serde_json::to_vec(&DeviceAttestationResponseV1 {
                version: Version1,
                expires_at_millis: EXPIRES_AT,
                attestation: DeviceAttestationEvidenceV1 {
                    compact: self.evidence.compact.clone(),
                },
            })
            .unwrap()
        } else if request.path_and_query.ends_with("/jwks")
            || request.path_and_query.ends_with("jwks.json")
        {
            self.time.0.store(self.key_refresh_time, Ordering::SeqCst);
            serde_json::to_vec(&self.keys).unwrap()
        } else {
            panic!("unexpected attestation fixture route")
        };
        Ok(HttpResponse {
            document_removal: false,
            status: 200,
            body,
        })
    }
}

fn keys(kid: &str, key: &ed25519_dalek::SigningKey) -> JwksResponseV1 {
    JwksResponseV1 {
        version: Version1,
        keys: vec![],
        attestation_keys: vec![JwkV1 {
            kty: "OKP".into(),
            crv: "Ed25519".into(),
            key_use: "sig".into(),
            alg: "EdDSA".into(),
            kid: kid.into(),
            x: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(key.verifying_key().as_bytes()),
        }],
        capability_keys: vec![],
    }
}

fn request(
    receive_time: i64,
    refresh_after: Option<i64>,
) -> Result<VerifiedDeviceAttestationV1, Failure> {
    let profile = ProfileId::parse("20000000-0000-4000-8000-000000000001").unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[78; 32]);
    let ring =
        DeviceAttestationKeyRing::new("current".into(), [("current".into(), key.clone())]).unwrap();
    let evidence = ring
        .sign(&DeviceAttestationClaimsV1 {
            version: koru_account_session::DEVICE_ATTESTATION_VERSION,
            issuer: ISSUER.into(),
            audience: koru_account_session::DEVICE_ATTESTATION_AUDIENCE.into(),
            koru_identity_id: "30000000-0000-4000-8000-000000000001".parse().unwrap(),
            device_id: koru_device::DeviceId::parse("10000000-0000-4000-8000-000000000001")
                .unwrap(),
            signing_public_key: DeviceSigningKey::from_secret_bytes([79; 32]).public_key(),
            endpoint_id: koru_device::ReplicationEndpointSecret::from_secret_bytes([80; 32])
                .endpoint_id(),
            authorized_profile_ids: vec![profile],
            session_id: "40000000-0000-4000-8000-000000000001".parse().unwrap(),
            token_id: "clock-regression".into(),
            issued_at_millis: ISSUED_AT,
            expires_at_millis: EXPIRES_AT,
        })
        .unwrap();
    // Request duration exceeds the skew allowance, so using the initial clock
    // would reject even a perfectly synchronized response.
    let time = Arc::new(Time(AtomicI64::new(ISSUED_AT - 40_000)));
    let cached_kid = if refresh_after.is_some() {
        "old"
    } else {
        "current"
    };
    let mut cache = Some(keys(cached_kid, &key));
    let client = AccountClient::with_transport(Arc::new(Hosted {
        time: time.clone(),
        evidence,
        keys: keys("current", &key),
        receive_time,
        key_refresh_time: refresh_after.unwrap_or(receive_time),
    }));
    let access = koru_account_wire::AccessToken("fixture".into());
    request_attestation(
        &client,
        ISSUER,
        profile,
        &AccountAuthorization::Bearer(&access),
        &mut cache,
        time.as_ref(),
    )
    .map(|(_, verified)| verified)
}

#[test]
fn proof_is_checked_after_http_and_accepts_the_tablets_small_clock_offset() {
    for received_at in [ISSUED_AT, ISSUED_AT - 1_300, ISSUED_AT - 30_000] {
        assert!(request(received_at, None).is_ok());
    }
    assert_eq!(
        request(ISSUED_AT - 30_001, None),
        Err(Failure::BadAttestation)
    );
    assert_eq!(request(EXPIRES_AT, None), Err(Failure::BadAttestation));
}

#[test]
fn proof_expiring_during_key_refresh_is_rejected() {
    assert!(request(EXPIRES_AT - 1, Some(EXPIRES_AT - 1)).is_ok());
    assert_eq!(
        request(EXPIRES_AT - 1, Some(EXPIRES_AT)),
        Err(Failure::BadAttestation)
    );
}
