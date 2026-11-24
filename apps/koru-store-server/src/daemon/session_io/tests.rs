// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_device::cloud::{CloudId, CloudPrincipalV1, CloudReplicaPrincipalV1};
use koru_device::{ReplicationEndpointId, ReplicationEndpointSecret};
use koru_network_edge::{NetworkPrincipal, ProtocolToken};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn endpoint() -> ReplicationEndpointId {
    ReplicationEndpointSecret::generate().endpoint_id()
}

fn principal() -> NetworkPrincipal {
    NetworkPrincipal::Cloud {
        cloud: CloudPrincipalV1 {
            version: 1,
            permit_id: uuid::Uuid::new_v4(),
            account_id: CloudId::from_bytes(*uuid::Uuid::new_v4().as_bytes()),
            profile_id: CloudId::from_bytes(*uuid::Uuid::new_v4().as_bytes()),
            dataset: CloudReplicaPrincipalV1::Workspace {
                workspace_id: CloudId::from_bytes(*uuid::Uuid::new_v4().as_bytes()),
                incarnation_id: CloudId::from_bytes(*uuid::Uuid::new_v4().as_bytes()),
                logical_replica_id: CloudId::from_bytes(*uuid::Uuid::new_v4().as_bytes()),
            },
            client_endpoint_id: endpoint(),
            worker_endpoint_id: endpoint(),
            credential_id: CloudId::from_bytes(*uuid::Uuid::new_v4().as_bytes()),
            authorization_version: 1,
            membership_epoch: 1,
            cloud_access_epoch: 1,
            authority_workspace_id: CloudId::from_bytes(*uuid::Uuid::new_v4().as_bytes()),
            authority_workspace_incarnation_id: CloudId::from_bytes(
                *uuid::Uuid::new_v4().as_bytes(),
            ),
            control_session_id: uuid::Uuid::new_v4(),
            account_generation: 1,
            session_generation: 1,
            expires_at_seconds: 600,
            credential_issued_at_seconds: None,
        },
    }
}

fn pair(capacity: usize) -> (TransportSession, TransportSession) {
    TransportSession::pair(
        principal(),
        principal(),
        ProtocolToken::parse("koru-cloud-content-v1").unwrap(),
        capacity,
    )
}

#[tokio::test]
async fn bytes_cross_in_order_and_a_dropped_writer_is_end_of_stream() {
    let (a, b) = pair(2);
    let (mut a_reader, mut a_writer) = split(a);
    let (mut b_reader, mut b_writer) = split(b);

    a_writer.write_all(b"hello ").await.unwrap();
    a_writer.write_all(b"world").await.unwrap();
    a_writer.flush().await.unwrap();
    let mut heard = [0; 11];
    b_reader.read_exact(&mut heard).await.unwrap();
    assert_eq!(&heard, b"hello world");

    b_writer.write_all(&[7; 10_000]).await.unwrap();
    drop(b_writer);
    let mut all = Vec::new();
    a_reader.read_to_end(&mut all).await.unwrap();
    assert_eq!(all.len(), 10_000);
    assert!(all.iter().all(|byte| *byte == 7));

    // A shutdown is the same half-close, observed as end of stream.
    a_writer.shutdown().await.unwrap();
    let mut tail = Vec::new();
    b_reader.read_to_end(&mut tail).await.unwrap();
    assert!(tail.is_empty());
}

#[tokio::test]
async fn a_full_channel_holds_the_write_until_the_reader_drains_it() {
    let (a, b) = pair(1);
    let (_a_reader, mut a_writer) = split(a);
    let (mut b_reader, _b_writer) = split(b);
    // Two writes into a channel of one: the second is held in flight and
    // completed by the flush, never dropped and never reordered.
    a_writer.write_all(b"1").await.unwrap();
    a_writer.write_all(b"2").await.unwrap();
    let flush = tokio::spawn(async move {
        a_writer.flush().await.unwrap();
        a_writer
    });
    let mut first = [0; 1];
    b_reader.read_exact(&mut first).await.unwrap();
    let mut second = [0; 1];
    b_reader.read_exact(&mut second).await.unwrap();
    assert_eq!((&first, &second), (b"1", b"2"));
    let _writer = flush.await.unwrap();
}

#[tokio::test]
async fn a_reset_is_a_connection_reset_not_a_clean_end() {
    let (a, b) = pair(2);
    let control = a.control();
    let (_a_reader, _a_writer) = split(a);
    let (mut b_reader, _b_writer) = split(b);
    control.reset(1);
    drop(_a_writer);
    let mut sink = Vec::new();
    let error = b_reader.read_to_end(&mut sink).await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
}
