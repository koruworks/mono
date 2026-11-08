// SPDX-License-Identifier: MPL-2.0

use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use koru_account_session::cloud::SyncCredentialClaimsV1;
use koru_device::DeviceSigningKey;
use std::sync::atomic::{AtomicUsize, Ordering};

fn vectors() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../contracts/vectors/cloud-sync-admission-v1/foundation.json"
    ))
    .unwrap()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
struct Provider {
    facts: CurrentAuthority,
    calls: AtomicUsize,
    refuse_after: usize,
    refusal: Refusal,
}
impl Authority for Provider {
    fn current(
        &self,
        _: &SessionClaims,
        _: CloudReplicaPrincipalV1,
    ) -> Result<CurrentAuthority, Refusal> {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 > self.refuse_after {
            return Err(self.refusal);
        }
        Ok(self.facts.clone())
    }
}
/// The worker's own assignment rows, which since D231 reach admission only
/// through `Authority::current` — no signed proof carries them any more.
fn assignment(v: &serde_json::Value, index: usize) -> CurrentAuthority {
    let claims: SyncCredentialClaimsV1 =
        serde_json::from_value(v["credential_claims"].clone()).unwrap();
    let rows = &v["sessions"][index]["assignment"];
    let id =
        |value: &serde_json::Value| -> CloudId { serde_json::from_value(value.clone()).unwrap() };
    CurrentAuthority {
        device_key: DeviceSigningKey::from_secret_bytes([3; 32]).public_key(),
        registered_endpoint: claims.client_endpoint_id,
        dataset: serde_json::from_value(rows["dataset"].clone()).unwrap(),
        authorization_version: claims.authorization_version,
        membership_epoch: claims.membership_epoch,
        cloud_access_epoch: claims.cloud_access_epoch,
        worker_endpoint: serde_json::from_value(rows["worker_endpoint_id"].clone()).unwrap(),
        worker_id: id(&rows["worker_id"]),
        lease_id: id(&rows["lease_id"]),
        lease_epoch: rows["lease_epoch"].as_i64().unwrap(),
    }
}
fn provider(v: &serde_json::Value, index: usize) -> Provider {
    Provider {
        facts: assignment(v, index),
        calls: AtomicUsize::new(0),
        refuse_after: usize::MAX,
        refusal: Refusal::DeviceRevoked,
    }
}
fn verifier() -> TokenCapabilityVerifier {
    // Build the purpose map through the existing public-only constructor.
    TokenCapabilityVerifier::new(
        koru_account_core::workspace_authorization::Region::Us,
        "https://us.example.invalid".into(),
        [(
            "regional".into(),
            ed25519_dalek::SigningKey::from_bytes(&[1; 32]).verifying_key(),
        )],
    )
    .unwrap()
}

fn begin(
    v: &serde_json::Value,
    index: usize,
    provider: &Provider,
    nonce: [u8; 32],
) -> Result<(PendingHandshake, HandshakeV1), Refusal> {
    PendingHandshake::begin(
        &verifier(),
        provider,
        CLOUD_ALPN_V1,
        decode(
            v["sessions"][index]["messages"][0]["json"]
                .as_str()
                .unwrap()
                .as_bytes(),
        )
        .unwrap(),
        // The live transport's endpoints are facts of the connection, not of
        // the authority snapshot, so a mutated snapshot really does disagree.
        assignment(v, index).registered_endpoint,
        assignment(v, index).worker_endpoint,
        nonce,
        1_800_000_000_000,
        10,
    )
}

#[test]
fn exact_transcripts_frames_and_four_messages_for_both_datasets() {
    let v = vectors();
    for index in 0..2 {
        let provider = provider(&v, index);
        let session = &v["sessions"][index];
        let transcript = transcript(
            provider.facts.registered_endpoint,
            provider.facts.worker_endpoint,
            provider.facts.dataset,
            [30; 32],
            [31; 32],
            v["credential"].as_str().unwrap(),
        );
        assert_eq!(hex(&transcript), session["transcript_hex"]);
        assert_eq!(
            transcript.len(),
            session["transcript_length"].as_u64().unwrap() as usize
        );
        for message in session["messages"].as_array().unwrap() {
            assert_eq!(
                hex(
                    &encode(&decode(message["json"].as_str().unwrap().as_bytes()).unwrap())
                        .unwrap()
                ),
                message["frame_hex"]
            );
        }
        let (pending, challenge) = begin(&v, index, &provider, [31; 32]).unwrap();
        assert_eq!(
            hex(&encode(&challenge).unwrap()),
            session["messages"][1]["frame_hex"]
        );
        let prepared = pending
            .finish(
                &provider,
                decode(session["messages"][2]["json"].as_str().unwrap().as_bytes()).unwrap(),
                1_800_000_000_000,
                20,
                [32; 32],
                1,
            )
            .unwrap();
        assert_eq!(
            hex(&encode(&prepared.accepted).unwrap()),
            session["messages"][3]["frame_hex"]
        );
        assert_eq!(
            prepared.principal.update_origin(),
            provider.facts.dataset.update_origin()
        );
        // A device's session carries no claim reference either way.
        assert_eq!(prepared.claim_references, transfer::ClaimReferences::None);
        assert_eq!(prepared.admit_transfer(), Err(Refusal::SyncUnavailable));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    }
}

#[test]
fn replay_wrong_signing_key_and_late_revocation_have_zero_acceptance_ds_d07_d09() {
    let v = vectors();
    for index in 0..2 {
        let provider = provider(&v, index);
        let session = &v["sessions"][index];
        let (pending, _) = begin(&v, index, &provider, [32; 32]).unwrap();
        assert_eq!(
            pending
                .finish(
                    &provider,
                    decode(session["messages"][2]["json"].as_str().unwrap().as_bytes()).unwrap(),
                    1_800_000_000_000,
                    20,
                    [32; 32],
                    1
                )
                .err(),
            Some(Refusal::Replay)
        );
        let (pending, _) = begin(&v, index, &provider, [31; 32]).unwrap();
        let wrong = HandshakeV1::ClientFinish {
            version: wire::Version1,
            signature: Bytes(
                URL_SAFE_NO_PAD
                    .decode(session["wrong_endpoint_key_signature"].as_str().unwrap())
                    .unwrap()
                    .try_into()
                    .unwrap(),
            ),
        };
        assert_eq!(
            pending
                .finish(&provider, wrong, 1_800_000_000_000, 20, [32; 32], 1)
                .err(),
            Some(Refusal::Replay)
        );
        let mut provider = provider;
        provider.calls.store(0, Ordering::SeqCst);
        provider.refuse_after = 2;
        let (pending, _) = begin(&v, index, &provider, [31; 32]).unwrap();
        assert_eq!(
            pending
                .finish(
                    &provider,
                    decode(session["messages"][2]["json"].as_str().unwrap().as_bytes()).unwrap(),
                    1_800_000_000_000,
                    20,
                    [32; 32],
                    1
                )
                .err(),
            Some(Refusal::DeviceRevoked)
        );
    }
}

#[test]
fn live_endpoint_refusals_from_the_workers_own_rows_ds_d08_d10() {
    let v = vectors();
    for index in 0..2 {
        let mut provider = provider(&v, index);
        // Precedence step 7 without a proof to read: the assignment the worker
        // holds must name the endpoint it is actually bound to.
        provider.facts.worker_endpoint = ReplicationEndpointId::from_bytes([99; 32]);
        assert_eq!(
            begin(&v, index, &provider, [31; 32]).err().unwrap().code(),
            "endpoint_mismatch"
        );
        provider.facts.registered_endpoint = ReplicationEndpointId::from_bytes([98; 32]);
        assert_eq!(
            begin(&v, index, &provider, [31; 32]).err().unwrap().code(),
            "endpoint_mismatch"
        );
    }
}

/// DS-D10 on the device's side: the whole check the withdrawal left it. A
/// worker that authenticated as some other endpoint is refused before the hello
/// is written, so no credential reaches it and no nonce is ever signed.
#[test]
fn client_refuses_a_worker_that_is_not_the_issued_route_endpoint_ds_d10() {
    let v = vectors();
    for index in 0..2 {
        let facts = assignment(&v, index);
        let elsewhere = ReplicationEndpointId::from_bytes(
            *DeviceSigningKey::from_secret_bytes([9; 32])
                .public_key()
                .as_bytes(),
        );
        assert_ne!(elsewhere, facts.worker_endpoint);
        assert_eq!(
            ClientHandshake::begin(
                &verifier(),
                v["credential"].as_str().unwrap().into(),
                facts.dataset,
                facts.registered_endpoint,
                elsewhere,
                facts.worker_endpoint,
                [30; 32],
                1_800_000_000_000,
            )
            .err(),
            Some(Refusal::EndpointMismatch)
        );
        // The same mismatch the other way round, and the matching pair accepted.
        assert_eq!(
            ClientHandshake::begin(
                &verifier(),
                v["credential"].as_str().unwrap().into(),
                facts.dataset,
                facts.registered_endpoint,
                facts.worker_endpoint,
                elsewhere,
                [30; 32],
                1_800_000_000_000,
            )
            .err(),
            Some(Refusal::EndpointMismatch)
        );
        assert!(
            ClientHandshake::begin(
                &verifier(),
                v["credential"].as_str().unwrap().into(),
                facts.dataset,
                facts.registered_endpoint,
                facts.worker_endpoint,
                facts.worker_endpoint,
                [30; 32],
                1_800_000_000_000,
            )
            .is_ok()
        );
    }
}

/// DS-D15 without a proof to compare against. The route attestation used to
/// carry the lease that was current when the challenge was written; now the
/// only thing that notices a lease replaced between challenge and finish is the
/// worker's own authority, whose owner refuses any change to `CurrentAuthority`
/// rather than adopting whatever lease happens to be current. Admission must
/// still end in `replica_fenced` with nothing accepted.
#[test]
fn lease_replaced_between_challenge_and_finish_is_refused_ds_d15() {
    let v = vectors();
    for index in 0..2 {
        let mut provider = provider(&v, index);
        // The challenge is written under the live lease; every later read is
        // the fenced owner, exactly as `Shared::install` leaves it.
        provider.refuse_after = 1;
        provider.refusal = Refusal::ReplicaFenced;
        let (pending, challenge) = begin(&v, index, &provider, [31; 32]).unwrap();
        assert!(matches!(challenge, HandshakeV1::WorkerChallenge { .. }));
        assert_eq!(
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
                    1_800_000_000_000,
                    20,
                    [32; 32],
                    1
                )
                .err()
                .unwrap()
                .code(),
            "replica_fenced"
        );
        // The transcript signature is never even reached, so no acceptance and
        // no further authority read can follow the fence.
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }
}

#[test]
fn whole_deadline_and_expiry_refuse_without_a_session() {
    let v = vectors();
    let provider = provider(&v, 0);
    for (wall, mono, code) in [
        (1_800_000_000_000, 30_010, "handshake_timeout"),
        (1_800_000_600_000, 20, "credential_expired"),
    ] {
        let (pending, _) = begin(&v, 0, &provider, [31; 32]).unwrap();
        assert_eq!(
            pending
                .finish(
                    &provider,
                    decode(
                        v["sessions"][0]["messages"][2]["json"]
                            .as_str()
                            .unwrap()
                            .as_bytes()
                    )
                    .unwrap(),
                    wall,
                    mono,
                    [32; 32],
                    1
                )
                .err()
                .unwrap()
                .code(),
            code
        );
    }
}

#[test]
fn strict_wire_caps_and_every_required_field_null() {
    let v = vectors();
    for session in v["sessions"].as_array().unwrap() {
        for message in session["messages"].as_array().unwrap() {
            let raw = message["json"].as_str().unwrap();
            let mut cap = raw.as_bytes()[..raw.len() - 1].to_vec();
            cap.resize(MAX_HANDSHAKE_BYTES - 1, b' ');
            cap.push(b'}');
            assert!(decode(&cap).is_ok());
            cap.insert(cap.len() - 1, b' ');
            assert_eq!(decode(&cap).unwrap_err(), Refusal::Malformed);
            let value: serde_json::Value = serde_json::from_str(raw).unwrap();
            for key in value.as_object().unwrap().keys() {
                let mut bad = value.clone();
                bad[key] = serde_json::Value::Null;
                assert_eq!(
                    decode(&serde_json::to_vec(&bad).unwrap()).unwrap_err(),
                    Refusal::Malformed
                );
            }
            for insert in [",\"version\":1", ",\"unexpected\":true"] {
                let bad = format!("{}{insert}}}", &raw[..raw.len() - 1]);
                assert_eq!(decode(bad.as_bytes()).unwrap_err(), Refusal::Malformed);
            }
        }
    }
}

#[test]
fn client_signs_the_two_nonce_transcript_and_pins_the_credential_expiry() {
    let v = vectors();
    for index in 0..2 {
        let facts = provider(&v, index).facts;
        let client = || {
            ClientHandshake::begin(
                &verifier(),
                v["credential"].as_str().unwrap().into(),
                facts.dataset,
                facts.registered_endpoint,
                facts.worker_endpoint,
                facts.worker_endpoint,
                [30; 32],
                1_800_000_000_000,
            )
            .unwrap()
            .0
        };
        let challenge = || {
            decode(
                v["sessions"][index]["messages"][1]["json"]
                    .as_str()
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap()
        };
        let (receipt, finish) = client()
            .challenge(
                challenge(),
                &DeviceSigningKey::from_secret_bytes([3; 32]),
                1_800_000_000_000,
            )
            .unwrap();
        assert_eq!(
            hex(&encode(&finish).unwrap()),
            v["sessions"][index]["messages"][2]["frame_hex"]
        );
        let accepted = decode(
            v["sessions"][index]["messages"][3]["json"]
                .as_str()
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        let prepared = receipt.accepted(accepted, 1_800_000_000_000, 0).unwrap();
        assert_eq!(prepared.principal, facts.dataset);
        assert_eq!(prepared.claim_references, transfer::ClaimReferences::None);
        // A challenge whose nonce was tampered with signs different bytes; the
        // worker's own transcript check is what rejects it. The client no longer
        // has a proof to reject, so it must not invent one.
        let mut bad = challenge();
        if let HandshakeV1::WorkerChallenge { server_nonce, .. } = &mut bad {
            server_nonce.0[0] ^= 1;
        }
        let (_, tampered) = client()
            .challenge(
                bad,
                &DeviceSigningKey::from_secret_bytes([3; 32]),
                1_800_000_000_000,
            )
            .unwrap();
        assert_ne!(
            hex(&encode(&tampered).unwrap()),
            v["sessions"][index]["messages"][2]["frame_hex"]
        );
        let (receipt, _) = client()
            .challenge(
                challenge(),
                &DeviceSigningKey::from_secret_bytes([3; 32]),
                1_800_000_000_000,
            )
            .unwrap();
        let mut extended = decode(
            v["sessions"][index]["messages"][3]["json"]
                .as_str()
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        if let HandshakeV1::SessionAccepted {
            expires_at_seconds, ..
        } = &mut extended
        {
            *expires_at_seconds += 1;
        }
        assert_eq!(
            receipt.accepted(extended, 1_800_000_000_000, 0).err(),
            Some(Refusal::Malformed)
        );
    }
}

#[test]
fn exact_authority_axes_and_connection_dataset_generations() {
    let v = vectors();
    for index in 0..2 {
        for (axis, expected) in [
            (0, Refusal::ReplicaMismatch),
            (1, Refusal::IncarnationStale),
            (2, Refusal::MembershipEpoch),
            (3, Refusal::CloudAccessEpoch),
            (4, Refusal::AuthorizationStale),
        ] {
            let mut provider = provider(&v, index);
            match axis {
                0 => match &mut provider.facts.dataset {
                    CloudReplicaPrincipalV1::Workspace {
                        logical_replica_id, ..
                    }
                    | CloudReplicaPrincipalV1::Profile {
                        logical_replica_id, ..
                    } => *logical_replica_id = CloudId::from_bytes([90; 16]),
                },
                1 => match &mut provider.facts.dataset {
                    CloudReplicaPrincipalV1::Workspace { incarnation_id, .. } => {
                        *incarnation_id = CloudId::from_bytes([90; 16])
                    }
                    CloudReplicaPrincipalV1::Profile { lineage_id, .. } => {
                        *lineage_id = CloudId::from_bytes([90; 16])
                    }
                },
                2 => provider.facts.membership_epoch += 1,
                3 => provider.facts.cloud_access_epoch += 1,
                _ => provider.facts.authorization_version += 1,
            }
            assert_eq!(begin(&v, index, &provider, [31; 32]).err(), Some(expected));
        }
    }
    let mut generations = ConnectionGenerations::default();
    let workspace = provider(&v, 0).facts.dataset;
    let profile = provider(&v, 1).facts.dataset;
    assert_eq!(generations.next(workspace), Ok(1));
    assert_eq!(generations.next(workspace), Ok(2));
    assert_eq!(generations.next(profile), Ok(1));
    assert_eq!(
        workspace.admit_on_device_path(),
        Err("principal_not_allowed")
    );
    let facts = provider(&v, 0);
    assert_eq!(
        PendingHandshake::begin(
            &verifier(),
            &facts,
            b"koru/replication/1",
            decode(
                v["sessions"][0]["messages"][0]["json"]
                    .as_str()
                    .unwrap()
                    .as_bytes()
            )
            .unwrap(),
            facts.facts.registered_endpoint,
            facts.facts.worker_endpoint,
            [31; 32],
            1_800_000_000_000,
            0
        )
        .err(),
        Some(Refusal::PrincipalNotAllowed)
    );
}

#[test]
fn recognized_refusals_survive_both_client_receipt_points() {
    let v = vectors();
    let facts = provider(&v, 0).facts;
    for code in [
        "device_revoked",
        "replica_fenced",
        "authorization_unavailable",
        "wrong_region",
    ] {
        let client = || {
            ClientHandshake::begin(
                &verifier(),
                v["credential"].as_str().unwrap().into(),
                facts.dataset,
                facts.registered_endpoint,
                facts.worker_endpoint,
                facts.worker_endpoint,
                [30; 32],
                1_800_000_000_000,
            )
            .unwrap()
            .0
        };
        let refused = || HandshakeV1::Refused {
            version: Version1,
            code: code.into(),
        };
        assert_eq!(
            client()
                .challenge(
                    refused(),
                    &DeviceSigningKey::from_secret_bytes([3; 32]),
                    1_800_000_000_000
                )
                .err()
                .unwrap()
                .code(),
            code
        );
        let challenge = decode(
            v["sessions"][0]["messages"][1]["json"]
                .as_str()
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        let (receipt, _) = client()
            .challenge(
                challenge,
                &DeviceSigningKey::from_secret_bytes([3; 32]),
                1_800_000_000_000,
            )
            .unwrap();
        assert_eq!(
            receipt
                .accepted(refused(), 1_800_000_000_000, 0)
                .err()
                .unwrap()
                .code(),
            code
        );
    }
    assert_eq!(Refusal::from_code("future_denial"), Refusal::Malformed);
}

mod app_connection;
