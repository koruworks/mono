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

/// The frozen device credential's device facts, with the catalog scope in
/// place of the dataset.
fn claims_json() -> Value {
    let device = foundation()["credential_claims"].clone();
    json!({
        "ver": 2,
        "purpose": CATALOG_SCOPE_SYNC_PURPOSE,
        "iss": device["iss"],
        "aud": CATALOG_SCOPE_SYNC_AUDIENCE,
        "region": "US",
        "sub": device["sub"],
        "catalog_scope_id": "31313131-3131-4131-8131-313131313131",
        "device_id": device["device_id"],
        "client_endpoint_id": device["client_endpoint_id"],
        "scope": {"kind": "user_catalog"},
        "authority_generation": 1,
        "iat": device["iat"],
        "exp": device["exp"],
        "jti": device["jti"],
    })
}

fn claims() -> CatalogScopeCredentialClaimsV2 {
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
    verify_catalog_scope_credential(&verifier(), token, now)
        .map(|_| "ok")
        .unwrap_or_else(|error| error.code())
}

#[test]
fn signed_credential_verifies_and_keeps_every_claim() {
    let signed = keys()
        .sign_catalog_scope_credential(ISSUER, &claims())
        .unwrap();
    assert_eq!(signed.expires_at_seconds(), claims().exp);
    let verified = verify_catalog_scope_credential(&verifier(), signed.compact(), NOW).unwrap();
    assert_eq!(
        serde_json::to_value(verified.claims()).unwrap(),
        claims_json()
    );
    assert_eq!(verified.claims().scope, CatalogScopeKind::UserCatalog);
    assert_eq!(
        code(signed.compact(), claims().exp * 1000),
        "credential_expired"
    );
    assert_eq!(
        code(signed.compact(), claims().iat * 1000 - 1),
        "credential_not_yet_valid"
    );
    // The personal scope names its Workspace and nothing else.
    let mut personal = claims_json();
    personal["scope"] =
        json!({"kind": "user_personal", "workspace_id": "0a0a0a0a-0a0a-4a0a-8a0a-0a0a0a0a0a0a"});
    let personal: CatalogScopeCredentialClaimsV2 = serde_json::from_value(personal).unwrap();
    let signed = keys()
        .sign_catalog_scope_credential(ISSUER, &personal)
        .unwrap();
    assert!(matches!(
        verify_catalog_scope_credential(&verifier(), signed.compact(), NOW)
            .unwrap()
            .claims()
            .scope,
        CatalogScopeKind::UserPersonal { .. }
    ));
}

#[test]
fn catalog_device_and_connection_credentials_never_verify_as_each_other() {
    // The frozen device credential, at the catalog verifier.
    let device = foundation()["credential"].as_str().unwrap().to_owned();
    assert_eq!(code(device.as_bytes(), NOW), "wrong_purpose");
    // A catalog credential, at the device verifier and the connection verifier.
    let catalog = keys()
        .sign_catalog_scope_credential(ISSUER, &claims())
        .unwrap();
    assert_eq!(
        crate::cloud::verify_credential(&verifier(), catalog.compact(), NOW)
            .unwrap_err()
            .code(),
        "wrong_purpose"
    );
    assert_eq!(
        crate::cloud::app_connection::verify_app_connection_credential(
            &verifier(),
            catalog.compact(),
            NOW
        )
        .unwrap_err()
        .code(),
        "wrong_purpose"
    );
    // The catalog header with the device purpose is refused by purpose.
    let mut wrong = claims_json();
    wrong["purpose"] = json!("koru:sync:device:v1");
    let token = signed(
        &serde_json::to_vec(&wrong).unwrap(),
        CATALOG_SCOPE_SYNC_TYPE,
    );
    assert_eq!(code(token.as_bytes(), NOW), "wrong_purpose");
    // Device-shaped claims under the catalog header are malformed.
    let device_claims = foundation()["credential_claims"].clone();
    let token = signed(
        &serde_json::to_vec(&device_claims).unwrap(),
        CATALOG_SCOPE_SYNC_TYPE,
    );
    assert_eq!(code(token.as_bytes(), NOW), "credential_malformed");
}

#[test]
fn strict_shape_refuses_null_unknown_and_bad_claims_and_signing_checks_them_too() {
    let check = |value: &[u8]| {
        let token = signed(value, CATALOG_SCOPE_SYNC_TYPE);
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
    unknown["workspace_id"] = json!("11111111-1111-4111-8111-111111111111");
    check(&serde_json::to_vec(&unknown).unwrap());
    let mut negative = payload.clone();
    negative["authority_generation"] = json!(-1);
    check(&serde_json::to_vec(&negative).unwrap());
    let mut scope = payload.clone();
    scope["scope"] =
        json!({"kind": "workspace", "workspace_id": "0a0a0a0a-0a0a-4a0a-8a0a-0a0a0a0a0a0a"});
    check(&serde_json::to_vec(&scope).unwrap());

    // Signing refuses what verification would: a zero generation, the wrong
    // version, a lifetime over the ceiling, another issuer.
    let mut zero = claims();
    zero.authority_generation = 0;
    assert!(keys().sign_catalog_scope_credential(ISSUER, &zero).is_err());
    let mut version = claims();
    version.ver = 1;
    assert!(
        keys()
            .sign_catalog_scope_credential(ISSUER, &version)
            .is_err()
    );
    let mut long = claims();
    long.exp = long.iat + CATALOG_SCOPE_SYNC_MAX_SECONDS + 1;
    assert!(keys().sign_catalog_scope_credential(ISSUER, &long).is_err());
    assert!(
        keys()
            .sign_catalog_scope_credential("https://other.example.invalid", &claims())
            .is_err()
    );
}
