// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::TokenCapabilityKeyRing;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};

const ISSUER: &str = "https://us.example.invalid";
const NOW: i64 = 1_800_000_000_000;

fn foundation() -> Value {
    serde_json::from_str(include_str!(
        "../../../../../contracts/vectors/cloud-sync-admission-v1/foundation.json"
    ))
    .unwrap()
}

/// The frozen device credential's claims, turned into a connection's: the
/// same dataset, endpoint and epochs, `connection_id` for `device_id`.
fn claims_json() -> Value {
    let device = foundation()["credential_claims"].clone();
    let mut claims = serde_json::Map::new();
    for (key, value) in device.as_object().unwrap() {
        match key.as_str() {
            "purpose" => {
                claims.insert(key.clone(), json!(APP_CONNECTION_SYNC_PURPOSE));
            }
            "device_id" => {
                claims.insert(
                    "connection_id".into(),
                    json!("21212121-2121-4121-8121-212121212121"),
                );
            }
            "iat" => {
                claims.insert("access".into(), json!("read_write"));
                claims.insert("stage".into(), json!("full"));
                claims.insert(key.clone(), value.clone());
            }
            _ => {
                claims.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(claims)
}

fn claims() -> AppConnectionSyncCredentialClaimsV1 {
    serde_json::from_value(claims_json()).unwrap()
}

fn keys() -> TokenCapabilityKeyRing {
    TokenCapabilityKeyRing::new(
        Region::Us,
        "regional".into(),
        [("regional".into(), SigningKey::from_bytes(&[1; 32]))],
    )
    .unwrap()
}

fn verifier() -> TokenCapabilityVerifier {
    keys().verifier(ISSUER.into()).unwrap()
}

/// Sign arbitrary payload bytes with the fixture key, bypassing the signing
/// validation, so the verifier's own refusals can be reached.
fn signed(payload: &[u8], kind: &str) -> String {
    let header = json!({"alg":"EdDSA","typ":kind,"kid":"regional"});
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(payload)
    );
    format!(
        "{}.{}",
        input,
        URL_SAFE_NO_PAD.encode(
            SigningKey::from_bytes(&[1; 32])
                .sign(input.as_bytes())
                .to_bytes()
        )
    )
}

fn code(token: &[u8], now: i64) -> &'static str {
    verify_app_connection_credential(&verifier(), token, now)
        .map(|_| "ok")
        .unwrap_or_else(|error| error.code())
}

#[test]
fn signed_credential_verifies_and_keeps_every_claim_in_order() {
    let signed = keys()
        .sign_app_connection_sync_credential(ISSUER, &claims())
        .unwrap();
    assert_eq!(signed.expires_at_seconds(), claims().exp);
    let verified = verify_app_connection_credential(&verifier(), signed.compact(), NOW).unwrap();
    let value = serde_json::to_value(verified.claims()).unwrap();
    assert_eq!(value, claims_json());
    let fields = value.as_object().unwrap().len();
    // The struct's own serialization order is the contract's field order.
    let serialized = serde_json::to_string(verified.claims()).unwrap();
    let mut at = 0;
    for key in [
        "\"ver\"",
        "\"purpose\"",
        "\"iss\"",
        "\"aud\"",
        "\"region\"",
        "\"sub\"",
        "\"profile_id\"",
        "\"connection_id\"",
        "\"client_endpoint_id\"",
        "\"workspace_id\"",
        "\"workspace_incarnation_id\"",
        "\"workspace_replica_id\"",
        "\"profile_lineage_id\"",
        "\"profile_replica_id\"",
        "\"authorization_version\"",
        "\"membership_epoch\"",
        "\"cloud_access_epoch\"",
        "\"access\"",
        "\"stage\"",
        "\"iat\"",
        "\"exp\"",
        "\"jti\"",
    ] {
        let found = serialized[at..]
            .find(key)
            .unwrap_or_else(|| panic!("{key}"));
        at += found + key.len();
    }
    assert_eq!(fields, 22);
    assert!(!serialized.contains("device_id"));
    assert_eq!(verified.claims().access, AppConnectionAccess::ReadWrite);
    assert_eq!(verified.claims().stage, AppConnectionStage::Full);
    assert_eq!(
        code(signed.compact(), claims().exp * 1000),
        "credential_expired"
    );
    assert_eq!(
        code(signed.compact(), claims().iat * 1000 - 1),
        "credential_not_yet_valid"
    );
}

#[test]
fn device_and_connection_credentials_never_verify_as_each_other() {
    // The frozen device credential, at the connection verifier.
    let device = foundation()["credential"].as_str().unwrap().to_owned();
    assert_eq!(code(device.as_bytes(), NOW), "wrong_purpose");
    // A connection credential, at the device verifier.
    let connection = keys()
        .sign_app_connection_sync_credential(ISSUER, &claims())
        .unwrap();
    assert_eq!(
        crate::cloud::verify_credential(&verifier(), connection.compact(), NOW)
            .unwrap_err()
            .code(),
        "wrong_purpose"
    );
    // The connection's header type with the device purpose, or the reverse
    // spelling of either, is refused by purpose rather than accepted.
    let mut wrong = claims_json();
    wrong["purpose"] = json!("koru:sync:device:v1");
    let token = signed(
        &serde_json::to_vec(&wrong).unwrap(),
        APP_CONNECTION_SYNC_TYPE,
    );
    assert_eq!(code(token.as_bytes(), NOW), "wrong_purpose");
    // Device-shaped claims under the connection's header are malformed.
    let device_claims = foundation()["credential_claims"].clone();
    let token = signed(
        &serde_json::to_vec(&device_claims).unwrap(),
        APP_CONNECTION_SYNC_TYPE,
    );
    assert_eq!(code(token.as_bytes(), NOW), "credential_malformed");
}

#[test]
fn strict_shape_refuses_null_unknown_duplicate_and_bad_spellings() {
    let check = |value: &[u8]| {
        let token = signed(value, APP_CONNECTION_SYNC_TYPE);
        assert_eq!(code(token.as_bytes(), NOW), "credential_malformed");
    };
    let payload = claims_json();
    for key in payload.as_object().unwrap().keys() {
        let mut bad = payload.clone();
        bad[key] = Value::Null;
        check(&serde_json::to_vec(&bad).unwrap());
        let mut missing = payload.clone();
        missing.as_object_mut().unwrap().remove(key);
        check(&serde_json::to_vec(&missing).unwrap());
    }
    let mut unknown = payload.clone();
    unknown["device_id"] = json!("11111111-1111-4111-8111-111111111111");
    check(&serde_json::to_vec(&unknown).unwrap());
    for (field, value) in [
        ("access", json!("write")),
        ("access", json!("READ")),
        ("stage", json!("identity")),
        (
            "connection_id",
            json!("21212121-2121-1121-8121-212121212121"),
        ),
        (
            "connection_id",
            json!("21212121-2121-4121-8121-21212121212A"),
        ),
        ("membership_epoch", json!(-1)),
    ] {
        let mut bad = payload.clone();
        bad[field] = value;
        check(&serde_json::to_vec(&bad).unwrap());
    }
    let raw = serde_json::to_string(&payload).unwrap();
    check(
        raw.replacen(
            "\"stage\":\"full\"",
            "\"stage\":\"full\",\"stage\":\"full\"",
            1,
        )
        .as_bytes(),
    );
    assert_eq!(code(&vec![b'a'; 10_241], NOW), "credential_malformed");
}

#[test]
fn verifier_bounds_lifetime_audience_and_region() {
    let sign_with = |change: &dyn Fn(&mut Value)| {
        let mut value = claims_json();
        change(&mut value);
        signed(
            &serde_json::to_vec(&value).unwrap(),
            APP_CONNECTION_SYNC_TYPE,
        )
    };
    let iat = claims().iat;
    let ok = sign_with(&|v| v["exp"] = json!(iat + APP_CONNECTION_SYNC_MAX_SECONDS));
    assert_eq!(code(ok.as_bytes(), NOW), "ok");
    let long = sign_with(&|v| v["exp"] = json!(iat + APP_CONNECTION_SYNC_MAX_SECONDS + 1));
    assert_eq!(code(long.as_bytes(), NOW), "credential_claims_invalid");
    let audience = sign_with(&|v| v["aud"] = json!("koru-api"));
    assert_eq!(code(audience.as_bytes(), NOW), "wrong_audience");
    let issuer = sign_with(&|v| v["iss"] = json!("https://other.invalid"));
    assert_eq!(code(issuer.as_bytes(), NOW), "wrong_issuer");
    let region = sign_with(&|v| {
        v["region"] = json!("EU");
        v["iss"] = json!("https://eu.example.invalid");
    });
    assert_eq!(code(region.as_bytes(), NOW), "wrong_region");
    let version = sign_with(&|v| v["ver"] = json!(2));
    assert_eq!(
        code(version.as_bytes(), NOW),
        "credential_version_unsupported"
    );
}

#[test]
fn signing_refuses_what_the_verifier_would_and_more() {
    let keys = keys();
    let sign = |claims: &AppConnectionSyncCredentialClaimsV1, issuer: &str| {
        keys.sign_app_connection_sync_credential(issuer, claims)
            .map(|_| ())
    };
    assert!(sign(&claims(), ISSUER).is_ok());
    let mut longest = claims();
    longest.exp = longest.iat + APP_CONNECTION_SYNC_MAX_SECONDS;
    assert!(sign(&longest, ISSUER).is_ok());
    let invalid: [&dyn Fn(&mut AppConnectionSyncCredentialClaimsV1); 7] = [
        &|c| c.exp = c.iat + APP_CONNECTION_SYNC_MAX_SECONDS + 1,
        &|c| c.exp = c.iat,
        &|c| c.purpose = "koru:sync:device:v1".into(),
        &|c| c.aud = "koru-api".into(),
        &|c| c.ver = 2,
        &|c| c.iat = -1,
        &|c| c.region = Region::Eu,
    ];
    for change in invalid {
        let mut bad = claims();
        change(&mut bad);
        assert!(matches!(sign(&bad, ISSUER), Err(CryptoError::InvalidInput)));
    }
    assert!(matches!(
        sign(&claims(), "https://other.invalid"),
        Err(CryptoError::InvalidInput)
    ));
    assert_eq!(
        APP_CONNECTION_SYNC_DEFAULT_SECONDS,
        crate::cloud::issuance::SYNC_CREDENTIAL_DEFAULT_SECONDS
    );
}

#[test]
fn stage_and_access_have_one_spelling_each() {
    assert_eq!(
        serde_json::to_value([AppConnectionStage::Manifest, AppConnectionStage::Full]).unwrap(),
        json!(["manifest", "full"])
    );
    assert_eq!(
        serde_json::to_value([AppConnectionAccess::Read, AppConnectionAccess::ReadWrite]).unwrap(),
        json!(["read", "read_write"])
    );
}
