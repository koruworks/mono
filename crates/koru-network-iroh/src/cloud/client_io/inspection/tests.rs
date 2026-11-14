// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

fn unit(scope: Scope) -> Unit {
    Unit::Document {
        workspace_id: koru_device::cloud::CloudId::from_bytes(
            scope.eligibility_workspace.as_uuid().into_bytes(),
        ),
        document_id: koru_device::cloud::CloudId::from_bytes(
            koru_store_core::DocumentId::new().as_uuid().into_bytes(),
        ),
    }
}

#[tokio::test]
async fn original_join_bytes_bind_head_scope_unit_and_semantic_local_version() {
    let (mut client, mut reader, mut writer, _, scope, budget) =
        super::super::tests::pair_with_workspace(true).await;
    client.receive = Box::new(|_| panic!("inspection must never invoke Store receive"));
    client.permission = Box::new(|_| panic!("inspection must never admit a transfer"));
    let unit = unit(scope);
    let doc = loro::LoroDoc::new();
    doc.get_text("body").insert(0, "captured").unwrap();
    let version = doc.oplog_vv().encode();
    let head = koru_store_core::sync_digest::revision(&version);
    let peer = async {
        let deadline = Instant::now() + Duration::from_secs(2);
        let request = records::read(&mut reader, deadline, &budget).await.unwrap();
        let (_, Record::Peer(frame), _) = request.into_parts() else {
            panic!()
        };
        let Metadata::Request(request) = metadata::decode(scope, frame, 1).unwrap().remove(0)
        else {
            panic!()
        };
        assert_eq!(request.unit, unit);
        assert!(!request.live);
        let response = request.response(scope, version.clone()).unwrap().unwrap();
        records::write(&mut writer, &response, deadline, &budget)
            .await
            .unwrap();
    };
    let (observed, ()) = tokio::join!(probe(client, unit, version.clone()), peer);
    let observed = observed.unwrap();
    assert!(observed.matches(scope, unit, head, &doc.oplog_vv().encode()));
    assert!(!observed.matches(scope, unit, [0; 32], &version));
    assert!(!observed.matches(scope, unit, head, &loro::VersionVector::default().encode()));
    assert!(!observed.matches(scope, unit, head, &[255]));
    let other_unit = Unit::Document {
        document_id: koru_device::cloud::CloudId::from_bytes([1; 16]),
        workspace_id: koru_device::cloud::CloudId::from_bytes(
            scope.eligibility_workspace.as_uuid().into_bytes(),
        ),
    };
    assert!(!observed.matches(scope, other_unit, head, &version));
    assert!(!observed.matches(
        Scope {
            eligibility_workspace: koru_store_core::WorkspaceId::new(),
            ..scope
        },
        unit,
        head,
        &version
    ));
    let mut other = scope;
    if let koru_device::cloud::CloudReplicaPrincipalV1::Workspace {
        ref mut incarnation_id,
        ..
    } = other.principal
    {
        *incarnation_id = koru_device::cloud::CloudId::from_bytes([9; 16]);
    }
    assert!(!observed.matches(other, unit, head, &version));
    assert_eq!(format!("{observed:?}"), "ObservedVersion(<redacted>)");
}

#[tokio::test]
async fn payload_start_is_rejected_before_any_import_or_permission_callback() {
    let (mut client, mut reader, mut writer, mut stream, scope, budget) =
        super::super::tests::pair_with_workspace(true).await;
    client.receive = Box::new(|_| panic!("Store callback reached"));
    client.permission = Box::new(|_| panic!("permission callback reached"));
    let unit = unit(scope);
    let peer = async {
        let deadline = Instant::now() + Duration::from_secs(2);
        records::read(&mut reader, deadline, &budget).await.unwrap();
        let (start, _) = stream
            .begin_outgoing(unit, [1; 8], vec![1], 0, 0, |_| Ok(()))
            .unwrap();
        records::write(&mut writer, &Record::Control(start), deadline, &budget)
            .await
            .unwrap();
        // Read the terminal protocol refusal; no payload is sent or accepted.
        let refusal = records::read(&mut reader, deadline, &budget).await.unwrap();
        assert!(matches!(
            refusal.record(),
            Record::Peer(koru_replication::protocol::PeerFrame::Error { .. })
        ));
    };
    let (result, ()) = tokio::join!(
        probe(client, unit, loro::VersionVector::default().encode()),
        peer
    );
    assert!(matches!(result, Err(Error::Invalid)));
}

#[tokio::test]
async fn authorization_is_rechecked_after_the_original_response_arrives() {
    let (mut client, mut reader, mut writer, _, scope, budget) =
        super::super::tests::pair_with_workspace(true).await;
    let revoked = Arc::new(AtomicBool::new(false));
    let check = revoked.clone();
    client.authorize = Box::new(move |_| {
        if check.load(Ordering::SeqCst) {
            Err(Refusal::DeviceRevoked)
        } else {
            Ok(())
        }
    });
    let unit = unit(scope);
    let version = loro::VersionVector::default().encode();
    let peer = async {
        let deadline = Instant::now() + Duration::from_secs(2);
        let request = records::read(&mut reader, deadline, &budget).await.unwrap();
        let (_, Record::Peer(frame), _) = request.into_parts() else {
            panic!()
        };
        let Metadata::Request(request) = metadata::decode(scope, frame, 1).unwrap().remove(0)
        else {
            panic!()
        };
        revoked.store(true, Ordering::SeqCst);
        records::write(
            &mut writer,
            &request.response(scope, version.clone()).unwrap().unwrap(),
            deadline,
            &budget,
        )
        .await
        .unwrap();
    };
    let (result, ()) = tokio::join!(probe(client, unit, version.clone()), peer);
    assert!(matches!(
        result,
        Err(Error::Authority(Refusal::DeviceRevoked))
    ));
}

#[tokio::test]
async fn a_response_for_another_document_never_proves_the_selected_one() {
    let (client, mut reader, mut writer, _, scope, budget) =
        super::super::tests::pair_with_workspace(true).await;
    let selected = unit(scope);
    let other = unit(scope);
    let version = loro::VersionVector::default().encode();
    let peer = async {
        let deadline = Instant::now() + Duration::from_secs(2);
        records::read(&mut reader, deadline, &budget).await.unwrap();
        let Record::Peer(frame) = metadata::join_request(scope, other, version.clone()).unwrap()
        else {
            panic!()
        };
        let Metadata::Request(request) = metadata::decode(scope, frame, 1).unwrap().remove(0)
        else {
            panic!()
        };
        records::write(
            &mut writer,
            &request.response(scope, version.clone()).unwrap().unwrap(),
            deadline,
            &budget,
        )
        .await
        .unwrap();
    };
    let (result, ()) = tokio::join!(probe(client, selected, version.clone()), peer);
    assert!(matches!(result, Err(Error::Invalid)));
}

#[tokio::test]
async fn cancelling_an_unanswered_probe_closes_the_carrier_without_store_work() {
    let (mut client, mut reader, _writer, _, scope, budget) =
        super::super::tests::pair_with_workspace(true).await;
    client.receive = Box::new(|_| panic!("cancellation cannot reach Store"));
    let peer = async {
        let deadline = Instant::now() + Duration::from_secs(2);
        records::read(&mut reader, deadline, &budget).await.unwrap();
        assert!(records::read(&mut reader, deadline, &budget).await.is_err());
    };
    let work = tokio::time::timeout(
        Duration::from_millis(50),
        probe(client, unit(scope), loro::VersionVector::default().encode()),
    );
    let (result, ()) = tokio::join!(work, peer);
    assert!(result.is_err());
}
