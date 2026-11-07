// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::TokenCapabilityKeyRing;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};

const ISSUER: &str = "https://us.example.invalid";
const NOW: i64 = 1_800_000_000_000;
const CONNECTION: &str = "21212121-2121-4121-8121-212121212121";

fn foundation() -> Value {
    serde_json::from_str(include_str!(
        "../../../../../contracts/vectors/cloud-sync-admission-v1/foundation.json"
    ))
    .unwrap()
}

fn device_claims() -> SyncCredentialClaimsV1 {
    serde_json::from_value(foundation()["credential_claims"].clone()).unwrap()
}

/// The frozen device credential's claims as a connection's, the way
/// `app_connection::tests` builds them.
fn connection_claims(access: &str, stage: &str) -> AppConnectionSyncCredentialClaimsV1 {
    let mut claims = serde_json::Map::new();
    for (key, value) in foundation()["credential_claims"].as_object().unwrap() {
        match key.as_str() {
            "purpose" => {
                claims.insert(key.clone(), json!("koru:sync:app-connection:v1"));
            }
            "device_id" => {
                claims.insert("connection_id".into(), json!(CONNECTION));
            }
            "iat" => {
                claims.insert("access".into(), json!(access));
                claims.insert("stage".into(), json!(stage));
                claims.insert(key.clone(), value.clone());
            }
            _ => {
                claims.insert(key.clone(), value.clone());
            }
        }
    }
    serde_json::from_value(Value::Object(claims)).unwrap()
}

fn keys(seed: u8) -> TokenCapabilityKeyRing {
    TokenCapabilityKeyRing::new(
        Region::Us,
        "regional".into(),
        [("regional".into(), SigningKey::from_bytes(&[seed; 32]))],
    )
    .unwrap()
}

fn verifier() -> TokenCapabilityVerifier {
    keys(1).verifier(ISSUER.into()).unwrap()
}

#[test]
fn a_device_credential_is_admitted_as_its_device() {
    let claims = device_claims();
    let signed = keys(1).sign_sync_credential(ISSUER, &claims).unwrap();
    let verified = verify_session_credential(&verifier(), signed.compact(), NOW).unwrap();
    let session = verified.claims();
    assert_eq!(
        session.principal,
        SessionPrincipal::Device {
            device_id: claims.device_id
        }
    );
    assert!(*session == SessionClaims::from(claims.clone()));
    assert_eq!(session.client_endpoint_id, claims.client_endpoint_id);
    assert_eq!(session.exp, claims.exp);
    // The verified times travel with the projection.
    assert_eq!(
        verified
            .verify_time_at(claims.exp * 1000)
            .unwrap_err()
            .code(),
        "credential_expired"
    );
}

#[test]
fn a_connection_credential_is_admitted_as_its_connection_with_access_and_stage() {
    for (access, stage, expected_access, expected_stage) in [
        (
            "read",
            "manifest",
            AppConnectionAccess::Read,
            AppConnectionStage::Manifest,
        ),
        (
            "read_write",
            "full",
            AppConnectionAccess::ReadWrite,
            AppConnectionStage::Full,
        ),
    ] {
        let claims = connection_claims(access, stage);
        let signed = keys(1)
            .sign_app_connection_sync_credential(ISSUER, &claims)
            .unwrap();
        let verified = verify_session_credential(&verifier(), signed.compact(), NOW).unwrap();
        let session = verified.claims();
        assert_eq!(
            session.principal,
            SessionPrincipal::AppConnection {
                connection_id: claims.connection_id,
                access: expected_access,
                stage: expected_stage,
            }
        );
        assert!(*session == SessionClaims::from(claims.clone()));
        assert_eq!(session.workspace_id, claims.workspace_id);
        assert_eq!(session.profile_replica_id, claims.profile_replica_id);
    }
}

#[test]
fn only_a_foreign_header_is_retried_and_every_other_refusal_is_final() {
    // A connection credential signed by an unknown key is refused by the
    // connection's verifier, not reported as the device's wrong purpose.
    let stranger = keys(9)
        .sign_app_connection_sync_credential(ISSUER, &connection_claims("read", "full"))
        .unwrap();
    assert_eq!(
        verify_session_credential(&verifier(), stranger.compact(), NOW)
            .map(|_| ())
            .unwrap_err()
            .code(),
        verify_app_connection_credential(&verifier(), stranger.compact(), NOW)
            .map(|_| ())
            .unwrap_err()
            .code(),
    );
    // A device credential the device verifier refuses for any reason but its
    // header is never re-read as a connection's.
    let device = keys(1)
        .sign_sync_credential(ISSUER, &device_claims())
        .unwrap();
    let expired = device_claims().exp * 1000;
    assert_eq!(
        verify_session_credential(&verifier(), device.compact(), expired)
            .map(|_| ())
            .unwrap_err(),
        Error::CredentialExpired
    );
    // A validly signed token of a third kind is refused by both verifiers, as
    // the wrong purpose, and a malformed one stays malformed.
    let foreign = signed(
        &serde_json::to_vec(&foundation()["credential_claims"]).unwrap(),
        super::super::blob::BLOB_CAPABILITY_TYPE,
    );
    assert_eq!(
        verify_session_credential(&verifier(), foreign.as_bytes(), NOW)
            .map(|_| ())
            .unwrap_err(),
        Error::WrongPurpose
    );
    assert_eq!(
        verify_session_credential(&verifier(), b"not.a.token", NOW)
            .map(|_| ())
            .unwrap_err(),
        Error::Malformed
    );
}

/// Sign arbitrary payload bytes under any header type with the fixture key.
fn signed(payload: &[u8], kind: &str) -> String {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::Signer;
    let header = json!({"alg":"EdDSA","typ":kind,"kid":"regional"});
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(payload)
    );
    let signature = SigningKey::from_bytes(&[1; 32]).sign(input.as_bytes());
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
}
