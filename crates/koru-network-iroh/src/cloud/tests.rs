// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_account_session::cloud::SyncCredentialClaimsV1;
use koru_device::DeviceSigningKey;
use koru_replication::cloud::CurrentAuthority;

mod preparation;
mod worker_endpoint;

fn verifier() -> TokenCapabilityVerifier {
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
struct Provider {
    claims: SyncCredentialClaimsV1,
    facts: CurrentAuthority,
}
impl Authority for Provider {
    fn current(
        &self,
        _: &koru_replication::cloud::SessionClaims,
        _: koru_device::cloud::CloudReplicaPrincipalV1,
    ) -> Result<CurrentAuthority, Refusal> {
        Ok(self.facts.clone())
    }
}
fn vectors() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../contracts/vectors/cloud-sync-admission-v1/foundation.json"
    ))
    .unwrap()
}
/// Since D231 the worker's assignment rows arrive only through `Authority`,
/// never as a signed proof on the wire. The frozen vectors keep them so a test
/// can arm the authority a real worker would have read from PostgreSQL.
fn provider(v: &serde_json::Value) -> Provider {
    let claims: SyncCredentialClaimsV1 =
        serde_json::from_value(v["credential_claims"].clone()).unwrap();
    let rows = &v["sessions"][0]["assignment"];
    let facts = CurrentAuthority {
        device_key: DeviceSigningKey::from_secret_bytes([3; 32]).public_key(),
        registered_endpoint: claims.client_endpoint_id,
        dataset: serde_json::from_value(rows["dataset"].clone()).unwrap(),
        authorization_version: claims.authorization_version,
        membership_epoch: claims.membership_epoch,
        cloud_access_epoch: claims.cloud_access_epoch,
        worker_endpoint: serde_json::from_value(rows["worker_endpoint_id"].clone()).unwrap(),
        worker_id: serde_json::from_value(rows["worker_id"].clone()).unwrap(),
        lease_id: serde_json::from_value(rows["lease_id"].clone()).unwrap(),
        lease_epoch: rows["lease_epoch"].as_i64().unwrap(),
    };
    Provider { claims, facts }
}

// Type-checked without opening a socket: production adapter futures can be
// spawned by a multithreaded worker, not only joined in a local unit test.
#[allow(dead_code)]
fn production_futures_are_send(
    connection: &iroh::endpoint::Connection,
    writer: &mut iroh::endpoint::SendStream,
    reader: &mut iroh::endpoint::RecvStream,
    verifier: &TokenCapabilityVerifier,
    provider: &dyn Authority,
    generations: &mut ConnectionGenerations,
    key: &DeviceSigningKey,
    dataset: koru_device::cloud::CloudReplicaPrincipalV1,
    endpoint: ReplicationEndpointId,
) {
    fn assert_send<T: Send>(_: T) {}
    assert_send(accept(
        connection,
        writer,
        reader,
        verifier,
        provider,
        endpoint,
        generations,
        || 0,
    ));
    assert_send(authenticate(
        connection,
        writer,
        reader,
        verifier,
        "proof".into(),
        dataset,
        endpoint,
        endpoint,
        key,
        0,
        || 0,
    ));
    assert_send(accept_prepared(
        connection,
        writer,
        reader,
        verifier,
        |_| async {
            Ok((
                BorrowedAuthority(provider),
                std::future::pending::<Refusal>(),
            ))
        },
        endpoint,
        generations,
        || 0,
    ));
}

#[tokio::test]
async fn bounded_stream_completes_handshake_but_never_enables_transfer() {
    let v = vectors();
    let provider = provider(&v);
    let verifier = verifier();
    let (client, server) = tokio::io::duplex(65536);
    let (mut client_reader, mut client_writer) = tokio::io::split(client);
    let (mut reader, mut writer) = tokio::io::split(server);
    let mut generations = ConnectionGenerations::default();
    let server = exchange(
        &mut writer,
        &mut reader,
        &verifier,
        &provider,
        provider.claims.client_endpoint_id,
        provider.facts.worker_endpoint,
        &mut generations,
        || 1_800_000_000_000,
    );
    let key = DeviceSigningKey::from_secret_bytes([3; 32]);
    let client = client_exchange(
        &mut client_writer,
        &mut client_reader,
        &verifier,
        v["credential"].as_str().unwrap().into(),
        provider.facts.dataset,
        provider.claims.client_endpoint_id,
        provider.facts.worker_endpoint,
        provider.facts.worker_endpoint,
        &key,
        0,
        || 1_800_000_000_000,
    );
    let (prepared, received) = tokio::join!(server, client);
    assert_eq!(received.unwrap().generation, 1);
    assert_eq!(
        prepared.unwrap().admit_transfer(),
        Err(Refusal::SyncUnavailable)
    );
}

#[tokio::test(start_paused = true)]
async fn first_length_byte_starts_one_whole_deadline() {
    let v = vectors();
    let provider = provider(&v);
    let verifier = verifier();
    let (mut client, server) = tokio::io::duplex(64);
    let (mut reader, mut writer) = tokio::io::split(server);
    client.write_all(&[0]).await.unwrap();
    let mut generations = ConnectionGenerations::default();
    let result = exchange(
        &mut writer,
        &mut reader,
        &verifier,
        &provider,
        provider.claims.client_endpoint_id,
        provider.facts.worker_endpoint,
        &mut generations,
        || 1_800_000_000_000,
    )
    .await;
    assert_eq!(result.err(), Some(Refusal::Timeout));
}

#[tokio::test]
async fn invalid_declared_length_refuses_without_waiting_for_body() {
    for length in [0, 32769, u32::MAX] {
        let (_sender, mut receiver) = tokio::io::duplex(8);
        assert_eq!(
            read_body(&mut receiver, length.to_be_bytes())
                .await
                .unwrap_err(),
            Refusal::Malformed
        );
    }
}
