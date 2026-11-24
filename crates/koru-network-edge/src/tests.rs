// SPDX-License-Identifier: MPL-2.0

//! The union at the edge is a wire contract, so these tests read the frozen
//! JSON in `contracts/vectors/host-control-v1/` rather
//! than a locally constructed object: a refactor that still round-trips its
//! own output would not notice that it had renamed a field.

use super::*;

#[tokio::test]
async fn abandoning_an_idle_reader_wakes_its_transport_sender() {
    let protocol = ProtocolToken::parse("koru-sync-v1").unwrap();
    let (a, b) = TransportSession::pair(principal(), principal(), protocol, 1);
    let mut closed = Box::pin(a.send.closed());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(1), &mut closed)
            .await
            .is_err()
    );
    drop(b);
    tokio::time::timeout(std::time::Duration::from_secs(1), closed)
        .await
        .unwrap();
}
use koru_device::ReplicationEndpointSecret;
use koru_device::cloud::{CloudId, CloudPrincipalV1, CloudReplicaPrincipalV1};

const PRINCIPALS: &str = include_str!("../../../contracts/vectors/host-control-v1/principals.json");

fn vectors() -> serde_json::Value {
    serde_json::from_str(PRINCIPALS).expect("host-control-v1 principals.json parses")
}

pub(crate) fn device_principal() -> DevicePrincipal {
    let endpoint = ReplicationEndpointSecret::generate();
    DevicePrincipal {
        device_id: DeviceId::new(),
        relationship_id: RelationshipId::new(),
        endpoint_id: endpoint.endpoint_id(),
        authorization_revision: 7,
    }
}

fn principal() -> NetworkPrincipal {
    device_principal().into()
}

fn cloud_id(byte: u8) -> CloudId {
    let mut bytes = [byte; 16];
    bytes[6] = 0x40 | (bytes[6] & 0x0f);
    bytes[8] = 0x80 | (bytes[8] & 0x3f);
    CloudId::from_bytes(bytes)
}

pub(crate) fn cloud_principal() -> CloudPrincipalV1 {
    CloudPrincipalV1 {
        version: 1,
        permit_id: cloud_id(0x11).as_uuid(),
        account_id: cloud_id(0x22),
        profile_id: cloud_id(0x33),
        dataset: CloudReplicaPrincipalV1::Workspace {
            workspace_id: cloud_id(0x44),
            incarnation_id: cloud_id(0x55),
            logical_replica_id: cloud_id(0x66),
        },
        client_endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
        worker_endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
        credential_id: cloud_id(0x77),
        authorization_version: 3,
        membership_epoch: 4,
        cloud_access_epoch: 5,
        authority_workspace_id: cloud_id(0x88),
        authority_workspace_incarnation_id: cloud_id(0x99),
        control_session_id: cloud_id(0xaa).as_uuid(),
        account_generation: 2,
        session_generation: 1,
        expires_at_seconds: 1_800_000_000,
        credential_issued_at_seconds: None,
    }
}

#[tokio::test]
async fn fake_edge_is_ordered_bounded_half_closable_and_cancellable() {
    let protocol = ProtocolToken::parse("koru/replication/1").unwrap();
    let (a, mut b) = TransportSession::pair(principal(), principal(), protocol, 1);
    a.send.try_send(vec![1]).unwrap();
    assert!(matches!(
        a.send.try_send(vec![2]),
        Err(mpsc::error::TrySendError::Full(_))
    ));
    assert_eq!(b.receive.recv().await.unwrap(), Some(vec![1]));
    a.cancel();
    assert!(a.is_cancelled());
    drop(a);
    assert_eq!(b.receive.recv().await, Err(EdgeError::Cancelled));
}

#[test]
fn protocol_tokens_are_bounded_ascii_values() {
    assert!(ProtocolToken::parse("koru/replication/1").is_ok());
    assert_eq!(ProtocolToken::parse(""), Err(EdgeError::InvalidProtocol));
    assert_eq!(
        ProtocolToken::parse("réplication"),
        Err(EdgeError::InvalidProtocol)
    );
}

#[tokio::test]
async fn a_cloud_session_carries_its_own_principal_arm() {
    let protocol = ProtocolToken::parse("koru-cloud-content-v1").unwrap();
    let cloud: NetworkPrincipal = cloud_principal().into();
    let (a, b) = TransportSession::pair(cloud.clone(), cloud, protocol, 1);
    assert!(a.principal.device().is_none());
    assert!(a.principal.cloud().is_some());
    assert!(b.principal.is_cloud());
}

#[test]
fn frozen_principal_and_target_arms_round_trip_exactly() {
    let vectors = vectors();
    for case in vectors["principals"].as_array().unwrap() {
        let json = case["json"].clone();
        let name = case["case"].as_str().unwrap();
        if case["valid"].as_bool().unwrap() {
            let principal: NetworkPrincipal = serde_json::from_value(json.clone())
                .unwrap_or_else(|error| panic!("{name} should decode: {error}"));
            assert_eq!(
                serde_json::to_value(&principal).unwrap(),
                json,
                "{name} must re-encode byte-for-byte"
            );
            match case["kind"].as_str().unwrap() {
                "device" => assert!(principal.device().is_some(), "{name} is the device arm"),
                "cloud" => assert!(principal.cloud().is_some(), "{name} is the Cloud arm"),
                other => panic!("{name} names an unknown arm {other}"),
            }
        } else {
            assert!(
                serde_json::from_value::<NetworkPrincipal>(json).is_err(),
                "{name} must be refused"
            );
        }
    }

    for case in vectors["targets"].as_array().unwrap() {
        let json = case["json"].clone();
        let name = case["case"].as_str().unwrap();
        if case["valid"].as_bool().unwrap() {
            let target: NetworkTarget = serde_json::from_value(json.clone())
                .unwrap_or_else(|error| panic!("{name} should decode: {error}"));
            assert_eq!(
                serde_json::to_value(target).unwrap(),
                json,
                "{name} must re-encode byte-for-byte"
            );
            assert_eq!(
                target.device_id().is_some(),
                case["kind"] == "device",
                "{name} names the wrong arm"
            );
        } else {
            assert!(
                serde_json::from_value::<NetworkTarget>(json).is_err(),
                "{name} must be refused"
            );
        }
    }
}

#[test]
fn a_cloud_principal_is_validated_for_shape_and_carries_no_secret() {
    let principal = cloud_principal();
    assert_eq!(principal.validate(), Ok(()));
    // Nothing in this value is a secret, so `Debug` redacts nothing — but it
    // must also never have grown a bearer field by accident.
    let rendered = format!("{principal:?}");
    assert!(rendered.contains("CloudPrincipalV1"));
    assert!(!rendered.to_ascii_lowercase().contains("secret"));
    assert!(!rendered.to_ascii_lowercase().contains("token"));

    let mut zero_session = principal;
    zero_session.session_generation = 0;
    assert_eq!(
        zero_session.validate(),
        Err("cloud_principal_session_generation")
    );
    let mut expired = principal;
    expired.expires_at_seconds = 0;
    assert_eq!(expired.validate(), Err("cloud_principal_expiry"));
    let mut wrong_version = principal;
    wrong_version.version = 2;
    assert_eq!(wrong_version.validate(), Err("cloud_principal_version"));
}
