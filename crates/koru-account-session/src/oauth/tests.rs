// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{TokenCapabilityKeyRing, verify_access_token};
use ed25519_dalek::SigningKey;
use koru_account_core::{AccessTokenClaims, AccountSessionId, TokenSigner};
use std::str::FromStr;

const ISSUER: &str = "https://us.example.invalid";
const RESOURCE: &str = "https://mcp.example.invalid/mcp";
const NOW: i64 = 1_800_000_000_000;

fn capability() -> TokenCapabilityKeyRing {
    TokenCapabilityKeyRing::new(
        Region::Us,
        "capability".into(),
        [("capability".into(), SigningKey::from_bytes(&[1; 32]))],
    )
    .unwrap()
}

fn access_ring() -> Ed25519KeyRing {
    Ed25519KeyRing::new(
        "access".into(),
        [("access".into(), SigningKey::from_bytes(&[2; 32]))],
    )
    .unwrap()
}

fn verifier(
    keys: std::collections::BTreeMap<String, ed25519_dalek::VerifyingKey>,
) -> TokenCapabilityVerifier {
    TokenCapabilityVerifier::new(Region::Us, ISSUER.into(), keys).unwrap()
}

fn access_claims(scope: McpScope) -> McpAccessTokenClaimsV1 {
    McpAccessTokenClaimsV1 {
        ver: 1,
        purpose: MCP_ACCESS_PURPOSE.into(),
        iss: ISSUER.into(),
        aud: RESOURCE.into(),
        region: Region::Us,
        sub: KoruIdentityId::from_str("11111111-1111-4111-8111-111111111111").unwrap(),
        client_id: "https://claude.example/client.json".into(),
        connection: AppConnectionId::from_str("22222222-2222-4222-8222-222222222222").unwrap(),
        scope,
        iat: NOW / 1000,
        exp: NOW / 1000 + MCP_ACCESS_SECONDS,
        jti: CloudId::from_bytes([3; 16]),
    }
}

fn request_claims() -> OAuthRequestClaimsV1 {
    OAuthRequestClaimsV1 {
        ver: 1,
        purpose: OAUTH_REQUEST_PURPOSE.into(),
        iss: ISSUER.into(),
        aud: OAUTH_REQUEST_AUDIENCE.into(),
        region: Region::Us,
        client_id: "dcr:44444444-4444-4444-8444-444444444444".into(),
        redirect_uri: "https://assistant.example/callback".into(),
        state: Some("opaque-state".into()),
        code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM".into(),
        resource: RESOURCE.into(),
        scope: McpScope::READ_WRITE,
        locale: "en".into(),
        iat: NOW / 1000,
        exp: NOW / 1000 + OAUTH_REQUEST_SECONDS,
        jti: CloudId::from_bytes([5; 16]),
    }
}

#[test]
fn an_access_token_verifies_for_its_resource_with_every_claim() {
    let keys = capability();
    let signed = keys
        .sign_mcp_access_token(ISSUER, &access_claims(McpScope::READ))
        .unwrap();
    let verified = verify_mcp_access_token(
        &verifier(keys.verification_keys()),
        signed.compact.expose_secret(),
        RESOURCE,
        NOW,
    )
    .unwrap();
    assert_eq!(verified.claims().scope, McpScope::READ);
    assert_eq!(
        verified.claims().client_id,
        "https://claude.example/client.json"
    );
    // Another resource is another audience.
    assert_eq!(
        verify_mcp_access_token(
            &verifier(keys.verification_keys()),
            signed.compact.expose_secret(),
            "https://other.example/mcp",
            NOW,
        )
        .unwrap_err(),
        Error::WrongAudience
    );
    assert_eq!(
        verify_mcp_access_token(
            &verifier(keys.verification_keys()),
            signed.compact.expose_secret(),
            RESOURCE,
            NOW + MCP_ACCESS_SECONDS * 1000,
        )
        .unwrap_err(),
        Error::CredentialExpired
    );
}

/// The MCP host refuses an account token, and an account route refuses an
/// MCP token: each fails at the header type before any claim is read.
#[test]
fn account_and_mcp_tokens_never_verify_as_each_other() {
    let access = access_ring();
    let account = access
        .sign_access_token(&AccessTokenClaims {
            issuer: ISSUER.into(),
            audience: "koru-account".into(),
            subject: KoruIdentityId::from_str("11111111-1111-4111-8111-111111111111").unwrap(),
            session_id: AccountSessionId::from_str("33333333-3333-4333-8333-333333333333").unwrap(),
            token_id: "token".into(),
            issued_at_millis: NOW,
            expires_at_millis: NOW + crate::ACCESS_TOKEN_MILLIS,
        })
        .unwrap();
    // Even a verifier wrongly holding the access ring's keys refuses it.
    assert_eq!(
        verify_mcp_access_token(
            &verifier(access.verification_keys()),
            account.compact.expose_secret(),
            RESOURCE,
            NOW,
        )
        .unwrap_err(),
        Error::WrongPurpose
    );

    let keys = capability();
    let mcp = keys
        .sign_mcp_access_token(ISSUER, &access_claims(McpScope::READ_WRITE))
        .unwrap();
    // With the capability keys too: the account verifier wants `typ` JWT.
    for ring in [access.verification_keys(), keys.verification_keys()] {
        assert!(
            verify_access_token(mcp.compact.expose_secret(), &ring, ISSUER, RESOURCE, NOW).is_err()
        );
    }
}

#[test]
fn signing_refuses_what_the_verifier_would() {
    let keys = capability();
    let mut long = access_claims(McpScope::READ);
    long.exp += 1;
    let mut purpose = access_claims(McpScope::READ);
    purpose.purpose = "koru:sync:app-connection:v1".into();
    let mut issuer = access_claims(McpScope::READ);
    issuer.iss = "https://elsewhere.example".into();
    let mut client = access_claims(McpScope::READ);
    client.client_id = String::new();
    for claims in [long, purpose, issuer, client] {
        assert_eq!(
            keys.sign_mcp_access_token(ISSUER, &claims).unwrap_err(),
            CryptoError::InvalidInput
        );
    }
}

#[test]
fn scope_has_two_spellings_and_requests_imply_read() {
    assert_eq!(McpScope::parse("documents:read"), Some(McpScope::READ));
    assert_eq!(
        McpScope::parse("documents:read documents:write"),
        Some(McpScope::READ_WRITE)
    );
    for refused in [
        "documents:write",
        "documents:write documents:read",
        "documents:read  documents:write",
        "",
    ] {
        assert_eq!(McpScope::parse(refused), None, "{refused:?}");
        assert!(serde_json::from_str::<McpScope>(&format!("{refused:?}")).is_err());
    }
    assert_eq!(McpScope::parse_request(None), Some(McpScope::READ_WRITE));
    assert_eq!(
        McpScope::parse_request(Some("  ")),
        Some(McpScope::READ_WRITE)
    );
    assert_eq!(
        McpScope::parse_request(Some("documents:read")),
        Some(McpScope::READ)
    );
    assert_eq!(
        McpScope::parse_request(Some("documents:write")),
        Some(McpScope::READ_WRITE)
    );
    assert_eq!(
        McpScope::parse_request(Some("documents:write documents:read documents:read")),
        Some(McpScope::READ_WRITE)
    );
    assert_eq!(McpScope::parse_request(Some("documents:read admin")), None);
    assert_eq!(
        McpScope::READ_WRITE.intersect(McpScope::READ),
        McpScope::READ
    );
    assert_eq!(
        McpScope::READ_WRITE.intersect(McpScope::READ_WRITE),
        McpScope::READ_WRITE
    );
    assert_eq!(
        McpScope::READ_WRITE.values(),
        &["documents:read", "documents:write"]
    );
}

#[test]
fn a_request_token_round_trips_and_expires() {
    let ring = access_ring();
    let signed = ring.sign_oauth_request(ISSUER, &request_claims()).unwrap();
    let check = verifier(ring.verification_keys());
    let verified = verify_oauth_request(&check, signed.compact.expose_secret(), NOW).unwrap();
    assert_eq!(verified.claims(), &request_claims());
    assert_eq!(
        verify_oauth_request(
            &check,
            signed.compact.expose_secret(),
            NOW + OAUTH_REQUEST_SECONDS * 1000
        )
        .unwrap_err(),
        Error::CredentialExpired
    );
    // A request token is not an access token, nor the reverse.
    assert_eq!(
        verify_mcp_access_token(&check, signed.compact.expose_secret(), RESOURCE, NOW).unwrap_err(),
        Error::WrongPurpose
    );
    let mcp = capability()
        .sign_mcp_access_token(ISSUER, &access_claims(McpScope::READ))
        .unwrap();
    assert!(
        verify_oauth_request(
            &verifier(capability().verification_keys()),
            mcp.compact.expose_secret(),
            NOW
        )
        .is_err()
    );
    // A tampered payload fails its signature.
    let text = String::from_utf8(signed.compact.expose_secret().to_vec()).unwrap();
    let mut parts: Vec<&str> = text.split('.').collect();
    let other = ring
        .sign_oauth_request(
            ISSUER,
            &OAuthRequestClaimsV1 {
                redirect_uri: "https://attacker.example/cb".into(),
                ..request_claims()
            },
        )
        .unwrap();
    let other_text = String::from_utf8(other.compact.expose_secret().to_vec()).unwrap();
    parts[1] = other_text.split('.').nth(1).unwrap();
    assert_eq!(
        verify_oauth_request(&check, parts.join(".").as_bytes(), NOW).unwrap_err(),
        Error::Signature
    );
}

#[test]
fn a_request_token_refuses_unbounded_fields() {
    let ring = access_ring();
    let mut long_state = request_claims();
    long_state.state = Some("s".repeat(MAX_STATE_BYTES + 1));
    let mut plain = request_claims();
    plain.code_challenge = "plain-challenge".into();
    let mut long_life = request_claims();
    long_life.exp += 1;
    for claims in [long_state, plain, long_life] {
        assert!(ring.sign_oauth_request(ISSUER, &claims).is_err());
    }
}
