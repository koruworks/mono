// SPDX-License-Identifier: MPL-2.0

//! An app connection through the unchanged four-message handshake: its
//! credential verifies by its own header type, and its transcript is signed by
//! the credential's endpoint key because a connection has no device key
//! (cloud-sync-admission, the app connection amendment).

use super::*;
use koru_account_session::{
    TokenCapabilityKeyRing, cloud::app_connection::AppConnectionSyncCredentialClaimsV1,
};

const NOW: i64 = 1_800_000_000_000;
/// The frozen vectors' client endpoint secret (`generate.py`, byte 5).
const ENDPOINT_SECRET: [u8; 32] = [5; 32];
/// The frozen vectors' device secret (byte 3), which a connection does not hold.
const DEVICE_SECRET: [u8; 32] = [3; 32];

/// The frozen device credential's claims as a connection's.
fn connection_claims(v: &serde_json::Value) -> AppConnectionSyncCredentialClaimsV1 {
    let mut claims = serde_json::Map::new();
    for (key, value) in v["credential_claims"].as_object().unwrap() {
        match key.as_str() {
            "purpose" => {
                claims.insert(key.clone(), "koru:sync:app-connection:v1".into());
            }
            "device_id" => {
                claims.insert(
                    "connection_id".into(),
                    "21212121-2121-4121-8121-212121212121".into(),
                );
            }
            "iat" => {
                claims.insert("access".into(), "read_write".into());
                claims.insert("stage".into(), "full".into());
                claims.insert(key.clone(), value.clone());
            }
            _ => {
                claims.insert(key.clone(), value.clone());
            }
        }
    }
    serde_json::from_value(serde_json::Value::Object(claims)).unwrap()
}

fn compact(v: &serde_json::Value) -> String {
    let ring = TokenCapabilityKeyRing::new(
        koru_account_core::workspace_authorization::Region::Us,
        "regional".into(),
        [(
            "regional".into(),
            ed25519_dalek::SigningKey::from_bytes(&[1; 32]),
        )],
    )
    .unwrap();
    let signed = ring
        .sign_app_connection_sync_credential("https://us.example.invalid", &connection_claims(v))
        .unwrap();
    String::from_utf8(signed.compact().to_vec()).unwrap()
}

/// The worker's rows for a connection: no device key, so the transcript key
/// is the credential's endpoint key.
fn connection_provider(v: &serde_json::Value, index: usize) -> Provider {
    let mut provider = provider(v, index);
    provider.facts.device_key = DeviceSigningKey::from_secret_bytes(ENDPOINT_SECRET).public_key();
    provider
}

fn hello(v: &serde_json::Value, index: usize, credential: String) -> HandshakeV1 {
    HandshakeV1::ClientHello {
        version: wire::Version1,
        dataset: assignment(v, index).dataset,
        client_nonce: Bytes([30; 32]),
        credential,
    }
}

fn begin_connection(
    v: &serde_json::Value,
    index: usize,
    provider: &Provider,
) -> Result<(PendingHandshake, HandshakeV1), Refusal> {
    PendingHandshake::begin(
        &verifier(),
        provider,
        CLOUD_ALPN_V1,
        hello(v, index, compact(v)),
        assignment(v, index).registered_endpoint,
        assignment(v, index).worker_endpoint,
        [31; 32],
        NOW,
        10,
    )
}

fn signed_finish(v: &serde_json::Value, index: usize, secret: [u8; 32]) -> HandshakeV1 {
    let facts = assignment(v, index);
    let transcript = transcript(
        facts.registered_endpoint,
        facts.worker_endpoint,
        facts.dataset,
        [30; 32],
        [31; 32],
        &compact(v),
    );
    HandshakeV1::ClientFinish {
        version: wire::Version1,
        signature: Bytes(
            *DeviceSigningKey::from_secret_bytes(secret)
                .sign_message(&transcript)
                .as_bytes(),
        ),
    }
}

#[test]
fn the_endpoint_key_is_the_one_that_signs_a_connections_transcript() {
    let v = vectors();
    let claims = connection_claims(&v);
    assert_eq!(
        DeviceSigningKey::from_secret_bytes(ENDPOINT_SECRET)
            .public_key()
            .as_bytes(),
        claims.client_endpoint_id.as_bytes(),
        "the vectors' client endpoint is the key byte 5 derives"
    );
    for index in 0..2 {
        let provider = connection_provider(&v, index);
        let (pending, _) = begin_connection(&v, index, &provider).unwrap();
        let prepared = pending
            .finish(
                &provider,
                signed_finish(&v, index, ENDPOINT_SECRET),
                NOW,
                20,
                [32; 32],
                1,
            )
            .unwrap();
        assert_eq!(prepared.principal, assignment(&v, index).dataset);
        assert_eq!(prepared.expires_at_seconds, claims.exp);

        // The device's key is not a connection's: its signature is a replay.
        let (pending, _) = begin_connection(&v, index, &provider).unwrap();
        assert_eq!(
            pending
                .finish(
                    &provider,
                    signed_finish(&v, index, DEVICE_SECRET),
                    NOW,
                    20,
                    [32; 32],
                    1,
                )
                .err(),
            Some(Refusal::Replay)
        );
    }
}

#[test]
fn a_provider_cannot_choose_another_key_for_a_connection() {
    let v = vectors();
    for index in 0..2 {
        // The unchanged device rows name the device's key. For a connection
        // that is refused before a challenge exists, whatever the provider says.
        let provider = provider(&v, index);
        assert_eq!(
            begin_connection(&v, index, &provider).err(),
            Some(Refusal::EndpointMismatch)
        );
        // The same rows still admit the device credential, unchanged.
        let (pending, _) = begin(&v, index, &provider, [31; 32]).unwrap();
        assert!(
            pending
                .finish(
                    &provider,
                    decode(
                        v["sessions"][index]["messages"][2]["json"]
                            .as_str()
                            .unwrap()
                            .as_bytes()
                    )
                    .unwrap(),
                    NOW,
                    20,
                    [32; 32],
                    1,
                )
                .is_ok()
        );
    }
}

#[test]
fn the_provider_reads_the_connection_principal_with_its_access_and_stage() {
    let v = vectors();
    let provider = connection_provider(&v, 0);
    let verified = VerifiedClientHello::verify(
        &verifier(),
        CLOUD_ALPN_V1,
        &hello(&v, 0, compact(&v)),
        assignment(&v, 0).registered_endpoint,
        NOW,
    )
    .unwrap();
    assert_eq!(
        verified.claims().principal,
        SessionPrincipal::AppConnection {
            connection_id: connection_claims(&v).connection_id,
            access: koru_account_core::workspace_authorization::AppConnectionAccess::ReadWrite,
            stage: koru_account_session::cloud::app_connection::AppConnectionStage::Full,
        }
    );
    assert!(
        provider
            .current(verified.claims(), verified.dataset())
            .is_ok()
    );
    // A connection credential still binds the authenticated endpoint.
    assert_eq!(
        VerifiedClientHello::verify(
            &verifier(),
            CLOUD_ALPN_V1,
            &hello(&v, 0, compact(&v)),
            assignment(&v, 0).worker_endpoint,
            NOW,
        )
        .err(),
        Some(Refusal::EndpointMismatch)
    );
}

#[test]
fn a_connection_replica_completes_the_client_side_with_its_endpoint_key() {
    let v = vectors();
    for index in 0..2 {
        let facts = assignment(&v, index);
        let provider = connection_provider(&v, index);
        let (client, client_hello) = ClientHandshake::begin(
            &verifier(),
            compact(&v),
            facts.dataset,
            facts.registered_endpoint,
            facts.worker_endpoint,
            facts.worker_endpoint,
            [30; 32],
            NOW,
        )
        .unwrap();
        let (pending, challenge) = PendingHandshake::begin(
            &verifier(),
            &provider,
            CLOUD_ALPN_V1,
            client_hello,
            facts.registered_endpoint,
            facts.worker_endpoint,
            [31; 32],
            NOW,
            10,
        )
        .unwrap();
        let (receipt, finish) = client
            .challenge(
                challenge,
                &DeviceSigningKey::from_secret_bytes(ENDPOINT_SECRET),
                NOW,
            )
            .unwrap();
        let prepared = pending
            .finish(&provider, finish, NOW, 20, [32; 32], 1)
            .unwrap();
        assert_eq!(
            receipt
                .accepted(prepared.accepted, NOW, 0)
                .unwrap()
                .principal,
            facts.dataset
        );
    }
}
